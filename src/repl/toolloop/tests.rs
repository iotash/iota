#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP49 L3 suite: the interactive turn engine driven end to end against
//! `iota_core::testing::ScriptedUi` and a scripted streaming provider — the orderings Go
//! could never unit-test, because `chat.Run` needed a terminal.
//!
//! The turn engine is crate-private by design (its only production consumer is the run loop),
//! so these tests live in-file (formerly a `#[path]`-mounted `tests/toolloop.rs`; merged
//! 2026-09-02).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::BoxFuture;
use crate::markdown::CodeTheme;
use crate::provider::model::{
    AssistantBody, Attachment, Body, Message, Raw, RawContent, Role, ToolCall,
};
use crate::provider::model::{JsonObject, ToolDef};
use crate::provider::sink::StreamSink;
use crate::provider::{Provider, RoundResult};
use crate::testing::{
    Failure, FakeProvider, Interrupt, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent, lock,
};
use crate::tool::Dispatcher;
use crate::tool::context::RunCtx;
use crate::tool::{
    Artifact, ArtifactKind, AskOption, AskQuestion, AskSpec, Interactor as _, Presentation,
    ToolOutput, ToolResult, post_artifact,
};
use crate::ui::facade::{
    BusyGuard, Input, PanelKind, PanelResult, PreviewHandle, ProgressState, ScopeGuard, StatusData,
    Suggestion, TabbedResult, TabbedSpec, Ui, UiError, UiStreamSink,
};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use crate::host::Presenter;
use crate::repl::approval::ApprovalGate;
use crate::repl::meter::CtxMeter;
use crate::repl::steer::Steerer;
use crate::repl::transcript::Transcript;
use crate::repl::turn::{TurnCtx, TurnFailure, TurnReport, run_turn};

// ---------------------------------------------------------------------------
// the scripted streaming provider
// ---------------------------------------------------------------------------

/// A `ToolProvider` playing `rounds` THROUGH the sink, so the render state machine sees real
/// deltas; every call past the script gets an empty round.
fn stream(rounds: Vec<Round>) -> FakeProvider {
    FakeProvider::new().with_tools().rounds(rounds)
}

/// The same script without the `ToolProvider` capability — the dedicated image dialects' shape.
fn unary(rounds: Vec<Round>) -> FakeProvider {
    FakeProvider::new().rounds(rounds)
}

// ---------------------------------------------------------------------------
// dispatcher doubles
// ---------------------------------------------------------------------------

/// A one-tool dispatcher with a chosen presentation that posts a fixed artifact.
struct ArtDispatch {
    name: String,
    mode: Presentation,
    artifact: Option<Artifact>,
    text: String,
    /// Whether the tool reports `requires_approval`.
    approval: bool,
}

impl ArtDispatch {
    fn new(name: &str, mode: Presentation, artifact: Option<Artifact>) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            mode,
            artifact,
            text: "ok".to_owned(),
            approval: false,
        })
    }
}

impl Dispatcher for ArtDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: self.name.clone(),
            ..ToolDef::default()
        }]
    }

    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Some(a) = &self.artifact {
                post_artifact(cx, a.clone());
            }
            Ok(ToolOutput::ok(self.text.clone()))
        })
    }

    fn presentation(&self, _name: &str) -> Presentation {
        self.mode
    }

    fn requires_approval(&self, _name: &str) -> bool {
        self.approval
    }
}

/// A dispatcher that hands out one pending tool definition after its first call — the
/// `take_pending_loads` mount point.
#[derive(Default)]
struct LoaderDispatch {
    called: Mutex<u32>,
    handed: Mutex<bool>,
}

impl Dispatcher for LoaderDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "search_tools".to_owned(),
            ..ToolDef::default()
        }]
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            *lock(&self.called) += 1;
            Ok(ToolOutput::ok("loaded"))
        })
    }

    fn take_pending_loads(&self) -> Vec<ToolDef> {
        let mut handed = lock(&self.handed);
        if *handed || *lock(&self.called) == 0 {
            return Vec::new();
        }
        *handed = true;
        vec![ToolDef {
            name: "late_tool".to_owned(),
            ..ToolDef::default()
        }]
    }
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// One turn's wiring over the scripted facade.
struct Fx {
    ui: Arc<ScriptedUi>,
    tr: Arc<Transcript>,
    cx: TurnCtx,
    root: CancellationToken,
}

impl Fx {
    fn new(dispatch: Arc<dyn Dispatcher>, script: Vec<Reply>) -> Self {
        Self::with_images(dispatch, script, Arc::new(|| None))
    }

    fn with_images(
        dispatch: Arc<dyn Dispatcher>,
        script: Vec<Reply>,
        images_dir: crate::repl::turn::ImagesDir,
    ) -> Self {
        let ui = ScriptedUi::new(script);
        let ui_dyn: Arc<dyn Ui> = Arc::clone(&ui) as Arc<dyn Ui>;
        let tr = Arc::new(Transcript::new(Arc::clone(&ui_dyn), None));
        let pres = Arc::new(Presenter::with_hosts(Vec::new(), true));
        let gate = Arc::new(ApprovalGate::new(
            Arc::clone(&ui_dyn),
            Arc::clone(&tr),
            Arc::clone(&pres),
        ));
        Self {
            ui,
            tr: Arc::clone(&tr),
            cx: TurnCtx {
                ui: ui_dyn,
                tr,
                dispatch,
                gate,
                overlay: String::new(),
                images_dir,
                can_retry: true,
                code_theme: CodeTheme::Monokai,
                pres,
            },
            root: CancellationToken::new(),
        }
    }

    async fn turn(&self, p: &dyn Provider, history: &mut Vec<Message>) -> TurnReport {
        let mut ctxm = CtxMeter::disabled();
        let mut steer = Steerer::new(Arc::clone(&self.cx.ui), Arc::clone(&self.cx.tr));
        run_turn(&self.cx, &self.root, p, history, &mut ctxm, &mut steer).await
    }

    fn events(&self) -> Vec<UiEvent> {
        self.ui.events()
    }
}

/// `n` empty steering drains — one per tool round (the loop asks at every round boundary).
fn quiet(n: usize) -> Vec<Reply> {
    (0..n).map(|_| Reply::Queued(Vec::new())).collect()
}

/// A `select` answer: the row the user committed.
fn choose(index: usize) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor: index,
            ..PanelResult::default()
        }],
    })
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: match args {
            serde_json::Value::Object(m) => m,
            _ => JsonObject::new(),
        },
    }
}

/// Every committed scrollback line, flattened in order.
fn printed(events: &[UiEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Print(lines) => Some(lines.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The plain text of every committed line (SGR stripped).
fn plain(events: &[UiEvent]) -> Vec<String> {
    printed(events)
        .iter()
        .map(|l| crate::text::ansi::strip_sgr(l))
        .collect()
}

/// Index of the first event matching `pred`.
fn pos(events: &[UiEvent], pred: impl Fn(&UiEvent) -> bool) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("event not found in {events:#?}"))
}

// ---------------------------------------------------------------------------
// the render sink (streaming state machine)
// ---------------------------------------------------------------------------

// New (Go's streamToolRound could not be unit-tested): the T-36 dispatch rule — a
// ToolProvider dialect with an EMPTY tool set still routes through the tool loop, still
// streams markdown, and still commits its rendered block.
#[tokio::test]
async fn no_tools_turn_streams_markdown_through_the_tool_loop() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let p = stream(vec![Round {
        content: vec!["Hello ".to_owned(), "**world**\n".to_owned()],
        result: RoundResult {
            content: "Hello **world**\n".to_owned(),
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("hi")];

    let report = fx.turn(&p, &mut history).await;
    let out = report.outcome.expect("turn");
    assert_eq!(out.content, "Hello **world**\n");
    assert!(!report.used_tools, "an empty tool set is not a tool turn");
    assert_eq!(p.calls(), 1);

    let events = fx.events();
    assert!(
        matches!(events.first(), Some(UiEvent::StreamStart)),
        "the turn opens its stream scope first: {events:#?}"
    );
    assert!(
        matches!(events.last(), Some(UiEvent::Done)),
        "and closes it last: {events:#?}"
    );
    assert_eq!(plain(&events), vec!["Hello world".to_owned()]);
}

// New: ESC mid-stream yields the partials the user actually saw — the finalize table's
// case 1 is reachable on a no-tools turn (T-36's whole point).
#[tokio::test]
async fn esc_mid_stream_yields_the_partials() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let root = fx.root.clone();
    let p = stream(vec![Round {
        content: vec!["half an ans".to_owned()],
        interrupt: Some(Interrupt::Token(root)),
        fail: Some(Failure::Other("context canceled".to_owned())),
        ..Round::default()
    }]);
    let mut history = vec![Message::user("hi")];

    let report = fx.turn(&p, &mut history).await;
    assert!(
        report.is_interrupted(),
        "want Interrupted, got {:?}",
        report.outcome
    );
    assert_eq!(
        report.partial, "half an ans",
        "the partial rides the report out"
    );
    assert!(report.partial_reasoning.is_empty());
    // What the user saw is committed before the turn unwinds.
    assert_eq!(plain(&fx.events()), vec!["half an ans".to_owned()]);
}

// A response with reasoning and nothing else renders the
// REASONING as the answer rather than reporting an empty turn.
#[tokio::test]
async fn reasoning_only_response_becomes_the_answer() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let p = stream(vec![Round {
        reasoning: vec!["let me ".to_owned(), "think\n".to_owned()],
        result: RoundResult {
            reasoning: "let me think\n".to_owned(),
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("hi")];

    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(out.content, "let me think\n");
    assert_eq!(out.reasoning, "let me think\n");

    let events = fx.events();
    // The thinking widget rose, its segment settled, and the text landed as content.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::CallPreview(l) if l.contains("Thinking"))),
        "no thinking widget: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::CallLine(l) if l.contains("◇ thought"))),
        "no settled thinking row: {events:#?}"
    );
    // A content boundary settles the activity group FIRST, so the thinking summary is
    // committed above the answer it produced.
    assert_eq!(
        plain(&events),
        vec![
            "◇ thought for <1s".to_owned(),
            String::new(),
            "let me think".to_owned()
        ]
    );
}

// A turn that ends in TEXT carries the terminating round's raw
// blocks out, so the run loop can stamp them on the assistant message it builds. Anthropic
// rejects a replayed thinking-mode turn whose thinking block is missing, and a turn ending in
// text is the common case, not the tool-round one.
#[tokio::test]
async fn terminating_text_round_carries_its_raw_blocks_out() {
    let sealed = Raw::from_string(
        r#"{"type":"thinking","thinking":"weigh it","signature":"SEAL"}"#.to_owned(),
    )
    .expect("raw");
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let p = stream(vec![Round {
        content: vec!["the answer".to_owned()],
        result: RoundResult {
            content: "the answer".to_owned(),
            raw_content: Some(RawContent::Anthropic(vec![sealed.clone()])),
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("hi")];

    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(out.content, "the answer");
    assert_eq!(
        out.raw_content,
        Some(RawContent::Anthropic(vec![sealed])),
        "the closing round's thinking block must leave the turn engine"
    );
}

// An image-only round is a valid turn, not an empty one.
#[tokio::test]
async fn image_only_round_is_a_valid_turn() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let p = stream(vec![Round {
        result: RoundResult {
            images: vec![png()],
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("draw")];

    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert!(out.content.is_empty());
    assert_eq!(out.images.len(), 1, "the images reach the success path");
    assert!(plain(&fx.events()).is_empty(), "nothing was rendered");
}

// Every byte reaches the history buffer, the
// mark fires exactly once, the first tool delta CUTS the render pipe and everything after
// it spills into its own block. Dormant in T1 (no dialect emits `tool_delta` — T-11);
// driven here directly so WP55 flips a tested path on.
#[tokio::test]
async fn the_content_tap_feeds_the_history_and_cuts_on_the_first_tool_delta() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&["write_file"])), Vec::new());
    let cancel = fx.root.child_token();
    let sink: Arc<dyn UiStreamSink> = Arc::from(fx.cx.ui.start_stream(cancel.clone()));
    let t = crate::repl::turn::Turn {
        cx: &fx.cx,
        sink,
        cancel,
        progress: crate::llm::progress::TurnProgress::new(),
    };
    let mut rs = crate::repl::turn::RenderSink::new(&t);

    rs.content("before ");
    rs.content("the cut\n");
    rs.tool_delta(None, "{\"pa"); // anonymous: cuts, never raises
    rs.tool_delta(Some("write_file"), "th\":"); // named: raises the composing widget
    rs.tool_delta(Some("write_file"), "\"a\"}"); // same label: no second raise
    rs.content("after the cut\n"); // spills
    rs.close_block();
    rs.render_spill();
    rs.finish();

    let events = fx.events();
    let raised: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::CallPreview(l) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(raised.len(), 1, "one raise for one label: {raised:?}");
    assert!(raised[0].contains("[write_file …]"), "{raised:?}");
    assert_eq!(
        plain(&events),
        vec![
            "before the cut".to_owned(),
            String::new(), // the composing widget's own block separator
            String::new(), // the spill block's separator
            "after the cut".to_owned(),
        ],
        "the spill is misordered but VISIBLE"
    );
}

// New: the mark-content law — `tr.mark_content` fires on the FIRST byte and only then, so
// a tool call announced in the same network read still defers behind the content block.
#[tokio::test]
async fn mark_content_fires_once_and_defers_the_widget() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&["shell"])), Vec::new());
    let cancel = fx.root.child_token();
    let sink: Arc<dyn UiStreamSink> = Arc::from(fx.cx.ui.start_stream(cancel.clone()));
    let t = crate::repl::turn::Turn {
        cx: &fx.cx,
        sink,
        cancel,
        progress: crate::llm::progress::TurnProgress::new(),
    };
    let mut rs = crate::repl::turn::RenderSink::new(&t);

    rs.content("text\n");
    // A widget announced while content is open is only REMEMBERED.
    fx.tr.open_call("[shell]");
    assert!(
        !fx.events()
            .iter()
            .any(|e| matches!(e, UiEvent::CallPreview(_))),
        "the widget rose while content was open"
    );
    rs.close_block(); // close_content raises the deferred call
    assert!(
        fx.events()
            .iter()
            .any(|e| matches!(e, UiEvent::CallPreview(l) if l == "[shell]")),
        "the deferred widget was not raised at close"
    );
}

// ---------------------------------------------------------------------------
// the tool walk
// ---------------------------------------------------------------------------

// The gate's three answers. Allow
// once asks again for the next call; allow for this session remembers the TOOL NAME; deny
// records the refusal as an is_error tool result and the turn continues.
#[tokio::test]
async fn approval_allow_once_session_and_deny() {
    // Two gated calls in one round, answered "Allow once" twice.
    let dispatch = Arc::new(StaticDispatcher::new(&["write_file"]).with_approval(&["write_file"]));
    let mut script = vec![choose(0), choose(0)];
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![
            call("c1", "write_file", serde_json::json!({"path": "a.txt"})),
            call("c2", "write_file", serde_json::json!({"path": "b.txt"})),
        ]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("edit")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    let events = fx.events();
    let asks: Vec<&crate::testing::TabbedSummary> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(asks.len(), 2, "allow-once asks per call");
    assert_eq!(
        asks[0].panels[0].title,
        "write_file path:a.txt wants to modify files — allow?"
    );
    assert_eq!(
        asks[0].panels[0].items,
        [
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
            "Deny".to_owned()
        ]
    );
    // The group clock freezes while the user deliberates and restarts after.
    assert!(
        pos(&events, |e| matches!(e, UiEvent::PauseClock))
            < pos(&events, |e| matches!(e, UiEvent::Tabbed(_)))
    );
    assert!(events.iter().any(|e| matches!(e, UiEvent::ResumeClock)));

    // "Allow for this session": the SECOND call is not asked again.
    let dispatch = Arc::new(StaticDispatcher::new(&["write_file"]).with_approval(&["write_file"]));
    let mut script = vec![choose(1)];
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![
            call("c1", "write_file", serde_json::json!({"path": "a.txt"})),
            call("c2", "write_file", serde_json::json!({"path": "b.txt"})),
        ]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("edit")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(
        fx.events()
            .iter()
            .filter(|e| matches!(e, UiEvent::Tabbed(_)))
            .count(),
        1,
        "the session grant covers the second call"
    );

    // "Deny": the refusal is the call's result and the turn continues.
    let dispatch = Arc::new(StaticDispatcher::new(&["write_file"]).with_approval(&["write_file"]));
    let mut script = vec![choose(2)];
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "write_file",
            serde_json::json!({"path": "a.txt"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("edit")];
    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(out.content, "done", "a declined call does not end the turn");
    let refusal = history
        .iter()
        .find(|m| m.role() == Role::Tool)
        .expect("a tool result");
    assert_eq!(refusal.content, "The user declined this call.");
    assert!(refusal.is_error());
    assert_eq!(refusal.tool_call_id(), "c1");
}

// A run of parallel-capable calls executes as ONE batch under
// ONE widget and ONE cancel scope; event rows and results keep CALL order regardless of
// who finished first, and the serial call after the run takes the normal path.
#[tokio::test]
async fn parallel_batch_keeps_one_widget_and_call_order() {
    let dispatch =
        Arc::new(StaticDispatcher::new(&["read_file", "write_file"]).with_parallel(&["read_file"]));
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![
            call("r1", "read_file", serde_json::json!({"path": "a"})),
            call("r2", "read_file", serde_json::json!({"path": "b"})),
            call("w1", "write_file", serde_json::json!({"path": "c"})),
        ]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("go")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    let events = fx.events();
    // ONE scope for the whole batch, plus one for the serial call.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, UiEvent::ScopePush))
            .count(),
        2,
        "one scope for the batch, one for the serial call: {events:#?}"
    );
    let rows: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::CallLine(l) => Some(crate::text::ansi::strip_sgr(l)),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(rows[0].contains("path:a"), "{rows:?}");
    assert!(rows[1].contains("path:b"), "{rows:?}");
    assert!(rows[2].contains("path:c"), "{rows:?}");
    // Results answer their calls in CALL order — a protocol requirement.
    let ids: Vec<&str> = history
        .iter()
        .filter(|m| m.role() == Role::Tool)
        .map(Message::tool_call_id)
        .collect();
    assert_eq!(ids, ["r1", "r2", "w1"]);
}

// New (DIVERGENCES X-05): the same batching law, driven through the REAL `shell` toolset —
// two `shell` calls in one round are ONE batch. The stub above proves the walk; this proves
// what `BashTool::supports_parallel` actually answers, and the wall clock proves the two
// `sleep 1`s overlapped instead of queueing.
#[tokio::test]
async fn shell_calls_share_one_parallel_batch() {
    use std::time::{Duration, Instant};

    // `sandbox: off` + `auto_run: true`: no gate to answer and no sandbox to depend on, so
    // the test measures the batch and nothing else.
    let node: crate::tool::sets::RawNode =
        serde_norway::from_str("sandbox: off\nauto_run: true\n").expect("shell config");
    let mut cfg = crate::tool::sets::ToolsConfig::new();
    cfg.insert("shell".to_owned(), node);
    let registry = crate::tool::Registry::build(&crate::tool::ToolEnv::default(), &cfg, &mut |w| {
        panic!("the shell set complained: {w}")
    });
    assert!(
        registry.supports_parallel("shell", None),
        "the shell tool must batch"
    );
    let dispatch: Arc<dyn Dispatcher> = Arc::new(registry);

    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![
            call("b1", "shell", serde_json::json!({"command": "sleep 1"})),
            call("b2", "shell", serde_json::json!({"command": "sleep 1"})),
        ]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("go")];
    let started = Instant::now();
    fx.turn(&p, &mut history).await.outcome.expect("turn");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(1800),
        "two `sleep 1` calls took {elapsed:?} — they were serialized"
    );
    // ONE cancel scope for the whole batch (a serial pair would push two).
    assert_eq!(
        fx.events()
            .iter()
            .filter(|e| matches!(e, UiEvent::ScopePush))
            .count(),
        1,
        "the two calls did not share one batch"
    );
    // …and the results still answer their calls in CALL order.
    let ids: Vec<&str> = history
        .iter()
        .filter(|m| m.role() == Role::Tool)
        .map(Message::tool_call_id)
        .collect();
    assert_eq!(ids, ["b1", "b2"]);
}

// An expanded call is a group
// boundary that settles into its posted DIFF artifact (T-35): the header carries the ±
// counts and the diff rows follow. The artifact never reaches the model.
#[tokio::test]
async fn showcase_expands_the_posted_diff_artifact() {
    let artifact = Artifact {
        kind: ArtifactKind::Diff,
        title: "a.txt".to_owned(),
        lines: vec![
            "@@ -1,2 +1,2 @@".to_owned(),
            " keep".to_owned(),
            "-old".to_owned(),
            "+new".to_owned(),
        ],
    };
    let dispatch = ArtDispatch::new("edit_file", Presentation::Expanded, Some(artifact));
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "a.txt"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("edit")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    let events = fx.events();
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::ClosePreview)),
        "the showcase widget was never morphed: {events:#?}"
    );
    let lines = plain(&events);
    assert!(
        lines[0].contains("[edit_file path:a.txt]") && lines[0].contains("+1 -1"),
        "showcase header: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("new")),
        "the diff body is missing: {lines:?}"
    );
    // The model still sees only the result text.
    let result = history
        .iter()
        .find(|m| m.role() == Role::Tool)
        .expect("tool result");
    assert_eq!(result.content, "ok");
}

// A `Note` artifact rides the classic
// event row's trailing detail instead of expanding.
#[tokio::test]
async fn note_artifact_rides_the_event_row() {
    let artifact = Artifact {
        kind: ArtifactKind::Note,
        title: String::new(),
        lines: vec!["3 rounds".to_owned(), "1.2k tokens".to_owned()],
    };
    let dispatch = ArtDispatch::new("survey", Presentation::Group, Some(artifact));
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "survey",
            serde_json::json!({"topic": "review"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("survey")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    let row = fx
        .events()
        .iter()
        .find_map(|e| match e {
            UiEvent::CallLine(l) => Some(crate::text::ansi::strip_sgr(l)),
            _ => None,
        })
        .expect("an event row");
    assert!(
        row.ends_with(" · 3 rounds · 1.2k tokens"),
        "the note is missing from {row:?}"
    );
}

// An interactive tool brings its own surface: it never enters
// the activity group, the clock freezes while the user answers, and the outcome lands as
// its own `?` record block.
#[tokio::test]
async fn surface_call_records_the_answer_and_pauses_the_clock() {
    let dispatch = ArtDispatch::new("choose", Presentation::Surface, None);
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(dispatch, script);
    let p = stream(vec![
        Round::calls(vec![call("c1", "choose", serde_json::json!({}))]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("ask me")];
    let report = fx.turn(&p, &mut history).await;
    report.outcome.expect("turn");
    assert_eq!(report.side_fx, 1, "a replay would re-ask the user");

    let events = fx.events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, UiEvent::CallPreview(_) | UiEvent::CallLine(_))),
        "an interactive call must not enter the activity panel: {events:#?}"
    );
    assert_eq!(
        plain(&events),
        vec!["? ok".to_owned(), String::new(), "done".to_owned()],
        "the answer lands as its own record block, the reply follows"
    );
}

// The round boundary in order: the user's queued message is
// ECHOED (which settles the running activity group) and appended, and only then do the
// tool definitions loaded this round mount, at the BOTTOM (T-24) and never before round 0.
#[tokio::test]
async fn steer_echo_settles_the_group_before_the_bottom_mount() {
    let dispatch = Arc::new(LoaderDispatch::default());
    let fx = Fx::new(
        Arc::clone(&dispatch) as Arc<dyn Dispatcher>,
        vec![
            Reply::Queued(vec![Input {
                display: "also check b".to_owned(),
                text: "also check b".to_owned(),
                ..Input::default()
            }]),
            Reply::Queued(Vec::new()),
        ],
    );
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "search_tools",
            serde_json::json!({"q": "x"}),
        )]),
        Round::calls(vec![call(
            "c2",
            "search_tools",
            serde_json::json!({"q": "y"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("start")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    // History shape: the injection lands after the round's tool results, the system-tools
    // mount after the injection.
    let shape: Vec<(Role, String)> = history
        .iter()
        .map(|m| (m.role(), m.content.clone()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (Role::User, "start".to_owned()),
            (Role::Assistant, String::new()),
            (Role::Tool, "loaded".to_owned()),
            (Role::User, "also check b".to_owned()),
            (Role::System, String::new()),
            (Role::Assistant, String::new()),
            (Role::Tool, "loaded".to_owned()),
        ]
    );
    assert_eq!(
        history[4].tools().first().map(|t| t.name.as_str()),
        Some("late_tool"),
        "the frozen mount carries the loaded schemas"
    );
    // Never before round 0: the first request carried no system-tools message.
    assert!(
        !p.send(0).iter().any(|m| m.role() == Role::System),
        "a system-tools message reached the FIRST request"
    );

    // The ❯ echo settles the running group first — the user is a stronger boundary than
    // content — so the group summary is committed before the user block.
    let events = fx.events();
    let user = pos(&events, |e| matches!(e, UiEvent::UserBlock(_)));
    let settle = pos(&events, |e| matches!(e, UiEvent::ClosePreview));
    assert!(
        settle < user,
        "the activity group must settle before the ❯ block: {events:#?}"
    );
}

// New (phase C): a background job that finishes MID-TURN rides the same queue a steering message does and
// lands at the same round boundary — but as a NOTICE: one dim line instead of the `❯` block, and a
// `Body::Notice` message the model sees as ordinary user text.
#[tokio::test]
async fn a_job_notice_lands_at_the_round_boundary_as_a_notice() {
    let dispatch = Arc::new(StaticDispatcher::new(&["read_file"]));
    let fx = Fx::new(
        dispatch,
        vec![Reply::Queued(vec![Input {
            display: "[background job b1 finished: exit 0 after 2s] make test".to_owned(),
            text: "[background job b1 finished: exit 0 after 2s] make test\nall green\n".to_owned(),
            kind: crate::ui::facade::InputKind::Notice,
        }])],
    );
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "read_file",
            serde_json::json!({"path": "a"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("go")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    // The injection is a user-role message the model reads in full, flagged as a notice.
    let injected = history
        .iter()
        .find(|m| m.is_notice())
        .expect("the notice never joined the history");
    assert_eq!(injected.role(), Role::User);
    assert!(
        injected.content.ends_with("all green\n"),
        "the model gets the whole notice: {:?}",
        injected.content
    );
    // …and it reached the SECOND request, right after the round's tool results.
    let send = p.send(1);
    assert!(
        send.iter().any(|m| m.content == injected.content),
        "the notice never reached the model"
    );

    // On screen it is a dim one-liner, never a `❯` block.
    let events = fx.events();
    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::UserBlock(_))),
        "a notice must not echo as something the user typed: {events:#?}"
    );
    let printed = plain(&events).join("\n");
    assert!(
        printed.contains("[background job b1 finished: exit 0 after 2s] make test"),
        "the headline is missing from the transcript:\n{printed}"
    );
    assert!(
        !printed.contains("all green"),
        "the job's output belongs to the model, not to the scrollback:\n{printed}"
    );
}

// New (Go could not test Run): the round-2 request is a legal conversation — every
// assistant message that requested tools is followed IMMEDIATELY by exactly its results,
// and a steering injection sits after them, never between a call and its answer.
#[tokio::test]
async fn replayed_history_keeps_the_tool_result_role_shape() {
    let dispatch = Arc::new(StaticDispatcher::new(&["read_file"]));
    let fx = Fx::new(
        dispatch,
        vec![Reply::Queued(vec![Input {
            display: "and b".to_owned(),
            text: "and b".to_owned(),
            ..Input::default()
        }])],
    );
    let p = stream(vec![
        Round::calls(vec![
            call("c1", "read_file", serde_json::json!({"path": "a"})),
            call("c2", "read_file", serde_json::json!({"path": "b"})),
        ]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("read them")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");

    let send = p.send(1);
    assert!(!send.is_empty(), "the second request was never made");
    let mut i = 0;
    let mut checked = 0;
    while i < send.len() {
        let m = &send[i];
        if m.role() == Role::Assistant && !m.tool_calls().is_empty() {
            for (k, tc) in m.tool_calls().iter().enumerate() {
                let answer = send
                    .get(i + 1 + k)
                    .unwrap_or_else(|| panic!("call {} has no answer in {send:#?}", tc.id));
                assert_eq!(answer.role(), Role::Tool, "{send:#?}");
                assert_eq!(
                    answer.tool_call_id(),
                    tc.id,
                    "results follow calls in order"
                );
                checked += 1;
            }
            i += 1 + m.tool_calls().len();
            continue;
        }
        i += 1;
    }
    assert_eq!(checked, 2, "both calls were answered in place");
    assert_eq!(
        send.last().map(|m| (m.role(), m.content.as_str())),
        Some((Role::User, "and b")),
        "the injection lands at the END of the request"
    );
}

// A mid-loop round's images are
// saved and attached to the message they belong to, with a caption notice per save; the
// terminating round's images travel out for the run loop's final attach.
#[tokio::test]
async fn round_and_final_images_are_saved_attached_and_captioned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let dispatch = Arc::new(StaticDispatcher::new(&["read_file"]));
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::with_images(
        dispatch,
        script,
        Arc::new(move || Some(path.clone()) as Option<PathBuf>),
    );
    let p = stream(vec![
        Round {
            result: RoundResult {
                tool_calls: vec![call("c1", "read_file", serde_json::json!({"path": "a"}))],
                images: vec![png()],
                ..RoundResult::default()
            },
            ..Round::default()
        },
        Round {
            result: RoundResult {
                content: "here it is".to_owned(),
                images: vec![png()],
                ..RoundResult::default()
            },
            ..Round::default()
        },
    ]);
    let mut history = vec![Message::user("draw")];
    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");

    let assistant = history
        .iter()
        .find(|m| m.role() == Role::Assistant)
        .expect("the round's assistant message");
    assert_eq!(
        assistant.attachments.len(),
        1,
        "the saved image rides the message it belongs to"
    );
    assert!(
        std::path::Path::new(&assistant.attachments[0].filename)
            .extension()
            .is_some_and(|e| e == "png"),
        "the filename is rewritten to the saved file: {:?}",
        assistant.attachments[0].filename
    );
    assert!(
        dir.path().join(&assistant.attachments[0].filename).exists(),
        "the file was not written"
    );
    assert_eq!(
        out.images.len(),
        1,
        "the terminating round's images travel out for the final attach"
    );
    let caption = plain(&fx.events())
        .into_iter()
        .find(|l| l.contains("🖼 saved:"))
        .expect("a caption notice");
    assert!(caption.contains(".png"), "{caption:?}");
}

// A save that fails says so and drops the image; the turn
// is unharmed.
#[tokio::test]
async fn image_save_failure_reports_and_drops_the_image() {
    let dispatch = Arc::new(StaticDispatcher::new(&[]));
    // No directory at all: `save_image` fails with Go's `$HOME is not defined`.
    let fx = Fx::with_images(dispatch, Vec::new(), Arc::new(|| None));
    let p = stream(vec![Round {
        result: RoundResult {
            content: "done".to_owned(),
            images: vec![png()],
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("draw")];
    let out = fx.turn(&p, &mut history).await.outcome.expect("turn");

    // The TERMINATING round's images are the run loop's to save; collect them here the way
    // the success path will.
    let mut msg = Message {
        content: out.content.clone(),
        body: Body::Assistant(AssistantBody {
            ..AssistantBody::default()
        }),
        ..Message::default()
    };
    crate::repl::turn::collect_images(&fx.tr, 80, None, &out.images, &mut msg);
    assert!(msg.attachments.is_empty(), "a failed save attaches nothing");
    let lines = plain(&fx.events());
    assert!(
        lines
            .iter()
            .any(|l| l == "Saving image failed: $HOME is not defined"),
        "the failure line is missing: {lines:?}"
    );
}

// New (chat/images.go:115): the image directory resolves ONLY when an image actually needs
// saving — an image-less turn must not create the session bundle's images directory.
#[tokio::test]
async fn image_less_turn_never_resolves_the_images_dir() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let fx = Fx::with_images(
        Arc::new(StaticDispatcher::new(&["read_file"])),
        quiet(1),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        }),
    );
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "read_file",
            serde_json::json!({"path": "a"}),
        )]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("go")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "an image-less turn resolved the images directory"
    );
}

// New: the tool loop refreshes the advertised set from round 1 on, so a tool loaded this
// round appears in the very next request.
#[tokio::test]
async fn advertised_tools_refresh_after_round_zero() {
    let dispatch = Arc::new(LoaderDispatch::default());
    let mut script = Vec::new();
    script.extend(quiet(1));
    let fx = Fx::new(Arc::clone(&dispatch) as Arc<dyn Dispatcher>, script);
    let p = stream(vec![
        Round::calls(vec![call("c1", "search_tools", serde_json::json!({}))]),
        Round::text("done"),
    ]);
    let mut history = vec![Message::user("go")];
    fx.turn(&p, &mut history).await.outcome.expect("turn");
    assert_eq!(p.seen_tools().len(), 2);
    assert_eq!(p.seen_tools()[1], vec!["search_tools".to_owned()]);
}

// The ask seam over the tabbed surface: one tab per question,
// the short header as the chip, the question as the panel prompt, wizard Enter, and the
// engine's inline `"Other…"` editor. Picks and a custom answer COEXIST on a multi-select.
#[tokio::test]
async fn interactor_maps_questions_onto_the_tabbed_wizard() {
    let ui = ScriptedUi::new(vec![Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 1,
        panels: vec![
            PanelResult {
                cursor: 0,
                ..PanelResult::default()
            },
            PanelResult {
                // row 1 = "httpx"; row 2 = the appended "Other…" editor.
                checked: vec![1, 2],
                custom: "aiohttp".to_owned(),
                ..PanelResult::default()
            },
        ],
    })]);
    let it = crate::repl::Interactor::new();
    it.bind(Arc::clone(&ui) as Arc<dyn Ui>);
    let spec = AskSpec {
        questions: vec![
            AskQuestion {
                header: "Auth".to_owned(),
                question: "Which auth?".to_owned(),
                options: vec![
                    AskOption {
                        label: "OAuth".to_owned(),
                        description: "standard".to_owned(),
                    },
                    AskOption {
                        label: "API key".to_owned(),
                        description: String::new(),
                    },
                ],
                multiple: false,
                allow_custom: false,
            },
            AskQuestion {
                header: "Libs".to_owned(),
                question: "Which libraries?".to_owned(),
                options: vec![
                    AskOption {
                        label: "requests".to_owned(),
                        description: String::new(),
                    },
                    AskOption {
                        label: "httpx".to_owned(),
                        description: String::new(),
                    },
                ],
                multiple: true,
                allow_custom: true,
            },
        ],
    };
    let res = it.ask(&RunCtx::default(), spec).await;
    assert!(!res.declined);
    assert_eq!(res.answers[0].selected, ["OAuth".to_owned()]);
    assert!(res.answers[0].custom.is_empty());
    assert_eq!(res.answers[1].selected, ["httpx".to_owned()]);
    assert_eq!(res.answers[1].custom, "aiohttp");

    let shape = ui
        .events()
        .iter()
        .find_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s.clone()),
            _ => None,
        })
        .expect("the wizard opened");
    assert!(shape.enter_advances, "the ask surface is a wizard");
    assert_eq!(shape.panels[0].title, "Auth");
    assert_eq!(shape.panels[0].prompt, "Which auth?");
    assert_eq!(shape.panels[0].kind, PanelKind::List);
    assert!(
        !shape.panels[0].custom,
        "allow_custom: false offers no editor"
    );
    assert_eq!(shape.panels[1].kind, PanelKind::Multi);
    assert!(shape.panels[1].custom);
    assert!(
        shape.panels[0].items[0].contains("OAuth") && shape.panels[0].items[0].contains("standard"),
        "the option description rides its row: {:?}",
        shape.panels[0].items
    );

    // A cancelled wizard — and an UNBOUND one — decline, which the ask tools already handle.
    let ui = ScriptedUi::new(vec![Reply::Tabbed(TabbedResult {
        cancelled: true,
        ..TabbedResult::default()
    })]);
    let it = crate::repl::Interactor::new();
    it.bind(Arc::clone(&ui) as Arc<dyn Ui>);
    assert!(
        it.ask(&RunCtx::default(), AskSpec::default())
            .await
            .declined
    );
    let unbound = crate::repl::Interactor::new();
    assert!(
        unbound
            .ask(&RunCtx::default(), AskSpec::default())
            .await
            .declined
    );
}

// ---------------------------------------------------------------------------
// teardown order
// ---------------------------------------------------------------------------

// The pinned teardown: `sink.done()` (drops a leaked preview,
// pops the turn scope) → `tr.reset_turn()` (a dropped widget's separator is reclaimed) →
// the turn token fires. Every step is recorded with a snapshot of that token, so "cancel
// last" is asserted rather than assumed.
//
// The order is one code path, so it is pinned on the turn that can distinguish the steps:
// a turn whose LAST round leaves the activity group unsettled (no content boundary
// followed it), so `reset_turn` has real work to do, and whose token is fired by the
// teardown alone. The interrupt case below re-checks the relative order — an ESC has
// already cancelled the token by then, which is exactly why it cannot pin the third step.
#[tokio::test]
async fn turn_teardown_is_done_then_reset_then_cancel() {
    let order = order_fixture(quiet(1));
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "read_file",
            serde_json::json!({"path": "a"}),
        )]),
        // An empty terminating round: no content block, so nothing settles the group
        // before the teardown reaches it.
        Round::default(),
    ]);
    let mut history = vec![Message::user("go")];
    order.turn(&p, &mut history).await.outcome.expect("turn");

    let log = order.ui.log();
    let done = at(&log, "done");
    let settle = at(&log, "close_preview");
    assert!(done < settle, "done must precede reset_turn: {log:?}");
    assert!(
        !log[done].1 && !log[settle].1,
        "the turn token fired before the teardown finished: {log:?}"
    );
    assert!(
        order.ui.turn_cancelled(),
        "the turn token was never cancelled"
    );
}

// The same teardown on the interrupt path (chat/run.go:1072-1076): ESC ends the round, the
// stream closes, and the group still settles before the turn unwinds — so a widget the
// interrupt orphaned never survives into the next turn.
#[tokio::test]
async fn interrupt_tears_down_before_the_turn_unwinds() {
    let order = order_fixture(quiet(1));
    let root = order.root.clone();
    let p = stream(vec![
        Round::calls(vec![call(
            "c1",
            "read_file",
            serde_json::json!({"path": "a"}),
        )]),
        Round {
            interrupt: Some(Interrupt::Token(root)),
            fail: Some(Failure::Other("context canceled".to_owned())),
            ..Round::default()
        },
    ]);
    let mut history = vec![Message::user("go")];
    let report = order.turn(&p, &mut history).await;
    assert!(report.is_interrupted());
    let log = order.ui.log();
    assert!(
        at(&log, "done") < at(&log, "close_preview"),
        "done must precede reset_turn: {log:?}"
    );
    assert!(order.ui.turn_cancelled());
}

/// Index of the first `what` step in the teardown log.
fn at(log: &[(String, bool)], what: &str) -> usize {
    log.iter()
        .position(|e| e.0 == what)
        .unwrap_or_else(|| panic!("no {what} in {log:?}"))
}

/// A fixture whose facade is the [`OrderUi`] recorder.
struct OrderFx {
    ui: Arc<OrderUi>,
    cx: TurnCtx,
    root: CancellationToken,
}

impl OrderFx {
    async fn turn(&self, p: &dyn Provider, history: &mut Vec<Message>) -> TurnReport {
        let mut ctxm = CtxMeter::disabled();
        let mut steer = Steerer::new(Arc::clone(&self.cx.ui), Arc::clone(&self.cx.tr));
        run_turn(&self.cx, &self.root, p, history, &mut ctxm, &mut steer).await
    }
}

fn order_fixture(script: Vec<Reply>) -> OrderFx {
    let order = Arc::new(OrderUi::new(ScriptedUi::new(script)));
    let ui: Arc<dyn Ui> = Arc::clone(&order) as Arc<dyn Ui>;
    let tr = Arc::new(Transcript::new(Arc::clone(&ui), None));
    let pres = Arc::new(Presenter::with_hosts(Vec::new(), true));
    let cx = TurnCtx {
        ui: Arc::clone(&ui),
        tr: Arc::clone(&tr),
        dispatch: Arc::new(StaticDispatcher::new(&["read_file"])),
        gate: Arc::new(ApprovalGate::new(
            Arc::clone(&ui),
            Arc::clone(&tr),
            Arc::clone(&pres),
        )),
        overlay: String::new(),
        images_dir: Arc::new(|| None),
        can_retry: true,
        code_theme: CodeTheme::Monokai,
        pres,
    };
    OrderFx {
        ui: order,
        cx,
        root: CancellationToken::new(),
    }
}

/// A `Ui` that records the teardown steps together with a snapshot of the TURN token, so
/// "cancel comes last" is observable. Everything else forwards to the scripted double.
struct OrderUi {
    inner: Arc<ScriptedUi>,
    log: Arc<Mutex<Vec<(String, bool)>>>,
    turn: Arc<Mutex<Option<CancellationToken>>>,
}

impl OrderUi {
    fn new(inner: Arc<ScriptedUi>) -> Self {
        Self {
            inner,
            log: Arc::default(),
            turn: Arc::default(),
        }
    }

    fn log(&self) -> Vec<(String, bool)> {
        lock(&self.log).clone()
    }

    fn turn_cancelled(&self) -> bool {
        lock(&self.turn)
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

/// Records `what` with the turn token's state at that moment.
fn note(
    log: &Arc<Mutex<Vec<(String, bool)>>>,
    turn: &Arc<Mutex<Option<CancellationToken>>>,
    what: &str,
) {
    let cancelled = lock(turn)
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled);
    lock(log).push((what.to_owned(), cancelled));
}

impl Ui for OrderUi {
    fn read_input<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Input, UiError>> {
        self.inner.read_input(cancel)
    }

    fn tabbed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        spec: TabbedSpec,
    ) -> BoxFuture<'a, Result<TabbedResult, UiError>> {
        self.inner.tabbed(cancel, spec)
    }

    fn take_queued_messages(&self) -> BoxFuture<'_, Vec<Input>> {
        self.inner.take_queued_messages()
    }

    fn close(&self) -> BoxFuture<'_, std::io::Result<()>> {
        self.inner.close()
    }

    fn enqueue(&self, input: Input) {
        self.inner.enqueue(input);
    }

    fn print_lines(&self, lines: Vec<String>) {
        note(&self.log, &self.turn, "print");
        self.inner.print_lines(lines);
    }

    fn user_block(&self, display: &str) {
        self.inner.user_block(display);
    }

    fn start_stream(&self, cancel: CancellationToken) -> Box<dyn UiStreamSink> {
        *lock(&self.turn) = Some(cancel.clone());
        note(&self.log, &self.turn, "start_stream");
        Box::new(OrderSink {
            log: Arc::clone(&self.log),
            turn: Arc::clone(&self.turn),
        })
    }

    fn busy(&self, label: &str) -> BusyGuard {
        self.inner.busy(label)
    }

    fn busy_detail(&self, detail: &str) {
        self.inner.busy_detail(detail);
    }

    fn push_cancel_scope(&self, cancel: CancellationToken) -> ScopeGuard {
        self.inner.push_cancel_scope(cancel)
    }

    fn set_status(&self, s: StatusData) {
        self.inner.set_status(s);
    }

    fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    fn set_slash_commands(&self, cmds: Vec<Suggestion>) {
        self.inner.set_slash_commands(cmds);
    }

    fn call_preview(&self, label: &str) {
        self.inner.call_preview(label);
    }

    fn call_detail(&self, detail: &str) {
        self.inner.call_detail(detail);
    }

    fn call_line(&self, line: &str) {
        self.inner.call_line(line);
    }

    fn close_preview(&self) {
        note(&self.log, &self.turn, "close_preview");
        self.inner.close_preview();
    }

    fn pause_clock(&self) {
        self.inner.pause_clock();
    }

    fn resume_clock(&self) {
        self.inner.resume_clock();
    }

    fn call_body(&self, rows: Vec<String>) {
        self.inner.call_body(rows);
    }

    fn set_progress(&self, s: ProgressState) {
        self.inner.set_progress(s);
    }

    fn notify(&self, text: &str) {
        self.inner.notify(text);
    }

    fn set_dark_background(&self, dark: bool) {
        self.inner.set_dark_background(dark);
    }

    fn width(&self) -> u16 {
        self.inner.width()
    }

    fn height(&self) -> u16 {
        self.inner.height()
    }

    fn done(&self) -> CancellationToken {
        self.inner.done()
    }
}

/// The stream handle [`OrderUi`] hands out: `done` is the first teardown step.
struct OrderSink {
    log: Arc<Mutex<Vec<(String, bool)>>>,
    turn: Arc<Mutex<Option<CancellationToken>>>,
}

impl UiStreamSink for OrderSink {
    fn block_preview(&self, _label: &str) -> Box<dyn PreviewHandle> {
        Box::new(NopPreview)
    }

    fn done(&self) {
        note(&self.log, &self.turn, "done");
    }
}

struct NopPreview;

impl PreviewHandle for NopPreview {
    fn write_raw_line(&mut self, _line: &str) {}

    fn close(&mut self) {}
}

/// A 1×1 PNG (the smallest attachment `save_images_for_turn` will write).
fn png() -> Attachment {
    Attachment {
        filename: "gen.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    }
}

// New: a provider WITHOUT tool support takes the unary fallback — one `chat()` call, its
// reply rendered as one markdown block, and no stream deltas at all (T-36).
#[tokio::test]
async fn unary_provider_takes_the_stream_turn_fallback() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&["read_file"])), Vec::new());
    let p = unary(vec![Round {
        result: RoundResult {
            content: "an image, described\n".to_owned(),
            ..RoundResult::default()
        },
        ..Round::default()
    }]);
    let mut history = vec![Message::user("draw")];
    let report = fx.turn(&p, &mut history).await;
    let out = report.outcome.expect("turn");
    assert_eq!(out.content, "an image, described\n");
    assert!(!report.used_tools);
    assert_eq!(plain(&fx.events()), vec!["an image, described".to_owned()]);
}

// New: the unary fallback has no partials by construction — an interrupted image turn
// takes `finalize_interrupt`'s rollback branch (T-36's accepted cost).
#[tokio::test]
async fn unary_provider_interrupt_yields_no_partials() {
    let fx = Fx::new(Arc::new(StaticDispatcher::new(&[])), Vec::new());
    let root = fx.root.clone();
    let p = unary(vec![Round {
        interrupt: Some(Interrupt::Token(root)),
        fail: Some(Failure::Other("context canceled".to_owned())),
        ..Round::default()
    }]);
    let mut history = vec![Message::user("draw")];
    let report = fx.turn(&p, &mut history).await;
    assert!(report.is_interrupted());
    assert!(report.partial.is_empty() && report.partial_reasoning.is_empty());
}

// New: a facade that closed under the turn ends the LOOP, not just the turn — the failure
// keeps its `Ui` shape so the run loop can exit with `ReplError::Ui`.
#[tokio::test]
async fn a_closed_facade_during_approval_ends_the_loop() {
    let dispatch = Arc::new(StaticDispatcher::new(&["write_file"]).with_approval(&["write_file"]));
    let fx = Fx::new(dispatch, vec![Reply::Closed]);
    let p = stream(vec![Round::calls(vec![call(
        "c1",
        "write_file",
        serde_json::json!({"path": "a"}),
    )])]);
    let mut history = vec![Message::user("edit")];
    let report = fx.turn(&p, &mut history).await;
    assert!(
        matches!(report.outcome, Err(TurnFailure::Ui(UiError::Closed))),
        "want a Ui failure, got {:?}",
        report.outcome
    );
}
