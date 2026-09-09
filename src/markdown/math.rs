//! Math delimiter guards + the renderer hook (`TUI_CONTRACTS` §3.5; T-08/T-15 CLOSED).
//!
//! The delimiter logic is load-bearing string parsing: display fences `"$$"` / `"\["` open,
//! `"$$"` / `"\]"` close, one-line forms need `len > 4` + a non-empty inner
//! (internal/mathtext/delim.go:181-213) — those two recognizers now LIVE in
//! [`crate::mathtext::delim`] and are re-exported here, so the writer's state machine and its
//! unit tests are untouched (DESIGN D16). `find_inline_math` stays here: it is the rune-index
//! twin of `mathtext::find_inline` written against markdown.go:740-801, and the brain's
//! "two implementations in lockstep" rule keeps a test on each side.
//!
//! Only the two BODY transforms sit behind [`MathRenderer`], whose sole impl is
//! [`crate::mathtext::Mathtext`]: inline bodies through `approx_inline`, display blocks through
//! `render_2d`. The T1 raw-LaTeX stand-in is gone — both hooks render for real.

/// The body transforms the writer delegates to; [`crate::mathtext::Mathtext`] is the one impl.
pub(crate) trait MathRenderer: Send {
    /// Inline body transform (markdown.go:572 `mathtext.ApproxInline`).
    fn approx_inline(&self, body: &str) -> String;
    /// Display block transform (markdown.go:1443 `mathtext.Render2D`), one string per row.
    fn render_2d(&self, src: &str, width: usize) -> Vec<String>;
}

/// The display-fence recognizers, now owned by [`crate::mathtext::delim`] (DESIGN D16): Go keeps
/// `DisplayOpen`/`IsDisplayClose` in `delim.go:181-219` and the markdown writer calls across.
pub(crate) use crate::mathtext::delim::{display_open, is_display_close};

/// findInlineMath twin (markdown.go:740-801): does an inline math span open at
/// `runes[start]`? Returns the inner body and the rune index just past the close.
///
/// - `"$ … $"`: a span only when the opener is not at end of line and not immediately
///   followed by a space (`"$ 5"`), an UNESCAPED `"$"` closes it later on this line,
///   and the body is non-empty. A digit right after the opener IS allowed
///   (`"$1/\pi$"`); currency is ruled out at the CLOSE — a `"$"` immediately followed
///   by a digit is not a close (keeps `"$5 or $10"` / `"$20,000"` literal), and a
///   `"$"` preceded by a space does not close (`"$a and $b"`).
/// - `"\$"` never opens (the caller consumes the escape before scanning here).
/// - `"\( … \)"` is always math.
/// - `"$$"` at start is a display fence, rejected here.
pub(crate) fn find_inline_math(runes: &[char], start: usize) -> Option<(String, usize)> {
    match *runes.get(start)? {
        '$' => {
            if runes.get(start + 1) == Some(&'$') {
                return None; // "$$" display fence, never inline
            }
            let next = *runes.get(start + 1)?; // trailing lone "$" is literal
            if next == ' ' {
                return None; // "$ " — spacing/currency, never a math opener
            }
            let mut i = start + 1;
            while i < runes.len() {
                match runes[i] {
                    '\\' => {
                        i += 2; // skip the escaped rune so "\$" cannot close the span
                        continue;
                    }
                    '$' => {
                        if runes.get(i + 1).is_some_and(char::is_ascii_digit) {
                            i += 1; // "$10" — currency, not a close; keep scanning
                            continue;
                        }
                        if runes[i - 1] == ' ' {
                            i += 1; // space before the close: the second "$" opens a
                            continue; // new token, it does not close the first
                        }
                        let inner: String = runes[start + 1..i].iter().collect();
                        if inner.trim().is_empty() {
                            return None; // empty body ("$ $"): not math
                        }
                        return Some((inner, i + 1));
                    }
                    _ => {}
                }
                i += 1;
            }
            None // no closing "$": the "$" is literal (lone/currency)
        }
        '\\' => {
            // Explicit "\( … \)" inline math.
            if runes.get(start + 1) == Some(&'(') {
                let mut i = start + 2;
                while i < runes.len() {
                    if runes[i] == '\\' {
                        if runes.get(i + 1) == Some(&')') {
                            let inner: String = runes[start + 2..i].iter().collect();
                            return Some((inner, i + 2));
                        }
                        i += 2; // skip any other escape
                        continue;
                    }
                    i += 1;
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{display_open, find_inline_math, is_display_close};

    fn runes(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    // Go: internal/mathtext/delim_test.go (DisplayOpen/IsDisplayClose shapes)
    #[test]
    fn display_fence_forms() {
        assert_eq!(display_open("$$"), Some((String::new(), false)));
        assert_eq!(display_open("\\["), Some((String::new(), false)));
        assert_eq!(display_open("$$x^2$$"), Some(("x^2".to_owned(), true)));
        assert_eq!(display_open("\\[ a+b \\]"), Some(("a+b".to_owned(), true)));
        assert_eq!(display_open("$$$$"), None); // empty one-line form
        assert_eq!(display_open("$$ $$"), None); // whitespace-only inner
        assert_eq!(display_open("\\]"), None); // a close is not an opener
        assert_eq!(display_open("text"), None);
        assert!(is_display_close("$$"));
        assert!(is_display_close("  \\]"));
        assert!(!is_display_close("\\["));
        assert!(!is_display_close("$$x$$"));
    }

    // Go: internal/markdown/markdown.go:740-801 guard set (unit slice; the full
    // adversarial corpus drives the public renderer in tests/math_corpus.rs)
    #[test]
    fn inline_guards() {
        let r = runes("$x$");
        assert_eq!(find_inline_math(&r, 0), Some(("x".to_owned(), 3)));
        assert_eq!(find_inline_math(&runes("$$x$$"), 0), None); // display fence
        assert_eq!(find_inline_math(&runes("$ 5 fee"), 0), None); // space after opener
        assert_eq!(find_inline_math(&runes("$"), 0), None); // trailing lone $
        assert_eq!(find_inline_math(&runes("$5 or $10"), 0), None); // digit close guard
        assert_eq!(find_inline_math(&runes("$a and $b"), 0), None); // space-before-close
        assert_eq!(find_inline_math(&runes("$ $"), 0), None); // space after opener
        assert_eq!(find_inline_math(&runes("$  $ x"), 0), None); // empty body
        let r = runes("$1/\\pi$ series");
        assert_eq!(find_inline_math(&r, 0), Some(("1/\\pi".to_owned(), 7)));
        let r = runes("\\(a+b\\)");
        assert_eq!(find_inline_math(&r, 0), Some(("a+b".to_owned(), 7)));
        assert_eq!(find_inline_math(&runes("\\(a+b"), 0), None); // unclosed paren form
    }
}
