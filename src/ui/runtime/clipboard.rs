//! Where a selection's text goes (Claude Code's three paths, `docs/design/fullscreen-tui.md`
//! "Selection"): the local clipboard tool, the tmux paste buffer, and OSC 52 for a remote session.
//!
//! - **Local** (`pbcopy`; `wl-copy`, `xclip`, `xsel`; `clip`) — on the machine iota runs on, so
//!   only when that machine is the user's: not over SSH, where it would fill the remote host's
//!   clipboard (or, headless, find no tool at all).
//! - **tmux** (`tmux load-buffer -w -`) — inside tmux (`$TMUX`): the paste buffer `prefix ]`
//!   pastes, and `-w` asks tmux to hand it on to the outer terminal's clipboard as well (OSC 52,
//!   as tmux's `set-clipboard` allows), which is what reaches the user's machine over SSH.
//! - **OSC 52** (`ESC ] 52 ; c ; <base64> BEL`) — over SSH outside tmux: the terminal on the
//!   user's side sets its own clipboard. Also the fallback when no local tool is found.
//!
//! The tools run on a thread of their own — a clipboard tool that hangs never stalls the loop —
//! and report back ([`Clipboard::poll`]): a copy is said to be done only when one of them
//! succeeded. When every tool fails (an `xclip` with no `$DISPLAY`, a `pbcopy` that errors) the
//! copy falls through to OSC 52, the one way left; OSC 52 itself cannot report, so the status row
//! names it. A selection whose sequence would pass [`OSC52_MAX`] is not sent: terminals truncate or
//! drop one that long, and a megabyte written with the frame stalls a slow link.

use std::io::{self, Write as _};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use base64::Engine as _;

/// The longest OSC 52 sequence sent (bytes, base64 and framing included): about 73 KB of text.
/// hterm and tmux keep 100 000 bytes of an OSC 52; past it most terminals drop or cut it.
pub(crate) const OSC52_MAX: usize = 100_000;

/// One way a copied text leaves iota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPath {
    /// The platform clipboard tool ([`copy_to_clipboard`]'s first one on `PATH`).
    Local,
    /// `tmux load-buffer -w -`.
    Tmux,
    /// An OSC 52 sequence written to the terminal ([`osc52`]).
    Osc52,
}

/// What became of a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Copied {
    /// A tool took it (`osc: false`), or the OSC 52 sequence was written (`osc: true`).
    Done { chars: usize, osc: bool },
    /// Every tool failed (or there was none) and the text is too long for OSC 52.
    TooLarge { chars: usize },
}

/// Where the selections of this session go: the programs a copy is piped into, and the modifier
/// the terminal takes for its own selection while iota has the mouse.
#[derive(Debug)]
pub(crate) struct Clipboard {
    /// The programs a copy is piped into (the local tool, tmux's `load-buffer`); one of them
    /// succeeding is a copy. None: OSC 52 is the way (over SSH outside tmux, or no tool here).
    commands: Vec<Vec<String>>,
    /// A copy the tools are still running, with its text for the OSC 52 fall-through.
    pending: Option<(String, mpsc::Receiver<bool>)>,
    /// `Shift`, or `Option` in iTerm2 (whose bypass key it is).
    pub(crate) native: &'static str,
    /// The texts a test's clipboard was handed (`None`: a live one, which runs the paths); shared,
    /// so a test still reads it after the terminal moved into the loop's thread.
    #[cfg(test)]
    pub(crate) recorded: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
}

impl Clipboard {
    /// The process's own: read from its environment once (`$TMUX`, `$SSH_CONNECTION`/`$SSH_TTY`,
    /// `$TERM_PROGRAM`/`$LC_TERMINAL`, the local tool on `$PATH`).
    pub(crate) fn from_env() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let is = |name: &str, want: &str| std::env::var(name).is_ok_and(|v| v == want);
        let ssh = set("SSH_CONNECTION") || set("SSH_TTY");
        let iterm = is("TERM_PROGRAM", "iTerm.app") || is("LC_TERMINAL", "iTerm2");
        let commands = paths(set("TMUX"), ssh)
            .into_iter()
            .filter_map(|p| match p {
                CopyPath::Local => local_tool(),
                CopyPath::Tmux => Some(argv(&["tmux", "load-buffer", "-w", "-"])),
                CopyPath::Osc52 => None,
            })
            .collect();
        Self::with_commands(commands, if iterm { "Option" } else { "Shift" })
    }

    /// A clipboard piping into `commands` (a test's: programs that succeed or fail on cue).
    pub(crate) fn with_commands(commands: Vec<Vec<String>>, native: &'static str) -> Self {
        Self {
            commands,
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
            ..Self::with_commands(Vec::new(), "Shift")
        }
    }

    /// Starts copying `text`. With no program to run the outcome is at hand — the OSC 52 sequence
    /// to write, if it is within [`OSC52_MAX`]; otherwise the programs run on a thread and
    /// [`Clipboard::poll`] has the outcome.
    pub(crate) fn start(&mut self, text: String) -> Option<(Copied, Option<String>)> {
        #[cfg(test)]
        if let Some(rec) = &self.recorded {
            let chars = text.chars().count();
            crate::sync::lock(rec).push(text);
            return Some((Copied::Done { chars, osc: false }, None));
        }
        if self.commands.is_empty() {
            return Some(osc_outcome(&text));
        }
        let (tx, rx) = mpsc::channel();
        let commands = self.commands.clone();
        let owned = text.clone();
        std::thread::spawn(move || {
            let mut ok = false;
            for c in &commands {
                let c: Vec<&str> = c.iter().map(String::as_str).collect();
                ok |= pipe(&c, &owned).is_ok();
            }
            let _ = tx.send(ok);
        });
        self.pending = Some((text, rx));
        None
    }

    /// The outcome of the copy the programs are running, once they are done: a copy when one of
    /// them succeeded, else the OSC 52 fall-through.
    pub(crate) fn poll(&mut self) -> Option<(Copied, Option<String>)> {
        let (text, rx) = self.pending.take()?;
        match rx.try_recv() {
            Ok(true) => Some((
                Copied::Done {
                    chars: text.chars().count(),
                    osc: false,
                },
                None,
            )),
            Ok(false) | Err(mpsc::TryRecvError::Disconnected) => Some(osc_outcome(&text)),
            Err(mpsc::TryRecvError::Empty) => {
                self.pending = Some((text, rx));
                None
            }
        }
    }

    /// Whether a copy's programs are still running.
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// OSC 52 for `text`: the sequence, or nothing when it would pass [`OSC52_MAX`].
fn osc_outcome(text: &str) -> (Copied, Option<String>) {
    let chars = text.chars().count();
    let seq = osc52(text);
    if seq.len() > OSC52_MAX {
        (Copied::TooLarge { chars }, None)
    } else {
        (Copied::Done { chars, osc: true }, Some(seq))
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// The paths for a session inside tmux or not, over SSH or not (module docs).
pub(crate) fn paths(tmux: bool, ssh: bool) -> Vec<CopyPath> {
    let mut out = Vec::new();
    if !ssh {
        out.push(CopyPath::Local);
    }
    if tmux {
        out.push(CopyPath::Tmux);
    } else if ssh {
        out.push(CopyPath::Osc52);
    }
    out
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

    /// The three paths by where iota runs: the local tool unless over SSH, tmux's buffer inside
    /// tmux, OSC 52 over SSH outside it.
    #[test]
    fn the_paths_follow_where_the_session_runs() {
        use CopyPath::{Local, Osc52, Tmux};
        assert_eq!(paths(false, false), vec![Local]);
        assert_eq!(paths(true, false), vec![Local, Tmux]);
        assert_eq!(paths(false, true), vec![Osc52]);
        assert_eq!(paths(true, true), vec![Tmux]);
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
    fn outcome(c: &mut Clipboard) -> (Copied, Option<String>) {
        for _ in 0..500 {
            if let Some(o) = c.poll() {
                return o;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the copy never finished");
    }

    /// A copy is done only when a program took it; when every one fails it falls through to OSC
    /// 52, which the outcome names — never a claim that the clipboard has it.
    #[test]
    fn a_failed_tool_falls_through_to_osc52_and_says_so() {
        let ok = argv(&["sh", "-c", "cat >/dev/null"]);
        let fail = argv(&["sh", "-c", "cat >/dev/null; exit 1"]);
        let mut c = Clipboard::with_commands(vec![fail.clone(), ok], "Shift");
        assert!(c.start("abc".to_owned()).is_none() && c.pending());
        assert_eq!(
            outcome(&mut c),
            (
                Copied::Done {
                    chars: 3,
                    osc: false
                },
                None
            ),
            "one took it"
        );
        assert!(!c.pending());
        let mut c =
            Clipboard::with_commands(vec![fail.clone(), argv(&["no-such-tool-iota"])], "Shift");
        c.start("abc".to_owned());
        assert_eq!(
            outcome(&mut c),
            (
                Copied::Done {
                    chars: 3,
                    osc: true
                },
                Some(osc52("abc"))
            )
        );
        let mut c = Clipboard::with_commands(vec![fail], "Shift");
        c.start("x".repeat(OSC52_MAX));
        assert_eq!(
            outcome(&mut c),
            (Copied::TooLarge { chars: OSC52_MAX }, None),
            "too long for OSC 52: not sent"
        );
    }

    /// With no program to run (over SSH outside tmux) the outcome is at once: OSC 52, capped.
    #[test]
    fn osc52_alone_is_at_once_and_capped() {
        let mut c = Clipboard::with_commands(Vec::new(), "Shift");
        assert_eq!(
            c.start("hi".to_owned()),
            Some((
                Copied::Done {
                    chars: 2,
                    osc: true
                },
                Some(osc52("hi"))
            ))
        );
        let big = "y".repeat(80_000);
        assert_eq!(
            c.start(big),
            Some((Copied::TooLarge { chars: 80_000 }, None))
        );
        assert!(
            osc52(&"z".repeat(70_000)).len() <= OSC52_MAX,
            "70 KB still goes"
        );
    }
}
