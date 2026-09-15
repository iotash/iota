//! The table block: its row parser and buffering state ([`TableBlock`], markdown.go:1291-1313)
//! and the flushTable twin (markdown.go:1586-1731) — water-filling, the
//! width−1 law, box borders with a rule row between every pair of adjacent rendered
//! rows, `<br>` multi-line cells, cell word-wrap at the pinned column width, and the
//! header as an ordinary first row with per-segment bold (`TUI_DESIGN` §7).
//!
//! VS16 strip and tab→space happen at the PARSE boundary ([`parse_table_cells`]);
//! the layout below sees already-parsed cells. Layout widths come from the raw cell text
//! (markers stripped per `<br>` segment, grapheme-measured — Go cellDisplayWidth);
//! padding comes from each rendered line's escape-aware width, so styled cells (the
//! math passthrough included) always pad to the same column edge.

use crate::markdown::PreviewHandle;
use crate::markdown::blocks::close_view;
use crate::markdown::inline::{DIM, highlight_inline, strip_inline_markdown};
use crate::markdown::style::Style;
use crate::text::ansi::{ansi_width, escape_len_at, sgr_carry};
use crate::text::width::{cluster_width, graphemes, str_width};

/// Minimum rendered column width (markdown.go:1629-1643).
const MIN_COL_WIDTH: usize = 3;

/// isTableLine twin: the trimmed line starts with `'|'`.
pub(crate) fn is_table_line(line: &str) -> bool {
    line.trim().starts_with('|')
}

/// parseTableCells twin (markdown.go:1291-1313): strip one leading and one trailing
/// `|`, split on `|`; per cell replace `\t` with one space (three rulers disagreed on
/// tabs), strip variation selectors (VS16/VS15 — cursor-advance ambiguity; flags and
/// ZWJ sequences deliberately kept), trim.
pub(crate) fn parse_table_cells(line: &str) -> Vec<String> {
    let mut t = line.trim();
    t = t.strip_prefix('|').unwrap_or(t);
    t = t.strip_suffix('|').unwrap_or(t);
    t.split('|')
        .map(|p| {
            strip_variation_selectors(&p.replace('\t', " "))
                .trim()
                .to_owned()
        })
        .collect()
}

/// stripVariationSelectors twin: drops U+FE0F/U+FE0E so bordered layouts stay aligned
/// on every terminal (only the bare base rune advances consistently everywhere).
pub(crate) fn strip_variation_selectors(s: &str) -> String {
    if !s.contains(['\u{FE0F}', '\u{FE0E}']) {
        return s.to_owned();
    }
    s.chars()
        .filter(|r| *r != '\u{FE0F}' && *r != '\u{FE0E}')
        .collect()
}

/// isTableSeparator twin: every cell matches `^:?-+:?$` (alignment colons parsed but
/// IGNORED).
pub(crate) fn is_table_separator(cells: &[String]) -> bool {
    !cells.is_empty() && cells.iter().all(|c| sep_cell(c))
}

fn sep_cell(c: &str) -> bool {
    let b = c.as_bytes();
    let mut i = usize::from(b.first() == Some(&b':'));
    let dash_start = i;
    while i < b.len() && b[i] == b'-' {
        i += 1;
    }
    if i == dash_start {
        return false;
    }
    if i < b.len() && b[i] == b':' {
        i += 1;
    }
    i == b.len()
}

/// splitBR twin — Go `(?i)<br\s*/?>`: splits a cell into multi-line segments.
pub(crate) fn split_br(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut seg_start = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' && i + 2 < b.len() && (b[i + 1] | 0x20) == b'b' && (b[i + 2] | 0x20) == b'r'
        {
            let mut j = i + 3;
            while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n' | b'\x0c' | b'\r') {
                j += 1;
            }
            if j < b.len() && b[j] == b'/' {
                j += 1;
            }
            if j < b.len() && b[j] == b'>' {
                parts.push(s[seg_start..i].to_owned());
                i = j + 1;
                seg_start = i;
                continue;
            }
        }
        i += 1;
    }
    parts.push(s[seg_start..].to_owned());
    parts
}

/// A table block: the parsed cells of each row and its separator flag
/// (markdown.go:1291-1313).
pub(crate) struct TableBlock {
    rows: Vec<Vec<String>>,
    seps: Vec<bool>,
    view: Option<Box<dyn PreviewHandle>>,
}

impl TableBlock {
    /// The live preview's label while the block buffers.
    pub(crate) const LABEL: &'static str = "rendering table…";

    /// A block opened by its first row, with the preview the Writer opened for it.
    pub(crate) fn open(line: &str, view: Option<Box<dyn PreviewHandle>>) -> Self {
        let mut table = Self {
            rows: Vec::new(),
            seps: Vec::new(),
            view,
        };
        table.push(line);
        table
    }

    /// Buffers one table row (parsed cells + separator flag), mirroring the raw line into
    /// the preview.
    pub(crate) fn push(&mut self, line: &str) {
        let cells = parse_table_cells(line);
        self.seps.push(is_table_separator(&cells));
        self.rows.push(cells);
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    /// Closes the preview and renders the block, trailing newline included.
    pub(crate) fn render(mut self, width: usize, color: bool) -> Option<String> {
        close_view(&mut self.view);
        if self.rows.is_empty() {
            return None;
        }
        let rendered = render_table(&self.rows, &self.seps, width, color);
        Some(format!("{rendered}\n"))
    }
}

/// flushTable's rendering seam (markdown.go:1586-1731). `rows` are the parsed cells,
/// `seps` flags the `|---|` source rows — those are skipped, and a faint `├─┼─┤` rule
/// is drawn between EVERY pair of adjacent rendered rows instead (Go `BorderRow`). The
/// header (the data row just before the first separator) goes in as an ordinary first
/// row, NOT via a header API — Go's clamps header cells to one line, which broke
/// multi-line `<br>` headers.
pub(crate) fn render_table(
    rows: &[Vec<String>],
    seps: &[bool],
    width: usize,
    color: bool,
) -> String {
    let max_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    // The header is the data row immediately before the first separator with index>0
    // (may be None: no header).
    let header_row = seps
        .iter()
        .enumerate()
        .find(|&(i, s)| *s && i > 0)
        .map(|(i, _)| i - 1);

    let col_widths = column_widths(rows, max_cols, width);

    let border = |l: char, mid: char, r: char| -> String {
        let spans: Vec<String> = col_widths.iter().map(|w| "─".repeat(w + 2)).collect();
        let mut row = String::new();
        row.push(l);
        row.push_str(&spans.join(&mid.to_string()));
        row.push(r);
        DIM.render(&row, color)
    };
    let bar = DIM.render("│", color);

    let mut out: Vec<String> = vec![border('┌', '┬', '┐')];
    let mut first_rendered = true;
    for (i, row) in rows.iter().enumerate() {
        if seps.get(i).copied().unwrap_or(false) {
            continue; // markdown |---| row; the rules are drawn between rows instead
        }
        if !first_rendered {
            out.push(border('├', '┼', '┤'));
        }
        first_rendered = false;

        // Per-column visual lines: each <br> segment styled then wrapped at the
        // pinned column width (lipgloss wrapped the padded cell the same way).
        let cells: Vec<Vec<String>> = (0..max_cols)
            .map(|j| {
                let raw = row.get(j).map_or("", String::as_str);
                cell_lines(raw, header_row == Some(i), col_widths[j], color)
            })
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for k in 0..height {
            let mut line = bar.clone();
            for (j, cell) in cells.iter().enumerate() {
                let content = cell.get(k).map_or("", String::as_str);
                let pad = col_widths[j].saturating_sub(ansi_width(content));
                line.push(' ');
                line.push_str(content);
                line.push_str(&" ".repeat(pad));
                line.push(' ');
                line.push_str(&bar);
            }
            out.push(line);
        }
    }
    out.push(border('└', '┴', '┘'));
    out.join("\n")
}

/// Natural column widths (over EVERY source row, separators included — Go measures all
/// rows) floored at [`MIN_COL_WIDTH`], then water-filled into the width−1 budget
/// (markdown.go:1621-1684): `available = width − 1 − overhead` (floored at cols×3) —
/// the WIDTH−1 LAW: a table never renders at the exact terminal width (the
/// deferred-wrap boundary). Water-filling: columns whose natural width already fits
/// their fair share keep it; only the wide columns shrink to absorb the deficit, so a
/// narrow column is never wrapped just because another column is huge.
fn column_widths(rows: &[Vec<String>], max_cols: usize, width: usize) -> Vec<usize> {
    let mut cols = vec![0usize; max_cols];
    for row in rows {
        for (j, cell) in row.iter().enumerate().take(max_cols) {
            let w = cell_display_width(cell);
            if w > cols[j] {
                cols[j] = w;
            }
        }
    }
    for w in &mut cols {
        if *w < MIN_COL_WIDTH {
            *w = MIN_COL_WIDTH;
        }
    }

    let overhead = 1 + max_cols * 3; // leading border + " cell " + border per column
    let available = width
        .saturating_sub(1 + overhead)
        .max(max_cols * MIN_COL_WIDTH);
    let total: usize = cols.iter().sum();
    if total <= available {
        return cols;
    }

    // Water-filling: iteratively settle every unsettled column whose natural width is
    // at or under the fair share (= remaining / remaining columns), deducting it from
    // the remaining budget, until nothing changes; the surviving wide columns then all
    // get the final fair share, floored at MIN_COL_WIDTH.
    let mut settled = vec![false; max_cols];
    let mut remaining = available;
    let mut remaining_cols = max_cols;
    loop {
        let fair = remaining / remaining_cols.max(1);
        let mut changed = false;
        for (j, col) in cols.iter().enumerate() {
            if !settled[j] && *col <= fair {
                settled[j] = true;
                remaining = remaining.saturating_sub(*col);
                remaining_cols -= 1;
                changed = true;
            }
        }
        if !changed || remaining_cols == 0 {
            break;
        }
    }
    if let Some(fair) = remaining.checked_div(remaining_cols) {
        let fair = fair.max(MIN_COL_WIDTH);
        for (j, col) in cols.iter_mut().enumerate() {
            if !settled[j] {
                *col = fair;
            }
        }
    }
    cols
}

/// One cell's visual lines: each `<br>` segment rendered (header: markers stripped +
/// bold per segment so the bold never spans a newline and bleeds into the borders —
/// Go headerCell; data: inline markdown per segment — Go styledCell), then word-wrapped
/// at the pinned column width.
fn cell_lines(raw: &str, header: bool, col_width: usize, color: bool) -> Vec<String> {
    let bold = Style::default().bold();
    let mut out = Vec::new();
    for seg in split_br(raw) {
        let styled = if header {
            bold.render(&strip_inline_markdown(seg.trim()), color)
        } else {
            highlight_inline(seg.trim(), color)
        };
        out.extend(word_wrap_ansi(&styled, col_width));
    }
    out
}

/// cellDisplayWidth twin (markdown.go:1756-1765): max per-`<br>`-segment grapheme
/// width of the marker-stripped text.
fn cell_display_width(cell: &str) -> usize {
    split_br(cell)
        .iter()
        .map(|seg| str_width(&strip_inline_markdown(seg.trim())))
        .max()
        .unwrap_or(0)
}

/// One lexed run of a styled line: a word (escapes embedded at zero width) or a run
/// of spaces.
struct Tok {
    /// Grapheme clusters and escape sequences, each with its display width.
    pieces: Vec<(String, usize)>,
    /// Total display width.
    width: usize,
    /// Whether this token is a run of spaces.
    space: bool,
}

/// Lexes a line into word and space-run tokens; escapes are zero-width and attach to
/// the word around them.
fn tokenize(s: &str) -> Vec<Tok> {
    let bytes = s.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut pos = 0;
    while pos < s.len() {
        let esc_n = escape_len_at(bytes, pos);
        if esc_n > 0 {
            let esc = s[pos..pos + esc_n].to_owned();
            match toks.last_mut() {
                Some(t) if !t.space => t.pieces.push((esc, 0)),
                _ => toks.push(Tok {
                    pieces: vec![(esc, 0)],
                    width: 0,
                    space: false,
                }),
            }
            pos += esc_n;
            continue;
        }
        let mut end = pos;
        while end < s.len() && escape_len_at(bytes, end) == 0 {
            end += s[end..].chars().next().map_or(1, char::len_utf8);
        }
        for g in graphemes(&s[pos..end]) {
            let space = g == " ";
            let w = cluster_width(g);
            match toks.last_mut() {
                Some(t) if t.space == space => {
                    t.pieces.push((g.to_owned(), w));
                    t.width += w;
                }
                _ => toks.push(Tok {
                    pieces: vec![(g.to_owned(), w)],
                    width: w,
                    space,
                }),
            }
        }
        pos = end;
    }
    toks
}

/// Word wrap, ANSI- and grapheme-aware (the lipgloss/reflow cell-wrap shape): breaks
/// at word boundaries (the spaces at a break are consumed), hard-breaks words longer
/// than `width` cluster by cluster, keeps escapes in place at zero width. Every row is
/// then re-sewn self-contained: carried SGR state is re-emitted at each continuation
/// row's head and an open row is closed with a reset — a padded table cell or barred
/// quote row must never bleed styling into the frame drawn around it.
///
/// The exact wrap points inside squeezed cells are not pinned by any Go test except
/// total line width (markdown spec §open-questions) — width-only assertions apply.
pub(crate) fn word_wrap_ansi(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    let mut pend = String::new();
    let mut pend_w = 0usize;
    for tok in tokenize(s) {
        if tok.space {
            for (p, _) in &tok.pieces {
                pend.push_str(p);
            }
            pend_w += tok.width;
            continue;
        }
        if line_w + pend_w + tok.width <= width {
            line.push_str(&pend);
            line_w += pend_w;
            pend.clear();
            pend_w = 0;
            for (p, _) in &tok.pieces {
                line.push_str(p);
            }
            line_w += tok.width;
        } else if tok.width <= width {
            // Break at the word boundary; the pending spaces are consumed by it.
            rows.push(std::mem::take(&mut line));
            pend.clear();
            pend_w = 0;
            for (p, _) in &tok.pieces {
                line.push_str(p);
            }
            line_w = tok.width;
        } else {
            // A word longer than the width hard-breaks cluster by cluster.
            if line_w + pend_w <= width {
                line.push_str(&pend);
                line_w += pend_w;
            }
            pend.clear();
            pend_w = 0;
            for (p, w) in &tok.pieces {
                if *w == 0 {
                    line.push_str(p);
                    continue;
                }
                if line_w + w > width {
                    rows.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                line.push_str(p);
                line_w += w;
            }
        }
    }
    line.push_str(&pend); // trailing spaces at end of input survive
    rows.push(line);
    resew_sgr(rows)
}

/// Re-emits the SGR state carried into each continuation row and closes any state a
/// row leaves open (see [`word_wrap_ansi`]). Rows whose spans are self-contained —
/// every span this crate's `Style` emits closes itself — pass through unchanged.
fn resew_sgr(mut rows: Vec<String>) -> Vec<String> {
    let mut state = String::new();
    for row in &mut rows {
        let original = std::mem::take(row);
        let carried = state.clone();
        state = sgr_carry(&state, &original);
        let mut rebuilt = String::new();
        rebuilt.push_str(&carried);
        rebuilt.push_str(&original);
        if !state.is_empty() {
            rebuilt.push_str("\x1b[0m");
        }
        *row = rebuilt;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{column_widths, is_table_separator, parse_table_cells, split_br, word_wrap_ansi};

    fn cells(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    // Water-filling pins (markdown.go:1644-1684): narrow columns keep their natural
    // width, only the wide ones shrink to the fair share.
    #[test]
    fn water_filling_settles_narrow_columns() {
        // width 40, 2 cols: overhead = 7, available = 40-1-7 = 32.
        // naturals: [4, 60] → col 0 settles at 4, col 1 gets 32-4 = 28.
        let rows = vec![cells(&["abcd", &"x".repeat(60)])];
        assert_eq!(column_widths(&rows, 2, 40), vec![4, 28]);
        // Both huge: each gets available/2 = 16.
        let rows = vec![cells(&[&"y".repeat(50), &"x".repeat(60)])];
        assert_eq!(column_widths(&rows, 2, 40), vec![16, 16]);
        // Fits naturally: untouched (floored at 3).
        let rows = vec![cells(&["ab", "cdef"])];
        assert_eq!(column_widths(&rows, 2, 80), vec![3, 4]);
    }

    // The width−1 law floor: available never drops under cols×3.
    #[test]
    fn water_filling_floor() {
        let rows = vec![cells(&[&"x".repeat(30)])];
        // width 5: available = max(5-1-4, 3) = 3.
        assert_eq!(column_widths(&rows, 1, 5), vec![3]);
    }

    #[test]
    fn word_wrap_breaks_at_spaces() {
        assert_eq!(word_wrap_ansi("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(word_wrap_ansi("aa bb cc", 4), ["aa", "bb", "cc"]);
        assert_eq!(word_wrap_ansi("short", 10), ["short"]);
        assert_eq!(word_wrap_ansi("", 10), [""]);
    }

    #[test]
    fn word_wrap_hard_breaks_overlong_words() {
        assert_eq!(word_wrap_ansi("abcdefgh", 3), ["abc", "def", "gh"]);
        // CJK: a wide cluster never splits across the boundary.
        assert_eq!(word_wrap_ansi("中文条目", 3), ["中", "文", "条", "目"]);
    }

    #[test]
    fn word_wrap_keeps_escapes_and_resews_state() {
        // The bold span breaks mid-word run: the continuation re-opens it and the
        // broken row closes itself.
        let rows = word_wrap_ansi("\x1b[1maa bb\x1b[0m", 3);
        assert_eq!(rows, ["\x1b[1maa\x1b[0m", "\x1b[1mbb\x1b[0m"]);
        // Self-contained spans pass through untouched.
        let rows = word_wrap_ansi("\x1b[1maa\x1b[0m bb", 10);
        assert_eq!(rows, ["\x1b[1maa\x1b[0m bb"]);
    }

    // Hand-parser pins for Go's tableSepRe `^:?-+:?$` (markdown spec §regexes).
    #[test]
    fn table_separator_pins() {
        assert!(is_table_separator(&cells(&[
            "---", "-", ":--:", ":-", "-:"
        ])));
        assert!(!is_table_separator(&cells(&["---", ""])));
        assert!(!is_table_separator(&cells(&["::"])));
        assert!(!is_table_separator(&cells(&["x--"])));
        assert!(!is_table_separator(&cells(&["--x"])));
        assert!(!is_table_separator(&[]));
    }

    // Hand-parser pins for Go's brRe `(?i)<br\s*/?>`.
    #[test]
    fn split_br_pins() {
        assert_eq!(split_br("a<br>b"), ["a", "b"]);
        assert_eq!(split_br("a<BR/>b"), ["a", "b"]);
        assert_eq!(split_br("a<br />b"), ["a", "b"]);
        assert_eq!(split_br("a<Br\t/>b"), ["a", "b"]);
        assert_eq!(split_br("a<brx>b"), ["a<brx>b"]);
        assert_eq!(split_br("a<b>r</b>"), ["a<b>r</b>"]);
        assert_eq!(split_br("plain"), ["plain"]);
    }

    // Go: internal/markdown/markdown.go:1291-1313.
    #[test]
    fn parse_table_cells_pins() {
        assert_eq!(parse_table_cells("| a | b |"), ["a", "b"]);
        assert_eq!(parse_table_cells("|a|b"), ["a", "b"]);
        assert_eq!(parse_table_cells("| a\tb |"), ["a b"]);
        assert_eq!(parse_table_cells("| \u{2696}\u{FE0F} c |"), ["\u{2696} c"]);
    }
}
