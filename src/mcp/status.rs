//! Per-server status snapshot (mcp/manager.go:32-55): what `Manager::servers()` reports and what the host prints as
//! `Warning: mcp server <name>: <err>` — and, per skipped duplicate, `Warning: mcp server <name>: duplicate wire
//! tool name <wire>, skipping` (DIVERGENCES X-29).

use crate::mcp::naming::{WIRE_NAME_PREFIX, compose_wire_name};

/// The state of one configured MCP server, index-aligned with the manager's configs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerStatus {
    /// Configured server name.
    pub name: String,
    /// Sanitised, de-duplicated name segment used in wire names (empty until connected).
    pub segment: String,
    /// `endpoint_of(expanded config)`: the URL, or the command line.
    pub endpoint: String,
    /// Connect still in flight (true from `Manager::new` until the result is merged).
    pub pending: bool,
    /// Handshake and `tools/list` succeeded.
    pub connected: bool,
    /// Number of tools advertised by the server.
    pub tool_count: usize,
    /// Raw (un-namespaced) tool names.
    pub tools: Vec<String>,
    /// Failure text (a `McpError` Display); `None` on success.
    pub err: Option<String>,
    /// Wire names the merge SKIPPED because an earlier tool already registered them (the first registration
    /// wins, manager.go:303-335). A server listing the same tool twice is the realistic way to get one; the host
    /// turns each into a user-visible warning (DIVERGENCES X-29).
    pub duplicates: Vec<String>,
}

impl ServerStatus {
    /// The wire names the raw `tools` registered under, in the same order — the
    /// `wire name → server` oracle the Tools tab's source column reads.
    pub fn wire_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|raw| compose_wire_name(&self.segment, raw))
            .collect()
    }

    /// `"mcp__<segment>__"` when connected and the segment is non-empty, else `""`.
    pub fn wire_prefix(&self) -> String {
        if self.connected && !self.segment.is_empty() {
            format!("{WIRE_NAME_PREFIX}{}__", self.segment)
        } else {
            String::new()
        }
    }

    /// The NON-FATAL warnings a host prints for this server, one line per skipped duplicate and without any
    /// prefix — the connect failure is `err`, reported on its own. Both outlets print exactly these lines:
    /// `Warning: mcp server <name>: <line>` on the headless stderr, `⚠ MCP <name>: <line>` in the transcript.
    pub fn warnings(&self) -> Vec<String> {
        self.duplicates
            .iter()
            .map(|wire| format!("duplicate wire tool name {wire}, skipping"))
            .collect()
    }
}
