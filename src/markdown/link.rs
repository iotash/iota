//! OSC 8 hyperlinks (`TUI_CONTRACTS` §3.5; markdown.go:90-101). Progressive enhancement —
//! terminals without support ignore the sequence and show the plain text.

/// OSC 8 wrapper `"\x1b]8;;URL\x1b\\text\x1b]8;;\x1b\\"`; URL control bytes (<0x20, 0x7f)
/// stripped so crafted content can never terminate or escape the OSC sequence (the
/// window-title lesson); `color=false` → bare text (no escapes into pipes); ZERO display
/// width for every ruler.
pub fn hyperlink(url: &str, text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let url: String = url
        .chars()
        .filter(|&r| r >= '\u{20}' && r != '\u{7f}')
        .collect();
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}
