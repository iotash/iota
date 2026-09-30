//! The bundle's single-writer lock (docs/design/bot-mode.md §2.3): `<bundle>/.lock`, held by whichever
//! process owns the bundle's [`SessionWriter`](crate::session::SessionWriter).
//!
//! The lock is `File::try_lock` — advisory, and released by the OS when the handle closes or the process
//! dies, so there is no such thing as a stale lock. The pid written into the file is only there so the
//! refusal can say who holds it; nothing ever trusts it for anything else.

use std::io::{Read, Seek, Write};
use std::path::Path;

use crate::session::error::SessionError;

/// The lock file's name inside a bundle.
pub const LOCK_FILE: &str = ".lock";

/// A held bundle lock. Dropping it releases the lock EXPLICITLY (`File::unlock`) before the handle closes.
///
/// Closing alone is not enough. On unix the lock is a `flock`, which belongs to the open file description,
/// not to the descriptor — and a child forked while the lock is held gets a copy of that description until its
/// `exec` closes the close-on-exec descriptors. std falls back from `posix_spawn` to fork + exec in several
/// cases (a `PATH` override with a bare program name, uid/gid, `pre_exec` hooks, a relative program with a
/// cwd on macOS), so any process this one starts can open that window. A plain close inside it would leave
/// the lock held by a half-born child: the next `resume` of this bundle, in this process or another, would
/// see `Locked` naming our own pid. Unlocking acts on the shared description, so it
/// releases the lock for every copy at once.
#[derive(Debug)]
pub(crate) struct BundleLock(std::fs::File);

impl Drop for BundleLock {
    fn drop(&mut self) {
        // Nothing to do on failure: the close that follows is the fallback it always was.
        let _ = self.0.unlock();
    }
}

/// Takes the bundle lock of `dir` (which must exist) or refuses with [`SessionError::Locked`]. `id` only
/// names the session in that refusal. The returned guard IS the lock: dropping it releases it.
pub(crate) fn lock_bundle(dir: &Path, id: &str) -> Result<BundleLock, SessionError> {
    let mut file = open_lock_file(&dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => {
            // Best effort: the pid is for the error text only, so a failed write does not fail the lock.
            let _ = write_pid(&mut file);
            Ok(BundleLock(file))
        }
        Err(std::fs::TryLockError::WouldBlock) => Err(SessionError::Locked {
            what: format!("session {id}"),
            pid: read_pid(&mut file),
        }),
        Err(std::fs::TryLockError::Error(e)) => Err(SessionError::Io(e)),
    }
}

/// Replaces the file's contents with this process's pid.
fn write_pid(file: &mut std::fs::File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(std::process::id().to_string().as_bytes())
}

/// The holder's pid as it wrote it; `None` when the file is empty (the holder has not written it yet) or
/// holds anything else.
fn read_pid(file: &mut std::fs::File) -> Option<u32> {
    let mut text = String::new();
    file.rewind().ok()?;
    file.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

/// Opens (creating when absent, never truncating) the lock file read-write, 0644 on unix.
#[cfg(unix)]
fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .open(path)
}

/// Non-unix hosts have no mode bits to set (phase-1 DIVERGENCES I-07).
#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::{LOCK_FILE, lock_bundle};
    use crate::session::error::SessionError;

    /// A second holder is refused with the first one's pid; once the first handle drops, the lock is free.
    #[test]
    fn conflict_names_the_holder_and_drop_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = lock_bundle(dir.path(), "k7q").expect("first lock");
        let err = lock_bundle(dir.path(), "k7q").expect_err("second lock refused");
        let SessionError::Locked { what, pid } = &err else {
            panic!("expected Locked, got {err:?}");
        };
        assert_eq!(what, "session k7q");
        assert_eq!(*pid, Some(std::process::id()));
        assert_eq!(
            err.to_string(),
            format!(
                "session k7q is open in another iota process (pid {})",
                std::process::id()
            )
        );
        // Deterministic even while other tests in this binary spawn processes: the guard UNLOCKS on drop,
        // so a child forked in the meantime cannot keep the lock alive (see `BundleLock`).
        drop(held);
        let again = lock_bundle(dir.path(), "k7q").expect("re-lock after drop");
        drop(again);
        assert!(dir.path().join(LOCK_FILE).exists());
    }

    /// A holder whose pid is not (yet) in the file is still refused — the text just omits the pid.
    #[test]
    fn unknown_pid_is_omitted_from_the_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = lock_bundle(dir.path(), "k7q").expect("first lock");
        std::fs::write(dir.path().join(LOCK_FILE), "").expect("clear pid");
        let err = lock_bundle(dir.path(), "k7q").expect_err("refused");
        assert_eq!(
            err.to_string(),
            "session k7q is open in another iota process"
        );
        drop(held);
    }
}
