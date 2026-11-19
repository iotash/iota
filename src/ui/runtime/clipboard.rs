//! Where a selection's text goes (`docs/design/fullscreen-tui.md` "Selection"): the local
//! clipboard tool, the tmux paste buffer, and OSC 52 for the terminal the user is looking at.
//!
//! - **Outside tmux: the local tool and OSC 52, both.** The tool (`pbcopy`; `wl-copy`, `xclip`,
//!   `xsel`; `clip`) writes the clipboard of the machine iota runs on; OSC 52
//!   (`ESC ] 52 ; c ; <base64> BEL`) asks the terminal to set its own, on the machine the user
//!   sits at. Which of the two the user is looking at cannot be read from the environment — a
//!   herdr-core terminal attached from another machine has no `SSH_*` and no `$TMUX` — so both
//!   are sent: on one machine the second write is the same text again, a terminal without OSC 52
//!   drops the sequence, and over SSH a remote host with a tool has its own clipboard written too.
//! - **Inside tmux** (`$TMUX`): `tmux load-buffer -w -` — the paste buffer `prefix ]` pastes, and
//!   `-w` asks tmux to hand it on to the outer terminal (OSC 52, as tmux's `set-clipboard`
//!   allows) — plus the local tool unless over SSH (`$SSH_CONNECTION`/`$SSH_TTY`). OSC 52 is
//!   written only when every one of them failed: tmux would swallow it otherwise.
//! - **Inside GNU screen** (`$STY`) as outside, the sequence wrapped for screen ([`screen_dcs`]):
//!   screen swallows a bare OSC 52 and prints most of a long one on the screen as text, but hands
//!   the contents of DCS strings to the outer terminal as they are (hterm's `osc52.sh`, vim-oscyank).
//!
//! Each program runs as a child of its own, its text fed by a thread of its own — a clipboard tool
//! that hangs stalls neither the loop nor the other programs — and [`Clipboard::poll`] collects
//! their exits; one that has not exited within [`COPY_DEADLINE`] counts as failed and is killed.
//! The sequence is written at once, in order with the frame. A copy is done
//! when at least one way ran: a tool that succeeded, or the sequence written (OSC 52 cannot
//! report). The outcome names the ways ([`Copied::Done`]), and the status row says them.
//! A selection whose sequence would pass [`OSC52_MAX`] is not sent: terminals truncate or drop one
//! that long, and a megabyte written with the frame stalls a slow link — a tool may still take it,
//! and the outcome then says the sequence was left out.

use std::io::{self, Write as _};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;

/// The longest OSC 52 sequence sent (bytes, base64 and framing included): about 73 KB of text.
/// hterm and tmux keep 100 000 bytes of an OSC 52; past it most terminals drop or cut it.
pub(crate) const OSC52_MAX: usize = 100_000;

/// How long a copy waits for its programs: one that has not exited by then (an `xclip` whose
/// X forwarding is gone hangs) counts as failed and is killed, and the copy settles without it.
pub(crate) const COPY_DEADLINE: Duration = Duration::from_secs(2);

/// One way a copied text leaves iota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPath {
    /// The platform clipboard tool ([`copy_to_clipboard`]'s first one on `PATH`).
    Local,
    /// `tmux load-buffer -w -`.
    Tmux,
    /// An OSC 52 sequence written to the terminal ([`osc52`]) alongside the programs.
    Osc52,
}

/// The name the status row gives OSC 52 among the ways a copy went.
pub(crate) const OSC52_WAY: &str = "OSC 52";

/// The multiplexer a session runs inside, which takes OSC 52 from iota or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mux {
    /// tmux (`$TMUX`): its buffer is a way of its own, and it swallows a bare OSC 52.
    Tmux,
    /// GNU screen (`$STY`): it passes OSC 52 on only inside DCS strings ([`screen_dcs`]).
    Screen,
}

/// What became of a copy's OSC 52 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc {
    /// Not offered (inside a multiplexer while a program may take the copy).
    Unsent,
    /// Written to the terminal.
    Sent,
    /// Offered, and longer than [`OSC52_MAX`].
    TooLarge,
}

/// What became of a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Copied {
    /// At least one way ran: the programs that succeeded (by name — `pbcopy`, `tmux`), then
    /// [`OSC52_WAY`] when the sequence was written; `osc_too_large` when it would have gone too
    /// but passed [`OSC52_MAX`] — the terminal the user sits at did not get it.
    Done {
        chars: usize,
        ways: Vec<String>,
        osc_too_large: bool,
    },
    /// No program took it (or there was none) and the text is too long for OSC 52.
    TooLarge { chars: usize },
}

/// A step of a copy: the OSC 52 sequence to write now, and the outcome once it is known.
pub(crate) type Step = (Option<String>, Option<Copied>);

/// One program of a copy.
#[derive(Debug)]
struct Run {
    /// Its name among the ways ([`way`]).
    name: String,
    /// The child, until it has exited (or never started).
    child: Option<Child>,
    /// It exited successfully.
    ok: bool,
}

/// A copy whose programs are still running.
#[derive(Debug)]
struct Pending {
    /// The text, for the OSC 52 fall-through inside tmux (`None`: no fall-through).
    fallback: Option<String>,
    chars: usize,
    /// The sequence offered when the copy started.
    osc: Osc,
    /// When the programs started, for [`COPY_DEADLINE`].
    started: Instant,
    /// The programs, in the order of the ways.
    runs: Vec<Run>,
}

/// A copy dropped with programs still running — settled past [`COPY_DEADLINE`], replaced by the
/// next copy, or left when the session ends — kills and reaps them, so none outlives it.
impl Drop for Pending {
    fn drop(&mut self) {
        for mut child in self.runs.iter_mut().filter_map(|r| r.child.take()) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Where the selections of this session go: the programs a copy is piped into, whether OSC 52 goes
/// with them, and the modifier the terminal takes for its own selection while iota has the mouse.
#[derive(Debug)]
pub(crate) struct Clipboard {
    /// The programs a copy is piped into (the local tool, tmux's `load-buffer`).
    commands: Vec<Vec<String>>,
    /// The multiplexer the session runs inside: OSC 52 is written with every copy except inside
    /// tmux, where it goes only when every program failed; inside screen it is wrapped.
    mux: Option<Mux>,
    /// A copy the programs are still running.
    pending: Option<Pending>,
    /// `Shift`, or `Option` in iTerm2 (whose bypass key it is).
    pub(crate) native: &'static str,
    /// The texts a test's clipboard was handed (`None`: a live one, which runs the paths); shared,
    /// so a test still reads it after the terminal moved into the loop's thread.
    #[cfg(test)]
    pub(crate) recorded: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
}

impl Clipboard {
    /// The process's own: read from its environment once (`$TMUX`, `$STY`,
    /// `$SSH_CONNECTION`/`$SSH_TTY`, `$TERM_PROGRAM`/`$LC_TERMINAL`, the local tool on `$PATH`).
    pub(crate) fn from_env() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let is = |name: &str, want: &str| std::env::var(name).is_ok_and(|v| v == want);
        let ssh = set("SSH_CONNECTION") || set("SSH_TTY");
        let iterm = is("TERM_PROGRAM", "iTerm.app") || is("LC_TERMINAL", "iTerm2");
        let mux = if set("TMUX") {
            Some(Mux::Tmux)
        } else if set("STY") {
            Some(Mux::Screen)
        } else {
            None
        };
        let paths = paths(mux, ssh);
        let commands = paths
            .iter()
            .filter_map(|p| match p {
                CopyPath::Local => local_tool(),
                CopyPath::Tmux => Some(argv(&["tmux", "load-buffer", "-w", "-"])),
                CopyPath::Osc52 => None,
            })
            .collect();
        Self::with_commands(commands, mux, if iterm { "Option" } else { "Shift" })
    }

    /// A clipboard piping into `commands`, with OSC 52 as `mux` takes it (a test's: programs that
    /// succeed, fail or hang on cue).
    pub(crate) fn with_commands(
        commands: Vec<Vec<String>>,
        mux: Option<Mux>,
        native: &'static str,
    ) -> Self {
        Self {
            commands,
            mux,
            pending: None,
            native,
            #[cfg(test)]
            recorded: None,
        }
    }

    /// A test's: records what it is handed and runs nothing (never the developer's clipboard).
    #[cfg(test)]
    pub(crate) fn recording() -> Self {
        Self {
            recorded: Some(std::sync::Arc::default()),
            ..Self::with_commands(Vec::new(), None, "Shift")
        }
    }

    /// Starts copying `text`: the OSC 52 sequence to write now (when it goes with every copy and is
    /// within [`OSC52_MAX`]), and the outcome when nothing is left to run; otherwise the programs
    /// are started and [`Clipboard::poll`] has the outcome.
    pub(crate) fn start(&mut self, text: String) -> Step {
        let chars = text.chars().count();
        #[cfg(test)]
        if let Some(rec) = &self.recorded {
            crate::sync::lock(rec).push(text);
            let ways = vec!["recorded".to_owned()];
            let osc_too_large = false;
            return (
                None,
                Some(Copied::Done {
                    chars,
                    ways,
                    osc_too_large,
                }),
            );
        }
        let in_tmux = self.mux == Some(Mux::Tmux);
        let (seq, osc) = if in_tmux {
            (None, Osc::Unsent)
        } else {
            offer(&text, self.mux)
        };
        if self.commands.is_empty() {
            return (seq, Some(outcome(chars, Vec::new(), osc)));
        }
        let shared: Arc<str> = Arc::from(text.as_str());
        let runs = self
            .commands
            .iter()
            .map(|c| Run {
                name: way(&c[0]),
                child: feed(c, &shared),
                ok: false,
            })
            .collect();
        self.pending = Some(Pending {
            fallback: in_tmux.then_some(text),
            chars,
            osc,
            started: Instant::now(),
            runs,
        });
        (seq, None)
    }

    /// The outcome of the copy the programs are running, once every one has exited or the copy is
    /// past [`COPY_DEADLINE`] (those still running are killed and count as failed): the programs
    /// that succeeded and the sequence written at the start; inside tmux, when none succeeded, the
    /// OSC 52 fall-through (its sequence to write now).
    pub(crate) fn poll(&mut self) -> Step {
        let Some(mut p) = self.pending.take() else {
            return (None, None);
        };
        for r in &mut p.runs {
            if let Some(child) = &mut r.child {
                match child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(status)) => {
                        r.ok = status.success();
                        r.child = None;
                    }
                    Err(_) => r.child = None,
                }
            }
        }
        if p.runs.iter().any(|r| r.child.is_some()) && p.started.elapsed() < COPY_DEADLINE {
            self.pending = Some(p);
            return (None, None);
        }
        let took: Vec<String> = p
            .runs
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.name.clone())
            .collect();
        let (seq, osc) = match &p.fallback {
            Some(text) if took.is_empty() => offer(text, self.mux),
            _ => (None, p.osc),
        };
        // `p` is dropped here: a program still running past the deadline is killed and reaped.
        (seq, Some(outcome(p.chars, took, osc)))
    }

    /// Whether a copy's programs are still running.
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// The OSC 52 sequence for `text` to write — inside screen wrapped for it — or nothing when the
/// sequence would pass [`OSC52_MAX`].
fn offer(text: &str, mux: Option<Mux>) -> (Option<String>, Osc) {
    let seq = osc52(text);
    if seq.len() > OSC52_MAX {
        (None, Osc::TooLarge)
    } else if mux == Some(Mux::Screen) {
        (Some(screen_dcs(&seq)), Osc::Sent)
    } else {
        (Some(seq), Osc::Sent)
    }
}

/// The bytes of one DCS string screen passes on ([`screen_dcs`]), as hterm and vim-oscyank cut
/// them (screen keeps a string of a bounded length).
const SCREEN_CHUNK: usize = 76;

/// `seq` (ASCII) for GNU screen: cut into [`SCREEN_CHUNK`]-byte pieces, each in a DCS string
/// `ESC P <piece> ESC \`, whose contents screen hands to the outer terminal as they are — so the
/// terminal reads the pieces as the one sequence.
fn screen_dcs(seq: &str) -> String {
    let mut out = String::with_capacity(seq.len() + seq.len().div_ceil(SCREEN_CHUNK) * 4);
    for piece in seq.as_bytes().chunks(SCREEN_CHUNK) {
        out.push_str("\x1bP");
        out.push_str(&String::from_utf8_lossy(piece));
        out.push_str("\x1b\\");
    }
    out
}

/// A copy of `chars` characters that the programs named in `took` succeeded at, its sequence
/// as `osc` left it: done when any way ran, else too large (the one way that cannot fail refused
/// it).
fn outcome(chars: usize, mut ways: Vec<String>, osc: Osc) -> Copied {
    if osc == Osc::Sent {
        ways.push(OSC52_WAY.to_owned());
    }
    if ways.is_empty() {
        Copied::TooLarge { chars }
    } else {
        let osc_too_large = osc == Osc::TooLarge;
        Copied::Done {
            chars,
            ways,
            osc_too_large,
        }
    }
}

/// The name a program goes by among the ways a copy went: its file name (`pbcopy`, `tmux`).
fn way(program: &str) -> String {
    std::path::Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// The paths for a session inside a multiplexer or not, over SSH or not (module docs): inside
/// tmux its buffer, and the local tool unless over SSH, OSC 52 being the fall-through; anywhere
/// else (screen too) the local tool and OSC 52, SSH or not.
pub(crate) fn paths(mux: Option<Mux>, ssh: bool) -> Vec<CopyPath> {
    match mux {
        Some(Mux::Tmux) => (!ssh)
            .then_some(CopyPath::Local)
            .into_iter()
            .chain([CopyPath::Tmux])
            .collect(),
        None | Some(Mux::Screen) => vec![CopyPath::Local, CopyPath::Osc52],
    }
}

/// The OSC 52 sequence that sets the clipboard (`c`) to `text`, BEL-terminated (what tmux,
/// Ghostty, `iTerm2`, kitty and `WezTerm` all accept).
pub(crate) fn osc52(text: &str) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{data}\x07")
}

/// The platform's clipboard tools, in the order they are tried.
fn local_tools() -> &'static [&'static [&'static str]] {
    if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else if cfg!(target_os = "windows") {
        &[&["clip"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    }
}

/// The first of the [`local_tools`] on `PATH`, as the command a copy is piped into.
fn local_tool() -> Option<Vec<String>> {
    let path = std::env::var_os("PATH")?;
    local_tools()
        .iter()
        .find(|t| std::env::split_paths(&path).any(|dir| dir.join(t[0]).is_file()))
        .map(|t| argv(t))
}

/// Pipes text into the platform clipboard tool (clipboard.go:14-39, the v1 promptui
/// implementation ported): `pbcopy` on macOS; `clip` on Windows; `wl-copy` /
/// `xclip -selection clipboard` / `xsel --clipboard --input` elsewhere. A tool that
/// is absent (spawn fails with `NotFound`) falls through to the next candidate; none
/// found is the error `"no clipboard tool found"`.
pub(crate) fn copy_to_clipboard(text: &str) -> io::Result<()> {
    for c in local_tools() {
        match pipe(c, text) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            done => return done,
        }
    }
    Err(io::Error::other("no clipboard tool found"))
}

/// Starts `argv` with `text` fed to its stdin by a thread (a child that never reads blocks the
/// thread alone, until it exits or is killed); `None` when it could not start.
fn feed(argv: &[String], text: &Arc<str>) -> Option<Child> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let text = Arc::clone(text);
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    Some(child)
}

/// Runs `argv` with `text` on its stdin; `NotFound` when the program is not there.
fn pipe(argv: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{} failed: {status}", argv[0])))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const TMUX: Option<Mux> = Some(Mux::Tmux);

    /// Inside tmux its buffer, and the local tool unless over SSH — OSC 52 the fall-through;
    /// anywhere else, screen too, the local tool and OSC 52 both, over SSH or not.
    #[test]
    fn the_paths_follow_where_the_session_runs() {
        use CopyPath::{Local, Osc52, Tmux};
        assert_eq!(paths(None, false), vec![Local, Osc52]);
        assert_eq!(paths(None, true), vec![Local, Osc52]);
        assert_eq!(paths(Some(Mux::Tmux), false), vec![Local, Tmux]);
        assert_eq!(paths(Some(Mux::Tmux), true), vec![Tmux]);
        assert_eq!(paths(Some(Mux::Screen), false), vec![Local, Osc52]);
        assert_eq!(paths(Some(Mux::Screen), true), vec![Local, Osc52]);
    }

    /// OSC 52 carries the text whole — UTF-8, newlines and all — as base64 for the `c` selection.
    #[test]
    fn osc52_is_the_texts_base64_for_the_clipboard() {
        let seq = osc52("héllo\nwörld");
        assert!(seq.starts_with("\x1b]52;c;") && seq.ends_with('\x07'));
        let data = &seq["\x1b]52;c;".len()..seq.len() - 1];
        let back = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(String::from_utf8(back).unwrap(), "héllo\nwörld");
    }

    /// Waits for the programs' outcome.
    fn finish(c: &mut Clipboard) -> Step {
        for _ in 0..500 {
            let step = c.poll();
            if step.1.is_some() {
                assert!(!c.pending());
                return step;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the copy never finished");
    }

    fn ok() -> Vec<String> {
        argv(&["sh", "-c", "cat >/dev/null"])
    }

    fn fail() -> Vec<String> {
        argv(&["sh", "-c", "cat >/dev/null; exit 1"])
    }

    fn done(chars: usize, ways: &[&str]) -> Copied {
        Copied::Done {
            chars,
            ways: argv(ways),
            osc_too_large: false,
        }
    }

    /// Outside tmux: the sequence is written at once, the tool runs too, and the outcome names both.
    #[test]
    fn outside_tmux_the_tool_and_osc52_both_run() {
        let mut c = Clipboard::with_commands(vec![ok()], None, "Shift");
        assert_eq!(c.start("abc".to_owned()), (Some(osc52("abc")), None));
        assert!(c.pending());
        assert_eq!(finish(&mut c), (None, Some(done(3, &["sh", "OSC 52"]))));
    }

    /// A fake `pbcopy` (a script by that name, at a full path) is named by its file name.
    #[cfg(unix)]
    #[test]
    fn a_tool_is_named_by_its_file_name() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("pbcopy");
        let out = dir.path().join("got");
        std::fs::write(&tool, format!("#!/bin/sh\ncat >'{}'\n", out.display())).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut c = Clipboard::with_commands(vec![argv(&[tool.to_str().unwrap()])], None, "Shift");
        assert_eq!(c.start("héllo".to_owned()), (Some(osc52("héllo")), None));
        assert_eq!(finish(&mut c), (None, Some(done(5, &["pbcopy", "OSC 52"]))));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "héllo");
    }

    /// A tool that fails (or is not there) outside tmux takes nothing from the copy: the sequence
    /// went at the start, and the copy is done by OSC 52 alone.
    #[test]
    fn a_failing_tool_still_sends_osc52() {
        let mut c =
            Clipboard::with_commands(vec![fail(), argv(&["no-such-tool-iota"])], None, "Shift");
        assert_eq!(c.start("abc".to_owned()), (Some(osc52("abc")), None));
        assert_eq!(finish(&mut c), (None, Some(done(3, &["OSC 52"]))));
    }

    /// No tool outside tmux: OSC 52 alone, at once.
    #[test]
    fn with_no_tool_osc52_alone_is_at_once() {
        let mut c = Clipboard::with_commands(Vec::new(), None, "Shift");
        assert_eq!(
            c.start("hi".to_owned()),
            (Some(osc52("hi")), Some(done(2, &["OSC 52"])))
        );
        assert!(!c.pending());
    }

    /// Inside tmux, as before: no sequence while a program may take the copy — tmux hands it on —
    /// and the OSC 52 fall-through only when every program failed.
    #[test]
    fn inside_tmux_osc52_is_only_the_fall_through() {
        let mut c = Clipboard::with_commands(vec![fail(), ok()], TMUX, "Shift");
        assert_eq!(c.start("abc".to_owned()), (None, None));
        assert_eq!(
            finish(&mut c),
            (None, Some(done(3, &["sh"]))),
            "one took it"
        );
        let mut c = Clipboard::with_commands(vec![ok(), ok()], TMUX, "Shift");
        c.start("abc".to_owned());
        assert_eq!(
            finish(&mut c),
            (None, Some(done(3, &["sh", "sh"]))),
            "both took it"
        );
        let mut c =
            Clipboard::with_commands(vec![fail(), argv(&["no-such-tool-iota"])], TMUX, "Shift");
        assert_eq!(c.start("abc".to_owned()), (None, None));
        assert_eq!(
            finish(&mut c),
            (Some(osc52("abc")), Some(done(3, &["OSC 52"])))
        );
    }

    /// A text too long for OSC 52 is never sent as one; a tool may still take it, and then the
    /// outcome names the tool alone and says the sequence was left out — outside a multiplexer,
    /// where it would have gone. With no tool to take it, it is too large.
    #[test]
    fn too_large_for_osc52_is_the_tools_alone_or_not_copied() {
        let big = || "x".repeat(OSC52_MAX);
        assert!(
            osc52(&"z".repeat(70_000)).len() <= OSC52_MAX,
            "70 KB still goes"
        );
        let mut c = Clipboard::with_commands(vec![ok()], None, "Shift");
        assert_eq!(c.start(big()), (None, None));
        let left_out = Copied::Done {
            chars: OSC52_MAX,
            ways: argv(&["sh"]),
            osc_too_large: true,
        };
        assert_eq!(finish(&mut c), (None, Some(left_out)));
        let mut c = Clipboard::with_commands(vec![ok()], TMUX, "Shift");
        c.start(big());
        assert_eq!(
            finish(&mut c),
            (None, Some(done(OSC52_MAX, &["sh"]))),
            "inside tmux no sequence was to go"
        );
        let mut c = Clipboard::with_commands(vec![fail()], None, "Shift");
        c.start(big());
        let too = Some(Copied::TooLarge { chars: OSC52_MAX });
        assert_eq!(finish(&mut c), (None, too.clone()));
        let mut c = Clipboard::with_commands(Vec::new(), None, "Shift");
        assert_eq!(c.start(big()), (None, too.clone()));
        let mut c = Clipboard::with_commands(vec![fail()], TMUX, "Shift");
        c.start(big());
        assert_eq!(
            finish(&mut c),
            (None, too),
            "inside tmux: the fall-through refuses it too"
        );
    }

    /// Inside screen the sequence goes with the tool, at once, cut into DCS strings of 76 bytes
    /// whose contents are the sequence again.
    #[test]
    fn inside_screen_osc52_goes_in_dcs_pieces() {
        let text = "y".repeat(200);
        let mut c = Clipboard::with_commands(vec![ok()], Some(Mux::Screen), "Shift");
        let (seq, none) = c.start(text.clone());
        assert_eq!(none, None);
        let seq = seq.unwrap();
        let pieces: Vec<&str> = seq
            .strip_prefix("\x1bP")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap()
            .split("\x1b\\\x1bP")
            .collect();
        assert!(pieces.len() > 1 && pieces.iter().all(|p| p.len() <= SCREEN_CHUNK));
        assert_eq!(pieces.concat(), osc52(&text));
        assert_eq!(finish(&mut c), (None, Some(done(200, &["sh", "OSC 52"]))));
    }

    /// A tool that hangs settles the copy at [`COPY_DEADLINE`] as one that failed, and is killed:
    /// outside tmux the sequence alone, inside it the fall-through.
    #[cfg(unix)]
    #[test]
    fn a_hung_tool_settles_at_the_deadline() {
        let hang = || argv(&["sh", "-c", "exec sleep 30"]);
        let mut c = Clipboard::with_commands(vec![hang()], None, "Shift");
        assert_eq!(c.start("abc".to_owned()), (Some(osc52("abc")), None));
        let pid = pending_pid(&c);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(c.poll(), (None, None), "within the deadline it waits");
        past(&mut c);
        let t0 = Instant::now();
        assert_eq!(c.poll(), (None, Some(done(3, &["OSC 52"]))));
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "the hung child was killed, not waited out: {:?}",
            t0.elapsed()
        );
        assert!(!c.pending());
        assert!(!alive(pid), "the hung child is gone");
        let mut c = Clipboard::with_commands(vec![hang()], TMUX, "Shift");
        assert_eq!(c.start("abc".to_owned()), (None, None));
        past(&mut c);
        assert_eq!(c.poll(), (Some(osc52("abc")), Some(done(3, &["OSC 52"]))));
    }

    /// A hung first program does not hold back the second: inside tmux a hung `pbcopy` and a
    /// `tmux` that succeeds settle at the deadline as the second one's way, with no fall-through —
    /// not before the deadline, and not past it.
    #[cfg(unix)]
    #[test]
    fn a_hung_program_does_not_starve_the_next() {
        let mut c = Clipboard::with_commands(
            vec![argv(&["sh", "-c", "exec sleep 30"]), ok()],
            TMUX,
            "Shift",
        );
        assert_eq!(c.start("abc".to_owned()), (None, None));
        let t0 = Instant::now();
        while c.pending.as_ref().unwrap().runs[1].child.is_some() {
            assert_eq!(
                c.poll(),
                (None, None),
                "the hung one is still within the deadline"
            );
            assert!(
                t0.elapsed() < Duration::from_secs(1),
                "the second never ran"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(c.pending(), "it waits for the hung one until the deadline");
        past(&mut c);
        assert_eq!(c.poll(), (None, Some(done(3, &["sh"]))));
        assert!(!c.pending());
    }

    /// A copy started while another is pending kills and reaps the first one's programs, and so
    /// does dropping the clipboard (the session's end) with a copy pending.
    #[cfg(unix)]
    #[test]
    fn a_new_copy_and_the_end_leave_no_program_behind() {
        let hang = || argv(&["sh", "-c", "exec sleep 30"]);
        let mut c = Clipboard::with_commands(vec![hang()], None, "Shift");
        c.start("abc".to_owned());
        let first = pending_pid(&c);
        c.start("def".to_owned());
        assert!(!alive(first), "the first copy's program is gone");
        let second = pending_pid(&c);
        assert!(alive(second), "the second copy's program runs");
        drop(c);
        assert!(
            !alive(second),
            "the pending copy's program is gone at the end"
        );
    }

    /// The pid of a pending copy's first program.
    #[cfg(unix)]
    fn pending_pid(c: &Clipboard) -> u32 {
        c.pending.as_ref().unwrap().runs[0]
            .child
            .as_ref()
            .unwrap()
            .id()
    }

    /// Whether `pid` is a live process (`kill -0`).
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// Moves a pending copy's start back past [`COPY_DEADLINE`].
    #[cfg(unix)]
    fn past(c: &mut Clipboard) {
        let p = c.pending.as_mut().unwrap();
        p.started = Instant::now().checked_sub(COPY_DEADLINE).unwrap();
    }
}
