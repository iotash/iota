//! `render_diff` goldens (`chat/compose_test.go` `RenderDiff*` ports) + the `udiff`-twin
//! differ goldens (the T-35 producer's diff shape).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::repl::render_diff;
use iota::text::ansi::{ansi_width, strip_sgr};
use pretty_assertions::assert_eq;

/// The dark-background shades (chat/diff.go:98-107; the default — WP50 flips light).
const BG_ADD: &str = "\x1b[48;5;22m";
const BG_DEL: &str = "\x1b[48;5;52m";
const FG_ADD: &str = "\x1b[38;5;114m";
const FG_DEL: &str = "\x1b[38;5;210m";

// Go: chat/compose_test.go:774 TestRenderDiffTruncatesWideRows — overwide diff rows
// TRUNCATE to the screen width; wrapping would wreck the column alignment diffs live by.
// (The Go test ran under NoColor; the plain form is the explicit `color: false` here.)
#[test]
fn test_render_diff_truncates_wide_rows() {
    let body = format!("+{}", "x".repeat(200));
    let rows = render_diff("", &body, 24, 40, false, true);
    assert_eq!(rows.len(), 1);
    let w = ansi_width(&rows[0]);
    assert!(w <= 39, "row width = {w}, must stay under the screen width");
}

// Go: chat/compose_test.go:826 TestRenderDiffBackgrounds — with color on, ± rows carry
// their background blocks, end SGR-self-contained, and a fresh-file "-0,0" hunk numbers
// from 1.
#[test]
fn test_render_diff_backgrounds() {
    let rows = render_diff(
        "main.go",
        "@@ -0,0 +1,2 @@\n+package main\n+var x = 1",
        24,
        100,
        true,
        true,
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (i, row) in rows.iter().enumerate() {
        assert!(
            row.contains(BG_ADD),
            "row {i} missing the addition background: {row:?}"
        );
        assert!(
            row.ends_with("\x1b[0m"),
            "row {i} must end SGR-self-contained: {row:?}"
        );
    }
    assert!(
        strip_sgr(&rows[0]).contains("1 + package main"),
        "gutter numbering wrong: {:?}",
        strip_sgr(&rows[0])
    );
    assert!(
        strip_sgr(&rows[1]).contains("2 + var x = 1"),
        "gutter numbering wrong: {:?}",
        strip_sgr(&rows[1])
    );
    // Any interior reset must re-arm the background so token styling can't cut the
    // block short (vacuous in T1's plain rows; the WP55 highlighted upgrade rides it).
    for row in &rows {
        let inner = row.strip_suffix("\x1b[0m").unwrap();
        if inner.contains("\x1b[0m") {
            assert!(
                inner.contains(&format!("\x1b[0m{BG_ADD}")),
                "token reset not re-armed with the background: {row:?}"
            );
        }
    }
}

// Go: chat/compose_test.go:891 TestRenderDiffNoAlienBackgrounds — a diff row whose
// content a lexer cannot parse must carry ONLY the block's own background: no alien
// `\x1b[48;` survives.
#[test]
fn test_render_diff_no_alien_backgrounds() {
    let rows = render_diff(
        "prompt.js",
        "@@ -1,1 +1,1 @@\n+  重要: 这是发给**开发者**的推荐语（clarity、naturalness）",
        24,
        200,
        true,
        true,
    );
    let stripped = rows[0].replace(BG_ADD, "");
    assert!(
        !stripped.contains("\x1b[48;"),
        "alien background survived in diff row:\n{:?}",
        rows[0]
    );
}

// Go: chat/compose_test.go:910 TestRenderDiffGutterInsideBlock — the ± block covers the
// line-number gutter: the row starts with the block background right after the indent,
// and the number + marker wear the row's accent color before the code's own foregrounds
// take over.
#[test]
fn test_render_diff_gutter_inside_block() {
    let rows = render_diff(
        "main.go",
        "@@ -1,1 +1,2 @@\n+package main\n-package old",
        24,
        100,
        true,
        true,
    );
    assert!(
        rows[0].starts_with(&format!("  {BG_ADD}{FG_ADD}")),
        "add row must open with block bg + accent fg over the gutter:\n{:?}",
        rows[0]
    );
    assert!(
        rows[1].starts_with(&format!("  {BG_DEL}{FG_DEL}")),
        "del row must open with block bg + accent fg over the gutter:\n{:?}",
        rows[1]
    );
    // The accent yields to the code's own foregrounds after the marker.
    assert!(
        rows[0].contains("\x1b[39m"),
        "accent fg must reset before the code:\n{:?}",
        rows[0]
    );
    assert!(
        strip_sgr(&rows[0]).contains("1 + package main"),
        "gutter layout changed: {:?}",
        strip_sgr(&rows[0])
    );
}

// The hunk gap row and the budget tail (chat/diff.go:163,196 byte pins).
#[test]
fn test_render_diff_gap_and_tail() {
    let body = "@@ -3,1 +3,1 @@\n-a\n+A\n@@ -20,1 +20,1 @@\n-b\n+B";
    let rows = render_diff("", body, 24, 80, false, true);
    // Rows: -a, +A, gap, -b, +B — the gap renders as the dim "⋮" marker row.
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert_eq!(rows[2], format!("  \x1b[2m{} ⋮\x1b[0m", " ".repeat(2)));

    let mut long = String::from("@@ -0,0 +1,10 @@");
    for i in 0..10 {
        use std::fmt::Write as _;
        let _ = write!(long, "\n+row-{i}");
    }
    let rows = render_diff("", &long, 5, 80, false, true);
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert_eq!(*rows.last().unwrap(), "\x1b[2m  … +6 more lines\x1b[0m");
}

// The udiff-twin differ goldens (go-udiff v0.4.1 byte shape; tool/code.go:661-675 feeds
// these rows into the artifact).
#[test]
fn test_udiff_twin_goldens() {
    use iota::tool::code::udiff::unified;

    // Equal inputs → the empty string (postDiff posts nothing).
    assert_eq!(unified("a.txt", "a.txt", "same\n", "same\n"), "");

    // A one-line replacement with context, the Go header/count shape.
    assert_eq!(
        unified("a.txt", "a.txt", "one\ntwo\nthree\n", "one\n2\nthree\n"),
        "--- a.txt\n+++ a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
    );

    // A fresh file diffs against empty content: the odd GNU "-0,0" form.
    assert_eq!(
        unified("n.txt", "n.txt", "", "alpha\nbeta\n"),
        "--- n.txt\n+++ n.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
    );

    // An unterminated final line carries the "\ No newline" marker.
    assert_eq!(
        unified("a", "a", "x\n", "x\ny"),
        "--- a\n+++ a\n@@ -1 +1,2 @@\n x\n+y\n\\ No newline at end of file\n"
    );
}
