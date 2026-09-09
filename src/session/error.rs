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

impl From<serde_json::Error> for SessionError {
    /// A JSON encode/decode failure is a data fault on the bundle: it travels as an `Io` error of kind
    /// `InvalidData` so `cannot read session {id}: {e}` keeps serde's own text.
    fn from(e: serde_json::Error) -> Self {
        Self::Io(e.into())
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
