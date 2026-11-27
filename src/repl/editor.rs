//! Ctrl+G — the composer's draft in the user's editor. One long-lived task (`serve`) holds the
//! facade's edit waiter (`Ui::next_edit`, the `read_input` shape): a Ctrl+G hands it the draft with
//! its paste tags expanded, the task writes it to a private file, has the loop lend the terminal to
//! the `editor:` command with the file's path appended (`Ui::run_external`), reads the file back and
//! answers with what became of the draft (`Ui::edit_done`) — then parks again. The loop never learns
//! about the config or the file; this task never touches the terminal.
//!
//! What the draft becomes:
//!
//! | the editor                       | the draft  | said                                        |
//! |----------------------------------|------------|---------------------------------------------|
//! | exits 0, the text as handed out  | untouched  | `draft unchanged` (status row)              |
//! | exits 0, the text changed        | replaced   | `draft updated` (status row)                |
//! | exits 0, the file emptied        | cleared    | `draft cleared` (status row)                |
//! | exits non-zero                   | untouched  | `editor exited with status N — draft unchanged` (status row) |
//! | file gone, unreadable, not UTF-8 | untouched  | a `⚠` line in the conversation              |
//! | command missing, does not start  | untouched  | a `⚠` line in the conversation              |
//!
//! The text read back goes through `paste::normalize` (CRLF to LF, the trailing newlines dropped),
//! and so does the draft it is compared with. The loop applies a new draft only while the composer
//! still holds what Ctrl+G handed out.
//!
//! With no `editor:` configured, Ctrl+G opens the `Editor` list below the composer: the environment's
//! editor first (`$VISUAL`, else `$EDITOR`, shown as the command it would store), then the
//! known editors this machine has (`KNOWN`), then `Other…` for a command typed in. A choice that fails — not
//! found, did not start, exited non-zero — prints its `⚠` line and opens the list again on that row; the
//! first that exits 0 is the editor from then on, written as `editor:` into the user's config file
//! ([`EditorSetup::config`]) so the list is asked once. A write that fails keeps the choice for the
//! session and says why in one line.

use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;
use crate::ui::facade::{Panel, TabbedSpec, Ui};
use crate::ui::input::paste;

/// What Ctrl+G runs and where the draft is written.
#[derive(Debug, Clone, Default)]
pub struct EditorSetup {
    /// The config's `editor:` as argv (`Config::editor`); `None` when unset, and Ctrl+G asks.
    pub command: Option<Vec<String>>,
    /// The environment's editor — `$VISUAL`, else `$EDITOR` — as (the variable, its value): the
    /// list's first row, never used without being chosen.
    pub env: Option<(&'static str, String)>,
    /// The user's config file a chosen editor is written into (`Config::user_file`); `None` without a
    /// home, when the choice holds for the session only.
    pub config: Option<PathBuf>,
    /// The directory the draft file is written in: `~/.iota/editor`, outside every sandbox write root
    /// (a turn may be running while the draft is open).
    pub dir: PathBuf,
}

/// The editors the list offers when this machine has them, in order, each as it would be stored: the
/// terminal ones first, then the windowed ones that need `--wait` to block until the file is closed.
pub(crate) const KNOWN: [&str; 10] = [
    "nvim",
    "vim",
    "vi",
    "emacs -nw",
    "nano",
    "hx",
    "micro",
    "code --wait",
    "zed --wait",
    "subl --wait",
];

/// The list's title.
const TITLE: &str = "Editor";

/// Serves Ctrl+G until the facade closes or `stop` fires: one edit at a time, the waiter parked again
/// only once the last edit is answered. An editor chosen from the list is the command from then on.
pub(crate) async fn serve(
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    mut setup: EditorSetup,
    stop: CancellationToken,
) {
    while let Ok(seed) = ui.next_edit(&stop).await {
        let (text, notice) = match setup.command.clone() {
            Some(argv) => edit(ui.as_ref(), &tr, &argv, &setup.dir, &seed).await,
            None => pick(ui.as_ref(), &tr, &mut setup, &seed, &stop).await,
        };
        ui.edit_done(text, notice);
    }
}

/// A row of the list: the command as it is stored, and how the row reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) command: String,
    pub(crate) label: String,
}

/// The list's rows: the environment's editor, then each [`KNOWN`] one whose program `found` finds
/// (the environment's command not repeated).
pub(crate) fn choices(
    env: Option<&(&'static str, String)>,
    found: impl Fn(&str) -> bool,
) -> Vec<Choice> {
    let env = env.and_then(|(var, value)| {
        let command = value.trim();
        argv_of(command).map(|_| Choice {
            command: command.to_owned(),
            label: format!("{command} · from ${var}"),
        })
    });
    let known: Vec<Choice> = KNOWN
        .iter()
        .filter(|k| env.as_ref().is_none_or(|e| e.command != **k))
        .filter(|k| k.split(' ').next().is_some_and(&found))
        .map(|k| Choice {
            command: (*k).to_owned(),
            label: (*k).to_owned(),
        })
        .collect();
    env.into_iter().chain(known).collect()
}

/// `command` split as a shell would (`shlex`), `None` when that names no program.
fn argv_of(command: &str) -> Option<Vec<String>> {
    shlex::split(command).filter(|argv| !argv.is_empty())
}

/// Ctrl+G with no editor configured: the list, until a choice exits 0 (it is then the session's
/// editor and written to the user config) or the list is closed (the draft untouched).
async fn pick(
    ui: &dyn Ui,
    tr: &Transcript,
    setup: &mut EditorSetup,
    seed: &str,
    stop: &CancellationToken,
) -> (Option<String>, Option<String>) {
    let rows = choices(setup.env.as_ref(), |name| {
        crate::shell::exec::find_in_path(name).is_some()
    });
    let labels: Vec<String> = rows.iter().map(|c| c.label.clone()).collect();
    let mut cursor = 0;
    loop {
        let panel = Panel::list(TITLE, labels.clone())
            .with_custom(true)
            .with_cursor(cursor);
        let spec = TabbedSpec {
            panels: vec![panel],
            ..TabbedSpec::default()
        };
        let Ok(r) = ui.tabbed(stop, spec).await else {
            return (None, None);
        };
        let Some(p) = r.panels.first().filter(|_| !r.cancelled) else {
            return (None, None);
        };
        // Past the listed rows is `Other…`, the command typed into it.
        cursor = p.cursor.min(rows.len());
        let command = rows
            .get(cursor)
            .map_or_else(|| p.custom.clone(), |c| c.command.clone());
        let Some(argv) = argv_of(&command) else {
            tr.error(&format!("⚠ editor `{command}`: not a command"));
            continue;
        };
        match run(ui, &argv, &setup.dir, seed).await {
            Run::Done(text, notice) => {
                setup.command = Some(argv);
                persist(tr, setup.config.as_deref(), &command);
                return (text, notice);
            }
            Run::Failed(why) => tr.error(&why),
            Run::Exited(how) => tr.error(&format!(
                "⚠ editor {}: exited with {how} — draft unchanged",
                argv[0]
            )),
            Run::Unreadable(why) => {
                // It ran and exited 0: the choice holds, the draft is what failed.
                setup.command = Some(argv);
                persist(tr, setup.config.as_deref(), &command);
                tr.error(&why);
                return (None, None);
            }
            Run::NoFile(why) => {
                tr.error(&why);
                return (None, None);
            }
        }
    }
}

/// Writes the chosen `command` as `editor:` into the user config, saying where — or, when it cannot,
/// that it holds for this session only, and why.
fn persist(tr: &Transcript, config: Option<&Path>, command: &str) {
    let saved = config.map(|path| {
        crate::config::edit::write_editor(path, command).map(|()| path.display().to_string())
    });
    match saved {
        Some(Ok(path)) => tr.notice(&format!("editor: {command} — saved to {path}")),
        Some(Err(e)) => tr.notice(&format!(
            "editor: {command} for this session — not saved: {e}"
        )),
        None => tr.notice(&format!(
            "editor: {command} for this session — not saved: no home directory"
        )),
    }
}

/// One edit of `seed` in the configured `argv`: the new draft, if any, and the status row's notice.
async fn edit(
    ui: &dyn Ui,
    tr: &Transcript,
    argv: &[String],
    dir: &Path,
    seed: &str,
) -> (Option<String>, Option<String>) {
    match run(ui, argv, dir, seed).await {
        Run::Done(text, notice) => (text, notice),
        Run::Exited(how) => (
            None,
            Some(format!("editor exited with {how} — draft unchanged")),
        ),
        Run::Failed(why) | Run::Unreadable(why) | Run::NoFile(why) => {
            tr.error(&why);
            (None, None)
        }
    }
}

/// What running the editor on the draft came to.
enum Run {
    /// It exited 0: the new draft, if any, and the status row's notice.
    Done(Option<String>, Option<String>),
    /// The command is not there or did not start: the `⚠` line.
    Failed(String),
    /// It exited non-zero: how (`status N`).
    Exited(String),
    /// It exited 0 but its file is gone, unreadable or not UTF-8: the `⚠` line.
    Unreadable(String),
    /// The draft file could not be written, so nothing ran: the `⚠` line.
    NoFile(String),
}

/// Runs `argv` with the draft's file appended, the command probed first: a missing one never
/// flickers the screen.
async fn run(ui: &dyn Ui, argv: &[String], dir: &Path, seed: &str) -> Run {
    let Some(name) = argv.first() else {
        return Run::Failed("⚠ editor: no command".to_owned());
    };
    if crate::shell::exec::find_in_path(name).is_none() {
        return Run::Failed(format!("⚠ editor {name}: command not found"));
    }
    let file = match DraftFile::create(dir, seed) {
        Ok(file) => file,
        Err(e) => {
            return Run::NoFile(format!(
                "⚠ editor: cannot write the draft to {}: {e}",
                dir.display()
            ));
        }
    };
    let mut full: Vec<OsString> = argv.iter().map(OsString::from).collect();
    full.push(file.path.clone().into_os_string());
    let status = match ui.run_external(full).await {
        Ok(status) => status,
        Err(e) => return Run::Failed(format!("⚠ editor {name}: {e}")),
    };
    if !status.success() {
        return Run::Exited(
            status
                .code()
                .map_or_else(|| status.to_string(), |code| format!("status {code}")),
        );
    }
    match file.read_back() {
        Ok(text) if text == paste::normalize(seed) => {
            Run::Done(None, Some("draft unchanged".into()))
        }
        Ok(text) if text.is_empty() => Run::Done(Some(text), Some("draft cleared".into())),
        Ok(text) => Run::Done(Some(text), Some("draft updated".into())),
        Err(e) => Run::Unreadable(format!("⚠ editor: {e} — draft unchanged")),
    }
}

/// The draft's file, `draft-<pid>.md` (mode 0600), removed when this is dropped.
struct DraftFile {
    path: PathBuf,
}

impl DraftFile {
    /// Writes `seed` and one trailing newline to a new file in `dir` — a leftover of this pid's, from a
    /// run that never removed it, removed first.
    fn create(dir: &Path, seed: &str) -> io::Result<Self> {
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dirs, 0o700);
        dirs.create(dir)?;
        let path = dir.join(format!("draft-{}.md", std::process::id()));
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut f = open.open(&path)?;
        // From here the file is ours to remove, written or not.
        let file = Self { path };
        f.write_all(seed.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(file)
    }

    /// The text the editor left, normalized; never a lossy decode.
    fn read_back(&self) -> Result<String, String> {
        let shown = self.path.display();
        let bytes = std::fs::read(&self.path).map_err(|e| format!("cannot read {shown}: {e}"))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8"))?;
        Ok(paste::normalize(&text))
    }
}

impl Drop for DraftFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests;
