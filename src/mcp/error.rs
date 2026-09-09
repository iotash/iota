//! `McpError` — the per-server and per-call MCP failure texts (mcp/manager.go). None of these aborts a run: every
//! variant lands in `ServerStatus.err` or in a tool result. The only aborting MCP error, `--mcp: empty server
//! specification`, is `crate::mcp::config::McpFlagError::EmptyFlag`.

use std::time::Duration;

use crate::text::go_duration;

/// A per-server connect failure or a per-call transport failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// Neither `command` nor `url` was configured.
    #[error("server config must have either command or url")]
    MissingTarget,
    /// The URL is not `http://` / `https://`.
    #[error("unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
    /// Spawn / handshake / header failure (stdio servers append `\n  subprocess stderr:\n<trimmed>`).
    #[error("connect failed: {0}")]
    Connect(String),
    /// `tools/list` failed after a successful handshake (the session is closed first).
    #[error("list tools: {0}")]
    ListTools(String),
    /// The per-server connect deadline elapsed; displayed in Go duration form (`30s`, `300ms`).
    #[error("connection timed out after {}", go_duration(*.0))]
    Timeout(Duration),
    /// A `tools/call` failure: the rmcp `ServiceError` Display text, or one of the two texts below.
    #[error("{0}")]
    Call(String),
}

/// `McpError::Call` text for an SEP-2322 `input_required` response (DIVERGENCES D-33).
pub(crate) const MRTR_UNSUPPORTED: &str =
    "server requested client input (SEP-2322 input_required), which iota does not support";

/// `McpError::Call` text for an SEP-2663 task response (DIVERGENCES D-33).
pub(crate) const TASK_UNSUPPORTED: &str =
    "server returned a task (SEP-2663), which iota does not support";
