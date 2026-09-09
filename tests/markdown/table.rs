//! Table suite (`markdown_test.go`:97-146,194-242,520-563,1002-1019,1389-1421) incl.
//! `TestTableAlignsEmojiAndCJK` — the width gate: every rendered line of a mixed
//! emoji/VS16/CJK/ASCII table spans the same grapheme-cluster width.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{render_md, render_md_opts, render_md_raw, strip_ansi, visible};
use iota::text::width::str_width;

// Go: internal/markdown/markdown_test.go:97
#[test]
fn test_table_render() {
    let src = "| Name | Note |\n|------|------|\n| `--key` | the **secret** |\n| x | y |\n";
    let got = render_md(src);

    // Box-drawing borders are present.
    assert!(
        got.contains('┌') && got.contains('│'),
        "table missing box-drawing:\n{got}"
    );
    // Inline markers are hidden inside cells.
    for bad in ["`--key`", "**secret**", "|---"] {
        assert!(
            !got.contains(bad),
            "rendered table still shows markup {bad:?}:\n{got}"
        );
    }
    // Cell contents survive (markers stripped).
    for want in ["--key", "secret", "Name", "Note"] {
        assert!(
            got.contains(want),
            "rendered table missing {want:?}:\n{got}"
        );
    }
}

// Go: internal/markdown/markdown_test.go:127 — a stream ending mid-table folds the
// unterminated final row INSIDE the rendered box.
#[test]
fn test_table_flush_on_unterminated() {
    let src = "| Name | Code |\n|------|------|\n| Euro | EUR |\n| Aussie | AUD |";
    let got = render_md(src);

    assert!(
        got.contains("Aussie") && got.contains("AUD"),
        "unterminated final row missing from the table:\n{got}"
    );
    assert!(
        !got.contains("| Aussie"),
        "final row leaked as raw markdown below the table:\n{got}"
    );
    // The row landed INSIDE the box: no content after the closing border.
    if let Some(i) = got.rfind('└') {
        assert!(
            !got[i..].contains("AUD"),
            "final row rendered outside the closing border:\n{got}"
        );
    }
}

// Go: internal/markdown/markdown_test.go:194 — CRITICAL (the width-hazard gate):
// plain wide emoji, VS16 sequences (stripped to their base rune at the parse
// boundary), CJK, and ASCII in one table; every line — borders and cell rows alike —
// spans the same number of terminal columns under grapheme-cluster measurement, with
// a ├─┼─┤ rule between every pair of adjacent rows.
#[test]
fn test_table_aligns_emoji_and_cjk() {
    let src = concat!(
        "| Icon | Name | Note |\n",
        "|------|------|------|\n",
        "| \u{1F30D} | Earth | plain wide |\n",
        "| \u{1F32A}\u{FE0F} | Tornado | VS16 |\n",
        "| \u{2696}\u{FE0F} | Scales | VS16 |\n",
        "| \u{2708}\u{FE0F} | Plane | VS16 |\n",
        "| \u{1F3DB}\u{FE0F} | Museum | VS16 |\n",
        "| 中文 | 漢字テスト | CJK and ascii42 |\n"
    );
    let got = render_md(src);
    let got = got.trim_end_matches('\n');
    let lines: Vec<&str> = got.split('\n').collect();
    assert!(lines.len() >= 3, "table too short:\n{got}");

    // Every line must span the same number of terminal columns.
    let want = str_width(lines[0]);
    for ln in &lines {
        assert_eq!(str_width(ln), want, "line width mismatch: {ln:?}\n{got}");
    }

    // A rule row separates every pair of adjacent cell rows.
    let mut cell_rows = 0;
    let mut rule_rows = 0;
    for (i, ln) in lines.iter().enumerate() {
        if ln.starts_with('│') {
            cell_rows += 1;
            if let Some(next) = lines.get(i + 1) {
                assert!(
                    !next.starts_with('│'),
                    "missing rule between rows at line {i}:\n{got}"
                );
            }
        } else if ln.starts_with('├') {
            rule_rows += 1;
        }
    }
    assert_eq!(
        cell_rows, 7,
        "cell rows (header + 6 data, one line each):\n{got}"
    );
    assert_eq!(rule_rows, cell_rows - 1, "rule rows:\n{got}");
}

// Go: internal/markdown/markdown_test.go:1394 — a squeezed table renders at most
// width−1 columns wide (the exact-width row sits on the deferred-wrap boundary) and
// keeps its right border.
#[test]
fn test_table_never_exact_terminal_width() {
    let long = "wide content ".repeat(20);
    for tw in [40usize, 41, 80] {
        let raw = render_md_opts(&format!("| Col |\n|-----|\n| {long} |\n"), tw, true);
        let vis = visible(&raw);
        let mut max_w = 0;
        let mut right = false;
        for ln in vis.trim_end_matches('\n').split('\n') {
            let w = str_width(ln);
            if w > max_w {
                max_w = w;
            }
            let t = ln.trim_end_matches(' ');
            if t.ends_with('│') || t.ends_with('┐') || t.ends_with('┘') || t.ends_with('┤')
            {
                right = true;
            }
        }
        assert!(
            max_w < tw,
            "tw={tw}: table row reaches {max_w} columns — the exact-width boundary:\n{vis}"
        );
        assert!(right, "tw={tw}: no right border found:\n{vis}");
    }
}

// Go: internal/markdown/markdown_test.go:520 — a tab inside a table cell becomes a
// space at the parse boundary; the content after it survives.
#[test]
fn test_table_cell_tab_preserved() {
    let out = render_md("| H | K |\n|---|---|\n| a\tb | x |\n\n");
    assert!(out.contains("a b"), "tabbed cell content lost:\n{out}");
}

// Go: internal/markdown/markdown_test.go:530 — a table indented under a list item
// still renders as a bordered table (the list flushes first), and the item survives.
#[test]
fn test_indented_table_under_list_renders() {
    let out = render_md("- item\n  | x | y |\n  |---|---|\n  | 1 | 2 |\n\n");
    assert!(
        out.contains('┌') && out.contains('┼') && out.contains("│ 1"),
        "indented table not rendered as a table:\n{out}"
    );
    assert!(out.contains("• item"), "list item lost:\n{out}");
}

// Go: internal/markdown/markdown_test.go:544 — U+FE0F is stripped from table cells
// (terminals disagree on a VS16 sequence's cursor advance; only the bare base rune
// aligns everywhere).
#[test]
fn test_table_strips_variation_selectors() {
    let raw = render_md_raw("| C | N |\n|---|---|\n| \u{2696}\u{FE0F} 法庭 | x |\n\n");
    assert!(
        !raw.contains('\u{FE0F}'),
        "rendered table still contains U+FE0F"
    );
    let plain = strip_ansi(&raw);
    assert!(plain.contains("\u{2696} 法庭"), "base rune lost:\n{plain}");
}

// Go: internal/markdown/markdown_test.go:558 — flag emoji (regional-indicator pairs)
// pass through table cells untouched: they have no lossless narrow form, so the
// misalignment on disagreeing terminals is accepted.
#[test]
fn test_table_keeps_flags() {
    let out = render_md_raw("| C | N |\n|---|---|\n| \u{1F1EA}\u{1F1FA} 欧洲 | x |\n\n");
    assert!(
        out.contains('\u{1F1EA}') && out.contains('\u{1F1FA}'),
        "flag emoji lost from table cell:\n{}",
        strip_ansi(&out)
    );
}

// Go: internal/markdown/markdown_test.go:1002 TestInlineMathInTableCell — inline math inside a
// table cell renders on a SINGLE line (`ApproxInline` guarantees it) so the table stays aligned:
// no "$" leaks, the approximated glyphs are present, and every rendered line spans the same
// terminal width.
#[test]
fn test_inline_math_in_table_cell() {
    let out = render_md("| Sym | Val |\n|-----|-----|\n| $\\alpha$ | $x^2$ |\n| a | b |\n");
    assert!(
        !out.contains('$'),
        "dollar delimiter leaked into table cell:\n{out}"
    );
    assert!(
        out.contains('α') && out.contains("x²"),
        "inline math not approximated in table cell:\n{out}"
    );
    let trimmed = out.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.split('\n').collect();
    let want = str_width(lines[0]);
    for ln in &lines {
        assert_eq!(
            str_width(ln),
            want,
            "table row width mismatch (math broke alignment): {ln:?}\n{out}"
        );
    }
}
