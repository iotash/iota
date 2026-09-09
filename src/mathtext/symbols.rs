//! Symbol tables (internal/mathtext/symbols.go): Greek letters, operators, super/subscript
//! runes, accent glyphs and the math-font code-point maps. Every table is a `match` copied one
//! entry per line from Go with the Go line in a trailing comment.
//!
//! The mapping VALUES were seeded from the go-latex/latex project's `internal/tex2unicode`
//! macro-name → rune table (BSD-3-Clause, archived) exactly as Go's `symbols.go:6-11` records;
//! no upstream code is used, only the data this engine needs.

/// Accent kinds (symbols.go:297-306).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccentKind {
    /// `\hat`.
    Hat,
    /// `\bar` (a full-width overline; no glyph).
    Bar,
    /// `\vec`.
    Vec,
    /// `\tilde`.
    Tilde,
    /// `\dot`.
    Dot,
    /// `\ddot`.
    Ddot,
}

/// Math font styles (symbols.go:336-343).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum MathStyle {
    /// No font change.
    #[default]
    Plain,
    /// `\mathbb`.
    Bb,
    /// `\mathcal`.
    Cal,
    /// `\mathfrak`.
    Frak,
}

/// Greek letter by macro name (symbols.go:18-59; NO `Alpha`/`Beta`… — LaTeX has no such macro).
// One entry per line with its Go line, so a reviewer can diff the table against
// symbols.go; merging the alias arms (le/leq, to/rightarrow, …) would destroy that.
#[allow(clippy::match_same_arms)]
pub(crate) fn greek_symbol(name: &str) -> Option<char> {
    let r = match name {
        "alpha" => 'α',      // symbols.go:19 U+03B1
        "beta" => 'β',       // symbols.go:20 U+03B2
        "gamma" => 'γ',      // symbols.go:21 U+03B3
        "Gamma" => 'Γ',      // symbols.go:22 U+0393
        "delta" => 'δ',      // symbols.go:23 U+03B4
        "Delta" => 'Δ',      // symbols.go:24 U+0394
        "epsilon" => 'ε',    // symbols.go:25 U+03B5
        "varepsilon" => 'ε', // symbols.go:26 U+03B5
        "zeta" => 'ζ',       // symbols.go:27 U+03B6
        "eta" => 'η',        // symbols.go:28 U+03B7
        "theta" => 'θ',      // symbols.go:29 U+03B8
        "Theta" => 'Θ',      // symbols.go:30 U+0398
        "vartheta" => 'ϑ',   // symbols.go:31 U+03D1
        "iota" => 'ι',       // symbols.go:32 U+03B9
        "kappa" => 'κ',      // symbols.go:33 U+03BA
        "lambda" => 'λ',     // symbols.go:34 U+03BB
        "Lambda" => 'Λ',     // symbols.go:35 U+039B
        "mu" => 'μ',         // symbols.go:36 U+03BC
        "nu" => 'ν',         // symbols.go:37 U+03BD
        "xi" => 'ξ',         // symbols.go:38 U+03BE
        "Xi" => 'Ξ',         // symbols.go:39 U+039E
        "pi" => 'π',         // symbols.go:40 U+03C0
        "Pi" => 'Π',         // symbols.go:41 U+03A0
        "varpi" => 'ϖ',      // symbols.go:42 U+03D6
        "rho" => 'ρ',        // symbols.go:43 U+03C1
        "varrho" => 'ϱ',     // symbols.go:44 U+03F1
        "sigma" => 'σ',      // symbols.go:45 U+03C3
        "Sigma" => 'Σ',      // symbols.go:46 U+03A3
        "varsigma" => 'ς',   // symbols.go:47 U+03C2
        "tau" => 'τ',        // symbols.go:48 U+03C4
        "upsilon" => 'υ',    // symbols.go:49 U+03C5
        "Upsilon" => 'Υ',    // symbols.go:50 U+03A5
        "phi" => 'φ',        // symbols.go:51 U+03C6
        "varphi" => 'ϕ',     // symbols.go:52 U+03D5
        "Phi" => 'Φ',        // symbols.go:53 U+03A6
        "chi" => 'χ',        // symbols.go:54 U+03C7
        "psi" => 'ψ',        // symbols.go:55 U+03C8
        "Psi" => 'Ψ',        // symbols.go:56 U+03A8
        "omega" => 'ω',      // symbols.go:57 U+03C9
        "Omega" => 'Ω',      // symbols.go:58 U+03A9
        _ => return None,
    };
    Some(r)
}

/// Operator/relation/miscellaneous symbol by macro name (symbols.go:65-172). Aliases share a
/// target (`le`/`leq`, `to`/`rightarrow`, `dots`/`ldots`, …).
// One entry per line with its Go line, so a reviewer can diff the table against
// symbols.go; merging the alias arms (le/leq, to/rightarrow, …) would destroy that.
#[allow(clippy::match_same_arms)]
pub(crate) fn op_symbol(name: &str) -> Option<char> {
    let r = match name {
        // binary operators (symbols.go:66-79)
        "times" => '×',    // symbols.go:67 U+00D7
        "div" => '÷',      // symbols.go:68 U+00F7
        "pm" => '±',       // symbols.go:69 U+00B1
        "mp" => '∓',       // symbols.go:70 U+2213
        "cdot" => '⋅',     // symbols.go:71 U+22C5
        "ast" => '∗',      // symbols.go:72 U+2217
        "star" => '⋆',     // symbols.go:73 U+22C6
        "circ" => '∘',     // symbols.go:74 U+2218
        "bullet" => '∙',   // symbols.go:75 U+2219
        "oplus" => '⊕',    // symbols.go:76 U+2295
        "otimes" => '⊗',   // symbols.go:77 U+2297
        "odot" => '⊙',     // symbols.go:78 U+2299
        "setminus" => '∖', // symbols.go:79 U+2216
        // relations (symbols.go:81-101)
        "leq" => '≤',    // symbols.go:82 U+2264
        "le" => '≤',     // symbols.go:83 U+2264
        "geq" => '≥',    // symbols.go:84 U+2265
        "ge" => '≥',     // symbols.go:85 U+2265
        "neq" => '≠',    // symbols.go:86 U+2260
        "ne" => '≠',     // symbols.go:87 U+2260
        "approx" => '≈', // symbols.go:88 U+2248
        "equiv" => '≡',  // symbols.go:89 U+2261
        "sim" => '∼',    // symbols.go:90 U+223C
        "simeq" => '≃',  // symbols.go:91 U+2243
        "cong" => '≅',   // symbols.go:92 U+2245
        "propto" => '∝', // symbols.go:93 U+221D
        "ll" => '≪',     // symbols.go:94 U+226A
        "gg" => '≫',     // symbols.go:95 U+226B
        "perp" => '⊥',   // symbols.go:96 U+22A5
        "angle" => '∠',  // symbols.go:97 U+2220
        "mid" => '∣',    // symbols.go:98 U+2223 (divides / "such that" bar)
        "nmid" => '∤',   // symbols.go:99 U+2224
        "asymp" => '≍',  // symbols.go:100 U+224D
        "doteq" => '≐',  // symbols.go:101 U+2250
        // set theory / logic (symbols.go:103-123)
        "in" => '∈',       // symbols.go:104 U+2208
        "notin" => '∉',    // symbols.go:105 U+2209
        "ni" => '∋',       // symbols.go:106 U+220B
        "subset" => '⊂',   // symbols.go:107 U+2282
        "subseteq" => '⊆', // symbols.go:108 U+2286
        "supset" => '⊃',   // symbols.go:109 U+2283
        "supseteq" => '⊇', // symbols.go:110 U+2287
        "cup" => '∪',      // symbols.go:111 U+222A
        "cap" => '∩',      // symbols.go:112 U+2229
        "emptyset" => '∅', // symbols.go:113 U+2205
        "forall" => '∀',   // symbols.go:114 U+2200
        "exists" => '∃',   // symbols.go:115 U+2203
        "nexists" => '∄',  // symbols.go:116 U+2204
        "neg" => '¬',      // symbols.go:117 U+00AC
        "lnot" => '¬',     // symbols.go:118 U+00AC
        "land" => '∧',     // symbols.go:119 U+2227
        "wedge" => '∧',    // symbols.go:120 U+2227
        "lor" => '∨',      // symbols.go:121 U+2228
        "vee" => '∨',      // symbols.go:122 U+2228
        "parallel" => '∥', // symbols.go:123 U+2225
        // big operators / calculus (symbols.go:125-133)
        "partial" => '∂', // symbols.go:126 U+2202
        "nabla" => '∇',   // symbols.go:127 U+2207
        "sum" => '∑',     // symbols.go:128 U+2211
        "prod" => '∏',    // symbols.go:129 U+220F
        "int" => '∫',     // symbols.go:130 U+222B
        "oint" => '∮',    // symbols.go:131 U+222E
        "infty" => '∞',   // symbols.go:132 U+221E
        "surd" => '√',    // symbols.go:133 U+221A
        // arrows (symbols.go:135-144)
        "rightarrow" => '→',     // symbols.go:136 U+2192
        "to" => '→',             // symbols.go:137 U+2192
        "leftarrow" => '←',      // symbols.go:138 U+2190
        "gets" => '←',           // symbols.go:139 U+2190
        "leftrightarrow" => '↔', // symbols.go:140 U+2194
        "Rightarrow" => '⇒',     // symbols.go:141 U+21D2
        "Leftarrow" => '⇐',      // symbols.go:142 U+21D0
        "Leftrightarrow" => '⇔', // symbols.go:143 U+21D4
        "mapsto" => '↦',         // symbols.go:144 U+21A6
        // dots (symbols.go:146-151)
        "ldots" => '…', // symbols.go:147 U+2026
        "dots" => '…',  // symbols.go:148 U+2026
        "cdots" => '⋯', // symbols.go:149 U+22EF
        "vdots" => '⋮', // symbols.go:150 U+22EE
        "ddots" => '⋱', // symbols.go:151 U+22F1
        // bare delimiter macros (symbols.go:153-162)
        "langle" => '⟨',     // symbols.go:156 U+27E8
        "rangle" => '⟩',     // symbols.go:157 U+27E9
        "lfloor" => '⌊',     // symbols.go:158 U+230A
        "rfloor" => '⌋',     // symbols.go:159 U+230B
        "lceil" => '⌈',      // symbols.go:160 U+2308
        "rceil" => '⌉',      // symbols.go:161 U+2309
        "backslash" => '\\', // symbols.go:162 U+005C
        // misc named symbols (symbols.go:164-171)
        "prime" => '′', // symbols.go:165 U+2032
        "aleph" => 'ℵ', // symbols.go:166 U+2135
        "hbar" => 'ℏ',  // symbols.go:167 U+210F
        "ell" => 'ℓ',   // symbols.go:168 U+2113
        "Re" => 'ℜ',    // symbols.go:169 U+211C
        "Im" => 'ℑ',    // symbols.go:170 U+2111
        "wp" => '℘',    // symbols.go:171 U+2118
        _ => return None,
    };
    Some(r)
}

/// Greek first, then operators (symbols.go:176 `symbolRune`).
pub(crate) fn symbol_rune(name: &str) -> Option<char> {
    greek_symbol(name).or_else(|| op_symbol(name))
}

/// The superscript form of `c` (symbols.go:192-234). `q` is deliberately absent (no code point
/// exists), and so are the capitals — the caller then keeps the script linear.
pub(crate) fn superscript_rune(c: char) -> Option<char> {
    let g = match c {
        '0' => '⁰', // symbols.go:193 U+2070
        '1' => '¹', // symbols.go:194 U+00B9
        '2' => '²', // symbols.go:195 U+00B2
        '3' => '³', // symbols.go:196 U+00B3
        '4' => '⁴', // symbols.go:197 U+2074
        '5' => '⁵', // symbols.go:198 U+2075
        '6' => '⁶', // symbols.go:199 U+2076
        '7' => '⁷', // symbols.go:200 U+2077
        '8' => '⁸', // symbols.go:201 U+2078
        '9' => '⁹', // symbols.go:202 U+2079
        '+' => '⁺', // symbols.go:203 U+207A
        '-' => '⁻', // symbols.go:204 U+207B
        '=' => '⁼', // symbols.go:205 U+207C
        '(' => '⁽', // symbols.go:206 U+207D
        ')' => '⁾', // symbols.go:207 U+207E
        'a' => 'ᵃ', // symbols.go:209 U+1D43
        'b' => 'ᵇ', // symbols.go:210 U+1D47
        'c' => 'ᶜ', // symbols.go:211 U+1D9C
        'd' => 'ᵈ', // symbols.go:212 U+1D48
        'e' => 'ᵉ', // symbols.go:213 U+1D49
        'f' => 'ᶠ', // symbols.go:214 U+1DA0
        'g' => 'ᵍ', // symbols.go:215 U+1D4D
        'h' => 'ʰ', // symbols.go:216 U+02B0
        'i' => 'ⁱ', // symbols.go:217 U+2071
        'j' => 'ʲ', // symbols.go:218 U+02B2
        'k' => 'ᵏ', // symbols.go:219 U+1D4F
        'l' => 'ˡ', // symbols.go:220 U+02E1
        'm' => 'ᵐ', // symbols.go:221 U+1D50
        'n' => 'ⁿ', // symbols.go:222 U+207F
        'o' => 'ᵒ', // symbols.go:223 U+1D52
        'p' => 'ᵖ', // symbols.go:224 U+1D56
        'r' => 'ʳ', // symbols.go:225 U+02B3
        's' => 'ˢ', // symbols.go:226 U+02E2
        't' => 'ᵗ', // symbols.go:227 U+1D57
        'u' => 'ᵘ', // symbols.go:228 U+1D58
        'v' => 'ᵛ', // symbols.go:229 U+1D5B
        'w' => 'ʷ', // symbols.go:230 U+02B7
        'x' => 'ˣ', // symbols.go:231 U+02E3
        'y' => 'ʸ', // symbols.go:232 U+02B8
        'z' => 'ᶻ', // symbols.go:233 U+1DBB
        _ => return None,
    };
    Some(g)
}

/// The subscript form of `c` (symbols.go:243-277). The subscript alphabet is much smaller than
/// the superscript one — `b c d f g q w y z` and every capital are absent, so those scripts stay
/// LINEAR (never a combining mark).
pub(crate) fn subscript_rune(c: char) -> Option<char> {
    let g = match c {
        '0' => '₀', // symbols.go:244 U+2080
        '1' => '₁', // symbols.go:245 U+2081
        '2' => '₂', // symbols.go:246 U+2082
        '3' => '₃', // symbols.go:247 U+2083
        '4' => '₄', // symbols.go:248 U+2084
        '5' => '₅', // symbols.go:249 U+2085
        '6' => '₆', // symbols.go:250 U+2086
        '7' => '₇', // symbols.go:251 U+2087
        '8' => '₈', // symbols.go:252 U+2088
        '9' => '₉', // symbols.go:253 U+2089
        '+' => '₊', // symbols.go:254 U+208A
        '-' => '₋', // symbols.go:255 U+208B
        '=' => '₌', // symbols.go:256 U+208C
        '(' => '₍', // symbols.go:257 U+208D
        ')' => '₎', // symbols.go:258 U+208E
        'a' => 'ₐ', // symbols.go:260 U+2090
        'e' => 'ₑ', // symbols.go:261 U+2091
        'h' => 'ₕ', // symbols.go:262 U+2095
        'i' => 'ᵢ', // symbols.go:263 U+1D62
        'j' => 'ⱼ', // symbols.go:264 U+2C7C
        'k' => 'ₖ', // symbols.go:265 U+2096
        'l' => 'ₗ', // symbols.go:266 U+2097
        'm' => 'ₘ', // symbols.go:267 U+2098
        'n' => 'ₙ', // symbols.go:268 U+2099
        'o' => 'ₒ', // symbols.go:269 U+2092
        'p' => 'ₚ', // symbols.go:270 U+209A
        'r' => 'ᵣ', // symbols.go:271 U+1D63
        's' => 'ₛ', // symbols.go:272 U+209B
        't' => 'ₜ', // symbols.go:273 U+209C
        'u' => 'ᵤ', // symbols.go:274 U+1D64
        'v' => 'ᵥ', // symbols.go:275 U+1D65
        'x' => 'ₓ', // symbols.go:276 U+2093
        _ => return None,
    };
    Some(g)
}

/// The accent row glyph (symbols.go:313-330): Hat `^`, Vec `→`, Tilde `~`, Dot `·`, Ddot `··`;
/// `Bar` → `None`, whose glyph is instead the FULL-WIDTH drawn overline the layout builds.
///
/// Only `layout.rs` (WP62) draws accents; the parser side needs the kind, not the glyph.
pub(crate) fn accent_glyph(kind: AccentKind) -> Option<&'static str> {
    match kind {
        AccentKind::Hat => Some("^"),   // symbols.go:316 U+005E
        AccentKind::Bar => None,        // symbols.go:318 full-width drawn ─ (U+2500)
        AccentKind::Vec => Some("→"),   // symbols.go:320 U+2192
        AccentKind::Tilde => Some("~"), // symbols.go:322 U+007E
        AccentKind::Dot => Some("·"),   // symbols.go:324 U+00B7
        AccentKind::Ddot => Some("··"), // symbols.go:326 two U+00B7
    }
}

/// The blackboard-bold Letterlike-Symbols hole for `c` (symbols.go:351-359).
fn blackboard_bold(c: char) -> Option<char> {
    let g = match c {
        'C' => 'ℂ', // symbols.go:352
        'H' => 'ℍ', // symbols.go:353
        'N' => 'ℕ', // symbols.go:354
        'P' => 'ℙ', // symbols.go:355
        'Q' => 'ℚ', // symbols.go:356
        'R' => 'ℝ', // symbols.go:357
        'Z' => 'ℤ', // symbols.go:358
        _ => return None,
    };
    Some(g)
}

/// The calligraphic Letterlike-Symbols holes (symbols.go:364-376).
fn calligraphic_hole(c: char) -> Option<char> {
    let g = match c {
        'B' => 'ℬ', // symbols.go:365
        'E' => 'ℰ', // symbols.go:366
        'F' => 'ℱ', // symbols.go:367
        'H' => 'ℋ', // symbols.go:368
        'I' => 'ℐ', // symbols.go:369
        'L' => 'ℒ', // symbols.go:370
        'M' => 'ℳ', // symbols.go:371
        'R' => 'ℛ', // symbols.go:372
        'e' => 'ℯ', // symbols.go:373
        'g' => 'ℊ', // symbols.go:374
        'o' => 'ℴ', // symbols.go:375
        _ => return None,
    };
    Some(g)
}

/// The fraktur Letterlike-Symbols holes (symbols.go:378-384).
fn fraktur_hole(c: char) -> Option<char> {
    let g = match c {
        'C' => 'ℭ', // symbols.go:379
        'H' => 'ℌ', // symbols.go:380
        'I' => 'ℑ', // symbols.go:381
        'R' => 'ℜ', // symbols.go:382
        'Z' => 'ℨ', // symbols.go:383
        _ => return None,
    };
    Some(g)
}

/// Shifts `c` by its offset from `base` into the Mathematical Alphanumeric Symbols block at
/// `target`; an unrepresentable result degrades to `c` (Go's `rune` arithmetic never fails here,
/// and no `unwrap` is used).
fn shift(c: char, base: char, target: u32) -> char {
    char::from_u32(target + (c as u32 - base as u32)).unwrap_or(c)
}

/// One character under a math font (symbols.go:392-429): the Unicode holes first, then the
/// systematic U+1D538/U+1D552/U+1D7D8 (blackboard), U+1D49C/U+1D4B6 (calligraphic) and
/// U+1D504/U+1D51E (fraktur) blocks. Anything else — and every `Plain` input — is unchanged, so
/// a math-font macro never triggers a fallback.
pub(crate) fn math_font_char(style: MathStyle, c: char) -> char {
    match style {
        MathStyle::Bb => {
            if let Some(g) = blackboard_bold(c) {
                return g;
            }
            match c {
                'A'..='Z' => shift(c, 'A', 0x0001_D538), // symbols.go:399 𝔸 …
                'a'..='z' => shift(c, 'a', 0x0001_D552), // symbols.go:402 𝕒 …
                '0'..='9' => shift(c, '0', 0x0001_D7D8), // symbols.go:405 𝟘 …
                _ => c,
            }
        }
        MathStyle::Cal => {
            if let Some(g) = calligraphic_hole(c) {
                return g;
            }
            match c {
                'A'..='Z' => shift(c, 'A', 0x0001_D49C), // symbols.go:412 𝒜 …
                'a'..='z' => shift(c, 'a', 0x0001_D4B6), // symbols.go:415 𝒶 …
                _ => c,
            }
        }
        MathStyle::Frak => {
            if let Some(g) = fraktur_hole(c) {
                return g;
            }
            match c {
                'A'..='Z' => shift(c, 'A', 0x0001_D504), // symbols.go:422 𝔄 …
                'a'..='z' => shift(c, 'a', 0x0001_D51E), // symbols.go:425 𝔞 …
                _ => c,
            }
        }
        MathStyle::Plain => c,
    }
}

/// `s` under a math font (symbols.go:435-444); `Plain` is the identity, every other style maps
/// each character through [`math_font_char`].
pub(crate) fn apply_math_font(style: MathStyle, s: &str) -> String {
    if style == MathStyle::Plain {
        return s.to_owned();
    }
    s.chars().map(|c| math_font_char(style, c)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        AccentKind, MathStyle, accent_glyph, apply_math_font, math_font_char, subscript_rune,
        superscript_rune, symbol_rune,
    };

    // Go: internal/mathtext/symbols_test.go:5 TestSymbolRune
    #[test]
    fn symbol_rune_lookups() {
        let cases: [(&str, Option<char>); 36] = [
            ("alpha", Some('α')),
            ("Alpha", None), // no LaTeX \Alpha macro; must NOT resolve
            ("beta", Some('β')),
            ("pi", Some('π')),
            ("Pi", Some('Π')),
            ("Sigma", Some('Σ')),
            ("Omega", Some('Ω')),
            ("phi", Some('φ')),
            ("varphi", Some('ϕ')),
            ("infty", Some('∞')),
            ("times", Some('×')),
            ("leq", Some('≤')),
            ("le", Some('≤')),
            ("geq", Some('≥')),
            ("neq", Some('≠')),
            ("approx", Some('≈')),
            ("equiv", Some('≡')),
            ("partial", Some('∂')),
            ("nabla", Some('∇')),
            ("sum", Some('∑')),
            ("prod", Some('∏')),
            ("int", Some('∫')),
            ("in", Some('∈')),
            ("subset", Some('⊂')),
            ("cup", Some('∪')),
            ("cap", Some('∩')),
            ("forall", Some('∀')),
            ("exists", Some('∃')),
            ("rightarrow", Some('→')),
            ("to", Some('→')),
            ("Rightarrow", Some('⇒')),
            ("ldots", Some('…')),
            ("dots", Some('…')),
            ("cdot", Some('⋅')),
            ("pm", Some('±')),
            ("div", Some('÷')),
        ];
        for (name, want) in cases {
            assert_eq!(symbol_rune(name), want, "symbol_rune({name:?})");
        }
    }

    // Go: internal/mathtext/symbols_test.go:61 TestGreekAliasesDistinct
    #[test]
    fn greek_aliases_distinct() {
        assert_ne!(
            symbol_rune("sigma"),
            symbol_rune("Sigma"),
            "sigma and Sigma must map to distinct glyphs"
        );
    }

    // Go: internal/mathtext/symbols_test.go:70 TestSuperscriptAvailability
    #[test]
    fn superscript_availability() {
        for c in "0123456789+-=()abcdefghijklmnoprstuvwxyz".chars() {
            assert!(
                superscript_rune(c).is_some(),
                "superscript_rune({c:?}) = None, want a glyph"
            );
        }
        for c in ['q', 'B', 'Q', 'Z', '/', '<'] {
            assert_eq!(
                superscript_rune(c),
                None,
                "superscript_rune({c:?}) resolved, want the linear fallback"
            );
        }
    }

    // Go: internal/mathtext/symbols_test.go:86 TestSubscriptAvailability
    #[test]
    fn subscript_availability() {
        for c in "0123456789+-=()aehijklmnoprstuvx".chars() {
            assert!(
                subscript_rune(c).is_some(),
                "subscript_rune({c:?}) = None, want a glyph"
            );
        }
        for c in ['b', 'c', 'd', 'f', 'g', 'q', 'w', 'y', 'z', 'B', 'X'] {
            assert_eq!(
                subscript_rune(c),
                None,
                "subscript_rune({c:?}) resolved, want the linear fallback"
            );
        }
    }

    // Go: internal/mathtext/symbols_test.go:103 TestSuperscriptGlyphValues
    #[test]
    fn superscript_glyph_values() {
        assert_eq!(superscript_rune('2'), Some('\u{00B2}'));
        assert_eq!(superscript_rune('0'), Some('\u{2070}'));
        assert_eq!(superscript_rune('n'), Some('\u{207F}'));
        assert_eq!(superscript_rune('i'), Some('\u{2071}'));
        assert_eq!(superscript_rune('x'), Some('\u{02E3}'));
    }

    // Go: internal/mathtext/symbols_test.go:120 TestSubscriptGlyphValues
    #[test]
    fn subscript_glyph_values() {
        assert_eq!(subscript_rune('0'), Some('\u{2080}'));
        assert_eq!(subscript_rune('i'), Some('\u{1D62}'));
        assert_eq!(subscript_rune('j'), Some('\u{2C7C}'));
        assert_eq!(subscript_rune('x'), Some('\u{2093}'));
        assert_eq!(subscript_rune('+'), Some('\u{208A}'));
    }

    // Go: internal/mathtext/symbols.go:313-330 accentGlyph / :392-444 mathFontRune
    #[test]
    fn accent_glyphs_and_font_maps() {
        assert_eq!(accent_glyph(AccentKind::Hat), Some("^"));
        assert_eq!(accent_glyph(AccentKind::Bar), None);
        assert_eq!(accent_glyph(AccentKind::Vec), Some("→"));
        assert_eq!(accent_glyph(AccentKind::Tilde), Some("~"));
        assert_eq!(accent_glyph(AccentKind::Dot), Some("·"));
        assert_eq!(accent_glyph(AccentKind::Ddot), Some("··"));
        // Holes win over the systematic block.
        assert_eq!(math_font_char(MathStyle::Bb, 'R'), 'ℝ');
        assert_eq!(math_font_char(MathStyle::Cal, 'L'), 'ℒ');
        assert_eq!(math_font_char(MathStyle::Frak, 'C'), 'ℭ');
        // Systematic ranges.
        assert_eq!(math_font_char(MathStyle::Bb, 'A'), '\u{1D538}');
        assert_eq!(math_font_char(MathStyle::Bb, 'a'), '\u{1D552}');
        assert_eq!(math_font_char(MathStyle::Bb, '0'), '\u{1D7D8}');
        assert_eq!(math_font_char(MathStyle::Cal, 'A'), '\u{1D49C}');
        assert_eq!(math_font_char(MathStyle::Frak, 'g'), '\u{1D524}');
        // Plain and unmapped runes degrade to themselves.
        assert_eq!(math_font_char(MathStyle::Plain, 'R'), 'R');
        assert_eq!(math_font_char(MathStyle::Bb, '+'), '+');
        assert_eq!(apply_math_font(MathStyle::Plain, "R+1"), "R+1");
        assert_eq!(apply_math_font(MathStyle::Bb, "RC"), "ℝℂ");
    }
}
