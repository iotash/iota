//! The two single-writer locks of docs/design/bot-mode.md §2.3: the bundle's `<bundle>/.lock`, held by
//! whichever process owns the bundle's [`SessionWriter`](crate::session::SessionWriter), and the bot's
//! `<bots>/<name>/lock`, held by the one process running that bot — from before its pointer is read until it
//! exits, so the window between writing the pointer and materialising the bundle has an owner too.
//!
//! The lock is `File::try_lock` — advisory, and released by the OS when the handle closes or the process
//! dies, so there is no such thing as a stale lock. The pid written into the file is only there so the
//! refusal can say who holds it; nothing ever trusts it for anything else.

use std::io::{Read, Seek, Write};
use std::path::Path;

use crate::session::error::SessionError;

/// The lock file's name inside a bundle.
pub const LOCK_FILE: &str = ".lock";

/// The lock file's name inside a bot's directory.
pub const BOT_LOCK_FILE: &str = "lock";

/// A held lock (a bundle's or a bot's). Dropping it releases the lock EXPLICITLY (`File::unlock`) before the handle closes.
///
/// Closing alone is not enough. On unix the lock is a `flock`, which belongs to the open file description,
/// not to the descriptor — and a child forked while the lock is held gets a copy of that description until its
/// `exec` closes the close-on-exec descriptors. std falls back from `posix_spawn` to fork + exec in several
/// cases (a `PATH` override with a bare program name, uid/gid, `pre_exec` hooks, a relative program with a
/// cwd on macOS), so any process this one starts can open that window. A plain close inside it would leave
/// the lock held by a half-born child: the next `resume` of this bundle, in this process or another, would
/// see `Locked` naming our own pid. Unlocking acts on the shared description, so it
/// releases the lock for every copy at once.
///
/// A filesystem that cannot lock at all (NFS or SMB without lock support) yields no guard: the open or the
/// delete that wanted it fails with [`SessionError::LockUnsupported`]. The writer's all-or-nothing batch cuts
/// the log back to where it began, which is only safe while nobody else can append — an unheld lock would let
/// that cut take another process's saved turns with it.
#[derive(Debug)]
pub(crate) struct HeldLock(std::fs::File);

impl Drop for HeldLock {
    fn drop(&mut self) {
        // Nothing to do on failure: the close that follows is the fallback it always was.
        let _ = self.0.unlock();
    }
}

/// Takes the bundle lock of `dir` (which must exist) or refuses with [`SessionError::Locked`]. `id` only
/// names the session in that refusal. The returned guard IS the lock: dropping it releases it.
pub(crate) fn lock_bundle(dir: &Path, id: &str) -> Result<HeldLock, SessionError> {
    try_lock_file(&dir.join(LOCK_FILE))?.map_err(|pid| SessionError::Locked {
        what: format!("session {id}"),
        pid,
    })
}

/// Takes the bot lock in `bot_dir` (created when missing) or refuses with [`SessionError::Locked`] naming
/// `bot <name>`.
pub(crate) fn lock_bot(bot_dir: &Path, bot: &str) -> Result<HeldLock, SessionError> {
    std::fs::create_dir_all(bot_dir)?;
    try_lock_file(&bot_dir.join(BOT_LOCK_FILE))?.map_err(|pid| SessionError::Locked {
        what: format!("bot {bot}"),
        pid,
    })
}

/// One `try_lock` on `path`: the held lock (with this process's pid written in), or — when another
/// handle holds it — the pid that holder wrote. The outer error is an I/O fault, not a conflict.
///
/// A filesystem that cannot lock at all (`ENOTSUP` / `EOPNOTSUPP`: NFS or SMB without lock support) fails
/// closed with [`SessionError::LockUnsupported`] naming the directory — never a guard that holds nothing.
fn try_lock_file(path: &Path) -> Result<Result<HeldLock, Option<u32>>, SessionError> {
    let mut file = open_lock_file(path)?;
    match try_lock(&file) {
        Ok(()) => {
            // Best effort: the pid is for the error text only, so a failed write does not fail the lock.
            let _ = write_pid(&mut file);
            Ok(Ok(HeldLock(file)))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(Err(read_pid(&mut file))),
        Err(std::fs::TryLockError::Error(e)) if cannot_lock(&e) => {
            Err(SessionError::LockUnsupported {
                dir: path.parent().unwrap_or(path).to_path_buf(),
            })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// `File::try_lock`, or — in a test that asked for it — the refusal a filesystem without locks gives.
fn try_lock(file: &std::fs::File) -> Result<(), std::fs::TryLockError> {
    #[cfg(test)]
    if tests::UNSUPPORTED.get() {
        return Err(std::fs::TryLockError::Error(
            std::io::Error::from_raw_os_error(ENOTSUP),
        ));
    }
    file.try_lock()
}

/// `ENOTSUP` and `EOPNOTSUPP` (one number on Linux, two on the BSDs and macOS).
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const ENOTSUP: i32 = 45;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const EOPNOTSUPP: i32 = 102;
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
const ENOTSUP: i32 = 95;
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
const EOPNOTSUPP: i32 = 95;

/// The filesystem cannot lock at all (as opposed to failing to).
fn cannot_lock(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Unsupported
        || matches!(e.raw_os_error(), Some(n) if n == ENOTSUP || n == EOPNOTSUPP)
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
pub(crate) mod tests {
    use super::{BOT_LOCK_FILE, LOCK_FILE, lock_bot, lock_bundle};
    use crate::session::error::SessionError;

    thread_local! {
        /// Every `try_lock` on this thread answers as a filesystem without locks does.
        pub(crate) static UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Runs `f` with this thread's locks refused as unsupported.
    pub(crate) fn without_locks<T>(f: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                UNSUPPORTED.set(false);
            }
        }
        UNSUPPORTED.set(true);
        let _reset = Reset;
        f()
    }

    /// A filesystem that cannot lock refuses the lock instead of handing out one that holds nothing (the
    /// cut-back of a failed batch needs it held); any other error is an ordinary I/O fault.
    #[test]
    fn an_unsupported_lock_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = without_locks(|| lock_bundle(dir.path(), "k7q")).expect_err("refused");
        assert!(matches!(&err, SessionError::LockUnsupported { dir: d } if d == dir.path()));
        assert_eq!(
            err.to_string(),
            format!(
                "file locking is not supported under {}; iota cannot open or delete a session there",
                dir.path().display()
            )
        );
        let bot = dir.path().join("bots").join("coder");
        assert!(matches!(
            without_locks(|| lock_bot(&bot, "coder")),
            Err(SessionError::LockUnsupported { .. })
        ));
        drop(lock_bundle(dir.path(), "k7q").expect("a real lock is taken"));
        assert!(super::cannot_lock(&std::io::Error::from_raw_os_error(
            super::EOPNOTSUPP
        )));
        assert!(!super::cannot_lock(&std::io::Error::from_raw_os_error(13)));
    }

    /// A resume or a delete on a filesystem without locks is refused and leaves the bundle as it was; the
    /// read-only load takes no lock and still reads it.
    #[test]
    fn a_resume_without_locks_is_refused() {
        use crate::provider::ProviderKind;
        use crate::provider::model::Message;
        use crate::session::{NewSession, SessionStore};
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut w = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create");
        w.append_messages(&[Message::user("hi")]).expect("write");
        let (id, log_path) = (w.id().to_owned(), w.dir().join("messages.jsonl"));
        drop(w);
        let log = std::fs::read(&log_path).expect("log");

        let err = without_locks(|| store.resume(&id, ProviderKind::OpenAi)).expect_err("refused");
        assert!(
            matches!(err, SessionError::LockUnsupported { .. }),
            "{err:?}"
        );
        let err = without_locks(|| store.delete(&id)).expect_err("refused");
        assert!(
            matches!(err, SessionError::LockUnsupported { .. }),
            "{err:?}"
        );
        assert_eq!(std::fs::read(&log_path).expect("still there"), log);
        let loaded = without_locks(|| store.load(&id, ProviderKind::OpenAi))
            .expect("a read-only load needs no lock");
        assert_eq!(loaded.messages.len(), 1);
    }

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
        // so a child forked in the meantime cannot keep the lock alive (see `HeldLock`).
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

    /// The bot lock refuses naming the bot, and creates the bot directory it lives in.
    #[test]
    fn bot_lock_names_the_bot() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("bots").join("coder");
        let held = lock_bot(&dir, "coder").expect("first lock");
        assert!(dir.join(BOT_LOCK_FILE).exists());
        let err = lock_bot(&dir, "coder").expect_err("second lock refused");
        assert!(matches!(
            &err,
            SessionError::Locked { what, pid } if what == "bot coder" && *pid == Some(std::process::id())
        ));
        assert_eq!(
            err.to_string(),
            format!(
                "bot coder is open in another iota process (pid {})",
                std::process::id()
            )
        );
        drop(held);
        drop(lock_bot(&dir, "coder").expect("re-lock after drop"));
    }
}
