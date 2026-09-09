//! The two-dimensional layout (internal/mathtext/layout.go): one function per [`Node`] kind
//! composing [`Pict`]s — fractions over a rule, radicals with a vinculum, stacked limits, tall
//! integrals, matrices with delimiters. The laws are spec `mathtext.md` §4; the 2D goldens under
//! `tests/fixtures/mathtext/` pin them byte for byte.
//!
//! The geometry is a hand-rolled port of `SymPy`'s `stringPict`/`prettyForm` pretty printer
//! (`sympy/printing/pretty/*`, BSD-3-Clause) exactly as Go's `layout.go:13-21` records: the
//! ALGORITHM is the reference, no upstream code is used.
//!
//! No combining mark (U+0300..=U+036F) is ever emitted: bars, vinculums, accents, tall
//! delimiters and tall operators are DRAWN across rows from spacing glyphs.

use crate::mathtext::parse::{Node, OpFamily};
use crate::mathtext::pict::{
    DelimKind, Pict, accent_row, blank_line, box_width, center, hconcat, hrule, left_delim,
    overline, pad, pad_left, right_delim, vstack,
};
use crate::mathtext::{macros, symbols};

/// Column gap inside a matrix (layout.go:579).
pub(crate) const MATRIX_COL_GAP: usize = 2;
/// Column gap inside `cases` (layout.go:582).
pub(crate) const CASES_COL_GAP: usize = 2;
/// Column gap inside `aligned` (layout.go:588).
pub(crate) const ALIGNED_COL_GAP: usize = 1;

/// Binary operators that get a space on both sides (layout.go:106-116; `-` is handled separately).
pub(crate) const BINARY_OPS: &str = "+=<>×÷±∓⋅∗≤≥≠≈≡∼≃≅∝≪≫∈∉∋⊂⊆⊃⊇∪∩∧∨∣∤‖∥≍≐→←↔⇒⇐⇔↦⊕⊗⊙∘∖";

/// The rising stroke of a radical (layout.go:281 — U+2571).
const SQRT_ARM: char = '╱';
/// The base of a radical's check (layout.go:283 — U+2572).
const SQRT_FOOT: char = '╲';
/// The top piece of a tall integral (layout.go:497 — U+2320).
const INT_TOP: &str = "⌠";
/// The extension piece of a tall integral (layout.go:500 — U+23AE).
const INT_EXT: &str = "⎮";
/// The bottom piece of a tall integral (layout.go:498 — U+2321).
const INT_BOTTOM: &str = "⌡";

/// Cell alignment inside an environment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CellAlign {
    /// Left-aligned.
    Left,
    /// Centred.
    Center,
    /// Right-aligned.
    Right,
}

/// Lays out `n` (layout.go:29-62 `layout`). Go's `nil` node is this crate's absent optional or
/// empty `Seq`; both give the empty picture.
pub(crate) fn layout(n: &Node) -> Pict {
    match n {
        Node::Seq(items) => layout_seq(items),
        // layout.go:63-79 layoutAtom / layoutText: one row, uniseg-measured; "" is nothing.
        Node::Atom(s) | Node::Text(s) => {
            if s.is_empty() {
                Pict::empty()
            } else {
                Pict::new(s)
            }
        }
        Node::Frac { num, den } => layout_frac(num, den),
        Node::Sqrt { radicand, index } => layout_sqrt(radicand, index.as_deref()),
        Node::Sup { base, exp } => layout_sup(base, exp),
        Node::Sub { base, sub } => layout_sub(base, sub),
        Node::SupSub { base, sup, sub } => layout_sup_sub(base, sup, sub),
        Node::Delim { left, right, inner } => layout_delim(left, right, inner),
        Node::BigOp { .. } => layout_big_op(n, 1),
        Node::Matrix { env, rows } => layout_matrix(env, rows),
        Node::Accent { kind, base } => layout_accent(*kind, base),
    }
}

/// [`layout`] of an optional node; `None` → the empty picture (Go's `layout(nil)`).
pub(crate) fn layout_opt(n: Option<&Node>) -> Pict {
    n.map_or_else(Pict::empty, layout)
}

/// The thin space inserted around a binary operator (layout.go:210 `spaceBox`).
fn space_pict() -> Pict {
    Pict::new(" ")
}

/// Whether `b` is a lone binary/relational operator wanting space on both sides (layout.go:126-
/// 139): one row whose baseline line is exactly ONE rune (a rune COUNT, so a 2-column CJK atom is
/// still one rune) that is `-` or a member of [`BINARY_OPS`].
fn is_binary_op_pict(b: &Pict) -> bool {
    if b.height() != 1 {
        return false;
    }
    let Some(line) = b.lines.get(b.baseline) else {
        return false;
    };
    let mut runes = line.chars();
    let (Some(c), None) = (runes.next(), runes.next()) else {
        return false;
    };
    c == '-' || BINARY_OPS.contains(c)
}

/// Lays out a run of nodes left to right (layout.go:147-206): a glyph big operator first folds in
/// the sibling that follows it (so `∫` can grow to the integrand's height), then a thin space is
/// inserted on whichever side of a binary operator carries an operand — a relation that OPENS an
/// alignment cell still keeps its trailing space (`&= y` reads `= y`).
fn layout_seq(items: &[Node]) -> Pict {
    if items.is_empty() {
        return Pict::empty();
    }
    let mut boxes: Vec<Pict> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if let Node::BigOp { word, .. } = &items[i] {
            // Lay the following operand out first: it sizes the operator glyph.
            let operand = items.get(i + 1).map_or_else(Pict::empty, layout);
            let h = operand.height().max(1);
            let mut op_box = layout_big_op(&items[i], h);
            if operand.height() > 0 {
                op_box = if word.is_empty() {
                    hconcat(&[op_box, operand])
                } else {
                    hconcat(&[op_box, space_pict(), operand])
                };
                i += 1; // the operand was folded into the operator picture
            }
            boxes.push(op_box);
            i += 1;
            continue;
        }
        let b = layout(&items[i]);
        if b.height() != 0 && b.width != 0 {
            boxes.push(b);
        }
        i += 1;
    }
    if boxes.is_empty() {
        return Pict::empty();
    }
    let last = boxes.len() - 1;
    let mut parts: Vec<Pict> = Vec::with_capacity(boxes.len());
    for (i, b) in boxes.into_iter().enumerate() {
        if is_binary_op_pict(&b) {
            if i > 0 {
                parts.push(space_pict());
            }
            parts.push(b);
            if i < last {
                parts.push(space_pict());
            }
            continue;
        }
        parts.push(b);
    }
    hconcat(&parts)
}

/// `\frac{num}{den}` (layout.go:216-230): the numerator centred over a drawn bar over the centred
/// denominator, the baseline ON the bar. The bar spans the wider part; two empty parts still give
/// a single `─` row.
fn layout_frac(num: &Node, den: &Node) -> Pict {
    let num = layout(num);
    let den = layout(den);
    let w = num.width.max(den.width).max(1);
    let bar = hrule(w);
    let baseline_row = num.height();
    vstack(&[num, bar, den], baseline_row)
}

/// `\sqrt[index]{radicand}` (layout.go:250-266): the radicand under a drawn vinculum with a check
/// whose `╱` stroke climbs to meet it; the optional degree index rides the radical's upper left.
fn layout_sqrt(radicand: &Node, index: Option<&Node>) -> Pict {
    let mut rad = layout(radicand);
    if rad.height() == 0 {
        rad = Pict::new(" ");
    }
    let body = sqrt_body(&rad);
    let Some(index) = index else {
        return body;
    };
    let idx = layout(index);
    if idx.height() == 0 {
        return body;
    }
    let col = script_column(&idx, &Pict::empty(), 0);
    hconcat(&[col, body])
}

/// Wraps a laid-out radicand in a scalable radical sign (layout.go:272-290). The wedge is
/// `h + 1` columns: a `╲` at the bottom-left and one `╱` per row climbing to the vinculum, which
/// is a run of `w + 1` underscores (they paint on the cell's bottom edge, so the bar sits
/// directly on the radicand and meets the topmost `╱` tip).
fn sqrt_body(rad: &Pict) -> Pict {
    let h = rad.height();
    let w = rad.width;
    let wedge = h + 1; // the ╲ column plus one ╱ per row
    let mut lines = Vec::with_capacity(h + 1);
    lines.push(blank_line(wedge) + &"_".repeat(w + 1));
    for (r, row) in rad.lines.iter().enumerate() {
        let mut arm = vec![' '; wedge];
        arm[wedge - 1 - r] = SQRT_ARM; // ╱ climbs right→left going up (row 0 is highest)
        if r == h - 1 {
            arm[0] = SQRT_FOOT; // the base of the check
        }
        let mut line: String = arm.into_iter().collect();
        line.push(' ');
        line.push_str(row);
        lines.push(line);
    }
    Pict {
        lines,
        baseline: rad.baseline + 1,
        width: wedge + 1 + w,
    }
}

/// `base^exp` (layout.go:296-304): the exponent in its own rows strictly above the base baseline.
fn layout_sup(base_node: &Node, exp_node: &Node) -> Pict {
    let base = layout(base_node);
    let exp = layout(exp_node);
    if exp.height() == 0 {
        return base;
    }
    let col = script_column(&exp, &Pict::empty(), accent_lift(base_node));
    hconcat(&[base, col])
}

/// `base_sub` (layout.go:307-314): the subscript strictly below the base baseline.
fn layout_sub(base_node: &Node, sub_node: &Node) -> Pict {
    let base = layout(base_node);
    let sub = layout(sub_node);
    if sub.height() == 0 {
        return base;
    }
    let col = script_column(&Pict::empty(), &sub, 0);
    hconcat(&[base, col])
}

/// `base^sup_sub` (layout.go:317-326): both scripts share ONE column right of the base.
fn layout_sup_sub(base_node: &Node, up: &Node, down: &Node) -> Pict {
    let base = layout(base_node);
    let sup = layout(up);
    let sub = layout(down);
    if sup.height() == 0 && sub.height() == 0 {
        return base;
    }
    let col = script_column(&sup, &sub, accent_lift(base_node));
    hconcat(&[base, col])
}

/// The extra rows a superscript rises to clear an accent drawn over the base (layout.go:335-340):
/// the accent's own ascent for an [`Node::Accent`] base, 0 for anything else. A SUBSCRIPT never
/// needs it — the accent sits on top.
fn accent_lift(base: &Node) -> usize {
    if matches!(base, Node::Accent { .. }) {
        layout(base).baseline
    } else {
        0
    }
}

/// The script column [`hconcat`] places right of a base (layout.go:358-379): the superscript's
/// rows, `sup_lift` blank rows, ONE blank anchor row (the baseline, so the base rune shows through
/// beside it) and the subscript's rows.
///
/// Go's `scriptColumn` takes the base as its first argument and never reads it; the parameter is
/// dropped here.
fn script_column(sup: &Pict, sub: &Pict, sup_lift: usize) -> Pict {
    let w = sup.width.max(sub.width).max(1);
    let mut rows: Vec<String> = Vec::with_capacity(sup.height() + sup_lift + 1 + sub.height());
    rows.extend(sup.lines.iter().map(|l| center(l, w)));
    for _ in 0..sup_lift {
        rows.push(blank_line(w));
    }
    let anchor = rows.len();
    rows.push(blank_line(w)); // the base-baseline row, blank in the script
    rows.extend(sub.lines.iter().map(|l| center(l, w)));
    Pict {
        lines: rows,
        baseline: anchor,
        width: w,
    }
}

/// An accent drawn as a glyph ROW above the base (layout.go:89-99), never a combining mark:
/// `\bar`/`\overline` reuse the full-width rule, every other accent is centred over the base.
fn layout_accent(kind: symbols::AccentKind, base_node: &Node) -> Pict {
    let mut base = layout(base_node);
    if base.height() == 0 {
        base = Pict::new(" ");
    }
    match symbols::accent_glyph(kind) {
        // symbols.go:318: \bar / \overline are full-width — the drawn rule spans the whole box.
        None => overline(base),
        Some(glyph) => accent_row(base, glyph),
    }
}

/// The [`DelimKind`] a normalized delimiter string draws, or `None` when the side is not drawn
/// (layout.go:383-401): `.`/`""` are invisible, and angle/floor/ceil (`⟨⟩ ⌊⌋ ⌈⌉ /`) have no tall
/// box-drawing form, so they fall through to [`literal_delim_pict`] rather than a wrong bracket.
fn delim_kind_for(s: &str) -> Option<DelimKind> {
    match s {
        "(" | ")" => Some(DelimKind::Paren),
        "[" | "]" => Some(DelimKind::Bracket),
        "{" | "}" => Some(DelimKind::Brace),
        "|" => Some(DelimKind::Bar),
        _ => None,
    }
}

/// A delimiter with no extensible tall form drawn as its literal glyph on the baseline row,
/// padded to the inner height (layout.go:406-423).
fn literal_delim_pict(glyph: &str, height: usize, baseline: usize) -> Pict {
    if glyph.is_empty() {
        return Pict::empty();
    }
    if height <= 1 {
        return Pict::new(glyph);
    }
    let w = box_width(glyph);
    let lines: Vec<String> = (0..height)
        .map(|i| {
            if i == baseline {
                glyph.to_owned()
            } else {
                blank_line(w)
            }
        })
        .collect();
    Pict {
        lines,
        baseline,
        width: w,
    }
}

/// `\left<L> inner \right<R>` (layout.go:428-443): the inner picture flanked by delimiters sized
/// to its height and aligned on its baseline; a `.` side is omitted.
fn layout_delim(left: &str, right: &str, inner: &Node) -> Pict {
    let inner = layout(inner);
    let h = inner.height().max(1);
    let l = delim_side(left, h, inner.baseline, true);
    let r = delim_side(right, h, inner.baseline, false);
    let mut parts = Vec::with_capacity(3);
    if !l.is_empty() {
        parts.push(l);
    }
    parts.push(inner);
    if !r.is_empty() {
        parts.push(r);
    }
    hconcat(&parts)
}

/// One side of a `\left…\right` pair (layout.go:448-459).
fn delim_side(sym: &str, height: usize, baseline: usize, left: bool) -> Pict {
    if sym.is_empty() || sym == "." {
        return Pict::empty();
    }
    match delim_kind_for(sym) {
        Some(kind) if left => left_delim(kind, height, baseline),
        Some(kind) => right_delim(kind, height, baseline),
        None => literal_delim_pict(sym, height, baseline),
    }
}

/// The rows of a tall integral of `height` rows (layout.go:493-505 `bigOpGlyph`'s `int` closure):
/// `⌠` over `⎮` extensions over `⌡`. Only the integral family is extensible; every other big
/// operator stays its single glyph however tall the operand is (p3fix `TestSumSingleGlyphOverTall
/// Body`), so Go's `repeatGlyphColumn` is unreachable and is not ported.
fn tall_integral(height: usize) -> String {
    let mut rows = Vec::with_capacity(height);
    rows.push(INT_TOP);
    rows.extend(std::iter::repeat_n(INT_EXT, height.saturating_sub(2)));
    rows.push(INT_BOTTOM);
    rows.join("\n")
}

/// A large operator with its limits (layout.go:511-548): the glyph (or the upright word) carries
/// the upper limit centred above and the lower centred below. `body_height` is the row count of
/// the sibling operand [`layout_seq`] folded in — it only ever grows an INTEGRAL.
fn layout_big_op(n: &Node, body_height: usize) -> Pict {
    let Node::BigOp {
        op,
        glyph,
        word,
        lower,
        upper,
    } = n
    else {
        return Pict::empty();
    };
    let op_box = if word.is_empty() {
        if *op == OpFamily::Int && body_height >= 2 {
            Pict::new_lines(&tall_integral(body_height), body_height / 2)
        } else {
            Pict::new(glyph)
        }
    } else {
        Pict::new(word) // \lim, \max, \det, …
    };

    let lower = layout_opt(lower.as_deref());
    let upper = layout_opt(upper.as_deref());
    // Stack: upper limit, operator, lower limit; the baseline stays on the operator.
    let mut stack = Vec::with_capacity(3);
    let mut baseline_row = 0;
    if upper.height() > 0 {
        baseline_row = upper.height();
        stack.push(upper);
    }
    let op_baseline = baseline_row + op_box.baseline;
    stack.push(op_box.clone());
    if lower.height() > 0 {
        stack.push(lower);
    }
    if stack.len() == 1 {
        return op_box;
    }
    vstack(&stack, op_baseline)
}

/// The delimiter kinds wrapping a matrix environment and whether each side is drawn
/// (layout.go:559-576). `Vmatrix` is a single bar like `vmatrix`; `cases` has only a left brace;
/// `matrix`/`smallmatrix` and every alignment environment have none.
fn matrix_delims_for(env: &str) -> (DelimKind, DelimKind, bool, bool) {
    match env {
        "pmatrix" => (DelimKind::Paren, DelimKind::Paren, true, true),
        "bmatrix" => (DelimKind::Bracket, DelimKind::Bracket, true, true),
        "Bmatrix" => (DelimKind::Brace, DelimKind::Brace, true, true),
        "vmatrix" | "Vmatrix" => (DelimKind::Bar, DelimKind::Bar, true, true),
        "cases" => (DelimKind::Brace, DelimKind::Brace, true, false),
        _ => (DelimKind::Paren, DelimKind::Paren, false, false),
    }
}

/// The alignment of column `j` of environment `env` (layout.go:603-617): `cases` left, gathered
/// centred, other alignment environments right for column 0 and left after, matrix family centred.
fn column_align(env: &str, j: usize) -> CellAlign {
    if env == "cases" {
        CellAlign::Left
    } else if macros::is_gathered_env(env) {
        CellAlign::Center
    } else if macros::is_aligned_env(env) {
        if j == 0 {
            CellAlign::Right
        } else {
            CellAlign::Left
        }
    } else {
        CellAlign::Center
    }
}

/// The inter-column gap of environment `env` (layout.go:620-629).
fn column_gap(env: &str) -> usize {
    if env == "cases" {
        CASES_COL_GAP
    } else if macros::is_aligned_env(env) {
        ALIGNED_COL_GAP
    } else {
        MATRIX_COL_GAP
    }
}

/// An array environment as an aligned grid inside its delimiters (layout.go:638-707): columns
/// sized to their widest cell, rows baseline-aligned, the grid's middle row the baseline, and ONE
/// blank column between a delimiter and the grid.
fn layout_matrix(env: &str, rows: &[Vec<Node>]) -> Pict {
    if rows.is_empty() {
        return Pict::empty();
    }
    let mut cols = 0;
    let cells: Vec<Vec<Pict>> = rows
        .iter()
        .map(|row| {
            cols = cols.max(row.len());
            row.iter().map(layout).collect()
        })
        .collect();
    if cols == 0 {
        return Pict::empty();
    }

    let mut col_width = vec![0usize; cols];
    for row in &cells {
        for (j, b) in row.iter().enumerate() {
            col_width[j] = col_width[j].max(b.width);
        }
    }

    let gap = column_gap(env);
    let row_boxes: Vec<Pict> = cells
        .iter()
        .map(|row| {
            let mut parts = Vec::with_capacity(cols * 2);
            for (j, &want) in col_width.iter().enumerate() {
                let empty = Pict::empty();
                let cell = row.get(j).unwrap_or(&empty);
                parts.push(pad_cell(cell, want, column_align(env, j)));
                if j < cols - 1 {
                    parts.push(blank_columns(gap));
                }
            }
            hconcat(&parts)
        })
        .collect();

    let grid = stack_rows(&row_boxes);
    let (left, right, draw_left, draw_right) = matrix_delims_for(env);
    let h = grid.height().max(1);
    let grid_baseline = grid.baseline;
    let mut parts = Vec::with_capacity(5);
    if draw_left {
        parts.push(left_delim(left, h, grid_baseline));
        parts.push(blank_columns(1));
    }
    parts.push(grid);
    if draw_right {
        parts.push(blank_columns(1));
        parts.push(right_delim(right, h, grid_baseline));
    }
    hconcat(&parts)
}

/// Pads a laid-out cell to its column width per `align`, keeping its own baseline
/// (layout.go:711-733). An absent cell (a ragged row) becomes ONE blank row of the column width.
fn pad_cell(cell: &Pict, width: usize, align: CellAlign) -> Pict {
    if cell.height() == 0 {
        return Pict {
            lines: vec![blank_line(width)],
            baseline: 0,
            width,
        };
    }
    if cell.width >= width {
        return cell.clone();
    }
    let lines = cell
        .lines
        .iter()
        .map(|l| match align {
            CellAlign::Left => pad(l, width),
            CellAlign::Right => pad_left(l, width),
            CellAlign::Center => center(l, width),
        })
        .collect();
    Pict {
        lines,
        baseline: cell.baseline,
        width,
    }
}

/// A one-row picture of `n` blank columns — an inter-column gap (layout.go:748-753).
fn blank_columns(n: usize) -> Pict {
    if n == 0 {
        return Pict::empty();
    }
    Pict {
        lines: vec![blank_line(n)],
        baseline: 0,
        width: n,
    }
}

/// Stacks row pictures top to bottom with NO blank separator (layout.go:756-779), centring each to
/// the common width; the grid baseline is the baseline row of the vertically middle row, so a pair
/// of tall delimiters brackets the grid symmetrically.
fn stack_rows(rows: &[Pict]) -> Pict {
    if rows.is_empty() {
        return Pict::empty();
    }
    let width = rows.iter().map(|r| r.width).max().unwrap_or(0);
    let mid = (rows.len() - 1) / 2;
    let mut lines: Vec<String> = Vec::new();
    let mut baseline_row = 0;
    for (i, r) in rows.iter().enumerate() {
        if i == mid {
            baseline_row = lines.len() + r.baseline;
        }
        lines.extend(r.lines.iter().map(|l| center(l, width)));
    }
    Pict {
        lines,
        baseline: baseline_row,
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::layout;
    use crate::mathtext::parse::parse;
    use crate::mathtext::pict::{Pict, box_width};
    use crate::mathtext::{has_combining_mark, render_2d};

    /// `layoutOf` (`layout_test.go`:9): parse then lay out, failing on a parse error.
    #[track_caller]
    fn layout_of(input: &str) -> Pict {
        match parse(input) {
            Ok(n) => layout(&n),
            Err(e) => panic!("parse({input:?}) error: {e}"),
        }
    }

    /// `wantLayout` (`layout_test.go`:21): exact rows and baseline, PLUS the box invariant (every
    /// row measures the declared width) and the hard rule that no combining mark ever appears.
    #[track_caller]
    fn want_layout(input: &str, want_lines: &[&str], want_baseline: usize) {
        let got = layout_of(input);
        assert_eq!(
            got.baseline,
            want_baseline,
            "{input:?} baseline\n{}",
            got.render()
        );
        assert_eq!(
            got.lines.len(),
            want_lines.len(),
            "{input:?} row count:\n{}",
            got.render()
        );
        for (i, want) in want_lines.iter().enumerate() {
            assert_eq!(
                &got.lines[i],
                want,
                "{input:?} row {i}\nfull:\n{}",
                got.render()
            );
        }
        for (i, l) in got.lines.iter().enumerate() {
            assert_eq!(box_width(l), got.width, "{input:?} row {i} width ({l:?})");
        }
        assert!(
            !has_combining_mark(&got.render()),
            "{input:?} layout contains a combining mark:\n{}",
            got.render()
        );
    }

    // Go: internal/mathtext/layout_test.go:47 TestLayoutFracStacked
    #[test]
    fn layout_frac_stacked() {
        want_layout(r"\frac{a}{b}", &["a", "─", "b"], 1);
    }

    // Go: internal/mathtext/layout_test.go:57 TestLayoutFracWideDenominator
    #[test]
    fn layout_frac_wide_denominator() {
        want_layout(r"\frac{a+b}{c}", &["a + b", "─────", "  c  "], 1);
    }

    // Go: internal/mathtext/layout_test.go:67 TestLayoutNestedFrac
    #[test]
    fn layout_nested_frac() {
        want_layout(r"\frac{\frac{a}{b}}{c}", &["a", "─", "b", "─", "c"], 3);
    }

    // Go: internal/mathtext/layout_test.go:80 TestLayoutSqrtVinculum
    #[test]
    fn layout_sqrt_vinculum() {
        want_layout(r"\sqrt{x+1}", &["  ______", "╲╱ x + 1"], 1);
    }

    // Go: internal/mathtext/layout_test.go:89 TestLayoutRootIndex
    #[test]
    fn layout_root_index() {
        want_layout(r"\sqrt[3]{x}", &["3  __", " ╲╱ x"], 1);
    }

    // Go: internal/mathtext/layout_test.go:98 TestLayoutSubSup
    #[test]
    fn layout_sub_sup() {
        want_layout("x_i^2", &[" 2", "x ", " i"], 1);
    }

    // Go: internal/mathtext/layout_test.go:107 TestLayoutSup
    #[test]
    fn layout_sup() {
        want_layout("x^2", &[" 2", "x "], 1);
    }

    // Go: internal/mathtext/layout_test.go:116 TestLayoutSumWithLimits
    #[test]
    fn layout_sum_with_limits() {
        want_layout(r"\sum_{i=1}^{n} i", &["  n   ", "  ∑  i", "i = 1 "], 1);
    }

    // Go: internal/mathtext/layout_test.go:126 TestLayoutTallIntegral
    #[test]
    fn layout_tall_integral() {
        want_layout(r"\int \frac{1}{x}", &["⌠1", "⎮─", "⌡x"], 1);
    }

    // Go: internal/mathtext/layout_test.go:135 TestLayoutPmatrix
    #[test]
    fn layout_pmatrix() {
        want_layout(
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            &["⎛ a  b ⎞", "⎝ c  d ⎠"],
            0,
        );
    }

    // Go: internal/mathtext/layout_test.go:143 TestLayoutBmatrix
    #[test]
    fn layout_bmatrix() {
        want_layout(
            r"\begin{bmatrix} 1 & 2 \\ 3 & 4 \end{bmatrix}",
            &["⎡ 1  2 ⎤", "⎣ 3  4 ⎦"],
            0,
        );
    }

    // Go: internal/mathtext/layout_test.go:151 TestLayoutCases
    #[test]
    fn layout_cases() {
        want_layout(
            r"\begin{cases} x & a \\ y & b \end{cases}",
            &["⎧ x  a", "⎩ y  b"],
            0,
        );
    }

    // Go: internal/mathtext/layout_test.go:160 TestLayoutDelimAroundFrac
    #[test]
    fn layout_delim_around_frac() {
        want_layout(r"\left( \frac{a}{b} \right)", &["⎛a⎞", "⎜─⎟", "⎝b⎠"], 1);
    }

    // Go: internal/mathtext/layout_test.go:169 TestLayoutLim
    #[test]
    fn layout_lim() {
        want_layout(r"\lim_{x \to 0} f", &[" lim  f", "x → 0  "], 0);
    }

    // Go: internal/mathtext/layout_test.go:178 TestLayoutCJKText
    #[test]
    fn layout_cjk_text() {
        let got = layout_of(r"\text{中文}");
        assert_eq!(got.width, 4, "CJK text width\n{}", got.render());
        assert_eq!(got.render(), "中文");
    }

    // Go: internal/mathtext/layout_test.go:191 TestLayoutQuadraticFormula
    #[test]
    fn layout_quadratic_formula() {
        let got = layout_of(r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}");
        assert!(
            !has_combining_mark(&got.render()),
            "quadratic formula has a combining mark:\n{}",
            got.render()
        );
        let bar = &got.lines[got.baseline];
        assert!(
            bar.trim_matches('─').is_empty() && box_width(bar) == got.width,
            "baseline row is not a full-width bar: {bar:?}"
        );
        assert_eq!(
            got.height(),
            5,
            "quadratic formula height:\n{}",
            got.render()
        );
    }

    // Go: internal/mathtext/layout_test.go:209 TestLayoutAccentHat
    #[test]
    fn layout_accent_hat() {
        want_layout(r"\hat{f}", &["^", "f"], 1);
    }

    // Go: internal/mathtext/layout_test.go:217 TestLayoutAccentVec
    #[test]
    fn layout_accent_vec() {
        want_layout(r"\vec{v}", &["→", "v"], 1);
    }

    // Go: internal/mathtext/layout_test.go:226 TestLayoutAccentDdot
    #[test]
    fn layout_accent_ddot() {
        want_layout(r"\ddot{x}", &["··", "x "], 1);
    }

    // Go: internal/mathtext/layout_test.go:235 TestLayoutOverline
    #[test]
    fn layout_overline() {
        want_layout(r"\overline{ab}", &["──", "ab"], 1);
    }

    // Go: internal/mathtext/layout_test.go:244 TestLayoutMathbb
    #[test]
    fn layout_mathbb() {
        assert_eq!(layout_of(r"\mathbb{R}").render(), "ℝ");
        assert_eq!(layout_of(r"\mathbf{E}").render(), "E", "plain degrade");
        assert_eq!(layout_of(r"\mathcal{L}").render(), "ℒ");
    }

    // Go: internal/mathtext/layout_test.go:258 TestLayoutAligned
    #[test]
    fn layout_aligned() {
        let got = layout_of(r"\begin{aligned} a &= b \\ c &= d \end{aligned}");
        assert_eq!(got.height(), 2, "aligned rows:\n{}", got.render());
        assert!(
            !got.render()
                .contains(['⎛', '⎝', '⎡', '⎣', '(', ')', '[', ']']),
            "aligned should carry no delimiters:\n{}",
            got.render()
        );
        let c0 = got.lines[0].find('=');
        let c1 = got.lines[1].find('=');
        assert!(
            c0.is_some() && c0 == c1,
            "aligned rows not lined up on '=': {c0:?}/{c1:?}\n{}",
            got.render()
        );
    }

    // Go: internal/mathtext/layout_test.go:276 TestLayoutLiteralBrace
    #[test]
    fn layout_literal_brace() {
        let got = layout_of(r"\{ x \}").render();
        assert!(
            got.starts_with('{') && got.ends_with('}'),
            "\\{{ x \\}} = {got:?}, want literal braces"
        );
    }

    // Go: internal/mathtext/layout_test.go:286 TestLayoutNoCombiningMarksBattery
    #[test]
    fn layout_no_combining_marks_battery() {
        for input in [
            r"\frac{a}{b}",
            r"\sqrt{x+1}",
            r"\sqrt[3]{x}",
            "x_i^2",
            r"\sum_{i=1}^{n} i",
            r"\int_0^1 \frac{1}{x}",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            r"\begin{cases} x & a \\ y & b \end{cases}",
            r"\left( \frac{a}{b} \right)",
            r"\lim_{x \to 0} f",
            r"\text{中文} + \alpha",
            r"\hat{f}",
            r"\vec{v}",
            r"\bar{x}",
            r"\dot{x}",
            r"\ddot{x}",
            r"\overline{ab}",
            r"\tilde{a}",
            r"\mathbb{R} \cup \mathbb{C}",
            r"\mathcal{L}",
            r"\begin{aligned} a &= b \\ c &= d \end{aligned}",
        ] {
            let got = layout_of(input);
            assert!(
                !has_combining_mark(&got.render()),
                "{input:?} produced a combining mark:\n{}",
                got.render()
            );
        }
    }

    // ---- p3fix_test.go: the Phase-3 review regressions ----

    // Go: internal/mathtext/p3fix_test.go:14 TestBareDelimiterMacros — a bare delimiter macro
    // outside \left…\right is an ordinary glyph, never a leaked macro name.
    #[test]
    fn bare_delimiter_macros() {
        for (src, want) in [
            (r"\langle a, b \rangle", &['⟨', '⟩'][..]),
            (
                r"\lfloor x \rfloor + \lceil y \rceil",
                &['⌊', '⌋', '⌈', '⌉'],
            ),
        ] {
            let (block, ok) = render_2d(src, 80);
            assert!(ok, "render_2d({src:?}) fell back:\n{block}");
            for r in want {
                assert!(
                    block.contains(*r),
                    "render_2d({src:?}) missing {r:?}:\n{block}"
                );
            }
            assert!(
                !block.contains("langle") && !block.contains("floor") && !block.contains("ceil"),
                "render_2d({src:?}) leaked a macro name:\n{block}"
            );
        }
    }

    // Go: internal/mathtext/p3fix_test.go:39 TestLeftRightAngleNotBar — an auto-sized \left…\right
    // around a tall body draws the real glyph, never a │ substitute.
    #[test]
    fn left_right_angle_not_bar() {
        let (block, ok) = render_2d(r"\left\langle \frac{a}{b}, c \right\rangle", 80);
        assert!(ok, "render_2d fell back:\n{block}");
        assert!(
            block.contains('⟨') && block.contains('⟩'),
            "angle delimiters not drawn:\n{block}"
        );
        assert!(
            !block.contains('│'),
            "angle pair drew a │ bar instead of ⟨⟩:\n{block}"
        );
    }

    // Go: internal/mathtext/p3fix_test.go:55 TestSumSingleGlyphOverTallBody — only ∫ grows.
    #[test]
    fn sum_single_glyph_over_tall_body() {
        let (block, ok) = render_2d(r"\sum_{i=1}^{n}\frac{1}{i^2}", 80);
        assert!(ok, "render_2d fell back:\n{block}");
        assert_eq!(
            block.matches('∑').count(),
            1,
            "∑ must be drawn exactly once:\n{block}"
        );
        let (iblock, _) = render_2d(r"\int_{0}^{\infty}\frac{1}{x^2}dx", 80);
        assert!(
            iblock.contains('⌠'),
            "∫ over a fraction should grow to ⌠⎮⌡:\n{iblock}"
        );
    }

    // Go: internal/mathtext/p3fix_test.go:73 TestAlignedRelationSpacing — a relation opening an
    // alignment cell keeps its trailing space.
    #[test]
    fn aligned_relation_spacing() {
        let (block, ok) = render_2d(r"\begin{aligned} x &= y + z \\ a &= b \end{aligned}", 80);
        assert!(ok, "render_2d fell back:\n{block}");
        assert!(
            block.contains("x = y"),
            "aligned relation missing space after '=':\n{block}"
        );
        let (m, _) = render_2d(r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}", 80);
        for line in m.split('\n') {
            if let Some(i) = line.find('=') {
                let next = &line[i + 1..];
                assert!(
                    next.is_empty() || next.starts_with(' '),
                    "no space after '=' in {line:?}:\n{m}"
                );
            }
        }
    }

    // Go: internal/mathtext/p3fix_test.go:95 TestAccentScriptLift — a superscript on an accented
    // base floats ABOVE the accent glyph instead of sharing its row.
    #[test]
    fn accent_script_lift() {
        let (block, ok) = render_2d(r"\hat{x}^2", 80);
        assert!(ok, "render_2d fell back:\n{block}");
        let lines: Vec<&str> = block.split('\n').collect();
        let row_of = |r: char| lines.iter().position(|l| l.contains(r));
        let (two, hat, x) = (row_of('2'), row_of('^'), row_of('x'));
        let (Some(two), Some(hat), Some(x)) = (two, hat, x) else {
            panic!("expected 2/^/x all present:\n{block}");
        };
        assert!(
            two < hat && hat < x,
            "want 2 above ^ above x (rows {two}/{hat}/{x}):\n{block}"
        );
        for l in &lines {
            assert!(
                !(l.contains('^') && l.contains('2')),
                "accent and exponent collide on one row {l:?}:\n{block}"
            );
        }
        let (sub, ok) = render_2d(r"\bar{x}_i", 80);
        assert!(
            ok && sub.contains('─') && sub.contains('i'),
            "\\bar{{x}}_i should render a bar over x with i below:\n{sub}"
        );
    }
}
