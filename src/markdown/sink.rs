//! The purity seam (markdown.go:137-141): where rendered output goes, and the metered
//! preview a buffering block opens while it streams. The preview handle is
//! [`PreviewHandle`] (`markdown::preview`), this module's own contract — the renderer
//! consumes it (one raw source line per call, `close` once per flush) and the UI implements it.

use crate::markdown::fragment::Fragment;
use crate::markdown::preview::PreviewHandle;

/// Where rendered output goes. The renderer itself never lays anything out at a width: a closed
/// block reaches the sink as its source ([`Sink::write_block`]). A text sink lays it out at its
/// `width()`; the terminal's keeps the source and lays it out again at every width the screen
/// takes (tui-plan S7a). `block_preview` returns `None` in the
/// piped/test/quote-child shape — no preview, everything else identical.
///
/// `Send` so a `Writer` (which owns a `Box<dyn Sink>`) can live inside the interactive
/// `crate::provider::sink::StreamSink` the provider streams into — that trait is `Send` by
/// contract, and the markdown renderer must run DURING the awaited provider call for the
/// output to stream at all.
pub trait Sink: Send {
    /// Receives one rendered chunk. Infallible (facade commits are fire-and-forget — T-07).
    fn write(&mut self, rendered: &str);
    /// Receives one closed buffering block (code, table, list, quote, display math) as its
    /// source. A text sink writes it laid out at [`Sink::width`]; one that can lay it out again
    /// at another width keeps the source.
    fn write_block(&mut self, block: Fragment) {
        let rendered = block.render(self.width());
        self.write(&rendered);
    }
    /// The width a text sink lays closed blocks out at; `0` → 80.
    fn width(&self) -> usize;
    /// Opens a metered block preview, when this sink meters at all.
    fn block_preview(&mut self, label: &str) -> Option<Box<dyn PreviewHandle>>;
}
