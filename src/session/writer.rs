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
use crate::session::loader::MAX_LOG_LINE;
use crate::session::lock::{HeldLock, lock_bundle};
use crate::session::meta::{SessionMeta, write_0644};
use crate::session::rawcodec::raw_to_blob;
use crate::session::record::{
    ATTACHMENTS_DIR, DATA_REF_PREFIX, IMAGES_DIR, LOG_FILE, ROLE_COMPACTION, SessionAttachment,
    SessionRaw, SessionRecord, SessionToolCall,
};

/// The optional figures a compaction marker carries beyond the summary (docs/design/bot-mode.md §3.6.2 item 4):
/// the raw material for measuring how much each compaction loses. Every field is optional on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompactionStats {
    /// Tokens of the messages the summary replaced.
    pub middle_tokens: Option<u64>,
    /// Tokens of the summary.
    pub summary_tokens: Option<u64>,
    /// A bot's session compacted without its memory flush.
    pub flush_skipped: bool,
}

/// What [`SessionWriter::on_created`] runs once the bundle is on disk.
pub type OnCreated = Box<dyn FnMut() -> Result<(), SessionError> + Send>;

/// Persists a live session: the bundle directory, its `meta.json`, the append-only `messages.jsonl` and
/// the content-addressed attachment store.
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
    /// The bundle's single-writer lock (`<dir>/.lock`), taken when the writer first holds the files and
    /// released when it drops — `None` while the bundle is still pending.
    lock: Option<HeldLock>,
    /// A bot's lock (`<bots>/<name>/lock`), held for as long as the writer lives — the bot runs exactly as
    /// long as its session is open. `None` outside bot mode.
    bot_lock: Option<HeldLock>,
    /// Runs once the bundle is on disk; kept (and retried by the next write) until it succeeds.
    on_created: Option<OnCreated>,
}

impl std::fmt::Debug for SessionWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWriter")
            .field("dir", &self.dir)
            .field("meta", &self.meta)
            .field("kind", &self.kind)
            .field("conv_count", &self.conv_count)
            .field("created", &self.created)
            .field("usage", &self.usage)
            .field("lock", &self.lock)
            .field("bot_lock", &self.bot_lock)
            .field("on_created", &self.on_created.is_some())
            .finish_non_exhaustive()
    }
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
            lock: None,
            bot_lock: None,
            on_created: None,
        }
    }

    /// A writer over an EXISTING bundle, positioned to append (`ResumeSession`, chat/session.go:396-400):
    /// `conv_count` and `usage` are seeded from the log that was just loaded; `lock` is the bundle lock the
    /// caller already holds.
    pub(crate) fn resumed(
        dir: PathBuf,
        meta: SessionMeta,
        kind: ProviderKind,
        file: std::fs::File,
        conv_count: usize,
        usage: Usage,
        lock: HeldLock,
    ) -> Self {
        Self {
            dir,
            meta,
            file: Some(file),
            kind,
            conv_count,
            created: true,
            usage,
            lock: Some(lock),
            bot_lock: None,
            on_created: None,
        }
    }

    /// Registers `f` to run once the bundle has been materialised — right after the first write created it
    /// (docs/design/bot-mode.md §2.2: a bot's pointer is marked `materialized` there). A failing `f` fails
    /// that write and is tried again by the next one. On a bundle already on disk it never runs.
    pub fn on_created(&mut self, f: OnCreated) {
        if !self.created {
            self.on_created = Some(f);
        }
    }

    /// Keeps a bot's lock alive for as long as this writer lives.
    pub(crate) fn hold_bot_lock(&mut self, lock: HeldLock) {
        self.bot_lock = Some(lock);
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
    ///
    /// A frozen-mode defer mount (`Message::system_tools`) is SKIPPED: it is runtime state, not persisted
    /// (tool-defer.md), and the record shape has no place for its tools — it would land as an empty system
    /// record. It counts toward nothing. Callers keep their watermark on the slice they HANDED in, so the
    /// skipped mount is still behind it and never retried; a batch of nothing but mounts touches no disk.
    pub fn append_messages(&mut self, msgs: &[Message]) -> Result<(), SessionError> {
        let mut msgs = msgs.iter().filter(|m| m.tools().is_empty()).peekable();
        if msgs.peek().is_none() {
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
        self.append_compaction_with(summary, retain_tail, usage, CompactionStats::default())
    }

    /// [`Self::append_compaction`] with the marker's optional figures (docs/design/bot-mode.md §3.6.2 item 4).
    /// `retain_tail` counts CONVERSATION messages — the non-system ones, the same ones `conv_count` counts —
    /// so a frozen-mode mount in the retained turn cannot shift `compacted_through`.
    pub fn append_compaction_with(
        &mut self,
        summary: &str,
        retain_tail: usize,
        usage: Option<Usage>,
        stats: CompactionStats,
    ) -> Result<(), SessionError> {
        self.ensure_created()?;
        let mut rec = SessionRecord {
            role: ROLE_COMPACTION.to_owned(),
            content: summary.to_owned(),
            compacted_through: i64::try_from(self.conv_count.saturating_sub(retain_tail))
                .unwrap_or(i64::MAX),
            middle_tokens: stats.middle_tokens,
            summary_tokens: stats.summary_tokens,
            flush_skipped: stats.flush_skipped,
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
    /// UNCONDITIONALLY, the bundle lock, the append handle, then the first meta write (which flushes
    /// pending setters) — and then the [`on_created`](Self::on_created) hook, until it has succeeded once.
    fn ensure_created(&mut self) -> Result<(), SessionError> {
        if !self.created {
            std::fs::create_dir_all(self.dir.join(ATTACHMENTS_DIR))?;
            self.lock = Some(lock_bundle(&self.dir, &self.meta.id)?);
            self.file = Some(open_append_0644(&self.dir.join(LOG_FILE))?);
            self.created = true;
            self.meta.write(&self.dir)?;
        }
        if let Some(f) = self.on_created.as_mut() {
            f()?;
            self.on_created = None;
        }
        Ok(())
    }

    /// Serialises one record compactly — cut down by [`fit_line`] when it would reach the reader's line
    /// cap — and appends it plus `'\n'`.
    fn write_line(&mut self, rec: &SessionRecord) -> Result<(), SessionError> {
        use std::io::Write;
        let mut line = fit_line(rec, MAX_LOG_LINE)?;
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

/// The compact serialisation of `rec`, guaranteed SHORTER than `cap` bytes (docs/design/bot-mode.md §2.7):
/// the reader aborts on a line that reaches [`MAX_LOG_LINE`], so one oversized record written here would
/// make the bundle unloadable for good.
///
/// An over-long record keeps its shape — no new key, no format change. Its `raw` payload goes first (a
/// dialect replaying it would resend the untruncated text), then `content` and, failing that, `reasoning`
/// are cut on a char boundary and end in `[record truncated: N bytes over the log line cap]`, `N` being
/// how far the original line was over. A record that still does not fit (its tool-call arguments alone
/// exceed the cap) is refused with `InvalidData`.
fn fit_line(rec: &SessionRecord, cap: usize) -> Result<Vec<u8>, SessionError> {
    let line = serde_json::to_vec(rec)?;
    if line.len() < cap {
        return Ok(line);
    }
    let over = line.len() + 1 - cap;
    let marker = format!("\n[record truncated: {over} bytes over the log line cap]");
    let mut rec = rec.clone();
    rec.raw = None;
    let mut line = serde_json::to_vec(&rec)?;
    if line.len() >= cap {
        shrink(&mut rec.content, line.len() + 1 - cap, &marker);
        line = serde_json::to_vec(&rec)?;
    }
    if line.len() >= cap {
        shrink(&mut rec.reasoning, line.len() + 1 - cap, &marker);
        line = serde_json::to_vec(&rec)?;
    }
    if line.len() < cap {
        Ok(line)
    } else {
        Err(SessionError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a session record exceeds the {cap}-byte log line limit"),
        )))
    }
}

/// Cuts `text` so its serialisation loses at least `excess` bytes, then ends it with `marker`. Every source
/// byte serialises to at least one byte, so cutting the excess plus the marker's own escaped length (`\n`
/// → two bytes) is always enough — when the text is long enough to give it.
fn shrink(text: &mut String, excess: usize, marker: &str) {
    let cut = excess + marker.len() + 1;
    let keep = text.floor_char_boundary(text.len().saturating_sub(cut));
    text.truncate(keep);
    text.push_str(marker);
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
        notice: msg.is_notice(),
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

#[cfg(test)]
mod tests {
    use super::fit_line;
    use crate::session::record::{SessionRaw, SessionRecord};

    fn tool(content: &str) -> SessionRecord {
        SessionRecord {
            role: "tool".to_owned(),
            content: content.to_owned(),
            tool_call_id: "c1".to_owned(),
            ..SessionRecord::default()
        }
    }

    /// A record under the cap is serialised untouched.
    #[test]
    fn a_short_record_is_untouched() {
        let rec = tool("hello");
        assert_eq!(
            fit_line(&rec, 1024).expect("fits"),
            serde_json::to_vec(&rec).expect("json")
        );
    }

    /// An over-long record keeps its keys, ends its content in the marker, and comes out strictly
    /// shorter than the cap — even when the content is all multi-byte characters and escapes.
    #[test]
    fn an_over_long_record_is_cut_to_fit_with_the_marker() {
        let cap = 400;
        for body in ["x".repeat(1000), "é\"\n".repeat(300)] {
            let rec = tool(&body);
            let over = serde_json::to_vec(&rec).expect("json").len() + 1 - cap;
            let line = fit_line(&rec, cap).expect("fits after cutting");
            assert!(line.len() < cap, "{} >= {cap}", line.len());
            let back: SessionRecord = serde_json::from_slice(&line).expect("still one record");
            assert_eq!(back.tool_call_id, "c1");
            assert!(body.starts_with(back.content.split('\n').next().unwrap_or_default()));
            assert!(
                back.content.ends_with(&format!(
                    "[record truncated: {over} bytes over the log line cap]"
                )),
                "{}",
                back.content
            );
        }
    }

    /// The raw replay payload is dropped first: it would carry the untruncated text back to the API.
    #[test]
    fn the_raw_payload_goes_before_the_content() {
        let mut rec = tool("short");
        rec.role = "assistant".to_owned();
        rec.raw = Some(SessionRaw {
            provider: "openai".to_owned(),
            blob: serde_json::from_str(&format!("{:?}", "y".repeat(1000))).expect("blob"),
        });
        let line = fit_line(&rec, 400).expect("fits");
        let back: SessionRecord = serde_json::from_slice(&line).expect("record");
        assert_eq!(back.raw, None);
        assert_eq!(back.content, "short", "dropping raw was enough");
    }

    /// When no text field can absorb the excess the record is refused rather than written.
    #[test]
    fn a_record_that_cannot_fit_is_refused() {
        let mut rec = tool("");
        rec.tool_call_id = "z".repeat(1000);
        assert!(fit_line(&rec, 400).is_err());
    }
}
