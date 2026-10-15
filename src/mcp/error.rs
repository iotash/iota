//! `McpError` — the per-server and per-call MCP failure texts (mcp/manager.go). None of these aborts a run: every
//! variant lands in `ServerState::Failed` or in a tool result. The only aborting MCP error, `--mcp: empty server
//! specification`, is `crate::mcp::config::McpFlagError::EmptyFlag`.
//!
//! Every text is written for the user (DIVERGENCES X-77): its FIRST line says what happened and what to do,
//! without the server's name (the outlet puts it in front); the lines after it are the detail
//! (`crate::mcp::explain`). Until 2026-10-15 the variants prefixed rmcp's own text — `connect failed: Send
//! message error Transport [rmcp::transport::worker::WorkerTransport<…>] error: …, when send initialize request`.

/// A per-server connect failure or a per-call transport failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// Neither `command` nor `url` was configured.
    #[error("the entry has neither a command nor a url — give it one")]
    MissingTarget,
    /// The URL is not `http://` / `https://`.
    #[error("the url must start with http:// or https:// (it is {0})")]
    UnsupportedScheme(String),
    /// Spawn / handshake / header failure, worded by `crate::mcp::explain`: the summary line, then the detail
    /// lines (the cause chain, a stdio server's stderr).
    #[error("{0}")]
    Connect(String),
    /// `tools/list` failed after a successful handshake (the session is closed first); worded like `Connect`.
    #[error("{0}")]
    ListTools(String),
    /// A server that wants a login nobody has done: an `auth: oauth` server with no usable token (none
    /// stored, or one the server rejected and the store could not refresh), or an `auto` server whose bare
    /// handshake the server answered 401/403. The text carries the way back in (`iota mcp login <name>`) —
    /// the same single server degrades, nothing else in the run is touched (brain page
    /// `mcp-graceful-degradation`).
    #[error("{}", crate::mcp::auth::not_logged_in(.0))]
    NotLoggedIn(String),
    /// The per-server connect deadline elapsed: `crate::mcp::explain::timeout`'s sentence, the deadline in
    /// words (`30 seconds`, `300 ms`).
    #[error("{0}")]
    Timeout(String),
    /// The connection is gone — a stdio server that exited, a transport that closed. The manager marks the server
    /// down on it (`ServerState::Down`) and words the call's failure itself.
    #[error("disconnected (it exited or closed the connection)")]
    Disconnected,
    /// A `tools/call` failure: `crate::mcp::explain::call`'s sentence, `interrupted`, or one of the two texts below.
    #[error("{0}")]
    Call(String),
}

/// `McpError::Call` text for an SEP-2322 `input_required` response (DIVERGENCES D-33).
pub(crate) const MRTR_UNSUPPORTED: &str =
    "server requested client input (SEP-2322 input_required), which iota does not support";

/// `McpError::Call` text for an SEP-2663 task response (DIVERGENCES D-33).
pub(crate) const TASK_UNSUPPORTED: &str =
    "server returned a task (SEP-2663), which iota does not support";
