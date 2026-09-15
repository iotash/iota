//! The fake `Dispatcher`s and the fake `Tool`: every shape the loop tests need a tool side for, in one
//! place. [`StaticDispatcher`] is the general one (named tools, echoing calls, per-name flags); the others
//! each stand for ONE capability question — approval ([`GatedDispatch`]), a set that grows mid-turn
//! ([`GrowingDispatcher`]), parallel batches ([`ParallelDispatch`]), no optional capability at all
//! ([`NoCapDispatch`]), a header without tools ([`HeaderDispatch`]).

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use tokio::sync::Barrier;

use super::{lock, tool_def};
use crate::BoxFuture;
use crate::chat::turns::RunCtx;
use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::error::ToolError;
use crate::tool::{Dispatcher, ToolOutput, ToolResult};

/// Static tools; `call_tool` echoes `"<name>:<args json>"`, records calls, optional per-name parallel/approval flags.
pub struct StaticDispatcher {
    /// The advertised definitions.
    pub defs: Vec<ToolDef>,
    /// Every call made, in order.
    pub calls: Mutex<Vec<(String, JsonObject)>>,
    /// Names that report `supports_parallel`.
    pub parallel: HashSet<String>,
    /// Names that report `requires_approval`.
    pub approval: HashSet<String>,
}

impl StaticDispatcher {
    /// Tools named `names`, none parallel, none needing approval.
    pub fn new(names: &[&str]) -> Self {
        Self {
            defs: names.iter().map(|n| tool_def(n)).collect(),
            calls: Mutex::new(Vec::new()),
            parallel: HashSet::new(),
            approval: HashSet::new(),
        }
    }

    /// Marks `names` as parallel-capable.
    #[must_use]
    pub fn with_parallel(mut self, names: &[&str]) -> Self {
        self.parallel.extend(names.iter().map(|n| (*n).to_owned()));
        self
    }

    /// Marks `names` as needing approval.
    #[must_use]
    pub fn with_approval(mut self, names: &[&str]) -> Self {
        self.approval.extend(names.iter().map(|n| (*n).to_owned()));
        self
    }
}

impl Dispatcher for StaticDispatcher {
    fn tools(&self) -> Vec<ToolDef> {
        self.defs.clone()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let json = serde_json::to_string(&args).unwrap_or_default();
            lock(&self.calls).push((name.to_owned(), args));
            Ok(ToolOutput::ok(format!("{name}:{json}")))
        })
    }

    fn requires_approval(&self, name: &str) -> bool {
        self.approval.contains(name)
    }

    fn supports_parallel(&self, name: &str, _args: Option<&JsonObject>) -> bool {
        self.parallel.contains(name)
    }
}

/// Owns one tool (`write_file`) that always needs approval and records whether it was ever executed.
#[derive(Default)]
pub struct GatedDispatch {
    /// Executions so far.
    pub ran: AtomicUsize,
    /// When set, `header_summary` reports the call's `path` argument.
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
        vec![tool_def("write_file")]
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
        let mut defs = vec![tool_def("search_tools")];
        if self.loaded.load(Ordering::SeqCst) {
            defs.push(tool_def("late_tool"));
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

/// A dispatcher without any optional capability: no tools, everything serializes, nothing needs approval,
/// no header (the digest applies).
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

/// A dispatcher carrying the header capability and nothing else: `header_summary` answers `summary` for
/// every call.
pub struct HeaderDispatch {
    /// The one summary (`Some("")` = the capability's own empty answer).
    pub summary: Option<String>,
}

impl Dispatcher for HeaderDispatch {
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

    fn header_summary(&self, _name: &str, _args: &JsonObject) -> Option<String> {
        self.summary.clone()
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::StaticDispatcher;
    use crate::chat::turns::RunCtx;
    use crate::provider::model::JsonObject;
    use crate::tool::Dispatcher;

    #[tokio::test]
    async fn static_dispatcher_echoes_and_records() {
        let d = StaticDispatcher::new(&["a", "b"])
            .with_parallel(&["a"])
            .with_approval(&["b"]);
        assert_eq!(
            d.tools()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(d.supports_parallel("a", None));
        assert!(!d.supports_parallel("b", None));
        assert!(d.requires_approval("b"));
        assert!(!d.requires_approval("a"));
        let mut args = JsonObject::new();
        args.insert("k".to_owned(), serde_json::Value::from(1));
        let cx = RunCtx::new(CancellationToken::new());
        let out = d.call_tool(&cx, "a", args.clone()).await.expect("ok");
        assert_eq!(out.text, "a:{\"k\":1}");
        assert!(!out.is_error);
        assert_eq!(super::lock(&d.calls).as_slice(), &[("a".to_owned(), args)]);
    }
}
