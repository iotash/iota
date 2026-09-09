//! The syntect highlight suite (`markdown_test.go`:244-268,1423-1466): the 256-color
//! code-block render, the foreground-only invariant the chat diff renderer's ± blocks
//! depend on, and the escape-framing laws the 2-space indent depends on. The plain
//! (no-color) shape is pinned by `tests/code.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{render_md_opts, render_md_raw, visible};
use iota::markdown::{CodeHighlighter, CodeTheme, SyntectHighlighter};

/// Both themes, so every invariant is asserted on the dark AND the light palette (Go ran
/// the neutralization test over `{"github", "monokai"}`).
const THEMES: [(&str, CodeTheme); 2] = [
    ("monokai", CodeTheme::Monokai),
    ("github", CodeTheme::Github),
];

/// The Go `TestHighlightNeutralizesErrorTokens` line: CJK prose with full-width
/// punctuation and markdown emphasis, fed to a JavaScript lexer. chroma marked every
/// unparseable run `Error` and painted it with an alarm BACKGROUND; syntect leaves it
/// scoped as source text — either way, no background byte may reach the terminal.
const ADVERSARIAL: &str = "重要: 这是一条发给**特定领域开发者**的推荐语（clarity、naturalness）";

// Go: internal/markdown/markdown_test.go:244 TestCodeBlockRender — the full assertion set
// now that the seam has a highlighter: fences hidden, every rendered line 2-space
// indented, content intact, AND the block is actually 256-color highlighted. `code.rs`
// keeps the T1 subset, which stays true either way.
#[test]
fn test_code_block_render() {
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
    assert!(
        raw.contains("\x1b[38;5;"),
        "code not syntax-highlighted:\n{raw:?}"
    );
}

// Go: internal/markdown/markdown_test.go:1450 TestHighlightNeutralizesErrorTokens —
// lexers mark what they cannot parse (CJK punctuation, prose inside a template literal)
// and most themes paint that with an alarm BACKGROUND. No background sequence may
// survive, under either theme, and the visible text must be untouched.
#[test]
fn test_highlight_neutralizes_error_tokens() {
    for (name, theme) in THEMES {
        let out = SyntectHighlighter.highlight(ADVERSARIAL, "JavaScript", theme);
        assert!(
            !out.contains("\x1b[48;"),
            "{name}: background sequence leaked:\n{out:?}"
        );
        assert_eq!(
            visible(&out),
            ADVERSARIAL,
            "{name}: content mangled:\n{out:?}"
        );
    }
}

// New (the foreground-only invariant, restated): the SAME adversarial inputs — CJK prose
// declared as JavaScript, a Chinese template literal, CJK box drawing in a Rust comment —
// carry no SGR 48 under EITHER theme, and their visible text survives byte for byte. The
// chat diff renderer paints its ± blocks around this output and re-arms them after every
// reset, so one background byte from here tears a hole in the block.
#[test]
fn highlighted_code_never_emits_a_background() {
    const SOURCES: [(&str, &str); 4] = [
        (ADVERSARIAL, "JavaScript"),
        ("const s = `重要：这是一段中文提示语，不是代码。`;", "js"),
        ("// 中文注释：┌───┐ │ A │ └───┘\nfn f() {}", "rust"),
        ("def f():\n    return 1", "python"),
    ];
    for (name, theme) in THEMES {
        for (src, lang) in SOURCES {
            let out = SyntectHighlighter.highlight(src, lang, theme);
            // SGR 48 in ANY spelling: the 256-color `48;5;`, the 24-bit `48;2;` syntect's
            // own formatter would emit, and a bare `48m`.
            assert!(
                !out.contains("\x1b[48"),
                "{name}/{lang}: background sequence leaked:\n{out:?}"
            );
            assert_eq!(visible(&out), src, "{name}/{lang}: content mangled");
        }
    }
    // And end to end through the code-block renderer (the harness writer is Monokai).
    let raw = render_md_opts(&format!("```js\n{ADVERSARIAL}\n```\n"), 80, true);
    assert!(
        !raw.contains("\x1b[48"),
        "background sequence leaked through the block renderer:\n{raw:?}"
    );
}

// New: an escape never spans a line break. `indent_code` prefixes EVERY rendered line
// (blank ones included) with two plain spaces, so a run left open across the newline
// would paint the next line's indent — and the diff renderer frames per line too.
#[test]
fn escapes_never_span_a_line_break() {
    let out = SyntectHighlighter.highlight(
        "def f():\n    s = \"\"\"\n    多行文本\n    \"\"\"\n    return s\n",
        "python",
        CodeTheme::Monokai,
    );
    assert!(!out.contains("\x1b[48;"), "background leaked:\n{out:?}");
    for line in out.split('\n') {
        let opens = line.matches("\x1b[").count();
        let resets = line.matches("\x1b[0m").count();
        assert_eq!(
            opens,
            resets * 2,
            "unbalanced SGR run on a line (every run is one open + one reset): {line:?}"
        );
    }
    // The rendered block keeps its plain indent on every line, colored or not.
    let block = render_md_raw("```python\ndef f():\n\n    return 1\n```\n");
    for line in visible(&block).trim_end_matches('\n').split('\n') {
        assert!(line.starts_with("  "), "indent lost: {line:?}");
    }
}

// New (Go's plaintext-lexer trap, generalized): a run the grammar gives no scope of its
// own stays UNCOLORED. chroma painted plain runs in the style's Text color — monokai's is
// near-white, invisible on a light terminal a background misdetection left it on. That is
// the same bet `test_bare_fence_uses_default_foreground` refuses; here it is refused for
// every plain-scope run inside a highlighted block too.
#[test]
fn plain_scope_runs_carry_no_color() {
    for (name, theme) in THEMES {
        for lang in ["txt", "text", "Plain Text"] {
            let out = SyntectHighlighter.highlight("hello world\n", lang, theme);
            assert_eq!(
                out, "hello world\n",
                "{name}/{lang}: a plain-scope run must carry no escapes"
            );
        }
    }
}

// New: an unknown language token renders VERBATIM rather than through a fallback lexer
// that would paint the whole block in the theme's Text color (the `lexers.Fallback`
// divergence, DEVIATIONS3 `[WP55]`).
#[test]
fn unknown_language_renders_verbatim() {
    for (name, theme) in THEMES {
        for lang in ["", "   ", "definitely-not-a-language"] {
            let out = SyntectHighlighter.highlight("┌───┐\n│ A │\n", lang, theme);
            assert_eq!(
                out, "┌───┐\n│ A │\n",
                "{name}/{lang:?}: unknown language must pass through untouched"
            );
        }
    }
}

// New (the shared-seam law, T-09): `highlight::active()` — the export the chat diff
// renderer calls — IS the syntect implementation, byte for byte.
// Go exported `markdown.Highlight` for exactly this reason: one pipeline, two callers.
#[test]
fn the_shared_seam_is_the_syntect_implementation() {
    let direct = SyntectHighlighter.highlight("let x = 1;", "rs", CodeTheme::Monokai);
    let seam =
        iota::markdown::highlight::active().highlight("let x = 1;", "rs", CodeTheme::Monokai);
    assert_eq!(seam, direct, "the diff renderer must share the pipeline");
    assert!(seam.contains("\x1b[38;5;"), "the seam is not highlighting");
    // Indent-free, exactly like Go's exported `Highlight`: the code-block renderer adds
    // the two spaces, the diff path must NOT inherit them.
    assert!(
        !visible(&seam).starts_with(' '),
        "the seam must not indent:\n{seam:?}"
    );
}

// New: the two themes really are two themes — the same source highlights differently —
// and neither reaches for the terminal-themable first 16 palette slots.
#[test]
fn themes_differ_and_stay_out_of_the_system_colors() {
    let src = "fn main() { let x = 1; }";
    let dark = SyntectHighlighter.highlight(src, "rs", CodeTheme::Monokai);
    let light = SyntectHighlighter.highlight(src, "rs", CodeTheme::Github);
    assert_ne!(dark, light, "monokai and github rendered identically");
    for out in [&dark, &light] {
        for chunk in out.split("38;5;").skip(1) {
            let digits: String = chunk.chars().take_while(char::is_ascii_digit).collect();
            let idx: u16 = digits.parse().expect("256-color index");
            assert!(idx >= 16, "index {idx} is a terminal-themable system color");
        }
    }
}

// New: the no-color switch still wins. `color: false` is the ONE gate over every escape
// byte in the crate, highlighter or not.
#[test]
fn no_color_renders_stay_escape_free() {
    let out = render_md_opts("```python\ndef f():\n    return 1\n```\n", 80, false);
    assert!(
        !out.contains('\x1b'),
        "escape emitted under color=false:\n{out:?}"
    );
}
