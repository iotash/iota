//! Quote suite (`markdown_test.go`:594-673,899-966,1109-1130): continuous bar, text
//! never faint, mutual flush, no-color bar, recursive interior blocks, soft-wrap, and
//! display math inside a blockquote (at Go's own 2D golden since WP62 flipped the display hook).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{
    assert_lines, render_md, render_md_opts, render_md_raw, sgr_params, strip_ansi, trimmed_lines,
};

// Go: internal/markdown/markdown_test.go:594 — a multi-line quote renders as one
// block with a continuous left bar: one │ per content row, and an empty ">" line
// becomes a bar-only interior blank row.
#[test]
fn test_blockquote_continuous_bar() {
    let out = render_md("> first line\n> second line\n>\n> last line\n");
    let lines = trimmed_lines(&out);
    assert_eq!(lines.len(), 4, "quote rows:\n{out}");
    assert_eq!(
        out.matches('│').count(),
        4,
        "border rune count (one per content row):\n{out}"
    );
    // The interior blank row (from ">") carries only the bar, no text.
    assert_eq!(
        lines[2].strip_prefix('│').unwrap_or(&lines[2]).trim(),
        "",
        "empty > line is not a blank interior row: {:?}",
        lines[2]
    );
    for (i, want) in ["first line", "second line", "", "last line"]
        .iter()
        .enumerate()
    {
        let got = lines[i].strip_prefix('│').unwrap_or(&lines[i]).trim();
        assert_eq!(&got, want, "row {i} text");
    }
}

// Go: internal/markdown/markdown_test.go:621 — quote text is normal foreground (no
// faint SGR 2) while inline markdown is preserved: bold keeps SGR 1, the bar carries
// the cyan accent (36), markers stay hidden.
#[test]
fn test_blockquote_text_not_faint() {
    let raw = render_md_raw("> a **bold** word and `code`\n");
    let params = sgr_params(&raw);
    assert!(
        !params.contains("2"),
        "quote text is faint (SGR 2 present), want normal foreground:\n{raw:?}"
    );
    assert!(
        params.contains("1"),
        "inline bold inside quote lost its SGR 1:\n{raw:?}"
    );
    assert!(
        params.contains("36"),
        "quote bar missing its accent color (36):\n{raw:?}"
    );
    let plain = strip_ansi(&raw);
    assert!(
        !plain.contains("**") && !plain.contains('`'),
        "inline markers leaked into quote:\n{plain}"
    );
    assert!(
        plain.contains("a bold word and code"),
        "quote text mangled:\n{plain}"
    );
}

// Go: internal/markdown/markdown_test.go:648 — a quote flushes on the first non-quote
// line with exactly one blank at the block→paragraph boundary.
#[test]
fn test_blockquote_interrupted_by_paragraph() {
    assert_lines(
        &render_md("> quoted\nplain paragraph\n"),
        &["│ quoted", "", "plain paragraph"],
    );
}

// Go: internal/markdown/markdown_test.go:660 — with color off a blockquote still
// shows the │ bar but emits zero escape bytes.
#[test]
fn test_blockquote_no_color_keeps_bar() {
    let raw = render_md_opts("> first\n> second\n", 80, false);
    assert!(
        !raw.contains('\x1b'),
        "NoColor blockquote contains escape codes:\n{raw:?}"
    );
    assert!(
        raw.contains('│'),
        "NoColor blockquote lost its bar:\n{raw:?}"
    );
    assert_eq!(
        raw.matches('│').count(),
        2,
        "NoColor blockquote bar count:\n{raw:?}"
    );
}

// Go: internal/markdown/markdown_test.go:901 — a single-line quote with no trailing
// newline still renders the │ bar, not the raw ">".
#[test]
fn test_blockquote_single_line_no_trailing_newline() {
    let plain = render_md("> just one line"); // note: no trailing newline
    assert!(!plain.contains('>'), "raw quote marker leaked:\n{plain:?}");
    assert!(
        plain.contains('│') && plain.contains("just one line"),
        "quote bar/text missing:\n{plain:?}"
    );
}

// Go: internal/markdown/markdown_test.go:914 — quote interiors are mini-documents:
// lists, headings, tables, and nested quotes render as their block forms, every row
// still fronted by the continuous │ bar (nested quotes yield "│ │").
#[test]
fn test_blockquote_recursive_blocks() {
    // List inside a quote → bullets, not raw "- ".
    let list = render_md("> - one\n> - two\n\n");
    assert!(
        !list.contains("- one") && list.contains('•'),
        "list not parsed inside quote:\n{list}"
    );
    // Heading inside a quote → markers hidden.
    let head = render_md("> ## Title\n> body\n\n");
    assert!(
        !head.contains("##"),
        "heading marker leaked inside quote:\n{head}"
    );
    // Table inside a quote → box drawing, not raw pipes.
    let tbl = render_md("> | a | b |\n> |---|---|\n> | 1 | 2 |\n\n");
    assert!(
        tbl.contains('┌') && tbl.contains('┼'),
        "table not parsed inside quote:\n{tbl}"
    );
    // Nested quote → two bar columns on the inner line.
    let nest = render_md("> outer\n> > inner\n\n");
    assert!(!nest.contains('>'), "raw > leaked in nested quote:\n{nest}");
    assert!(
        nest.contains("│ │"),
        "nested quote lacks a second bar:\n{nest}"
    );
    // Every rendered row is still fronted by the bar.
    for ln in tbl.trim_end_matches('\n').split('\n') {
        assert!(ln.starts_with('│'), "quote row without leading bar: {ln:?}");
    }
}

// Go: internal/markdown/markdown_test.go:949 — an overlong quote paragraph soft-wraps
// with │ on EVERY wrapped row while the in-quote table stays intact.
#[test]
fn test_blockquote_long_line_wraps() {
    let long = "word ".repeat(60); // ~300 cols, exceeds the 80-col default
    let out = render_md(&format!(
        "> {long}\n>\n> | a | b |\n> |---|---|\n> | 1 | 2 |\n\n"
    ));
    let joined = out.trim_end_matches('\n').to_owned();
    let rows: Vec<&str> = joined.split('\n').collect();
    assert!(
        rows.len() >= 4,
        "expected the long line to wrap into multiple rows:\n{joined}"
    );
    for (i, ln) in rows.iter().enumerate() {
        assert!(
            ln.starts_with('│'),
            "row {i} lost the bar: {ln:?}\nfull:\n{joined}"
        );
    }
    // Table survived intact inside the quote.
    assert!(
        joined.contains('┌') && joined.contains('┼') && joined.contains('┘'),
        "table mangled inside wrapped quote:\n{joined}"
    );
}

// Go: internal/markdown/markdown_test.go:1111 TestDisplayMathInBlockquote — a $$ block inside a
// blockquote fits inside the bar at the reduced inner width, every rendered row keeps the │, and
// the 2D fraction (a over a drawn bar over b) survives the nesting.
#[test]
fn test_display_math_in_blockquote() {
    let out = render_md("> text\n> $$\n> \\frac{a}{b}\n> $$\n");
    let lines = trimmed_lines(&out);
    for l in &lines {
        if l.is_empty() {
            continue;
        }
        assert!(
            l.starts_with('│'),
            "quoted math row lost its bar: {l:?}\nfull:\n{out}"
        );
    }
    let joined = lines.join("\n");
    for frag in ["a", "─", "b"] {
        assert!(joined.contains(frag), "quoted math lost {frag:?}:\n{out}");
    }
}
