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
/// `Err` inside: the filesystem cannot lock at all (NFS or SMB without lock support — fable M5), so nothing
/// is held; it carries the directory the lock file is in, for [`HeldLock::caution`].
#[derive(Debug)]
pub(crate) struct HeldLock(Result<std::fs::File, std::path::PathBuf>);

impl HeldLock {
    /// What to tell the user when the filesystem refused to lock at all — a second process would not be
    /// stopped; `None` for a lock that is really held.
    pub(crate) fn caution(&self) -> Option<String> {
        let dir = self.0.as_ref().err()?;
        Some(format!(
            "file locking is not supported under {}; opened without a lock, so do not open it from a second iota process",
            dir.display()
        ))
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        // Nothing to do on failure: the close that follows is the fallback it always was.
        if let Ok(file) = &self.0 {
            let _ = file.unlock();
        }
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

/// Takes the bot lock in `bot_dir` (created when missing) or refuses with [`SessionError::BotRunning`].
pub(crate) fn lock_bot(bot_dir: &Path, bot: &str) -> Result<HeldLock, SessionError> {
    std::fs::create_dir_all(bot_dir)?;
    try_lock_file(&bot_dir.join(BOT_LOCK_FILE))?.map_err(|pid| SessionError::BotRunning {
        bot: bot.to_owned(),
        pid,
    })
}

/// One `try_lock` on `path`: the held lock (with this process's pid written in), or — when another
/// handle holds it — the pid that holder wrote. The outer error is an I/O fault, not a conflict.
///
/// A filesystem that cannot lock at all (`ENOTSUP` / `EOPNOTSUPP`: NFS or SMB without lock support) is not
/// a fault: the answer is a guard that holds nothing ([`HeldLock::caution`]), and the caller cautions (fable M5) — a
/// session directory there would otherwise open nothing at all.
fn try_lock_file(path: &Path) -> std::io::Result<Result<HeldLock, Option<u32>>> {
    let mut file = open_lock_file(path)?;
    match try_lock(&file) {
        Ok(()) => {
            // Best effort: the pid is for the error text only, so a failed write does not fail the lock.
            let _ = write_pid(&mut file);
            Ok(Ok(HeldLock(Ok(file))))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(Err(read_pid(&mut file))),
        Err(std::fs::TryLockError::Error(e)) if cannot_lock(&e) => Ok(Ok(HeldLock(Err(path
            .parent()
            .unwrap_or(path)
            .to_path_buf())))),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
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

    /// Fable M5: a filesystem that cannot lock opens unlocked instead of failing; any other error still fails.
    #[test]
    fn an_unsupported_lock_opens_unlocked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = without_locks(|| lock_bundle(dir.path(), "k7q")).expect("opens");
        assert_eq!(
            held.caution(),
            Some(format!(
                "file locking is not supported under {}; opened without a lock, so do not open it from a second iota process",
                dir.path().display()
            ))
        );
        drop(held);
        assert!(
            lock_bundle(dir.path(), "k7q")
                .expect("locks")
                .caution()
                .is_none()
        );
        assert!(super::cannot_lock(&std::io::Error::from_raw_os_error(
            super::EOPNOTSUPP
        )));
        assert!(!super::cannot_lock(&std::io::Error::from_raw_os_error(13)));
    }

    /// Fable M5: a session directory on a filesystem without locks still resumes — with the caution the
    /// caller prints.
    #[test]
    fn a_resume_without_locks_opens_with_a_caution() {
        use crate::provider::ProviderKind;
        use crate::provider::model::Message;
        use crate::session::{NewSession, SessionStore};
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut w = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create");
        w.append_messages(&[Message::user("hi")]).expect("write");
        let (id, dir) = (w.id().to_owned(), w.dir().to_path_buf());
        drop(w);

        let (w, session) =
            without_locks(|| store.resume(&id, ProviderKind::OpenAi)).expect("resumes");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            w.lock_cautions(),
            [format!(
                "file locking is not supported under {}; opened without a lock, so do not open it from a second iota process",
                dir.display()
            )]
        );
        drop(w);
        let (w, _) = store.resume(&id, ProviderKind::OpenAi).expect("resumes");
        assert!(w.lock_cautions().is_empty(), "a real lock says nothing");
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

    /// The bot lock refuses with the bot's own sentence, and creates the bot directory it lives in.
    #[test]
    fn bot_lock_names_the_bot() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("bots").join("coder");
        let held = lock_bot(&dir, "coder").expect("first lock");
        assert!(dir.join(BOT_LOCK_FILE).exists());
        let err = lock_bot(&dir, "coder").expect_err("second lock refused");
        assert!(matches!(
            &err,
            SessionError::BotRunning { bot, pid } if bot == "coder" && *pid == Some(std::process::id())
        ));
        assert_eq!(
            err.to_string(),
            format!("bot coder is already running (pid {})", std::process::id())
        );
        drop(held);
        drop(lock_bot(&dir, "coder").expect("re-lock after drop"));
    }
}
