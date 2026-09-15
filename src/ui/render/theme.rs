//! The single home of every color the frame and its surfaces emit (internal/ui/theme.go;
//! `TUI_CONTRACTS` §9). The ui layer deliberately renders with raw SGR fragments — no
//! styling library in the frame path: whatever the palette does must survive being
//! clipped, wrapped, and re-emitted by the staging window and `wrap_ansi`, and bare
//! fragments compose predictably there. Semantics:
//!
//! - faint  — chrome and secondary text: separators, hints, placeholders
//! - cyan   — the accent: cursor rows, markers, prompt, spinner
//! - green  — positive: checked boxes, command highlight
//! - yellow — warning: a figure heading somewhere the user should notice
//! - red    — alerts ([`ERR_PREFIX`])
//! - revOn  — strong emphasis: focused tab chips, user blocks
//!
//! Chat-side TEXT colors live in `crate::repl::styles`; markdown/code rendering owns its
//! own theme in `crate::markdown::style`. One home per layer.
//!
//! `NO_COLOR` is not decided here (DIVERGENCES X-28): these fragments are the palette, and
//! whether the terminal gets the colors in it is settled once at the byte→cell boundary,
//! `super::spans::ansi_to_spans`, which under `crate::app::color::ColorMode::Off` drops every
//! foreground and background and keeps the attributes — so `FAINT` and `REV_ON` still
//! shape the frame when `CYAN` and `RED` have gone.

/// Faint/dim SGR (theme.go:22).
pub(crate) const FAINT: &str = "\x1b[2m";
/// Cyan foreground SGR (theme.go:23).
pub(crate) const CYAN: &str = "\x1b[36m";
/// Green foreground SGR (theme.go:24).
pub(crate) const GREEN: &str = "\x1b[32m";
/// Yellow foreground SGR (theme.go:25).
pub(crate) const YELLOW: &str = "\x1b[33m";
/// Red foreground SGR (theme.go:26).
pub(crate) const RED: &str = "\x1b[31m";
/// Reverse-video SGR (theme.go:27).
pub(crate) const REV_ON: &str = "\x1b[7m";
/// Full SGR reset (theme.go:28).
pub(crate) const RESET: &str = "\x1b[0m";

/// Styles surface-level error rows: red `"⚠ "` then reset (theme.go:32,
/// `RED` + `"⚠ "` + `RESET`).
pub(crate) const ERR_PREFIX: &str = "\x1b[31m⚠ \x1b[0m";

/// Search-hit highlight: reverse video (`TUI_CONTRACTS` §9).
pub(crate) const SEARCH_HIT: &str = "\x1b[7m";
/// Current search hit: reverse + yellow (`TUI_CONTRACTS` §9).
pub(crate) const SEARCH_CUR: &str = "\x1b[7;33m";

/// The input field's background for a dark or light terminal: one gray step off the
/// terminal background — distinguishable, not loud. Text renders in the default foreground
/// on top of it; the placeholder adds faint (theme.go:47-52).
pub(crate) fn input_bg(dark: bool) -> &'static str {
    if dark {
        "\x1b[48;5;236m"
    } else {
        "\x1b[48;5;254m"
    }
}
