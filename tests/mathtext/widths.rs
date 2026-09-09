//! The ruler gate (WP61): every glyph of `glyph-widths.txt` measures 1 under
//! `iota::text::width::str_width`, `中` measures 2.
//!
//! The fixture is `uniseg.StringWidth` of every glyph the Go engine draws or maps. The layout's
//! equal-row-width invariant is only as true as the agreement between the two rulers, so if a row
//! here fails the fix belongs in the ruler crate pin — never in the layout (spec §9.4).

use iota::text::width::str_width;

/// One `<glyph> U+XXXX w=N` row.
struct Row {
    /// 1-based line number.
    line: usize,
    /// The glyph.
    glyph: String,
    /// The code point as spelled in the fixture.
    code: String,
    /// The width Go's uniseg reported.
    want: usize,
}

/// Parses `glyph-widths.txt` (three space-separated fields per row).
fn rows() -> Vec<Row> {
    let body = crate::fixture("glyph-widths.txt");
    body.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let mut f = l.split(' ');
            let glyph = f
                .next()
                .unwrap_or_else(|| panic!("line {}: no glyph", i + 1));
            let code = f
                .next()
                .unwrap_or_else(|| panic!("line {}: no code point", i + 1));
            let w = f
                .next()
                .and_then(|s| s.strip_prefix("w="))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("line {}: no w= field", i + 1));
            Row {
                line: i + 1,
                glyph: glyph.to_owned(),
                code: code.to_owned(),
                want: w,
            }
        })
        .collect()
}

/// Every glyph measures exactly what Go's uniseg measured.
#[test]
fn glyph_widths_match_uniseg() {
    let rows = rows();
    assert!(
        rows.len() >= 148,
        "glyph-widths.txt shrank to {} rows",
        rows.len()
    );
    for r in &rows {
        assert_eq!(
            str_width(&r.glyph),
            r.want,
            "glyph-widths.txt:{}: str_width({} {}) disagrees with uniseg",
            r.line,
            r.glyph,
            r.code
        );
    }
}

/// The two anchors the whole layout hangs on: every DRAWN glyph is one column, and a CJK atom is
/// two (spec §4.12 / `T3_TEST_PLAN` §1).
#[test]
fn drawn_glyphs_are_one_column_and_cjk_is_two() {
    for g in [
        "⎛", "⎜", "⎝", "⎞", "⎟", "⎠", "⎡", "⎢", "⎣", "⎤", "⎥", "⎦", "⎧", "⎨", "⎩", "⎪", "⎫", "⎬",
        "⎭", "│", "─", "╱", "╲", "⌠", "⎮", "⌡", "∑", "∏", "∫", "√", "‖", "·", "→", "~", "^",
    ] {
        assert_eq!(str_width(g), 1, "drawn glyph {g} is not one column");
    }
    assert_eq!(str_width("中"), 2);
    assert_eq!(str_width("中文"), 4);
    // The rows the fixture reports as wide are exactly the CJK ones.
    for r in rows() {
        assert!(
            r.want == 1 || r.glyph == "中",
            "glyph-widths.txt:{}: unexpected wide glyph {}",
            r.line,
            r.glyph
        );
    }
}
