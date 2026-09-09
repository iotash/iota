//! The blank/spacing suite: blank-run collapse, the one-blank-per-boundary invariant,
//! and the preview-pays-separator law (`markdown_test.go`:677-897, 1514-1565).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{
    assert_lines, assert_preview_follows_blank, blanks_between, render_md, render_md_opts,
    render_trace, visible,
};

// Go: internal/markdown/markdown_test.go:677 — runs of blanks collapse to one;
// leading blanks are dropped; a single blank is preserved (collapse, not re-spacing).
#[test]
fn test_blank_run_collapse() {
    assert_lines(&render_md("a\n\n\n\nb\n"), &["a", "", "b"]);
    assert_lines(&render_md("\n\n\nfirst\n"), &["first"]);
    assert_lines(&render_md("a\n\nb\n"), &["a", "", "b"]);
}

// Go: internal/markdown/markdown_test.go:700 — blank lines INSIDE a code fence are
// content: both survive, the collapse never reaches into a fence.
#[test]
fn test_blank_run_collapse_code_fence_preserved() {
    let out = render_md_opts("```\nx\n\n\ny\n```\n", 80, false);
    let blanks = out
        .trim_end_matches('\n')
        .split('\n')
        .filter(|r| r.trim().is_empty())
        .count();
    assert_eq!(
        blanks, 2,
        "code-fence interior blank rows (not collapsed):\n{out:?}"
    );
}

// Go: internal/markdown/markdown_test.go:722 — a single blank between a list block
// and a following paragraph is preserved: not doubled, not removed.
#[test]
fn test_blank_between_block_and_paragraph() {
    assert_lines(
        &render_md("- one\n- two\n\nafter\n"),
        &["• one", "• two", "", "after"],
    );
}

// Go: internal/markdown/markdown_test.go:768 — a document with NO source blanks gets
// exactly one blank at every block boundary: none glued, none doubled.
#[test]
fn test_block_adjacency_no_source_blanks() {
    let src =
        "A para\n- item\n- item\n## Heading\nNext para\n| a | b |\n|---|---|\n| 1 | 2 |\nTail\n";
    let out = render_md(src);
    for (start, end) in [
        ("A para", "item"),
        ("item", "Heading"),
        ("Heading", "Next para"),
        ("Next para", "a"),
        ("2", "Tail"),
    ] {
        assert_eq!(
            blanks_between(&out, start, end),
            1,
            "blank lines between {start:?} and {end:?}:\n{out}"
        );
    }
}

// Go: internal/markdown/markdown_test.go:798 — three consecutive plain lines render
// adjacent: no blank inserted inside a paragraph.
#[test]
fn test_paragraph_integrity() {
    assert_lines(
        &render_md("line one\nline two\nline three\n"),
        &["line one", "line two", "line three"],
    );
}

// Go: internal/markdown/markdown_test.go:809 — a blank run before an unterminated
// tail still collapses to one.
#[test]
fn test_blank_collapse_still_works() {
    assert_lines(&render_md("a\n\n\nb"), &["a", "", "b"]);
}

// Go: internal/markdown/markdown_test.go:820 — a heading between paragraphs gets
// exactly one blank above and below, with no source blanks.
#[test]
fn test_heading_bounding() {
    assert_lines(
        &render_md("text\n## H\ntext\n"),
        &["text", "", "H", "", "text"],
    );
}

// Go: internal/markdown/markdown_test.go:833 — a horizontal rule between paragraphs
// gets exactly one blank above and below.
#[test]
fn test_horizontal_rule_bounding() {
    assert_lines(
        &render_md("text\n---\ntext\n"),
        &["text", "", "---", "", "text"],
    );
}

// Go: internal/markdown/markdown_test.go:847 — a document ending in a block has NO
// trailing blank (the closing blank is produced lazily; there is no next unit at EOF).
#[test]
fn test_no_dangling_trailing_blank() {
    let out = render_md("para\n| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert!(
        !out.ends_with("\n\n"),
        "document ending in a block has a dangling trailing blank:\n{out:?}"
    );
    assert_eq!(blanks_between(&out, "para", "a"), 1);
}

// Go: internal/markdown/markdown_test.go:862 — the adjacency document under no-color:
// zero escape bytes AND identical spacing.
#[test]
fn test_block_adjacency_no_color() {
    let src =
        "A para\n- item\n- item\n## Heading\nNext para\n| a | b |\n|---|---|\n| 1 | 2 |\nTail\n";
    let raw = render_md_opts(src, 80, false);
    assert!(
        !raw.contains('\x1b'),
        "NoColor adjacency output contains escapes:\n{raw:?}"
    );
    let rendered = visible(&raw);
    for (start, end) in [
        ("A para", "item"),
        ("item", "Heading"),
        ("Heading", "Next para"),
        ("Next para", "a"),
        ("2", "Tail"),
    ] {
        assert_eq!(
            blanks_between(&rendered, start, end),
            1,
            "NoColor blank lines between {start:?} and {end:?}:\n{rendered}"
        );
    }
}

// Go: internal/markdown/markdown_test.go:1514 — the preview-pays-separator law for
// all FIVE buffering block types: the event trace shows …LINE,BLANK,PREVIEW… (never
// doubled), and a source blank does not change the layout (idempotent).
#[test]
fn test_block_preview_pays_its_separator_on_open() {
    for (name, src) in [
        ("list", "Here is a list:\n- one\n- two\n"),
        (
            "table",
            "Here is a table:\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        ),
        ("code", "Here is code:\n```go\nx := 1\n```\n"),
        ("quote", "Here is a quote:\n> hi\n"),
        ("math", "Here is math:\n$$\nx = 1\n$$\n"),
    ] {
        // No blank in the source: the gap must appear anyway, before the preview.
        let glued = render_trace(src);
        assert_preview_follows_blank(&glued);

        // A source that DOES carry the blank must not end up with two: beginBlock
        // consumes the credit the preview already paid.
        let spaced = render_trace(&src.replacen(":\n", ":\n\n", 1));
        assert_preview_follows_blank(&spaced);
        assert_eq!(
            spaced.len(),
            glued.len(),
            "{name}: blank in the source changed the layout:\nglued  {glued:?}\nspaced {spaced:?}"
        );
    }
}
