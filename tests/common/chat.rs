//! Shared fakes of the `iota-chat` test crates (WP13-owned): the dispatchers and providers the Go chat tests
//! hand-roll (`chat/toolloop_test.go`, `chat/approval_test.go`, `chat/parallel_test.go`), on top of the workspace fakes
//! in `iota::testing` (`StaticDispatcher`, `FakeToolProvider`).
//!
//! - [`GatedDispatch`] — one `write_file` tool that always needs approval and counts its executions
//!   (`gatedDispatch`; with `header` set it also reports the call's `path` like `detailDispatch`);
//! - [`GrowingDispatcher`] — gains `late_tool` once `search_tools` ran (`growingDispatcher`);
//! - [`ParallelDispatch`] — parallel-capable by name or by the `agent` argument; calls meet at a `Barrier` and
//!   record the peak overlap (`parallelDispatch`);
//! - [`NoCapDispatch`] — no optional capability at all (`noCapDispatch`);
//! - [`WritingProvider`] — asks for `write_file` once, then echoes the last history entry (`writingProvider`);
//! - [`EffortProvider`] — a `Tunable` provider that records the effort it was given.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use iota::BoxFuture;
use iota::chat::turns::RunCtx;
use iota::provider::error::ProviderError;
use iota::provider::model::{JsonObject, Message, ToolCall, ToolDef};
use iota::provider::sink::StreamSink;
use iota::provider::{
    ChatResult, Effort, Provider, ProviderKind, RoundResult, ToolProvider, Tunable,
};
use iota::tool::error::ToolError;
use iota::tool::{Dispatcher, ToolOutput, ToolResult};
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

/// Locks a fixture mutex, tolerating poisoning.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `ToolDef` with just a name.
pub fn def(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        ..ToolDef::default()
    }
}

/// A `ToolCall` with empty arguments (Go `call(id, name)`).
pub fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: JsonObject::new(),
    }
}

/// A `ToolCall` with string arguments.
pub fn call_with(id: &str, name: &str, args: &[(&str, &str)]) -> ToolCall {
    let mut arguments = JsonObject::new();
    for (k, v) in args {
        arguments.insert((*k).to_owned(), serde_json::Value::from(*v));
    }
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

/// Owns one tool (`write_file`) that always needs approval and records whether it was ever executed.
#[derive(Default)]
pub struct GatedDispatch {
    /// Executions so far.
    pub ran: AtomicUsize,
    /// When set, `header_summary` reports the call's `path` argument (Go `detailDispatch`).
    pub header: bool,
}

impl GatedDispatch {
    /// The plain gate (no header capability).
    pub fn new() -> Self {
        Self::default()
    }

    /// A gate whose tool names the file it would write.
    pub fn with_header() -> Self {
        Self {
            ran: AtomicUsize::new(0),
            header: true,
        }
    }

    /// How many times the gated tool actually ran.
    pub fn ran(&self) -> usize {
        self.ran.load(Ordering::SeqCst)
    }
}

impl Dispatcher for GatedDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        vec![def("write_file")]
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::ok("written"))
        })
    }

    fn requires_approval(&self, _name: &str) -> bool {
        true
    }

    fn header_summary(&self, _name: &str, args: &JsonObject) -> Option<String> {
        self.header.then(|| {
            args.get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
    }
}

/// Gains a tool after its `search_tools` is called — the deferred-loading shape.
#[derive(Default)]
pub struct GrowingDispatcher {
    loaded: AtomicBool,
}

impl Dispatcher for GrowingDispatcher {
    fn tools(&self) -> Vec<ToolDef> {
        let mut defs = vec![def("search_tools")];
        if self.loaded.load(Ordering::SeqCst) {
            defs.push(def("late_tool"));
        }
        defs
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if name == "search_tools" {
                self.loaded.store(true, Ordering::SeqCst);
                return Ok(ToolOutput::ok("Loaded 1 tool(s)"));
            }
            Ok(ToolOutput::ok("ok"))
        })
    }
}

/// A dispatcher whose named tools are parallel-capable and whose calls meet at a barrier, so a test can prove
/// they overlap. With `by_agent` set the answer comes from the call's `agent` argument instead of its name — the
/// per-call shape, where one name covers calls that differ. Calls observe cancellation (`ToolError::Cancelled`).
#[derive(Default)]
pub struct ParallelDispatch {
    parallel: HashSet<String>,
    by_agent: Option<HashMap<String, bool>>,
    barrier: Option<Arc<Barrier>>,
    peak: AtomicUsize,
    live: AtomicUsize,
}

impl ParallelDispatch {
    /// Parallel-capable by tool name.
    pub fn by_name(names: &[&str]) -> Self {
        Self {
            parallel: names.iter().map(|n| (*n).to_owned()).collect(),
            ..Self::default()
        }
    }

    /// Parallel-capable by the `agent` argument.
    pub fn by_agent(agents: &[(&str, bool)]) -> Self {
        Self {
            by_agent: Some(
                agents
                    .iter()
                    .map(|(a, ok)| ((*a).to_owned(), *ok))
                    .collect(),
            ),
            ..Self::default()
        }
    }

    /// Every call waits at a barrier of `n` parties before finishing: a serial loop deadlocks, a batch passes.
    #[must_use]
    pub fn with_barrier(mut self, n: usize) -> Self {
        self.barrier = Some(Arc::new(Barrier::new(n)));
        self
    }

    /// The highest number of calls that were in flight at once.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

impl Dispatcher for ParallelDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if cx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let n = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            if let Some(b) = &self.barrier {
                b.wait().await;
            }
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::ok(format!("out:{name}")))
        })
    }

    fn supports_parallel(&self, name: &str, args: Option<&JsonObject>) -> bool {
        if let Some(by_agent) = &self.by_agent {
            let agent = args
                .and_then(|a| a.get("agent"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            return by_agent.get(agent).copied().unwrap_or(false);
        }
        self.parallel.contains(name)
    }
}

/// A dispatcher without any optional capability: everything serializes, nothing needs approval.
#[derive(Default)]
pub struct NoCapDispatch;

impl Dispatcher for NoCapDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async { Ok(ToolOutput::ok("")) })
    }
}

/// Asks for the gated tool once, then answers with `saw: ` + the last history entry's content — so a test can
/// assert on what the model was actually told (Go `writingProvider` / `pathWritingProvider`).
#[derive(Default)]
pub struct WritingProvider {
    calls: AtomicUsize,
    /// Arguments of the `write_file` call (empty by default; `pathWritingProvider` sets `path`).
    pub args: JsonObject,
}

impl WritingProvider {
    /// The `pathWritingProvider` shape: the call names the file it would write.
    pub fn with_path(path: &str) -> Self {
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), serde_json::Value::from(path));
        Self {
            calls: AtomicUsize::new(0),
            args,
        }
    }

    /// Number of model calls so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for WritingProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    fn model(&self) -> &'static str {
        "gpt-test"
    }

    fn set_model(&mut self, _model: String) {}

    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async { Ok(ChatResult::default()) })
    }

    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        Some(self)
    }
}

impl ToolProvider for WritingProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        messages: &'a [Message],
        _tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(async move {
            sink.reasoning_done();
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                return Ok(RoundResult {
                    tool_calls: vec![ToolCall {
                        id: "c1".to_owned(),
                        name: "write_file".to_owned(),
                        arguments: self.args.clone(),
                    }],
                    ..RoundResult::default()
                });
            }
            let last = messages
                .last()
                .map(|m| m.content.as_str())
                .unwrap_or_default();
            Ok(RoundResult {
                content: format!("saw: {last}"),
                ..RoundResult::default()
            })
        })
    }
}

/// A `Tunable` provider that records the effort it was given and answers `reply` on the unary path. The shared
/// `effort` cell lets a test observe what a caller set on the instance it built.
pub struct EffortProvider {
    /// The effort the caller set (shared with the test).
    pub effort: Arc<Mutex<Option<Effort>>>,
    temperature: Option<f64>,
    /// The unary reply.
    pub reply: String,
}

impl EffortProvider {
    /// A provider answering `reply`, recording into `effort`.
    pub fn new(reply: &str, effort: Arc<Mutex<Option<Effort>>>) -> Self {
        Self {
            effort,
            temperature: None,
            reply: reply.to_owned(),
        }
    }
}

impl Tunable for EffortProvider {
    fn set_temperature(&mut self, t: Option<f64>) {
        self.temperature = t;
    }

    fn temperature(&self) -> Option<f64> {
        self.temperature
    }

    fn set_effort(&mut self, e: Option<Effort>) {
        *lock(&self.effort) = e;
    }

    fn effort(&self) -> Option<Effort> {
        *lock(&self.effort)
    }
}

impl Provider for EffortProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn model(&self) -> &'static str {
        "claude-test"
    }

    fn set_model(&mut self, _model: String) {}

    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            Ok(ChatResult {
                text: self.reply.clone(),
                usage: Some(iota::provider::usage::Usage {
                    input: 7,
                    output: 3,
                    ..iota::provider::usage::Usage::default()
                }),
                images: Vec::new(),
            })
        })
    }

    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        Some(self)
    }
}
