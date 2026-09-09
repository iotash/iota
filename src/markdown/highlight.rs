//! The one code-highlight seam, shared with the chat diff renderer
//! (`TUI_CONTRACTS` §3.5; T-09; Go markdown.Highlight, markdown.go:1498-1532), and its shipped
//! implementation, [`SyntectHighlighter`] (below; formerly `highlight_syntect.rs`).

use crate::markdown::CodeTheme;

/// The one code-highlight pipeline, shared with the diff renderer.
pub trait CodeHighlighter: Send + Sync {
    /// Highlights `code` for `lang` under `theme`. MUST emit foreground-only SGR
    /// (never `\x1b[48;`) — the Error-token-neutralization invariant: the diff
    /// renderer's ± background blocks are painted over this output and rely on it.
    /// The result is NOT indented — Go's `Highlight` never indents; the code-block
    /// renderer adds the 2-space indent and the diff path shares the seam indent-free.
    fn highlight(&self, code: &str, lang: &str, theme: CodeTheme) -> String;
}

/// The no-highlight impl: the code passes through unchanged with zero escapes, and the
/// code-block renderer's uniform indent yields the plain 2-space-indented block (T-09).
/// Not the shipped seam ([`active`] is syntect); kept as the pass-through implementation
/// of [`CodeHighlighter`] for callers that want none.
pub struct PlainIndent;

impl CodeHighlighter for PlainIndent {
    fn highlight(&self, code: &str, _lang: &str, _theme: CodeTheme) -> String {
        code.to_owned()
    }
}

/// The active fenced-code highlighter (T-09): [`SyntectHighlighter`], always.
///
/// Public because the chat diff renderer shares this exact pipeline, the way Go exported
/// `markdown.Highlight` for `chat/diff.go` (DEVIATIONS3 `[WP55]`): a caller outside the
/// crate gets the one shipped highlighter, with no plumbing of its own.
#[must_use]
pub fn active() -> &'static dyn CodeHighlighter {
    &SyntectHighlighter
}

// --- the shipped implementation ---------------------------------------------------------
// The shipped implementation of the [`CodeHighlighter`] seam: syntect +
// two-face standing in for Go's chroma (`markdown.Highlight`, markdown.go:1498-1532;
// `TUI_DIVERGENCES` T-09).
//
// Three laws, all load-bearing and all pinned by `tests/highlight.rs`:
//
// 1. **Foreground only.** The formatter emits `\x1b[38;5;N` (plus the bold / italic /
//    underline attributes) and NEVER `\x1b[48;`. The chat diff renderer paints its ±
//    background blocks *around* this output and re-arms them after every reset, so a
//    single background byte from here would tear a hole in the block.
// 2. **Unrecognised text is left alone.** Go had to actively neutralise chroma's `Error`
//    token, whose styles carry an alarm BACKGROUND (github: white on deep red), because
//    lexers mark anything they cannot parse — CJK punctuation, prose inside a template
//    literal, dialect syntax — as `Error`. syntect has no `Error` token: unparsed runs
//    simply keep the theme's default foreground. Emitting *nothing* for those regions
//    reproduces Go's "a parse failure deserves no styling at all" and extends the
//    bare-fence law (code.rs) to every plain-scope run: the terminal's own foreground is
//    readable by definition, a theme's is a bet on background detection.
// 3. **Escapes never span a line.** Every styled run resets (`\x1b[0m`) at its end and a
//    trailing newline is emitted OUTSIDE the run, so the code-block renderer's uniform
//    2-space indent (and the diff renderer's per-line framing) can never inherit a color.
//
// The syntax set is bat's (via two-face), which is where the chroma-comparable language
// coverage comes from; the themes are `MonokaiExtended` / `Github`, the two-face
// equivalents of chroma's `monokai` / `github`.

use std::fmt::Write as _;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, Style, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

/// The shipped [`CodeHighlighter`]: syntect highlighting rendered as
/// foreground-only 256-color SGR. Shared by the fenced-code renderer and the chat diff
/// renderer, exactly like Go's exported `markdown.Highlight`.
///
/// Unit struct by design — the syntax set and themes are process-wide lazies, so the
/// highlighter itself is free to construct and safe to hold in a `static`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyntectHighlighter;

impl CodeHighlighter for SyntectHighlighter {
    /// Highlights `code` as `lang` under `theme`, or returns it verbatim when the language
    /// is unknown or the grammar fails mid-parse.
    ///
    /// Go fell back to a whole-block green (`codeFallbackStyle`) when chroma errored; that
    /// lives on the far side of a `Result` the frozen seam does not have, and the diff
    /// half of the same Go pipeline already falls back to plain text, so both halves fall
    /// back to plain here (DEVIATIONS3 `[WP55]`).
    fn highlight(&self, code: &str, lang: &str, theme: CodeTheme) -> String {
        let syntaxes = syntaxes();
        let Some(syntax) = resolve(syntaxes, lang) else {
            return code.to_owned();
        };
        render(code, syntax, syntaxes, theme_of(theme)).unwrap_or_else(|| code.to_owned())
    }
}

/// bat's syntax set (two-face), deserialized once per process on first use.
///
/// `pub(crate)` because the `/export` HTML renderer's `SyntaxHighlighterAdapter`
/// (`markdown::html`) highlights the same fences into CSS classes and must resolve them
/// against the SAME grammar set — a second `SyntaxSet` would double the dump in memory.
pub(crate) fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// The two-face theme set; individual themes deserialize lazily inside it.
///
/// `pub(crate)` for `markdown::html`, which reads two of the same themes to emit the
/// `/export` token stylesheet.
pub(crate) fn themes() -> &'static EmbeddedLazyThemeSet {
    static THEMES: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES.get_or_init(two_face::theme::extra)
}

/// The two-face theme standing in for the chroma style of the same name
/// (markdown.go:1546-1550 `SetCodeTheme`): monokai for dark backgrounds, github for light.
fn theme_of(theme: CodeTheme) -> &'static Theme {
    themes().get(match theme {
        CodeTheme::Monokai => EmbeddedThemeName::MonokaiExtended,
        CodeTheme::Github => EmbeddedThemeName::Github,
    })
}

/// Resolves a fence's language token (or, on the diff path, a file extension) to a
/// grammar. `find_syntax_by_token` already tries extensions then case-insensitive names;
/// a dotted token (`"main.rs"`, `".rs"`) retries on its last segment so the diff renderer
/// can hand over a bare file name.
///
/// `None` — including for the empty token — means "render verbatim". Go reached for
/// `lexers.Fallback` (plaintext) here, which paints the whole block in the style's Text
/// color; the bare-fence law (code.rs) already rejects that bet, so an unknown language
/// gets the terminal's default foreground instead.
///
/// `pub(crate)` for `markdown::html`: the `/export` adapter must make the SAME
/// known/unknown decision the terminal renderer makes, because an unrecognised fence has
/// to fall through to the plain `<pre><code>` shape (goldmark-highlighting does exactly
/// this when chroma has no lexer).
pub(crate) fn resolve<'a>(syntaxes: &'a SyntaxSet, lang: &str) -> Option<&'a SyntaxReference> {
    let lang = lang.trim();
    if lang.is_empty() {
        return None;
    }
    syntaxes.find_syntax_by_token(lang).or_else(|| {
        let tail = lang.rsplit('.').next()?;
        if tail == lang || tail.is_empty() {
            return None;
        }
        syntaxes.find_syntax_by_token(tail)
    })
}

/// Renders every line through the grammar. `None` = the grammar failed (a bad regex, a
/// runaway backtrack): the caller falls back to the verbatim code rather than emitting a
/// half-highlighted block whose parse state is already wrong.
fn render(
    code: &str,
    syntax: &SyntaxReference,
    syntaxes: &SyntaxSet,
    theme: &Theme,
) -> Option<String> {
    let mut lines = HighlightLines::new(syntax, theme);
    // `Highlighter::get_default().foreground` verbatim (syntect highlighter.rs:304-310),
    // read straight off the theme: building a second `Highlighter` just to ask would walk
    // every scope rule again, and the diff renderer calls this once PER LINE.
    let plain = theme.settings.foreground.unwrap_or(Color::BLACK);
    let mut out = String::with_capacity(code.len() + code.len() / 2);
    for line in LinesWithEndings::from(code) {
        for (style, text) in lines.highlight_line(line, syntaxes).ok()? {
            // The newline rides OUTSIDE the SGR run: an escape that spans the line break
            // would be inherited by the next line's indent.
            let body = text.trim_end_matches('\n');
            push_styled(&mut out, style, body, plain);
            out.push_str(&text[body.len()..]);
        }
    }
    Some(out)
}

/// Appends one region: a foreground-only SGR run with a trailing reset, or the raw text
/// when the region carries no styling of its own (law 2).
fn push_styled(out: &mut String, style: Style, text: &str, plain: Color) {
    if text.is_empty() {
        return;
    }
    let bold = style.font_style.contains(FontStyle::BOLD);
    let italic = style.font_style.contains(FontStyle::ITALIC);
    let underline = style.font_style.contains(FontStyle::UNDERLINE);
    if !bold && !italic && !underline && same_rgb(style.foreground, plain) {
        out.push_str(text);
        return;
    }
    out.push_str("\x1b[");
    if bold {
        out.push_str("1;");
    }
    if italic {
        out.push_str("3;");
    }
    if underline {
        out.push_str("4;");
    }
    // Writing into a String is infallible; the Result exists only to satisfy `fmt::Write`.
    let _ = write!(out, "38;5;{}m", xterm256(style.foreground));
    out.push_str(text);
    out.push_str("\x1b[0m");
}

/// RGB equality; the alpha byte is theme metadata (0 marks "inherit"), not a color.
fn same_rgb(a: Color, b: Color) -> bool {
    a.r == b.r && a.g == b.g && a.b == b.b
}

/// The six component levels of the xterm 6x6x6 color cube.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Maps a theme's 24-bit color onto the xterm-256 palette — the `terminal256` formatter
/// Go asked chroma for. Only the 6x6x6 cube (16-231) and the 24-step grey ramp (232-255)
/// are candidates: the first 16 slots are whatever the user's terminal theme redefined
/// them to be, which is precisely the readability bet the bare-fence law refuses.
fn xterm256(c: Color) -> u8 {
    let want = (i32::from(c.r), i32::from(c.g), i32::from(c.b));
    let (ri, gi, bi) = (cube_index(c.r), cube_index(c.g), cube_index(c.b));
    let cube_dist = dist(want, (level(ri), level(gi), level(bi)));

    // Grey ramp: index 232+i has every component at 8 + 10i, so the nearest step of a
    // color whose average is `avg` is round((avg - 8) / 10) = (avg - 3) / 10.
    let avg = (u16::from(c.r) + u16::from(c.g) + u16::from(c.b)) / 3;
    let step = u8::try_from(avg.saturating_sub(3) / 10)
        .unwrap_or(23)
        .min(23);
    let grey = i32::from(8 + 10 * u16::from(step));
    let grey_dist = dist(want, (grey, grey, grey));

    if grey_dist < cube_dist {
        232 + step
    } else {
        16 + 36 * ri + 6 * gi + bi
    }
}

/// The value of cube level `i` (`i` always comes from [`cube_index`], so it is in range).
fn level(i: u8) -> i32 {
    i32::from(CUBE_LEVELS[usize::from(i).min(5)])
}

/// The nearest cube level index (0-5) for one component.
fn cube_index(v: u8) -> u8 {
    let mut best = 0_u8;
    let mut best_d = i32::MAX;
    for (i, lv) in CUBE_LEVELS.iter().enumerate() {
        let d = (i32::from(v) - i32::from(*lv)).abs();
        if d < best_d {
            best_d = d;
            best = u8::try_from(i).unwrap_or(0);
        }
    }
    best
}

/// Squared RGB distance (no square root needed to compare two candidates).
fn dist(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
    let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    dr * dr + dg * dg + db * db
}

#[cfg(test)]
mod tests {
    use super::{CUBE_LEVELS, cube_index, resolve, syntaxes, theme_of, xterm256};
    use syntect::highlighting::Color;

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 0xff }
    }

    // New: the 256-color quantizer only ever lands in the cube (16-231) or the grey ramp
    // (232-255) — never in the terminal-themable first 16 slots.
    #[test]
    fn xterm256_stays_out_of_the_system_colors() {
        for r in (0..=255_u16).step_by(17) {
            for g in (0..=255_u16).step_by(51) {
                for b in (0..=255_u16).step_by(51) {
                    #[allow(clippy::cast_possible_truncation)]
                    let idx = xterm256(rgb(r as u8, g as u8, b as u8));
                    assert!(idx >= 16, "index {idx} is a system color");
                }
            }
        }
    }

    // New: the exact cube corners and grey steps round-trip to their own indices.
    #[test]
    fn xterm256_pins_exact_palette_entries() {
        assert_eq!(xterm256(rgb(0, 0, 0)), 16);
        assert_eq!(xterm256(rgb(255, 255, 255)), 231);
        assert_eq!(xterm256(rgb(255, 0, 0)), 196);
        assert_eq!(xterm256(rgb(0, 255, 0)), 46);
        assert_eq!(xterm256(rgb(0, 0, 255)), 21);
        // 8,8,8 is grey step 0; 238,238,238 is grey step 23.
        assert_eq!(xterm256(rgb(8, 8, 8)), 232);
        assert_eq!(xterm256(rgb(238, 238, 238)), 255);
        for (i, level) in CUBE_LEVELS.iter().enumerate() {
            assert_eq!(usize::from(cube_index(*level)), i);
        }
    }

    // New: the language resolver accepts a fence token, an extension and a bare file name
    // (the diff renderer's shape), and refuses to guess for an empty or unknown one.
    #[test]
    fn resolve_accepts_fence_tokens_extensions_and_file_names() {
        let ps = syntaxes();
        for token in ["python", "Python", "rs", "JavaScript", "go", "main.rs", "c"] {
            assert!(resolve(ps, token).is_some(), "unresolved: {token}");
        }
        assert!(resolve(ps, "").is_none());
        assert!(resolve(ps, "   ").is_none());
        assert!(resolve(ps, "definitely-not-a-language").is_none());
    }

    // New: both themes load — a missing two-face theme name would panic inside `get`.
    #[test]
    fn both_code_themes_resolve() {
        for theme in [
            crate::markdown::CodeTheme::Monokai,
            crate::markdown::CodeTheme::Github,
        ] {
            assert!(theme_of(theme).settings.foreground.is_some());
        }
    }
}
