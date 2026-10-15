//! One live MCP connection (mcp/manager.go:337-398,478-512): `connect_one` builds the rmcp transport (stdio child
//! process or streamable HTTP), runs the `initialize` handshake and lists the tools; `Session` is the crate-private
//! seam the manager routes calls through, and `RmcpSession` is its production implementation: ONE `tools/call`
//! request under the call deadline, never `Peer::call_tool`, which drives SEP-2322 MRTR rounds (DIVERGENCES D-33).

use std::{borrow::Cow, sync::Arc, time::Duration};

use crate::BoxFuture;
use crate::mcp::config::{AuthMode, ServerConfig, expand_server_config};
use crate::provider::model::{JsonObject, ToolDef};
use rmcp::{
    RoleClient, ServiceExt,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientInfo, ClientRequest,
        ContentBlock, ServerResult,
    },
    service::{Peer, PeerRequestOptions, RunningService, ServiceError},
};
use tokio_util::sync::CancellationToken;

use crate::mcp::error::{MRTR_UNSUPPORTED, McpError, TASK_UNSUPPORTED};
use crate::mcp::explain;
use crate::mcp::manager::ManagerOptions;
use crate::sync::lock;

/// Upper bound on one session's close (rmcp: transport close → stdin EOF → 3 s → kill).
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a failed stdio handshake waits for the child's stderr to reach EOF before it reports
/// without the appendix. A child whose handshake failed has almost always exited already, so this
/// is scheduling slack, not a real wait; the bound exists for the case where a grandchild inherited
/// the pipe and holds it open, which must not stall the error.
pub(crate) const STDERR_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// `McpError::Call` text of a `tools/call` abandoned because the run was cancelled; the manager maps it to
/// `ToolError::Cancelled` when the token is set.
pub(crate) const CALL_INTERRUPTED: &str = "interrupted";

/// Crate-private seam over one live connection (`RmcpSession` in production; duplex echo server in unit tests).
pub trait Session: Send + Sync {
    /// `tools/call` of the RAW tool name, selected against `cancel`; `Ok((joined text, is_error))`.
    /// `Err(McpError::Disconnected)` when the connection is gone — the server exited or closed it.
    fn call_tool<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        raw: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, Result<(String, bool), McpError>>;

    /// What a stdio server last wrote to stderr (the tail the capture kept), after a bounded wait for the
    /// pipe to reach EOF — what explains a server that exited. Empty for every other kind of session.
    fn stderr_tail(&self) -> BoxFuture<'_, Vec<u8>> {
        Box::pin(std::future::ready(Vec::new()))
    }

    /// Closes the connection; a second call is a no-op.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// The production `Session`. `RunningService::cancel(self)` CONSUMES the service, so it sits behind an async mutex;
/// `call_tool` uses the cloned `Peer` and never takes the lock; `close` takes the service OUT of the mutex (leaving
/// `None`) and awaits `timeout(10 s, running.cancel())` — a second `close` is a no-op.
pub(crate) struct RmcpSession {
    /// The served connection; `None` once closed.
    running: tokio::sync::Mutex<Option<RunningService<RoleClient, ClientInfo>>>,
    /// Cloned peer used by every call (no lock).
    peer: Peer<RoleClient>,
    /// How long one `tools/call` may go unanswered (`ManagerOptions::call_timeout`, or the entry's
    /// `tool_timeout_sec`).
    call_timeout: Duration,
    /// A stdio server's stderr, captured for the session's whole life; `None` for HTTP.
    stderr: Option<StderrCapture>,
}

impl RmcpSession {
    /// Wraps a freshly served connection, cloning its peer: the default call deadline, no stderr.
    pub(crate) fn new(running: RunningService<RoleClient, ClientInfo>) -> Self {
        let peer = running.peer().clone();
        Self {
            running: tokio::sync::Mutex::new(Some(running)),
            peer,
            call_timeout: crate::mcp::manager::DEFAULT_CALL_TIMEOUT,
            stderr: None,
        }
    }

    /// The call deadline.
    pub(crate) fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }

    /// The stdio server's stderr capture, kept so an exit can be explained.
    fn with_stderr(mut self, stderr: Option<StderrCapture>) -> Self {
        self.stderr = stderr;
        self
    }
}

/// manager.go:450-459 — every `TextContent.text` in content order joined by `"\n"`; other block kinds are dropped.
fn join_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(ContentBlock::as_text)
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

impl Session for RmcpSession {
    /// ONE `tools/call` request (`CallToolRequestParams::new(raw).with_arguments(args)`) — what rmcp's
    /// `call_tool_once` sends — but under `PeerRequestOptions::with_timeout(call_timeout)`, so a server that
    /// never answers ends in `ServiceError::Timeout` (rmcp then sends it `notifications/cancelled`) instead of
    /// holding the turn forever; selected against the token. A call result → `(text blocks joined by "\n",
    /// is_error.unwrap_or(false))`; `input_required` → `Err(McpError::Call(MRTR_UNSUPPORTED.into()))`; a task →
    /// `Err(McpError::Call(TASK_UNSUPPORTED.into()))`; a closed connection → `Err(McpError::Disconnected)`;
    /// any other `ServiceError` → `McpError::Call(explain::call(&e))`, the sentence that follows the server's
    /// name.
    fn call_tool<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        raw: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, Result<(String, bool), McpError>> {
        Box::pin(async move {
            let params = CallToolRequestParams::new(raw.to_owned()).with_arguments(args);
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let call = async {
                self.peer
                    .send_request_with_option(
                        request,
                        PeerRequestOptions::with_timeout(self.call_timeout),
                    )
                    .await?
                    .await_response()
                    .await
            };
            let response = tokio::select! {
                r = call => r,
                () = cancel.cancelled() => return Err(McpError::Call(CALL_INTERRUPTED.to_owned())),
            };
            match response {
                Ok(ServerResult::CallToolResult(r)) => {
                    Ok((join_text(&r), r.is_error.unwrap_or(false)))
                }
                Ok(ServerResult::InputRequiredResult(_)) => {
                    Err(McpError::Call(MRTR_UNSUPPORTED.to_owned()))
                }
                Ok(ServerResult::CreateTaskResult(_)) => {
                    Err(McpError::Call(TASK_UNSUPPORTED.to_owned()))
                }
                Ok(_) => Err(McpError::Call(explain::call(
                    &ServiceError::UnexpectedResponse,
                ))),
                Err(e) if explain::is_disconnect(&e) => Err(McpError::Disconnected),
                Err(e) => Err(McpError::Call(explain::call(&e))),
            }
        })
    }

    fn stderr_tail(&self) -> BoxFuture<'_, Vec<u8>> {
        Box::pin(async move {
            match &self.stderr {
                Some(capture) => capture.bytes().await,
                None => Vec::new(),
            }
        })
    }

    /// `timeout(10 s, running.cancel())` on the service taken out of the mutex (rmcp: transport close → stdin EOF →
    /// 3 s → kill).
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let running = self.running.lock().await.take();
            if let Some(running) = running {
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, running.cancel()).await;
            }
        })
    }
}

/// Captured stderr of a stdio server: the buffer a spawned task drains into — its LAST `stderr_cap` bytes,
/// the part that explains an exit — plus that task's handle, so a failed handshake or an exit can wait for
/// the pipe to reach EOF before it reads. Reading the buffer without that wait is a race the child usually
/// loses — the bytes are still in the pipe, or the task has not been scheduled yet — and the reason the
/// text promises then appears only sometimes, which is the same as not promising it. Until 2026-10-15 the
/// cap kept the FIRST bytes and the capture died with the handshake; it now lives as long as the session.
pub(crate) struct StderrCapture {
    /// What the drain task has captured: the tail, at most `stderr_cap` bytes.
    buf: Arc<std::sync::Mutex<Vec<u8>>>,
    /// The drain task; it completes at EOF. `None` when the child had no stderr pipe, or once waited for.
    drain: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl StderrCapture {
    /// The captured bytes, after waiting up to [`STDERR_DRAIN_GRACE`] for the drain to reach EOF (the
    /// first time only). On timeout the handle is dropped, which only detaches the task — it goes on
    /// keeping the tail for as long as the pipe is open.
    async fn bytes(&self) -> Vec<u8> {
        let drain = lock(&self.drain).take();
        if let Some(drain) = drain {
            let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, drain).await;
        }
        lock(&self.buf).clone()
    }
}

/// manager.go:337-398 — expand (`opts.resolver`) → endpoint → transport → `ClientInfo::new(
/// ClientCapabilities::default(), opts.client_info.clone()).serve(transport)` → `peer().list_all_tools()` →
/// `ToolDef { name: raw, description: unwrap_or_default, input_schema: Some((*input_schema).clone()), deferred: false }`,
/// the schema passed through [`schema::normalize`](crate::mcp::schema::normalize).
///
/// Transport selection: url non-empty → must start `http://` / `https://` else `UnsupportedScheme(url)`, then
/// by the entry's [`effective_auth`](ServerConfig::effective_auth) — `oauth`: [`connect_oauth`]; `none`:
/// [`http_transport`] bare, every failure a `Connect`; `auto`: [`connect_oauth`] when a token file exists for
/// the name (whatever it holds — a stale one is that path's own `NotLoggedIn`), else bare, where a 401/403 at
/// the handshake ([`is_auth_required`]) is `NotLoggedIn(name)` — the server asked for a login nobody has done
/// yet — and anything else `Connect`. Command non-empty → `spawn_stdio`; both empty → `MissingTarget`. A
/// handshake failure is `Connect`, worded by [`explain::handshake`] (stdio: with the captured stderr, after a
/// bounded wait for the pipe to reach EOF — [`STDERR_DRAIN_GRACE`] — its last line in the summary and the
/// rest in the detail); a `tools/list` failure closes the session and is `ListTools`. The caller wraps this in
/// `timeout(connect_timeout, ..)` (`Elapsed` → `Timeout`).
pub(crate) async fn connect_one(
    server_cfg: &ServerConfig,
    opts: &ManagerOptions,
) -> Result<(Arc<dyn Session>, Vec<ToolDef>), McpError> {
    let server_cfg = expand_server_config(server_cfg, &opts.env);

    if !server_cfg.url.is_empty() {
        if !(server_cfg.url.starts_with("http://") || server_cfg.url.starts_with("https://")) {
            return Err(McpError::UnsupportedScheme(server_cfg.url));
        }
        let discover = match server_cfg.effective_auth() {
            AuthMode::Oauth => return connect_oauth(&server_cfg, opts).await,
            AuthMode::Auto => {
                if has_token_file(&server_cfg, opts) {
                    return connect_oauth(&server_cfg, opts).await;
                }
                true
            }
            AuthMode::None => false,
        };
        let transport = http_transport(&server_cfg, opts.http.clone())?;
        let running = match client_info(opts).serve(transport).await {
            Ok(running) => running,
            Err(e) if discover && is_auth_required(&e) => {
                return Err(McpError::NotLoggedIn(server_cfg.name.clone()));
            }
            Err(e) => {
                return Err(McpError::Connect(
                    explain::handshake(&server_cfg, &e, &[]).into_text(),
                ));
            }
        };
        return list_tools(&server_cfg, opts, running, None).await;
    }

    if !server_cfg.command.is_empty() {
        let (transport, stderr) = spawn_stdio(&server_cfg, opts.stderr_cap)?;
        let running = match client_info(opts).serve(transport).await {
            Ok(running) => running,
            Err(e) => {
                // Bounded wait FIRST: the handshake can fail before the child's stderr has even
                // been read, and a reason that depends on which of the two wins is a diagnostic
                // the user cannot rely on.
                let captured = stderr.bytes().await;
                return Err(McpError::Connect(
                    explain::handshake(&server_cfg, &e, &captured).into_text(),
                ));
            }
        };
        return list_tools(&server_cfg, opts, running, Some(stderr)).await;
    }

    Err(McpError::MissingTarget)
}

/// Whether a token file exists for the server's name — what turns an `auto` entry's connect into an OAuth
/// one. Its content is not judged here: a file with nothing usable in it is [`connect_oauth`]'s own
/// `NotLoggedIn`, which is the right answer for it too.
fn has_token_file(server_cfg: &ServerConfig, opts: &ManagerOptions) -> bool {
    crate::mcp::auth::TokenStore::for_server(&opts.env.dirs, &server_cfg.name, &server_cfg.url)
        .is_some_and(|store| store.path().is_file())
}

/// The HTTP branch of a server that logs in (`auth: oauth`, or `auto` with a token file): the transport's
/// client is rmcp's `AuthClient` over the run's reqwest client and the store-backed manager
/// (`mcp::auth::runtime_manager`), which puts the bearer token on every request and refreshes it when the
/// server rejects it. No token in the store, or a handshake the server answers 401 after the refresh failed
/// too, is `NotLoggedIn` — the one text that says what to do.
async fn connect_oauth(
    server_cfg: &ServerConfig,
    opts: &ManagerOptions,
) -> Result<(Arc<dyn Session>, Vec<ToolDef>), McpError> {
    use rmcp::transport::auth::AuthClient;
    use rmcp::transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    };

    let store =
        crate::mcp::auth::TokenStore::for_server(&opts.env.dirs, &server_cfg.name, &server_cfg.url)
            .ok_or_else(|| McpError::NotLoggedIn(server_cfg.name.clone()))?;
    let manager = crate::mcp::auth::runtime_manager(
        opts.http.clone(),
        store,
        Some(server_cfg.client_secret.as_str()),
    )
    .await
    .map_err(|e| {
        McpError::Connect(format!(
            "can't use the stored login: {e} — run iota mcp login {}",
            server_cfg.name
        ))
    })?
    .ok_or_else(|| McpError::NotLoggedIn(server_cfg.name.clone()))?;
    let headers = parse_headers(server_cfg)?;
    let config = StreamableHttpClientTransportConfig::with_uri(server_cfg.url.as_str())
        .custom_headers(headers);
    let client = AuthClient::new(opts.http.clone(), manager);
    let transport = StreamableHttpClientTransport::with_client(client, config);
    let running = match client_info(opts).serve(transport).await {
        Ok(running) => running,
        Err(e) if is_auth_required(&e) => {
            return Err(McpError::NotLoggedIn(server_cfg.name.clone()));
        }
        Err(e) => {
            return Err(McpError::Connect(match auth_failure(&e) {
                Some(text) => format!(
                    "the login failed: {text} — run iota mcp login {}",
                    server_cfg.name
                ),
                None => explain::handshake(server_cfg, &e, &[]).into_text(),
            }));
        }
    };
    list_tools(server_cfg, opts, running, None).await
}

/// The OAuth failure inside a handshake error, when that is what it is — a refresh the server would not do,
/// an exchange it refused — as rmcp's own sentence, without the transport wrapper around it. `None` for
/// every other failure.
fn auth_failure(e: &rmcp::service::ClientInitializeError) -> Option<String> {
    use rmcp::transport::streamable_http_client::StreamableHttpError;

    let rmcp::service::ClientInitializeError::TransportError { error, .. } = e else {
        return None;
    };
    match error
        .error
        .downcast_ref::<StreamableHttpError<reqwest::Error>>()?
    {
        StreamableHttpError::Auth(auth) => Some(auth.to_string()),
        _ => None,
    }
}

/// Whether a handshake failure is the server asking for a login: a 401 challenge or rmcp's own "authorization
/// required", a 403 challenge (`insufficient_scope` — a token it wants more from), or a bare 401/403 with no
/// `WWW-Authenticate` at all — which rmcp's POST path reports as an unexpected `HTTP 401 …` / `HTTP 403 …`
/// response (its text is the only handle) and its GET path as the reqwest status error.
fn is_auth_required(e: &rmcp::service::ClientInitializeError) -> bool {
    use rmcp::transport::auth::AuthError;
    use rmcp::transport::streamable_http_client::StreamableHttpError;

    let rmcp::service::ClientInitializeError::TransportError { error, .. } = e else {
        return false;
    };
    let Some(err) = error
        .error
        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
    else {
        return false;
    };
    match err {
        StreamableHttpError::AuthRequired(_)
        | StreamableHttpError::InsufficientScope(_)
        | StreamableHttpError::Auth(
            AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_),
        ) => true,
        StreamableHttpError::Client(e) => matches!(
            e.status(),
            Some(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN)
        ),
        StreamableHttpError::UnexpectedServerResponse(text) => {
            text.starts_with("HTTP 401 ") || text.starts_with("HTTP 403 ")
        }
        _ => false,
    }
}

/// The `initialize` handshake handler: rmcp's default capabilities (no `roots.listChanged`, D-02) and the configured
/// `clientInfo` (`iota/<CARGO_PKG_VERSION>`).
fn client_info(opts: &ManagerOptions) -> ClientInfo {
    ClientInfo::new(
        rmcp::model::ClientCapabilities::default(),
        opts.client_info.clone(),
    )
}

/// manager.go:371-395 — `tools/list` (auto-paginated) on a served connection; a failure closes the session and is
/// `ListTools`, worded by [`explain::list_tools`]. The session gets the entry's call deadline
/// (`tool_timeout_sec`, else the manager's) and, for a stdio server, its stderr capture.
async fn list_tools(
    server_cfg: &ServerConfig,
    opts: &ManagerOptions,
    running: RunningService<RoleClient, ClientInfo>,
    stderr: Option<StderrCapture>,
) -> Result<(Arc<dyn Session>, Vec<ToolDef>), McpError> {
    let tools = match running.peer().list_all_tools().await {
        Ok(tools) => tools,
        Err(e) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, running.cancel()).await;
            return Err(McpError::ListTools(
                explain::list_tools(server_cfg, &e).into_text(),
            ));
        }
    };
    let defs = tools
        .into_iter()
        .map(|t| ToolDef {
            name: t.name.into_owned(),
            description: t.description.map(Cow::into_owned).unwrap_or_default(),
            input_schema: Some({
                let mut schema = (*t.input_schema).clone();
                crate::mcp::schema::normalize(&mut schema);
                schema
            }),
            deferred: false,
        })
        .collect();
    let call_timeout = server_cfg
        .tool_timeout_sec
        .map_or(opts.call_timeout, Duration::from_secs);
    let session = RmcpSession::new(running)
        .with_call_timeout(call_timeout)
        .with_stderr(stderr);
    Ok((Arc::new(session), defs))
}

/// stdio half of `make_transport`: `tokio::process::Command::new(cmd).args(args).envs(env)`,
/// `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()`; the returned stderr is drained by a spawned task
/// into the capture (its last `stderr_cap` bytes), whose handle rides along in [`StderrCapture`] so a failed
/// handshake — or, through the session, an exit — can wait for it. Spawn failure → `Connect`, worded by [`explain::spawn`].
pub(crate) fn spawn_stdio(
    server_cfg: &ServerConfig,
    stderr_cap: usize,
) -> Result<(rmcp::transport::TokioChildProcess, StderrCapture), McpError> {
    use tokio::io::AsyncReadExt;

    let mut command = tokio::process::Command::new(&server_cfg.command);
    command.args(&server_cfg.args).envs(&server_cfg.env);
    let (transport, stderr) = rmcp::transport::TokioChildProcess::builder(command)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| McpError::Connect(explain::spawn(server_cfg, &e).into_text()))?;
    let capture: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let drain = stderr.map(|mut stderr| {
        let sink = Arc::clone(&capture);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let n = match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                // Keep the TAIL: what a server wrote last is what explains its exit.
                let mut captured = lock(&sink);
                captured.extend_from_slice(&buf[..n]);
                let excess = captured.len().saturating_sub(stderr_cap);
                captured.drain(..excess);
            }
        })
    });
    Ok((
        transport,
        StderrCapture {
            buf: capture,
            drain: std::sync::Mutex::new(drain),
        },
    ))
}

/// HTTP half of `make_transport`: `StreamableHttpClientTransport::with_client(http,
/// StreamableHttpClientTransportConfig::with_uri(url).custom_headers(parsed))`; a `HeaderName` / `HeaderValue` parse
/// failure → `Connect(e)`. (Reserved names — `accept`, `Mcp-Session-Id`, `Last-Event-Id` — are rejected by rmcp at
/// handshake time and surface as `Connect(..)` too, DIVERGENCES D-01.)
pub(crate) fn http_transport(
    server_cfg: &ServerConfig,
    http: reqwest::Client,
) -> Result<rmcp::transport::StreamableHttpClientTransport<reqwest::Client>, McpError> {
    use rmcp::transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    };

    let config = StreamableHttpClientTransportConfig::with_uri(server_cfg.url.as_str())
        .custom_headers(parse_headers(server_cfg)?);
    Ok(StreamableHttpClientTransport::with_client(http, config))
}

/// The config's `headers:` as typed header values; a parse failure → `Connect`, naming the header.
fn parse_headers(
    server_cfg: &ServerConfig,
) -> Result<
    std::collections::HashMap<reqwest::header::HeaderName, reqwest::header::HeaderValue>,
    McpError,
> {
    use reqwest::header::{HeaderName, HeaderValue};

    let mut headers = std::collections::HashMap::with_capacity(server_cfg.headers.len());
    for (name, value) in &server_cfg.headers {
        let invalid = |e: &dyn std::fmt::Display| {
            McpError::Connect(format!(
                "its config has an invalid header {name:?}: {e} — fix it in the config"
            ))
        };
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| invalid(&e))?;
        let value = HeaderValue::from_str(value).map_err(|e| invalid(&e))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use crate::provider::model::JsonObject;
    use rmcp::{
        RoleServer, ServerHandler,
        model::{
            CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities,
            ClientInfo, ContentBlock, CreateTaskResult, Implementation, InputRequiredResult,
            ProtocolVersion, Task, TaskStatus,
        },
        service::RequestContext,
    };
    use tokio_util::sync::CancellationToken;

    use super::{RmcpSession, Session};
    use crate::mcp::error::{MRTR_UNSUPPORTED, McpError, TASK_UNSUPPORTED};
    use crate::mcp::testutil::serve_pair;

    /// A server whose `tools/call` answers by raw name: `ask` → SEP-2322 `input_required`, `task` → SEP-2663
    /// task, `error` → `isError: true`, anything else → `Complete("<name>")`.
    struct MrtrServer;

    impl ServerHandler for MrtrServer {
        fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResponse, rmcp::ErrorData>> {
            std::future::ready(Ok(match request.name.as_ref() {
                "ask" => CallToolResponse::InputRequired(InputRequiredResult::from_request_state(
                    "opaque",
                )),
                "task" => CallToolResponse::Task(CreateTaskResult::new(Task::new(
                    "t-1",
                    TaskStatus::Working,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z",
                ))),
                "error" => {
                    CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(
                        "boom",
                    )]))
                }
                other => CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text(other.to_owned()),
                    ContentBlock::image("AA==", "image/png"),
                    ContentBlock::text("second"),
                ])),
            }))
        }
    }

    // DIVERGENCES D-33: `call_tool_once` never loops MRTR rounds; the two non-`Complete` kinds are error texts.
    #[tokio::test]
    async fn call_tool_once_maps_input_required_and_task_to_errors() {
        // `input_required` needs a 2026-07-28 peer and a task result needs the tasks capability, so this client
        // negotiates both (the production client keeps rmcp's defaults — D-02).
        let client = ClientInfo::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::new("test-client", "1.0.0"),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28);
        let session = RmcpSession::new(serve_pair(MrtrServer, client).await);
        let cancel = CancellationToken::new();

        let err = session
            .call_tool(&cancel, "ask", JsonObject::new())
            .await
            .expect_err("input_required is an error");
        assert_eq!(err, McpError::Call(MRTR_UNSUPPORTED.to_owned()));
        assert_eq!(
            err.to_string(),
            "server requested client input (SEP-2322 input_required), which iota does not support"
        );

        let err = session
            .call_tool(&cancel, "task", JsonObject::new())
            .await
            .expect_err("task is an error");
        assert_eq!(err, McpError::Call(TASK_UNSUPPORTED.to_owned()));
        assert_eq!(
            err.to_string(),
            "server returned a task (SEP-2663), which iota does not support"
        );

        // Complete results flatten text blocks only (image dropped) and pass isError through.
        let (text, is_error) = session
            .call_tool(&cancel, "echo", JsonObject::new())
            .await
            .expect("complete");
        assert_eq!(text, "echo\nsecond");
        assert!(!is_error);
        let (text, is_error) = session
            .call_tool(&cancel, "error", JsonObject::new())
            .await
            .expect("isError result is not a hard error");
        assert_eq!(text, "boom");
        assert!(is_error);

        // A cancelled token abandons the call with the interrupted text.
        cancel.cancel();
        let err = session
            .call_tool(&cancel, "echo", JsonObject::new())
            .await
            .expect_err("cancelled");
        assert_eq!(err, McpError::Call("interrupted".to_owned()));

        session.close().await;
        session.close().await;
    }
}
