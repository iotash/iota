//! The single home of every color the frame and its surfaces emit (internal/ui/theme.go;
//! `TUI_CONTRACTS` §9). The ui layer deliberately renders with raw SGR fragments — no
//! styling library in the frame path: whatever the palette does must survive being
//! clipped, wrapped, and re-emitted by the frame and `wrap_ansi`, and bare
//! fragments compose predictably there. Semantics:
//!
//! - bold   — weight: the strong column of a two-column list (the completion list's labels)
//! - faint  — chrome and secondary text: separators, hints, placeholders
//! - cyan   — the accent: the cursor marker, prompt, spinner (never a list label)
//! - green  — positive: checked boxes, command highlight
//! - row bg — the current row of a list ([`row_bg`]): tint + [`CURSOR_MARK`] + bold label
//! - yellow — warning: a figure heading somewhere the user should notice
//! - red    — alerts ([`ERR_PREFIX`])
//! - revOn  — strong emphasis, "this is yours / this is focused": user blocks, focused tab chips,
//!   search hits
//! - bar    — a user block's `▌` column and its tab's fill: gray ([`USER_BAR`]) typed, teal
//!   ([`HOST_BAR`]) a block with a tab (a host's message) (X-105, X-107)
//!
//! Chat-side TEXT colors live in `crate::repl::render::styles`; markdown/code rendering owns its
//! own theme in `crate::markdown::style`. One home per layer.
//!
//! `NO_COLOR` is not decided here (DIVERGENCES X-28): these fragments are the palette, and
//! whether the terminal gets the colors in it is settled once at the byte→cell boundary,
//! `super::spans::ansi_to_spans`, which under `crate::app::color::ColorMode::Off` drops every
//! foreground and background and keeps the attributes — so `BOLD`, `FAINT` and `REV_ON` still
//! shape the frame when `CYAN` and `RED` have gone.

/// Bold SGR: weight, not colour — the strong text beside faint secondary text. An attribute, so
/// it survives `NO_COLOR` where a foreground does not.
pub(crate) const BOLD: &str = "\x1b[1m";
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

/// A typed user block's `▌` bar, INSIDE its reversed span: a mid gray (#8a8a8a) that reads on a dark
/// terminal and a light one alike, so one constant serves both. It is a BACKGROUND colour because
/// reverse video swaps the pair: the cell's background attribute is what the glyph is drawn in,
/// while the cell behind it shows the default foreground, the block's own background. A
/// foreground here would become the cell's background under the reverse instead.
pub(crate) const USER_BAR: &str = "\x1b[48;5;245m";
/// The bar of a block with a tab (a host's message) and the tab's fill: a teal (#008787) set as
/// a background for the same reason as [`USER_BAR`]. Its luminance is close to the gray's, so the
/// hue alone says "another sender", and it reads on a dark terminal (2.94:1 against the block) and
/// a light one (3.82:1) alike; the tab's dark ink stays AA on it (4.82:1).
pub(crate) const HOST_BAR: &str = "\x1b[48;5;30m";
/// Default background (SGR 49): ends the bar's colour after the bar, keeping the reverse.
pub(crate) const BG_DEFAULT: &str = "\x1b[49m";

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

/// The current row's background in a list, the completion list and every panel list alike: one
/// gray step past [`input_bg`] (237 vs 236 dark, 253 vs 254 light), so the "Other…" field being
/// edited on the current row stays visible against it. Painted across the whole row by
/// `crate::text::ansi::paint_row`; under `NO_COLOR` it drops and [`CURSOR_MARK`] and the bold
/// label ([`emphasize`]) carry the row.
pub(crate) fn row_bg(dark: bool) -> &'static str {
    if dark {
        "\x1b[48;5;237m"
    } else {
        "\x1b[48;5;253m"
    }
}

/// The current row's gutter: a cyan `▸` and one space, the one cursor glyph of every list. The
/// foreground closes with SGR 39 alone, so the row's background runs on.
pub(crate) const CURSOR_MARK: &str = "\x1b[36m▸\x1b[39m ";

/// A multi-select row's checked box: bold green `[x]`. ASCII on purpose — `✓` is ambiguous-width
/// in CJK locales and would shift the label column. Bold survives `NO_COLOR`.
pub(crate) const BOX_ON: &str = "\x1b[1;32m[x]\x1b[22;39m";
/// A multi-select row's unchecked box: faint `[ ]`.
pub(crate) const BOX_OFF: &str = "\x1b[2m[ ]\x1b[22m";

/// The current row's label: bold, re-asserted after every SGR escape inside it, so a label
/// carrying its own colours (`/tools`, `/jobs`) is as emphasized as a plain one. The label keeps
/// its own colours; it is never recoloured.
pub(crate) fn emphasize(label: &str) -> String {
    format!(
        "{BOLD}{}\x1b[22m",
        crate::text::ansi::reassert_sgr(label, BOLD)
    )
}
