//! The display-math markdown twins (`markdown_test.go`:1076-1212) at GO'S OWN goldens: the
//! `Writer` now installs `mathtext::Mathtext`, so a `$$…$$` / `\[…\]` block renders as the 2D
//! layout and an unlayoutable formula degrades to the cleaned linear source (DESIGN D16 step 2 —
//! WP61 flipped the inline half, WP62 the display half). Four twins Go has and the T1 port
//! skipped (`:1137`, `:1151`, `:1169`, `:1212`) are added here.

use crate::harness::{
    assert_lines, render_md, render_md_chunked, render_md_opts, render_md_raw, sgr_params,
};

// A multi-line
// $$…$$ block renders as a 2D layout bounded by one blank line above and below (a block unit),
// with the fraction stacked over a drawn bar.
#[test]
fn a_multi_line_display_block_is_a_2d_layout_bounded_by_blanks() {
    assert_lines(
        &render_md("before\n$$\n\\frac{a}{b}\n$$\nafter\n"),
        &["before", "", "  a", "  ─", "  b", "", "after"],
    );
}

// The one-line "$$…$$"
// form renders the same 2D block as the multi-line fence form.
#[test]
fn the_one_line_display_form_renders_the_same_2d_block() {
    assert_lines(&render_md("$$\\frac{a}{b}$$\n"), &["  a", "  ─", "  b"]);
}

// The "\[ … \]" fence
// form is recognized and laid out (the exponent rides its own row above the base).
#[test]
fn the_bracket_fence_form_is_recognised_and_laid_out() {
    assert_lines(&render_md("\\[\nx^2\n\\]\n"), &["   2", "  x"]);
}

// A Tier-3
// construct falls back to the CLEANED linear source, which reads as math-ish text (the
// \overbrace decoration is unwrapped to its content) with no raw backslash-macro noise.
#[test]
fn an_unparseable_formula_falls_back_to_its_cleaned_linear_source() {
    let out = render_md("$$\n\\overbrace{x+y}\n$$\n");
    assert!(
        out.contains("x+y"),
        "unparseable math did not fall back to cleaned source:\n{out}"
    );
    assert!(
        !out.contains("\\overbrace") && !out.contains('\\'),
        "fallback leaked raw TeX to the terminal:\n{out}"
    );
}

// The
// richer fallback: an unsupported wrapper around a \frac and greek surfaces the approximated
// "a/b" and the glyph, never the raw "\frac"/"\alpha".
#[test]
fn the_linear_fallback_approximates_fractions_and_greek() {
    let out = render_md("$$\n\\overbrace{\\frac{a}{b} + \\alpha}\n$$\n");
    assert!(
        out.contains("a/b"),
        "fallback did not approximate \\frac to a/b:\n{out}"
    );
    assert!(
        out.contains('α'),
        "fallback did not map \\alpha to its glyph:\n{out}"
    );
    assert!(
        !out.contains("\\frac") && !out.contains("\\alpha") && !out.contains("\\overbrace"),
        "fallback leaked raw TeX macros:\n{out}"
    );
}

// The linear
// fallback for a formula the 2D engine cannot lay out (\binom has no vertical form) renders in
// NORMAL color: the approximation is still the reader's formula, and dim is for decoration.
#[test]
fn the_linear_fallback_renders_in_normal_colour() {
    let raw = render_md_raw("$$\n(a+b)^n = \\sum_{k=0}^{n} \\binom{n}{k} a^{n-k} b^k\n$$\n");
    assert!(
        !sgr_params(&raw).contains("2"),
        "display-math fallback is faint (SGR 2 present), want normal foreground:\n{raw:?}"
    );
    let plain = crate::harness::strip_ansi(&raw);
    assert!(
        plain.contains("C(n, k)"),
        "fallback did not approximate \\binom to C(n, k):\n{plain}"
    );
    assert!(
        !plain.contains("binom") && !plain.contains('\\'),
        "fallback leaked raw TeX:\n{plain}"
    );
}

// Streaming
// determinism: a 5-byte-chunk feed produces byte-identical output to one-shot Write, across
// split fence lines.
#[test]
fn a_chunked_feed_renders_byte_identical_to_one_shot() {
    let src = "intro\n$$\n\\frac{x+1}{y}\n$$\ndone\n";
    assert_eq!(
        render_md(src),
        render_md_chunked(src, 5),
        "streaming != one-shot"
    );
    // The property holds for a mixed document too (list + fence + math + table).
    let src = "para\n- a\n- b\n\n```go\nx := 1\n```\n$$\nE\n$$\n| a |\n|---|\n| 1 |\n";
    assert_eq!(
        render_md(src),
        render_md_chunked(src, 5),
        "mixed doc streaming != one-shot"
    );
}

// With color off a
// display-math block emits ZERO ANSI escape bytes and still draws its bar (the layout is
// glyph-based, so it needs no color at all).
#[test]
fn a_display_block_under_no_colour_emits_no_escape_and_still_draws_its_bar() {
    let raw = render_md_opts("$$\n\\frac{a}{b}\n$$\n", 80, false);
    assert!(
        !raw.contains('\x1b'),
        "NoColor display math emitted escapes: {raw:?}"
    );
    assert!(
        raw.contains('─'),
        "NoColor display math lost its bar: {raw:?}"
    );
}

// No rendered
// display block ever contains a Unicode combining mark (U+0300..=U+036F), the hard rule.
#[test]
fn a_display_block_never_contains_a_combining_mark() {
    for src in [
        "$$\\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}$$\n",
        "$$\n\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}\n$$\n",
        "$$\\sum_{i=1}^{n} \\frac{1}{i}$$\n",
    ] {
        let out = render_md(src);
        for c in out.chars() {
            let u = c as u32;
            assert!(
                !(0x0300..=0x036F).contains(&u),
                "display math emitted a combining mark U+{u:04X} for {src:?}:\n{out}"
            );
        }
    }
}

// Display math
// indented under a list item renders as a 2D block: the list flushes first, exactly as the
// indented-table and fenced-code cases do. Before the escape hatch existed, `list_consume`
// swallowed the fence and the formula as continuation text and the reader saw raw "$$" lines —
// the shape an LLM produces constantly ("3. the rigorous version:" then an indented formula).
#[test]
fn display_math_indented_under_a_list_item_renders() {
    let out = render_md(
        "1. first\n2. the rigorous version:\n   $$\n   \\frac{a}{b}\n   $$\n   after.\n\n",
    );
    assert!(
        !out.contains("$$"),
        "display fence leaked raw under a list item:\n{out}"
    );
    for frag in ["a", "─", "b"] {
        assert!(out.contains(frag), "indented math lost {frag:?}:\n{out}");
    }
    assert!(
        out.contains("first") && out.contains("the rigorous version:"),
        "list items lost:\n{out}"
    );
}

// The
// one-line "$$…$$" form escapes a list item too; the bug was never about the multi-line fence,
// it was about the list branch running first.
#[test]
fn one_line_display_math_under_a_list_item_renders() {
    let out = render_md("1. item\n   $$\\frac{a}{b}$$\n   after.\n\n");
    assert!(
        !out.contains("$$"),
        "one-line display math leaked raw under a list item:\n{out}"
    );
    for frag in ["a", "─", "b"] {
        assert!(
            out.contains(frag),
            "indented one-line math lost {frag:?}:\n{out}"
        );
    }
}
