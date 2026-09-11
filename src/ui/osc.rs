//! OSC title emission, the terminal's native progress + notification sequences, and the
//! OSC 11 background detect (`TUI_CONTRACTS` §5, T3 `T3_CONTRACTS` §5.7).
//!
//! The title rides crossterm's `SetTitle` command (a raw OSC 0 — the facade sanitized
//! the text, the loop emits on change only). `detect_background` is the one OSC 11
//! round-trip: it MUST run before any event loop claims stdin (the reply arrives as
//! terminal input), honors a 100 ms deadline, and defaults to dark — the safer side
//! for every adaptive shade (theme.go:36-38; Go used termenv the same way).
//!
//! The progress and notification tables below are the bytes
//! `github.com/charmbracelet/x/ansi` v0.11.7 emits through bubbletea's renderer
//! (`cursed_renderer.go:779-800`), written verbatim so the wire is byte-identical to Go's.

use std::borrow::Cow;
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::queue;
use crossterm::terminal::SetTitle;

use super::facade::ProgressState;

/// The OSC 11 reply deadline (`TUI_CONTRACTS` §5: 100ms timeout).
const BG_REPLY_TIMEOUT: Duration = Duration::from_millis(100);

/// OSC 9;4 reset — no bar (`ProgressState::None`, and the exit clear).
pub(crate) const OSC_PROGRESS_RESET: &str = "\x1b]9;4;0\x07";
/// OSC 9;4 indeterminate — a turn is running (no value).
pub(crate) const OSC_PROGRESS_BUSY: &str = "\x1b]9;4;3\x07";
/// OSC 9;4 warning at 100 — blocked on the user.
///
/// A full bar, not a 0 % sliver: warning and error are states of the WHOLE turn, and a
/// sliver would be invisible on terminals that render the percentage (model.go:121-124).
pub(crate) const OSC_PROGRESS_INPUT: &str = "\x1b]9;4;4;100\x07";
/// OSC 9;4 error at 100 — the turn failed.
pub(crate) const OSC_PROGRESS_ERROR: &str = "\x1b]9;4;2;100\x07";
/// Enable focus reporting (mode 1004; crossterm `EnableFocusChange`, event.rs:387).
pub(crate) const CSI_FOCUS_ON: &str = "\x1b[?1004h";
/// Disable focus reporting (crossterm `DisableFocusChange`, event.rs:403).
pub(crate) const CSI_FOCUS_OFF: &str = "\x1b[?1004l";

/// The OSC 9;4 sequence of a progress state; `None` resets the bar.
pub(crate) fn progress_seq(s: ProgressState) -> &'static str {
    match s {
        ProgressState::None => OSC_PROGRESS_RESET,
        ProgressState::Busy => OSC_PROGRESS_BUSY,
        ProgressState::Input => OSC_PROGRESS_INPUT,
        ProgressState::Error => OSC_PROGRESS_ERROR,
    }
}

/// One attention ping: the OSC 9 desktop notification carrying `text` (whose BEL merely
/// terminates the sequence) followed by a bare BEL that actually rings — hosts listen to
/// one or the other (model.go:145-162, `ansi.Notify(text) + "\a"`).
pub(crate) fn notify_seq(text: &str) -> String {
    format!("\x1b]9;{text}\x07\x07")
}

/// Defuses a notification payload that would parse as a PROGRESS REPORT: an OSC 9 body
/// opening `"4;"` is `9;4;…` to every terminal supporting both, so a leading space keeps
/// it a notification. Guarded here at the mechanism, not by call-site convention — the
/// text is an arbitrary content digest (model.go:151-157).
pub(crate) fn defuse_notify(text: &str) -> Cow<'_, str> {
    if text.starts_with("4;") {
        Cow::Owned(format!(" {text}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Queues the window-title OSC onto `w` (crossterm `SetTitle`); the caller flushes.
pub(crate) fn emit_title<W: Write>(w: &mut W, title: &str) -> io::Result<()> {
    queue!(w, SetTitle(title))
}

/// ONE OSC 11 round-trip on the tty; 100ms timeout; default true (dark).
///
/// Mechanism: the query is chased by a DSR 6n (`\x1b[6n`) on the same write. Every
/// terminal this UI can run on answers DSR (ratatui's inline anchoring depends on it),
/// so the read loop ends at the DSR reply's `R` terminator whether or not OSC 11 was
/// understood — the 100ms recv timeout only covers a terminal that answers neither.
/// In that pathological case the detached reader thread stays parked on the tty read;
/// accepted and documented (it exits on the next byte, and a terminal mute to DSR
/// cannot run the TUI anyway).
pub(crate) fn detect_background() -> bool {
    detect_dark().unwrap_or(true)
}

/// The fallible body: `Err` (no tty, raw-mode failure, timeout) → the dark default.
fn detect_dark() -> io::Result<bool> {
    let mut tty = open_tty()?;
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw {
        crossterm::terminal::enable_raw_mode()?;
    }
    let result = query(&mut tty);
    if !was_raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    result
}

/// The controlling terminal, opened read+write: the query goes out on it and the reply
/// comes back on it, which is why this is `/dev/tty` and not stdin/stdout (either may be a
/// pipe, and a redirect must not turn the detect into a write to a file).
#[cfg(unix)]
fn open_tty() -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

/// Windows has no `/dev/tty`, and its nearest pair — `CONIN$` / `CONOUT$` — cannot stand in
/// here: they are two handles where this needs one duplex file, the reply only arrives once
/// the console input mode carries VT sequences, and `conhost` answers OSC 11 with nothing at
/// all. So the round-trip is declined outright rather than attempted and left to time out:
/// [`detect_background`] reads this `Err` as "unknown" and takes its documented default,
/// dark — which is also what Windows Terminal ships with. A real detect belongs with a real
/// terminal to verify it against.
#[cfg(windows)]
fn open_tty() -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the OSC 11 background query needs a controlling terminal (no /dev/tty on Windows)",
    ))
}

/// Writes `OSC 11 ; ? ST` + `DSR 6n`, reads the reply bytes off the tty (bounded by
/// the DSR terminator), and classifies the reported background color.
fn query(tty: &mut File) -> io::Result<bool> {
    tty.write_all(b"\x1b]11;?\x1b\\\x1b[6n")?;
    tty.flush()?;
    let mut reader = tty.try_clone()?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("iota-osc11".to_owned())
        .spawn(move || {
            let mut buf = Vec::with_capacity(64);
            let mut byte = [0u8; 1];
            loop {
                match reader.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        buf.push(byte[0]);
                        // The DSR reply "\x1b[<row>;<col>R" always trails the OSC
                        // answer (same write, in-order replies) — its 'R' ends the
                        // read. The cap guards a garbage-spewing pty.
                        if byte[0] == b'R' || buf.len() > 256 {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(buf);
        })?;
    let buf = rx
        .recv_timeout(BG_REPLY_TIMEOUT)
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no OSC 11 / DSR reply"))?;
    Ok(parse_osc11_dark(&buf).unwrap_or(true))
}

/// Extracts the `rgb:`/`rgba:` payload of an OSC 11 reply and classifies it dark by
/// perceived luminance — the Go `darkHex` formula (host/background.go:118-128):
/// `(299·r + 587·g + 114·b) / 1000 < 128`.
fn parse_osc11_dark(buf: &[u8]) -> Option<bool> {
    let s = String::from_utf8_lossy(buf);
    let at = s.find("]11;")?;
    let rest = &s[at + 4..];
    let rest = rest
        .strip_prefix("rgb:")
        .or_else(|| rest.strip_prefix("rgba:"))?;
    let mut parts = rest.split('/');
    let r = component(parts.next()?)?;
    let g = component(parts.next()?)?;
    let b = component(parts.next()?)?;
    Some((299 * r + 587 * g + 114 * b) / 1000 < 128)
}

/// One `X11` color component (1–4 hex digits, possibly trailed by the sequence
/// terminator), scaled to 0..=255.
fn component(raw: &str) -> Option<u32> {
    let end = raw
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(raw.len());
    let hex = &raw[..end];
    if hex.is_empty() || hex.len() > 4 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (4 * hex.len())) - 1;
    Some(v * 255 / max)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{
        CSI_FOCUS_OFF, CSI_FOCUS_ON, OSC_PROGRESS_BUSY, OSC_PROGRESS_ERROR, OSC_PROGRESS_INPUT,
        OSC_PROGRESS_RESET, component, defuse_notify, notify_seq, parse_osc11_dark, progress_seq,
    };
    use crate::ui::facade::ProgressState;

    /// The wire table (T3 spec §2.5), pinned as literal bytes: these are what
    /// `charmbracelet/x/ansi` v0.11.7 emits and what every terminal parses.
    #[test]
    fn progress_and_focus_byte_tables() {
        assert_eq!(progress_seq(ProgressState::None), "\x1b]9;4;0\x07");
        assert_eq!(progress_seq(ProgressState::Busy), "\x1b]9;4;3\x07");
        assert_eq!(progress_seq(ProgressState::Input), "\x1b]9;4;4;100\x07");
        assert_eq!(progress_seq(ProgressState::Error), "\x1b]9;4;2;100\x07");
        assert_eq!(OSC_PROGRESS_RESET, progress_seq(ProgressState::None));
        assert_eq!(OSC_PROGRESS_BUSY, progress_seq(ProgressState::Busy));
        assert_eq!(OSC_PROGRESS_INPUT, progress_seq(ProgressState::Input));
        assert_eq!(OSC_PROGRESS_ERROR, progress_seq(ProgressState::Error));
        assert_eq!(CSI_FOCUS_ON, "\x1b[?1004h");
        assert_eq!(CSI_FOCUS_OFF, "\x1b[?1004l");
    }

    /// OSC 9 + a ringing BEL in ONE write; a `"4;"` payload gets the defusing space.
    #[test]
    fn notify_sequence_and_the_progress_collision() {
        assert_eq!(
            notify_seq("approval needed"),
            "\x1b]9;approval needed\x07\x07"
        );
        assert_eq!(defuse_notify("done"), "done");
        assert_eq!(defuse_notify("4; things to fix"), " 4; things to fix");
        assert_eq!(
            notify_seq(&defuse_notify("4; things to fix")),
            "\x1b]9; 4; things to fix\x07\x07"
        );
        // A digest merely CONTAINING "4;" is untouched — only the prefix collides.
        assert_eq!(defuse_notify("fix 4; now"), "fix 4; now");
    }

    #[test]
    fn parses_osc11_reply_light_and_dark() {
        // Light: white background, BEL-terminated, DSR reply trailing.
        let light = b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[24;1R";
        assert_eq!(parse_osc11_dark(light), Some(false));
        // Dark: near-black, ST-terminated.
        let dark = b"\x1b]11;rgb:1111/1111/1111\x1b\\\x1b[24;1R";
        assert_eq!(parse_osc11_dark(dark), Some(true));
        // DSR only (terminal ignored OSC 11): unknown → caller defaults dark.
        assert_eq!(parse_osc11_dark(b"\x1b[24;1R"), None);
    }

    #[test]
    fn scales_component_widths() {
        assert_eq!(component("ff"), Some(255));
        assert_eq!(component("ffff"), Some(255));
        assert_eq!(component("f"), Some(255));
        assert_eq!(component("8000"), Some(127));
        assert_eq!(component("0000\x07"), Some(0)); // terminator trailing the digits
        assert_eq!(component(""), None);
    }
}
