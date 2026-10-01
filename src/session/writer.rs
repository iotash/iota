//! Writing a bundle (chat/session.go:307-748).
//!
//! The bundle is created LAZILY: [`SessionStore::create`](crate::session::SessionStore::create) touches no disk,
//! so a session that never reaches a real turn leaves nothing behind — except a bot's, which is put on disk
//! at its first launch so its pointer never names a bundle that does not exist (docs/design/bot-mode.md
//! §2.2). Go's nil-receiver no-ops become an `Option<SessionWriter>` at the call site, and Go's eight `Set*`
//! methods collapse into [`SessionWriter::update_meta`] — the schema forces one rule for all of them. There
//! is no `close()`: `Drop` closes the handle and every batch already `sync_all()`s.

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
    /// A resumed log's last measurement ([`crate::session::LoadedLog::measured`]); `None` for a fresh session.
    measured: Option<Usage>,
    /// The bundle's single-writer lock (`<dir>/.lock`), taken when the writer first holds the files and
    /// released when it drops — `None` while the bundle is still pending.
    lock: Option<HeldLock>,
    /// A bot's lock (`<bots>/<name>/lock`), held for as long as the writer lives — the bot runs exactly as
    /// long as its session is open. `None` outside bot mode.
    bot_lock: Option<HeldLock>,
    /// While set and raised, every log line fails to write — a disk going away under an open handle, which
    /// no file-system trick in a test can reproduce ([`SessionWriter::fail_log_writes_while`]).
    #[cfg(feature = "testing")]
    log_down: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Where a failed batch began, while cutting the log back to it has not been confirmed: the log may still
    /// hold part of that batch, so nothing more is appended until a cut succeeds (review R1 — a retry on top
    /// of the remains would leave them in the middle of the log for good).
    uncut: Option<u64>,
    /// The meta in memory holds something `meta.json` does not: its last write failed. The next
    /// [`SessionWriter::update_meta`] writes even when its own change is none.
    meta_dirty: bool,
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
            .field("measured", &self.measured)
            .field("lock", &self.lock)
            .field("bot_lock", &self.bot_lock)
            .field("uncut", &self.uncut)
            .field("meta_dirty", &self.meta_dirty)
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
            measured: None,
            lock: None,
            bot_lock: None,
            #[cfg(feature = "testing")]
            log_down: None,
            uncut: None,
            meta_dirty: false,
        }
    }

    /// A writer over an EXISTING bundle, positioned to append (`ResumeSession`, chat/session.go:396-400):
    /// `conv_count`, `usage` and `measured` are seeded from the log that was just loaded; `lock` is the bundle
    /// lock the caller already holds.
    pub(crate) fn resumed(
        dir: PathBuf,
        meta: SessionMeta,
        kind: ProviderKind,
        file: std::fs::File,
        log: &crate::session::LoadedLog,
        lock: HeldLock,
    ) -> Self {
        let (conv_count, usage, measured) = (log.conv_count, log.usage, log.measured);
        Self {
            dir,
            meta,
            file: Some(file),
            kind,
            conv_count,
            created: true,
            usage,
            measured,
            lock: Some(lock),
            bot_lock: None,
            #[cfg(feature = "testing")]
            log_down: None,
            uncut: None,
            meta_dirty: false,
        }
    }

    /// Puts the bundle on disk now instead of at the first write: a bot's pointer may only name a bundle that
    /// exists (docs/design/bot-mode.md §2.2).
    pub(super) fn materialize(&mut self) -> Result<(), SessionError> {
        self.ensure_created()
    }

    /// Test seam: while `down` is raised every log line this writer writes fails, as a disk that went away
    /// under the open handle would. The batch is then cut back like any failed batch.
    #[cfg(feature = "testing")]
    pub fn fail_log_writes_while(&mut self, down: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.log_down = Some(down);
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

    /// The usage the resumed log's last answer since its last compaction carries — what that request measured,
    /// the system segment and the tool definitions included, which a local count of the view is not. A resumed
    /// bot's meter settles on it (bot-mode.md §4.1). `None` for a fresh session, or when nothing has been
    /// answered since the last compaction.
    pub fn measured(&self) -> Option<Usage> {
        self.measured
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
    ///
    /// The batch is all or nothing for the log: a failure before the `sync_all()` has returned cuts the log
    /// back to where the batch began and leaves the counters alone, so the caller retries the same slice. A
    /// failure of the meta rewrite AFTER it is [`SessionError::MetaNotSaved`]: the batch IS in the log and
    /// must not be appended again. When the cut fails too, the error is [`SessionError::CutFailed`] and every
    /// later append (and compaction marker) first tries the cut again — refusing with
    /// [`SessionError::LogNotCutBack`] until it succeeds, then going on as if the batch had never been
    /// tried.
    pub fn append_messages(&mut self, msgs: &[Message]) -> Result<(), SessionError> {
        let mut msgs = msgs.iter().filter(|m| m.tools().is_empty()).peekable();
        if msgs.peek().is_none() {
            return Ok(());
        }
        self.ensure_created()?;
        let start = self.log_start()?;
        let (mut count, mut conv, mut usage) = (0, 0, Usage::default());
        let written: Result<(), SessionError> = (|| {
            for msg in msgs {
                let rec = to_record(&self.dir, self.kind, msg)?;
                self.write_line(&rec)?;
                count += 1;
                if msg.role() != Role::System {
                    conv += 1;
                }
                if let Some(u) = msg.usage() {
                    usage += u; // keep usage() == what the log sums to
                }
            }
            self.sync()
        })();
        self.settle_batch(start, written)?;
        self.meta.message_count += count;
        self.conv_count += conv;
        self.usage += usage;
        self.write_meta_after_log()
    }

    /// Records that the session's system prompt was CLEARED (its `system:` removed from the config, §2.2): a
    /// system record flagged `system_cleared`, which wins over the prompt before it on the next load. Said
    /// explicitly, never read off an empty system message — any other empty system message the history holds
    /// (a defer mount with no tools) is written without the flag and never clears anything. Counts toward
    /// `message_count` like any system record; all or nothing like any batch.
    pub fn clear_system(&mut self) -> Result<(), SessionError> {
        self.ensure_created()?;
        let rec = SessionRecord {
            role: Role::System.as_str().to_owned(),
            system_cleared: true,
            ..SessionRecord::default()
        };
        let start = self.log_start()?;
        let written = self.write_line(&rec).and_then(|()| self.sync());
        self.settle_batch(start, written)?;
        self.meta.message_count += 1;
        self.write_meta_after_log()
    }

    /// `AppendCompaction` (chat/session.go:583-606): the summary plus how many leading conversation
    /// messages it supersedes (`max(0, conv_count - retain_tail)`). The originals stay in the log; the
    /// marker drives the derived view on reload. Bumps NEITHER `message_count` NOR `conv_count`.
    /// `retain_tail` counts CONVERSATION messages — the non-system ones, the same ones `conv_count` counts —
    /// so a frozen-mode mount in the retained turn cannot shift `compacted_through`. `flush_skipped` marks a
    /// bot's session compacted without its memory flush (docs/design/bot-mode.md §3.6.1).
    pub fn append_compaction(
        &mut self,
        summary: &str,
        retain_tail: usize,
        usage: Option<Usage>,
        flush_skipped: bool,
    ) -> Result<(), SessionError> {
        self.ensure_created()?;
        let mut rec = SessionRecord {
            role: ROLE_COMPACTION.to_owned(),
            content: summary.to_owned(),
            compacted_through: i64::try_from(self.conv_count.saturating_sub(retain_tail))
                .unwrap_or(i64::MAX),
            flush_skipped,
            ..SessionRecord::default()
        };
        if let Some(u) = usage {
            rec.usage = Some(u.into());
        }
        let start = self.log_start()?;
        let written = self.write_line(&rec).and_then(|()| self.sync());
        self.settle_batch(start, written)?;
        if let Some(u) = usage {
            self.usage += u;
        }
        self.write_meta_after_log()
    }

    /// The single meta mutator, standing in for Go's eight `Set*` methods (chat/session.go:612-741):
    /// applies `f`, then writes `meta.json` ONLY when the bundle already exists — a pending value is
    /// flushed by `ensure_created`'s first meta write, exactly like Go — and `f` changed something. An
    /// unchanged meta is not rewritten, so `updated_at` stays the last write that said anything: a bot that
    /// is opened and closed again restates what it runs under every time, and its next resume must still
    /// read how long ago the session was really written (fable M4).
    ///
    /// The writer owns `id`, `version`, `message_count` and `updated_at`; a closure that changes them is
    /// the caller's problem.
    ///
    /// "Unchanged" means unchanged against what `meta.json` holds: after a failed write the memory is ahead of
    /// the disk, so the next call writes even when it restates the same value.
    pub fn update_meta(&mut self, f: impl FnOnce(&mut SessionMeta)) -> Result<(), SessionError> {
        let before = self.created.then(|| self.meta.clone());
        f(&mut self.meta);
        match before {
            Some(before) if self.meta_dirty || before != self.meta => self.write_meta(),
            _ => Ok(()),
        }
    }

    /// Rewrites `meta.json`, remembering a failure so that [`Self::update_meta`] does not take the memory for
    /// the disk.
    fn write_meta(&mut self) -> Result<(), SessionError> {
        let written = self.meta.write(&self.dir);
        self.meta_dirty = written.is_err();
        written
    }

    /// Materialises the bundle on first use (`ensureCreated`, chat/session.go:363-378): `attachments/`
    /// UNCONDITIONALLY, the bundle lock, the append handle, then the first meta write (which flushes
    /// pending setters).
    fn ensure_created(&mut self) -> Result<(), SessionError> {
        if !self.created {
            std::fs::create_dir_all(self.dir.join(ATTACHMENTS_DIR))?;
            // The lock is kept only together with the handle: a failed open lets it go, so the next write
            // can take it again instead of being refused by this writer's own guard.
            let lock = lock_bundle(&self.dir, &self.meta.id)?;
            self.file = Some(open_append_0644(&self.dir.join(LOG_FILE))?);
            self.lock = Some(lock);
            self.created = true;
            self.write_meta()?;
        }
        Ok(())
    }

    /// The log's length before a batch — where [`Self::settle_batch`] cuts it back to. A failed batch whose
    /// cut is still unconfirmed is cut first; while that keeps failing, nothing is written.
    fn log_start(&mut self) -> Result<u64, SessionError> {
        if let Some(start) = self.uncut {
            self.cut_to(start).map_err(SessionError::LogNotCutBack)?;
            self.uncut = None;
        }
        Ok(self.log()?.metadata()?.len())
    }

    /// A batch that failed before its `sync_all()` returned leaves no trace in the log: whatever part of it
    /// was written is cut off again (the bundle lock is held, so nobody else appended meanwhile). The error
    /// is handed back as it came — unless the cut fails too: then the log may hold part of the batch, the
    /// writer remembers where it began ([`Self::log_start`] retries the cut), and the error says both.
    fn settle_batch(
        &mut self,
        start: u64,
        written: Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let Err(write) = written else { return Ok(()) };
        match self.cut_to(start) {
            Ok(()) => Err(write),
            Err(cut) => {
                self.uncut = Some(start);
                Err(SessionError::CutFailed {
                    write: Box::new(write),
                    cut,
                })
            }
        }
    }

    /// Cuts the log back to `len` bytes and syncs the cut — through a handle of its own opened for writing: the
    /// append handle may not be allowed to truncate (on Windows an append-only handle lacks `FILE_WRITE_DATA`,
    /// fable N3), and a cut that could never succeed would hold every later write back.
    fn cut_to(&mut self, len: u64) -> std::io::Result<()> {
        #[cfg(test)]
        if tests::CUTS_TO_FAIL.get() > 0 {
            tests::CUTS_TO_FAIL.set(tests::CUTS_TO_FAIL.get() - 1);
            return Err(std::io::Error::other("injected: the cut fails"));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.dir.join(LOG_FILE))?;
        file.set_len(len)?;
        file.sync_all()
    }

    /// The meta rewrite that follows a batch already in the log: its failure is
    /// [`SessionError::MetaNotSaved`], never a reason to append the batch again.
    fn write_meta_after_log(&mut self) -> Result<(), SessionError> {
        self.write_meta()
            .map_err(|e| SessionError::MetaNotSaved(Box::new(e)))
    }

    /// Serialises one record compactly — cut down by [`fit_line`] when it would reach the reader's line
    /// cap — and appends it plus `'\n'`.
    fn write_line(&mut self, rec: &SessionRecord) -> Result<(), SessionError> {
        use std::io::Write;
        let mut line = fit_line(rec, MAX_LOG_LINE)?;
        line.push(b'\n');
        #[cfg(feature = "testing")]
        if self
            .log_down
            .as_ref()
            .is_some_and(|d| d.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(SessionError::Io(std::io::Error::other(
                "injected: the disk is down",
            )));
        }
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

    thread_local! {
        /// How many of this thread's next cut-backs fail.
        pub(super) static CUTS_TO_FAIL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// Review R1: a batch whose write AND cut-back both fail is reported as both, and the writer appends
    /// nothing — not the retried batch, not a compaction marker — until a cut succeeds; then the retry lands
    /// once, and no call is left without its result in the middle of the log.
    #[test]
    fn a_failed_cut_holds_every_write_until_the_log_is_cut_back() {
        use crate::provider::ProviderKind;
        use crate::provider::model::{Attachment, Message, Role, ToolCall};
        use crate::session::error::SessionError;
        use crate::session::{NewSession, SessionStore};
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut w = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create");
        w.append_messages(&[Message::user("seed")]).expect("seed");
        let (id, dir) = (w.id().to_owned(), w.dir().to_path_buf());
        let lines = || {
            std::fs::read_to_string(dir.join("messages.jsonl"))
                .expect("log")
                .lines()
                .count()
        };

        // The third record's attachment cannot be stored, after two records reached the log.
        let c1 = ToolCall {
            id: "c1".to_owned(),
            name: "read_file".to_owned(),
            ..ToolCall::default()
        };
        let mut result = Message::tool_result(&c1, "ok", false);
        result.attachments.push(Attachment {
            filename: "a.bin".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            data: b"data".to_vec(),
        });
        let batch = [
            Message::user("q"),
            Message::assistant("").with_tool_calls(vec![c1]),
            result,
        ];
        let attachments = dir.join("attachments");
        std::fs::remove_dir(&attachments).expect("empty store");
        std::fs::write(&attachments, "").expect("a file in its place");
        CUTS_TO_FAIL.set(3);

        let err = w.append_messages(&batch).expect_err("write and cut fail");
        assert!(matches!(err, SessionError::CutFailed { .. }), "{err:?}");
        assert!(err.to_string().contains("could not be cut back"), "{err}");
        assert_eq!(lines(), 3, "the remains of the batch are still there");
        std::fs::remove_file(&attachments).expect("clear");
        std::fs::create_dir(&attachments).expect("restore the store");

        // Refused, and nothing appended, while the cut keeps failing.
        let err = w
            .append_compaction("summary", 0, None, false)
            .expect_err("no marker");
        assert!(matches!(err, SessionError::LogNotCutBack(_)), "{err:?}");
        let err = w.append_messages(&batch).expect_err("no retry");
        assert!(matches!(err, SessionError::LogNotCutBack(_)), "{err:?}");
        assert_eq!(lines(), 3);

        // Once the cut succeeds, the same batch lands once.
        w.append_messages(&batch).expect("the retry lands");
        assert_eq!(lines(), 4);
        assert_eq!(w.meta().message_count, 4);
        drop(w);
        let (_w, session) = store.resume(&id, ProviderKind::OpenAi).expect("resume");
        let roles: Vec<Role> = session.messages.iter().map(Message::role).collect();
        assert_eq!(roles, [Role::User, Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(session.messages[3].attachments.len(), 1);
        assert_eq!(session.meta.message_count, 4);
    }

    /// Fable N4: only [`super::SessionWriter::clear_system`] clears the prompt; an empty system message that
    /// reaches the log some other way is written as one an old log's empty mount was — and never wins.
    #[test]
    fn only_an_explicit_clear_is_a_cleared_prompt() {
        use crate::provider::ProviderKind;
        use crate::provider::model::Message;
        use crate::session::{NewSession, SessionStore};
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut w = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create");
        let last = |w: &super::SessionWriter| {
            std::fs::read_to_string(w.dir().join("messages.jsonl"))
                .expect("log")
                .lines()
                .last()
                .map(str::to_owned)
        };
        w.append_messages(&[Message::system("keep me"), Message::user("hi")])
            .expect("write");
        w.append_messages(&[Message::system("")]).expect("write");
        assert_eq!(last(&w).as_deref(), Some("{\"role\":\"system\"}"));
        let id = w.id().to_owned();
        assert_eq!(
            store
                .load(&id, ProviderKind::OpenAi)
                .expect("load")
                .messages[0]
                .content,
            "keep me"
        );
        w.clear_system().expect("clear");
        assert_eq!(
            last(&w).as_deref(),
            Some("{\"role\":\"system\",\"system_cleared\":true}")
        );
        assert_eq!(w.meta().message_count, 4);
        let loaded = store.load(&id, ProviderKind::OpenAi).expect("load");
        assert!(loaded.messages.iter().all(|m| m.content != "keep me"));
    }

    /// Review N2: a meta rewrite that failed leaves the memory ahead of the disk, so restating the same value
    /// writes it (and a reload sees it without any message having been appended); once saved, restating it
    /// again writes nothing.
    #[test]
    fn a_meta_change_that_failed_is_written_by_the_same_change_again() {
        use crate::provider::ProviderKind;
        use crate::provider::model::Message;
        use crate::session::meta::META_TMP_FILE;
        use crate::session::{NewSession, SessionStore};
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut w = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create");
        w.append_messages(&[Message::user("seed")]).expect("seed");
        let dir = w.dir().to_path_buf();

        std::fs::create_dir(dir.join(META_TMP_FILE)).expect("block the rewrite");
        w.update_meta(|m| m.title = "changed".to_owned())
            .expect_err("meta.json cannot be rewritten");
        std::fs::remove_dir(dir.join(META_TMP_FILE)).expect("unblock");
        w.update_meta(|m| m.title = "changed".to_owned())
            .expect("the same change again");
        let reloaded = store
            .load(w.id(), ProviderKind::OpenAi)
            .expect("reload without another message");
        assert_eq!(reloaded.meta.title, "changed");

        // Saved and unchanged: no rewrite, so `updated_at` stays put.
        std::fs::write(dir.join("meta.json"), "{}").expect("mark the file");
        w.update_meta(|m| m.title = "changed".to_owned())
            .expect("nothing to do");
        assert_eq!(
            std::fs::read_to_string(dir.join("meta.json")).expect("meta"),
            "{}"
        );
    }

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
