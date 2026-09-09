//! MCP manager on rmcp 3.1.4 (mcp/): stdio + streamable-HTTP transports, a 30 s connect fan-out with a
//! deterministic config-order merge, `mcp__<segment>__<tool>` naming, the live tool view, call routing and close.
//! `Manager` implements `crate::tool::Dispatcher` and produces `crate::tool::PrefixOf`; `config` holds
//! `ServerConfig`, `parse_mcp_flag`, `expand_server_config`, `endpoint_of` and `McpFlagError` (mcp/manager.go,
//! mcp/vars.go), which `crate::cmd::assemble` parses without naming `Manager`.

pub mod config;
pub(crate) mod error;
pub(crate) mod manager;
pub mod naming;
pub(crate) mod status;
pub(crate) mod transport;

pub use manager::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_STDERR_CAP, Manager, ManagerOptions};
pub(crate) use status::ServerStatus;

/// In-process MCP servers for the unit tests (rmcp `server` dev-feature). Go: `mcp/manager_test.go` `startEchoServer`.
#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::{Arc, Mutex, PoisonError};

    use crate::BoxFuture;
    use crate::provider::model::JsonObject;
    use crate::testing::map_resolver;
    use rmcp::{
        RoleClient, RoleServer, ServerHandler, ServiceExt,
        model::{
            CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities,
            ClientInfo, ContentBlock, Implementation, InitializeResult, ListToolsResult,
            PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
        },
        service::{RequestContext, RunningService},
    };
    use tokio_util::sync::CancellationToken;

    use crate::mcp::error::McpError;
    use crate::mcp::manager::ManagerOptions;
    use crate::mcp::transport::{RmcpSession, Session};

    /// `ManagerOptions` for tests: a fresh `reqwest::Client` and an empty map resolver (no process environment).
    pub(crate) fn options() -> ManagerOptions {
        ManagerOptions::new(reqwest::Client::new(), Arc::new(map_resolver(&[])))
    }

    /// Serves `handler` on one end of a `tokio::io::duplex` pair (its service loop detached) and runs the client
    /// handshake with `client` on the other, returning the connected client side.
    pub(crate) async fn serve_pair<H: ServerHandler>(
        handler: H,
        client: ClientInfo,
    ) -> RunningService<RoleClient, ClientInfo> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            if let Ok(server) = handler.serve(server_io).await {
                let _ = server.waiting().await;
            }
        });
        client.serve(client_io).await.expect("client handshake")
    }

    /// The Go `startEchoServer` handler: one tool `echo` (`"echo back the server id"`, schema `{"type":"object"}`)
    /// answering `"<id>:<raw name the server saw>"`.
    struct EchoServer {
        id: String,
    }

    impl ServerHandler for EchoServer {
        fn get_info(&self) -> ServerInfo {
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new(self.id.clone(), "1.0.0"))
        }

        fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> {
            let mut schema = JsonObject::new();
            schema.insert("type".to_owned(), serde_json::Value::from("object"));
            std::future::ready(Ok(ListToolsResult::with_all_items(vec![Tool::new(
                "echo",
                "echo back the server id",
                schema,
            )])))
        }

        fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResponse, rmcp::ErrorData>> {
            std::future::ready(Ok(CallToolResponse::Complete(CallToolResult::success(
                vec![ContentBlock::text(format!("{}:{}", self.id, request.name))],
            ))))
        }
    }

    /// Starts an rmcp server named `id` on a `tokio::io::duplex` pair whose single tool `echo` (`"echo back the
    /// server id"`, schema `{"type":"object"}`) answers `"<id>:<raw name>"`, runs the real `initialize` handshake and
    /// returns the connected client side as a production `RmcpSession`.
    pub(crate) async fn echo_server(id: &str) -> Arc<dyn Session> {
        let client = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("test-client", "1.0.0"),
        );
        let running = serve_pair(EchoServer { id: id.to_owned() }, client).await;
        Arc::new(RmcpSession::new(running))
    }

    /// An in-memory `Session` with no transport at all: every call answers `"<id>:<raw name>"` and `close` is a
    /// no-op. For `merge_result` routing tests that do not need the handshake.
    pub(crate) struct EchoSession {
        /// The id echoed back in front of every raw tool name.
        pub(crate) id: String,
    }

    impl Session for EchoSession {
        fn call_tool<'a>(
            &'a self,
            _cancel: &'a CancellationToken,
            raw: &'a str,
            _args: JsonObject,
        ) -> BoxFuture<'a, Result<(String, bool), McpError>> {
            Box::pin(std::future::ready(Ok((
                format!("{}:{raw}", self.id),
                false,
            ))))
        }

        fn close(&self) -> BoxFuture<'_, ()> {
            Box::pin(std::future::ready(()))
        }
    }

    /// Runs `f` with a thread-local `tracing` subscriber that records every event's formatted `message` (the Go
    /// `logf` capture of `TestManagerSkipsDuplicateWireName`).
    pub(crate) fn capture_warnings(f: impl FnOnce()) -> Vec<String> {
        struct Capture(Mutex<Vec<String>>);

        struct MessageVisitor(String);

        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }

        impl tracing::Subscriber for Capture {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }

            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }

            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }

            fn event(&self, event: &tracing::Event<'_>) {
                let mut visitor = MessageVisitor(String::new());
                event.record(&mut visitor);
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(visitor.0);
            }

            fn enter(&self, _span: &tracing::span::Id) {}

            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        tracing::subscriber::with_default(Arc::clone(&capture), f);
        let messages = capture.0.lock().unwrap_or_else(PoisonError::into_inner);
        messages.clone()
    }
}
