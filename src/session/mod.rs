//! The on-disk session bundle store (chat/session.go, plus the tuning replay of
//! `chat.ApplySessionTuning`).
//!
//! A session is a directory bundle:
//!
//! ```text
//! <root>/<id>/                       (or <root>/projects/<slug>/<id>/ in agent mode)
//!     meta.json                      small, rewritten on every change (temp + rename)
//!     messages.jsonl                 one compact JSON record per line, append-only
//!     attachments/<sha256 hex>       attachment bytes, content-addressed and deduped
//!     images/                        images generated inside this session (on demand)
//! ```
//!
//! The append-only log is the event store; `load_log` derives the replay view from it (the last system
//! record first, then the tail the last compaction marker retained). Nothing here reads the process
//! environment (ci.sh greps for it): a [`SessionStore`] is built from a root path or from an injected
//! [`HostDirs`](crate::app::HostDirs), and everything is synchronous `std::fs` under an fsync discipline —
//! no async runtime and no HTTP client in this module.

pub(crate) mod error;
pub(crate) mod id;
pub(crate) mod loader;
pub(crate) mod meta;
pub(crate) mod rawcodec;
pub(crate) mod record;
pub(crate) mod store;
pub(crate) mod tuning;
pub(crate) mod writer;

pub use error::SessionError;
pub use id::{SESSION_ID_ALPHABET, SESSION_ID_LENGTH, generate_id, resolve_in};
pub use loader::{
    LoadedLog, MAX_LOG_LINE, SUMMARY_PREFIX, SUMMARY_SEPARATOR, Session, load_full_history,
    load_log, record_to_message, scan_records, summary_preamble,
};
pub use meta::{
    META_FILE, META_TMP_FILE, SESSION_SCHEMA_VERSION, SessionMeta, now_rfc3339, parse_rfc3339,
};
pub use rawcodec::{blob_to_raw, raw_to_blob};
pub use record::{
    ATTACHMENTS_DIR, DATA_REF_PREFIX, IMAGES_DIR, LOG_FILE, ROLE_COMPACTION, SessionAttachment,
    SessionRaw, SessionRecord, SessionToolCall, SessionUsage,
};
pub use store::{PROJECTS_DIR_NAME, SessionInfo, SessionStore};
pub use tuning::{Overrides, apply_session_tuning, replay_session_settings};
pub use writer::SessionWriter;
