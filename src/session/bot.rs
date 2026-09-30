//! A bot's home and its pointer (docs/design/bot-mode.md §1.1, §2.1): `<bots>/<name>/bot.json` names the ONE
//! session that is the bot's body, and whether that session has ever reached the disk.
//!
//! The pointer is written before the bundle exists — the id is fixed from the first launch on — and is
//! rewritten once more, with `materialized: true`, when the bundle's first write lands. From then on a
//! bundle that cannot be found is lost, not "not yet created" (§2.7). Every write is `tmp + rename`.

use std::path::Path;

use crate::session::error::SessionError;

/// The pointer's file name inside a bot's directory.
pub const BOT_POINTER_FILE: &str = "bot.json";

/// `<bots>/<name>/bot.json`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BotPointer {
    /// The session id this bot is.
    pub session: String,
    /// The bundle has reached the disk at least once.
    pub materialized: bool,
}

impl BotPointer {
    /// A pointer at `session`, not yet materialised.
    pub fn new(session: &str) -> Self {
        Self {
            session: session.to_owned(),
            materialized: false,
        }
    }

    /// The pointer in `bot_dir`; `Ok(None)` when there is none. A pointer that cannot be read or parsed is
    /// an error — a bot whose body cannot be named must not quietly start a new one.
    pub fn read(bot_dir: &Path) -> Result<Option<Self>, SessionError> {
        let path = bot_dir.join(BOT_POINTER_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                SessionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SessionError::Io(e)),
        }
    }

    /// Writes the pointer into `bot_dir` atomically (the directory is created when missing).
    pub fn write(&self, bot_dir: &Path) -> Result<(), SessionError> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        crate::app::fs::write_atomic(&bot_dir.join(BOT_POINTER_FILE), &bytes, Some(0o644))?;
        Ok(())
    }
}

/// A bot name is a directory name: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (§1.1).
pub fn valid_bot_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Every bot directory's pointer under `bots`, as `(bot name, pointer)`, name-sorted. A missing `bots`
/// directory is no bots, a bot directory without a pointer is skipped; a pointer (or the directory) that
/// cannot be read is an error — "no owner" must never stand in for "cannot tell" (§2.7) — together with the
/// path that could not be read.
pub(crate) fn pointers(
    bots: &Path,
) -> Result<Vec<(String, BotPointer)>, (std::path::PathBuf, SessionError)> {
    let root = |e: std::io::Error| (bots.to_path_buf(), SessionError::Io(e));
    let entries = match std::fs::read_dir(bots) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(root(e)),
    };
    let mut out = Vec::new();
    for e in entries {
        let e = e.map_err(root)?;
        let dir = e.path();
        if !e.file_type().map_err(|e| (dir.clone(), e.into()))?.is_dir() {
            continue;
        }
        if let Some(ptr) = BotPointer::read(&dir).map_err(|e| (dir.join(BOT_POINTER_FILE), e))? {
            out.push((e.file_name().to_string_lossy().into_owned(), ptr));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{BOT_POINTER_FILE, BotPointer, pointers, valid_bot_name};

    /// Absent is `None`; a written pointer reads back; the file is the documented JSON shape.
    #[test]
    fn pointer_round_trips() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("coder");
        assert_eq!(BotPointer::read(&dir).expect("read"), None);
        let mut ptr = BotPointer::new("01KABC");
        ptr.write(&dir).expect("write");
        assert_eq!(
            std::fs::read_to_string(dir.join(BOT_POINTER_FILE)).expect("file"),
            "{\"session\":\"01KABC\",\"materialized\":false}\n"
        );
        ptr.materialized = true;
        ptr.write(&dir).expect("rewrite");
        assert_eq!(BotPointer::read(&dir).expect("read"), Some(ptr));
        // Nothing but the pointer is left behind: the temp file was renamed over it.
        let names: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(BOT_POINTER_FILE)]);
    }

    /// A corrupt pointer is an error that names the file, never "no pointer".
    #[test]
    fn a_corrupt_pointer_is_an_error() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(home.path().join(BOT_POINTER_FILE), "{oops").expect("write");
        let err = BotPointer::read(home.path()).expect_err("corrupt");
        assert!(err.to_string().contains(BOT_POINTER_FILE), "{err}");
        // Well-formed JSON without the session it must name is just as corrupt.
        std::fs::write(
            home.path().join(BOT_POINTER_FILE),
            "{\"materialized\":true}",
        )
        .expect("write");
        let err = BotPointer::read(home.path()).expect_err("no session");
        assert!(err.to_string().contains(BOT_POINTER_FILE), "{err}");
    }

    #[test]
    fn bot_names() {
        for ok in ["coder", "a", "A1", "my.bot_2-x", &"x".repeat(64)] {
            assert!(valid_bot_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "-x",
            "_x",
            "a/b",
            "a b",
            "..",
            "café",
            &"x".repeat(65),
        ] {
            assert!(!valid_bot_name(bad), "{bad}");
        }
    }

    /// The scan lists every pointer by directory name and skips a directory without one; a pointer that
    /// cannot be parsed fails the scan, naming the file (review R6: never "no owner").
    #[test]
    fn pointers_scan_the_bots_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(
            pointers(&home.path().join("absent"))
                .expect("absent")
                .is_empty()
        );
        BotPointer::new("s2")
            .write(&home.path().join("b"))
            .expect("b");
        BotPointer::new("s1")
            .write(&home.path().join("a"))
            .expect("a");
        std::fs::create_dir_all(home.path().join("empty")).expect("empty");
        let got: Vec<(String, String)> = pointers(home.path())
            .expect("scan")
            .into_iter()
            .map(|(n, p)| (n, p.session))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a".to_owned(), "s1".to_owned()),
                ("b".to_owned(), "s2".to_owned())
            ]
        );
        std::fs::create_dir_all(home.path().join("bad")).expect("bad");
        std::fs::write(home.path().join("bad").join(BOT_POINTER_FILE), "x").expect("bad ptr");
        let (path, err) = pointers(home.path()).expect_err("a corrupt pointer");
        assert_eq!(path, home.path().join("bad").join(BOT_POINTER_FILE));
        assert!(err.to_string().contains(BOT_POINTER_FILE), "{err}");
    }
}
