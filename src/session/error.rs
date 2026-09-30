//! The session store's error type (chat/session.go). Every `Display` text is byte-equal to the Go line it ports
//! (`{0:?}` reproduces Go's `%q` for ASCII — DIVERGENCES D-10).

/// Why a session-store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// No bundle with this id exists in either layout (chat/session.go:208).
    #[error("session {0} not found")]
    NotFound(String),
    /// `meta.json` could not be read or parsed (chat/session.go:390 `ResumeSession`, :911 `LoadSession`).
    #[error("cannot read session {id}: {source}")]
    CannotRead {
        /// The session id the caller asked for.
        id: String,
        /// The underlying read/parse failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The fragment matched no session (chat/session.go:257,274 — `errNoSessionMatch` wrapped). Scoped
    /// resolution widens to the global view on THIS variant and no other.
    #[error("no session matches {0:?}")]
    NoMatch(String),
    /// The fragment is a prefix of several ids; the candidates are joined with `", "` in listing order
    /// (chat/session.go:278).
    #[error("session id {0:?} is ambiguous: {1}")]
    Ambiguous(String, String),
    /// The id is not a bare bundle name — empty, or holding a separator or `..` (chat/session.go
    /// `DeleteSession`); it can never address anything outside the sessions root.
    #[error("invalid session id {0:?}")]
    InvalidId(String),
    /// Another process holds the bundle's single-writer lock (docs/design/bot-mode.md §2.3). `what` names
    /// the bundle (`session <id>`); `pid` is what the holder wrote into the lock file, `None` when it has
    /// not written it yet.
    #[error("{what} is open in another iota process{}", pid_suffix(*.pid))]
    Locked {
        /// What is locked, as the text names it.
        what: String,
        /// The holder's pid, when known.
        pid: Option<u32>,
    },
    /// Another process holds the bot's lock (`~/.iota/bots/<name>/lock`, §2.3): the bot is running. A
    /// variant of its own rather than a second spelling of [`Locked`](Self::Locked) — what is refused
    /// here is running the bot, not opening a bundle, and the sentence says so.
    #[error("bot {bot} is already running{}", pid_suffix(*.pid))]
    BotRunning {
        /// The bot's name.
        bot: String,
        /// The holder's pid, when known.
        pid: Option<u32>,
    },
    /// The session is a bot's body (its `bot.json` points at it, §2.7): only `iota run <bot>` opens it, and
    /// nothing deletes it while it is pointed at.
    #[error("session {id} belongs to bot {bot}; run iota run {bot}")]
    BotOwned {
        /// The session id.
        id: String,
        /// The bot whose pointer names it.
        bot: String,
    },
    /// The bot's session was saved once and is gone now (deleted, or the disk changed) — a hard error, never
    /// a silent fresh start (§2.2, review I1). The text names both ways out.
    #[error(
        "bot {bot}'s session {id} is missing: restore {}, or delete {} to start over (memory is kept)",
        .bundle.display(),
        .pointer.display()
    )]
    BotMissing {
        /// The bot's name.
        bot: String,
        /// The session id its pointer names.
        id: String,
        /// Where the bundle lived (`<sessions root>/<id>/`).
        bundle: std::path::PathBuf,
        /// The pointer to delete (`<bots>/<name>/bot.json`).
        pointer: std::path::PathBuf,
    },
    /// A batch reached the log, but `meta.json` could not be rewritten after it. The batch must NOT be
    /// appended again; the meta catches up with the next write.
    #[error("{0}")]
    MetaNotSaved(#[source] Box<SessionError>),
    /// Whether a session is a bot's body cannot be told: a pointer under the bots root (or the root itself)
    /// cannot be read (§2.7). Anything that would write or delete a bot's body is refused until it can —
    /// fail-closed, so ONE broken pointer blocks every session's resume and delete; the text names the file
    /// and the way out.
    #[error("cannot tell whether session {id} belongs to a bot: {source}; {}", owner_unknown_fix(.path))]
    BotOwnerUnknown {
        /// The session id.
        id: String,
        /// What could not be read: a `bot.json`, a bot directory, or the bots root.
        path: std::path::PathBuf,
        /// Why the pointers could not be read.
        #[source]
        source: Box<SessionError>,
    },
    /// A write reached the log before `ensure_created` opened it — a bug, not a state.
    #[error("session log is not open")]
    LogNotOpen,
    /// The log could not be scanned to the end — an oversized line or an I/O fault (chat/session.go:834).
    #[error("read session log: {0}")]
    ReadLog(#[source] std::io::Error),
    /// No home directory is known, so the sessions root cannot be resolved (internal/app/app.go:29,
    /// `os.UserHomeDir`; the same text `iota-chat` prints).
    #[error("$HOME is not defined")]
    HomeNotDefined,
    /// Any other filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// ` (pid N)` when the holder is known, nothing otherwise.
fn pid_suffix(pid: Option<u32>) -> String {
    pid.map_or_else(String::new, |p| format!(" (pid {p})"))
}

impl From<serde_json::Error> for SessionError {
    /// A JSON encode/decode failure is a data fault on the bundle: it travels as an `Io` error of kind
    /// `InvalidData` so `cannot read session {id}: {e}` keeps serde's own text.
    fn from(e: serde_json::Error) -> Self {
        Self::Io(e.into())
    }
}

/// The way out of [`SessionError::BotOwnerUnknown`] for `path`: a pointer is repaired or deleted (deleting it
/// lets its bundle go back to being an ordinary session, and the bot starts a new one); anything else must be
/// made readable again.
fn owner_unknown_fix(path: &std::path::Path) -> String {
    if path.file_name() == Some(std::ffi::OsStr::new(crate::session::bot::BOT_POINTER_FILE)) {
        format!(
            "repair or delete {} (without it the bot starts a new session; memory is kept)",
            path.display()
        )
    } else {
        format!("make {} readable", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::SessionError;

    /// Every Display is byte-equal to the Go line it ports (CONTRACTS S§5).
    #[test]
    fn display_texts_match_go() {
        assert_eq!(
            SessionError::NotFound("k7qz3xv9m2ht".to_owned()).to_string(),
            "session k7qz3xv9m2ht not found"
        );
        assert_eq!(
            SessionError::CannotRead {
                id: "k7q".to_owned(),
                source: "unexpected end of JSON input".into(),
            }
            .to_string(),
            "cannot read session k7q: unexpected end of JSON input"
        );
        assert_eq!(
            SessionError::NoMatch("zzz".to_owned()).to_string(),
            "no session matches \"zzz\""
        );
        assert_eq!(
            SessionError::Ambiguous("k7".to_owned(), "k7qz3xv9m2ht, k7ab00000000".to_owned())
                .to_string(),
            "session id \"k7\" is ambiguous: k7qz3xv9m2ht, k7ab00000000"
        );
        assert_eq!(
            SessionError::ReadLog(std::io::Error::other("token too long")).to_string(),
            "read session log: token too long"
        );
        assert_eq!(
            SessionError::HomeNotDefined.to_string(),
            "$HOME is not defined"
        );
        assert_eq!(
            SessionError::Io(std::io::Error::other("disk on fire")).to_string(),
            "disk on fire"
        );
        assert_eq!(
            SessionError::BotRunning {
                bot: "coder".to_owned(),
                pid: Some(4242),
            }
            .to_string(),
            "bot coder is already running (pid 4242)"
        );
        assert_eq!(
            SessionError::BotRunning {
                bot: "coder".to_owned(),
                pid: None,
            }
            .to_string(),
            "bot coder is already running"
        );
        assert_eq!(
            SessionError::BotOwned {
                id: "01K".to_owned(),
                bot: "coder".to_owned(),
            }
            .to_string(),
            "session 01K belongs to bot coder; run iota run coder"
        );
        assert_eq!(
            SessionError::BotMissing {
                bot: "coder".to_owned(),
                id: "01K".to_owned(),
                bundle: "/h/.iota/sessions/01K".into(),
                pointer: "/h/.iota/bots/coder/bot.json".into(),
            }
            .to_string(),
            "bot coder's session 01K is missing: restore /h/.iota/sessions/01K, or delete /h/.iota/bots/coder/bot.json to start over (memory is kept)"
        );
    }

    /// A serde failure arrives as `Io(InvalidData)` and keeps serde's text.
    #[test]
    fn serde_errors_travel_as_io() {
        let err: SessionError = serde_json::from_str::<serde_json::Value>("{oops")
            .expect_err("invalid json")
            .into();
        assert!(matches!(err, SessionError::Io(_)));
        assert!(err.to_string().contains("key must be a string"));
    }
}
