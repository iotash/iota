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
//! - **A live block** is a laid-out block still being written (tui-plan S7b): a code block, a
//!   table, a list, a quote or a formula whose reply is still streaming. It is committed again,
//!   under the same [`LiveId`], every time it grows and once more when it closes; the buffer
//!   replaces it in place, so it appears line by line at the width of the screen.
//!
//! The block's kind is its source's type — the [`Layout`] implementation (`markdown::Fragment`,
//! the banner, the user block, the diff listing, the error block); nothing downstream branches on
//! it, so no separate tag is carried.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::text::ansi::wrap_ansi;
use crate::tool::fmt::{LabelBody, ToolLabel};
use crate::ui::render::theme::{BG_DEFAULT, RESET, REV_ON, USER_BAR};

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
    /// A laid-out block still being written, as it stands now (module docs). Boxed: it is rare,
    /// and the buffer's every entry holds a `Block`.
    Live(Box<Live>),
}

/// Which live block a commit replaces: one per open block, never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveId(u64);

impl LiveId {
    /// A fresh id.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A live block as it stands: its id, its layout so far, and whether more is coming (`false` on
/// the commit that closes it).
#[derive(Clone)]
pub struct Live {
    /// Which block this is.
    pub id: LiveId,
    /// Its layout as it stands.
    pub layout: Arc<dyn Layout>,
    /// Whether more is coming.
    pub open: bool,
}

impl Block {
    /// A block laid out by `layout`.
    pub fn laid(layout: impl Layout + 'static) -> Self {
        Self::Laid(Arc::new(layout))
    }

    /// Live block `id` as it stands, laid out by `layout`; `open` while more is coming.
    pub fn live(id: LiveId, layout: impl Layout + 'static, open: bool) -> Self {
        Self::Live(Box::new(Live {
            id,
            layout: Arc::new(layout),
            open,
        }))
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
            Self::Live(l) => l.layout.lines(width),
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

/// The user's submitted text as full-width rows (ui.go:224-243, chat/replay.go:196-215
/// `printUserBlock`), laid out from the text at the width of the draw: every row is ONE reverse
/// video span (the terminal's foreground behind it, its background as the text — what sets the
/// block apart from body text in any theme), so no cell of it shows the terminal background
/// (X-105). Column 0 is the bar: `▌` on every row, one continuous gray column down the block, drawn
/// in the `USER_BAR` gray — set as the BACKGROUND, which the reverse turns into the glyph's colour, the
/// cell behind it staying the block's — then `BG_DEFAULT`, the reverse kept. Column 1 is the
/// gutter space, the text from column 2, padded by display width to width − 2 so the span runs to
/// the edge at any width, one `RESET`. Under `ColorMode::Off` it writes no escape at all — the
/// rows are committed text that the exit writes out raw, past the frame's colour gate, so a
/// reverse attribute would leak there — and the bar's character carries the block. The live block
/// and the resume echo both lay out through this one, the same rows.
///
/// A message that IS one XML element (`<hc from="agent">…</hc>`, [`crate::text::xml`]) shows its
/// inner text as the block and its tag as a [`Tab`] on the row above it (X-107).
pub struct UserBlock {
    /// The tag of a message that is one element.
    pub tab: Option<Tab>,
    /// The text the block shows (its display form: paste tags bounded; an element's inner text).
    pub text: String,
}

/// A tagged message's header: the element's name and its attributes in document order (a bare
/// attribute has no value). Drawn as the user block's first row — a tool call's label without
/// its brackets, `name k:v k:v`, on a fill of the bar's gray from column 0 to one column past the
/// text, which starts at column 2 with the block's.
pub struct Tab {
    /// The element's name.
    pub name: String,
    /// Its attributes.
    pub attrs: Vec<(String, Option<String>)>,
}

impl UserBlock {
    /// The block for a submitted `message`: one element becomes its tab and its inner text, any
    /// other message is shown as it was written.
    pub fn new(message: &str) -> Self {
        match crate::text::xml::single_element(message) {
            Some(el) => Self {
                tab: Some(Tab {
                    name: el.name.to_owned(),
                    attrs: el
                        .attrs
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned)))
                        .collect(),
                }),
                text: el.inner.to_owned(),
            },
            None => Self {
                tab: None,
                text: message.to_owned(),
            },
        }
    }
}

impl Tab {
    /// The tab as a call label no wider than `budget` columns: a bare attribute is a positional
    /// argument, a value one-lined; attributes are dropped from the end behind the label's own
    /// `… +N args` tail until it fits, and the one left is cut with `…`.
    fn label(&self, budget: usize) -> ToolLabel {
        use crate::text::width::{str_width, truncate_cols};
        let all: Vec<(Option<String>, String)> = self
            .attrs
            .iter()
            .map(|(k, v)| match v {
                Some(v) => (Some(k.clone()), v.replace('\n', " ")),
                None => (None, k.clone()),
            })
            .collect();
        let make = |shown: usize| ToolLabel {
            name: self.name.clone(),
            body: LabelBody::Args {
                pairs: all[..shown].to_vec(),
                more: all.len() - shown,
            },
        };
        for shown in (1..=all.len()).rev() {
            let l = make(shown);
            if str_width(&l.unbracketed()) <= budget {
                return l;
            }
        }
        let mut l = make(all.len().min(1));
        let over = str_width(&l.unbracketed()).saturating_sub(budget);
        if let LabelBody::Args { pairs, .. } = &mut l.body
            && let Some((_, v)) = pairs.first_mut()
        {
            *v = truncate_cols(v, str_width(v).saturating_sub(over));
        }
        l
    }

    /// The tab's row at `width` (≥ 8): the label from column 2 on the bar's gray, from column 0
    /// to one column past it; with colour off, the plain `  name k:v`.
    fn row(&self, width: usize, color: bool) -> String {
        const LEAD: usize = 2;
        let label = self.label(width - LEAD - 1);
        if color {
            let painted = crate::repl::render::styles::paint_label_segments(
                &label,
                &crate::repl::render::styles::TAB_INK,
            );
            let cols = LEAD + crate::text::width::str_width(&label.unbracketed()) + 1;
            crate::text::ansi::paint_row(&format!("  {painted}"), USER_BAR, cols.min(width))
        } else {
            format!("  {}", label.unbracketed())
        }
    }
}

impl Layout for UserBlock {
    fn lines(&self, width: usize) -> Vec<String> {
        const GUTTER: usize = 2;
        let w = width.max(8);
        let color = crate::app::color::enabled();
        let tab = self.tab.as_ref().map(|t| t.row(w, color));
        tab.into_iter()
            .chain(
                crate::text::ansi::wrap_by_width(&self.text, w - GUTTER)
                    .into_iter()
                    .map(|row| {
                        let pad = (w - GUTTER).saturating_sub(crate::text::width::str_width(&row));
                        let pad = " ".repeat(pad);
                        if color {
                            format!("{REV_ON}{USER_BAR}▌{BG_DEFAULT} {row}{pad}{RESET}")
                        } else {
                            format!("▌ {row}{pad}")
                        }
                    }),
            )
            .collect()
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
            Self::Live(l) => f
                .debug_tuple("Live")
                .field(&l.id)
                .field(&l.layout.lines(80))
                .field(&l.open)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Layout, UserBlock};
    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;

    /// The block at 80 columns as bytes: one reverse-video span per row — the bar in the 245 gray
    /// (`▌` on every row, the wrapped one too), the default background back, the gutter
    /// space, the text from column 2, padded to the edge — every row 80 wide.
    #[test]
    fn the_user_block_is_one_reverse_span_with_a_gray_bar() {
        // 78 columns of text beside the gutter, then 5 more: the wrap lands at column 2 too.
        let text = format!("{}{}", "x".repeat(78), "tail.");
        let rows = UserBlock::new(&text).lines(80);
        assert_eq!(
            rows,
            [
                format!("\x1b[7m\x1b[48;5;245m▌\x1b[49m {}\x1b[0m", "x".repeat(78)),
                format!(
                    "\x1b[7m\x1b[48;5;245m▌\x1b[49m tail.{}\x1b[0m",
                    " ".repeat(73)
                ),
            ]
        );
        for row in &rows {
            assert_eq!(str_width(&strip_sgr(row)), 80, "full width: {row:?}");
        }
        let one = UserBlock::new("hi").lines(20);
        assert_eq!(
            one,
            [format!(
                "\x1b[7m\x1b[48;5;245m▌\x1b[49m hi{}\x1b[0m",
                " ".repeat(16)
            )]
        );
    }

    /// A message that is one element: the tab is the block's first row — the bar's gray from
    /// column 0, the label's segments in the tab's dark ink from column 2 (the gray re-asserted after each of their
    /// resets), one trailing gray space, reset — and the block below shows the inner text alone.
    #[test]
    fn a_tagged_message_gets_a_tab_row_above_its_inner_text() {
        let b = UserBlock::new(r#"<hc from="worker-2" urgent>build is green</hc>"#);
        assert_eq!(b.text, "build is green");
        let rows = b.lines(40);
        assert_eq!(rows.len(), 2, "tab + one block row: {rows:?}");
        let g = "\x1b[48;5;245m";
        assert_eq!(
            rows[0],
            format!(
                "{g}  \x1b[1;38;5;16m{g}hc\x1b[0m{g} \x1b[2;38;5;16m{g}from:\x1b[0m{g}\x1b[38;5;16m{g}worker-2\x1b[0m{g} \x1b[38;5;16m{g}urgent\x1b[0m{g} \x1b[0m"
            )
        );
        assert_eq!(strip_sgr(&rows[0]), "  hc from:worker-2 urgent ");
        assert_eq!(
            rows[1],
            format!(
                "\x1b[7m\x1b[48;5;245m▌\x1b[49m build is green{}\x1b[0m",
                " ".repeat(24)
            )
        );
        // One tab row whatever the inner text's length; the block wraps under it as before.
        let long = UserBlock::new(&format!("<hc from=\"a\">{}</hc>", "x".repeat(60))).lines(40);
        assert_eq!(long.len(), 3, "{long:?}");
        assert_eq!(strip_sgr(&long[0]), "  hc from:a ");
    }

    /// The tab never runs past the width: attributes go from the end behind the label's
    /// `… +N args` tail, and the one left is cut with `…`.
    #[test]
    fn the_tab_is_cut_to_the_width_with_the_labels_tail() {
        let tab = |msg: &str, w: usize| strip_sgr(&UserBlock::new(msg).lines(w)[0]);
        let msg = r#"<hc from="worker-2" to="lead" topic="the release">x</hc>"#;
        assert_eq!(
            tab(msg, 80),
            "  hc from:worker-2 to:lead topic:the release "
        );
        assert_eq!(tab(msg, 40), "  hc from:worker-2 to:lead … +1 args ");
        assert_eq!(tab(msg, 30), "  hc from:worker-2 … +2 args ");
        assert_eq!(tab(msg, 24), "  hc from:wo… … +2 args ");
        for w in [8, 12, 24, 30, 40, 80] {
            assert!(str_width(&tab(msg, w)) <= w, "{w}: {:?}", tab(msg, w));
        }
    }

    /// Anything that is not exactly one element — here, text after it — has no tab and keeps
    /// its text as it was written.
    #[test]
    fn a_message_with_trailing_text_is_shown_unchanged() {
        let msg = r#"<hc from="a">hi</hc> and more"#;
        let b = UserBlock::new(msg);
        assert!(b.tab.is_none());
        assert_eq!(b.text, msg);
        let rows = b.lines(80);
        assert_eq!(rows.len(), 1);
        assert!(strip_sgr(&rows[0]).starts_with(&format!("▌ {msg}")));
    }

    /// A CJK row: wide runes count two columns, so the reversed span is padded by display width,
    /// not by chars — the row still 80 wide.
    #[test]
    fn the_cjk_user_block_pads_by_display_width() {
        let rows = UserBlock::new("中文一行").lines(80);
        assert_eq!(
            rows,
            [format!(
                "\x1b[7m\x1b[48;5;245m▌\x1b[49m 中文一行{}\x1b[0m",
                " ".repeat(70)
            )]
        );
    }

    /// The row round-trips through the frame's span parser: the bar cell is reversed with the 245
    /// gray as its background (drawn as the glyph) and the default foreground (the cell behind
    /// it, the block's colour); the rest of the row is reversed with neither colour set.
    #[test]
    fn the_bar_cell_parses_as_reversed_gray_on_the_block() {
        use crate::ui::render::spans::ansi_to_spans_with;
        use ratatui::style::{Color, Modifier};
        let row = &UserBlock::new("hi").lines(20)[0];
        let line = ansi_to_spans_with(row, true);
        let spans: Vec<_> = line
            .spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style))
            .collect();
        assert_eq!(spans.len(), 2, "{spans:?}");
        let (bar, st) = spans[0];
        assert_eq!(bar, "▌");
        assert_eq!((st.fg, st.bg), (None, Some(Color::Indexed(245))));
        assert!(st.add_modifier.contains(Modifier::REVERSED));
        let (rest, st) = spans[1];
        assert_eq!(rest, format!(" hi{}", " ".repeat(16)));
        assert_eq!((st.fg, st.bg), (None, None));
        assert!(st.add_modifier.contains(Modifier::REVERSED));
    }
}
