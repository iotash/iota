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
//! The tools run on a thread of their own: a clipboard tool that hangs never stalls the loop. A
//! path that fails fails silently — the selection is still on the screen to copy again.

use std::io::{self, Write as _};
use std::process::{Command, Stdio};

use base64::Engine as _;

/// One way a copied text leaves iota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPath {
    /// The platform clipboard tool ([`copy_to_clipboard`]).
    Local,
    /// `tmux load-buffer -w -`.
    Tmux,
    /// An OSC 52 sequence written to the terminal ([`osc52`]).
    Osc52,
}

/// Where the selections of this session go: the paths, and the modifier the terminal takes for
/// its own selection while iota has the mouse.
#[derive(Debug)]
pub(crate) struct Clipboard {
    paths: Vec<CopyPath>,
    /// `Shift`, or `Option` in iTerm2 (whose bypass key it is).
    pub(crate) native: &'static str,
    /// The texts a test's clipboard was handed (`None`: a live one, which runs the paths); shared,
    /// so a test still reads it after the terminal moved into the loop's thread.
    #[cfg(test)]
    pub(crate) recorded: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
}

impl Clipboard {
    /// The process's own: read from its environment once (`$TMUX`, `$SSH_CONNECTION`/`$SSH_TTY`,
    /// `$TERM_PROGRAM`/`$LC_TERMINAL`).
    pub(crate) fn from_env() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let is = |name: &str, want: &str| std::env::var(name).is_ok_and(|v| v == want);
        let ssh = set("SSH_CONNECTION") || set("SSH_TTY");
        let iterm = is("TERM_PROGRAM", "iTerm.app") || is("LC_TERMINAL", "iTerm2");
        Self {
            paths: paths(set("TMUX"), ssh),
            native: if iterm { "Option" } else { "Shift" },
            #[cfg(test)]
            recorded: None,
        }
    }

    /// A test's: records what it is handed and runs nothing (never the developer's clipboard).
    #[cfg(test)]
    pub(crate) fn recording() -> Self {
        Self {
            paths: Vec::new(),
            native: "Shift",
            recorded: Some(std::sync::Arc::default()),
        }
    }

    /// Sends `text` down every path; the OSC 52 one, when it is among them, is returned for the
    /// caller to write to the terminal (in order with the frame).
    pub(crate) fn copy(&mut self, text: &str) -> Option<String> {
        #[cfg(test)]
        if let Some(rec) = &self.recorded {
            crate::sync::lock(rec).push(text.to_owned());
            return None;
        }
        let mut osc = self.paths.contains(&CopyPath::Osc52).then(|| osc52(text));
        let local = self.paths.contains(&CopyPath::Local);
        let tmux = self.paths.contains(&CopyPath::Tmux);
        let owned = text.to_owned();
        // No local tool on this machine: OSC 52 is the last way to reach a clipboard.
        if local && !tmux && osc.is_none() && !has_local_tool() {
            osc = Some(osc52(text));
        }
        std::thread::spawn(move || {
            if local {
                let _ = copy_to_clipboard(&owned);
            }
            if tmux {
                let _ = pipe(&["tmux", "load-buffer", "-w", "-"], &owned);
            }
        });
        osc
    }
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

/// Whether one of the [`local_tools`] is on `PATH`.
fn has_local_tool() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    local_tools()
        .iter()
        .any(|t| std::env::split_paths(&path).any(|dir| dir.join(t[0]).is_file()))
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
}
