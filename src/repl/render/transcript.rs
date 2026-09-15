//! The transcript single-writer (chat/transcript.go): the session's single write surface
//! for the chat area — every block that lands in the scrollback (user input, activity
//! summaries, markdown content, notices, errors, resume echoes) is declared here, and the
//! transcript alone decides the spacing between them. Two rules replace ad-hoc `""`
//! commits:
//!
//! - every block opens with exactly one blank separator (above the first block sits the
//!   pre-Tui banner or the previous turn), consecutive same-kind notices/errors grouping
//!   into one block;
//! - blank lines INSIDE a block are deferred (latched) until more content follows, so no
//!   block can export trailing blanks for a neighbor to lean on.
//!
//! The activity-group half of the state machine lives in `group.rs`. Safe for concurrent
//! use (the async MCP reporter interleaves with streaming turns) — one mutex.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::headless::images::IMAGE_INDENT_COLS;
use crate::repl::render::group;
use crate::repl::render::group::ActivityGroup;
use crate::repl::render::styles::{dim, red, truncate_runes};
use crate::sync::lock;

/// The image-generation widget's label (transcript.go:373,381).
pub(crate) const IMAGE_WIDGET_LABEL: &str = "image";

/// Classifies the chat area's logical blocks (transcript.go `blockKind`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockKind {
    /// Nothing committed yet (the session's first block pays no separator).
    #[default]
    None,
    /// The `❯` user block.
    User,
    /// An activity group (widget raise / settled summary).
    Activity,
    /// A markdown content block.
    Content,
    /// An interactive tool's `"?"` record block.
    Ask,
    /// A dim notice; consecutive notices merge.
    Notice,
    /// A red error; consecutive errors merge.
    Error,
    /// A resume echo.
    Echo,
    /// An image block (transcript.go:52 blockImage; T3, WP63).
    Image,
}

/// Token estimator behind the thinking meter (`None` in T1 — the tiktoken counter is
/// WP53's; the seam keeps the meter plumbing testable).
pub(crate) type TokenEstimator = Box<dyn Fn(&str) -> u64 + Send>;

/// The `/debug` verbose hook (T-18: the command is unregistered this slice; the closure
/// survives so verbose-settling stays test-pinned).
pub(crate) type VerboseFn = Box<dyn Fn() -> bool + Send>;

/// The mutex-guarded transcript state (Go `transcript` fields).
pub struct Inner {
    pub u: Arc<dyn crate::ui::facade::Ui>,
    pub tokens: Option<TokenEstimator>,
    pub verbose: Option<VerboseFn>,
    pub last: BlockKind,
    /// Deferred block-interior blank lines (the latch).
    pub pending: usize,
    /// A markdown content block is streaming (the renderer may hold buffered lines).
    pub content_open: bool,
    /// Tool call announced while thinking/content streams; raised at close.
    pub pending_call: String,
    /// A dropped widget's separator awaits reuse by the next block.
    pub orphan_sep: bool,
    pub grp: ActivityGroup,
    /// The terminal's background tone the diff shades follow (chat/theme.go `themeDark`):
    /// dark by default, refreshed by the run loop between turns only.
    pub dark: bool,
}

impl Inner {
    pub fn verbose_on(&self) -> bool {
        self.verbose.as_ref().is_some_and(|f| f())
    }

    /// Opens a new block (transcript.go `beginLocked`): the previous block's deferred
    /// blanks die, one separator is paid — unless a dropped widget left its separator
    /// behind (the orphaned blank serves as this block's), or this is the session's FIRST
    /// block (the environment above always ends with exactly one blank of its own).
    pub fn begin(&mut self, kind: BlockKind) {
        self.pending = 0;
        let first = self.last == BlockKind::None;
        self.last = kind;
        if first || self.orphan_sep {
            self.orphan_sep = false;
            return;
        }
        self.u.print_lines(vec![String::new()]);
    }

    /// Commits lines into the current block (transcript.go `pushLocked`), deferring
    /// interior blank lines until more content follows (a block never ends with trailing
    /// blanks). Entries with embedded newlines (a provider error carrying its JSON body)
    /// are expanded first: the latch needs line granularity, and downstream row
    /// accounting assumes one row per line.
    pub fn push_lines(&mut self, lines: &[&str]) {
        let mut out: Vec<String> = Vec::new();
        for chunk in lines {
            for ln in chunk.split('\n') {
                let ln = ln.strip_suffix('\r').unwrap_or(ln);
                if ln.trim().is_empty() {
                    self.pending += 1;
                    continue;
                }
                for _ in 0..self.pending {
                    out.push(String::new());
                }
                self.pending = 0;
                out.push(ln.to_owned());
            }
        }
        if !out.is_empty() {
            self.u.print_lines(out);
        }
    }
}

/// The transcript state machine (blank latch, activity group, `verbose` closure hook).
/// The ONLY writer to the chat area: constructed once by the run loop and shared (Arc)
/// with the stream task and the async MCP reporter.
pub struct Transcript {
    inner: Mutex<Inner>,
}

impl Transcript {
    /// A transcript over `surface`; `tokens` feeds the thinking meter (`None` in T1).
    pub fn new(surface: Arc<dyn crate::ui::facade::Ui>, tokens: Option<TokenEstimator>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                u: surface,
                tokens,
                verbose: None,
                last: BlockKind::None,
                pending: 0,
                content_open: false,
                pending_call: String::new(),
                orphan_sep: false,
                grp: ActivityGroup::default(),
                dark: true,
            }),
        }
    }

    /// Adopts the terminal's background tone for the diff shades (chat/theme.go
    /// `applyCodeTheme`): pre-loop from the probe, then between turns from the host.
    pub fn set_dark(&self, dark: bool) {
        self.lock().dark = dark;
    }

    /// Installs the verbose hook (run.go:187-191 `tr.verbose = reqLog.Verbose`): with it
    /// returning true the activity group settles after EVERY event (classic per-call
    /// blocks).
    pub fn set_verbose(&self, f: Option<VerboseFn>) {
        self.lock().verbose = f;
    }

    /// The transcript state under its lock (poison-tolerant).
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// Renders the submitted input as the `❯` block (transcript.go `user`). A mid-turn
    /// injection (steering) lands while an activity group is running — the user speaking
    /// is a stronger boundary than content, so the group settles first; at a normal turn
    /// start the group is empty and the settle is a no-op.
    pub fn user(&self, display: &str) {
        let mut inner = self.lock();
        group::settle_group(&mut inner);
        inner.begin(BlockKind::User);
        inner.u.user_block(display);
    }

    /// Prints a dim one-liner; consecutive notices group into one block.
    pub fn notice(&self, text: &str) {
        self.grouped(BlockKind::Notice, &dim(text));
    }

    /// Prints a red one-liner; consecutive errors group into one block.
    pub fn error(&self, text: &str) {
        self.grouped(BlockKind::Error, &red(text));
    }

    /// Renders a structured error (transcript.go:207-231): a red `"✗ headline"` row over
    /// dim detail rows in the tool-result idiom (`"  ⎿ "` on the first, four-space indent
    /// after). Detail rows are pre-wrapped under the hanging indent at width−5 (floor 20)
    /// so no produced row reaches the exact screen width. Groups with adjacent error
    /// blocks like [`Transcript::error`].
    pub fn error_block(&self, headline: &str, detail: &[String]) {
        let mut inner = self.lock();
        if inner.last != BlockKind::Error {
            inner.begin(BlockKind::Error);
        }
        let wrap_at = usize::from(inner.u.width()).saturating_sub(5).max(20);
        let mut lines = vec![red(&format!("✗ {headline}"))];
        let mut indent = "  ⎿ ";
        for d in detail {
            for ln in d.split('\n') {
                if ln.trim().is_empty() {
                    continue;
                }
                for row in crate::text::ansi::wrap_ansi(ln.trim_end_matches('\r'), wrap_at) {
                    lines.push(dim(&format!("{indent}{row}")));
                    indent = "    ";
                }
            }
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
    }

    fn grouped(&self, kind: BlockKind, line: &str) {
        let mut inner = self.lock();
        if inner.last != kind {
            inner.begin(kind);
        }
        inner.push_lines(&[line]);
    }

    /// Commits a pre-rendered multi-line result as one grouped notice block
    /// (transcript.go `noticeLines`).
    pub fn notice_lines(&self, lines: &[String]) {
        let mut inner = self.lock();
        if inner.last != BlockKind::Notice {
            inner.begin(BlockKind::Notice);
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
    }

    /// Replays pre-rendered history (session resume) as one block; the latch swallows its
    /// trailing blanks, interior spacing passes through (transcript.go `echo`).
    pub fn echo(&self, lines: &[String]) {
        let mut inner = self.lock();
        inner.begin(BlockKind::Echo);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
    }

    /// Clears the streaming-phase guards at the top of a stream round (transcript.go
    /// `beginRound`): a round that died mid-stream may have leaked
    /// `thinking_up`/`content_open`, which would silently defer the next round's widget
    /// forever. Group counters deliberately survive — rounds accumulate into one group.
    pub fn begin_round(&self) {
        let mut inner = self.lock();
        inner.grp.thinking_up = false;
        inner.content_open = false;
    }

    /// Closes out widget bookkeeping at a turn boundary (transcript.go `resetTurn`). A
    /// group that recorded events still settles (the widget itself is already gone:
    /// `sink.done()` dropped it, so the summary commits as plain lines); a widget dropped
    /// before any event settled leaves its separator for the next block to reuse.
    pub fn reset_turn(&self) {
        let mut inner = self.lock();
        if inner.grp.has_events() {
            group::settle_group(&mut inner);
        } else if inner.grp.up || inner.grp.thinking_up {
            inner.orphan_sep = true;
        }
        inner.grp = ActivityGroup::default();
        inner.content_open = false;
        inner.pending_call.clear();
    }

    /// Materializes deferred interior blanks NOW (transcript.go `flushPending`): a block
    /// preview about to occupy the next frame row IS more content — preview and settled
    /// block must be spaced identically.
    pub fn flush_pending(&self) {
        let mut inner = self.lock();
        for _ in 0..inner.pending {
            inner.u.print_lines(vec![String::new()]);
        }
        inner.pending = 0;
    }

    /// Marks the content block open (transcript.go `markContent`). The provider's content
    /// path calls it on the FIRST content byte — on the stream side, so the mark
    /// happens-before any tool-call delta that follows in the same network read.
    pub fn mark_content(&self) {
        self.lock().content_open = true;
    }

    /// Marks a markdown content block as streaming and returns its committer
    /// (transcript.go `openContent`). While it is open, a tool call announced by the
    /// stream observer is only remembered; [`Transcript::close_content`] raises it.
    pub fn open_content(self: &Arc<Self>) -> ContentCommitter {
        self.lock().content_open = true;
        self.content_block()
    }

    /// Ends the streaming content block (call after the renderer's final flush) and
    /// raises a tool call deferred while it was open (transcript.go `closeContent`).
    pub fn close_content(&self) {
        let mut inner = self.lock();
        inner.content_open = false;
        group::raise_pending(&mut inner);
    }

    /// Commits a rendered image block (transcript.go:335-361): the half-block rows, then the dim
    /// caption (`"🖼 saved: <path>"`), all inside ONE block under a uniform two-space indent so
    /// the picture does not sit flush against the edge. The indent is safe to prepend: every row
    /// is SGR-self-contained, so the leading spaces render in the terminal's own colours.
    ///
    /// Three openings, in Go's order:
    /// - accumulated activity settles first, and the image opens its own block;
    /// - a raised generation widget IS this image's widget — the block morphs it in place, paying
    ///   no second separator (the region replaces the preview rows bottom-up);
    /// - otherwise a plain new block.
    pub fn image(&self, rows: &[String], caption: &str) {
        let mut inner = self.lock();
        if inner.grp.has_events() {
            // Accumulated activity settles first; the image opens its own block.
            group::settle_group(&mut inner);
            inner.begin(BlockKind::Image);
        } else if inner.grp.up {
            // An image-generation widget is up: the image IS its result — morph the widget into
            // the image block in place (the separator was paid at the raise).
            inner.grp = ActivityGroup::default();
            inner.pending = 0;
            inner.last = BlockKind::Image;
            inner.u.close_preview();
        } else {
            inner.begin(BlockKind::Image);
        }
        let indent = " ".repeat(IMAGE_INDENT_COLS);
        let indented: Vec<String> = rows.iter().map(|r| format!("{indent}{r}")).collect();
        let refs: Vec<&str> = indented.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
        inner.push_lines(&[&format!("{indent}{}", dim(caption))]);
    }

    /// Ensures the image-generation widget ahead of progressive frames (transcript.go:368-383).
    ///
    /// Accumulated activity settles first — partial frames replace the widget body WHOLESALE, so
    /// activity rows and refining thumbnails cannot share it. While thinking or content owns the
    /// slot the raise defers like any composing call (`settle_thinking`/`close_content` raises it).
    pub fn image_widget(&self) {
        let mut inner = self.lock();
        if inner.grp.thinking_up || inner.content_open {
            if inner.pending_call.is_empty() {
                IMAGE_WIDGET_LABEL.clone_into(&mut inner.pending_call);
            }
            return;
        }
        if inner.grp.has_events() {
            group::settle_group(&mut inner);
        }
        if !inner.grp.up {
            group::ensure_widget(&mut inner, IMAGE_WIDGET_LABEL);
        }
    }

    /// The committer for one markdown content block (transcript.go `contentBlock`). The
    /// block opens lazily on the first line — settling the activity group first: a
    /// content boundary is what closes a group — and re-opens (new separator) if an async
    /// block (an MCP failure notice) interleaved since.
    pub fn content_block(self: &Arc<Self>) -> ContentCommitter {
        ContentCommitter {
            tr: Arc::clone(self),
            opened: false,
        }
    }
}

/// Committer of one markdown content block (the Go `func(lines ...string)` closure).
pub struct ContentCommitter {
    tr: Arc<Transcript>,
    opened: bool,
}

impl ContentCommitter {
    /// Commits `lines` into the block, opening (or re-opening) it as needed.
    pub fn push<S: AsRef<str>>(&mut self, lines: &[S]) {
        let mut inner = self.tr.lock();
        if !self.opened || inner.last != BlockKind::Content {
            self.opened = true;
            group::settle_group(&mut inner);
            inner.begin(BlockKind::Content);
        }
        let refs: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
        inner.push_lines(&refs);
    }
}

/// Shapes a completed reply into the attention ping's text (chat/chat.go:229-238
/// `notifyDigest`): the first content line with its markdown dressing stripped, capped at
/// 60 runes for a notification banner; `"Response ready"` for empty replies.
///
/// The one-line flattening is [`crate::repl::title::flatten_line`] — the same helper the
/// window title and the session picker use (chat/editpicker.go:57 `flattenLine`).
pub(crate) fn notify_digest(reply: &str) -> String {
    for line in reply.split('\n') {
        let line = line.trim_start_matches(['#', '>', '-', '*', '+', ' ', '\t']);
        let line = line.replace("**", "").replace('`', "");
        let line = line.trim();
        if !line.is_empty() {
            return truncate_runes(&crate::repl::title::flatten_line(line), 60);
        }
    }
    "Response ready".to_owned()
}

#[cfg(test)]
mod tests;
