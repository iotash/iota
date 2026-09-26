//! The output staging window — a 1:1 port of internal/ui/region.go.
//!
//! The region is the answer to "why can't the preview area just be overwritten in
//! place?" — it is. All committed output flows THROUGH it: new lines enter the window,
//! older lines overflow into real scrollback. A block preview occupies window rows by
//! stealing them (each steal commits one tail line — a growth, never a shrink), and when
//! the block flushes, its rendered lines flow through the same window: the head commits
//! above, the last rows REPLACE the preview in place. When the replacement is SHORTER
//! than the preview (a reasoning window collapsing to its one-line "thought for Ns"
//! marker), the uncovered preview rows stay put as residue and later lines consume them
//! top-down — never entering scrollback. The window's height only ever grows to
//! [`TAIL_KEEP`] and then stays constant, so the composer never pops upward — the bounce
//! class is gone by construction.
//!
//! The window never closes: at idle it shows the last lines of the previous reply
//! (visually indistinguishable from scrollback); [`Region::flush`] flushes it. The only
//! remaining shrinks are an interrupted preview and end-of-turn residue (both through
//! [`Region::drop_preview`]), one-frame artifacts on turn boundaries.
//!
//! Concurrency: Go's region carried its own internal mutex; here the handle wraps the
//! whole struct in a `Mutex<Region>` (`TUI_CONTRACTS` §5) and every method publishes
//! before it returns, so the Go publish-under-the-lock global-ordering law holds: the
//! caller's guard spans mutation AND emission, and concurrent writers (stream task, MCP
//! reporter) serialize on that one lock while the single mailbox consumer preserves
//! arrival order.
//!
//! Deliberate non-ports (divergence rows): `sanitizeOverflow` (T-01 — wart W6's
//! `Paragraph::line_count` self-consistency removes the deferred-wrap miscount class) and
//! `joinOverflow`'s blank→`" "` substitution (T-02 — a blank ratatui `Line` inserts as one
//! blank row). `setCallBody` (T-16) is ported in full: [`Region::set_call_body`] replaces the
//! preview body wholesale for progressive image frames, beside the rolling
//! [`Region::preview_line`] law.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Instant;

use crate::text::ansi::{ansi_width, wrap_ansi};

use super::debug::debug_region;

/// The staging window height: the last N output lines live INSIDE the frame (directly
/// above the separator) instead of scrollback (region.go:13). The LIVE cap is
/// [`Region::tail_keep`] — this is its ceiling, reached on any terminal with room.
pub(crate) const TAIL_KEEP: usize = 4;

/// The frame rows nothing can trim away: the spacer, both separators, one composer row
/// and the bottom zone (status | desc | surface). The frame's floor (T-40).
const FRAME_FLOOR: usize = 5;

/// The rolling preview body keeps at most this many source lines (region.go:35).
pub(crate) const PREVIEW_WINDOW: usize = 3;

/// Test seam callback: receives the (unchunked) overflow and the fresh snapshot,
/// exactly like Go's non-nil `region.emit` (region.go:39,120-122).
#[cfg(test)]
pub(crate) type EmitFn = Box<dyn FnMut(Vec<String>, RegionSnapshot) + Send>;

/// The live publish seam. WP44's loop wiring implements this over its `UiMsg` mailbox
/// sender (`UiMsg::Scrollback` per chunk, then `UiMsg::Region`); the mailbox FIFO plus
/// the region lock preserve the ordering Go's mutex-held `Println`+`Send` guaranteed.
pub(crate) trait LivePublish: Send {
    /// One chunked scrollback batch (already ≤ `max(2, screen_height/2)` rows).
    fn scrollback(&self, rows: Vec<String>);
    /// The fresh display snapshot (sent after every mutation, overflow or not).
    fn region(&self, snap: RegionSnapshot);
}

/// Where a publish goes: the unit-test capture seam, or the live loop mailbox
/// (region.go `emit` nil/non-nil split; `TUI_CONTRACTS` §5 `Emit{Test, Live}`).
pub(crate) enum Emit {
    /// Test seam: captures raw overflow + snapshot; chunking is bypassed like Go's
    /// `emit != nil` path.
    #[cfg(test)]
    Test(EmitFn),
    /// Live path: overflow chunked below the screen height, then the snapshot.
    Live {
        /// The loop-mailbox publisher (see [`LivePublish`]).
        tx: Box<dyn LivePublish>,
    },
}

/// The display snapshot the model renders (region.go `regionMsg`): a deep copy taken
/// under the lock; the frame renders only this.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RegionSnapshot {
    /// Committed-pending lines shown in the frame.
    pub(crate) tail: Vec<String>,
    /// Stale preview rows awaiting in-place replacement (rendered BLANK).
    pub(crate) residue: Vec<String>,
    /// Preview header label (`""` = no preview).
    pub(crate) label: String,
    /// Preview rolling source lines (≤ [`PREVIEW_WINDOW`]).
    pub(crate) preview_tail: Vec<String>,
    /// Call preview lifecycle start; `Some` = render the `"⎿ elapsed"` row and tick
    /// (`None` = plain preview; Go zero-`time.Time` idiom).
    pub(crate) since: Option<Instant>,
    /// `Some` = the elapsed figure freezes here (user being consulted).
    pub(crate) paused_at: Option<Instant>,
    /// Call preview live status-row prefix (`"1.2k tokens"`).
    pub(crate) detail: String,
}

/// The staging window state (region.go `region`). Lives in a `Mutex<Region>` inside the
/// facade handle; all methods mutate and publish while the caller holds that lock.
pub(crate) struct Region {
    emit: Emit,
    /// Shared terminal width (facade atomic); 0 = unknown → commit skips wrapping
    /// (startup, emit-seam tests — region.go screenWidth).
    width: Arc<AtomicU16>,
    /// Shared terminal height (facade atomic); 0 → 24 fallback (region.go screenHeight).
    height: Arc<AtomicU16>,
    /// Committed-pending lines shown in the frame.
    pub(crate) tail: Vec<String>,
    /// Stale preview rows awaiting in-place replacement.
    pub(crate) residue: Vec<String>,
    /// Preview header label (`""` = no preview).
    pub(crate) label: String,
    /// Preview rolling source lines (≤ [`PREVIEW_WINDOW`]).
    pub(crate) preview_tail: Vec<String>,
    /// Preview receiving lines (false once closed/deferred).
    pub(crate) open: bool,
    /// Call preview lifecycle start (`None` = plain preview).
    pub(crate) since: Option<Instant>,
    /// Call clock frozen here (user being consulted).
    pub(crate) paused_at: Option<Instant>,
    /// Call preview live status-row prefix.
    pub(crate) detail: String,
}

impl Region {
    /// A fresh, empty window publishing through `emit`, reading the terminal geometry
    /// from the shared `width`/`height` atomics (Go's `r.u` reads).
    pub(crate) fn new(emit: Emit, width: Arc<AtomicU16>, height: Arc<AtomicU16>) -> Self {
        Self {
            emit,
            width,
            height,
            tail: Vec::new(),
            residue: Vec::new(),
            label: String::new(),
            preview_tail: Vec::new(),
            open: false,
            since: None,
            paused_at: None,
            detail: String::new(),
        }
    }

    /// Preview row count in the window: header + body rows + the model-rendered
    /// `"⎿ elapsed"` status row for call previews (region.go:61-70).
    fn preview_rows(&self) -> usize {
        if self.label.is_empty() {
            return 0;
        }
        let mut n = 1 + self.preview_tail.len();
        if self.since.is_some() {
            n += 1;
        }
        n
    }

    /// Lets `n` freshly displayed rows overwrite the oldest residue rows — the in-place
    /// replacement that keeps the window height flat (region.go:74-80).
    fn consume_residue(&mut self, n: usize) {
        if n >= self.residue.len() {
            self.residue.clear();
            return;
        }
        self.residue.drain(..n);
    }

    /// The staging window's LIVE height cap (T-40 — no Go twin).
    ///
    /// [`TAIL_KEEP`] on any terminal with room for it, trimmed on a short one so the
    /// frame always leaves `max(2, screen_h/2)` rows above it — exactly the batch size
    /// [`chunk_overflow`] inserts. Without the trim, a frame taller than
    /// `screen_h − batch` overlaps the rows `insert_before` is scrolling out and the
    /// scrollback is permanently damaged; below ~12 rows that was every frame.
    fn tail_keep(&self) -> usize {
        let h = self.screen_height();
        let insert_room = (h / 2).max(2);
        TAIL_KEEP.min(h.saturating_sub(insert_room).saturating_sub(FRAME_FLOOR))
    }

    /// Keeps `tail + residue + preview_rows ≤ tail_keep()` by overflowing the oldest
    /// tail lines; returns the overflow to publish (region.go:84-91).
    fn rebalance(&mut self) -> Vec<String> {
        let cap = self.tail_keep();
        let mut over = Vec::new();
        while self.tail.len() + self.residue.len() + self.preview_rows() > cap
            && !self.tail.is_empty()
        {
            over.push(self.tail.remove(0));
        }
        over
    }

    /// Re-fits the window after a terminal resize: staged rows wider than the NEW width
    /// are rewrapped in place (they render inside the frame, where the cell grid would
    /// clip them), and [`Region::tail_keep`] is re-applied — the cap moves with the
    /// height (T-40) and a rewrap may overfill it. Whatever that pushes out is published,
    /// and so is a rewrapped window. Nothing is dropped: every staged row either stays
    /// staged or overflows into the scrollback, exactly as new output would move it.
    ///
    /// This replaced flushing the tail on a width change. That flush existed because the
    /// frame's reflowed top rows ghosted above it; the resize pass now re-anchors the
    /// frame and redraws the window inside it (`term.rs` W5), and a flush on top of that
    /// only committed the same rows a second time below the ghost.
    pub(crate) fn retrim(&mut self) {
        let tail = std::mem::take(&mut self.tail);
        let before = tail.len();
        self.tail = self.fit_width(tail);
        let rewrapped = self.tail.len() != before;
        let over = self.rebalance();
        if over.is_empty() && !rewrapped {
            return;
        }
        self.publish(over);
    }

    /// Deep-copied display snapshot (region.go:93-103).
    pub(crate) fn snapshot(&self) -> RegionSnapshot {
        RegionSnapshot {
            tail: self.tail.clone(),
            residue: self.residue.clone(),
            label: self.label.clone(),
            preview_tail: self.preview_tail.clone(),
            since: self.since,
            paused_at: self.paused_at,
            detail: self.detail.clone(),
        }
    }

    /// Hard-wraps entries wider than the CURRENT screen to width−1 (see [`Region::commit`]);
    /// rows that fit pass through untouched, and width ≤ 1 (startup, tests) skips it.
    fn fit_width(&self, lines: Vec<String>) -> Vec<String> {
        let w = self.screen_width();
        if w <= 1 || lines.iter().all(|ln| ansi_width(ln) <= w) {
            return lines;
        }
        let mut wrapped = Vec::with_capacity(lines.len() + 4);
        for ln in lines {
            if ansi_width(&ln) <= w {
                wrapped.push(ln);
            } else {
                wrapped.extend(wrap_ansi(&ln, w - 1));
            }
        }
        wrapped
    }

    /// Emits the overflow and the fresh snapshot (region.go:115-128). The live path
    /// CHUNKS the overflow below the screen height — a single scrollback insert taller
    /// than the region is the unclamped-scroll hazard class (kept per T-06); the
    /// snapshot always follows the chunks, and the caller's `Mutex<Region>` guard spans
    /// this whole call, preserving the Go publish-ordering law. `sanitizeOverflow` is
    /// NOT ported (T-01) and blank rows pass through as-is (T-02).
    fn publish(&mut self, over: Vec<String>) {
        // Staged rows were wrapped at the width they were COMMITTED at; a narrowing since
        // (the resize pass flushes the tail) must not hand the insert a row that wraps
        // again — W6's one-entry-one-row law (term.rs) holds at the width it lands at.
        let over = self.fit_width(over);
        if !over.is_empty() {
            debug_region(|| format!("  overflow {over:?}"));
        }
        let h = self.screen_height();
        let snap = self.snapshot();
        match &mut self.emit {
            #[cfg(test)]
            Emit::Test(f) => f(over, snap),
            Emit::Live { tx } => {
                for chunk in chunk_overflow(over, h) {
                    tx.scrollback(chunk);
                }
                tx.region(snap);
            }
        }
    }

    /// Screen height with the 24-row fallback (region.go:151-159).
    fn screen_height(&self) -> usize {
        let h = self.height.load(Ordering::Relaxed);
        if h > 0 { usize::from(h) } else { 24 }
    }

    /// Screen width; 0 = unknown (region.go:161-166).
    fn screen_width(&self) -> usize {
        usize::from(self.width.load(Ordering::Relaxed))
    }

    /// Flows lines through the window (region.go:252-291). With a (possibly
    /// deferred-closed) preview present, this is the in-place morph: the lines cover
    /// the header row first, then the preview rows top-down; preview rows the block's
    /// lines don't reach stay as residue for later lines — height constant either way.
    /// A call preview's status row contributes a blank placeholder so its vanishing
    /// never shrinks the window.
    ///
    /// Overwide entries are hard-wrapped on the way in (staged tail rows render inside
    /// the frame where the cell grid CLIPS overwide lines, while overflowed rows
    /// soft-wrap in the terminal): rows that already fit (markdown, user blocks) pass
    /// through untouched, and wrapping targets width−1 — paired with markdown's width−1
    /// table law. Width ≤ 1 (startup, tests) skips wrapping.
    pub(crate) fn commit(&mut self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        let lines = self.fit_width(split_rows(lines));
        debug_region(|| {
            format!(
                "commit {lines:?} label={:?} open={} residue={}",
                self.label,
                self.open,
                self.residue.len()
            )
        });
        if !self.label.is_empty() && !self.open {
            // Deferred preview close: its replacement content has arrived.
            let mut rows = self.preview_tail.clone();
            if self.since.is_some() {
                rows.push(String::new());
            }
            let covered = lines.len() - 1;
            if covered < rows.len() {
                self.residue.extend(rows.drain(covered..));
            }
            self.label.clear();
            self.preview_tail.clear();
            self.since = None;
            self.paused_at = None;
            self.detail.clear();
        } else {
            self.consume_residue(lines.len());
        }
        self.tail.extend(lines);
        let over = self.rebalance();
        self.publish(over);
    }

    /// Starts a block preview (header + rolling source lines). Opening over an existing
    /// preview folds that one into residue — the new header takes the old header's row,
    /// the old rows await replacement — so back-to-back previews never move the
    /// composer (region.go:297-305).
    pub(crate) fn open_preview(&mut self, label: &str) {
        self.fold_open(label);
        self.since = None;
        self.paused_at = None;
        let over = self.rebalance(); // header row may steal a tail line
        self.publish(over);
    }

    /// Ensures the tool-call lifecycle widget: a header plus the model-rendered
    /// `"⎿ [detail ·] elapsed · ESC"` status row. When a call preview is already open
    /// this relabels it in place (the clock and detail keep running — a composing call
    /// expanding to its full header); otherwise it fold-opens fresh with a cleared
    /// detail (region.go:312-328).
    pub(crate) fn open_call_preview(&mut self, label: &str) {
        debug_region(|| {
            format!(
                "openCallPreview {label:?} ensure={}",
                !self.label.is_empty() && self.since.is_some()
            )
        });
        if !self.label.is_empty() && self.since.is_some() {
            self.label = one_row(label);
            self.open = true;
            self.publish(Vec::new());
            return;
        }
        self.fold_open(label);
        self.since = Some(Instant::now());
        self.paused_at = None;
        self.detail.clear();
        let over = self.rebalance();
        self.publish(over);
    }

    /// Updates a PLAIN preview's header in place (the live `"rendering table… · N
    /// lines"` counter). A no-op for call previews (they relabel through
    /// [`Region::open_call_preview`]) and once the preview closed — a throttled counter
    /// racing the flush must not resurrect the header (region.go:334-342).
    pub(crate) fn relabel_preview(&mut self, label: &str) {
        if self.label.is_empty() || self.since.is_some() || !self.open {
            return;
        }
        self.label = one_row(label);
        self.publish(Vec::new());
    }

    /// Freezes the call widget's elapsed figure (the user is being consulted — an
    /// approval prompt, an interactive tool's surface — and human deliberation must not
    /// count as activity time). A no-op without a live call preview or when already
    /// paused (region.go:348-356).
    pub(crate) fn pause_clock(&mut self) {
        if self.since.is_none() || self.paused_at.is_some() {
            return;
        }
        self.paused_at = Some(Instant::now());
        self.publish(Vec::new());
    }

    /// Restarts a paused clock, shifting the start forward by the paused span so the
    /// elapsed figure continues where it froze (region.go:360-371).
    pub(crate) fn resume_clock(&mut self) {
        let Some(paused) = self.paused_at else {
            return;
        };
        if let Some(since) = self.since {
            self.since = Some(since + paused.elapsed());
        }
        self.paused_at = None;
        self.publish(Vec::new());
    }

    /// Updates the call preview's live status-row prefix (`"1.2k tokens"`); a no-op
    /// unless a call preview is up and still receiving — a throttled meter update
    /// racing the settle must not resurrect the row (region.go:395-403).
    pub(crate) fn set_call_detail(&mut self, detail: &str) {
        if self.label.is_empty() || self.since.is_none() || !self.open {
            return;
        }
        self.detail = one_row(detail);
        self.publish(Vec::new());
    }

    /// Replaces the call preview's body rows wholesale (region.go:377-388): each progressive
    /// image frame supersedes the last, so the widget refines in place instead of scrolling.
    ///
    /// A no-op unless a call preview is up and still receiving — a frame racing the settle must
    /// not resurrect the widget. Bounded by the caller (frames are 64×12, so ≤ 12 rows); rows go
    /// through [`one_row`] like every preview entry, then the window is rebalanced and published.
    pub(crate) fn set_call_body(&mut self, rows: Vec<String>) {
        if self.label.is_empty() || self.since.is_none() || !self.open {
            return;
        }
        self.preview_tail = rows.into_iter().map(|ln| one_row(&ln)).collect();
        let over = self.rebalance();
        self.publish(over);
    }

    /// Replaces any current preview with a fresh one, folding the old rows (and a
    /// status-row placeholder for call previews) into residue (region.go:407-419).
    fn fold_open(&mut self, label: &str) {
        if self.label.is_empty() {
            self.consume_residue(1); // the header row overwrites a residue row
        } else {
            self.residue.append(&mut self.preview_tail);
            if self.since.is_some() {
                self.residue.push(String::new());
            }
        }
        self.label = one_row(label);
        self.preview_tail.clear();
        self.open = true;
    }

    /// Appends a raw source line to the rolling preview window (region.go:422-436).
    pub(crate) fn preview_line(&mut self, line: &str) {
        if !self.open {
            return;
        }
        self.preview_tail.push(one_row(line));
        if self.preview_tail.len() > PREVIEW_WINDOW {
            let excess = self.preview_tail.len() - PREVIEW_WINDOW;
            self.preview_tail.drain(..excess);
        } else {
            self.consume_residue(1); // a genuinely new row overwrites a residue row
        }
        let over = self.rebalance(); // growth steals tail lines: commit, never shrink
        self.publish(over);
    }

    /// Marks the preview finished but KEEPS it on screen: the next commit replaces it
    /// in place (the morph). This is what makes the flush bounce-free — nothing shrinks
    /// between the source window and the rendered block (region.go:442-447).
    pub(crate) fn close_preview(&mut self) {
        debug_region(|| format!("closePreview label={:?}", self.label));
        self.open = false;
    }

    /// Discards a preview and any residue outright (interrupted turn, end of turn): the
    /// one shrink this design keeps, on turn boundaries (region.go:451-465).
    pub(crate) fn drop_preview(&mut self) {
        debug_region(|| {
            format!(
                "dropPreview label={:?} residue={}",
                self.label,
                self.residue.len()
            )
        });
        if self.label.is_empty() && self.residue.is_empty() {
            return;
        }
        self.label.clear();
        self.preview_tail.clear();
        self.residue.clear();
        self.open = false;
        self.since = None;
        self.paused_at = None;
        self.publish(Vec::new());
    }

    /// Commits the staged tail into scrollback while KEEPING an open preview. Called before
    /// every tabbed open; the tail refills from subsequent output (region.go:472-481 —
    /// Go also called it on a width change, which [`Region::retrim`] now covers).
    pub(crate) fn flush_tail(&mut self) {
        if self.tail.is_empty() {
            return;
        }
        let over = std::mem::take(&mut self.tail);
        self.publish(over);
    }

    /// Commits everything still staged (shutdown: scrollback must hold the full
    /// transcript). Preview rows and residue are display-only — dropped, not flushed
    /// (region.go:486-499).
    pub(crate) fn flush(&mut self) {
        let over = std::mem::take(&mut self.tail);
        self.residue.clear();
        if !self.label.is_empty() {
            self.label.clear();
            self.preview_tail.clear();
        }
        self.since = None;
        self.paused_at = None;
        self.publish(over);
    }
}

/// Expands entries with embedded newlines into one entry per row. All window
/// bookkeeping (tail height, rebalance, overflow row counts — and through them the
/// frame anchor and the composer cursor) assumes one visual row per entry; a
/// multi-line entry silently desyncs them all (region.go:172-188).
pub(crate) fn split_rows(lines: Vec<String>) -> Vec<String> {
    if lines.iter().all(|ln| !ln.contains('\n')) {
        return lines;
    }
    let mut out = Vec::with_capacity(lines.len() + 4);
    for ln in &lines {
        out.extend(ln.split('\n').map(str::to_owned));
    }
    out
}

/// Collapses embedded line breaks into spaces. Every frame row the model renders — the
/// preview label, its rolling source lines, the status-row detail — is counted as
/// exactly one visual line in `rows_above` (the composer cursor offset); a newline
/// smuggled in by a producer would desync the frame anchor just like an unsplit
/// multi-line commit (region.go:196-203).
pub(crate) fn one_row(s: &str) -> String {
    if !s.contains(['\r', '\n']) {
        return s.to_owned();
    }
    s.replace("\r\n", " ").replace(['\n', '\r'], " ")
}

/// Splits overflow lines into batches of at most `max(2, h/2)` lines (region.go:206-223,
/// CHUNK const, `TUI_CONTRACTS` §8): a single scrollback insert taller than the screen
/// is the frame-anchor desync hazard class — kept as cheap insurance (T-06).
pub(crate) fn chunk_overflow(over: Vec<String>, h: usize) -> Vec<Vec<String>> {
    if over.is_empty() {
        return Vec::new();
    }
    let size = (h / 2).max(2);
    let mut chunks = Vec::with_capacity(over.len().div_ceil(size));
    let mut it = over.into_iter().peekable();
    while it.peek().is_some() {
        chunks.push(it.by_ref().take(size).collect());
    }
    chunks
}

#[cfg(test)]
mod tests;
