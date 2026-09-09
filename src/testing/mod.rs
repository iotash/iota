//! Shared test fakes (feature `testing`): a recording sink, a static dispatcher, a fake delegator, a scripted
//! tool provider, and map-backed `VarResolver`/`EnvSource` fixtures that replace `t.Setenv`.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use crate::chat::turns::RunCtx;
use crate::provider::error::ProviderError;
use crate::provider::model::{JsonObject, Message, ToolCall, ToolDef};
use crate::provider::sink::StreamSink;
use crate::provider::usage::Usage;
use crate::provider::{ChatResult, Provider, ProviderKind, RoundResult, ToolProvider};
use crate::tool::{
    AgentInfo, DelegateOutcome, DelegateResult, DelegateSpec, Delegator, Dispatcher, ToolOutput,
    ToolResult,
};
use crate::vars::{EnvSource, VarResolver};
use crate::{BoxError, BoxFuture};

mod scripted;
pub use scripted::{PanelSummary, RecordingHost, Reply, ScriptedUi, TabbedSummary, UiEvent};

/// Locks a fixture mutex, tolerating poisoning (a panicking test must not hide the state from the next assertion).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Fixed agents table + canned outcome; records the specs it ran.
pub struct FakeDelegator {
    /// The agents table.
    pub agents: BTreeMap<String, AgentInfo>,
    names: Vec<String>,
    /// Every spec `run` received, in order.
    pub ran: Mutex<Vec<DelegateSpec>>,
    /// The canned outcome every `run` returns.
    pub outcome: Mutex<Option<DelegateOutcomeSpec>>,
}

/// What `FakeDelegator::run` returns.
#[derive(Clone, Debug, Default)]
pub struct DelegateOutcomeSpec {
    /// The child's reply.
    pub reply: String,
    /// Rounds the child "ran".
    pub rounds: u32,
    /// Usage the child "spent".
    pub usage: Usage,
    /// Error text, if the child "failed".
    pub error: Option<String>,
}

impl FakeDelegator {
    /// A delegator over `agents` with no canned outcome yet.
    pub fn new(agents: BTreeMap<String, AgentInfo>) -> Self {
        let names = agents.keys().cloned().collect();
        Self {
            agents,
            names,
            ran: Mutex::new(Vec::new()),
            outcome: Mutex::new(None),
        }
    }

    /// Sets the canned outcome.
    #[must_use]
    pub fn with_outcome(self, o: DelegateOutcomeSpec) -> Self {
        Self {
            outcome: Mutex::new(Some(o)),
            ..self
        }
    }
}

impl Delegator for FakeDelegator {
    fn agent_names(&self) -> &[String] {
        &self.names
    }

    fn agent(&self, name: &str) -> Option<&AgentInfo> {
        self.agents.get(name)
    }

    /// Records `spec`, returns the canned outcome (an empty one when none is set) and — like the real delegator
    /// (CONTRACTS §6.7: `cx.ledger.add(rounds, usage)` ALWAYS) — books the child's cost into `cx.ledger`.
    fn run<'a>(&'a self, cx: &'a RunCtx, spec: DelegateSpec) -> BoxFuture<'a, DelegateOutcome> {
        Box::pin(async move {
            lock(&self.ran).push(spec);
            let o = lock(&self.outcome).clone().unwrap_or_default();
            if let Some(ledger) = &cx.ledger {
                ledger.add(o.rounds, o.usage);
            }
            DelegateOutcome {
                result: DelegateResult {
                    reply: o.reply,
                    rounds: o.rounds,
                    usage: o.usage,
                    duration: Duration::ZERO,
                },
                error: o.error.map(BoxError::from),
            }
        })
    }
}

/// Scripted `ToolProvider`/`Provider`: per-round `RoundResult`s; after the script ends returns the final text; kind
/// `OpenAi`, model `"gpt-test"`.
pub struct FakeToolProvider {
    /// Remaining scripted rounds.
    pub rounds: Mutex<VecDeque<RoundResult>>,
    /// The text returned once the script is exhausted.
    pub final_text: String,
    /// Number of `stream_chat_with_tools` calls so far.
    pub calls: AtomicUsize,
    /// The tool names advertised on each call.
    pub seen_tools: Mutex<Vec<Vec<String>>>,
    /// Fail call number `n` (1-based) with `ProviderError::other("boom")`.
    pub fail_on: Option<usize>,
    /// `looping(calls, 0)`: once the script is empty keep requesting this many `noop` calls per round forever (Go's
    /// `loopingToolProvider` with `stopAfter == 0`, the runaway case `--max-turns` guards against).
    endless: Option<u32>,
}

impl FakeToolProvider {
    /// Requests `calls` tool calls per round until `stop_after` rounds, then the final text `"done"`; `stop_after == 0`
    /// never stops (Go `loopingToolProvider`).
    pub fn looping(calls: u32, stop_after: u32) -> Self {
        let rounds = (1..=stop_after).map(|n| looping_round(n, calls)).collect();
        Self {
            rounds: Mutex::new(rounds),
            final_text: "done".to_owned(),
            calls: AtomicUsize::new(0),
            seen_tools: Mutex::new(Vec::new()),
            fail_on: None,
            endless: (stop_after == 0).then_some(calls),
        }
    }

    /// Reproduces `reportingProvider`: round n usage = `{input: 100n, output: 10n, cache_read: n, total: 110n}`,
    /// calls tool `noop` until `stop_after`, then final text `"final answer"`; `fail_on = Some(n)` fails call n
    /// with `ProviderError::other("boom")`.
    pub fn reporting(stop_after: u32, fail_on: Option<u32>) -> Self {
        let usage = |n: u32| {
            let n = u64::from(n);
            Some(Usage {
                input: 100 * n,
                output: 10 * n,
                cache_read: n,
                total: 110 * n,
                ..Usage::default()
            })
        };
        let mut rounds: VecDeque<RoundResult> = (1..=stop_after)
            .map(|n| RoundResult {
                tool_calls: vec![ToolCall {
                    id: format!("c{n}"),
                    name: "noop".to_owned(),
                    arguments: JsonObject::new(),
                }],
                usage: usage(n),
                ..RoundResult::default()
            })
            .collect();
        // The terminating round reports usage too (Go sets `last` before checking stopAfter).
        rounds.push_back(RoundResult {
            content: "final answer".to_owned(),
            usage: usage(stop_after + 1),
            ..RoundResult::default()
        });
        Self {
            rounds: Mutex::new(rounds),
            final_text: "final answer".to_owned(),
            calls: AtomicUsize::new(0),
            seen_tools: Mutex::new(Vec::new()),
            fail_on: fail_on.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
            endless: None,
        }
    }

    /// Round results verbatim (e.g. a terminating round whose `images` is non-empty —
    /// `tool_loop_final_round_images_are_saved`).
    pub fn scripted(rounds: Vec<RoundResult>, final_text: &str) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
            final_text: final_text.to_owned(),
            calls: AtomicUsize::new(0),
            seen_tools: Mutex::new(Vec::new()),
            fail_on: None,
            endless: None,
        }
    }

    /// Counts the call and pops the next scripted round (or synthesises the endless one).
    fn next_round(&self, call: usize) -> Option<RoundResult> {
        if let Some(r) = lock(&self.rounds).pop_front() {
            return Some(r);
        }
        let n = u32::try_from(call).unwrap_or(u32::MAX);
        self.endless.map(|calls| looping_round(n, calls))
    }
}

/// Round `n` of `loopingToolProvider`: `calls` requests for `noop` with ids `call-<n>` (or `call-<n>-<i>` when a
/// round carries several).
fn looping_round(n: u32, calls: u32) -> RoundResult {
    RoundResult {
        tool_calls: (1..=calls)
            .map(|i| ToolCall {
                id: if calls == 1 {
                    format!("call-{n}")
                } else {
                    format!("call-{n}-{i}")
                },
                name: "noop".to_owned(),
                arguments: JsonObject::new(),
            })
            .collect(),
        ..RoundResult::default()
    }
}

impl Provider for FakeToolProvider {
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

    /// The unary path consumes the same script: the next round's content/usage/images, or the final text.
    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_on == Some(call) {
                return Err(ProviderError::other("boom"));
            }
            Ok(match self.next_round(call) {
                Some(r) => ChatResult {
                    text: r.content,
                    usage: r.usage,
                    images: r.images,
                },
                None => ChatResult {
                    text: self.final_text.clone(),
                    ..ChatResult::default()
                },
            })
        })
    }

    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        Some(self)
    }
}

impl ToolProvider for FakeToolProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(async move {
            sink.reasoning_done();
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            lock(&self.seen_tools).push(tools.iter().map(|t| t.name.clone()).collect());
            if self.fail_on == Some(call) {
                return Err(ProviderError::other("boom"));
            }
            Ok(self.next_round(call).unwrap_or_else(|| RoundResult {
                content: self.final_text.clone(),
                ..RoundResult::default()
            }))
        })
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
    use std::{collections::BTreeMap, sync::Arc};

    use tokio_util::sync::CancellationToken;

    use super::{
        DelegateOutcomeSpec, FakeDelegator, FakeToolProvider, RecordingSink, SinkEvent,
        StaticDispatcher, map_env, map_resolver,
    };
    use crate::chat::turns::{DelegationLedger, RunCtx};
    use crate::provider::model::JsonObject;
    use crate::provider::sink::StreamSink;
    use crate::provider::usage::Usage;
    use crate::provider::{Effort, Provider, ToolProvider};
    use crate::tool::{AgentInfo, DelegateSpec, Delegator, Dispatcher};
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

    #[tokio::test]
    async fn fake_delegator_records_and_books_the_ledger() {
        let d = FakeDelegator::new(BTreeMap::from([(
            "review".to_owned(),
            AgentInfo {
                description: "reviews".to_owned(),
                read_only: true,
            },
        )]))
        .with_outcome(DelegateOutcomeSpec {
            reply: "ok".to_owned(),
            rounds: 2,
            usage: Usage {
                input: 5,
                total: 7,
                ..Usage::default()
            },
            error: Some("no api key".to_owned()),
        });
        assert_eq!(d.agent_names(), ["review".to_owned()]);
        assert!(d.agent("review").is_some_and(|a| a.read_only));
        assert!(d.agent("x").is_none());
        let ledger = Arc::new(DelegationLedger::default());
        let cx = RunCtx {
            ledger: Some(Arc::clone(&ledger)),
            ..RunCtx::default()
        };
        let spec = DelegateSpec {
            agent: "review".to_owned(),
            task: "t".to_owned(),
            effort: Some(Effort::Low),
        };
        let out = d.run(&cx, spec.clone()).await;
        assert_eq!(out.result.reply, "ok");
        assert_eq!(out.result.rounds, 2);
        assert_eq!(
            out.error.map(|e| e.to_string()),
            Some("no api key".to_owned())
        );
        assert_eq!(super::lock(&d.ran).as_slice(), &[spec]);
        assert_eq!(ledger.snapshot().map(|(r, u)| (r, u.total)), Some((2, 7)));

        // No canned outcome: an empty success, nothing booked.
        let bare = FakeDelegator::new(BTreeMap::new());
        let out = bare.run(&RunCtx::default(), DelegateSpec::default()).await;
        assert!(out.error.is_none());
        assert_eq!(out.result.rounds, 0);
    }

    #[tokio::test]
    async fn fake_tool_provider_scripts() {
        let cancel = CancellationToken::new();
        let tools = [crate::provider::model::ToolDef {
            name: "noop".to_owned(),
            ..Default::default()
        }];

        // looping(1, 2): two rounds with one call each, then "done"; endless when stop_after == 0.
        let p = FakeToolProvider::looping(1, 2);
        let mut sink = RecordingSink::default();
        for n in 1..=2 {
            let r = p
                .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
                .await
                .expect("round");
            assert_eq!(r.tool_calls.len(), 1);
            assert_eq!(r.tool_calls[0].id, format!("call-{n}"));
            assert_eq!(r.tool_calls[0].name, "noop");
        }
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("final");
        assert_eq!(r.content, "done");
        assert!(r.tool_calls.is_empty());
        assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(super::lock(&p.seen_tools).len(), 3);
        assert_eq!(super::lock(&p.seen_tools)[0], vec!["noop".to_owned()]);
        assert_eq!(sink.events, vec![SinkEvent::ReasoningDone; 3]);

        let endless = FakeToolProvider::looping(3, 0);
        for n in 1..=100 {
            let r = endless
                .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
                .await
                .expect("round");
            assert_eq!(r.tool_calls.len(), 3);
            assert_eq!(r.tool_calls[2].id, format!("call-{n}-3"));
        }

        // reporting(2, Some(2)): round 1 reports usage, call 2 fails, round 2 is still queued.
        let p = FakeToolProvider::reporting(2, Some(2));
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("round 1");
        assert_eq!(r.tool_calls[0].id, "c1");
        assert_eq!(
            r.usage,
            Some(Usage {
                input: 100,
                output: 10,
                cache_read: 1,
                total: 110,
                ..Usage::default()
            })
        );
        let err = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect_err("boom");
        assert_eq!(err.to_string(), "boom");
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("round 2");
        assert_eq!(r.tool_calls[0].id, "c2");
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("final");
        assert_eq!(r.content, "final answer");
        assert_eq!(r.usage.map(|u| u.total), Some(330));
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("past the script");
        assert_eq!(r.content, "final answer");
        assert!(r.usage.is_none());

        // scripted: rounds verbatim then the final text; the unary path consumes the same script.
        let p = FakeToolProvider::scripted(
            vec![crate::provider::RoundResult {
                content: "scripted".to_owned(),
                usage: Some(Usage {
                    input: 1,
                    ..Usage::default()
                }),
                ..Default::default()
            }],
            "fin",
        );
        assert_eq!(p.kind(), crate::provider::ProviderKind::OpenAi);
        assert_eq!(p.model(), "gpt-test");
        assert!(p.as_tool_provider().is_some());
        assert!(p.list_models(&cancel).await.expect("models").is_empty());
        let c = p.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(c.text, "scripted");
        assert_eq!(c.usage.map(|u| u.input), Some(1));
        let c = p.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(c.text, "fin");
        assert!(c.usage.is_none());
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
