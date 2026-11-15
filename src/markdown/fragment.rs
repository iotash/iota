//! A buffering block once it has closed, kept as its SOURCE (tui-plan S7a): the five
//! width-dependent elements of a reply — a fenced code block, a table, a list, a quote, a display
//! formula — render from it at whatever width they are laid out at, so a terminal that changes
//! width re-lays each one out instead of wrapping the rows of another width.
//!
//! The [`crate::markdown::Writer`] builds one when a block flushes and hands it to its sink
//! ([`crate::markdown::Sink::write_block`]); a sink that only needs text writes
//! [`Fragment::render`] at its own width, the terminal's keeps the fragment.

use crate::markdown::RenderOptions;
use crate::markdown::blocks::code::indent_code;
use crate::markdown::blocks::list::{ListItem, render_list};
use crate::markdown::blocks::math::render_math;
use crate::markdown::blocks::quote::render_quote;
use crate::markdown::blocks::table::render_table;

/// What a fragment renders from.
pub(crate) enum Source {
    /// A fenced code block, already highlighted (the highlight does not depend on the width).
    Code(String),
    /// A table's parsed cells and its `|---|` row flags.
    Table {
        rows: Vec<Vec<String>>,
        seps: Vec<bool>,
    },
    /// A list's parsed items.
    List { items: Vec<ListItem>, loose: bool },
    /// A quote's inner lines, markers stripped (a mini-document).
    Quote(Vec<String>),
    /// A display formula's source.
    Math(String),
}

/// One closed buffering block: its source and the options it renders with.
pub struct Fragment {
    src: Source,
    opts: RenderOptions,
}

impl Fragment {
    pub(crate) const fn new(src: Source, opts: RenderOptions) -> Self {
        Self { src, opts }
    }

    /// The block laid out at `width` columns, trailing newline included — exactly what the
    /// streaming renderer writes for it at that width.
    pub fn render(&self, width: usize) -> String {
        let width = if width == 0 { 80 } else { width };
        let color = self.opts.color;
        match &self.src {
            Source::Code(code) => indent_code(code, width),
            Source::Table { rows, seps } => {
                format!("{}\n", render_table(rows, seps, width, color))
            }
            Source::List { items, loose } => {
                format!("{}\n", render_list(items, *loose, color, width))
            }
            Source::Quote(body) => format!("{}\n", render_quote(body, width, self.opts)),
            Source::Math(src) => render_math(src, width),
        }
    }

    /// [`Fragment::render`] as rows, its trailing newline dropped.
    pub fn lines(&self, width: usize) -> Vec<String> {
        let out = self.render(width);
        let out = out.strip_suffix('\n').unwrap_or(&out);
        out.split('\n').map(str::to_owned).collect()
    }
}
