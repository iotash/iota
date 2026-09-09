//! List suite (`markdown_test.go`:272-368): nesting + hanging indent, ordered markers
//! as written, task glyphs, loose lists, flush laws, CJK width.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{assert_lines, render_md, render_md_raw, trimmed_lines, visible};
use iota::text::width::str_width;

// Go: internal/markdown/markdown_test.go:272 — two-level nesting, the hanging indent
// of a continuation line, and inline styling inside items.
#[test]
fn test_list_nested_unordered_hanging_indent() {
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

// Go: internal/markdown/markdown_test.go:290 — ordered items keep their numbers AS
// WRITTEN (no renumbering) and the enumerator is dimmed like unordered bullets.
#[test]
fn test_list_ordered_start_offset() {
    let raw = render_md_raw("3. third\n4. fourth\n5. fifth\n");
    assert_lines(&visible(&raw), &["3. third", "4. fourth", "5. fifth"]);
    assert!(
        raw.contains("\x1b[2m"),
        "ordered marker not dimmed:\n{raw:?}"
    );
}

// Go: internal/markdown/markdown_test.go:308
#[test]
fn test_list_task_glyphs() {
    assert_lines(
        &render_md("- [ ] write tests\n- [x] ship it\n"),
        &["☐ write tests", "☑ ship it"],
    );
}

// Go: internal/markdown/markdown_test.go:320 — a blank between items stays inside the
// block (loose list) while the blank that ends the list re-emits after it.
#[test]
fn test_list_loose_keeps_blank() {
    assert_lines(
        &render_md("- alpha\n\n- beta\n\nafter paragraph\n"),
        &["• alpha", "", "• beta", "", "after paragraph"],
    );
}

// Go: internal/markdown/markdown_test.go:336 — a plain line flushes the block and the
// state machine inserts exactly one blank at the block→paragraph boundary even though
// the source had none.
#[test]
fn test_list_flushed_by_paragraph() {
    assert_lines(
        &render_md("- one\n- two\nplain paragraph\n"),
        &["• one", "• two", "", "plain paragraph"],
    );
}

// Go: internal/markdown/markdown_test.go:349 — Flush renders a list whose last line
// never saw a newline.
#[test]
fn test_list_flush_on_unterminated() {
    assert_lines(&render_md("- one\n- two"), &["• one", "• two"]);
}

// Go: internal/markdown/markdown_test.go:359 — CJK item text keeps its display width
// after the bullet prefix: "• " (2) + four CJK runes (8) = 10 columns.
#[test]
fn test_list_cjk_width() {
    let got = trimmed_lines(&render_md("- 中文条目\n- ascii item\n"));
    assert_eq!(got[0], "• 中文条目");
    assert_eq!(str_width(&got[0]), 10, "line 0 width");
}
