//! Quote suite (`markdown_test.go`:594-673,899-966,1109-1130): continuous bar, text
//! never faint, mutual flush, no-color bar, recursive interior blocks, soft-wrap, and
//! display math inside a blockquote (at Go's own 2D golden since WP62 flipped the display hook).

use crate::harness::{
    assert_lines, render_md, render_md_opts, render_md_raw, sgr_params, strip_ansi, trimmed_lines,
};

// A multi-line quote renders as one
// block with a continuous left bar: one │ per content row, and an empty ">" line
// becomes a bar-only interior blank row.
#[test]
fn a_blockquote_renders_one_continuous_bar_with_bar_only_blank_rows() {
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

// Quote text is normal foreground (no
// faint SGR 2) while inline markdown is preserved: bold keeps SGR 1, the bar carries
// the cyan accent (36), markers stay hidden.
#[test]
fn quote_text_keeps_the_normal_foreground_and_its_inline_styling() {
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

// A quote flushes on the first non-quote
// line with exactly one blank at the block→paragraph boundary.
#[test]
fn a_quote_flushes_on_the_first_non_quote_line_with_one_blank() {
    assert_lines(
        &render_md("> quoted\nplain paragraph\n"),
        &["│ quoted", "", "plain paragraph"],
    );
}

// With color off a blockquote still
// shows the │ bar but emits zero escape bytes.
#[test]
fn a_quote_under_no_colour_keeps_its_bar_and_emits_no_escape() {
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

// A single-line quote with no trailing
// newline still renders the │ bar, not the raw ">".
#[test]
fn a_single_line_quote_without_a_newline_still_renders_its_bar() {
    let plain = render_md("> just one line"); // note: no trailing newline
    assert!(!plain.contains('>'), "raw quote marker leaked:\n{plain:?}");
    assert!(
        plain.contains('│') && plain.contains("just one line"),
        "quote bar/text missing:\n{plain:?}"
    );
}

// Quote interiors are mini-documents:
// lists, headings, tables, and nested quotes render as their block forms, every row
// still fronted by the continuous │ bar (nested quotes yield "│ │").
#[test]
fn quote_interiors_render_their_blocks_behind_the_bar() {
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

// An overlong quote paragraph soft-wraps
// with │ on EVERY wrapped row while the in-quote table stays intact.
#[test]
fn an_overlong_quote_paragraph_wraps_with_a_bar_on_every_row() {
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

// A $$ block inside a
// blockquote fits inside the bar at the reduced inner width, every rendered row keeps the │, and
// the 2D fraction (a over a drawn bar over b) survives the nesting.
#[test]
fn display_math_inside_a_quote_fits_the_bar_at_the_inner_width() {
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
