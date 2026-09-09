//! The adversarial math guard corpus (`markdown_test.go`:968-1293) at GO'S OWN goldens: every
//! literal (currency/shell/degenerate) case AND every math body is byte-verbatim from Go now that
//! the inline hook renders through `mathtext::approx_inline` (T-08 closed for INLINE; DESIGN D16
//! step 1 — the DISPLAY twins flip with `Writer::new` in WP62).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::harness::{render_md, render_md_opts, render_md_raw};

fn one_line(s: &str) -> String {
    s.trim_end_matches('\n').to_owned()
}

// Go: internal/markdown/markdown_test.go:1238 TestInlineMathVsDollarAdversarial — THE
// disambiguation corpus: math spans vs currency/ranges/thousands/shell-vars/lone-$/trailing-$/
// $$-fence/padded-$. The rule (opener not before a space; close neither before a digit nor after
// a space) must give exactly one classification per line.
#[test]
fn test_inline_math_vs_dollar_adversarial() {
    let cases: [(&str, &str, &str); 35] = [
        // Real inline math — delimiters consumed, body approximated.
        (r"$x$", "x", "simple var"),
        (
            r"$1/\pi$ series",
            "1/π series",
            "digit opener + macro (the reported bug)",
        ),
        (r"$E=mc^2$", "E=mc²", "superscript"),
        (r"$2x + 3$", "2x + 3", "digit opener, inner spaces"),
        (r"$0.5$", "0.5", "decimal opener"),
        (r"$\alpha + \beta$", "α + β", "greek"),
        (r"$a_1$", "a₁", "subscript"),
        (
            r"value $x=5$ ok",
            "value x=5 ok",
            "digit before close, then space",
        ),
        (r"$\frac{1}{2}$", "1/2", "frac"),
        (
            r"the $n$-th term",
            "the n-th term",
            "single letter, hyphen after close",
        ),
        (
            r"$a + b$ and $c - d$",
            "a + b and c - d",
            "two spans with inner spaces",
        ),
        (r"$P(A|B)$", "P(A|B)", "pipe inside body"),
        (r"$x$ and $y$", "x and y", "two adjacent spans"),
        // Currency — must survive verbatim.
        (r"It costs $5", "It costs $5", "single price"),
        (r"$5.00 total", "$5.00 total", "decimal price"),
        (
            r"from $5 to $10",
            "from $5 to $10",
            "price range (classic pair trap)",
        ),
        (
            r"$20,000 and $30,000",
            "$20,000 and $30,000",
            "thousands separators",
        ),
        (r"prices $1, $2, $3", "prices $1, $2, $3", "three prices"),
        (r"the total is $100.", "the total is $100.", "price at end"),
        (r"$5-$10 range", "$5-$10 range", "dash range"),
        (
            r"I paid $99 today",
            "I paid $99 today",
            "mid-sentence price",
        ),
        // Shell / prose variables and code — must survive.
        (r"echo $PATH", "echo $PATH", "single shell var (no close)"),
        (r"$HOME/bin exists", "$HOME/bin exists", "path var"),
        (
            r"set $a and $b now",
            "set $a and $b now",
            "two letter vars, space before close",
        ),
        (
            r"run $CMD then $ARG",
            "run $CMD then $ARG",
            "two upper vars",
        ),
        (
            r"$5 for the $x plan",
            "$5 for the $x plan",
            "price + var (digit-opener rule guard)",
        ),
        (
            "use `$x$` inline",
            "use $x$ inline",
            "code span wins over math",
        ),
        (r"\$5 escaped", "$5 escaped", "backslash-escaped dollar"),
        (r"a $b and $c z", "a $b and $c z", "letter var pair"),
        (
            r"cost $5, gain $x, net $y",
            "cost $5, gain $x, net $y",
            "price + two vars",
        ),
        // Boundary / degenerate.
        (r"just $ alone", "just $ alone", "lone dollar with space"),
        (r"ends with $", "ends with $", "trailing dollar"),
        (r"$$x$$ fence", "$$x$$ fence", "display fence, not inline"),
        (r"$ x $ padded", "$ x $ padded", "space right after opener"),
        (r"100$ suffix", "100$ suffix", "dollar as suffix"),
    ];
    for (input, want, note) in cases {
        assert_eq!(one_line(&render_md(input)), want, "[{note}] {input:?}");
    }
}

// Go: internal/markdown/markdown_test.go:972 TestInlineMathApproximation — the inline-math
// wiring: `$…$` and `\(…\)` spans are replaced by their single-line Unicode approximation with
// the delimiters hidden, while the guarded non-math cases ($ escaped, unpaired, currency, inside
// a code span) stay literal.
#[test]
fn test_inline_math_approximation() {
    let cases = [
        ("greek", r"$\alpha$", "α"),
        ("superscript", r"$x^2$", "x²"),
        ("paren-form", r"\(a+b\)", "a+b"),
        ("in-sentence", r"value is $\beta$ here", "value is β here"),
        ("code-wins", "`$x$`", "$x$"),
        ("escaped-dollar", r"\$5", "$5"),
        ("unpaired", "cost is $5 today", "cost is $5 today"),
        ("empty", "a $ $ b", "a $ $ b"),
    ];
    for (name, input, want) in cases {
        assert_eq!(one_line(&render_md(input)), want, "[{name}] {input:?}");
    }
    // A math span must actually be styled (the cyan accent), not merely stripped.
    assert!(
        render_md_raw(r"$x^2$").contains("\x1b["),
        "inline math span lost its styling"
    );
}

// Go: internal/markdown/markdown_test.go:1024 TestInlineMathNoColor — the NoColor coupling: a
// line containing inline math emits ZERO escape bytes while still approximating the formula and
// hiding the "$" delimiters.
#[test]
fn test_inline_math_no_color() {
    let raw = render_md_opts("the value $x^2$ matters", 80, false);
    assert!(
        !raw.contains('\x1b'),
        "NoColor inline math emitted escapes: {raw:?}"
    );
    assert_eq!(one_line(&raw), "the value x² matters");
}
