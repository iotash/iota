//! LaTeX math rendering for the terminal (internal/mathtext; T3 design D1): the inline Unicode
//! approximation (`approx_inline`), the two-dimensional block layout (`render_2d`) and the
//! delimiter scanners the markdown renderer shares. Go's `Box` is `pict::Pict` here.
//!
//! Module map (one file per Go file): `delim` (delim.go), `parse` (parse.go), `symbols`
//! (symbols.go), `macros` (macros.go), `inline` (inline.go), `pict` (box.go), `layout`
//! (layout.go); this file is render.go. The markdown hooks call [`approx_inline`] and
//! [`render_2d`] directly (WP61 inline, WP62 display — DESIGN D16); nothing here names
//! `markdown` back, so this module is a leaf over `text`.
//!
//! Two hard rules hold everywhere below: NO combining mark (U+0300..=U+036F) is ever emitted —
//! bars, vinculums, accents and tall delimiters are DRAWN from spacing glyphs — and every width
//! goes through the one ruler, [`crate::text::width::str_width`].

pub mod delim;
pub(crate) mod inline;
pub(crate) mod layout;
pub(crate) mod macros;
pub(crate) mod parse;
pub(crate) mod pict;
pub(crate) mod symbols;

/// Parser recursion cap (parse.go:26 `maxParseDepth`): deeper input is `Unsupported`.
pub(crate) const MAX_PARSE_DEPTH: usize = 64;
/// Input cap in runes on the STRIPPED body (parse.go:30 `maxMathInputRunes`).
pub(crate) const MAX_MATH_INPUT_RUNES: usize = 4000;
/// Soft brace-recursion guard of the inline approximation (Rust hardening, DESIGN §2): past it the
/// rest of the input is copied with braces dropped and no further recursion.
pub(crate) const INLINE_MAX_DEPTH: usize = 256;

/// The inline Unicode approximation of `latex` (inline.go:26 `ApproxInline`): delimiters stripped,
/// macros mapped to symbols, scripts to super/subscript runes, one line, space runs collapsed.
///
/// A single physical line is a hard guarantee — stray newlines fold to spaces before the runs of
/// spaces macro handling can introduce are collapsed.
#[must_use]
pub fn approx_inline(latex: &str) -> String {
    let body = strip_delimiters(latex);
    let out = inline::approx(&body);
    let out = out.replace('\n', " ");
    inline::collapse_space_runs(&out).trim().to_owned()
}

/// The two-dimensional block of `latex` (render.go:25 `Render2D`; `width` is advisory and ignored).
/// Returns `(block, true)` on success, `(clean_source(latex), false)` when the input cannot be
/// parsed or the layout carries a combining mark (render.go:25-39).
///
/// A block wider than `width` is STILL returned with `ok = true`: best-effort overflow beats
/// dropping to linear source.
#[must_use]
pub fn render_2d(latex: &str, width: usize) -> (String, bool) {
    let _ = width; // advisory; overflow is best-effort (render.go:26)
    let Ok(node) = parse::parse(latex) else {
        return (clean_source(latex), false);
    };
    let out = layout::layout(&node).render();
    if has_combining_mark(&out) {
        // Unreachable by construction — the layout is combining-mark-free — but never leak one.
        return (clean_source(latex), false);
    }
    (out, true)
}

/// The readable fallback of an unparseable formula (delim.go:245 `CleanSource`) — the same
/// transform as [`approx_inline`], so the display fallback and inline math share one linear
/// renderer and an unrenderable formula still shows `√(b²-4ac)`, never `\sqrt{b^2-4ac}`.
#[must_use]
pub fn clean_source(latex: &str) -> String {
    approx_inline(latex)
}

/// Strips one layer of math delimiters (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) around `body`
/// (delim.go:216 `StripDelimiters`), trimming the result; text without a recognized pair comes
/// back trimmed and otherwise unchanged.
///
/// The affixes are ASCII, so the byte prefix/suffix tests and the byte slice are Go-exact.
#[must_use]
pub fn strip_delimiters(body: &str) -> String {
    let s = body.trim();
    if s.len() >= 4 && s.starts_with("$$") && s.ends_with("$$") {
        return s[2..s.len() - 2].trim().to_owned();
    }
    if s.len() >= 4 && s.starts_with("\\[") && s.ends_with("\\]") {
        return s[2..s.len() - 2].trim().to_owned();
    }
    if s.len() >= 4 && s.starts_with("\\(") && s.ends_with("\\)") {
        return s[2..s.len() - 2].trim().to_owned();
    }
    if s.len() >= 2 && s.starts_with('$') && s.ends_with('$') {
        return s[1..s.len() - 1].trim().to_owned();
    }
    s.to_owned()
}

/// Whether `s` carries a combining mark in U+0300..=U+036F (layout.go:784-791
/// `hasCombiningMark`) — the layout must never emit one, because terminals disagree on its width.
pub(crate) fn has_combining_mark(s: &str) -> bool {
    s.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::{clean_source, has_combining_mark, render_2d, strip_delimiters};

    #[test]
    fn combining_mark_range_is_u0300_to_u036f() {
        assert!(has_combining_mark("x\u{0302}"));
        assert!(has_combining_mark("a\u{0300}b"));
        assert!(has_combining_mark("z\u{036F}"));
        assert!(!has_combining_mark("plain ascii"));
        assert!(!has_combining_mark("中文 ∑ ∫"));
        assert!(!has_combining_mark("\u{0370}"));
    }

    // Go: internal/mathtext/render_test.go:37 TestRender2DFallback — the Err arm; the Ok arm and
    // the other seven render_test.go ports are the `render_ok` block below (WP62).
    #[test]
    fn render_2d_fallback() {
        for src in [
            r"\overbrace{x}",
            r"\begin{matrixx} a \end{matrixx}",
            r"\foobar",
        ] {
            let (block, ok) = render_2d(src, 80);
            assert!(!ok, "render_2d({src:?}) ok = true, want the fallback");
            assert_eq!(block, clean_source(src), "render_2d({src:?}) fallback");
        }
    }

    // Go: internal/mathtext/render_test.go:166 TestRender2DOverlongFallsBack
    #[test]
    fn render_2d_overlong_falls_back() {
        let huge = "a+b+".repeat(2000) + "c"; // ~8000 runes, over the cap
        let (out, ok) = render_2d(&huge, 80);
        assert!(!ok, "an overlong formula must fall back");
        assert!(
            out.contains("a+b+"),
            "the fallback should show cleaned source, got {} chars",
            out.len()
        );
    }

    // Go: internal/mathtext/delim.go:216-229 StripDelimiters — the byte-length guards.
    #[test]
    fn strip_delimiters_guards() {
        // "$$" is too short for the $$ pair, so the SINGLE-dollar arm strips it to "".
        assert_eq!(strip_delimiters("$$"), "");
        assert_eq!(strip_delimiters("$x$"), "x");
        assert_eq!(strip_delimiters("  $ w $  "), "w");
        assert_eq!(strip_delimiters("$"), "$");
        assert_eq!(strip_delimiters(r"\(a\)"), "a");
    }
}

/// The `Render2D` Ok-arm ports of `render_test.go` (WP62 hand-off): everything that needs the 2D
/// layout, kept in its own block so the WP61 fallback tests above stay untouched.
#[cfg(test)]
mod render_ok {
    use super::{has_combining_mark, render_2d};

    // Go: internal/mathtext/render_test.go:10 TestRender2DSuccess
    #[test]
    fn render_2d_success() {
        let (block, ok) = render_2d(r"\frac{a}{b}", 80);
        assert!(ok, "render_2d ok=false for a valid fraction");
        assert_eq!(block, "a\n─\nb");
    }

    // Go: internal/mathtext/render_test.go:24 TestRender2DStripsDelimiters — the display fence is
    // stripped before parsing, so "$$…$$" renders exactly as the bare body.
    #[test]
    fn render_2d_strips_delimiters() {
        let (bare, ok1) = render_2d("x^2", 80);
        let (fenced, ok2) = render_2d("$$x^2$$", 80);
        assert!(ok1 && ok2, "render_2d ok = {ok1}/{ok2}, want true/true");
        assert_eq!(fenced, bare, "fenced != bare");
    }

    // Go: internal/mathtext/render_test.go:55 TestRender2DNoCombiningMarks
    #[test]
    fn render_2d_no_combining_marks() {
        for src in [
            r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
            r"\sum_{i=1}^{n} \frac{1}{i^2}",
            r"\begin{pmatrix} \alpha & \beta \\ \gamma & \delta \end{pmatrix}",
            r"\sqrt[3]{\frac{x}{y}}",
        ] {
            let (block, _) = render_2d(src, 80);
            assert!(
                !has_combining_mark(&block),
                "render_2d({src:?}) has a combining mark:\n{block}"
            );
        }
    }

    // Go: internal/mathtext/render_test.go:71 TestRender2DWideStillRenders — a formula wider than
    // the budget is STILL returned with ok=true (best-effort overflow; `width` is advisory).
    #[test]
    fn render_2d_wide_still_renders() {
        let (block, ok) = render_2d(r"\frac{aaaaaaaaaa+bbbbbbbbbb}{c}", 5);
        assert!(ok, "render_2d ok=false for a wide-but-valid formula");
        assert!(block.contains('─'), "wide fraction lost its bar:\n{block}");
    }

    // Go: internal/mathtext/render_test.go:84 TestRender2DBigOpGlyphs — a big operator draws its
    // OWN glyph, never the family's: a union must not render as a summation.
    #[test]
    fn render_2d_big_op_glyphs() {
        for (src, want) in [
            (r"\bigcup_{i} A_i", '⋃'),
            (r"\bigcap_{i} A_i", '⋂'),
            (r"\bigoplus_{i} V_i", '⨁'),
            (r"\coprod_{i} X_i", '∐'),
            (r"\sum_{i} a_i", '∑'),
            (r"\prod_{i} a_i", '∏'),
        ] {
            let (out, ok) = render_2d(src, 80);
            assert!(ok, "render_2d({src:?}) fell back unexpectedly");
            assert!(
                out.contains(want),
                "render_2d({src:?}) missing {want:?}:\n{out}"
            );
            assert!(
                want == '∑' || !out.contains('∑'),
                "render_2d({src:?}) drew ∑ instead of {want:?}:\n{out}"
            );
        }
    }

    // Go: internal/mathtext/render_test.go:112 TestRender2DBattery — the Phase-3 real-world
    // failure battery: every formula must render 2D (ok=true) and be free of combining marks.
    #[test]
    fn render_2d_battery() {
        let battery: [(&str, &str, &[char]); 8] = [
            (
                "fourier_hat",
                r"\hat{f}(\xi) = \int_{-\infty}^{\infty} f(x) \, e^{-2\pi i x \xi} \, dx",
                &['^', '∫', 'ξ'],
            ),
            (
                "gaussian_exp",
                r"f(x \mid \mu, \sigma^2) = \frac{1}{\sigma\sqrt{2\pi}} \exp\left(-\frac{(x - \mu)^2}{2\sigma^2}\right)",
                &['∣', '╲', '─'],
            ),
            (
                "navier_stokes",
                r"\rho \left( \frac{\partial v}{\partial t} + v \cdot \nabla v \right) = -\nabla p + \mu \nabla^2 v + f",
                &['ρ', '∂', '∇'],
            ),
            (
                "maxwell_aligned",
                r"\begin{aligned} \nabla \cdot \mathbf{E} &= \frac{\rho}{\varepsilon_0} \\ \nabla \cdot \mathbf{B} &= 0 \end{aligned}",
                &['∇', 'ε', '─'],
            ),
            (
                "derivative_lim",
                r"\lim_{h \to 0} \frac{f(x+h) - f(x)}{h} = f'(x)",
                &['l', '→', '─'],
            ),
            ("ftc_int", r"\int f(x)dx = F(b) - F(a)", &['∫']),
            (
                "set_intersection",
                r"A \cap B = \{ x \mid x \in A \text{ and } x \in B \}",
                &['∩', '{', '∣', '}'],
            ),
            (
                "powerset_mathcal",
                r"\mathcal{P}(S) = \{ T \mid T \subseteq S \}",
                &['𝒫', '{', '∣', '⊆', '}'],
            ),
        ];
        for (name, src, must) in battery {
            let (block, ok) = render_2d(src, 80);
            assert!(ok, "{name}: render_2d fell back (ok=false):\n{block}");
            assert!(
                !has_combining_mark(&block),
                "{name}: rendered block has a combining mark:\n{block}"
            );
            for r in must {
                assert!(
                    block.contains(*r),
                    "{name}: rendered block missing {r:?}:\n{block}"
                );
            }
        }
    }

    // Go: internal/mathtext/render_test.go:146 TestRender2DNoCombiningMarkBattery — the full
    // probe corpus, greped rune by rune for U+0300..=U+036F.
    #[test]
    fn render_2d_no_combining_mark_battery() {
        for src in [
            r"\hat{f}",
            r"\bar{x}",
            r"\vec{v}",
            r"\tilde{a}",
            r"\dot{x}",
            r"\ddot{x}",
            r"\overline{x + y}",
            r"\mathbb{R}",
            r"\mathcal{L}",
            r"\mathfrak{g}",
            r"\begin{aligned} a &= b \\ c &= d \end{aligned}",
            r"\{ x \mid x \in A \}",
            r"\| v \|",
            r"\exp(x)",
            r"\hat{f}(\xi) = \int_{-\infty}^{\infty} f(x) e^{-2\pi i x \xi} dx",
        ] {
            let (block, _) = render_2d(src, 80);
            for c in block.chars() {
                let u = c as u32;
                assert!(
                    !(0x0300..=0x036F).contains(&u),
                    "render_2d({src:?}) emitted combining mark U+{u:04X}:\n{block}"
                );
            }
        }
    }
}
