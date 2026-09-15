//! Chat styles (`TUI_CONTRACTS` §9): raw 16-color SGR builders — DimStyle=2, ErrorStyle=31,
//! CodeStyle=36 (tool headers), CodeBlockStyle=32, Bold=1, Underline=4, UserBlock=reverse —
//! plus the 256-color diff shades keyed on the detected background (chat/styles.go,
//! chat/diff.go:98-107, chat/theme.go). No styling library anywhere in the frame path.
//!
//! Every wrapper asks [`crate::app::color::enabled`] first and hands the text back UNTOUCHED when
//! the answer is no — attributes included, because this is fatih/color's `NoColor` rule and
//! the chat side is text that gets copied, exported and replayed (DIVERGENCES X-27).

/// SGR reset.
pub(crate) const RESET: &str = "\x1b[0m";

/// `"\x1b[{params}m{s}\x1b[0m"`, or `s` verbatim when color is off.
fn sgr(params: &str, s: &str) -> String {
    if crate::app::color::enabled() {
        format!("\x1b[{params}m{s}{RESET}")
    } else {
        s.to_owned()
    }
}

/// Wraps `s` faint (SGR 2) — fatih `DimStyle` twin.
pub(crate) fn dim(s: &str) -> String {
    sgr("2", s)
}

/// Wraps `s` red (SGR 31) — fatih `ErrorStyle` twin.
pub(crate) fn red(s: &str) -> String {
    sgr("31", s)
}

/// Wraps `s` cyan (SGR 36) — fatih `CodeStyle` twin (tool-call headers).
pub(crate) fn cyan(s: &str) -> String {
    sgr("36", s)
}

/// Wraps `s` green (SGR 32) — fatih `CodeBlockStyle` twin.
pub(crate) fn green(s: &str) -> String {
    sgr("32", s)
}

/// Wraps `s` yellow (SGR 33) — fatih `YellowStyle` twin.
pub(crate) fn yellow(s: &str) -> String {
    sgr("33", s)
}

/// Wraps `s` bold (SGR 1) — fatih `BoldStyle` twin.
pub(crate) fn bold(s: &str) -> String {
    sgr("1", s)
}

/// Wraps `s` underlined (SGR 4) — fatih `UnderlineStyle` twin.
pub(crate) fn underline(s: &str) -> String {
    sgr("4", s)
}

/// Wraps `s` reverse-video (SGR 7) — fatih `UserBlockStyle` twin: fg/bg swap with no
/// explicit color, so the terminal's own colors form the block.
pub(crate) fn reverse(s: &str) -> String {
    sgr("7", s)
}

/// The code theme the diff renderer highlights with — the `diffCodeTheme` half of Go's
/// `applyCodeTheme` pair (chat/theme.go:38-50), which mirrors the SAME detected
/// background as [`diff_shades`] (DEVIATIONS3 `[WP55]`).
pub(crate) fn diff_code_theme(dark: bool) -> crate::markdown::CodeTheme {
    if dark {
        crate::markdown::CodeTheme::Monokai
    } else {
        crate::markdown::CodeTheme::Github
    }
}

/// The 256-color sequences for ± diff rows, matching the detected terminal background
/// (chat/diff.go:98-107): `(bg_add, bg_del, fg_add, fg_del)` — the accent foreground is
/// what the line number and marker wear (GitHub-style: the gutter wears the row's
/// semantic color, not a neutral dim).
pub(crate) fn diff_shades(dark: bool) -> (&'static str, &'static str, &'static str, &'static str) {
    if dark {
        (
            "\x1b[48;5;22m",  // deep green block
            "\x1b[48;5;52m",  // deep red block
            "\x1b[38;5;114m", // bright green accent readable on it
            "\x1b[38;5;210m", // bright red accent
        )
    } else {
        (
            "\x1b[48;5;194m", // pale green block
            "\x1b[48;5;224m", // pale red block
            "\x1b[38;5;22m",  // deep green accent
            "\x1b[38;5;88m",  // deep red accent
        )
    }
}

/// Truncates on rune boundaries + `'…'` so CJK text is never cut mid-rune
/// (chat/chat.go:250 `truncateRunes`).
pub(crate) fn truncate_runes(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        head + "…"
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::{diff_shades, dim, red, truncate_runes};

    #[test]
    fn sgr_pins() {
        // TUI_CONTRACTS §9 byte pins.
        assert_eq!(dim("x"), "\x1b[2mx\x1b[0m");
        assert_eq!(red("x"), "\x1b[31mx\x1b[0m");
        assert_eq!(super::cyan("x"), "\x1b[36mx\x1b[0m");
        assert_eq!(super::green("x"), "\x1b[32mx\x1b[0m");
        assert_eq!(super::bold("x"), "\x1b[1mx\x1b[0m");
    }

    #[test]
    fn diff_shades_follow_background() {
        // Default dark (chat/theme.go default).
        let (bg_add, bg_del, fg_add, fg_del) = diff_shades(true);
        assert_eq!(bg_add, "\x1b[48;5;22m");
        assert_eq!(bg_del, "\x1b[48;5;52m");
        assert_eq!(fg_add, "\x1b[38;5;114m");
        assert_eq!(fg_del, "\x1b[38;5;210m");
    }

    #[test]
    fn truncate_runes_is_cjk_safe() {
        assert_eq!(truncate_runes("hello", 10), "hello");
        assert_eq!(truncate_runes("hello", 5), "hello");
        assert_eq!(truncate_runes("hello!", 5), "hello…");
        assert_eq!(truncate_runes("很长的回复文字", 3), "很长的…");
    }
}
