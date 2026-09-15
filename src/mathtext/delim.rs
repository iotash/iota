//! Math delimiter scanning (internal/mathtext/delim.go): inline `$…$` / `\(…\)` spans with the
//! currency/space/escape guards, the display fences, and the strip/clean helpers. The markdown
//! renderer's `display_open`/`is_display_close` become re-exports of the twins here in WP62
//! (DESIGN D16); `markdown::inline::find_inline_math` (the rune-index twin) stays where it is.
//!
//! Everything here scans BYTES exactly as Go does (`src[i]`, `src[i-1]`): every delimiter and
//! every guard character is ASCII, so byte offsets always land on `char` boundaries.

/// One inline math span found by [`find_inline`] (delim.go:19 `InlineDelimiter`); `start`/`end` are
/// BYTE offsets into the source, `body` the text between the delimiters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineDelimiter {
    /// Byte offset of the opening delimiter.
    pub start: usize,
    /// Byte offset just past the closing delimiter.
    pub end: usize,
    /// The text between the delimiters.
    pub body: String,
}

/// The first inline math span at or after byte offset `from` (delim.go:42 `FindInline`).
///
/// Openers: `\(…\)` is always math; `$…$` is math unless the `$` is escaped (`\$`), part of a
/// `$$` display fence, or looks like currency (`"$5"` with no close, `"$ "`, a trailing lone
/// `$`). A digit right after the opener IS allowed (`$1/\pi$`); currency pairs are ruled out at
/// the CLOSE instead (see `scan_dollar_math`, which is crate-private).
#[must_use]
pub fn find_inline(src: &str, from: usize) -> Option<InlineDelimiter> {
    let b = src.as_bytes();
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                // \( ... \) explicit inline math (delim.go:47-51).
                if i + 1 < b.len()
                    && b[i + 1] == b'('
                    && let Some((body, end)) = scan_paren_math(src, i + 2)
                {
                    return Some(InlineDelimiter {
                        start: i,
                        end,
                        body,
                    });
                }
                // Any other backslash escape (\$, \\, \[, …) skips the escaped byte, so a
                // literal \$ can never open a span (delim.go:52-55).
                i += 2;
            }
            b'$' => {
                if i + 1 < b.len() && b[i + 1] == b'$' {
                    i += 2; // display fence "$$": not inline (delim.go:57-61)
                    continue;
                }
                if is_currency_dollar(b, i) {
                    i += 1; // delim.go:62-64
                    continue;
                }
                if let Some((body, end)) = scan_dollar_math(src, i + 1) {
                    return Some(InlineDelimiter {
                        start: i,
                        end,
                        body,
                    });
                }
                i += 1; // no valid close on this run: a literal $ (delim.go:68-69)
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether byte offset `i` of `src` opens an inline math span (delim.go:145 `IsInlineOpen`) — the
/// cheap gate; [`find_inline`] still validates the close.
#[must_use]
pub fn is_inline_open(src: &str, i: usize) -> bool {
    let b = src.as_bytes();
    if i >= b.len() {
        return false;
    }
    match b[i] {
        b'$' => {
            if i + 1 < b.len() && b[i + 1] == b'$' {
                return false;
            }
            !is_currency_dollar(b, i)
        }
        b'\\' => i + 1 < b.len() && b[i + 1] == b'(',
        _ => false,
    }
}

/// Whether the trimmed `line` is a bare display fence (`$$`, `\[` or `\]`; delim.go:166
/// `IsDisplayFence`). A one-line `$$…$$` formula is a BODY, not a fence.
#[must_use]
pub fn is_display_fence(line: &str) -> bool {
    let t = line.trim();
    t == "$$" || t == "\\[" || t == "\\]"
}

/// Does the trimmed line OPEN a display-math block? `(body, one_line)` — body empty for the bare
/// multi-line fence (delim.go:181 `DisplayOpen`; the twin of `markdown::blocks::math::display_open`,
/// which becomes a re-export of this in WP62).
///
/// A bare `\]`, or a lone `$$` closing an already-open block, is NOT an opener: the caller tracks
/// the open state.
#[must_use]
pub fn display_open(line: &str) -> Option<(String, bool)> {
    let t = line.trim();
    // One-line dollar form: "$$ … $$" with a non-empty middle (delim.go:184-189).
    if t.len() > 4 && t.starts_with("$$") && t.ends_with("$$") {
        let inner = t[2..t.len() - 2].trim();
        if !inner.is_empty() {
            return Some((inner.to_owned(), true));
        }
    }
    // One-line bracket form: "\[ … \]" (delim.go:191-196).
    if t.len() > 4 && t.starts_with("\\[") && t.ends_with("\\]") {
        let inner = t[2..t.len() - 2].trim();
        if !inner.is_empty() {
            return Some((inner.to_owned(), true));
        }
    }
    // Multi-line openers: a bare "$$" or "\[" on its own line (delim.go:198-200).
    if t == "$$" || t == "\\[" {
        return Some((String::new(), false));
    }
    None
}

/// A bare `$$` or `\]` on its own line (delim.go:206 `IsDisplayClose`).
#[must_use]
pub fn is_display_close(line: &str) -> bool {
    let t = line.trim();
    t == "$$" || t == "\\]"
}

/// Whether the `$` at byte `i` is a currency sign rather than a delimiter (delim.go:80
/// `isCurrencyDollar`): a trailing lone `$`, or one immediately followed by a space.
pub(crate) fn is_currency_dollar(src: &[u8], i: usize) -> bool {
    if i + 1 >= src.len() {
        return true; // trailing lone '$'
    }
    src[i + 1] == b' '
}

/// Scans a `$…$` span starting at byte `start` (just past the opener); `(body, end)`
/// (delim.go:97 `scanDollarMath`).
///
/// A `$` immediately followed by an ASCII digit does NOT close (`"$10"` is currency), nor does one
/// preceded by a space (`"$a and $b"`); a newline aborts, and an all-whitespace body is rejected.
pub(crate) fn scan_dollar_math(src: &str, start: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,      // skip the escaped byte (delim.go:100-101)
            b'\n' => return None, // inline math never spans a newline (delim.go:102-103)
            b'$' => {
                if i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                    i += 1; // "$10" — currency, not a close (delim.go:105-107)
                    continue;
                }
                if i > 0 && b[i - 1] == b' ' {
                    i += 1; // a space before the close opens a new token (delim.go:108-112)
                    continue;
                }
                let inner = src.get(start..i)?;
                if inner.trim().is_empty() {
                    return None; // delim.go:114-116
                }
                return Some((inner.to_owned(), i + 1));
            }
            _ => i += 1,
        }
    }
    None
}

/// Scans a `\(…\)` span starting at byte `start` (just past the opener); `(body, end)`
/// (delim.go:125 `scanParenMath`).
pub(crate) fn scan_paren_math(src: &str, start: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let mut i = start;
    while i < b.len() {
        if b[i] == b'\\' {
            if i + 1 < b.len() && b[i + 1] == b')' {
                return Some((src.get(start..i)?.to_owned(), i + 2));
            }
            i += 2; // skip any other escape (delim.go:131)
            continue;
        }
        if b[i] == b'\n' {
            return None; // delim.go:134-136
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{display_open, find_inline, is_display_close, is_display_fence, is_inline_open};
    use crate::mathtext::{clean_source, strip_delimiters};

    fn body(src: &str) -> Option<String> {
        find_inline(src, 0).map(|d| d.body)
    }

    // Go: internal/mathtext/delim_test.go:8 TestFindInlineDollar
    #[test]
    fn find_inline_dollar() {
        let src = "the value $x^2$ is nice";
        let d = find_inline(src, 0).expect("FindInline: no match");
        assert_eq!(d.body, "x^2");
        assert_eq!(&src[d.start..d.end], "$x^2$");
    }

    // Go: internal/mathtext/delim_test.go:22 TestFindInlineParen
    #[test]
    fn find_inline_paren() {
        let src = r"energy \(E = mc^2\) here";
        let d = find_inline(src, 0).expect("FindInline: no match");
        assert_eq!(d.body, "E = mc^2");
        assert_eq!(&src[d.start..d.end], r"\(E = mc^2\)");
    }

    // Go: internal/mathtext/delim_test.go:36 TestFindInlineEscapedDollarLiteral
    #[test]
    fn find_inline_escaped_dollar_literal() {
        assert_eq!(find_inline(r"it costs \$5 and \$10 total", 0), None);
    }

    // Go: internal/mathtext/delim_test.go:44 TestFindInlineCurrencyGuard
    #[test]
    fn find_inline_currency_guard() {
        assert_eq!(find_inline("$5 is cheap", 0), None);
        assert_eq!(find_inline("pay $ 5 now", 0), None);
        assert_eq!(find_inline("only $100", 0), None);
        assert_eq!(find_inline("it costs $5 or $10", 0), None);
        assert_eq!(find_inline("$20,000 and $30,000 total", 0), None);
    }

    // Go: internal/mathtext/delim_test.go:73 TestFindInlineDigitOpener
    #[test]
    fn find_inline_digit_opener() {
        assert_eq!(body(r"$1/\pi$ series").as_deref(), Some(r"1/\pi"));
        assert_eq!(body("$2x+3$ here").as_deref(), Some("2x+3"));
        assert_eq!(body("at $0.5$ scale").as_deref(), Some("0.5"));
    }

    // Go: internal/mathtext/delim_test.go:99 TestFindInlineAdversarial
    #[test]
    fn find_inline_adversarial() {
        let math: [(&str, &str); 13] = [
            (r"$x$", "x"),
            (r"$1/\pi$ series", r"1/\pi"),
            (r"$E=mc^2$", "E=mc^2"),
            (r"$2x + 3$", "2x + 3"),
            (r"$0.5$", "0.5"),
            (r"$\alpha + \beta$", r"\alpha + \beta"),
            (r"$a_1$", "a_1"),
            (r"value $x=5$ ok", "x=5"),
            (r"$\frac{1}{2}$", r"\frac{1}{2}"),
            (r"the $n$-th term", "n"),
            (r"$P(A|B)$", "P(A|B)"),
            (r"$x$ and $y$", "x"), // first span
            (r"$a + b$ and $c-d$", "a + b"),
        ];
        for (src, want) in math {
            assert_eq!(body(src).as_deref(), Some(want), "find_inline({src:?})");
        }

        let literal: [&str; 20] = [
            "It costs $5",
            "$5.00 total",
            "from $5 to $10",
            "$20,000 and $30,000",
            "prices $1, $2, $3",
            "the total is $100.",
            "$5-$10 range",
            "I paid $99 today",
            "echo $PATH",
            "$HOME/bin exists",
            "set $a and $b now",
            "run $CMD then $ARG",
            "$5 for the $x plan",
            "a $b and $c z",
            "cost $5, gain $x, net $y",
            "just $ alone",
            "ends with $",
            "$$x$$ fence",
            "$ x $ padded",
            "100$ suffix",
        ];
        for src in literal {
            assert_eq!(find_inline(src, 0), None, "find_inline({src:?}) matched");
        }
    }

    // Go: internal/mathtext/delim_test.go:155 TestFindInlineDisplayFenceNotInline
    #[test]
    fn find_inline_display_fence_not_inline() {
        assert_eq!(find_inline("$$x+y$$", 0), None);
    }

    // Go: internal/mathtext/delim_test.go:162 TestFindInlineNoNewline
    #[test]
    fn find_inline_no_newline() {
        assert_eq!(find_inline("$a\nb$", 0), None);
    }

    // Go: internal/mathtext/delim_test.go:169 TestIsDisplayFence
    #[test]
    fn is_display_fence_shapes() {
        for s in ["$$", "  $$  ", "\\[", "\\]"] {
            assert!(is_display_fence(s), "is_display_fence({s:?}) = false");
        }
        for s in ["$$x$$", "$", "text", "\\("] {
            assert!(!is_display_fence(s), "is_display_fence({s:?}) = true");
        }
    }

    // Go: internal/mathtext/delim_test.go:184 TestStripDelimiters
    #[test]
    fn strip_delimiters_pairs() {
        let cases: [(&str, &str); 7] = [
            ("$x+1$", "x+1"),
            ("$$a=b$$", "a=b"),
            (r"\(y\)", "y"),
            (r"\[z\]", "z"),
            ("  $ w $  ", "w"),
            ("no delim", "no delim"),
            ("$", "$"), // too short to be a pair
        ];
        for (src, want) in cases {
            assert_eq!(strip_delimiters(src), want, "strip_delimiters({src:?})");
        }
    }

    // Go: internal/mathtext/delim_test.go:204 TestCleanSource
    #[test]
    fn clean_source_readable() {
        let cases: [(&str, &str); 6] = [
            (r"$\frac{a}{b}$", "a/b"),
            ("$$  a  +   b  $$", "a + b"),
            (r"\( x \quad y \)", "x y"),
            ("plain   text   here", "plain text here"),
            (r"$\alpha + \beta$", "α + β"),
            (r"\[ \sqrt{b^2-4ac} \]", "√(b²-4ac)"),
        ];
        for (src, want) in cases {
            assert_eq!(clean_source(src), want, "clean_source({src:?})");
        }
    }

    // Go: internal/mathtext/delim_test.go:225 TestCleanSourceOutOfScopeReadable
    #[test]
    fn clean_source_out_of_scope_readable() {
        let cases: [(&str, &str); 5] = [
            (r"$$\overbrace{x + y + z}^{n}$$", "x + y + zⁿ"),
            (r"$$\phantom{x} + \alpha$$", "+ α"),
            (r"$$\underbrace{a + b}_{\text{sum}}$$", "a + bₛᵤₘ"),
            (
                r"\[ \frac{-b \pm \sqrt{b^2-4ac}}{2a} \]",
                "(-b ± √(b²-4ac))/2a",
            ),
            (
                r"$$\mathcal{P}(S) = \{ T \mid T \subseteq S \}$$",
                "𝒫(S) = { T ∣ T ⊆ S }",
            ),
        ];
        for (src, want) in cases {
            let got = clean_source(src);
            assert_eq!(got, want, "clean_source({src:?})");
            assert!(
                !got.contains('\\'),
                "clean_source({src:?}) = {got:?} leaked raw TeX"
            );
        }
    }

    // Go: internal/mathtext/delim_test.go:244 TestIsInlineOpen
    #[test]
    fn is_inline_open_gate() {
        assert!(is_inline_open("$x$", 0));
        assert!(is_inline_open("$5", 0)); // gate only; the close is validated later
        assert!(!is_inline_open("$ 5", 0));
        assert!(!is_inline_open("$$x", 0));
        assert!(is_inline_open(r"\(x", 0));
        assert!(!is_inline_open(r"\[x", 0));
    }

    // Go: internal/mathtext/delim_test.go:269 TestFindInlineIgnoresDisplayFence
    #[test]
    fn find_inline_ignores_display_fence() {
        for src in ["$$x$$", "$$x^2$$", "$$a+b$$", "$$"] {
            assert_eq!(find_inline(src, 0), None, "find_inline({src:?})");
        }
    }

    // Go: internal/mathtext/delim.go:181-209 DisplayOpen / IsDisplayClose (the twins of
    // `markdown::blocks::math`, which re-exports these in WP62 — DESIGN D16)
    #[test]
    fn display_open_and_close() {
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
}
