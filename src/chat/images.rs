//! Saving generated images to disk (chat/images.go): file extension by MIME type, the timestamped file name, and
//! the quiet (collect-don't-fail) save used by the headless run, which also yields the attachments a resumed
//! session persists.

use std::path::{Path, PathBuf};

use crate::provider::model::{Attachment, Message, Role};

/// Widest half-block picture the transcript draws (chat/images.go:64 `imageMaxCols`).
pub(crate) const IMAGE_MAX_COLS: usize = 72;
/// Tallest half-block picture the transcript draws (chat/images.go:65 `imageMaxRows`).
pub(crate) const IMAGE_MAX_ROWS: usize = 14;
/// The picture's and its caption's indent (chat/images.go:66 `imageIndentCols`).
pub(crate) const IMAGE_INDENT_COLS: usize = 2;

/// The newest user message (chat/images.go:80 `lastUserMessage`) — what `/redo` re-sends.
pub(crate) fn last_user_message(history: &[Message]) -> Option<&Message> {
    history.iter().rev().find(|m| m.role() == Role::User)
}

/// The image attachments of the newest assistant message that carries any (chat/images.go:92
/// `lastGeneratedImages`; clones) — what `/edit <prompt>` re-attaches.
pub(crate) fn last_generated_images(history: &[Message]) -> Vec<Attachment> {
    history
        .iter()
        .rev()
        .filter(|m| m.role() == Role::Assistant)
        .map(|m| {
            m.attachments
                .iter()
                .filter(|a| a.is_image())
                .cloned()
                .collect::<Vec<_>>()
        })
        .find(|imgs| !imgs.is_empty())
        .unwrap_or_default()
}

/// image/png → "png", image/jpeg → "jpg", image/webp → "webp", image/gif → "gif", else "bin" (images.go:31-43).
pub(crate) fn image_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// The error text when no home directory is known (Go: `os.UserHomeDir`, os/file.go:517; kept on every platform).
pub const HOME_NOT_DEFINED: &str = "$HOME is not defined";

/// dir None → Err("$HOME is not defined") per image (Go: `app.Home()` propagates `os.UserHomeDir`'s error verbatim,
/// os/file.go:517 — the literal is kept on every platform); `create_dir_all(dir)` (0755 umask default); name
/// `{jiff::Zoned::now().strftime("%Y%m%d-%H%M%S")}-{seq}.{ext}` (Go layout `20060102-150405`, local time); write
/// 0644; returns the absolute path (images.go:21-28,48-58).
pub fn save_image(att: &Attachment, dir: Option<&Path>, seq: usize) -> Result<PathBuf, String> {
    let dir = dir.ok_or_else(|| HOME_NOT_DEFINED.to_owned())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S");
    let name = format!("{stamp}-{seq}.{}", image_ext(&att.mime_type));
    let path = dir.join(name);
    write_0644(&path, &att.data).map_err(|e| e.to_string())?;
    std::path::absolute(&path).map_err(|e| e.to_string())
}

/// `os.WriteFile(path, data, 0o644)`: create-or-truncate with mode 0644 (umask applied, like Go).
#[cfg(unix)]
fn write_0644(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)?;
    f.write_all(data)
}

/// Non-unix hosts have no mode bits to set (DIVERGENCES I-07).
#[cfg(not(unix))]
fn write_0644(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// What one turn's image saves produced (CONTRACTS S§3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedImages {
    /// The SAVED subset, each `filename` rewritten to the saved file's basename and the original mime/data kept
    /// — exactly what Go attaches to the persisted assistant message (images.go:131).
    pub attachments: Vec<Attachment>,
    /// Absolute paths of the images that were saved, as display strings.
    pub paths: Vec<String>,
    /// `saving image failed: {e}` lines, one per image whose save failed.
    pub failures: Vec<String>,
}

/// Go's `collectImages` + `saveImagesQuiet` merged for the turn (images.go:117-148, 156-170): every generated
/// image is tried and a failure never stops the next one. A SUCCESSFUL save yields the path (for stdout / the
/// JSON report) AND the attachment to persist, with `filename` REWRITTEN to the saved file's basename
/// (images.go:131 `att.Filename = filepath.Base(path)`) and the original mime/data; a FAILED save yields only
/// the `saving image failed: {e}` line — the image is NOT attached. Replaces phase 1's `save_images_quiet`
/// (sole caller: `run_once`); `save_image` is unchanged.
pub fn save_images_for_turn(images: &[Attachment], dir: Option<&Path>) -> SavedImages {
    let mut saved = SavedImages::default();
    for (i, att) in images.iter().enumerate() {
        match save_image(att, dir, i) {
            Ok(p) => {
                saved.attachments.push(Attachment {
                    // `filepath.Base` of the saved path: the link from the persisted record to the file on disk.
                    filename: p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    mime_type: att.mime_type.clone(),
                    data: att.data.clone(),
                });
                saved.paths.push(p.to_string_lossy().into_owned());
            }
            Err(e) => saved.failures.push(format!("saving image failed: {e}")),
        }
    }
    saved
}

#[cfg(test)]
mod tests {
    use super::image_ext;

    #[test]
    fn ext_table() {
        assert_eq!(image_ext("image/png"), "png");
        assert_eq!(image_ext("image/jpeg"), "jpg");
        assert_eq!(image_ext("image/webp"), "webp");
        assert_eq!(image_ext("image/gif"), "gif");
        assert_eq!(image_ext("image/jpg"), "bin");
        assert_eq!(image_ext(""), "bin");
    }
}
