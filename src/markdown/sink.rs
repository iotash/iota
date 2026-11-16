//! The purity seam (markdown.go:137-141): where rendered output goes — rows, closed blocks as
//! their source, and, for a sink that shows them, open blocks as they grow.

use crate::markdown::fragment::Fragment;

/// Where rendered output goes. The renderer itself never lays anything out at a width: a closed
/// block reaches the sink as its source ([`Sink::write_block`]). A text sink lays it out at its
/// `width()`; the terminal's keeps the source and lays it out again at every width the screen
/// takes (tui-plan S7a), and shows an open block as it grows (tui-plan S7b).
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
    /// Whether this sink shows a block while it is still being written: the renderer then hands
    /// it [`Sink::grow_block`] after every line the block takes. A text sink does not.
    fn shows_open_blocks(&self) -> bool {
        false
    }
    /// The open buffering block as it stands (only when [`Sink::shows_open_blocks`]): it replaces
    /// the one given before, and the [`Sink::write_block`] that closes it replaces the last.
    fn grow_block(&mut self, _block: Fragment) {}
}
