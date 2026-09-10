//! Paste handling: wart W7 `PASTE_CR_NORMALIZATION` (`\r\n`/`\r`→`\n`, trailing `\n`s
//! trimmed), the `[#N …]` tag store for multi-line pastes, and the bounded echo
//! expansion on submit (`TUI_DESIGN` §5; model.go:234-253, 1204-1253).
//!
//! A multi-line paste collapses to a tag in the composer — a thousand-line blob is
//! unusable to edit, and ↑-recall keeps the tag so a re-submit re-expands from the
//! store. `Input.text` carries the full expansion (what the model receives);
//! `Input.display` the bounded echo (what the transcript shows). The Go
//! `pasteTagRe` regex is a hand parser here — the crate keeps zero regex deps.

use crate::ui::facade::Input;

use super::event_loop::Model;

/// How many head rows of a pasted block the sent-message echo shows (model.go:1219).
pub(crate) const PASTE_ECHO_MAX_LINES: usize = 20;

/// W7 `PASTE_CR_NORMALIZATION`: bracketed paste arrives with `\n`→`\r` translation
/// (tmux); normalize `\r\n`/`\r` → `\n` and trim the trailing newlines
/// (model.go:243-245 — Go `strings.TrimRight(content, "\n")` trims ALL of them).
pub(crate) fn normalize(data: &str) -> String {
    let s = data.replace("\r\n", "\n").replace('\r', "\n");
    s.trim_end_matches('\n').to_owned()
}

/// Routes one bracketed paste into the composer: multi-line content is stored and the
/// composer gets a `[#N …]` tag; single-line content inserts verbatim
/// (model.go:246-252).
pub(crate) fn on_paste(m: &mut Model, data: &str) {
    let content = normalize(data);
    if content.contains('\n') {
        m.pastes.push(content.clone());
        let tag = paste_tag(m.pastes.len(), &content);
        m.composer.insert_str(&tag);
    } else {
        m.composer.insert_str(&content);
    }
}

/// Renders the composer stand-in for a stored multi-line paste:
/// `"[#N first20runes… M lines]"` (model.go pasteTag, 1205-1213).
pub(crate) fn paste_tag(id: usize, content: &str) -> String {
    let lines = content.matches('\n').count() + 1;
    let first: String = content
        .split('\n')
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(20)
        .collect();
    format!("[#{id} {first}… {lines} lines]")
}

/// Builds the Display/Text pair for a submitted line (model.go makeInput): `text`
/// expands paste tags in full, `display` bounded for the transcript echo. The
/// composer's own value keeps the tags.
pub(crate) fn make_input(pastes: &[String], text: &str) -> Input {
    Input {
        display: expand_paste_echo(text, pastes),
        text: expand_paste_tags(text, pastes),
        ..Input::default()
    }
}

/// Replaces every stored-paste tag with its full content (model.go expandPasteTags).
pub(crate) fn expand_paste_tags(text: &str, pastes: &[String]) -> String {
    replace_paste_tags(text, pastes, str::to_owned)
}

/// Expands tags for the transcript echo, trimming any paste longer than
/// [`PASTE_ECHO_MAX_LINES`] to its head plus a count of what follows
/// (model.go expandPasteEcho).
pub(crate) fn expand_paste_echo(text: &str, pastes: &[String]) -> String {
    replace_paste_tags(text, pastes, |content| {
        let lines: Vec<&str> = content.split('\n').collect();
        if lines.len() <= PASTE_ECHO_MAX_LINES {
            return content.to_owned();
        }
        format!(
            "{}\n… +{} more lines",
            lines[..PASTE_ECHO_MAX_LINES].join("\n"),
            lines.len() - PASTE_ECHO_MAX_LINES
        )
    })
}

/// Rewrites every stored-paste tag through `render`; an index with no stored paste
/// (a literal `"[#7 …]"` the user typed) is left alone. Hand-parses the Go
/// `\[#(\d+)[^\]]*\]` shape: `"[#"`, one or more digits, anything but `']'`, `']'`
/// (model.go replacePasteTags).
fn replace_paste_tags(text: &str, pastes: &[String], render: impl Fn(&str) -> String) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && bytes.get(i + 1) == Some(&b'#') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 {
                let mut k = j;
                while k < bytes.len() && bytes[k] != b']' {
                    k += 1;
                }
                if k < bytes.len() {
                    // A full tag [i..=k]; out-of-range ids stay verbatim (Go parity).
                    let idx: usize = text[i + 2..j].parse().unwrap_or(0);
                    if idx >= 1 && idx <= pastes.len() {
                        out.push_str(&render(&pastes[idx - 1]));
                    } else {
                        out.push_str(&text[i..=k]);
                    }
                    i = k + 1;
                    continue;
                }
            }
        }
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    out
}
