//! List suite (`markdown_test.go`:272-368): nesting + hanging indent, ordered markers
//! as written, task glyphs, loose lists, flush laws, CJK width.

use crate::harness::{assert_lines, render_md, render_md_raw, trimmed_lines, visible};
use iota::text::width::str_width;

// Two-level nesting, the hanging indent
// of a continuation line, and inline styling inside items.
#[test]
fn a_nested_list_keeps_the_hanging_indent_and_inline_styling() {
    let src =
        "- parent one\n  wraps onto a second line\n  - child one\n  - `code` child\n- parent two\n";
    assert_lines(
        &render_md(src),
        &[
            "• parent one",
            "  wraps onto a second line",
            "  • child one",
            "  • code child",
            "• parent two",
        ],
    );
}

// Ordered items keep their numbers AS
// WRITTEN (no renumbering) and the enumerator is dimmed like unordered bullets.
#[test]
fn ordered_items_keep_their_numbers_as_written() {
    let raw = render_md_raw("3. third\n4. fourth\n5. fifth\n");
    assert_lines(&visible(&raw), &["3. third", "4. fourth", "5. fifth"]);
    assert!(
        raw.contains("\x1b[2m"),
        "ordered marker not dimmed:\n{raw:?}"
    );
}
#[test]
fn task_items_render_their_check_glyphs() {
    assert_lines(
        &render_md("- [ ] write tests\n- [x] ship it\n"),
        &["☐ write tests", "☑ ship it"],
    );
}

// A blank between items stays inside the
// block (loose list) while the blank that ends the list re-emits after it.
#[test]
fn a_loose_lists_interior_blank_stays_inside_the_block() {
    assert_lines(
        &render_md("- alpha\n\n- beta\n\nafter paragraph\n"),
        &["• alpha", "", "• beta", "", "after paragraph"],
    );
}

// A plain line flushes the block and the
// state machine inserts exactly one blank at the block→paragraph boundary even though
// the source had none.
#[test]
fn a_plain_line_flushes_the_list_with_exactly_one_blank() {
    assert_lines(
        &render_md("- one\n- two\nplain paragraph\n"),
        &["• one", "• two", "", "plain paragraph"],
    );
}

// Flush renders a list whose last line
// never saw a newline.
#[test]
fn flush_renders_a_list_whose_last_line_had_no_newline() {
    assert_lines(&render_md("- one\n- two"), &["• one", "• two"]);
}

// CJK item text keeps its display width
// after the bullet prefix: "• " (2) + four CJK runes (8) = 10 columns.
#[test]
fn cjk_item_text_keeps_its_display_width_after_the_bullet() {
    let got = trimmed_lines(&render_md("- 中文条目\n- ascii item\n"));
    assert_eq!(got[0], "• 中文条目");
    assert_eq!(str_width(&got[0]), 10, "line 0 width");
}
