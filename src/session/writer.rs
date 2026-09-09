//! Writing a bundle (chat/session.go:307-748).
//!
//! The bundle is created LAZILY: [`SessionStore::create`](crate::session::SessionStore::create) touches no disk, so
//! a session that never reaches a real turn leaves nothing behind. Go's nil-receiver no-ops become an
//! `Option<SessionWriter>` at the call site, and Go's eight `Set*` methods collapse into
//! [`SessionWriter::update_meta`] — the schema forces one rule for all of them. There is no `close()`:
//! `Drop` closes the handle and every batch already `sync_all()`s.

use std::path::{Path, PathBuf};

use crate::provider::ProviderKind;
use crate::provider::model::{Attachment, Message, Role};
use crate::provider::usage::Usage;
use sha2::{Digest, Sha256};

use crate::session::error::SessionError;
use crate::session::meta::{SessionMeta, write_0644};
use crate::session::rawcodec::raw_to_blob;
use crate::session::record::{
    ATTACHMENTS_DIR, DATA_REF_PREFIX, IMAGES_DIR, LOG_FILE, ROLE_COMPACTION, SessionAttachment,
    SessionRaw, SessionRecord, SessionToolCall,
};

/// Persists a live session: the bundle directory, its `meta.json`, the append-only `messages.jsonl` and
/// the content-addressed attachment store.
#[derive(Debug)]
pub struct SessionWriter {
    dir: PathBuf,
    meta: SessionMeta,
    /// `messages.jsonl`, opened for append — `None` until the bundle is materialised.
    file: Option<std::fs::File>,
    kind: ProviderKind,
    /// Conversation messages appended (system messages and compaction markers excluded).
    conv_count: usize,
    /// Whether the on-disk bundle exists yet (lazy creation).
    created: bool,
    /// What the log sums to: a resumed session's cumulative figures pick up from here, not from zero.
    usage: Usage,
}

impl SessionWriter {
    /// A writer over a bundle that does NOT exist yet (`NewSessionWriter`, chat/session.go:335-361):
    /// nothing is on disk until the first append.
    pub fn pending(dir: PathBuf, meta: SessionMeta, kind: ProviderKind) -> Self {
        Self {
            dir,
            meta,
            file: None,
            kind,
            conv_count: 0,
            created: false,
            usage: Usage::default(),
        }
    }

    /// A writer over an EXISTING bundle, positioned to append (`ResumeSession`, chat/session.go:396-400):
    /// `conv_count` and `usage` are seeded from the log that was just loaded.
    pub fn resumed(
        dir: PathBuf,
        meta: SessionMeta,
        kind: ProviderKind,
        file: std::fs::File,
        conv_count: usize,
        usage: Usage,
    ) -> Self {
        Self {
            dir,
            meta,
            file: Some(file),
            kind,
            conv_count,
            created: true,
            usage,
        }
    }

    /// The session id.
    pub fn id(&self) -> &str {
        &self.meta.id
    }

    /// The metadata as it stands in memory.
    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    /// What the log sums to (chat/session.go:407-412) — zero for a fresh session.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Whether the bundle exists on disk yet (chat/session.go:477-479).
    pub fn on_disk(&self) -> bool {
        self.created
    }

    /// The bundle directory (it may not exist yet).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<dir>/images` with NO side effects (Go's `imagesPath`, chat/session.go:646-651).
    pub fn images_path(&self) -> PathBuf {
        self.dir.join(IMAGES_DIR)
    }

    /// `ensureCreated` + `mkdir -p <dir>/images` (Go's `ImagesDir`, chat/session.go:653-665). `None` on
    /// failure, like Go's `""`.
    pub fn images_dir(&mut self) -> Option<PathBuf> {
        self.ensure_created().ok()?;
        let dir = self.images_path();
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// `AppendMessages` (chat/session.go:541-575). A no-op on an empty slice.
    ///
    /// Per message: attachments go to the content-addressed store, `raw_content` through
    /// [`raw_to_blob`], and the compact line plus `'\n'` to the log; `message_count`
    /// grows ALWAYS, `conv_count` only for a non-system role, and `usage` accumulates the message's own.
    /// After the WHOLE batch: ONE `sync_all()`, then ONE meta rewrite.
    pub fn append_messages(&mut self, msgs: &[Message]) -> Result<(), SessionError> {
        if msgs.is_empty() {
            return Ok(());
        }
        self.ensure_created()?;
        for msg in msgs {
            let rec = to_record(&self.dir, self.kind, msg)?;
            self.write_line(&rec)?;
            self.meta.message_count += 1;
            if msg.role() != Role::System {
                self.conv_count += 1;
            }
            if let Some(u) = msg.usage() {
                self.usage += u; // keep usage() == what the log sums to
            }
        }
        self.sync()?;
        self.meta.write(&self.dir)
    }

    /// `AppendCompaction` (chat/session.go:583-606): the summary plus how many leading conversation
    /// messages it supersedes (`max(0, conv_count - retain_tail)`). The originals stay in the log; the
    /// marker drives the derived view on reload. Bumps NEITHER `message_count` NOR `conv_count`.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        retain_tail: usize,
        usage: Option<Usage>,
    ) -> Result<(), SessionError> {
        self.ensure_created()?;
        let mut rec = SessionRecord {
            role: ROLE_COMPACTION.to_owned(),
            content: summary.to_owned(),
            compacted_through: i64::try_from(self.conv_count.saturating_sub(retain_tail))
                .unwrap_or(i64::MAX),
            ..SessionRecord::default()
        };
        if let Some(u) = usage {
            rec.usage = Some(u.into());
            self.usage += u;
        }
        self.write_line(&rec)?;
        self.sync()?;
        self.meta.write(&self.dir)
    }

    /// The single meta mutator, standing in for Go's eight `Set*` methods (chat/session.go:612-741):
    /// applies `f`, then writes `meta.json` ONLY when the bundle already exists — a pending value is
    /// flushed by `ensure_created`'s first meta write, exactly like Go.
    ///
    /// The writer owns `id`, `version`, `message_count` and `updated_at`; a closure that changes them is
    /// the caller's problem.
    pub fn update_meta(&mut self, f: impl FnOnce(&mut SessionMeta)) -> Result<(), SessionError> {
        f(&mut self.meta);
        if !self.created {
            return Ok(());
        }
        self.meta.write(&self.dir)
    }

    /// Materialises the bundle on first use (`ensureCreated`, chat/session.go:363-378): `attachments/`
    /// UNCONDITIONALLY, the append handle, then the first meta write (which flushes pending setters).
    fn ensure_created(&mut self) -> Result<(), SessionError> {
        if self.created {
            return Ok(());
        }
        std::fs::create_dir_all(self.dir.join(ATTACHMENTS_DIR))?;
        self.file = Some(open_append_0644(&self.dir.join(LOG_FILE))?);
        self.created = true;
        self.meta.write(&self.dir)
    }

    /// Serialises one record compactly and appends it plus `'\n'`.
    fn write_line(&mut self, rec: &SessionRecord) -> Result<(), SessionError> {
        use std::io::Write;
        let mut line = serde_json::to_vec(rec)?;
        line.push(b'\n');
        self.log()?.write_all(&line)?;
        Ok(())
    }

    /// One `sync_all()` for the whole batch (chat/session.go:571).
    fn sync(&mut self) -> Result<(), SessionError> {
        self.log()?.sync_all()?;
        Ok(())
    }

    /// The open log handle; `ensure_created` always runs first, so `None` here is a bug, not a state.
    fn log(&mut self) -> Result<&mut std::fs::File, SessionError> {
        self.file.as_mut().ok_or(SessionError::LogNotOpen)
    }
}

/// `os.OpenFile(path, O_CREATE|O_WRONLY|O_APPEND, 0o644)` (chat/session.go:371).
#[cfg(unix)]
pub(crate) fn open_append_0644(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true) // implies write access, so `O_WRONLY` needs no separate `.write(true)`
        .mode(0o644)
        .open(path)
}

/// Non-unix hosts have no mode bits to set (phase-1 DIVERGENCES I-07).
#[cfg(not(unix))]
pub(crate) fn open_append_0644(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true) // implies write access
        .open(path)
}

/// `toSessionMessage` (chat/session.go:506-539): the in-memory message as one log record, with its
/// attachments written to the content-addressed store first.
fn to_record(dir: &Path, kind: ProviderKind, msg: &Message) -> Result<SessionRecord, SessionError> {
    let mut rec = SessionRecord {
        role: msg.role().as_str().to_owned(),
        content: msg.content.clone(),
        reasoning: msg.reasoning().to_owned(),
        tool_call_id: msg.tool_call_id().to_owned(),
        tool_call_name: msg.tool_call_name().to_owned(),
        is_error: msg.is_error(),
        interrupted: msg.interrupted(),
        usage: msg.usage().map(Into::into),
        ..SessionRecord::default()
    };
    for att in &msg.attachments {
        rec.attachments.push(write_attachment(dir, att)?);
    }
    for call in msg.tool_calls() {
        rec.tool_calls.push(SessionToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
    }
    // Persist the dialect's raw payload tagged with the producing provider type, so a resume under the
    // same type can restore the reasoning chain.
    if let Some(rc) = msg.raw_content()
        && let Some(blob) = raw_to_blob(kind, rc)
    {
        rec.raw = Some(SessionRaw {
            provider: kind.as_str().to_owned(),
            blob,
        });
    }
    Ok(rec)
}

/// `writeAttachment` (chat/session.go:490-504): `attachments/<sha256 lowercase hex>`, written 0644 ONLY
/// when absent (content-addressed dedup).
fn write_attachment(dir: &Path, att: &Attachment) -> Result<SessionAttachment, SessionError> {
    let hex = hex_lower(&Sha256::digest(&att.data));
    let path = dir.join(ATTACHMENTS_DIR).join(&hex);
    if !path.exists() {
        write_0644(&path, &att.data)?;
    }
    Ok(SessionAttachment {
        filename: att.filename.clone(),
        mime_type: att.mime_type.clone(),
        data_ref: format!("{DATA_REF_PREFIX}{hex}"),
    })
}

/// Lowercase hex of a digest (`hex.EncodeToString`).
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}
