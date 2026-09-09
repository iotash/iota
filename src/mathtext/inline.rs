//! The one-line approximation (internal/mathtext/inline.go): macros become symbols, scripts
//! become super/subscript runes where the alphabet allows, fractions become `a/b`, layout macros
//! are dropped; the result never contains a newline and never a combining mark.
//!
//! Everything here scans RUNES (Go `[]rune`). The one deliberate departure from Go is the soft
//! [`INLINE_MAX_DEPTH`] guard on brace recursion (DESIGN §2): Go recurses without a cap, and with
//! `panic = "abort"` a `{{{…}}}`×10⁴ model output would take the process down. Past the cap the
//! remaining input is copied with braces dropped. The deepest nesting in the goldens is 4, so no
//! golden moves.

use crate::mathtext::INLINE_MAX_DEPTH;
use crate::mathtext::macros::math_font_style;
use crate::mathtext::symbols::{apply_math_font, subscript_rune, superscript_rune, symbol_rune};

/// The approximation of `s` (inline.go:81 `approx`; the caller strips delimiters first).
pub(crate) fn approx(s: &str) -> String {
    approx_at(s, 0)
}

/// [`approx`] with the brace-recursion depth threaded through (Rust hardening; see the module
/// docs). At the cap the rest is copied verbatim with braces dropped and no further recursion.
fn approx_at(s: &str, depth: usize) -> String {
    if depth > INLINE_MAX_DEPTH {
        return s.chars().filter(|c| *c != '{' && *c != '}').collect();
    }
    let runes: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < runes.len() {
        match runes[i] {
            '\\' => i = emit_macro(&mut out, &runes, i, depth),
            '^' | '_' => i = emit_script(&mut out, &runes, i, depth),
            // Bare grouping braces carry no meaning once scripts/args have their own scanners.
            '{' | '}' => i += 1,
            '~' => {
                out.push(' '); // non-breaking space in LaTeX
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Collapses runs of two or more ASCII spaces into one (inline.go:41-60), leaving every other
/// character untouched.
pub(crate) fn collapse_space_runs(s: &str) -> String {
    if !s.contains("  ") {
        return s.to_owned();
    }
    let mut b = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        b.push(c);
    }
    b
}

/// The INLINE layout-macro set (inline.go:68-78) — distinct from
/// [`crate::mathtext::macros::is_layout_macro`]: `left`/`right` are droppable HERE but structural
/// in the parser, and the parser's `\,`-family control symbols are handled earlier here.
pub(crate) fn is_layout_macro_inline(name: &str) -> bool {
    matches!(
        name,
        // inline.go:69
        "left" | "right"
        // inline.go:70
        | "quad" | "qquad" | "enspace" | "thinspace"
        // inline.go:71-72
        | "displaystyle" | "textstyle" | "scriptstyle" | "scriptscriptstyle"
        // inline.go:73
        | "limits" | "nolimits" | "nonumber" | "notag"
        // inline.go:74-77
        | "bigl" | "bigr" | "Bigl" | "Bigr"
        | "biggl" | "biggr" | "Biggl" | "Biggr"
        | "big" | "Big" | "bigg" | "Bigg"
        | "bigm" | "Bigm" | "biggm" | "Biggm"
    )
}

/// A backslash sequence at `r[i]`; returns the index just past what it consumed (inline.go:110-201).
/// THE ORDER OF THE CASES IS THE CONTRACT (spec `mathtext.md` §5.4).
// The three `emit_text_arg` groups below share a body but NOT a reason — text/font, accent,
// annotation — and each carries its own inline.go line; merging them would lose that.
#[allow(clippy::match_same_arms)]
fn emit_macro(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    // Escaped punctuation and the spacing control symbols (inline.go:112-125).
    if let Some(&next) = r.get(i + 1) {
        match next {
            '$' | '%' | '&' | '#' | '_' | '{' | '}' | ' ' => {
                b.push(next);
                return i + 2;
            }
            '\\' => {
                b.push(' '); // a source line break folds to a space
                return i + 2;
            }
            ',' | ';' | ':' | '!' => return i + 2, // thin/med/neg spaces: drop
            _ => {}
        }
    }

    let (name, next, spaced) = scan_macro_name(r, i + 1);
    if name.is_empty() {
        return next; // a lone trailing backslash is dropped (inline.go:128-131)
    }

    match name.as_str() {
        // inline.go:134-135
        "frac" | "tfrac" | "dfrac" | "cfrac" => return emit_frac(b, r, next, depth),
        // inline.go:136-137
        "binom" | "dbinom" | "tbinom" => return emit_binom(b, r, next, depth),
        // inline.go:138-139
        "sqrt" => return emit_sqrt(b, r, next, depth),
        // \text{…} and the PLAIN math fonts unwrap to their inner content (inline.go:140-145).
        "text" | "textrm" | "textbf" | "textit" | "textnormal" | "textsf" | "texttt" | "mathrm"
        | "mathbf" | "mathit" | "mathsf" | "mathtt" | "mathnormal" | "boldsymbol" | "bm"
        | "operatorname" => return emit_text_arg(b, r, next, depth),
        // Accents keep only the base: an inline accent would need a combining mark, which is
        // forbidden (inline.go:146-152).
        "hat" | "bar" | "vec" | "tilde" | "dot" | "ddot" | "widehat" | "widetilde" | "overline"
        | "underline" | "acute" | "grave" | "check" | "breve" | "mathring" | "overrightarrow"
        | "overleftarrow" => return emit_text_arg(b, r, next, depth),
        // Annotation/box wrappers unwrap to the inner content; \overset/\underset's SECOND group
        // is then scanned by the main loop, so both survive (inline.go:153-160).
        "overbrace" | "underbrace" | "overbracket" | "underbracket" | "overset" | "underset"
        | "boxed" | "mbox" | "hbox" => return emit_text_arg(b, r, next, depth),
        // Invisible spacing: consume the argument, emit nothing (inline.go:161-165).
        "phantom" | "hphantom" | "vphantom" => return read_arg(r, next).1,
        _ => {}
    }

    // A styled math font with a real Unicode alphanumeric variant (inline.go:167-173); the plain
    // styles were caught above.
    if let Some(style) = math_font_style(&name) {
        let (arg, next) = read_arg(r, next);
        b.push_str(&apply_math_font(style, &approx_at(&arg, depth + 1)));
        return next;
    }

    // Layout-only macro: emit nothing. The swallowed trailing space is intentionally dropped —
    // these carry no glyph (inline.go:175-179).
    if is_layout_macro_inline(&name) {
        return next;
    }

    // A symbol from the greek/operator tables. LaTeX swallows the space after a control word, but
    // in linear text that space is the only thing separating "α" from a following "+" — so a
    // single space is restored (inline.go:181-191).
    if let Some(sym) = symbol_rune(&name) {
        b.push(sym);
        if spaced {
            b.push(' ');
        }
        return next;
    }

    // Unknown macro: best effort is the bare name, e.g. \foo → "foo" (inline.go:196-200).
    b.push_str(&name);
    if spaced {
        b.push(' ');
    }
    next
}

/// Reads a macro name just after the backslash at `i` (inline.go:207-228): a control word is a run
/// of ASCII letters (with its trailing ASCII SPACES swallowed), a control symbol is one non-letter.
fn scan_macro_name(r: &[char], i: usize) -> (String, usize, bool) {
    let Some(&first) = r.get(i) else {
        return (String::new(), i, false);
    };
    if !first.is_ascii_alphabetic() {
        // A control symbol; already-escaped punctuation was handled by the caller.
        return (first.to_string(), i + 1, false);
    }
    let mut j = i;
    while r.get(j).is_some_and(char::is_ascii_alphabetic) {
        j += 1;
    }
    let name: String = r[i..j].iter().collect();
    let mut spaced = false;
    while r.get(j) == Some(&' ') {
        j += 1;
        spaced = true;
    }
    (name, j, spaced)
}

/// A `^` or `_` script at `r[i]` (inline.go:235-251): the approximated argument becomes
/// super/subscript glyphs when EVERY character maps, else the marker plus the plain inner text.
fn emit_script(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    let kind = r[i]; // '^' or '_'
    let (arg, next) = scan_script_arg(r, i + 1);
    // Approximate first, so `x^{\alpha}` resolves to its glyph before the mapping is attempted.
    let inner = approx_at(&arg, depth + 1);
    if let Some(mapped) = map_script(&inner, kind) {
        b.push_str(&mapped);
        return next;
    }
    b.push(kind);
    b.push_str(&inner);
    next
}

/// Maps every rune of `s` to its super/subscript glyph; `None` if ANY rune lacks one, so the
/// caller uses the linear fallback (inline.go:255-274).
fn map_script(s: &str, kind: char) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut b = String::with_capacity(s.len());
    for c in s.chars() {
        let g = if kind == '^' {
            superscript_rune(c)
        } else {
            subscript_rune(c)
        }?;
        b.push(g);
    }
    Some(b)
}

/// A script's argument at `r[i]`: a braced group (without the braces), a whole `\macro` token, or
/// one rune (inline.go:279-294).
fn scan_script_arg(r: &[char], i: usize) -> (String, usize) {
    let Some(&c) = r.get(i) else {
        return (String::new(), i);
    };
    if c == '{' {
        return scan_group(r, i);
    }
    if c == '\\' {
        // Rebuild the macro token WITHOUT any swallowed trailing space.
        let (name, next, _spaced) = scan_macro_name(r, i + 1);
        return (format!("\\{name}"), next);
    }
    (c.to_string(), i + 1)
}

/// `\frac{num}{den}` → `num/den`, each side parenthesized unless it is a single visual atom
/// (inline.go:299-306).
fn emit_frac(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    let (num, i) = read_arg(r, i);
    let (den, i) = read_arg(r, i);
    b.push_str(&frac_part(&num, depth));
    b.push('/');
    b.push_str(&frac_part(&den, depth));
    i
}

/// One side of a fraction, wrapped in parentheses unless [`is_single_atom`] (inline.go:311-317).
fn frac_part(arg: &str, depth: usize) -> String {
    let s = approx_at(arg, depth + 1);
    if is_single_atom(&s) {
        s
    } else {
        format!("({s})")
    }
}

/// `\binom{n}{k}` → `C(n, k)` — the widely recognized linear form the 2D engine punts to
/// (inline.go:323-332).
fn emit_binom(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    let (top, i) = read_arg(r, i);
    let (bot, i) = read_arg(r, i);
    b.push_str("C(");
    b.push_str(&approx_at(&top, depth + 1));
    b.push_str(", ");
    b.push_str(&approx_at(&bot, depth + 1));
    b.push(')');
    i
}

/// `\sqrt{x}` → `√x`, `\sqrt{x+1}` → `√(x+1)`; an optional `[n]` index is skipped entirely
/// (inline.go:338-352).
fn emit_sqrt(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    let mut i = i;
    if r.get(i) == Some(&'[') {
        i = scan_bracket(r, i).1;
    }
    let (arg, i) = read_arg(r, i);
    let s = approx_at(&arg, depth + 1);
    b.push('√');
    if is_single_atom(&s) {
        b.push_str(&s);
    } else {
        b.push('(');
        b.push_str(&s);
        b.push(')');
    }
    i
}

/// `\text{…}`, plain fonts, accents and annotation wrappers: emit the inner content approximated
/// (inline.go:357-361).
fn emit_text_arg(b: &mut String, r: &[char], i: usize, depth: usize) -> usize {
    let (arg, i) = read_arg(r, i);
    b.push_str(&approx_at(&arg, depth + 1));
    i
}

/// The next macro argument at `r[i]`: a braced group (contents without braces), a `\macro` token,
/// or one rune. Leading ASCII spaces are skipped (inline.go:378-393).
fn read_arg(r: &[char], i: usize) -> (String, usize) {
    let mut i = i;
    while r.get(i) == Some(&' ') {
        i += 1;
    }
    let Some(&c) = r.get(i) else {
        return (String::new(), i);
    };
    if c == '{' {
        return scan_group(r, i);
    }
    if c == '\\' {
        let (name, next, _spaced) = scan_macro_name(r, i + 1);
        return (format!("\\{name}"), next);
    }
    (c.to_string(), i + 1)
}

/// A brace-balanced group starting at `r[i]` (`r[i] == '{'`): the inner contents and the index
/// past the matching close. A backslash skips the next rune so `\{` / `\}` do not affect the
/// depth; an unbalanced group runs to end of input (inline.go:398-415).
fn scan_group(r: &[char], i: usize) -> (String, usize) {
    let mut depth: i32 = 0;
    let start = i + 1;
    let mut j = i;
    while j < r.len() {
        match r[j] {
            '\\' => j += 2,
            '{' => {
                depth += 1;
                j += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (r[start..j].iter().collect(), j + 1);
                }
                j += 1;
            }
            _ => j += 1,
        }
    }
    (r[start.min(r.len())..].iter().collect(), r.len())
}

/// A `[…]` group starting at `r[i]`: the inner contents and the index past the `]`; unbalanced
/// runs to end (inline.go:419-426).
fn scan_bracket(r: &[char], i: usize) -> (String, usize) {
    let mut j = i + 1;
    while j < r.len() {
        if r[j] == ']' {
            return (r[i + 1..j].iter().collect(), j + 1);
        }
        j += 1;
    }
    (r[(i + 1).min(r.len())..].iter().collect(), r.len())
}

/// Whether `s` reads as ONE visual atom for fraction/root parenthesization: a single rune, or a
/// run with no operator/space that would make an unparenthesized slash ambiguous
/// (inline.go:431-446).
fn is_single_atom(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next().is_none() || chars.next().is_none() {
        return true; // 0 or 1 runes — an operator alone still counts (inline.go:433-435)
    }
    !s.chars().any(|c| "+-*/=<> ".contains(c))
}

#[cfg(test)]
mod tests {
    use super::{collapse_space_runs, is_layout_macro_inline, is_single_atom};
    use crate::mathtext::approx_inline;

    // Go: internal/mathtext/inline_test.go:8 TestApproxInlineGolden
    #[test]
    fn approx_inline_golden() {
        let cases: [(&str, &str); 22] = [
            // Greek + operators, with restored separating spaces.
            (r"\alpha + \beta", "α + β"),
            (r"5 \times 3", "5 × 3"),
            (r"\hbar\omega", "ℏω"),
            // Superscripts: every char has a glyph → unicode.
            ("x^2 + y^2", "x² + y²"),
            ("E = mc^2", "E = mc²"),
            ("e^{-x}", "e⁻ˣ"),
            ("a^{10}", "a¹⁰"),
            // Subscripts: available letters map, unavailable fall back to linear.
            ("a_i", "aᵢ"),
            ("x_{ij}", "xᵢⱼ"),
            ("x_{10}", "x₁₀"),
            ("C_B", "C_B"),     // capital B has NO subscript → linear
            ("x_{ab}", "x_ab"), // 'b' has no subscript → the whole script goes linear
            // Fractions.
            (r"\frac{a}{b}", "a/b"),
            (r"\frac{a+b}{c}", "(a+b)/c"),
            // Roots.
            (r"\sqrt{x}", "√x"),
            (r"\sqrt{x+1}", "√(x+1)"),
            // Sum with limits (super+subscripts each fully mappable).
            (r"\sum_{i=1}^{n} x_i", "∑ᵢ₌₁ⁿ xᵢ"),
            // \text passthrough + relation with clean spacing.
            (r"\text{if } x \geq 0", "if x ≥ 0"),
            // Accent stripped (no combining mark).
            (r"\vec{v}", "v"),
            // Layout-only macros dropped.
            (r"\left( \frac{a}{b} \right)", "( a/b )"),
            // Tolerant of delimiters left on by the caller.
            ("$x^2$", "x²"),
            (r"\(a + b\)", "a + b"),
        ];
        for (src, want) in cases {
            assert_eq!(approx_inline(src), want, "approx_inline({src:?})");
        }
    }

    // Go: internal/mathtext/inline_test.go:62 TestApproxInlineNeverMultiline
    #[test]
    fn approx_inline_never_multiline() {
        for src in ["a\nb", r"\frac{a}{b}", "x^2 \\\\ y", "line1\nline2"] {
            assert!(
                !approx_inline(src).contains('\n'),
                "approx_inline({src:?}) contains a newline"
            );
        }
    }

    // Go: internal/mathtext/inline_test.go:70 TestApproxInlineDollarNotMangled
    #[test]
    fn approx_inline_dollar_not_mangled() {
        assert_eq!(approx_inline(r"\$5 is cheap"), "$5 is cheap");
    }

    // Go: internal/mathtext/inline_test.go:78 TestApproxInlineMissingSubscriptStaysLinear
    #[test]
    fn approx_inline_missing_subscript_stays_linear() {
        for src in ["C_B", "x_c", "y_d", "z_q", "n_w"] {
            let got = approx_inline(src);
            assert!(
                got.contains('_'),
                "approx_inline({src:?}) = {got:?}, want the linear underscore fallback"
            );
            assert!(
                !got.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c)),
                "approx_inline({src:?}) = {got:?} contains a combining mark"
            );
        }
    }

    // Go: internal/mathtext/inline_test.go:94 TestApproxInlineNoCombiningMarks
    #[test]
    fn approx_inline_no_combining_marks() {
        let inputs = [
            "x^2 + y_j",
            r"\vec{v}",
            r"\hat{x}",
            r"\sum_{i}^{n}",
            r"\theta^\circ",
            r"\frac{\alpha}{\beta}",
            "C_B^A",
        ];
        for src in inputs {
            for c in approx_inline(src).chars() {
                let u = c as u32;
                assert!(
                    !((0x0300..=0x036F).contains(&u)
                        || (0x1AB0..=0x1AFF).contains(&u)
                        || (0x1DC0..=0x1DFF).contains(&u)
                        || (0x20D0..=0x20FF).contains(&u)),
                    "approx_inline({src:?}) emitted combining mark U+{u:04X}"
                );
            }
        }
    }

    // Go: internal/mathtext/inline_test.go:110 TestApproxInlineUnknownMacro
    #[test]
    fn approx_inline_unknown_macro() {
        assert_eq!(approx_inline(r"\foobar x"), "foobar x");
    }

    // Go: internal/mathtext/inline.go:41-60 collapseSpaceRuns / :68-78 layoutMacros /
    // :431-446 isSingleAtom
    #[test]
    fn inline_helpers() {
        assert_eq!(collapse_space_runs("a  b   c"), "a b c");
        assert_eq!(collapse_space_runs("a b\t\tc"), "a b\t\tc"); // only ASCII spaces collapse
        assert_eq!(collapse_space_runs("no runs"), "no runs");
        assert!(is_layout_macro_inline("left") && is_layout_macro_inline("Biggm"));
        assert!(!is_layout_macro_inline("medspace")); // in the PARSER set only
        assert!(is_single_atom("x") && is_single_atom("2x") && is_single_atom("αβ"));
        assert!(!is_single_atom("a+b") && !is_single_atom("a b"));
    }

    // Rust hardening (DESIGN §2): pathological brace nesting terminates instead of blowing the
    // stack, and the guard is far above anything real (nesting 4 in the 137 goldens).
    #[test]
    fn deep_brace_nesting_is_bounded() {
        let deep = format!("x^{}a{}", "{".repeat(600), "}".repeat(600));
        let got = approx_inline(&deep);
        assert!(!got.is_empty() && !got.contains('{'));
    }
}
