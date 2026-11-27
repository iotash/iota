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
//! still holds what Ctrl+G handed out. With no `editor:` configured, Ctrl+G prints one line naming
//! the key and does nothing else.

use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;
use crate::ui::facade::Ui;
use crate::ui::input::paste;

/// What Ctrl+G runs and where the draft is written.
#[derive(Debug, Clone, Default)]
pub struct EditorSetup {
    /// The config's `editor:` as argv (`Config::editor`); `None` when unset.
    pub command: Option<Vec<String>>,
    /// The directory the draft file is written in: `~/.iota/editor`, outside every sandbox write root
    /// (a turn may be running while the draft is open).
    pub dir: PathBuf,
}

/// The line Ctrl+G prints with no `editor:` configured.
pub(crate) const NO_EDITOR: &str =
    "Ctrl+G edits the draft in your editor: set `editor:` in ~/.iota.yaml (e.g. `editor: vim`)";

/// Serves Ctrl+G until the facade closes or `stop` fires: one edit at a time, the waiter parked again
/// only once the last edit is answered.
pub(crate) async fn serve(
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    setup: EditorSetup,
    stop: CancellationToken,
) {
    while let Ok(seed) = ui.next_edit(&stop).await {
        let (text, notice) = edit(ui.as_ref(), &tr, &setup, &seed).await;
        ui.edit_done(text, notice);
    }
}

/// One edit of `seed`: the new draft, if any, and the status row's notice.
async fn edit(
    ui: &dyn Ui,
    tr: &Transcript,
    setup: &EditorSetup,
    seed: &str,
) -> (Option<String>, Option<String>) {
    let Some((name, _)) = setup.command.as_deref().and_then(<[String]>::split_first) else {
        tr.notice(NO_EDITOR);
        return (None, None);
    };
    // Probed before the terminal is lent: a missing command never flickers the screen.
    if crate::shell::exec::find_in_path(name).is_none() {
        tr.error(&format!("⚠ editor {name}: command not found"));
        return (None, None);
    }
    let file = match DraftFile::create(&setup.dir, seed) {
        Ok(file) => file,
        Err(e) => {
            tr.error(&format!(
                "⚠ editor: cannot write the draft to {}: {e}",
                setup.dir.display()
            ));
            return (None, None);
        }
    };
    let mut argv: Vec<OsString> = setup.command.iter().flatten().map(OsString::from).collect();
    argv.push(file.path.clone().into_os_string());
    let status = match ui.run_external(argv).await {
        Ok(status) => status,
        Err(e) => {
            tr.error(&format!("⚠ editor {name}: {e}"));
            return (None, None);
        }
    };
    if !status.success() {
        let how = status
            .code()
            .map_or_else(|| status.to_string(), |code| format!("status {code}"));
        return (
            None,
            Some(format!("editor exited with {how} — draft unchanged")),
        );
    }
    match file.read_back() {
        Ok(text) if text == paste::normalize(seed) => (None, Some("draft unchanged".into())),
        Ok(text) if text.is_empty() => (Some(text), Some("draft cleared".into())),
        Ok(text) => (Some(text), Some("draft updated".into())),
        Err(e) => {
            tr.error(&format!("⚠ editor: {e} — draft unchanged"));
            (None, None)
        }
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
