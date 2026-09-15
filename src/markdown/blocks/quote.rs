//! Quote renderer: the renderQuote twin (markdown.go:1057-1090) — the inner lines are
//! a mini-document rendered recursively through a CHILD `Writer` at width−2 (lists,
//! headings, tables, code, math, and nested quotes inside a quote reuse the full
//! pipeline), then every visual row is fronted by the cyan `│` bar with one column of
//! padding; overlong plain paragraphs soft-wrap at the pinned width while the child's
//! already-fitted tables/code pass through un-rewrapped; under no-color the bar glyph
//! is still drawn, colorless (`TUI_DESIGN` §7).

use std::sync::{Arc, Mutex, PoisonError};

use crate::markdown::blocks::table::word_wrap_ansi;
use crate::markdown::style::Style;
use crate::markdown::{PreviewHandle, Sink};
use crate::markdown::{RenderOptions, Writer};
use crate::text::ansi::ansi_width;

/// The terminal columns the quote frame adds around its text: the left border glyph
/// (1) plus one column of padding (markdown.go quoteBorderCols).
pub(crate) const QUOTE_BORDER_COLS: usize = 2;

/// The no-preview child sink capturing the recursive mini-document render.
/// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` only because `Sink` is `Send`
/// (DEVIATIONS3 `[WP49]`); the child render is single-threaded and never contends.
struct BufSink {
    out: Arc<Mutex<String>>,
    width: usize,
}

impl Sink for BufSink {
    fn write(&mut self, rendered: &str) {
        self.out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_str(rendered);
    }

    fn width(&self) -> usize {
        self.width
    }

    fn block_preview(&mut self, _label: &str) -> Option<Box<dyn PreviewHandle>> {
        None // the quote-child shape never previews
    }
}

/// renderQuote twin (markdown.go:1064-1090). The child renders at
/// `inner = width − QUOTE_BORDER_COLS` (min 3) so nested blocks fit inside the frame;
/// the whole content then renders behind the bar with the style width pinned to
/// `width − 1` (min 3 — the content area including the one-column left padding,
/// excluding the border glyph), so text wraps at width−2 and the `│` bar is drawn on
/// EVERY visual row. The child already laid out tables/code to at most `inner`
/// columns, so those rows never exceed the pin and pass through intact — only
/// overlong paragraphs (which the child leaves unwrapped) soft-wrap. Rows pad to the
/// pinned width, the lipgloss block shape.
pub(crate) fn render_quote(body: &[String], width: usize, opts: RenderOptions) -> String {
    let inner = width
        .saturating_sub(QUOTE_BORDER_COLS)
        .max(QUOTE_BORDER_COLS + 1);
    let out = Arc::new(Mutex::new(String::new()));
    let mut child = Writer::new(
        Box::new(BufSink {
            out: Arc::clone(&out),
            width: inner,
        }),
        opts,
    );
    child.write(format!("{}\n", body.join("\n")).as_bytes());
    child.flush();
    drop(child);

    let content = out.lock().unwrap_or_else(PoisonError::into_inner);
    let content = content.trim_end_matches('\n');
    let pin = width.saturating_sub(1).max(QUOTE_BORDER_COLS + 1);
    let text_w = pin - 1; // minus the left padding column: text wraps at width−2
    let bar = Style::default().fg(6).render("│", opts.color);

    let src_rows: Vec<&str> = if content.is_empty() {
        vec![""] // an all-blank body still renders one bar row
    } else {
        content.split('\n').collect()
    };
    let mut rows: Vec<String> = Vec::new();
    for line in src_rows {
        let wrapped: Vec<String> = if ansi_width(line) > text_w {
            word_wrap_ansi(line, text_w)
        } else {
            vec![line.to_owned()]
        };
        for v in wrapped {
            let pad = text_w.saturating_sub(ansi_width(&v));
            rows.push(format!("{bar} {v}{}", " ".repeat(pad)));
        }
    }
    rows.join("\n")
}
