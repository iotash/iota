//! In-app selection over the conversation (tui-plan S6, D3): what a drag or a double-click
//! selects, how it is drawn, and the text it copies.
//!
//! A point is a display cell of the BUFFER, not of the screen: a row number (rows are numbered as
//! committed, so the cap dropping old rows moves nothing), a line within that row's wrap, and a
//! display column. A selection can therefore run past the viewport — the drag scrolls it, and the
//! ends stay on their text while new output arrives.
//!
//! - **Drawn** as reverse video on the screen's cells only ([`span`]): the rows themselves are
//!   never touched, so a write-out (`Ctrl+O`, the exit) never carries a selection's styling.
//! - **Copied** as plain text ([`text`]): SGR stripped, a row's wrapped lines joined back into the
//!   one line it was committed as, rows joined by `\n`, trailing blanks trimmed.
//! - **A word** ([`word_at`]) is a run of letters, digits and the punctuation of paths, URLs and
//!   identifiers (`_-./~+:@\`), the way terminals double-click; a trailing `.`, `,` or `:` is a
//!   sentence's, not the word's.

use std::cmp::Ordering;

use crate::text::width::{graphemes, str_width};

/// One display cell of the conversation buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Point {
    /// The row's number (`Conversation` numbers rows as committed).
    pub(crate) row: u64,
    /// The line within the row's wrap at the width it is drawn at.
    pub(crate) line: usize,
    /// The display column.
    pub(crate) col: usize,
}

impl Ord for Point {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.row, self.line, self.col).cmp(&(other.row, other.line, other.col))
    }
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A selection: where the press was and where the pointer is now. Either may come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) anchor: Point,
    pub(crate) head: Point,
}

impl Selection {
    /// The ends in document order, both inclusive.
    pub(crate) fn ordered(&self) -> (Point, Point) {
        if self.head < self.anchor {
            (self.head, self.anchor)
        } else {
            (self.anchor, self.head)
        }
    }

    /// The columns `[from, to)` it covers on display line `(row, line)`, whose text is `cols`
    /// wide — `None` when it covers none. A line wholly inside covers its text (one cell at least,
    /// so a selected blank line shows).
    pub(crate) fn span(&self, row: u64, line: usize, cols: usize) -> Option<(usize, usize)> {
        let (start, end) = self.ordered();
        let here = (row, line);
        if here < (start.row, start.line) || here > (end.row, end.line) {
            return None;
        }
        let from = if here == (start.row, start.line) {
            start.col
        } else {
            0
        };
        let to = if here == (end.row, end.line) {
            end.col + 1
        } else {
            cols.max(1)
        };
        (from < to).then_some((from, to))
    }
}

/// The text a selection copies. `lines_of(row)` is the plain (SGR-stripped) display lines of a
/// row at the width the selection was made at, `None` once the row is gone.
pub(crate) fn text(
    sel: &Selection,
    mut lines_of: impl FnMut(u64) -> Option<Vec<String>>,
) -> String {
    let (start, end) = sel.ordered();
    let mut out: Vec<String> = Vec::new();
    for row in start.row..=end.row {
        let Some(lines) = lines_of(row) else { continue };
        let mut joined = String::new();
        for (i, line) in lines.iter().enumerate() {
            let cols = str_width(line);
            if let Some((from, to)) = sel.span(row, i, cols) {
                joined.push_str(&slice_cols(line, from, to));
            }
        }
        out.push(joined.trim_end().to_owned());
    }
    out.join("\n")
}

/// The part of `plain` in display columns `[from, to)`: a wide glyph is in when its first
/// column is.
pub(crate) fn slice_cols(plain: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for g in graphemes(plain) {
        if col >= to {
            break;
        }
        if col >= from {
            out.push_str(g);
        }
        col += str_width(g);
    }
    out
}

/// Punctuation a double-clicked word keeps: paths, URLs, identifiers.
const WORD_PUNCT: &str = "_-./~+:@\\";

/// The display columns `[from, to]` of the word under column `col` of `plain`, or `None` when
/// `col` is on a blank, past the end, or on punctuation alone.
pub(crate) fn word_at(plain: &str, col: usize) -> Option<(usize, usize)> {
    let cells: Vec<(usize, &str)> = {
        let mut c = 0;
        graphemes(plain)
            .map(|g| {
                let at = c;
                c += str_width(g);
                (at, g)
            })
            .collect()
    };
    let is_word = |g: &str| {
        g.chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || WORD_PUNCT.contains(c))
    };
    let hit = cells
        .iter()
        .rposition(|&(at, _)| at <= col)
        .filter(|&i| col < cells[i].0 + str_width(cells[i].1).max(1))?;
    if !is_word(cells[hit].1) {
        return None;
    }
    let mut first = hit;
    while first > 0 && is_word(cells[first - 1].1) {
        first -= 1;
    }
    let mut last = hit;
    while last + 1 < cells.len() && is_word(cells[last + 1].1) {
        last += 1;
    }
    while last > first && matches!(cells[last].1, "." | "," | ":") {
        last -= 1;
    }
    if !cells[first..=last]
        .iter()
        .any(|(_, g)| g.chars().any(char::is_alphanumeric))
    {
        return None;
    }
    let end = cells[last].0 + str_width(cells[last].1).max(1) - 1;
    Some((cells[first].0, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(row: u64, line: usize, col: usize) -> Point {
        Point { row, line, col }
    }

    fn sel(a: Point, b: Point) -> Selection {
        Selection { anchor: a, head: b }
    }

    /// A drag that goes backwards selects the same cells as the one that goes forwards.
    #[test]
    fn either_end_may_come_first() {
        let fwd = sel(pt(1, 0, 2), pt(3, 1, 4));
        let back = sel(pt(3, 1, 4), pt(1, 0, 2));
        assert_eq!(fwd.ordered(), back.ordered());
        assert_eq!(back.span(1, 0, 10), Some((2, 10)));
        assert_eq!(
            back.span(2, 0, 0),
            Some((0, 1)),
            "a blank line inside shows"
        );
        assert_eq!(back.span(3, 1, 10), Some((0, 5)));
        assert_eq!(back.span(3, 2, 10), None);
        assert_eq!(back.span(0, 0, 10), None);
    }

    /// The payload: plain text, each row's wrap joined back into its line, rows on their own lines,
    /// the ends cut at their columns, trailing blanks trimmed.
    #[test]
    fn the_copy_is_the_rows_plain_text_with_the_wraps_undone() {
        let rows = [
            vec!["alpha beta".to_owned()],
            vec!["a long line th".to_owned(), "at wrapped   ".to_owned()],
            vec![String::new()],
            vec!["gamma delta".to_owned()],
        ];
        let lines_of = |r: u64| rows.get(usize::try_from(r).ok()?).cloned();
        let s = sel(pt(0, 0, 6), pt(3, 0, 4));
        assert_eq!(
            text(&s, lines_of),
            "beta\na long line that wrapped\n\ngamma"
        );
        // Within one line.
        let s = sel(pt(1, 0, 2), pt(1, 0, 5));
        assert_eq!(text(&s, lines_of), "long");
        // Across the wrap of one row.
        let s = sel(pt(1, 0, 12), pt(1, 1, 1));
        assert_eq!(text(&s, lines_of), "that");
        // A row gone (dropped by the cap) is skipped, the rest kept.
        let s = sel(pt(3, 0, 0), pt(5, 0, 3));
        assert_eq!(text(&s, lines_of), "gamma delta");
    }

    /// Display columns, not bytes: a CJK glyph is two columns and is taken whole.
    #[test]
    fn columns_are_display_columns() {
        assert_eq!(slice_cols("中文ab", 2, 5), "文a");
        assert_eq!(
            slice_cols("中文ab", 1, 3),
            "文",
            "a glyph starting inside is in"
        );
        assert_eq!(slice_cols("abc", 5, 9), "");
    }

    /// A double-click's word: paths and URLs whole, a sentence's full stop left out, nothing on a
    /// blank.
    #[test]
    fn a_double_click_takes_the_word_under_it() {
        let line = "see src/ui/mod.rs:12. then https://x.dev/a?b ok";
        let word = |col| word_at(line, col).map(|(a, b)| slice_cols(line, a, b + 1));
        assert_eq!(word(1).as_deref(), Some("see"));
        assert_eq!(word(6).as_deref(), Some("src/ui/mod.rs:12"));
        assert_eq!(word(3), None, "a blank");
        assert_eq!(word(30).as_deref(), Some("https://x.dev/a"), "`?` ends it");
        assert_eq!(word(99), None, "past the end");
        assert_eq!(word_at("a -- b", 2), None, "punctuation alone");
        assert_eq!(word_at("中文 x", 3), Some((0, 3)), "a CJK run is one word");
    }
}
