//! The unit of the conversation (tui-plan S7a): what the app commits and what the full screen's
//! buffer holds. A block is either a fixed ROW, or something LAID OUT from its source at whatever
//! width the screen has when it is drawn.
//!
//! - **A row** is one logical line as it was written: a paragraph line, a heading, a notice, an
//!   activity row, a separator blank. Nothing in it depends on the width, so the screen wraps it
//!   when it is wider than the viewport and shows it whole when it is not — widening re-flows it.
//! - **A laid-out block** keeps its source (a table's cells, a list's items, a code block's lines,
//!   a quote's inner document, the banner's facts, a diff's hunks, the user's text) and a
//!   [`Layout`] that renders it at a width. The buffer calls it at the width of the draw and keeps
//!   the result until the width changes, so a width change re-lays-out what is on screen from its
//!   source, not from the rows of another width.
//!
//! The block's kind is its source's type — the [`Layout`] implementation (`markdown::Fragment`,
//! the banner, the user block, the diff listing, the error block); nothing downstream branches on
//! it, so no separate tag is carried.

use std::sync::Arc;

use crate::text::ansi::wrap_ansi;

/// Lays a block out from its source at a width.
pub trait Layout: Send + Sync {
    /// The block's rows at `width` columns (an empty `Vec` for a block with nothing to show). A
    /// row wider than `width` is wrapped by the screen, but a layout fits its rows itself — that
    /// is the point of having one.
    fn lines(&self, width: usize) -> Vec<String>;
}

/// One unit of the conversation (module docs).
#[derive(Clone)]
pub enum Block {
    /// A fixed row, wrapped by the screen when it is wider than the viewport.
    Row(String),
    /// A block laid out from its source at the width of the draw.
    Laid(Arc<dyn Layout>),
}

impl Block {
    /// A block laid out by `layout`.
    pub fn laid(layout: impl Layout + 'static) -> Self {
        Self::Laid(Arc::new(layout))
    }

    /// One row per line.
    pub fn rows<S: Into<String>>(lines: impl IntoIterator<Item = S>) -> Vec<Self> {
        lines.into_iter().map(|l| Self::Row(l.into())).collect()
    }

    /// The block's lines at `width` as it would commit them: a row as written, a laid-out block
    /// rendered at `width`. (The screen then wraps whatever is still wider than the viewport —
    /// [`Block::display`].)
    pub fn render(&self, width: usize) -> Vec<String> {
        match self {
            Self::Row(t) => vec![t.clone()],
            Self::Laid(l) => l.lines(width),
        }
    }

    /// The block's display lines at `width`: [`Block::render`], every line wider than `width`
    /// wrapped (`wrap_ansi` carries the SGR across the break) — each with whether it CONTINUES the
    /// line before it (a segment the screen's wrap made) or starts a line of its own. A copy joins
    /// the continuations back and separates the rest.
    pub fn display(&self, width: usize) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for line in self.render(width) {
            if width > 0 && crate::text::ansi::ansi_width(&line) > width {
                out.extend(
                    wrap_ansi(&line, width)
                        .into_iter()
                        .enumerate()
                        .map(|(i, seg)| (seg, i > 0)),
                );
            } else {
                out.push((line, false));
            }
        }
        out
    }
}

/// Assistant markdown: a closed buffering block laid out from its source.
impl Layout for crate::markdown::Fragment {
    fn lines(&self, width: usize) -> Vec<String> {
        Self::lines(self, width)
    }
}

impl From<String> for Block {
    fn from(s: String) -> Self {
        Self::Row(s)
    }
}

impl From<&str> for Block {
    fn from(s: &str) -> Self {
        Self::Row(s.to_owned())
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Row(t) => f.debug_tuple("Row").field(t).finish(),
            Self::Laid(l) => f.debug_tuple("Laid").field(&l.lines(80)).finish(),
        }
    }
}
