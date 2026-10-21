//! Chat styles (`TUI_CONTRACTS` §9): raw 16-color SGR builders — DimStyle=2, ErrorStyle=31,
//! CodeStyle=36, CodeBlockStyle=32, Bold=1, Underline=4, UserBlock=reverse — plus the 256-color
//! diff shades keyed on the detected background (chat/styles.go, chat/diff.go:98-107,
//! chat/theme.go), and the tool-call label painted in segments ([`paint_label`]: dim brackets,
//! keys and tails, a bold cyan name, cyan values — where Go's header was one `CodeStyle` run,
//! DIVERGENCES X-83). No styling library anywhere in the frame path.
//!
//! Every wrapper asks [`crate::app::color::enabled`] first and hands the text back UNTOUCHED when
//! the answer is no — attributes included, because this is fatih/color's `NoColor` rule and
//! the chat side is text that gets copied, exported and replayed (DIVERGENCES X-27).

/// SGR reset.
pub(crate) const RESET: &str = "\x1b[0m";

/// `"\x1b[{params}m{s}\x1b[0m"`, or `s` verbatim when color is off.
fn sgr(params: &str, s: &str) -> String {
    sgr_if(crate::app::color::enabled(), params, s)
}

/// [`sgr`] with the decision passed in.
fn sgr_if(on: bool, params: &str, s: &str) -> String {
    if on {
        format!("\x1b[{params}m{s}{RESET}")
    } else {
        s.to_owned()
    }
}

/// A tool-call label's name: bold cyan.
const LABEL_NAME: &str = "1;36";
/// A label's values — a positional argument, a keyed argument's value, a phrase: cyan, the color
/// the status row's job segment gives the same command.
const LABEL_VALUE: &str = "36";
/// A label's frame and annotations — the brackets, `key:`, `… +N args`, the composing `…`: faint.
const LABEL_MARK: &str = "2";

/// Paints a call label in segments (docs/design/tool-label-style.md §2.2): every segment is
/// self-contained SGR around text [`ToolLabel`] already cut, so stripping the SGR gives back its
/// plain [`Display`] byte for byte, and with color off it IS that text (X-27).
///
/// [`ToolLabel`]: crate::tool::fmt::ToolLabel
/// [`Display`]: std::fmt::Display
pub(crate) fn paint_label(l: &crate::tool::fmt::ToolLabel) -> String {
    paint_label_if(crate::app::color::enabled(), l)
}

/// [`paint_label`] with the decision passed in.
fn paint_label_if(on: bool, l: &crate::tool::fmt::ToolLabel) -> String {
    use crate::tool::fmt::LabelBody;
    let mark = |s: &str| sgr_if(on, LABEL_MARK, s);
    let value = |s: &str| sgr_if(on, LABEL_VALUE, s);
    let mut out = mark("[");
    out.push_str(&sgr_if(on, LABEL_NAME, &l.name));
    // A body with no text is the bare `[name]`, exactly as the plain label drops its blank.
    let body = if l.body.detail().is_empty() {
        &LabelBody::Phrase(String::new())
    } else {
        &l.body
    };
    match body {
        LabelBody::Composing => {
            out.push(' ');
            out.push_str(&mark("…"));
        }
        LabelBody::Phrase(s) if !s.is_empty() => {
            out.push(' ');
            out.push_str(&value(s));
        }
        LabelBody::Phrase(_) => {}
        LabelBody::Args { pairs, more } => {
            for (k, v) in pairs {
                out.push(' ');
                if let Some(k) = k {
                    out.push_str(&mark(&format!("{k}:")));
                }
                out.push_str(&value(v));
            }
            if *more > 0 {
                out.push(' ');
                out.push_str(&mark(&format!("… +{more} args")));
            }
        }
    }
    out.push_str(&mark("]"));
    out
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

    use crate::tool::fmt::{LabelBody, ToolLabel};

    fn label(name: &str, body: LabelBody) -> ToolLabel {
        ToolLabel {
            name: name.to_owned(),
            body,
        }
    }

    fn args(pairs: &[(Option<&str>, &str)], more: usize) -> LabelBody {
        LabelBody::Args {
            pairs: pairs
                .iter()
                .map(|(k, v)| (k.map(str::to_owned), (*v).to_owned()))
                .collect(),
            more,
        }
    }

    /// One of each shape and the composing state, plus the edges a cut or a summary produces.
    fn shapes() -> Vec<(&'static str, ToolLabel, &'static str)> {
        vec![
            ("name only", label("edit_file", args(&[], 0)), "[edit_file]"),
            (
                "composing",
                label("shell", LabelBody::Composing),
                "[shell …]",
            ),
            (
                "positional",
                label("shell", args(&[(None, "ls -la")], 0)),
                "[shell ls -la]",
            ),
            (
                "positional with a colon",
                label("shell", args(&[(None, "echo a:b")], 0)),
                "[shell echo a:b]",
            ),
            (
                "keyed",
                label(
                    "srv:search",
                    args(&[(Some("limit"), "10"), (Some("query"), "rust tui")], 0),
                ),
                "[srv:search limit:10 query:rust tui]",
            ),
            (
                "keyed with a tail",
                label(
                    "x",
                    args(&[(Some("a"), "1"), (Some("b"), "2"), (Some("c"), "3")], 2),
                ),
                "[x a:1 b:2 c:3 … +2 args]",
            ),
            (
                "phrase",
                label(
                    "delegate",
                    LabelBody::Phrase("summarize the failing tests".to_owned()),
                ),
                "[delegate summarize the failing tests]",
            ),
            (
                "CJK value",
                label("read_file", args(&[(None, "文档/很长的文件名…")], 0)),
                "[read_file 文档/很长的文件名…]",
            ),
            (
                "empty phrase",
                label("x", LabelBody::Phrase(String::new())),
                "[x]",
            ),
            (
                "empty positional",
                label("x", args(&[(None, "")], 0)),
                "[x]",
            ),
        ]
    }

    // docs/design/tool-label-style.md §2.4: painting adds SGR and nothing else — the plain label is
    // what the screen reads, byte for byte the text before the segments were painted.
    #[test]
    fn a_painted_label_strips_back_to_its_plain_text() {
        for (case, l, plain) in shapes() {
            assert_eq!(l.to_string(), plain, "{case}: plain");
            let painted = super::paint_label_if(true, &l);
            assert!(painted.contains('\x1b'), "{case}: not painted: {painted:?}");
            assert_eq!(
                crate::text::ansi::strip_sgr(&painted),
                plain,
                "{case}: painted {painted:?}"
            );
        }
    }

    // X-27: with color off the label is its plain text — no color, no attribute.
    #[test]
    fn with_color_off_a_label_is_its_plain_text() {
        for (case, l, plain) in shapes() {
            assert_eq!(super::paint_label_if(false, &l), plain, "{case}");
        }
    }

    // The segments, byte for byte: faint brackets, keys and tails, a bold cyan name, cyan values.
    #[test]
    fn a_label_paints_its_frame_name_keys_and_values_apart() {
        let d = |s: &str| format!("\x1b[2m{s}\x1b[0m");
        let n = |s: &str| format!("\x1b[1;36m{s}\x1b[0m");
        let c = |s: &str| format!("\x1b[36m{s}\x1b[0m");
        let paint = |l: &ToolLabel| super::paint_label_if(true, l);

        assert_eq!(
            paint(&label("edit_file", args(&[], 0))),
            format!("{}{}{}", d("["), n("edit_file"), d("]"))
        );
        assert_eq!(
            paint(&label("shell", LabelBody::Composing)),
            format!("{}{} {}{}", d("["), n("shell"), d("…"), d("]"))
        );
        // A positional argument has no key to dim, even when its text holds a colon.
        assert_eq!(
            paint(&label("shell", args(&[(None, "echo a:b")], 0))),
            format!("{}{} {}{}", d("["), n("shell"), c("echo a:b"), d("]"))
        );
        assert_eq!(
            paint(&label(
                "x",
                args(&[(Some("a"), "1"), (Some("b"), "l1 l2")], 4)
            )),
            format!(
                "{}{} {}{} {}{} {}{}",
                d("["),
                n("x"),
                d("a:"),
                c("1"),
                d("b:"),
                c("l1 l2"),
                d("… +4 args"),
                d("]")
            )
        );
        // A phrase is one sentence, painted whole.
        assert_eq!(
            paint(&label("delegate", LabelBody::Phrase("go on".to_owned()))),
            format!("{}{} {}{}", d("["), n("delegate"), c("go on"), d("]"))
        );
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
