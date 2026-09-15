//! The metered block-preview handle, defined where it is CONSUMED (Phase 5 PR-5): the markdown
//! renderer opens one per buffering block through [`crate::markdown::Sink::block_preview`], counts
//! raw source lines into it and closes it once per flush. The terminal layer implements it
//! (`ui::sink::PreviewWriter`) and re-exports it from its facade; `testing::ScriptedPreview`
//! doubles it. Until this file existed the trait lived in `ui::facade` and `markdown::sink`
//! re-exported it — the one edge from the renderer UP into the terminal layer. Now the edge points
//! down: `ui` depends on `markdown`, never the reverse (the brain's `rust-arch-cleanup` reversal
//! records the decision).

/// Metered block-preview handle (sink.go `previewWriter` contract): the writer COUNTS raw
/// source lines; one row `"label · N lines"`; 150ms throttle, FIRST tick delayed a full
/// period; close is deferred (the row stays until the rendered block morphs it). Drop = close.
/// `Send` because a `Writer` parks its open previews inside itself and the interactive renderer
/// lives in a `StreamSink`.
pub trait PreviewHandle: Send {
    /// Counts one raw source line into the metered row.
    fn write_raw_line(&mut self, line: &str);
    /// Deferred close: the row stays until the rendered block morphs it.
    fn close(&mut self);
}
