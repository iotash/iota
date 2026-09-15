//! One live MCP connection (mcp/manager.go:337-398,478-512): `connect_one` builds the rmcp transport (stdio child
//! process or streamable HTTP), runs the `initialize` handshake and lists the tools; `Session` is the crate-private
//! seam the manager routes calls through, and `RmcpSession` is its production implementation over
//! `Peer::call_tool_once` (never `call_tool`, which drives SEP-2322 MRTR rounds — DIVERGENCES D-33).

use std::{borrow::Cow, sync::Arc, time::Duration};

use crate::BoxFuture;
use crate::mcp::config::{ServerConfig, expand_server_config};
use crate::provider::model::{JsonObject, ToolDef};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ClientInfo, ContentBlock},
    service::{Peer, RunningService, ServiceError},
};
use tokio_util::sync::CancellationToken;

use crate::mcp::error::{MRTR_UNSUPPORTED, McpError, TASK_UNSUPPORTED};
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
    fn call_tool<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        raw: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, Result<(String, bool), McpError>>;

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
}

impl RmcpSession {
    /// Wraps a freshly served connection, cloning its peer.
    pub(crate) fn new(running: RunningService<RoleClient, ClientInfo>) -> Self {
        let peer = running.peer().clone();
        Self {
            running: tokio::sync::Mutex::new(Some(running)),
            peer,
        }
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
    /// `peer.call_tool_once(CallToolRequestParams::new(raw).with_arguments(args))` selected against the token:
    /// `Complete(r)` → `(text blocks joined by "\n", r.is_error.unwrap_or(false))`; `InputRequired(_)` →
    /// `Err(McpError::Call(MRTR_UNSUPPORTED.into()))`; `Task(_)` → `Err(McpError::Call(TASK_UNSUPPORTED.into()))`;
    /// `Err(ServiceError)` → `McpError::Call(e.to_string())`.
    fn call_tool<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        raw: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, Result<(String, bool), McpError>> {
        Box::pin(async move {
            let params = CallToolRequestParams::new(raw.to_owned()).with_arguments(args);
            let response = tokio::select! {
                r = self.peer.call_tool_once(params) => r,
                () = cancel.cancelled() => return Err(McpError::Call(CALL_INTERRUPTED.to_owned())),
            };
            match response {
                Ok(CallToolResponse::Complete(r)) => {
                    Ok((join_text(&r), r.is_error.unwrap_or(false)))
                }
                Ok(CallToolResponse::InputRequired(_)) => {
                    Err(McpError::Call(MRTR_UNSUPPORTED.to_owned()))
                }
                Ok(CallToolResponse::Task(_)) => Err(McpError::Call(TASK_UNSUPPORTED.to_owned())),
                // `CallToolResponse` is `#[non_exhaustive]`: a kind this build does not know.
                Ok(_) => Err(McpError::Call(ServiceError::UnexpectedResponse.to_string())),
                Err(e) => Err(McpError::Call(e.to_string())),
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

/// Captured stderr of a stdio server: the `stderr_cap`-capped buffer a spawned task drains into,
/// plus that task's handle, so a failed handshake can wait for the pipe to reach EOF before it
/// reads. Reading the buffer without that wait is a race the child usually loses — the bytes are
/// still in the pipe, or the task has not been scheduled yet — and the appendix `McpError::Connect`
/// promises then appears only sometimes, which is the same as not promising it.
pub(crate) struct StderrCapture {
    /// What the drain task has captured so far, capped at `stderr_cap`.
    buf: Arc<std::sync::Mutex<Vec<u8>>>,
    /// The drain task; it completes at EOF. `None` when the child had no stderr pipe.
    drain: Option<tokio::task::JoinHandle<()>>,
}

impl StderrCapture {
    /// The captured bytes, after waiting up to [`STDERR_DRAIN_GRACE`] for the drain to reach EOF.
    /// On timeout the handle is dropped, which only detaches the task — it goes on capping the
    /// buffer for as long as the pipe is open.
    async fn into_bytes(mut self) -> Vec<u8> {
        if let Some(drain) = self.drain.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, drain).await;
        }
        lock(&self.buf).clone()
    }
}

/// manager.go:337-398 — expand (`opts.resolver`) → endpoint → transport → `ClientInfo::new(
/// ClientCapabilities::default(), opts.client_info.clone()).serve(transport)` → `peer().list_all_tools()` →
/// `ToolDef { name: raw, description: unwrap_or_default, input_schema: Some((*input_schema).clone()), deferred: false }`.
///
/// Transport selection: url non-empty → must start `http://` / `https://` else `UnsupportedScheme(url)`, then
/// `http_transport`; command non-empty → `spawn_stdio`; both empty → `MissingTarget`. A handshake failure is `Connect(e)` (stdio: plus
/// `\n  subprocess stderr:\n<trimmed>` when the capture is non-empty, after a bounded wait for the pipe to reach
/// EOF — [`STDERR_DRAIN_GRACE`]); a `tools/list` failure closes the session and
/// is `ListTools(e)`. The caller wraps this in `timeout(connect_timeout, ..)` (`Elapsed` → `Timeout`).
pub(crate) async fn connect_one(
    server_cfg: &ServerConfig,
    opts: &ManagerOptions,
) -> Result<(Arc<dyn Session>, Vec<ToolDef>), McpError> {
    let server_cfg = expand_server_config(server_cfg, &opts.env);

    if !server_cfg.url.is_empty() {
        if !(server_cfg.url.starts_with("http://") || server_cfg.url.starts_with("https://")) {
            return Err(McpError::UnsupportedScheme(server_cfg.url));
        }
        let transport = http_transport(&server_cfg, opts.http.clone())?;
        let running = client_info(opts)
            .serve(transport)
            .await
            .map_err(|e| McpError::Connect(e.to_string()))?;
        return list_tools(running).await;
    }

    if !server_cfg.command.is_empty() {
        let (transport, stderr) = spawn_stdio(&server_cfg, opts.stderr_cap)?;
        let running = match client_info(opts).serve(transport).await {
            Ok(running) => running,
            Err(e) => {
                let mut msg = e.to_string();
                // Bounded wait FIRST: the handshake can fail before the child's stderr has even
                // been read, and an appendix that depends on which of the two wins is a diagnostic
                // the user cannot rely on.
                let captured = stderr.into_bytes().await;
                if !captured.is_empty() {
                    msg.push_str("\n  subprocess stderr:\n");
                    msg.push_str(String::from_utf8_lossy(&captured).trim_end_matches('\n'));
                }
                return Err(McpError::Connect(msg));
            }
        };
        return list_tools(running).await;
    }

    Err(McpError::MissingTarget)
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
/// `ListTools(e)`.
async fn list_tools(
    running: RunningService<RoleClient, ClientInfo>,
) -> Result<(Arc<dyn Session>, Vec<ToolDef>), McpError> {
    let tools = match running.peer().list_all_tools().await {
        Ok(tools) => tools,
        Err(e) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, running.cancel()).await;
            return Err(McpError::ListTools(e.to_string()));
        }
    };
    let defs = tools
        .into_iter()
        .map(|t| ToolDef {
            name: t.name.into_owned(),
            description: t.description.map(Cow::into_owned).unwrap_or_default(),
            input_schema: Some((*t.input_schema).clone()),
            deferred: false,
        })
        .collect();
    Ok((Arc::new(RmcpSession::new(running)), defs))
}

/// stdio half of `make_transport`: `tokio::process::Command::new(cmd).args(args).envs(env)`,
/// `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()`; the returned stderr is drained by a spawned task
/// into the `stderr_cap`-capped capture, whose handle rides along in [`StderrCapture`] so a failed handshake can wait
/// for it. Spawn failure → `Connect(e)`.
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
        .map_err(|e| McpError::Connect(e.to_string()))?;
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
                let mut captured = lock(&sink);
                let room = stderr_cap.saturating_sub(captured.len());
                captured.extend_from_slice(&buf[..n.min(room)]);
            }
        })
    });
    Ok((
        transport,
        StderrCapture {
            buf: capture,
            drain,
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
    use std::collections::HashMap;

    use reqwest::header::{HeaderName, HeaderValue};
    use rmcp::transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    };

    let mut headers = HashMap::with_capacity(server_cfg.headers.len());
    for (name, value) in &server_cfg.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| McpError::Connect(e.to_string()))?;
        let value = HeaderValue::from_str(value).map_err(|e| McpError::Connect(e.to_string()))?;
        headers.insert(name, value);
    }
    let config = StreamableHttpClientTransportConfig::with_uri(server_cfg.url.as_str())
        .custom_headers(headers);
    Ok(StreamableHttpClientTransport::with_client(http, config))
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
