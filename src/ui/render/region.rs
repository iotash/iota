//! The frame's preview — what the full screen shows below the conversation while a tool call
//! runs (a port of internal/ui/region.go, cut down by tui-plan S7b).
//!
//! Committed blocks pass straight through to the conversation buffer ([`Region::commit`]): a row
//! is wrapped and a laid-out block rendered by the screen at the width of each draw, and a block
//! still being written is a live block the buffer replaces in place as it grows. Nothing is staged
//! in the frame any more — the staging window (the last rows held above the composer, the
//! residue, the overflow chunking) existed because a row inserted into an inline terminal's
//! history could never be taken back, and a buffered markdown block could only meter its progress
//! there; the buffer takes rows back now.
//!
//! What the region still holds is the CALL PREVIEW: the tool-call lifecycle widget, a header plus
//! the rolling body rows and the live `"⎿ elapsed"` status row the frame renders. Its close is
//! deferred — the widget stays up until the next commit, its result, replaces it — so settling a
//! call never leaves the frame without one or the other.
//!
//! Concurrency: the handle wraps the struct in a `Mutex<Region>` (`TUI_CONTRACTS` §5) and every
//! method publishes before it returns, so the caller's guard spans mutation AND emission, and
//! concurrent writers (stream task, MCP reporter) serialize on that one lock while the single
//! mailbox consumer preserves arrival order. [`Region::set_call_body`] (T-16) replaces the preview
//! body wholesale for progressive image frames, beside the rolling [`Region::preview_line`] law.

use std::time::Instant;

use crate::ui::block::Block;

use super::debug::debug_region;

/// The rolling preview body keeps at most this many source lines (region.go:35).
pub(crate) const PREVIEW_WINDOW: usize = 3;

/// Test seam callback: receives the committed blocks rendered at 80 columns and the fresh
/// snapshot, exactly like Go's non-nil `region.emit` (region.go:39,120-122).
#[cfg(test)]
pub(crate) type EmitFn = Box<dyn FnMut(Vec<String>, RegionSnapshot) + Send>;

/// The live publish seam, implemented over the loop's `UiMsg` mailbox (`UiMsg::Scrollback`,
/// then `UiMsg::Region`); the mailbox FIFO plus the region lock preserve the ordering Go's
/// mutex-held `Println`+`Send` guaranteed.
pub(crate) trait LivePublish: Send {
    /// One batch of committed blocks.
    fn scrollback(&self, rows: Vec<Block>);
    /// The fresh display snapshot (sent after every mutation).
    fn region(&self, snap: RegionSnapshot);
}

/// Where a publish goes: the unit-test capture seam, or the live loop mailbox
/// (region.go `emit` nil/non-nil split; `TUI_CONTRACTS` §5 `Emit{Test, Live}`).
pub(crate) enum Emit {
    /// Test seam: captures the committed rows + snapshot.
    #[cfg(test)]
    Test(EmitFn),
    /// Live path: the committed blocks, then the snapshot.
    Live {
        /// The loop-mailbox publisher (see [`LivePublish`]).
        tx: Box<dyn LivePublish>,
    },
}

/// The display snapshot the model renders (region.go `regionMsg`): a deep copy taken
/// under the lock; the frame renders only this.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RegionSnapshot {
    /// Preview header label (`""` = no preview).
    pub(crate) label: String,
    /// Preview rolling source lines (≤ [`PREVIEW_WINDOW`]).
    pub(crate) preview_tail: Vec<String>,
    /// Call preview lifecycle start; `Some` = render the `"⎿ elapsed"` row and tick.
    pub(crate) since: Option<Instant>,
    /// `Some` = the elapsed figure freezes here (user being consulted).
    pub(crate) paused_at: Option<Instant>,
    /// Call preview live status-row prefix (`"1.2k tokens"`).
    pub(crate) detail: String,
}

/// The call preview's state (region.go `region`). Lives in a `Mutex<Region>` inside the facade
/// handle; all methods mutate and publish while the caller holds that lock.
pub(crate) struct Region {
    emit: Emit,
    /// Preview header label (`""` = no preview).
    pub(crate) label: String,
    /// Preview rolling source lines (≤ [`PREVIEW_WINDOW`]).
    pub(crate) preview_tail: Vec<String>,
    /// Preview receiving lines (false once closed/deferred).
    pub(crate) open: bool,
    /// Call preview lifecycle start (`None` = no preview).
    pub(crate) since: Option<Instant>,
    /// Call clock frozen here (user being consulted).
    pub(crate) paused_at: Option<Instant>,
    /// Call preview live status-row prefix.
    pub(crate) detail: String,
}

impl Region {
    /// A fresh region with no preview, publishing through `emit`.
    pub(crate) fn new(emit: Emit) -> Self {
        Self {
            emit,
            label: String::new(),
            preview_tail: Vec::new(),
            open: false,
            since: None,
            paused_at: None,
            detail: String::new(),
        }
    }

    /// Deep-copied display snapshot (region.go:93-103).
    pub(crate) fn snapshot(&self) -> RegionSnapshot {
        RegionSnapshot {
            label: self.label.clone(),
            preview_tail: self.preview_tail.clone(),
            since: self.since,
            paused_at: self.paused_at,
            detail: self.detail.clone(),
        }
    }

    /// Emits the committed blocks (if any) and the fresh snapshot; the caller's `Mutex<Region>`
    /// guard spans this whole call, preserving the Go publish-ordering law.
    fn publish(&mut self, blocks: Vec<Block>) {
        let snap = self.snapshot();
        match &mut self.emit {
            #[cfg(test)]
            Emit::Test(f) => f(blocks.iter().flat_map(|b| b.render(80)).collect(), snap),
            Emit::Live { tx } => {
                if !blocks.is_empty() {
                    tx.scrollback(blocks);
                }
                tx.region(snap);
            }
        }
    }

    /// Commits blocks to the conversation as they are: a row with embedded newlines becomes one
    /// row per line, everything else passes through untouched. A preview whose close was deferred
    /// comes down now — this is its replacement arriving (the morph).
    pub(crate) fn commit<B: Into<Block>>(&mut self, blocks: Vec<B>) {
        if blocks.is_empty() {
            return;
        }
        let blocks = split_blocks(blocks.into_iter().map(Into::into).collect());
        debug_region(|| format!("commit {blocks:?} label={:?}", self.label));
        if !self.label.is_empty() && !self.open {
            self.end_preview();
        }
        self.publish(blocks);
    }

    /// Takes the preview down (its rows are replaced by what was just committed).
    fn end_preview(&mut self) {
        self.label.clear();
        self.preview_tail.clear();
        self.since = None;
        self.paused_at = None;
        self.detail.clear();
    }

    /// Ensures the tool-call lifecycle widget: a header plus the model-rendered
    /// `"⎿ [detail ·] elapsed · ESC"` status row. When a call preview is already open
    /// this relabels it in place (the clock and detail keep running — a composing call
    /// expanding to its full header); otherwise it opens fresh, over any closing one, with a
    /// cleared detail (region.go:312-328).
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
        self.label = one_row(label);
        self.preview_tail.clear();
        self.open = true;
        self.since = Some(Instant::now());
        self.paused_at = None;
        self.detail.clear();
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
    /// The rows keep the order they are given, top to bottom: they are a picture, not a log, so
    /// [`Region::preview_line`]'s newest-first roll does not apply to them.
    ///
    /// A no-op unless a call preview is up and still receiving — a frame racing the settle must
    /// not resurrect the widget. Bounded by the caller (frames are 64×12, so ≤ 12 rows); rows go
    /// through [`one_row`] like every preview entry, then the snapshot is published.
    pub(crate) fn set_call_body(&mut self, rows: Vec<String>) {
        if self.label.is_empty() || self.since.is_none() || !self.open {
            return;
        }
        self.preview_tail = rows.into_iter().map(|ln| one_row(&ln)).collect();
        self.publish(Vec::new());
    }

    /// Rolls a raw source line into the preview window newest-first (X-118; Go appended,
    /// region.go:422-436): the new row lands at the front — directly under the header, where the
    /// eye already is — the older rows move down, and the oldest drops off the end.
    pub(crate) fn preview_line(&mut self, line: &str) {
        if !self.open {
            return;
        }
        self.preview_tail.insert(0, one_row(line));
        self.preview_tail.truncate(PREVIEW_WINDOW);
        self.publish(Vec::new());
    }

    /// Marks the preview finished but KEEPS it on screen: the next commit replaces it
    /// (the morph), so nothing shrinks between the widget and its result (region.go:442-447).
    pub(crate) fn close_preview(&mut self) {
        debug_region(|| format!("closePreview label={:?}", self.label));
        self.open = false;
    }

    /// Discards a preview outright (interrupted turn, end of turn) (region.go:451-465).
    pub(crate) fn drop_preview(&mut self) {
        debug_region(|| format!("dropPreview label={:?}", self.label));
        if self.label.is_empty() {
            return;
        }
        self.end_preview();
        self.open = false;
        self.publish(Vec::new());
    }
}

/// A row with embedded newlines becomes one row per line (the buffer's one-row-per-line law,
/// region.go:172-188); a laid-out block is left whole.
fn split_blocks(blocks: Vec<Block>) -> Vec<Block> {
    if blocks
        .iter()
        .all(|b| !matches!(b, Block::Row(t) if t.contains('\n')))
    {
        return blocks;
    }
    let mut out = Vec::with_capacity(blocks.len() + 4);
    for b in blocks {
        match b {
            Block::Row(t) if t.contains('\n') => out.extend(Block::rows(t.split('\n'))),
            other => out.push(other),
        }
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

#[cfg(test)]
mod tests;
