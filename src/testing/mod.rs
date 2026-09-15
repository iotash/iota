//! Shared test fakes (feature `testing`): the one fake provider ([`FakeProvider`]), a recording sink, a
//! static dispatcher, the scripted UI facade, and map-backed `VarResolver`/`EnvSource` fixtures that replace
//! `t.Setenv`.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::BoxFuture;
use crate::chat::turns::RunCtx;
use crate::provider::model::{JsonObject, ToolCall, ToolDef};
use crate::provider::sink::StreamSink;
use crate::tool::{Dispatcher, ToolOutput, ToolResult};
use crate::vars::{EnvSource, VarResolver};

mod provider;
mod scripted;
pub use provider::{Call, Failure, FakeProvider, Interrupt, Log, Path, Round};
pub use scripted::{PanelSummary, RecordingHost, Reply, ScriptedUi, TabbedSummary, UiEvent};

/// Locks a fixture mutex, tolerating poisoning (a panicking test must not hide the state from the next assertion).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `ToolDef` with just a name.
pub fn tool_def(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        ..ToolDef::default()
    }
}

/// A `ToolCall` with empty arguments.
pub fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: JsonObject::new(),
    }
}

/// A `ToolCall` with string arguments.
pub fn tool_call_with(id: &str, name: &str, args: &[(&str, &str)]) -> ToolCall {
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

/// One sink event, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkEvent {
    /// A content delta.
    Content(String),
    /// A reasoning delta.
    Reasoning(String),
    /// `reasoning_done` fired.
    ReasoningDone,
    /// A progressive image frame (`image_partial`) with its decoded bytes.
    ImagePartial(Vec<u8>),
}

/// Ordered event log; tests assert `ReasoningDone` precedes the first `Content`.
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// Every event, in order.
    pub events: Vec<SinkEvent>,
}

impl StreamSink for RecordingSink {
    fn content(&mut self, delta: &str) {
        self.events.push(SinkEvent::Content(delta.to_owned()));
    }

    fn reasoning(&mut self, delta: &str) {
        self.events.push(SinkEvent::Reasoning(delta.to_owned()));
    }

    fn reasoning_done(&mut self) {
        self.events.push(SinkEvent::ReasoningDone);
    }

    fn image_partial(&mut self, frame: &[u8]) {
        self.events.push(SinkEvent::ImagePartial(frame.to_vec()));
    }
}

impl RecordingSink {
    /// All content deltas concatenated.
    pub fn content(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::Content(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All reasoning deltas concatenated.
    pub fn reasoning(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::Reasoning(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether `ReasoningDone` was recorded before the first `Content` (Go's `closedBeforeContent`: true until a
    /// content write arrives while reasoning is still open; a stream with no content at all is trivially ordered).
    pub fn closed_before_content(&self) -> bool {
        let mut closed = false;
        for e in &self.events {
            match e {
                SinkEvent::ReasoningDone => closed = true,
                SinkEvent::Content(_) if !closed => return false,
                _ => {}
            }
        }
        true
    }
}

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
            defs: names
                .iter()
                .map(|n| ToolDef {
                    name: (*n).to_owned(),
                    ..ToolDef::default()
                })
                .collect(),
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

/// `HashMap`-backed `VarResolver` (env vars + fixed cwd/home) — replaces `t.Setenv`.
#[derive(Clone, Debug, Default)]
pub struct MapResolver {
    /// Environment variables.
    pub vars: HashMap<String, String>,
    /// The working directory.
    pub cwd: Option<PathBuf>,
    /// The home directory.
    pub home: Option<PathBuf>,
}

impl VarResolver for MapResolver {
    fn env_var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }

    fn cwd(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
}

/// A `MapResolver` over `vars` with no cwd/home.
pub fn map_resolver(vars: &[(&str, &str)]) -> MapResolver {
    MapResolver {
        vars: vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..MapResolver::default()
    }
}

/// `HashMap`-backed `EnvSource`.
#[derive(Clone, Debug, Default)]
pub struct MapEnv(pub HashMap<String, String>);

impl EnvSource for MapEnv {
    /// Like `ProcessEnv`, an empty value counts as unset.
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).filter(|v| !v.is_empty()).cloned()
    }
}

/// A `MapEnv` over `vars`.
pub fn map_env(vars: &[(&str, &str)]) -> MapEnv {
    MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::{RecordingSink, SinkEvent, StaticDispatcher, map_env, map_resolver};
    use crate::chat::turns::RunCtx;
    use crate::provider::model::JsonObject;
    use crate::provider::sink::StreamSink;
    use crate::tool::Dispatcher;
    use crate::vars::{EnvSource, VarResolver, expand};

    #[test]
    fn recording_sink_orders_events() {
        let mut s = RecordingSink::default();
        StreamSink::reasoning(&mut s, "th");
        StreamSink::reasoning(&mut s, "ink");
        s.reasoning_done();
        StreamSink::content(&mut s, "hi");
        assert_eq!(s.reasoning(), "think");
        assert_eq!(s.content(), "hi");
        assert!(s.closed_before_content());
        assert_eq!(s.events[2], SinkEvent::ReasoningDone);

        let mut late = RecordingSink::default();
        StreamSink::content(&mut late, "x");
        late.reasoning_done();
        assert!(!late.closed_before_content());
        let mut never = RecordingSink::default();
        StreamSink::content(&mut never, "x");
        assert!(!never.closed_before_content());
        assert!(RecordingSink::default().closed_before_content());
    }

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

    #[test]
    fn map_fixtures() {
        let r = map_resolver(&[("A", "1")]);
        assert_eq!(r.env_var("A"), Some("1".to_owned()));
        assert_eq!(r.env_var("B"), None);
        assert!(r.cwd().is_none() && r.home().is_none());
        assert_eq!(expand("${env:A}${cwd}", &r), "1${cwd}");
        let e = map_env(&[("K", "v"), ("EMPTY", "")]);
        assert_eq!(e.var("K"), Some("v".to_owned()));
        assert_eq!(e.var("EMPTY"), None);
        assert_eq!(e.var("MISSING"), None);
    }
}
