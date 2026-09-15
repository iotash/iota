//! Code-block suite (`markdown_test.go`:152-187,244-268,697-717,1433-1448): the
//! bare-fence law, the unified Flush dispatch, fence-interior blank preservation, and
//! the T1 plain render shape.

use crate::harness::{render_md, render_md_opts, render_md_raw, visible};

// A fence with no language carries NO
// color sequences even with the Monokai theme active: the terminal's default
// foreground is readable on any background.
#[test]
fn a_fence_without_a_language_uses_the_default_foreground() {
    let raw = render_md_raw("```\n┌───┐\n│ A │\n└───┘\n```\n");
    assert!(
        !raw.contains("\x1b[38;") && !raw.contains("\x1b[48;"),
        "bare fence must not carry color sequences:\n{raw:?}"
    );
    assert!(
        visible(&raw).contains("│ A │"),
        "content missing:\n{:?}",
        visible(&raw)
    );
}

// The unified Flush contract: the final
// partial line (no trailing newline) runs through the SAME dispatch as a terminated
// line, so every construct renders instead of leaking raw markdown. The one-line
// display-math sub-case renders as the 2D layout (T-08 closed in T3) — the no-leak
// assertion is unchanged.
#[test]
fn flush_renders_a_partial_final_line_as_its_construct() {
    // A final partial heading renders styled, not as raw "#" text.
    let got = render_md("body\n\n# Done");
    assert!(
        !got.contains("# Done") && got.contains("Done"),
        "partial heading leaked raw:\n{got}"
    );
    // A final partial list marker opens (and closes) a list: bullet rendered.
    let got = render_md("intro\n\n- item");
    assert!(
        got.contains("• item"),
        "partial list item not rendered as a bullet:\n{got}"
    );
    // A final partial one-line display formula renders as math, not raw "$$".
    let got = render_md("see:\n\n$$x^2$$");
    assert!(
        !got.contains("$$"),
        "partial display math leaked raw:\n{got}"
    );
    // A final partial closing fence ends the code block — no literal backticks.
    let got = render_md("```go\nfmt.Println(1)\n```");
    assert!(
        !got.contains("```"),
        "partial closing fence leaked into the code block:\n{got}"
    );
    // A final partial OPENING fence starts an empty block that closes clean.
    let got = render_md("text\n\n```go");
    assert!(
        !got.contains("```"),
        "partial opening fence leaked raw:\n{got}"
    );
    // A final partial quote line still renders with the quote bar.
    let got = render_md("> quoted");
    assert!(
        got.contains('│') && !got.contains('>'),
        "partial quote line not framed:\n{got}"
    );
}

// Blank lines inside a code fence are
// CONTENT and pass through verbatim: the blank-run collapse never reaches into a
// fence. (The spacing suite exercises the same law from the collapse side; this copy
// pins it from the fence side per the WP42 list.)
#[test]
fn blank_lines_inside_a_fence_are_content() {
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

// The T1 subset
// (T-09): fences hidden, every rendered line 2-space-indented, content intact. The
// 256-color highlight assertion (\x1b[38;5;) ships with the syntect impl in WP55.
#[test]
fn a_code_block_hides_its_fences_and_indents_every_line() {
    let raw = render_md_raw("```python\ndef f():\n    return 1\n```\n");
    assert!(!raw.contains("```"), "code fence not hidden:\n{raw:?}");
    let v = visible(&raw);
    let v = v.trim_end_matches('\n');
    for ln in v.split('\n') {
        assert!(
            ln.starts_with("  "),
            "code line not indented by two spaces: {ln:?}"
        );
    }
    assert!(
        v.contains("def f():") && v.contains("return 1"),
        "code content missing:\n{v:?}"
    );
}
