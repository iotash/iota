//! The one atomic file writer: a sibling temporary file, synced, renamed over the target. A config file the
//! user hand-edits and a token file that must never be half-written both go through it; neither is ever seen
//! in an intermediate state, and a crash between the write and the rename leaves the old file in place.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use rand::RngCore as _;

/// Writes `bytes` to `path` in one step. `mode` is the unix mode of a NEW file (ignored elsewhere); when it is
/// `None` and `path` already exists, the existing permissions are carried over, which is what a rewrite of a
/// hand-edited config wants. The parent directory is created when missing.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    // The existing file's permissions, read BEFORE anything is written: a rename drops the target's mode
    // bits along with its bytes.
    let existing = std::fs::metadata(path).ok().map(|m| m.permissions());
    let tmp = dir.join(temp_name(path));
    let result = write_then_rename(&tmp, path, bytes, mode, existing);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `.<name>.<pid>.<nonce>.tmp` beside the target: unique per process and per call, hidden on unix.
fn temp_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        ".{name}.{}.{:08x}.tmp",
        std::process::id(),
        rand::rng().next_u32()
    )
}

fn write_then_rename(
    tmp: &Path,
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    existing: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = opts.open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if mode.is_none()
        && let Some(perm) = existing
    {
        std::fs::set_permissions(tmp, perm)?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("f.yaml");
        write_atomic(&path, b"one\n", None).expect("first write creates the parent");
        assert_eq!(std::fs::read(&path).expect("read"), b"one\n");
        write_atomic(&path, b"two\n", None).expect("second write replaces");
        assert_eq!(std::fs::read(&path).expect("read"), b"two\n");
        let names: Vec<String> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["f.yaml"],
            "no temp file survives a successful write"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_keeps_existing_permissions_and_applies_a_requested_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let kept = dir.path().join("kept.yaml");
        std::fs::write(&kept, b"x").expect("write");
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        write_atomic(&kept, b"y", None).expect("rewrite");
        let mode = std::fs::metadata(&kept).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "a rewrite carries the file's own mode over");

        let secret = dir.path().join("token.json");
        write_atomic(&secret, b"{}", Some(0o600)).expect("write 0600");
        let mode = std::fs::metadata(&secret)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a requested mode applies to the new file");
        // The mode is the temp file's from `open` (`OpenOptions::mode`, under the umask), never a
        // `set_permissions` after the bytes are in: no window in which the file is wider than asked. And
        // a target that already exists with a wider mode does not keep it — the rename replaces the inode.
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        write_atomic(&secret, b"{\"n\":2}", Some(0o600)).expect("rewrite 0600");
        let mode = std::fs::metadata(&secret)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "a rewrite with a requested mode does not inherit the old one"
        );
        assert_eq!(std::fs::read(&secret).expect("read"), b"{\"n\":2}");
    }
}
