//! The MCP manager (mcp/manager.go:65-474): 30 s connect fan-out with a deterministic config-order merge, the live
//! namespaced tool view, call routing and close. Implements `crate::tool::Dispatcher`. With it, the two things
//! only it produces: the wire-name composition (`mcp__<segment>__<tool>`, manager.go:82-155) and the per-server
//! status snapshot (manager.go:32-55) that `servers()` reports and the hosts print.
//!
//! Lock discipline: `state` is a `std::sync::RwLock` that is never held across an `.await` — `call_tool` clones the
//! `Session` `Arc` and drops the guard before calling; `close` takes the sessions out under the write lock and closes
//! them outside it.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, PoisonError, RwLock},
    time::Duration,
};

use crate::BoxFuture;
use crate::app::env::Env;
use crate::mcp::config::{ServerConfig, endpoint_of, expand_server_config};
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::truncate_to_char_boundary;
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{Dispatcher, PrefixOf, ToolOutput, ToolResult};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::mcp::error::McpError;
use crate::mcp::transport::{CALL_INTERRUPTED, CLOSE_TIMEOUT, Session, connect_one};

/// Default per-server connect deadline (handshake + `tools/list`).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default cap on the captured stderr of a stdio server (POLICY §4: 64 KiB).
pub const DEFAULT_STDERR_CAP: usize = 64 * 1024;

/// `clientInfo.name` sent in the `initialize` handshake.
pub(crate) const CLIENT_NAME: &str = "iota";

/// `clientInfo.version` sent in the `initialize` handshake: the release, the same constant `--version`
/// prints (`app::VERSION`) — until 2026-09-15 this was a literal `"1.0.0"` that every server was told
/// regardless of the release.
pub(crate) const CLIENT_VERSION: &str = crate::app::VERSION;

// ---- wire names (mcp/manager.go:82-155): `mcp__<segment>__<tool>` with sanitisation, a 64-byte cap and a
// sha256 disambiguation suffix. Pure functions.

/// Maximum length of a wire tool name.
pub const WIRE_NAME_MAX_LEN: usize = 64;

/// Prefix of every MCP wire tool name.
pub(crate) const WIRE_NAME_PREFIX: &str = "mcp__";

/// Segment used when sanitisation leaves nothing.
pub(crate) const EMPTY_SEGMENT: &str = "srv";

/// Length of the disambiguation suffix: `'_'` plus 8 lowercase hex digits (4 bytes of sha256).
const SUFFIX_LEN: usize = 9;

/// Lowercase hex alphabet of the disambiguation suffix (Go `hex.EncodeToString`).
const HEX: [u8; 16] = *b"0123456789abcdef";

/// `[A-Za-z0-9]` kept; any other char (one per Unicode scalar) → `'_'` with runs collapsed; trim `'_'`; empty →
/// `"srv"`.
pub fn sanitize_name_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_underscore = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            prev_underscore = false;
            out.push(c);
        } else {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        EMPTY_SEGMENT.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `base = "mcp__" + segment + "__"`; `wire = base + sanitize(tool)`; if `wire == base + tool && len <= 64` → `wire`;
/// else `suffix = "_" + lowercase hex of sha256(base + RAW tool)[..4]`; if `len(wire) + 9 > 64` → `wire[..55]`;
/// `wire + suffix`.
pub(crate) fn compose_wire_name(segment: &str, tool: &str) -> String {
    let base = format!("{WIRE_NAME_PREFIX}{segment}__");
    let raw = format!("{base}{tool}");
    let mut wire = format!("{base}{}", sanitize_name_segment(tool));
    if wire == raw && wire.len() <= WIRE_NAME_MAX_LEN {
        return wire;
    }
    let digest = Sha256::digest(raw.as_bytes());
    let mut suffix = String::with_capacity(SUFFIX_LEN);
    suffix.push('_');
    for byte in &digest[..4] {
        suffix.push(char::from(HEX[usize::from(byte >> 4)]));
        suffix.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    if wire.len() + suffix.len() > WIRE_NAME_MAX_LEN {
        // Go slices bytes; the sanitised wire is ASCII for every sanitised segment, so the boundary-safe cut is the
        // same cut — it only differs (and avoids a panic) for an unsanitised non-ASCII segment.
        let cut = truncate_to_char_boundary(&wire, WIRE_NAME_MAX_LEN - suffix.len()).len();
        wire.truncate(cut);
    }
    wire.push_str(&suffix);
    wire
}

/// `compose_wire_name(sanitize_name_segment(server), tool)`.
pub fn wire_tool_name(server: &str, tool: &str) -> String {
    compose_wire_name(&sanitize_name_segment(server), tool)
}

// ---- the per-server status (mcp/manager.go:32-55): what `Manager::servers()` reports and what the host prints
// as `Warning: mcp server <name>: <err>` — and, per skipped duplicate, `Warning: mcp server <name>: duplicate
// wire tool name <wire>, skipping` (DIVERGENCES X-29).

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

/// Construction-time knobs of a `Manager`.
#[derive(Clone)]
pub struct ManagerOptions {
    /// Per-server connect deadline; `Elapsed` → `McpError::Timeout(connect_timeout)`.
    pub connect_timeout: Duration,
    /// HTTP client shared by every streamable-HTTP transport (built AFTER the TLS provider is installed).
    pub http: reqwest::Client,
    /// `clientInfo` of the `initialize` handshake.
    pub client_info: rmcp::model::Implementation,
    /// Cap on the captured stderr of a stdio server.
    pub stderr_cap: usize,
    /// The environment every config is expanded with (`expand_server_config`) before connecting.
    pub env: Env,
}

impl ManagerOptions {
    /// 30 s connect timeout, `iota/<CARGO_PKG_VERSION>` client info, 64 KiB stderr cap.
    pub fn new(http: reqwest::Client, env: Env) -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            http,
            client_info: rmcp::model::Implementation::new(CLIENT_NAME, CLIENT_VERSION),
            stderr_cap: DEFAULT_STDERR_CAP,
            env,
        }
    }
}

/// Connects to the configured servers, namespaces their tools and routes calls.
pub struct Manager {
    /// Server configs in CONFIG order (config-file servers sorted by name, then `--mcp` flags).
    configs: Vec<ServerConfig>,
    /// Construction-time knobs.
    opts: ManagerOptions,
    /// Mutable view; never held across an `.await`.
    state: RwLock<State>,
}

/// The manager's mutable view.
#[derive(Default)]
pub(crate) struct State {
    /// Live sessions; `None` once closed. Indexed by `ToolTarget::session`.
    pub(crate) sessions: Vec<Option<Arc<dyn Session>>>,
    /// Namespaced tool definitions in merge order.
    pub(crate) tools: Vec<ToolDef>,
    /// Wire name → owning session + raw name.
    pub(crate) tool_index: HashMap<String, ToolTarget>,
    /// Segments already assigned (collision → `{base}_{n}` from n = 2).
    pub(crate) segments: HashSet<String>,
    /// Statuses index-aligned with the configs.
    pub(crate) servers: Vec<ServerStatus>,
}

impl State {
    /// manager.go:274-285 — reserves the sanitised name, or the first free `{base}_{n}` from n = 2.
    fn assign_segment(&mut self, name: &str) -> String {
        let base = sanitize_name_segment(name);
        let mut seg = base.clone();
        let mut n = 2;
        while self.segments.contains(&seg) {
            seg = format!("{base}_{n}");
            n += 1;
        }
        self.segments.insert(seg.clone());
        seg
    }
}

/// Where a wire tool name routes.
pub(crate) struct ToolTarget {
    /// Index into `State::sessions`.
    pub(crate) session: usize,
    /// The raw (un-namespaced) tool name the server expects.
    pub(crate) raw: String,
}

/// Outcome of one server's connect attempt, merged by `Manager::merge_result`.
pub enum ServerResult {
    /// The connect failed; `status.err` carries the `McpError` text.
    Failed(ServerStatus),
    /// The handshake and `tools/list` succeeded.
    Connected {
        /// Status without `segment` (assigned at merge time).
        status: ServerStatus,
        /// The live connection.
        session: Arc<dyn Session>,
        /// Raw tool definitions (`name` un-namespaced, `deferred: false`).
        tools: Vec<ToolDef>,
    },
}

impl Manager {
    /// Does NOT connect. Seeds `servers[i] = {name, endpoint: endpoint_of(expanded), pending: true}` index-aligned
    /// with `configs`.
    pub fn new(configs: Vec<ServerConfig>, opts: ManagerOptions) -> Arc<Manager> {
        let servers = configs
            .iter()
            .map(|cfg| ServerStatus {
                name: cfg.name.clone(),
                endpoint: endpoint_of(&expand_server_config(cfg, &opts.env)),
                pending: true,
                ..ServerStatus::default()
            })
            .collect();
        Arc::new(Self {
            configs,
            opts,
            state: RwLock::new(State {
                servers,
                ..State::default()
            }),
        })
    }

    /// Headless entry (Go `ConnectWait`): `JoinSet` fan-out, per-server `timeout(connect_timeout, connect_one)`;
    /// results merged in CONFIG index order; returns the resolved statuses (index-aligned).
    pub async fn connect_all(self: &Arc<Self>, cancel: &CancellationToken) -> Vec<ServerStatus> {
        let mut set = JoinSet::new();
        let mut task_index = HashMap::with_capacity(self.configs.len());
        for i in 0..self.configs.len() {
            let this = Arc::clone(self);
            let cancel = cancel.clone();
            let handle = set.spawn(async move { (i, this.connect_indexed(i, &cancel).await) });
            task_index.insert(handle.id(), i);
        }

        let mut results: Vec<Option<ServerResult>> = self.configs.iter().map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            // A connect task that panicked or was aborted is one more kind of connect failure; `task_index` maps its
            // id back to the config slot.
            let resolved = match joined {
                Ok((i, r)) => Some((i, r)),
                Err(e) => task_index.get(&e.id()).map(|&i| {
                    (
                        i,
                        ServerResult::Failed(
                            self.failed_status(i, &McpError::Connect(e.to_string())),
                        ),
                    )
                }),
            };
            if let Some((i, r)) = resolved
                && let Some(slot) = results.get_mut(i)
            {
                *slot = Some(r);
            }
        }
        for (i, r) in results.into_iter().enumerate() {
            let r = r.unwrap_or_else(|| {
                ServerResult::Failed(
                    self.failed_status(i, &McpError::Connect("task vanished".to_owned())),
                )
            });
            self.merge_result(i, r);
        }
        self.servers()
    }

    /// Interactive entry (Go `Connect`, manager.go:197-208): starts every server's connect concurrently, one
    /// task each, and returns IMMEDIATELY so a cold `npx` server overlaps the model/session selection instead of
    /// blocking it. Each result is merged the moment it lands — its tools join the live set at once — and the
    /// resolved status is sent on the returned receiver, which CLOSES once every server has resolved (an empty
    /// config list closes it before the caller can poll).
    ///
    /// The channel is buffered to the server count, so a connect task never waits on a late consumer; the
    /// binary maps the statuses to `crate::repl::McpEvent` and the loop reports the failures once the transcript
    /// exists (`TUI_DESIGN` §8.2.7).
    pub fn connect_background(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> tokio::sync::mpsc::Receiver<ServerStatus> {
        let (tx, rx) = tokio::sync::mpsc::channel(self.configs.len().max(1));
        for i in 0..self.configs.len() {
            let this = Arc::clone(self);
            let cancel = cancel.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = this.connect_indexed(i, &cancel).await;
                let status = this.merge_result(i, result);
                // A receiver dropped before every server resolved is not an error: the run moved on.
                let _ = tx.send(status).await;
            });
        }
        rx
    }

    /// manager.go:244-252 — one server's attempt under the deadline and the run token, as a `ServerResult`.
    async fn connect_indexed(&self, i: usize, cancel: &CancellationToken) -> ServerResult {
        let Some(cfg) = self.configs.get(i) else {
            return ServerResult::Failed(
                self.failed_status(i, &McpError::Connect("no such server".to_owned())),
            );
        };
        let attempt = tokio::time::timeout(self.opts.connect_timeout, connect_one(cfg, &self.opts));
        let outcome = tokio::select! {
            r = attempt => r,
            () = cancel.cancelled() => {
                return ServerResult::Failed(self.failed_status(i, &McpError::Connect(CALL_INTERRUPTED.to_owned())));
            }
        };
        match outcome {
            Ok(Ok((session, tools))) => {
                let mut status = self.seed_status(i);
                status.connected = true;
                status.tool_count = tools.len();
                status.tools = tools.iter().map(|t| t.name.clone()).collect();
                ServerResult::Connected {
                    status,
                    session,
                    tools,
                }
            }
            Ok(Err(e)) => ServerResult::Failed(self.failed_status(i, &e)),
            Err(_elapsed) => ServerResult::Failed(
                self.failed_status(i, &McpError::Timeout(self.opts.connect_timeout)),
            ),
        }
    }

    /// `{name, endpoint}` of server `i` as seeded by `new` (no pending / connected / err).
    fn seed_status(&self, i: usize) -> ServerStatus {
        let st = self.state.read().unwrap_or_else(PoisonError::into_inner);
        st.servers
            .get(i)
            .map(|s| ServerStatus {
                name: s.name.clone(),
                endpoint: s.endpoint.clone(),
                ..ServerStatus::default()
            })
            .unwrap_or_default()
    }

    /// The seeded status of server `i` with `err` set to the error text.
    fn failed_status(&self, i: usize, err: &McpError) -> ServerStatus {
        let mut status = self.seed_status(i);
        status.err = Some(err.to_string());
        status
    }

    /// Snapshot of every server's status (index-aligned with the configs).
    pub fn servers(&self) -> Vec<ServerStatus> {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .servers
            .clone()
    }

    /// `crate::tool::PrefixOf` — scans `servers()` for the name and returns its `wire_prefix()`; queried lazily
    /// per call.
    pub fn prefix_of(self: &Arc<Self>) -> PrefixOf {
        let manager = Arc::clone(self);
        Arc::new(move |name: &str| {
            let st = manager.state.read().unwrap_or_else(PoisonError::into_inner);
            st.servers
                .iter()
                .find(|s| s.name == name)
                .map(ServerStatus::wire_prefix)
                .unwrap_or_default()
        })
    }

    /// Takes the sessions under the write lock (leaving `None`), clears the tool index, then closes each outside the
    /// lock with `timeout(10 s, ..)`. Idempotent.
    pub async fn close(&self) {
        let sessions: Vec<Arc<dyn Session>> = {
            let mut st = self.state.write().unwrap_or_else(PoisonError::into_inner);
            st.tool_index.clear();
            st.tools.clear();
            st.sessions.iter_mut().filter_map(Option::take).collect()
        };
        futures::future::join_all(
            sessions
                .iter()
                .map(|s| tokio::time::timeout(CLOSE_TIMEOUT, s.close())),
        )
        .await;
    }

    /// manager.go:303-335 — the test seam. `pending = false`; `Failed` → `servers[idx] = status`; `Connected` →
    /// session index = position among connected; `segment = assign_segment(name)` (`sanitize_name_segment`, then
    /// `{base}_{n}` from n = 2); for each tool: `wire = compose_wire_name(&segment, raw)`; duplicate → skipped
    /// (first wins) and recorded in `status.duplicates` for the host to warn about; else push + index;
    /// `servers[idx] = status` with the segment. Returns the merged status, which is what
    /// [`connect_background`](Self::connect_background) publishes.
    ///
    /// The duplicate used to be a `tracing::warn!` — which no subscriber received and which `release_max_level_off`
    /// compiled out of the shipped binary, so no user ever saw it (DIVERGENCES X-29). Warnings meant for the user
    /// travel in the status now; `tracing` is the developer's diagnostic channel (`IOTA_LOG`).
    pub(crate) fn merge_result(&self, idx: usize, r: ServerResult) -> ServerStatus {
        let mut st = self.state.write().unwrap_or_else(PoisonError::into_inner);
        let status = match r {
            ServerResult::Failed(mut status) => {
                status.pending = false;
                status.connected = false;
                status
            }
            ServerResult::Connected {
                mut status,
                session,
                tools,
            } => {
                status.pending = false;
                status.connected = true;
                let session_idx = st.sessions.len();
                st.sessions.push(Some(session));
                status.segment = st.assign_segment(&status.name);
                for td in tools {
                    let raw = td.name;
                    let wire = compose_wire_name(&status.segment, &raw);
                    if st.tool_index.contains_key(&wire) {
                        status.duplicates.push(wire);
                        continue;
                    }
                    st.tools.push(ToolDef {
                        name: wire.clone(),
                        ..td
                    });
                    st.tool_index.insert(
                        wire,
                        ToolTarget {
                            session: session_idx,
                            raw,
                        },
                    );
                }
                status
            }
        };
        if let Some(slot) = st.servers.get_mut(idx) {
            slot.clone_from(&status);
        }
        status
    }
}

impl Dispatcher for Manager {
    /// Clone of the namespaced tool list under the read lock.
    fn tools(&self) -> Vec<ToolDef> {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tools
            .clone()
    }

    /// Looks up the target, clones the `Session` `Arc`, DROPS the lock, then calls. Unknown or closed →
    /// `ToolError::UnknownTool`; `McpError` → `ToolError::Transport`; cancelled → `ToolError::Cancelled`. No other
    /// capability.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let target = {
                let st = self.state.read().unwrap_or_else(PoisonError::into_inner);
                st.tool_index.get(name).and_then(|t| {
                    st.sessions
                        .get(t.session)
                        .cloned()
                        .flatten()
                        .map(|session| (session, t.raw.clone()))
                })
            };
            let Some((session, raw)) = target else {
                return Err(ToolError::UnknownTool(name.to_owned()));
            };
            if cx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            match session.call_tool(&cx.cancel, &raw, args).await {
                Ok((text, is_error)) => Ok(ToolOutput { text, is_error }),
                Err(_) if cx.cancel.is_cancelled() => Err(ToolError::Cancelled),
                Err(e) => Err(ToolError::Transport(Box::new(e))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use crate::provider::model::{JsonObject, ToolDef};
    use crate::tool::Dispatcher;
    use crate::tool::context::RunCtx;
    use crate::tool::error::ToolError;
    use pretty_assertions::assert_eq;
    use tokio_util::sync::CancellationToken;

    use super::{Manager, ServerResult, ServerStatus};
    use crate::mcp::config::ServerConfig;
    use crate::mcp::testutil::{EchoSession, echo_server, options};
    use crate::mcp::transport::Session;

    /// A manager over `n` pending stdio configs named `names[i]` (never connected; the tests drive `merge_result`).
    fn manager(names: &[&str]) -> Arc<Manager> {
        let configs = names
            .iter()
            .map(|name| ServerConfig {
                name: (*name).to_owned(),
                command: "true".to_owned(),
                ..ServerConfig::default()
            })
            .collect();
        Manager::new(configs, options())
    }

    fn connected(name: &str, session: Arc<dyn Session>, tools: Vec<ToolDef>) -> ServerResult {
        ServerResult::Connected {
            status: ServerStatus {
                name: name.to_owned(),
                connected: true,
                tool_count: tools.len(),
                tools: tools.iter().map(|t| t.name.clone()).collect(),
                ..ServerStatus::default()
            },
            session,
            tools,
        }
    }

    fn echo_def(description: &str) -> ToolDef {
        ToolDef {
            name: "echo".to_owned(),
            description: description.to_owned(),
            input_schema: None,
            deferred: false,
        }
    }

    async fn call(m: &Manager, wire: &str) -> Result<(String, bool), ToolError> {
        let cx = RunCtx::new(CancellationToken::new());
        m.call_tool(&cx, wire, JsonObject::new())
            .await
            .map(|o| (o.text, o.is_error))
    }

    // Go: mcp/manager_test.go:204
    #[tokio::test]
    async fn test_manager_routes_same_tool_name_across_servers() {
        let ids = ["alpha", "beta"];
        let m = manager(&ids);
        for (i, id) in ids.iter().enumerate() {
            m.merge_result(
                i,
                connected(
                    id,
                    echo_server(id).await,
                    vec![echo_def("echo back the server id")],
                ),
            );
        }

        // Both tools survive registration under distinct wire names.
        let defs = m.tools();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["mcp__alpha__echo", "mcp__beta__echo"]);
        // ServerStatus keeps raw names and carries the assigned segment.
        for s in m.servers() {
            assert_eq!(
                s.tools,
                vec!["echo"],
                "server {} should list raw tool names",
                s.name
            );
            assert_eq!(s.segment, s.name, "server {}: assigned segment", s.name);
            assert!(s.connected && !s.pending);
        }

        // Each wire name reaches its own server, which sees the raw name "echo".
        for (wire, want) in [
            ("mcp__alpha__echo", "alpha:echo"),
            ("mcp__beta__echo", "beta:echo"),
        ] {
            let (text, is_err) = call(&m, wire).await.expect("call_tool");
            assert!(!is_err, "CallTool({wire}) isErr");
            assert_eq!(text, want, "CallTool({wire})");
        }

        // The raw name is not registered — only wire names are dispatchable.
        let err = call(&m, "echo").await.expect_err("raw name");
        assert!(
            matches!(err, ToolError::UnknownTool(ref n) if n == "echo"),
            "{err}"
        );
        assert_eq!(err.to_string(), "unknown tool: echo");
        m.close().await;
    }

    // Go: mcp/manager_test.go:261
    #[tokio::test]
    async fn test_manager_duplicate_server_names() {
        let ids = ["first", "second"];
        let m = manager(&["npx", "npx"]);
        for (i, id) in ids.iter().enumerate() {
            m.merge_result(
                i,
                connected("npx", echo_server(id).await, vec![echo_def("")]),
            );
        }

        let defs = m.tools();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["mcp__npx__echo", "mcp__npx_2__echo"]);
        let servers = m.servers();
        assert_eq!(servers[0].segment, "npx");
        assert_eq!(servers[1].segment, "npx_2");

        for (wire, want) in [
            ("mcp__npx__echo", "first:echo"),
            ("mcp__npx_2__echo", "second:echo"),
        ] {
            let (text, is_err) = call(&m, wire).await.expect("call_tool");
            assert!(!is_err, "CallTool({wire}) isErr");
            assert_eq!(text, want, "CallTool({wire})");
        }
        m.close().await;
    }

    // Go: mcp/manager_test.go:305
    #[tokio::test]
    async fn test_manager_sanitize_colliding_server_names() {
        let names = ["my-server", "my_server"];
        let m = manager(&names);
        for (i, name) in names.iter().enumerate() {
            m.merge_result(
                i,
                connected(name, echo_server(name).await, vec![echo_def("")]),
            );
        }

        let defs = m.tools();
        let got: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(got, vec!["mcp__my_server__echo", "mcp__my_server_2__echo"]);
        m.close().await;
    }

    // Go: mcp/manager_test.go:333 — the skipped duplicate travels in the status (X-29), not in a log line.
    #[test]
    fn test_manager_skips_duplicate_wire_name() {
        let m = manager(&["srv"]);
        let session: Arc<dyn Session> = Arc::new(EchoSession {
            id: "srv".to_owned(),
        });
        let merged = m.merge_result(
            0,
            connected("srv", session, vec![echo_def("first"), echo_def("second")]),
        );

        let defs = m.tools();
        assert_eq!(defs.len(), 1, "duplicate should be skipped: {defs:?}");
        assert_eq!(defs[0].name, "mcp__srv__echo");
        assert_eq!(defs[0].description, "first", "the first registration wins");
        assert_eq!(merged.duplicates, vec!["mcp__srv__echo"]);
        assert_eq!(
            merged.warnings(),
            vec!["duplicate wire tool name mcp__srv__echo, skipping"]
        );
        // The status still counts every listed tool (Go parity), and the published snapshot is the merged one.
        let s = &m.servers()[0];
        assert_eq!(s, &merged);
        assert_eq!(s.tool_count, 2);
        assert_eq!(s.tools, vec!["echo", "echo"]);
    }

    // Go: mcp/manager_test.go:367
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_manager_concurrent_merge_and_read() {
        const N: usize = 8;
        let names: Vec<String> = (0..N).map(|i| format!("srv{i}")).collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let m = manager(&name_refs);

        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let m = Arc::clone(&m);
            let stop = Arc::clone(&stop);
            readers.push(tokio::spawn(async move {
                let cx = RunCtx::new(CancellationToken::new());
                while !stop.load(Ordering::Relaxed) {
                    let _ = m.tools();
                    let _ = m.servers();
                    let _ = m.call_tool(&cx, "mcp__srv0__echo", JsonObject::new()).await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        let mut mergers = Vec::new();
        for (i, id) in names.iter().enumerate() {
            let m = Arc::clone(&m);
            let id = id.clone();
            mergers.push(tokio::spawn(async move {
                let session = echo_server(&id).await;
                m.merge_result(i, connected(&id, session, vec![echo_def("")]));
            }));
        }
        for h in mergers {
            h.await.expect("merger");
        }
        stop.store(true, Ordering::Relaxed);
        for h in readers {
            h.await.expect("reader");
        }

        // All servers resolved to connected with their tool registered.
        assert_eq!(m.tools().len(), N, "Tools() after all merges");
        for s in m.servers() {
            assert!(
                s.connected && !s.pending,
                "server {}: Connected={} Pending={}",
                s.name,
                s.connected,
                s.pending
            );
        }
        let (text, _) = call(&m, "mcp__srv0__echo")
            .await
            .expect("call after merges");
        assert_eq!(text, "srv0:echo");
        m.close().await;
    }

    // DIVERGENCES D-05: after `close()` the index is cleared, so a call is `unknown tool`, never a panic.
    #[tokio::test]
    async fn call_tool_after_close_is_unknown_tool() {
        let m = manager(&["alpha"]);
        m.merge_result(
            0,
            connected("alpha", echo_server("alpha").await, vec![echo_def("")]),
        );
        let (text, _) = call(&m, "mcp__alpha__echo").await.expect("before close");
        assert_eq!(text, "alpha:echo");

        m.close().await;
        assert!(m.tools().is_empty(), "tools() is empty after close");
        let err = call(&m, "mcp__alpha__echo").await.expect_err("after close");
        assert_eq!(err.to_string(), "unknown tool: mcp__alpha__echo");
        // The status is untouched by close; a second close is a no-op.
        assert!(m.servers()[0].connected);
        m.close().await;
    }

    // A cancelled run token short-circuits to `ToolError::Cancelled` before the session is reached.
    #[tokio::test]
    async fn call_tool_with_cancelled_token_is_cancelled() {
        let m = manager(&["alpha"]);
        let session: Arc<dyn Session> = Arc::new(EchoSession {
            id: "alpha".to_owned(),
        });
        m.merge_result(0, connected("alpha", session, vec![echo_def("")]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let cx = RunCtx::new(cancel);
        let err = m
            .call_tool(&cx, "mcp__alpha__echo", JsonObject::new())
            .await
            .expect_err("cancelled");
        assert!(matches!(err, ToolError::Cancelled), "{err}");
        assert_eq!(err.to_string(), "interrupted");
    }

    // `Manager::new` seeds one pending status per config with the expanded endpoint (manager.go:171-188).
    #[test]
    fn new_seeds_pending_statuses_in_config_order() {
        let m = Manager::new(
            vec![
                ServerConfig {
                    name: "fs".to_owned(),
                    command: "npx".to_owned(),
                    args: vec!["-y".to_owned(), "srv".to_owned()],
                    ..ServerConfig::default()
                },
                ServerConfig {
                    name: "https://x/mcp".to_owned(),
                    url: "https://x/mcp".to_owned(),
                    ..ServerConfig::default()
                },
            ],
            options(),
        );
        let servers = m.servers();
        assert_eq!(
            servers,
            vec![
                ServerStatus {
                    name: "fs".to_owned(),
                    endpoint: "npx -y srv".to_owned(),
                    pending: true,
                    ..ServerStatus::default()
                },
                ServerStatus {
                    name: "https://x/mcp".to_owned(),
                    endpoint: "https://x/mcp".to_owned(),
                    pending: true,
                    ..ServerStatus::default()
                },
            ]
        );
        assert!(m.tools().is_empty());
        // prefix_of answers "" until the server is connected.
        let prefix_of = m.prefix_of();
        assert_eq!(prefix_of("fs"), "");
        assert_eq!(prefix_of("nope"), "");
        let session: Arc<dyn Session> = Arc::new(EchoSession {
            id: "fs".to_owned(),
        });
        m.merge_result(0, connected("fs", session, vec![echo_def("")]));
        assert_eq!(prefix_of("fs"), "mcp__fs__");
        assert_eq!(prefix_of("https://x/mcp"), "");
        // A failed merge clears pending and records the error text.
        m.merge_result(
            1,
            ServerResult::Failed(ServerStatus {
                name: "https://x/mcp".to_owned(),
                endpoint: "https://x/mcp".to_owned(),
                err: Some("connect failed: nope".to_owned()),
                ..ServerStatus::default()
            }),
        );
        let s = &m.servers()[1];
        assert!(!s.pending && !s.connected);
        assert_eq!(s.err.as_deref(), Some("connect failed: nope"));
        assert_eq!(s.segment, "");
    }

    /// `connect_background` publishes EVERY server's resolved status and then closes the channel — the shape
    /// `iota_repl`'s MCP reporter task drains (WP51). The servers here connect to `true`, which exits before the
    /// handshake, so both resolve as failures without touching the network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_background_forwards_every_status_then_closes() {
        let m = manager(&["alpha", "beta"]);
        let mut rx = m.connect_background(&CancellationToken::new());
        let mut names = Vec::new();
        while let Some(status) = rx.recv().await {
            assert!(!status.pending, "{} still pending", status.name);
            assert!(!status.connected, "{} unexpectedly connected", status.name);
            assert!(
                status.err.is_some(),
                "{} carries no error text",
                status.name
            );
            names.push(status.name);
        }
        names.sort();
        assert_eq!(names, vec!["alpha".to_owned(), "beta".to_owned()]);
        // Closed, not merely drained: every connect task has dropped its sender.
        assert!(rx.recv().await.is_none());
        // The merge happened under the lock, so the snapshot agrees with what the channel published.
        assert!(m.servers().iter().all(|s| !s.pending && !s.connected));
    }

    /// No configured server: the receiver is closed from the start, so the reporter task ends immediately
    /// instead of parking for the life of the chat.
    #[tokio::test]
    async fn connect_background_with_no_servers_closes_at_once() {
        let m = manager(&[]);
        let mut rx = m.connect_background(&CancellationToken::new());
        assert!(rx.recv().await.is_none());
    }
}
