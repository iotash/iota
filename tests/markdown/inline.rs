//! Inline renderer goldens driven through the public Writer
//! (`markdown_test.go`:429-471+ and the nesting/link/no-color suites).

use crate::harness::{render_md, render_md_opts, render_md_raw, sgr_params, visible};
use iota::markdown::Style;
use iota::markdown::hyperlink;

fn one_line(s: &str) -> String {
    s.trim_end_matches('\n').to_owned()
}

// Inline markers hidden while styling is
// still applied.
#[test]
fn inline_markers_are_hidden_while_the_styling_applies() {
    let cases = [
        ("**bold**", "bold"),
        ("__bold__", "bold"),
        ("*italic*", "italic"),
        ("_italic_", "italic"),
        ("`code`", "code"),
        ("a **b** and `c`", "a b and c"),
        ("see [docs](http://x) ok", "see docs (http://x) ok"),
        ("plain text", "plain text"),
    ];
    for (input, want) in cases {
        assert_eq!(one_line(&render_md(input)), want, "visible of {input:?}");
    }
    // The styling must still be applied (markers hidden, not just deleted).
    assert!(
        render_md_raw("**x**").contains("\x1b["),
        "bold span lost its styling"
    );
}

// H1 bold+underline, every other level
// plain bold (the reverted-faint decision), # markers hidden throughout.
#[test]
fn h1_is_bold_underlined_and_every_other_level_plain_bold() {
    let cases: [(&str, &str, &[&str], &[&str]); 4] = [
        ("# Top", "Top", &["1", "4"], &["2"]),
        ("## Second", "Second", &["1"], &["2", "4"]),
        ("### Third", "Third", &["1"], &["2", "4"]),
        ("##### Fifth", "Fifth", &["1"], &["2", "4"]),
    ];
    for (input, text, want, absent) in cases {
        let raw = render_md_raw(input);
        assert_eq!(one_line(&visible(&raw)), text, "visible of {input:?}");
        let params = sgr_params(&raw);
        for p in want {
            assert!(params.contains(*p), "{input:?} missing SGR {p}: {raw:?}");
        }
        for p in absent {
            assert!(
                !params.contains(*p),
                "{input:?} has unwanted SGR {p}: {raw:?}"
            );
        }
    }
}

// Exact bytes: rules dim-wrapped,
// including interior-space forms.
#[test]
fn a_horizontal_rule_is_dim_wrapped_byte_for_byte() {
    assert_eq!(render_md_raw("---"), "\x1b[2m---\x1b[0m\n");
    assert_eq!(render_md_raw("* * *"), "\x1b[2m* * *\x1b[0m\n");
}

// The exact 16-color SGR byte pins for
// simple spans; link params asserted as a set with the visible form.
#[test]
fn simple_spans_carry_the_exact_16_colour_sgr_bytes() {
    assert_eq!(render_md_raw("**b**"), "\x1b[1mb\x1b[0m\n");
    assert_eq!(render_md_raw("*i*"), "\x1b[3mi\x1b[0m\n");
    assert_eq!(render_md_raw("`c`"), "\x1b[36mc\x1b[0m\n");

    let raw = render_md_raw("[docs](http://x)");
    assert_eq!(one_line(&visible(&raw)), "docs (http://x)");
    let params = sgr_params(&raw);
    for p in ["36", "4", "2"] {
        assert!(params.contains(p), "link missing SGR {p}: {raw:?}");
    }
}

// Style-context composition: containers
// recurse with attributes composed, so an inner reset can never cut the outer style.
#[test]
fn nested_spans_compose_their_attributes_so_an_inner_reset_never_cuts_the_outer() {
    let bold = Style::default().bold();
    let bold_code = bold.fg(6);
    let bold_italic = bold.italic();

    let got = one_line(&render_md_raw("**a `c` b**"));
    let want = format!(
        "{}{}{}",
        bold.render("a ", true),
        bold_code.render("c", true),
        bold.render(" b", true)
    );
    assert_eq!(got, want, "bold∋code");
    // The tail after the inner span must STILL be bold (the reset-cut class).
    assert!(
        got.contains(&bold.render(" b", true)),
        "outer style lost: {got:?}"
    );

    assert_eq!(
        one_line(&render_md_raw("***x***")),
        bold_italic.render("x", true)
    );

    // Code spans stay leaves: their content is literal, never re-parsed.
    assert_eq!(one_line(&render_md("`*args*`")), "*args*");

    // Link text nests; the URL stays a dim leaf.
    let got = render_md_raw("[see `x`](http://u)");
    let link_code = Style::default().fg(6).underline().fg(6);
    assert!(
        got.contains(&link_code.render("x", true)),
        "link∋code = {got:?}, want code styled with the link underline"
    );
    assert!(
        visible(&got).contains("(http://u)"),
        "url lost: {:?}",
        visible(&got)
    );
}

// A heading
// whose text carries inline markers renders the words, never the markers themselves.
#[test]
fn a_heading_renders_its_words_never_its_inline_markers() {
    let plain = visible(&render_md("## **Bold** and `code` title\n\n"));
    assert!(
        !plain.contains("**") && !plain.contains('`'),
        "inline markers leaked into heading: {plain:?}"
    );
    assert!(
        plain.contains("Bold and code title"),
        "heading text mangled: {plain:?}"
    );
}

// Heading∋code composes heading bold +
// cyan instead of stripping.
#[test]
fn code_inside_a_heading_composes_bold_and_cyan() {
    let raw = render_md_raw("## Use `brew` now\n");
    assert!(
        visible(&raw).contains("Use brew now"),
        "heading text mangled: {:?}",
        visible(&raw)
    );
    let h2_code = Style::default().bold().fg(6);
    assert!(
        raw.contains(&h2_code.render("brew", true)),
        "heading code segment not composed (want bold+cyan): {raw:?}"
    );
}

// OSC 8 shape, zero-width for the
// ruler, control bytes stripped from the URL, NoColor bare passthrough.
#[test]
fn a_hyperlink_is_an_osc_8_of_zero_width_with_control_bytes_stripped() {
    let link = hyperlink("file:///tmp/a.png", "a.png", true);
    assert!(
        link.contains("\x1b]8;;file:///tmp/a.png") && link.contains("a.png"),
        "link = {link:?}"
    );
    assert_eq!(iota::text::ansi::ansi_width(&link), 5);

    let evil = hyperlink("file:///a\x1bZ;rm -rf", "x", true);
    assert!(
        evil.contains("]8;;file:///aZ;rm -rf"),
        "control bytes not stripped from the URL: {evil:?}"
    );

    assert_eq!(hyperlink("http://x", "plain", false), "plain");
}

// Markdown links carry the OSC 8
// wrapper around the styled text; visible text unchanged.
#[test]
fn a_markdown_link_wraps_its_styled_text_in_osc_8() {
    let got = render_md_raw("[docs](http://x)");
    assert!(
        got.contains("\x1b]8;;http://x"),
        "link not hyperlinked: {got:?}"
    );
    assert!(
        visible(&got).contains("docs (http://x)"),
        "visible text changed: {:?}",
        visible(&got)
    );
}

// The color gate: zero escape bytes
// across the whole path while markers are still hidden/replaced (layout intact).
#[test]
fn with_colour_off_no_escape_is_emitted_and_markers_stay_hidden() {
    let src = "# Title\n\n**bold**, *it*, `code` and [docs](http://x)\n\n> quoted\n\n---\n\n\
               - item one\n- item two\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
    let raw = render_md_opts(src, 80, false);
    assert!(
        !raw.contains('\x1b'),
        "NoColor output contains escapes:\n{raw:?}"
    );
    for want in [
        "Title",
        "bold, it, code",
        "docs (http://x)",
        "│ quoted",
        "---",
        "• item one",
        "┌",
    ] {
        assert!(
            raw.contains(want),
            "NoColor output missing {want:?}:\n{raw}"
        );
    }
    for bad in ["# Title", "**bold**", "`code`", "> quoted", "- item"] {
        assert!(
            !raw.contains(bad),
            "NoColor output still shows markup {bad:?}:\n{raw}"
        );
    }
}
