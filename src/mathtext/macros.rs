//! Macro classification (internal/mathtext/macros.go + the parser's env tables): which names are
//! big operators, word operators, function names, accents, font switches, layout macros and
//! matrix/aligned/gathered environments.
//!
//! The INLINE approximation keeps its OWN layout-macro set ([`crate::mathtext::inline`], from
//! inline.go:68-78) — the two sets differ (`left`/`right` are structural here, droppable there)
//! and must never be merged.

use crate::mathtext::parse::OpFamily;
use crate::mathtext::symbols::{AccentKind, MathStyle};

/// The family of a big-operator macro (macros.go:17-27): `\iint`/`\iiint`/`\oint` collapse to
/// `Int`, the `\bigX` operators to their family. Only the limit-stacking behaviour depends on
/// the family; the drawn glyph comes from [`big_op_single_glyph`].
pub(crate) fn big_op_kind(name: &str) -> OpFamily {
    match name {
        // macros.go:19
        "prod" | "coprod" | "bigotimes" | "bigodot" | "bigwedge" | "bigvee" => OpFamily::Prod,
        // macros.go:21
        "int" | "iint" | "iiint" | "oint" => OpFamily::Int,
        // macros.go:23 sum, bigcup, bigcap, bigoplus, …
        _ => OpFamily::Sum,
    }
}

/// The single-glyph form of a big operator (macros.go:34-67). A union stays `⋃`, not `∑`.
#[allow(clippy::match_same_arms)] // one entry per line, diffable against macros.go
pub(crate) fn big_op_single_glyph(name: &str) -> &'static str {
    match name {
        "sum" => "\u{2211}",       // macros.go:37 ∑
        "prod" => "\u{220f}",      // macros.go:39 ∏
        "coprod" => "\u{2210}",    // macros.go:41 ∐
        "int" => "\u{222b}",       // macros.go:43 ∫
        "iint" => "\u{222c}",      // macros.go:45 ∬
        "iiint" => "\u{222d}",     // macros.go:47 ∭
        "oint" => "\u{222e}",      // macros.go:49 ∮
        "bigcup" => "\u{22c3}",    // macros.go:51 ⋃
        "bigcap" => "\u{22c2}",    // macros.go:53 ⋂
        "bigoplus" => "\u{2a01}",  // macros.go:55 ⨁
        "bigotimes" => "\u{2a02}", // macros.go:57 ⨂
        "bigodot" => "\u{2a00}",   // macros.go:59 ⨀
        "bigwedge" => "\u{22c0}",  // macros.go:61 ⋀
        "bigvee" => "\u{22c1}",    // macros.go:63 ⋁
        _ => "\u{2211}",           // macros.go:65 ∑ (unreachable: the parser vets the name)
    }
}

/// The word form of a word operator (macros.go:71-80).
#[allow(clippy::match_same_arms)] // one entry per line, diffable against macros.go
pub(crate) fn word_op_name(name: &str) -> &'static str {
    match name {
        "limsup" => "lim sup", // macros.go:74
        "liminf" => "lim inf", // macros.go:76
        "lim" => "lim",
        "max" => "max",
        "min" => "min",
        "sup" => "sup",
        "inf" => "inf",
        "det" => "det",
        "gcd" => "gcd",
        "arg" => "arg",
        "deg" => "deg",
        "dim" => "dim",
        "ker" => "ker",
        "hom" => "hom",
        _ => "", // macros.go:78 (unreachable: the parser vets the name)
    }
}

/// Whether `name` is an upright named function rendered as its own name (macros.go:85-91).
/// `\lim` and friends are word big operators (they take under-limits) and are NOT here.
pub(crate) fn is_func_name(name: &str) -> bool {
    matches!(
        name,
        // macros.go:86
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc"
        // macros.go:87
        | "sinh" | "cosh" | "tanh" | "coth"
        // macros.go:88
        | "arcsin" | "arccos" | "arctan"
        // macros.go:89
        | "log" | "ln" | "lg" | "exp"
        // macros.go:90
        | "mod" | "bmod" | "pmod"
    )
}

/// The accent a single-argument macro applies (macros.go:100-117); `\overline` reuses the
/// full-width drawn bar so it covers the whole base.
#[allow(clippy::match_same_arms)] // one entry per line, diffable against macros.go
pub(crate) fn accent_kind(name: &str) -> Option<AccentKind> {
    let k = match name {
        "hat" => AccentKind::Hat,         // macros.go:101
        "widehat" => AccentKind::Hat,     // macros.go:102
        "bar" => AccentKind::Bar,         // macros.go:103
        "overline" => AccentKind::Bar,    // macros.go:104
        "vec" => AccentKind::Vec,         // macros.go:105
        "tilde" => AccentKind::Tilde,     // macros.go:106
        "widetilde" => AccentKind::Tilde, // macros.go:107
        "dot" => AccentKind::Dot,         // macros.go:108
        "ddot" => AccentKind::Ddot,       // macros.go:109
        _ => return None,
    };
    Some(k)
}

/// The font a macro switches to (macros.go:124-137). The plain styles degrade every letter to
/// itself (the box model is glyph-plain, no SGR), so a font macro never causes a fallback.
#[allow(clippy::match_same_arms)] // one entry per line, diffable against macros.go
pub(crate) fn math_font_style(name: &str) -> Option<MathStyle> {
    let s = match name {
        "mathbb" => MathStyle::Bb,        // macros.go:125
        "mathcal" => MathStyle::Cal,      // macros.go:126
        "mathscr" => MathStyle::Cal,      // macros.go:127
        "mathfrak" => MathStyle::Frak,    // macros.go:128
        "mathbf" => MathStyle::Plain,     // macros.go:129
        "boldsymbol" => MathStyle::Plain, // macros.go:130
        "bm" => MathStyle::Plain,         // macros.go:131
        "mathrm" => MathStyle::Plain,     // macros.go:132
        "mathit" => MathStyle::Plain,     // macros.go:133
        "mathsf" => MathStyle::Plain,     // macros.go:134
        "mathtt" => MathStyle::Plain,     // macros.go:135
        "mathnormal" => MathStyle::Plain, // macros.go:136
        _ => return None,
    };
    Some(s)
}

/// The PARSER's layout-macro set (macros.go:151-165) — spacing/style/size macros with no glyph
/// and no structural argument, dropped by `parse_command`. `\left`/`\right` and `\begin`/`\end`
/// are NOT here: they are structural.
pub(crate) fn is_layout_macro(name: &str) -> bool {
    matches!(
        name,
        // horizontal spacing (macros.go:153-155)
        "quad" | "qquad" | "!" | "," | ":" | ";"
        | " " | "thinspace" | "medspace" | "thickspace"
        | "negthinspace" | "negmedspace" | "negthickspace"
        // math style / size selectors (macros.go:157-162)
        | "displaystyle" | "textstyle" | "scriptstyle"
        | "scriptscriptstyle" | "limits" | "nolimits"
        | "big" | "Big" | "bigg" | "Bigg"
        | "bigl" | "Bigl" | "biggl" | "Biggl"
        | "bigr" | "Bigr" | "biggr" | "Biggr"
        | "bigm" | "Bigm" | "biggm" | "Biggm"
        // numbering / tagging noise (macros.go:164)
        | "nonumber" | "notag"
    )
}

/// Whether `name` is a glyph big operator (parse.go:463-464).
pub(crate) fn is_big_op(name: &str) -> bool {
    matches!(
        name,
        "sum"
            | "prod"
            | "int"
            | "iint"
            | "iiint"
            | "oint"
            | "coprod"
            | "bigcup"
            | "bigcap"
            | "bigoplus"
            | "bigotimes"
            | "bigwedge"
            | "bigvee"
    )
}

/// Whether `name` is a word operator carrying under-limits (parse.go:466-467).
pub(crate) fn is_word_op(name: &str) -> bool {
    matches!(
        name,
        "lim"
            | "limsup"
            | "liminf"
            | "max"
            | "min"
            | "sup"
            | "inf"
            | "det"
            | "gcd"
            | "arg"
            | "deg"
            | "dim"
            | "ker"
            | "hom"
    )
}

/// Whether `env` is a supported matrix/cases environment (macros.go:173-182).
pub(crate) fn is_matrix_env(env: &str) -> bool {
    matches!(
        env,
        "matrix"
            | "pmatrix"
            | "bmatrix"
            | "Bmatrix"
            | "vmatrix"
            | "Vmatrix"
            | "smallmatrix"
            | "cases"
    )
}

/// Whether `env` is an alignment environment, laid out with no surrounding delimiters
/// (macros.go:192-199).
pub(crate) fn is_aligned_env(env: &str) -> bool {
    matches!(
        env,
        "aligned" | "align" | "split" | "gathered" | "gather" | "eqnarray"
    )
}

/// Whether `env` centres a single column (macros.go:205-207).
///
/// Only `layout.rs` (WP62) branches on this; the parser treats every aligned env alike.
pub(crate) fn is_gathered_env(env: &str) -> bool {
    env == "gathered" || env == "gather"
}

#[cfg(test)]
mod tests {
    use super::{
        accent_kind, big_op_kind, big_op_single_glyph, is_aligned_env, is_big_op, is_func_name,
        is_gathered_env, is_layout_macro, is_matrix_env, is_word_op, math_font_style, word_op_name,
    };
    use crate::mathtext::parse::OpFamily;
    use crate::mathtext::symbols::{AccentKind, MathStyle};

    // Go: internal/mathtext/macros.go:17-27 bigOpKind / :34-67 bigOpSingleGlyph
    #[test]
    fn big_op_families_and_glyphs() {
        assert_eq!(big_op_kind("sum"), OpFamily::Sum);
        assert_eq!(big_op_kind("bigcup"), OpFamily::Sum);
        assert_eq!(big_op_kind("prod"), OpFamily::Prod);
        assert_eq!(big_op_kind("bigotimes"), OpFamily::Prod);
        assert_eq!(big_op_kind("int"), OpFamily::Int);
        assert_eq!(big_op_kind("oint"), OpFamily::Int);
        for (name, glyph) in [
            ("sum", "∑"),
            ("prod", "∏"),
            ("coprod", "∐"),
            ("int", "∫"),
            ("iint", "∬"),
            ("iiint", "∭"),
            ("oint", "∮"),
            ("bigcup", "⋃"),
            ("bigcap", "⋂"),
            ("bigoplus", "⨁"),
            ("bigotimes", "⨂"),
            ("bigodot", "⨀"),
            ("bigwedge", "⋀"),
            ("bigvee", "⋁"),
        ] {
            assert_eq!(
                big_op_single_glyph(name),
                glyph,
                "big_op_single_glyph({name})"
            );
            assert!(
                is_big_op(name) || name == "bigodot",
                "{name} big-op membership"
            );
        }
    }

    // Go: internal/mathtext/macros.go:71-80 wordOpName / parse.go:466-467
    #[test]
    fn word_ops() {
        assert_eq!(word_op_name("limsup"), "lim sup");
        assert_eq!(word_op_name("liminf"), "lim inf");
        assert_eq!(word_op_name("lim"), "lim");
        assert_eq!(word_op_name("det"), "det");
        assert!(is_word_op("lim") && is_word_op("hom"));
        assert!(!is_word_op("sum") && !is_word_op("sin"));
    }

    // Go: internal/mathtext/macros.go:85-94 isFuncName / :100-117 accentKind / :124-145 fonts
    #[test]
    fn classification_tables() {
        assert!(is_func_name("sin") && is_func_name("bmod") && is_func_name("pmod"));
        assert!(!is_func_name("lim") && !is_func_name("foobar"));
        assert_eq!(accent_kind("hat"), Some(AccentKind::Hat));
        assert_eq!(accent_kind("overline"), Some(AccentKind::Bar));
        assert_eq!(accent_kind("widetilde"), Some(AccentKind::Tilde));
        assert_eq!(accent_kind("ddot"), Some(AccentKind::Ddot));
        assert_eq!(accent_kind("acute"), None); // not in the Tier-1 subset
        assert_eq!(math_font_style("mathbb"), Some(MathStyle::Bb));
        assert_eq!(math_font_style("mathscr"), Some(MathStyle::Cal));
        assert_eq!(math_font_style("mathbf"), Some(MathStyle::Plain));
        assert_eq!(math_font_style("operatorname"), None);
        assert!(is_layout_macro("quad") && is_layout_macro(",") && is_layout_macro("notag"));
        assert!(!is_layout_macro("left") && !is_layout_macro("right"));
    }

    // Go: internal/mathtext/macros.go:173-207 environment tables
    #[test]
    fn environment_tables() {
        assert!(is_matrix_env("pmatrix") && is_matrix_env("cases") && is_matrix_env("smallmatrix"));
        assert!(!is_matrix_env("aligned") && !is_matrix_env("matrixx"));
        assert!(
            is_aligned_env("aligned") && is_aligned_env("eqnarray") && is_aligned_env("gather")
        );
        assert!(is_gathered_env("gather") && is_gathered_env("gathered"));
        assert!(!is_gathered_env("aligned"));
    }
}
