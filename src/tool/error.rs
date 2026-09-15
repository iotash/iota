//! The tool-side error: `ToolError`, the hard (transport-level) tool failure; its Display texts are
//! model-facing.

use crate::BoxError;

/// A hard (transport-level) tool failure — rendered `Error calling tool: …` by the chat layer.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// No tool of that name is dispatched.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    /// The run was cancelled.
    #[error("interrupted")]
    Cancelled,
    /// MCP transport / JSON-RPC failure; Display is the inner text.
    #[error("{0}")]
    Transport(#[source] BoxError),
}
