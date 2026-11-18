//! OSC title emission, the terminal's native progress + notification sequences, and the
//! OSC 11 background detect (`TUI_CONTRACTS` §5, T3 `T3_CONTRACTS` §5.7).
//!
//! The title rides crossterm's `SetTitle` command (a raw OSC 0 — the facade sanitized
//! the text, the loop emits on change only). `detect_background` is the one OSC 11
//! round-trip: it MUST run before any event loop claims stdin (the reply arrives as
//! terminal input), honors a 100 ms deadline, and defaults to dark — the safer side
//! for every adaptive shade (theme.go:36-38; Go used termenv the same way). Once the loop
//! has the terminal the same question is asked again with no round-trip at all: the loop
//! writes [`OSC11_QUERY`] and finds the answer in its own event stream
//! (`super::requery`, X-108).
//!
//! The progress and notification tables below are the bytes
//! `github.com/charmbracelet/x/ansi` v0.11.7 emits through bubbletea's renderer
//! (`cursed_renderer.go:779-800`), written verbatim so the wire is byte-identical to Go's.

use std::borrow::Cow;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::Read;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::AsFd as _;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use crossterm::queue;
use crossterm::terminal::SetTitle;

use crate::ui::facade::ProgressState;

/// A query's reply deadline (`TUI_CONTRACTS` §5: 100ms timeout).
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_millis(100);

/// The OSC 11 query: "what is your background colour?". The answer is the same sequence with
/// `rgb:RRRR/GGGG/BBBB` where the `?` is, ended the way the terminal likes (ST or BEL).
pub(crate) const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

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
/// Mechanism: the query is chased by a DSR 6n (`\x1b[6n`) on the same write — a read
/// terminator, not a cursor query the UI uses (the full screen never asks where the cursor is):
/// the read ends at the DSR reply's `R` whether or not OSC 11 was understood, and the 100ms
/// deadline only covers a terminal that answers neither. Nothing is left behind in that case:
/// the read waits on the descriptor with the deadline ([`exchange`]), so past it no thread is
/// parked on the tty and the next keystroke is the event loop's.
pub(crate) fn detect_background() -> bool {
    detect_dark().unwrap_or(true)
}

/// The fallible body: `Err` (no tty, raw-mode failure, timeout) → the dark default.
fn detect_dark() -> io::Result<bool> {
    let buf = round_trip(OSC11_QUERY)?;
    Ok(parse_osc11_dark(&buf).unwrap_or(true))
}

/// ONE query round-trip on the controlling terminal: `query` chased by a DSR 6n, written in raw
/// mode, and the reply bytes read back up to the DSR's `R` ([`detect_background`] says why that
/// ends every read), bounded by [`REPLY_TIMEOUT`]. `Err` when there is no terminal, raw mode
/// cannot be entered, or nothing answers in time — and, before any of it, when this process is not
/// the terminal's foreground process group: raw mode is a `tcsetattr`, which stops a background
/// process with `SIGTTOU` (measured: an hc-probing `iota -m` started by the shell tool, whose
/// commands get a process group of their own, hung stopped). Shared by the OSC 11 background detect and the
/// herdr-core probe (`host::hc::probe_tty`): both run before any event loop claims the input, one
/// after the other. It is never run once the loop has the terminal: it reads the tty itself, and a
/// key typed before the reply would be read here and lost (`super::requery` asks without reading).
///
/// The terminal is `/dev/tty`, opened read+write: the query goes out on it and the reply comes
/// back on it, which is why it is not stdin/stdout (either may be a pipe, and a redirect must not
/// turn the detect into a write to a file).
#[cfg(unix)]
pub(crate) fn round_trip(query: &[u8]) -> io::Result<Vec<u8>> {
    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    if nix::unistd::tcgetpgrp(&tty)? != nix::unistd::getpgrp() {
        return Err(io::Error::other(
            "not the terminal's foreground process group",
        ));
    }
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw {
        crossterm::terminal::enable_raw_mode()?;
    }
    let result = exchange(&mut tty, query, REPLY_TIMEOUT);
    if !was_raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    result
}

/// Windows has no `/dev/tty`, and its nearest pair — `CONIN$` / `CONOUT$` — cannot stand in
/// here: they are two handles where this needs one duplex file, the reply only arrives once
/// the console input mode carries VT sequences, and `conhost` answers OSC 11 with nothing at
/// all. So the round-trip is declined outright rather than attempted and left to time out:
/// [`detect_background`] reads this `Err` as "unknown" and takes its documented default,
/// dark — which is also what Windows Terminal ships with. A real detect belongs with a real
/// terminal to verify it against.
#[cfg(windows)]
pub(crate) fn round_trip(_query: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a terminal query needs a controlling terminal (no /dev/tty on Windows)",
    ))
}

/// Writes `query` + `DSR 6n` and reads the reply bytes off the tty up to the DSR's `R`, which
/// always trails the OSC answer (same write, in-order replies); the cap guards a garbage-spewing
/// pty.
///
/// Every byte is read only after the descriptor said one is there ([`super::tty::readable`], which
/// says why `select(2)` and not `poll(2)`), and the wait is bounded by what is left of `timeout` —
/// a signal that cuts it short is waited out, not taken for the deadline: when it runs out the function returns with nothing in
/// flight — no thread parked on the tty, no read that a later keystroke would complete. (Until
/// 2026-11-18 a detached thread did the reading, and on a terminal that answered nothing it stayed
/// parked and took the first key typed.) Nothing past the `R` is read either.
#[cfg(unix)]
fn exchange(tty: &mut File, query: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
    tty.write_all(&[query, b"\x1b[6n"].concat())?;
    tty.flush()?;
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ready = match super::tty::readable(tty.as_fd(), left) {
            // A signal (a resize at startup) is no answer: wait out what is left.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            ready => ready?,
        };
        if !ready {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no reply to the query or the DSR",
            ));
        }
        if tty.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.push(byte[0]);
        if byte[0] == b'R' || buf.len() > 256 {
            return Ok(buf);
        }
    }
}

/// Extracts the `rgb:`/`rgba:` payload of an OSC 11 reply and classifies it dark by
/// perceived luminance — the Go `darkHex` formula (host/background.go:118-128):
/// `(299·r + 587·g + 114·b) / 1000 < 128`.
pub(crate) fn parse_osc11_dark(buf: &[u8]) -> Option<bool> {
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

    /// A terminal stood in for by one end of a socket pair: `exchange` gets the other end as the
    /// file it would have opened.
    #[cfg(unix)]
    fn fake_tty() -> (std::fs::File, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        (
            std::fs::File::from(std::os::fd::OwnedFd::from(ours)),
            theirs,
        )
    }

    /// The timeout path, over a socket pair (what a real `/dev/tty` does with the wait is the
    /// `script(1)` test's to show, below): a terminal that answers nothing costs the deadline and
    /// leaves NOTHING behind. The byte the terminal sends afterwards — the user's next key — is still there to be
    /// read by whoever reads the tty next; a reader left parked by `exchange` (the detached thread
    /// it used to have) would have taken it, and this read would then find nothing.
    #[cfg(unix)]
    #[test]
    fn a_mute_terminal_times_out_and_keeps_the_next_byte() {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::io::{Read as _, Write as _};
        use std::os::fd::AsFd as _;
        use std::time::{Duration, Instant};

        let (mut tty, mut terminal) = fake_tty();
        let started = Instant::now();
        let err = super::exchange(&mut tty, super::OSC11_QUERY, super::REPLY_TIMEOUT).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let took = started.elapsed();
        // (The margin is for the clock's grain, not for the wait.)
        assert!(
            took >= super::REPLY_TIMEOUT * 9 / 10 && took < Duration::from_secs(2),
            "waited {took:?}"
        );
        // The query and its DSR chaser went out, in one write.
        let mut asked = [0u8; 64];
        let n = terminal.read(&mut asked).unwrap();
        assert_eq!(&asked[..n], b"\x1b]11;?\x1b\\\x1b[6n");

        terminal.write_all(b"x").unwrap();
        std::thread::sleep(Duration::from_millis(50)); // a parked reader would have it by now
        let mut fds = [PollFd::new(tty.as_fd(), PollFlags::POLLIN)];
        assert_eq!(
            poll(&mut fds, PollTimeout::ZERO).unwrap(),
            1,
            "the key was taken"
        );
        let mut key = [0u8; 8];
        assert_eq!(tty.read(&mut key).unwrap(), 1);
        assert_eq!(key[0], b'x');
    }

    /// A terminal that answers late but inside the deadline is heard, and the read stops at the
    /// DSR's `R`: what the terminal sent behind the reply in the same write stays unread.
    #[cfg(unix)]
    #[test]
    fn a_late_reply_is_read_up_to_the_dsr_and_no_further() {
        use std::io::{Read as _, Write as _};
        use std::time::Duration;

        let (mut tty, mut terminal) = fake_tty();
        let answer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            terminal
                .write_all(b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[24;1Rk")
                .unwrap();
            terminal
        });
        let reply = super::exchange(&mut tty, super::OSC11_QUERY, Duration::from_secs(5)).unwrap();
        assert_eq!(reply, b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[24;1R");
        assert_eq!(parse_osc11_dark(&reply), Some(false));
        let _terminal = answer.join().unwrap();
        let mut key = [0u8; 8];
        assert_eq!(tty.read(&mut key).unwrap(), 1);
        assert_eq!(key[0], b'k');
    }

    /// A signal that cuts the wait short is not the deadline: the round-trip waits out the rest and
    /// hears a reply that comes after it. The signal is aimed at the waiting thread (a resize at
    /// startup is SIGWINCH to the process; a thread that has it caught returns `EINTR` from
    /// `select` on macOS and Linux alike, `SA_RESTART` or not).
    #[cfg(unix)]
    #[test]
    fn a_signal_in_the_wait_is_waited_out() {
        use nix::sys::pthread::{pthread_kill, pthread_self};
        use nix::sys::signal::Signal;
        use std::io::{Read as _, Write as _};
        use std::time::Duration;
        use tokio::signal::unix::{SignalKind, signal};

        // A handler, so the signal interrupts instead of killing (tokio's, as any caught signal).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut caught = rt
            .block_on(async { signal(SignalKind::user_defined1()) })
            .unwrap();

        let (mut tty, mut terminal) = fake_tty();
        let waiting = pthread_self();
        let answer = std::thread::spawn(move || {
            let mut asked = [0u8; 64];
            let _ = terminal.read(&mut asked).unwrap(); // the query is out: the wait has begun
            std::thread::sleep(Duration::from_millis(20));
            pthread_kill(waiting, Signal::SIGUSR1).unwrap();
            std::thread::sleep(Duration::from_millis(60));
            terminal
                .write_all(b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[24;1R")
                .unwrap();
            terminal
        });
        let reply = super::exchange(&mut tty, super::OSC11_QUERY, Duration::from_millis(300));
        let _terminal = answer.join().unwrap();
        assert_eq!(reply.unwrap(), b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[24;1R");
        // …and the signal did arrive, inside the wait.
        assert!(
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(1), caught.recv()).await
            })
            .is_ok()
        );
    }

    /// Where the child of [`a_real_terminal_that_answers_nothing_costs_the_deadline_and_no_more`]
    /// reports what its round-trip came to.
    #[cfg(unix)]
    const REAL_TTY_OUT: &str = "IOTA_TEST_REAL_TTY_OUT";

    /// The child half: this test binary run again under `script(1)`
    /// ([`crate::ui::testutil::under_script`]). Does nothing unless it is that child.
    #[cfg(unix)]
    #[test]
    #[ignore = "the child of a_real_terminal_that_answers_nothing_costs_the_deadline_and_no_more"]
    fn real_tty_child() {
        let Some(out) = std::env::var_os(REAL_TTY_OUT) else {
            return;
        };
        let started = std::time::Instant::now();
        let result = super::round_trip(super::OSC11_QUERY);
        let took = started.elapsed().as_millis();
        let what = match result {
            Ok(bytes) => format!("answered {bytes:?}"),
            Err(e) => format!("{:?}", e.kind()),
        };
        crate::ui::testutil::report(std::path::Path::new(&out), &format!("{what} {took}"));
    }

    /// The timeout path on a REAL `/dev/tty`, which is the only place it can go wrong: a socket
    /// pair (the tests above) and a pty's own device both wait correctly under `poll(2)`, while
    /// macOS's `poll` on `/dev/tty` itself returns at once and left the read blocking until a key
    /// was typed. The terminal answers neither the OSC 11 query nor the DSR and nothing is typed:
    /// the round-trip must come back `TimedOut` after the deadline — not hang.
    #[cfg(unix)]
    #[test]
    fn a_real_terminal_that_answers_nothing_costs_the_deadline_and_no_more() {
        use crate::ui::testutil::{await_report, under_script};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("round-trip");
        let mut script = under_script(
            "ui::runtime::osc::tests::real_tty_child",
            &[(REAL_TTY_OUT, out.as_os_str())],
        );
        let said = await_report(&out, Duration::from_secs(20));
        let _ = script.kill();
        let _ = script.wait();
        let said = said.expect("the round-trip never came back: it is blocked on the tty");

        let (what, took) = said.rsplit_once(' ').unwrap();
        assert_eq!(what, "TimedOut", "{said}");
        let took: u128 = took.parse().unwrap();
        assert!(
            (90..2000).contains(&took),
            "waited {took} ms, want about 100"
        );
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
