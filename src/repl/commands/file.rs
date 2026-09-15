//! `/file` — attachments riding the next user message (chat/file.go + chat/run.go:390-446).
//!
//! Two shapes share one command. `"/file <path>"` attaches immediately, which is what a
//! shell-minded user reaches for; the BARE form opens a two-tab surface — `"Attached"`
//! (a Multi over what is already queued, checked rows meaning "remove") and `"Add"` (a
//! directory Browser) — and which tab the user committed FROM decides which action runs.
//! That is why the surface commits from anywhere: the focused tab IS the verb.
//!
//! Attachments queue on [`crate::repl::state::Conversation::pending`] until the next send. An interrupted
//! turn hands them back (`run.rs`), so a cancelled message never silently drops the file
//! the user just attached.

use std::path::{Path, PathBuf};

use crate::provider::model::Attachment;
use crate::ui::facade::{Panel, TabbedSpec};

use crate::repl::run::Repl;

/// The attachment size ceiling (chat/file.go `maxFileSize`) — 20 MiB.
const MAX_FILE_SIZE: u64 = 20 * 1024 * 1024;

/// Extension → MIME type (chat/file.go `mimeTypes`). An extension that is not here is
/// REFUSED rather than guessed: a provider that rejects an unknown part fails the whole
/// turn, and a wrong guess is worse than a clear "unsupported file type".
const MIME_TYPES: &[(&str, &str)] = &[
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".png", "image/png"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".pdf", "application/pdf"),
    (".txt", "text/plain"),
    (".md", "text/plain"),
    (".go", "text/plain"),
    (".py", "text/plain"),
    (".js", "text/plain"),
    (".ts", "text/plain"),
    (".jsx", "text/plain"),
    (".tsx", "text/plain"),
    (".java", "text/plain"),
    (".c", "text/plain"),
    (".cpp", "text/plain"),
    (".h", "text/plain"),
    (".rs", "text/plain"),
    (".rb", "text/plain"),
    (".sh", "text/plain"),
    (".yaml", "text/plain"),
    (".yml", "text/plain"),
    (".json", "text/plain"),
    (".xml", "text/plain"),
    (".html", "text/plain"),
    (".css", "text/plain"),
    (".sql", "text/plain"),
    (".csv", "text/plain"),
    (".log", "text/plain"),
    (".toml", "text/plain"),
    (".ini", "text/plain"),
    (".cfg", "text/plain"),
    (".conf", "text/plain"),
];

/// Go `filepath.Ext`: everything from the LAST `'.'` of the final path element, or `""`.
///
/// Not `Path::extension`, which treats a leading dot as the stem — Go calls `".env"` an
/// extension, and the refusal message names it. The table lookup is what decides; this
/// only has to agree with Go about what the string IS.
fn go_ext(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.rfind('.')
        .map_or_else(String::new, |i| name[i..].to_lowercase())
}

/// Why an attachment was refused (chat/file.go:59-92), in the order the checks run; every
/// Display text is Go's, the OS text carried where Go's `%w` carried it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AttachError {
    /// A `"~/"` path on a host with no home directory to expand it against.
    #[error("cannot resolve home dir: $HOME is not defined")]
    NoHome,
    /// The path could not be stat'ed.
    #[error("cannot access file: {0}")]
    Access(#[source] std::io::Error),
    /// The path is a directory.
    #[error("path is a directory, not a file")]
    IsDirectory,
    /// The file is over the size ceiling.
    #[error("file too large: {size} bytes (max {max})")]
    TooLarge {
        /// The file's size.
        size: u64,
        /// The ceiling.
        max: u64,
    },
    /// The extension is not in the table (`""` for a file without one).
    #[error("unsupported file type: {0}")]
    UnsupportedType(String),
    /// The file could not be read.
    #[error("cannot read file: {0}")]
    Read(#[source] std::io::Error),
}

/// The MIME type for `path`'s extension, or the Go refusal (chat/file.go
/// `DetectMimeType`). The extension is lower-cased; a file without one gets `""`, whose
/// message reads `"unsupported file type: "`, exactly as in Go.
pub(crate) fn detect_mime_type(path: &Path) -> Result<&'static str, AttachError> {
    let ext = go_ext(path);
    MIME_TYPES
        .iter()
        .find(|(k, _)| *k == ext)
        .map(|(_, v)| *v)
        .ok_or(AttachError::UnsupportedType(ext))
}

/// Expands a leading `"~/"` against `home` (chat/file.go `ReadAttachment`). A host with
/// no home directory refuses rather than reading a literal `./~/…`.
fn expand_home(path: &str, home: Option<&Path>) -> Result<PathBuf, AttachError> {
    let Some(rest) = path.strip_prefix("~/") else {
        return Ok(PathBuf::from(path));
    };
    home.map(|h| h.join(rest)).ok_or(AttachError::NoHome)
}

/// Reads one attachment, with `home` supplied rather than probed — the whole function is
/// then testable without touching the process environment (chat/file.go `ReadAttachment`).
///
/// The checks run in Go's order, because the error the user sees should name the FIRST
/// thing that was wrong: reachable → not a directory → within the size cap → a known
/// type → readable.
pub(crate) fn read_attachment_in(
    path: &str,
    home: Option<&Path>,
) -> Result<Attachment, AttachError> {
    let path = expand_home(path, home)?;
    let info = std::fs::metadata(&path).map_err(AttachError::Access)?;
    if info.is_dir() {
        return Err(AttachError::IsDirectory);
    }
    if info.len() > MAX_FILE_SIZE {
        return Err(AttachError::TooLarge {
            size: info.len(),
            max: MAX_FILE_SIZE,
        });
    }
    let mime_type = detect_mime_type(&path)?;
    let data = std::fs::read(&path).map_err(AttachError::Read)?;
    Ok(Attachment {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        mime_type: mime_type.to_owned(),
        data,
    })
}

/// [`read_attachment_in`] against the host's real home directory.
pub(crate) fn read_attachment(path: &str) -> Result<Attachment, AttachError> {
    read_attachment_in(path, crate::app::user_home().as_deref())
}

/// A byte count in the Go display form (chat/file.go `humanSize`).
pub(crate) fn human_size(n: usize) -> String {
    #[allow(clippy::cast_precision_loss)] // display only; the ratio is what is shown
    let f = n as f64;
    if n >= 1024 * 1024 {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// One row of the `"Attached"` tab (chat/file.go `attachmentLabel`).
pub(crate) fn attachment_label(a: &Attachment) -> String {
    format!(
        "{} ({}, {})",
        a.filename,
        a.mime_type,
        human_size(a.data.len())
    )
}

/// The notice a successful attach prints — byte-exact, and the same line for both shapes
/// of the command (chat/run.go:398,445).
fn attached_notice(a: &Attachment) -> String {
    format!(
        "Attached: {} ({}, {} bytes)",
        a.filename,
        a.mime_type,
        a.data.len()
    )
}

/// The directory the `"Add"` browser opens in: the working directory, falling back to the
/// home directory when it cannot be read (chat/run.go:406-409).
fn browse_dir() -> PathBuf {
    std::env::current_dir()
        .ok()
        .or_else(crate::app::user_home)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The `/file` command. `arg` is the trimmed text after the command (`""` = the bare
/// form, which opens the surface).
pub(crate) async fn cmd_file(repl: &mut Repl, arg: &str) {
    let cancel = &repl.handles.cancel.clone();
    if !arg.is_empty() {
        match read_attachment(arg) {
            Ok(att) => {
                repl.handles.tr.notice(&attached_notice(&att));
                repl.conv.pending.push(att);
            }
            Err(e) => repl.handles.tr.error(&format!("Error: {e}")),
        }
        return;
    }

    let rows: Vec<String> = repl.conv.pending.iter().map(attachment_label).collect();
    let spec = TabbedSpec {
        panels: vec![
            Panel::multi("Attached".to_owned(), rows).with_search(true),
            Panel::browser("Add".to_owned(), browse_dir()).with_search(true),
        ],
        ..TabbedSpec::default()
    };
    // A facade failure is treated exactly like a cancel: commands never end the loop —
    // a closed facade ends it at the next `read_input`, the ONE exit path.
    let Ok(r) = repl.handles.ui.tabbed(cancel, spec).await else {
        return;
    };
    if r.cancelled {
        return;
    }
    match r.focused {
        // The "Attached" tab: checked rows are the ones to drop. Checks index the
        // ORIGINAL items (the engine's commit contract), so they index `pending`
        // directly even when a filter was applied.
        0 => {
            let Some(checked) = r.panels.first().map(|p| p.checked.clone()) else {
                return;
            };
            if checked.is_empty() {
                return;
            }
            let before = repl.conv.pending.len();
            repl.conv.pending = std::mem::take(&mut repl.conv.pending)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !checked.contains(i))
                .map(|(_, a)| a)
                .collect();
            repl.handles.tr.notice(&format!(
                "Removed {} attachment(s).",
                before - repl.conv.pending.len()
            ));
        }
        // The "Add" tab: the browser's chosen file.
        1 => {
            let path = r.panels.get(1).map(|p| p.path.clone()).unwrap_or_default();
            if path.is_empty() {
                return;
            }
            match read_attachment(&path) {
                Ok(att) => {
                    repl.handles.tr.notice(&attached_notice(&att));
                    repl.conv.pending.push(att);
                }
                Err(e) => repl.handles.tr.error(&format!("Error: {e}")),
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_FILE_SIZE, attachment_label, detect_mime_type, human_size, read_attachment_in,
    };
    use crate::provider::model::Attachment;
    use std::path::{Path, PathBuf};

    fn att(name: &str, mime: &str, n: usize) -> Attachment {
        Attachment {
            filename: name.to_owned(),
            mime_type: mime.to_owned(),
            data: vec![0; n],
        }
    }

    /// The extension table and its refusal text (chat/file.go `DetectMimeType`): the
    /// lookup is case-insensitive, and an unknown extension is named in the message.
    #[test]
    fn detect_mime_type_covers_the_go_table() {
        for (path, want) in [
            ("a.PNG", "image/png"),
            ("a.jpeg", "image/jpeg"),
            ("a.JPG", "image/jpeg"),
            ("a.pdf", "application/pdf"),
            ("a.rs", "text/plain"),
            ("dir/deep/notes.md", "text/plain"),
        ] {
            assert_eq!(detect_mime_type(Path::new(path)).ok(), Some(want), "{path}");
        }
        assert_eq!(
            detect_mime_type(Path::new("a.exe"))
                .unwrap_err()
                .to_string(),
            "unsupported file type: .exe"
        );
        assert_eq!(
            detect_mime_type(Path::new("Makefile"))
                .unwrap_err()
                .to_string(),
            "unsupported file type: "
        );
        // Go's `filepath.Ext` treats a leading dot as the extension, so a dotfile is named
        // in the refusal — and a file literally called ".md" IS a text file.
        assert_eq!(
            detect_mime_type(Path::new(".env")).unwrap_err().to_string(),
            "unsupported file type: .env"
        );
        assert_eq!(detect_mime_type(Path::new(".md")).ok(), Some("text/plain"));
    }

    /// `read_attachment`'s refusals, in Go's order (chat/file.go:59-92). Every message is
    /// byte-exact up to the OS text a `%w` carries.
    #[test]
    fn read_attachment_refusals_are_byte_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.txt");
        let err = read_attachment_in(&missing.to_string_lossy(), None)
            .expect_err("must fail")
            .to_string();
        assert!(err.starts_with("cannot access file: "), "{err}");

        let err = read_attachment_in(&dir.path().to_string_lossy(), None)
            .expect_err("must fail")
            .to_string();
        assert_eq!(err, "path is a directory, not a file");

        let big = dir.path().join("big.txt");
        std::fs::write(
            &big,
            vec![b'x'; usize::try_from(MAX_FILE_SIZE).unwrap_or(0) + 1],
        )
        .expect("write");
        let err = read_attachment_in(&big.to_string_lossy(), None)
            .expect_err("must fail")
            .to_string();
        assert_eq!(
            err,
            format!("file too large: {} bytes (max 20971520)", MAX_FILE_SIZE + 1)
        );

        // The type check runs AFTER the size check: a 30MB .exe complains about its size.
        let unknown = dir.path().join("tool.exe");
        std::fs::write(&unknown, b"MZ").expect("write");
        assert_eq!(
            read_attachment_in(&unknown.to_string_lossy(), None)
                .unwrap_err()
                .to_string(),
            "unsupported file type: .exe"
        );
    }

    /// The happy path: basename, mapped MIME type, raw bytes — and the `"~/"` expansion,
    /// exercised against an INJECTED home so the test never reads the environment.
    #[test]
    fn read_attachment_reads_bytes_and_expands_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("notes.md");
        std::fs::write(&file, b"# hi\n").expect("write");

        let a = read_attachment_in(&file.to_string_lossy(), None).expect("read");
        assert_eq!(a.filename, "notes.md");
        assert_eq!(a.mime_type, "text/plain");
        assert_eq!(a.data, b"# hi\n");

        let home: PathBuf = dir.path().to_path_buf();
        let a = read_attachment_in("~/notes.md", Some(&home)).expect("read through ~");
        assert_eq!(a.data, b"# hi\n");

        // No home: the expansion refuses rather than reading a literal "~/…".
        assert_eq!(
            read_attachment_in("~/notes.md", None)
                .unwrap_err()
                .to_string(),
            "cannot resolve home dir: $HOME is not defined"
        );
    }

    /// `humanSize` + `attachmentLabel` (chat/file.go:95-110) — the `"Attached"` tab's rows.
    #[test]
    fn attachment_labels_render_the_go_forms() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(
            attachment_label(&att("shot.png", "image/png", 2048)),
            "shot.png (image/png, 2.0 KB)"
        );
    }
}
