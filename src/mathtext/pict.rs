//! The rectangular picture the layout composes (internal/mathtext/box.go; Go `Box` → `Pict`):
//! equal-width rows, a baseline row, and the combinators (`hconcat`, `vstack`, `raise`/`lower`,
//! overlines, accent rows, rules, delimiters). Every width is [`box_width`] =
//! `text::width::str_width` — THE ruler; never `len()`.
//!
//! The box model is a hand-rolled port of `SymPy`'s `stringPict`/`prettyForm` algorithm
//! (`sympy/printing/pretty/stringpict.py`, BSD-3-Clause) exactly as Go's `box.go:13-17` records:
//! only the geometry was reimplemented, no upstream code is used.
//!
//! Two invariants every valid picture keeps: every row in `lines` has display width `width`, and
//! `baseline` indexes the reference row (`0` for a plain one-row atom). The default `Pict` — no
//! rows, width 0 — is the valid empty picture and the identity of [`hconcat`].

use crate::text::width::str_width;

/// The rule glyph of [`overline`] and [`hrule`] (box.go:255 — U+2500).
const RULE: char = '─';

/// A rectangular picture (box.go:20-33).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pict {
    /// The rows, every one `width` columns wide.
    pub(crate) lines: Vec<String>,
    /// The baseline row index.
    pub(crate) baseline: usize,
    /// The display width of every row.
    pub(crate) width: usize,
}

impl Pict {
    /// One-row picture of `s` (box.go:37 `NewBox`).
    pub(crate) fn new(s: &str) -> Self {
        Self {
            lines: vec![s.to_owned()],
            baseline: 0,
            width: box_width(s),
        }
    }

    /// Multi-row picture from `text`'s lines, padded to the widest, baseline clamped
    /// (box.go:43 `NewBoxLines`). An empty string yields a single empty row.
    pub(crate) fn new_lines(text: &str, baseline: usize) -> Self {
        let raw: Vec<&str> = text.split('\n').collect();
        let width = raw.iter().map(|l| box_width(l)).max().unwrap_or(0);
        let lines: Vec<String> = raw.iter().map(|l| pad(l, width)).collect();
        let baseline = baseline.min(lines.len().saturating_sub(1));
        Self {
            lines,
            baseline,
            width,
        }
    }

    /// The empty picture — no rows, width 0 (box.go:62 `EmptyBox`).
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// The number of rows (box.go:109 `Height`).
    pub(crate) fn height(&self) -> usize {
        self.lines.len()
    }

    /// The rows BELOW the baseline (box.go:113 `depth`); a one-row picture has depth 0.
    pub(crate) fn depth(&self) -> usize {
        self.height()
            .saturating_sub(1)
            .saturating_sub(self.baseline)
    }

    /// Whether the picture carries no drawable content (box.go:104 `isEmptyBox`): width 0 or no
    /// rows. Used to skip an omitted delimiter side.
    pub(crate) fn is_empty(&self) -> bool {
        self.width == 0 || self.lines.is_empty()
    }

    /// Rows joined with `\n`; `""` when empty (box.go:68 `String`).
    pub(crate) fn render(&self) -> String {
        self.lines.join("\n")
    }
}

/// Delimiter families (box.go:298-305).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DelimKind {
    /// `( … )`.
    Paren,
    /// `[ … ]`.
    Bracket,
    /// `{ … }`.
    Brace,
    /// `| … |`.
    Bar,
}

/// The drawn pieces of ONE side of a tall delimiter (box.go:311-317): every field is a width-1
/// glyph, `mid` empty when the kind has no centre piece.
struct DelimGlyphs {
    /// The height-1 form.
    single: &'static str,
    /// The top corner/hook.
    top: &'static str,
    /// The extension repeated between the corners.
    ext: &'static str,
    /// The bottom corner/hook.
    bottom: &'static str,
    /// The centre piece (curly braces only); `""` means none.
    mid: &'static str,
}

/// The left-side glyph set of `kind` (box.go:329-334; U+239B-U+23AD, U+2502).
const fn left_glyphs(kind: DelimKind) -> DelimGlyphs {
    match kind {
        DelimKind::Paren => DelimGlyphs {
            single: "(",
            top: "⎛",
            ext: "⎜",
            bottom: "⎝",
            mid: "",
        },
        DelimKind::Bracket => DelimGlyphs {
            single: "[",
            top: "⎡",
            ext: "⎢",
            bottom: "⎣",
            mid: "",
        },
        DelimKind::Brace => DelimGlyphs {
            single: "{",
            top: "⎧",
            ext: "⎪",
            bottom: "⎩",
            mid: "⎨",
        },
        DelimKind::Bar => DelimGlyphs {
            single: "│",
            top: "│",
            ext: "│",
            bottom: "│",
            mid: "",
        },
    }
}

/// The right-side glyph set of `kind` (box.go:336-341).
const fn right_glyphs(kind: DelimKind) -> DelimGlyphs {
    match kind {
        DelimKind::Paren => DelimGlyphs {
            single: ")",
            top: "⎞",
            ext: "⎟",
            bottom: "⎠",
            mid: "",
        },
        DelimKind::Bracket => DelimGlyphs {
            single: "]",
            top: "⎤",
            ext: "⎥",
            bottom: "⎦",
            mid: "",
        },
        DelimKind::Brace => DelimGlyphs {
            single: "}",
            top: "⎫",
            ext: "⎪",
            bottom: "⎭",
            mid: "⎬",
        },
        DelimKind::Bar => DelimGlyphs {
            single: "│",
            top: "│",
            ext: "│",
            bottom: "│",
            mid: "",
        },
    }
}

/// Horizontal concatenation on a common baseline (box.go:134-170): empty operands drop out, the
/// result baseline is the deepest ascent and its depth the deepest descent, each part padded with
/// blank rows above and below before the rows are joined left to right.
pub(crate) fn hconcat(parts: &[Pict]) -> Pict {
    let kept: Vec<&Pict> = parts
        .iter()
        .filter(|b| b.height() != 0 && b.width != 0)
        .collect();
    if kept.is_empty() {
        return Pict::empty();
    }
    let baseline = kept.iter().map(|b| b.baseline).max().unwrap_or(0);
    let depth = kept.iter().map(|b| b.depth()).max().unwrap_or(0);
    let height = baseline + depth + 1;
    let mut lines = vec![String::new(); height];
    let mut total_width = 0;
    for b in kept {
        let above = baseline - b.baseline; // blank rows to add on top
        for (row, line) in lines.iter_mut().enumerate() {
            match row.checked_sub(above).and_then(|src| b.lines.get(src)) {
                Some(src) => line.push_str(src),
                None => line.push_str(&blank_line(b.width)),
            }
        }
        total_width += b.width;
    }
    Pict {
        lines,
        baseline,
        width: total_width,
    }
}

/// Vertical stack, every row centred to the common width, with the baseline at row `baseline_row`
/// (clamped) (box.go:193-216). No rows at all yields the empty picture.
pub(crate) fn vstack(parts: &[Pict], baseline_row: usize) -> Pict {
    let width = parts.iter().map(|b| b.width).max().unwrap_or(0);
    let lines: Vec<String> = parts
        .iter()
        .flat_map(|b| b.lines.iter().map(|l| center(l, width)))
        .collect();
    if lines.is_empty() {
        return Pict::empty();
    }
    let baseline = baseline_row.min(lines.len() - 1);
    Pict {
        lines,
        baseline,
        width,
    }
}

/// Shifts the baseline UP by `n` rows so [`hconcat`] lifts the picture, as a superscript
/// (box.go:222-236). No rows are added; `n` is clamped so the baseline stays inside.
///
/// Go defines and tests this primitive but the layout never calls it — `layout::script_column`
/// builds explicit rows instead (spec `mathtext.md` §4.2) — so it is exercised only by the
/// `box_test.go` port below.
#[allow(dead_code)] // box.go:223 parity: defined and tested, deliberately uncalled (spec §4.2)
pub(crate) fn raise(b: Pict, n: isize) -> Pict {
    if b.height() == 0 {
        return b;
    }
    let last = isize::try_from(b.height() - 1).unwrap_or(isize::MAX);
    let shifted = isize::try_from(b.baseline)
        .unwrap_or(isize::MAX)
        .saturating_sub(n);
    let baseline = usize::try_from(shifted.clamp(0, last)).unwrap_or(0);
    Pict { baseline, ..b }
}

/// Shifts the baseline DOWN by `n` rows, as a subscript (box.go:241-243 `Lower`). Like [`raise`]
/// it is Go-parity surface the layout never calls.
#[allow(dead_code)] // box.go:241 parity: defined and tested, deliberately uncalled (spec §4.2)
pub(crate) fn lower(b: Pict, n: isize) -> Pict {
    raise(b, -n)
}

/// A DRAWN full-width rule row above `b` — the vinculum of `\bar`/`\overline` (box.go:250-258).
/// The baseline shifts down one because a row was inserted above every existing row; an empty
/// picture yields the bare rule glyph.
pub(crate) fn overline(b: Pict) -> Pict {
    if b.height() == 0 {
        return Pict::new(&RULE.to_string());
    }
    let mut lines = Vec::with_capacity(b.height() + 1);
    lines.push(RULE.to_string().repeat(b.width));
    lines.extend(b.lines);
    Pict {
        lines,
        baseline: b.baseline + 1,
        width: b.width,
    }
}

/// A DRAWN accent row of `glyph` centred above `b` (box.go:267-284) — never a combining mark. A
/// glyph wider than the base widens the whole picture so the row invariant holds; an empty
/// picture yields the bare glyph.
pub(crate) fn accent_row(b: Pict, glyph: &str) -> Pict {
    if b.height() == 0 {
        return Pict::new(glyph);
    }
    let width = b.width.max(box_width(glyph));
    let mut lines = Vec::with_capacity(b.height() + 1);
    lines.push(center(glyph, width));
    lines.extend(b.lines.into_iter().map(|l| center(&l, width)));
    Pict {
        lines,
        baseline: b.baseline + 1,
        width,
    }
}

/// A one-row rule of `w` columns — the fraction bar (box.go:290-295). `w == 0` yields the empty
/// picture.
pub(crate) fn hrule(w: usize) -> Pict {
    if w == 0 {
        return Pict::empty();
    }
    Pict {
        lines: vec![RULE.to_string().repeat(w)],
        baseline: 0,
        width: w,
    }
}

/// The left delimiter of `kind` spanning `height` rows with the baseline at `baseline`
/// (box.go:349-351).
pub(crate) fn left_delim(kind: DelimKind, height: usize, baseline: usize) -> Pict {
    delim_pict(&left_glyphs(kind), height, baseline)
}

/// The right delimiter of `kind` spanning `height` rows with the baseline at `baseline`
/// (box.go:355-357).
pub(crate) fn right_delim(kind: DelimKind, height: usize, baseline: usize) -> Pict {
    delim_pict(&right_glyphs(kind), height, baseline)
}

/// Assembles one delimiter column from its glyph set (box.go:362-393): height 0 becomes 1;
/// height 1 is the `single` form on baseline 0 (the `baseline` argument is ignored, matching Go);
/// otherwise the corners cap an extension run with the brace middle piece on row `(height-1)/2`
/// — tested AFTER the corners, so a 2-tall brace is `⎧`/`⎩` with no middle.
fn delim_pict(g: &DelimGlyphs, height: usize, baseline: usize) -> Pict {
    let height = height.max(1);
    if height == 1 {
        return Pict {
            lines: vec![g.single.to_owned()],
            baseline: 0,
            width: 1,
        };
    }
    let mid_row = (height - 1) / 2;
    let lines: Vec<String> = (0..height)
        .map(|row| {
            if row == 0 {
                g.top
            } else if row == height - 1 {
                g.bottom
            } else if !g.mid.is_empty() && row == mid_row {
                g.mid
            } else {
                g.ext
            }
            .to_owned()
        })
        .collect();
    Pict {
        lines,
        baseline: baseline.min(height - 1),
        width: 1,
    }
}

/// THE width ruler of the layout: `crate::text::width::str_width` (box.go:39 `boxWidth`).
pub(crate) fn box_width(s: &str) -> usize {
    str_width(s)
}

/// Right-pads `line` with spaces to `want` columns (box.go:47); a line already that wide comes
/// back unchanged.
pub(crate) fn pad(line: &str, want: usize) -> String {
    let w = box_width(line);
    if w >= want {
        return line.to_owned();
    }
    let mut out = line.to_owned();
    out.push_str(&blank_line(want - w));
    out
}

/// Left-pads `line` with spaces to `want` columns — a right-aligned cell (layout.go:729-735).
pub(crate) fn pad_left(line: &str, want: usize) -> String {
    let w = box_width(line);
    if w >= want {
        return line.to_owned();
    }
    let mut out = blank_line(want - w);
    out.push_str(line);
    out
}

/// Centres `line` in `width` columns (box.go:175-184): the odd extra column goes RIGHT.
pub(crate) fn center(line: &str, width: usize) -> String {
    let w = box_width(line);
    if w >= width {
        return line.to_owned();
    }
    let total = width - w;
    let left = total / 2;
    let mut out = blank_line(left);
    out.push_str(line);
    out.push_str(&blank_line(total - left));
    out
}

/// `w` spaces — a full-width empty row (box.go:57).
pub(crate) fn blank_line(w: usize) -> String {
    " ".repeat(w)
}

#[cfg(test)]
mod tests {
    use super::{
        DelimKind, Pict, box_width, center, hconcat, hrule, left_delim, lower, overline, pad,
        pad_left, raise, right_delim, vstack,
    };

    /// `wantBox` (`box_test.go`:11): rows, baseline, width — and the core invariant that every row
    /// measures exactly the declared display width.
    #[track_caller]
    fn want_pict(got: &Pict, want_lines: &[&str], want_baseline: usize, want_width: usize) {
        assert_eq!(got.lines, want_lines, "lines mismatch:\n{}", got.render());
        assert_eq!(got.baseline, want_baseline, "baseline");
        assert_eq!(got.width, want_width, "width");
        for (i, l) in got.lines.iter().enumerate() {
            assert_eq!(box_width(l), got.width, "row {i} width ({l:?})");
        }
    }

    // Go: internal/mathtext/box_test.go:31 TestNewBoxSingleLine
    #[test]
    fn new_box_single_line() {
        want_pict(&Pict::new("x"), &["x"], 0, 1);
    }

    // Go: internal/mathtext/box_test.go:35 TestNewBoxLinesPadsToWidth
    #[test]
    fn new_box_lines_pads_to_width() {
        want_pict(&Pict::new_lines("ab\nc", 1), &["ab", "c "], 1, 2);
        // An empty string is ONE empty row, and an over-range baseline clamps to the last.
        want_pict(&Pict::new_lines("", 0), &[""], 0, 0);
        want_pict(&Pict::new_lines("ab\nc", 9), &["ab", "c "], 1, 2);
    }

    // Go: internal/mathtext/box_test.go:41 TestBoxWidthCJK
    #[test]
    fn box_width_cjk() {
        let b = Pict::new("中文");
        assert_eq!(b.width, 4, "CJK box width");
        let st = vstack(&[b, Pict::new("x")], 0);
        assert_eq!(st.width, 4, "stacked width");
        for l in &st.lines {
            assert_eq!(box_width(l), 4, "row {l:?} display width");
        }
    }

    // Go: internal/mathtext/box_test.go:60 TestHConcatBaselineAlignment
    #[test]
    fn hconcat_baseline_alignment() {
        let tall = Pict {
            lines: vec!["⎛".to_owned(), "⎜".to_owned(), "⎝".to_owned()],
            baseline: 1,
            width: 1,
        };
        let got = hconcat(&[Pict::new("x"), tall]);
        want_pict(&got, &[" ⎛", "x⎜", " ⎝"], 1, 2);
    }

    // Go: internal/mathtext/box_test.go:72 TestHConcatEmptyOperandsIgnored
    #[test]
    fn hconcat_empty_operands_ignored() {
        let got = hconcat(&[Pict::empty(), Pict::new("a"), Pict::empty(), Pict::new("b")]);
        want_pict(&got, &["ab"], 0, 2);
        assert_eq!(hconcat(&[]), Pict::empty(), "no operands at all");
    }

    // Go: internal/mathtext/box_test.go:77 TestVStackFractionShape
    #[test]
    fn vstack_fraction_shape() {
        let frac = vstack(&[Pict::new("a"), hrule(1), Pict::new("b")], 1);
        want_pict(&frac, &["a", "─", "b"], 1, 1);
    }

    // Go: internal/mathtext/box_test.go:88 TestVStackCentersWiderDenominator
    #[test]
    fn vstack_centers_wider_denominator() {
        let frac = vstack(&[Pict::new("1"), hrule(3), Pict::new("x+y")], 1);
        want_pict(&frac, &[" 1 ", "───", "x+y"], 1, 3);
    }

    // Go: internal/mathtext/box_test.go:99 TestOverlineWidthMatchesAndDrawsRule
    #[test]
    fn overline_width_matches_and_draws_rule() {
        let over = overline(Pict::new("x+1"));
        want_pict(&over, &["───", "x+1"], 1, 3);
        assert_eq!(over.width, Pict::new("x+1").width, "overline width");
    }

    // Go: internal/mathtext/box_test.go:112 TestHRule
    #[test]
    fn hrule_shapes() {
        want_pict(&hrule(4), &["────"], 0, 4);
        assert_eq!(hrule(0).height(), 0, "HRule(0) should be empty");
    }

    // Go: internal/mathtext/box_test.go:119 TestRaiseLowerBaseline
    #[test]
    fn raise_lower_baseline() {
        let b = Pict {
            lines: vec!["p".to_owned(), "q".to_owned(), "r".to_owned()],
            baseline: 1,
            width: 1,
        };
        assert_eq!(raise(b.clone(), 1).baseline, 0, "Raise");
        assert_eq!(lower(b.clone(), 1).baseline, 2, "Lower");
        assert_eq!(raise(b.clone(), 5).baseline, 0, "Raise over-clamp");
        assert_eq!(lower(b, 5).baseline, 2, "Lower over-clamp");
        assert_eq!(raise(Pict::empty(), 3), Pict::empty(), "empty is untouched");
    }

    // Go: internal/mathtext/box_test.go:136 TestLeftDelimTallParen
    #[test]
    fn left_delim_tall_paren() {
        want_pict(&left_delim(DelimKind::Paren, 3, 1), &["⎛", "⎜", "⎝"], 1, 1);
    }

    // Go: internal/mathtext/box_test.go:142 TestRightDelimTallBracket
    #[test]
    fn right_delim_tall_bracket() {
        want_pict(
            &right_delim(DelimKind::Bracket, 3, 1),
            &["⎤", "⎥", "⎦"],
            1,
            1,
        );
    }

    // Go: internal/mathtext/box_test.go:147 TestBraceDelimHasMiddlePiece
    #[test]
    fn brace_delim_has_middle_piece() {
        want_pict(
            &left_delim(DelimKind::Brace, 5, 2),
            &["⎧", "⎪", "⎨", "⎪", "⎩"],
            2,
            1,
        );
        // The corner cases are tested FIRST, so a 2-tall brace has no middle piece.
        want_pict(&left_delim(DelimKind::Brace, 2, 0), &["⎧", "⎩"], 0, 1);
    }

    // Go: internal/mathtext/box_test.go:154 TestDelimHeightOneIsSingleGlyph
    #[test]
    fn delim_height_one_is_single_glyph() {
        want_pict(&left_delim(DelimKind::Paren, 1, 0), &["("], 0, 1);
        want_pict(&right_delim(DelimKind::Paren, 1, 0), &[")"], 0, 1);
        want_pict(&left_delim(DelimKind::Bracket, 1, 0), &["["], 0, 1);
        want_pict(&left_delim(DelimKind::Brace, 1, 0), &["{"], 0, 1);
        want_pict(&left_delim(DelimKind::Bar, 1, 0), &["│"], 0, 1);
        // A height of 0 is lifted to 1 (box.go:363).
        want_pict(&right_delim(DelimKind::Brace, 0, 4), &["}"], 0, 1);
    }

    // Go: internal/mathtext/box_test.go:162 TestBarDelimIsAllVerticals
    #[test]
    fn bar_delim_is_all_verticals() {
        want_pict(&left_delim(DelimKind::Bar, 3, 1), &["│", "│", "│"], 1, 1);
        want_pict(&right_delim(DelimKind::Bar, 3, 1), &["│", "│", "│"], 1, 1);
    }

    // Go: internal/mathtext/box_test.go:167 TestFractionInsideTallParens
    #[test]
    fn fraction_inside_tall_parens() {
        let frac = vstack(&[Pict::new("a"), hrule(1), Pict::new("b")], 1);
        let left = left_delim(DelimKind::Paren, frac.height(), frac.baseline);
        let right = right_delim(DelimKind::Paren, frac.height(), frac.baseline);
        let got = hconcat(&[left, frac, right]);
        want_pict(&got, &["⎛a⎞", "⎜─⎟", "⎝b⎠"], 1, 3);
    }

    // Go: internal/mathtext/box.go:47,175,729 pad/center/padLeft — the padding helpers, incl. the
    // "extra column goes RIGHT" rule the whole layout leans on.
    #[test]
    fn padding_helpers() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("abcde", 2), "abcde", "never truncates");
        assert_eq!(pad_left("ab", 5), "   ab");
        assert_eq!(center("a", 4), " a  ", "the odd column goes right");
        assert_eq!(center("ab", 4), " ab ");
        assert_eq!(center("abc", 2), "abc", "never truncates");
        assert_eq!(center("中", 4), " 中 ", "measured in display columns");
    }

    // Go: internal/mathtext/box.go:104-119 isEmptyBox/String/depth.
    #[test]
    fn empty_render_and_depth() {
        assert!(Pict::empty().is_empty());
        assert!(Pict::new("").is_empty(), "width 0 counts as empty");
        assert_eq!(Pict::empty().render(), "");
        assert_eq!(Pict::new("x").depth(), 0, "a one-row picture has depth 0");
        let b = Pict {
            lines: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            baseline: 0,
            width: 1,
        };
        assert_eq!(b.depth(), 2);
    }
}
