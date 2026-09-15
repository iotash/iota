//! The metered block-preview sink — a port of internal/ui/sink.go: the
//! [`crate::ui::facade::UiStreamSink`] implementation plus the [`PreviewWriter`] that meters
//! a streaming block into ONE header row (`"rendering table… · 37 lines"`), never a
//! rolling window of raw source. A one-row preview is always covered by the rendered
//! block's morph, so the residue/shrink class — a short list collapsing under its own
//! preview at end of turn — cannot occur.
//!
//! Divergence note: Go's `previewWriter` was an `io.Writer` counting `'\n'` bytes with a
//! `partial` flag; the facade [`PreviewHandle`] is line-based (`write_raw_line`, one call
//! per consumed source line — `TUI_CONTRACTS` §2), so the counter increments per call and
//! partial-line tracking lives in the producer. The throttle and first-tick laws are
//! unchanged.

use std::sync::{Arc, Mutex};

use crate::sync::lock;
use std::time::{Duration, Instant};

use crate::ui::facade::{PreviewHandle, UiStreamSink};

use super::region::Region;
use super::theme::{FAINT, RESET};

/// Throttles the preview header's line counter, mirroring the thinking meter's cadence
/// (sink.go:31; `TUI_CONTRACTS` §8 `PREVIEW_COUNTER_EVERY`).
pub(crate) const PREVIEW_COUNTER_EVERY: Duration = Duration::from_millis(150);

/// The [`UiStreamSink`] implementation (sink.go `streamSink`): fire-and-forget region
/// calls; ordering rides the region lock + the loop mailbox FIFO, so committed lines,
/// preview updates, and the closing `done` arrive in call order.
pub(crate) struct StreamSink {
    region: Arc<Mutex<Region>>,
    /// Pops the turn cancel scope (Go `Send(scopePopMsg{})`, sink.go:26); WP45 wires it
    /// to the loop mailbox.
    on_done: Box<dyn Fn() + Send + Sync>,
}

impl StreamSink {
    /// A sink over the shared region; `on_done` runs once per [`UiStreamSink::done`]
    /// after the leaked/deferred preview is dropped.
    pub(crate) fn new(
        region: Arc<Mutex<Region>>,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            region,
            on_done: Box::new(on_done),
        }
    }
}

impl UiStreamSink for StreamSink {
    fn block_preview(&self, label: &str) -> Box<dyn PreviewHandle> {
        lock(&self.region).open_preview(label);
        // `last` starts NOW: the first counter update waits a full throttle tick, so a
        // block that flushes quickly never even shows one — zero churn for the
        // short-block case the single-row preview exists to protect (sink.go:18-21).
        Box::new(PreviewWriter {
            region: Arc::clone(&self.region),
            base: label.to_owned(),
            lines: 0,
            last: Instant::now(),
            closed: false,
        })
    }

    fn done(&self) {
        lock(&self.region).drop_preview(); // a leaked/deferred preview dies with the turn
        (self.on_done)();
    }
}

/// Meters the source streaming into a block preview (sink.go `previewWriter`): counts
/// raw source lines and relabels the header every ≥ [`PREVIEW_COUNTER_EVERY`] with
/// `base + faint + " · " + count_lines(n) + reset` (sink.go:57).
pub(crate) struct PreviewWriter {
    /// The shared staging window.
    pub(crate) region: Arc<Mutex<Region>>,
    /// The bare label the counter suffix rides on.
    pub(crate) base: String,
    /// Raw source lines counted so far.
    pub(crate) lines: usize,
    /// Last relabel instant; starts at open time so the first tick waits a full period.
    pub(crate) last: Instant,
    /// Guards the deferred close against a late `Drop` closing a NEWER preview.
    pub(crate) closed: bool,
}

impl PreviewHandle for PreviewWriter {
    fn write_raw_line(&mut self, _line: &str) {
        self.lines += 1;
        if self.last.elapsed() >= PREVIEW_COUNTER_EVERY {
            self.last = Instant::now();
            let label = format!("{}{FAINT} · {}{RESET}", self.base, count_lines(self.lines));
            lock(&self.region).relabel_preview(&label);
        }
    }

    /// Marks the preview finished; the window keeps showing it until the rendered block
    /// flows through and replaces it in place (`Region::commit` morph; sink.go:71-74).
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        lock(&self.region).close_preview();
    }
}

/// Drop = close (facade contract): a handle leaked without an explicit `close` still
/// defer-closes its preview exactly once.
impl Drop for PreviewWriter {
    fn drop(&mut self) {
        self.close();
    }
}

/// `"1 line"` / `"N lines"` counter wording (sink.go:62-67).
pub(crate) fn count_lines(n: usize) -> String {
    if n == 1 {
        "1 line".to_owned()
    } else {
        format!("{n} lines")
    }
}
