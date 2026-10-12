//! A stalled stream, an output the ceiling cut off, the retry budget and a hung title pass through the public
//! entry point (`iota::repl::run`; DIVERGENCES X-64, X-69, X-72, X-73,
//! `docs/history/request-cancellation-review-*.md`).
//!
//! What only exists end to end is what the NEXT request carries — whether a stall kept the turn's
//! completed tool round — and whether the loop's next step (another message, a command, the exit)
//! waits on a title pass that never answers, or gives up one that would have. Both are read off the provider's call log and the wall clock.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::model::{Message, Role};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::SessionStore;
use iota::testing::{
    FakeProvider, Interrupt, Log, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent, tool_call,
};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, TabbedResult, Ui};
use tokio_util::sync::CancellationToken;

/// Far under the title pass's own 30 s deadline, far over a slow runner's turn.
const PROMPT: Duration = Duration::from_secs(10);

struct Fixture {
    ui: Arc<ScriptedUi>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
    deadline: Duration,
    /// What the run starts under and the config layers behind it (both empty unless [`Self::under`]).
    params: iota::session::LayeredParams,
    layers: iota::cmd::ParamLayers,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
            deadline: PROMPT,
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
        }
    }

    /// A run under the config `yaml` (its `default` agent): the layers it declares, and the five
    /// parameters a new session evaluates from them.
    fn under(mut self, yaml: &str) -> Self {
        let cfg =
            iota::cmd::Config::parse(yaml.as_bytes(), &iota::app::env::Env::default(), &mut |w| {
                panic!("unexpected config warning: {w}")
            })
            .expect("the config loads");
        let resolved = resolve(yaml);
        self.params = resolved
            .declared(None)
            .evaluate(&iota::session::LayeredParams::default());
        self.layers = iota::cmd::ParamLayers::new(&cfg, &resolved);
        self
    }

    /// A RESUMED run under the config `yaml`: the layers it declares, and the five parameters restored from
    /// `meta` — `replayed` false for a bundle recorded on another endpoint (a `-M` move).
    fn resumed(mut self, yaml: &str, meta: &iota::session::SessionMeta, replayed: bool) -> Self {
        self = self.under(yaml);
        self.params = resolve(yaml).declared(None).resume(meta, replayed);
        self
    }

    /// A run under tokio's paused clock, where whole retry ladders pass in virtual time: the deadline is
    /// virtual too, so it is set past the longest ladder the test could climb.
    fn patient(mut self) -> Self {
        self.deadline = Duration::from_secs(3600);
        self
    }

    /// Runs the script to its end with `provider` answering the turns, `title` the title pass and
    /// `tools` advertised; panics when the run outlives [`PROMPT`].
    async fn run(&self, provider: FakeProvider, title: Option<FakeProvider>, tools: &[&str]) {
        self.run_on(Box::new(provider), title, tools, None).await;
    }

    /// A run on `provider` with a title instance of any type (no tools, no writer).
    async fn run_on_with_title(
        &self,
        provider: Box<dyn iota::provider::Provider>,
        title: Option<Box<dyn iota::provider::Provider>>,
    ) {
        self.run_full(provider, title, &[], None).await;
    }

    /// [`Self::run`] with any provider — a real dialect against a mock server — and, when given, a
    /// session writer the turns persist through.
    async fn run_on(
        &self,
        provider: Box<dyn iota::provider::Provider>,
        title: Option<FakeProvider>,
        tools: &[&str],
        writer: Option<iota::session::SessionWriter>,
    ) {
        let title = title.map(|t| Box::new(t) as Box<dyn iota::provider::Provider>);
        self.run_full(provider, title, tools, writer).await;
    }

    /// The one place a run is assembled.
    async fn run_full(
        &self,
        provider: Box<dyn iota::provider::Provider>,
        title_provider: Option<Box<dyn iota::provider::Provider>>,
        tools: &[&str],
        writer: Option<iota::session::SessionWriter>,
    ) {
        let params = RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider,
            title_provider,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(tools)) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: McpHooks::default(),
            session: SessionCtx {
                writer,
                store: self.store.clone(),
                new_session: None,
                scope: None,
                bot: false,
                notices: Vec::new(),
                recorded_notices: Vec::new(),
                memory: None,
            },
            params: self.params.clone(),
            layers: self.layers.clone(),
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        };
        tokio::time::timeout(self.deadline, iota::repl::run(params))
            .await
            .expect("the loop waited on something that never answers")
            .expect("clean exit");
    }

    fn printed(&self) -> Vec<String> {
        self.ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Print(lines) => Some(lines),
                _ => None,
            })
            .flatten()
            .map(|l| strip_sgr(&l))
            .collect()
    }

    fn printed_count(&self, needle: &str) -> usize {
        self.printed().iter().filter(|l| l.contains(needle)).count()
    }
}

/// The `default` agent of the config `yaml`, resolved as a run resolves it.
fn resolve(yaml: &str) -> iota::cmd::Resolved {
    iota::cmd::Config::parse(yaml.as_bytes(), &iota::app::env::Env::default(), &mut |w| {
        panic!("unexpected config warning: {w}")
    })
    .expect("the config loads")
    .resolve_agent("default")
    .expect("the agent resolves")
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
    })
}

/// A message's shape, for comparing histories: role, content, and the ids of its tool calls or
/// the call it answers.
fn shape(m: &Message) -> (Role, String, Vec<String>) {
    let ids = match m.role() {
        Role::Assistant => m.tool_calls().iter().map(|c| c.id.clone()).collect(),
        Role::Tool => vec![m.tool_call_id().to_owned()],
        _ => Vec::new(),
    };
    (m.role(), m.content.clone(), ids)
}

/// A tool round runs, the round that explains its result stalls mid-stream — and the user's next
/// message still carries the call, its result and what streamed before the stall. The stall stays
/// an error (red block, no retry, no `Interrupted.`) that says the reply may be incomplete.
#[tokio::test]
async fn a_stall_after_a_tool_round_keeps_the_call_and_its_result() {
    let log = Log::default();
    let provider = FakeProvider::new()
        .with_tools()
        .rounds([
            Round::calls(vec![tool_call("c1", "noop")]),
            Round::stalled("the tool said"),
        ])
        .replying("ok")
        .with_log(log.clone());
    // The tool walk asks for steering messages once; there are none.
    let fx = Fixture::new(vec![
        input("do it"),
        Reply::Queued(Vec::new()),
        input("next"),
        Reply::Closed,
    ]);
    fx.run(provider, None, &["noop"]).await;

    assert_eq!(
        log.calls(),
        3,
        "the stall is not retried: {:?}",
        log.prompts()
    );
    let third: Vec<_> = log.send(2).iter().map(shape).collect();
    let tail = &third[third.len() - 5..];
    assert_eq!(tail[0], (Role::User, "do it".to_owned(), vec![]));
    assert_eq!(tail[1].0, Role::Assistant);
    assert_eq!(tail[1].2, vec!["c1".to_owned()], "the call is kept");
    assert_eq!(tail[2].0, Role::Tool);
    assert_eq!(tail[2].2, vec!["c1".to_owned()], "its result is kept");
    assert_eq!(
        (tail[3].0, tail[3].1.as_str()),
        (Role::Assistant, "the tool said"),
        "the text that streamed before the stall is kept"
    );
    assert_eq!(tail[4], (Role::User, "next".to_owned(), vec![]));

    assert_eq!(
        fx.printed_count("Response stalled"),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(fx.printed_count("the reply may be incomplete"), 1);
    assert_eq!(fx.printed_count("Interrupted."), 0, "a stall is not an ESC");
}

/// A stall before anything streamed leaves nothing to keep: the turn rolls back like any failure.
#[tokio::test]
async fn a_stall_with_nothing_to_keep_rolls_the_turn_back() {
    let log = Log::default();
    let mut stall = Round::stalled("");
    stall.content.clear();
    let provider = FakeProvider::new()
        .with_tools()
        .round(stall)
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![input("do it"), input("next"), Reply::Closed]);
    fx.run(provider, None, &["noop"]).await;

    assert_eq!(log.calls(), 2);
    let second: Vec<_> = log.send(1).iter().map(|m| m.content.clone()).collect();
    assert!(!second.contains(&"do it".to_owned()), "{second:?}");
    assert_eq!(fx.printed_count("Response stalled"), 1);
    assert_eq!(fx.printed_count("the reply may be incomplete"), 0);
}

/// The GLM bug (X-69, X-70): the output ceiling cuts the model off mid-THOUGHT — no text at all. The draft is not
/// promoted to the answer: the turn is an error naming the ceiling and the knob, it is not retried, and with
/// nothing to keep it rolls back — so the next request carries neither the question nor the half-thought as
/// something the assistant said.
#[tokio::test]
async fn a_thought_cut_off_by_the_ceiling_is_never_the_answer() {
    let log = Log::default();
    let provider = FakeProvider::new()
        .with_tools()
        .round(Round::cut_off(
            "The user is leaning toward option 1. This needs flagging as a",
            "",
        ))
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![input("geo dns?"), input("next"), Reply::Closed]);
    fx.run(provider, None, &[]).await;

    assert_eq!(
        log.calls(),
        2,
        "the cut is not retried: {:?}",
        log.prompts()
    );
    let second: Vec<_> = log.send(1).iter().map(shape).collect();
    assert!(
        !second
            .iter()
            .any(|(_, content, _)| content.contains("flagging as a")),
        "the half-thought leaked into the history: {second:#?}"
    );
    assert!(
        !second.iter().any(|(_, content, _)| content == "geo dns?"),
        "nothing was kept, so the turn rolled back: {second:#?}"
    );
    assert_eq!(
        fx.printed_count("Output truncated"),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(fx.printed_count("32000-token limit"), 1);
    assert_eq!(
        fx.printed_count("This session runs under the built-in default ceiling."),
        1,
        "nothing set the ceiling, and the hint says so: {:#?}",
        fx.printed()
    );
    assert_eq!(
        fx.printed_count("flagging as a"),
        0,
        "the thought must not be printed as an answer: {:#?}",
        fx.printed()
    );
}

/// One `anthropic` provider `a`; `ma` declares `model` (if any), the `default` agent runs `ma` and declares
/// `agent` (if any). `mb` on the same provider is a second model to switch through.
fn ceiling_config(agent: Option<u32>, model: Option<u32>) -> String {
    let key = |v: Option<u32>| v.map(|v| format!(", max_tokens: {v}")).unwrap_or_default();
    format!(
        "providers:\n  a: {{type: anthropic, key: x}}\nmodels:\n  ma: {{provider: a, id: claude-x{}}}\n  mb: {{provider: a, id: claude-y, max_tokens: 2222}}\nagents:\n  default: {{model: ma{}}}\n",
        key(model),
        key(agent)
    )
}

/// A bundle on `a` recording `max_tokens: value` with the five-key source map tagging it `source`.
fn ceiling_meta(value: u32, source: &str) -> iota::session::SessionMeta {
    serde_json::from_value(serde_json::json!({
        "v": 2, "provider": "a", "model": "claude-x", "max_tokens": value,
        "param_sources": {"context_window": "builtin", "effort": "builtin", "temperature": "builtin", "top_p": "builtin", "max_tokens": source},
    }))
    .expect("meta")
}

/// R1/R2: the cut's hint never credits a declaration with a ceiling the session is not running under. A
/// resumed session restores the value it recorded whatever today's config says, and a `config` tag does
/// not say which entry it came from — so the hint says only what the record says, and points the edit at
/// the entry of the LOADED config that can take effect: the agent's when it declares the key (it overrides
/// every model's — the model entry is then never offered), else the model's.
#[tokio::test]
async fn a_cut_never_credits_a_declaration_the_session_is_not_running_under() {
    const RECORDED: &str = "This session runs under a ceiling it recorded from the config.";
    const OWN: &str = "This session runs under a ceiling of its own, not one the config declares.";
    const EDIT_AGENT: &str =
        "To raise it, edit max_tokens: on the agent's `agents:` entry — it overrides every model's";
    const EDIT_MODEL: &str = "To raise it, set max_tokens: on this model's `models:` entry";
    // A value a move evaluated from the target's model entry, then resumed under a config whose agent
    // declares another ceiling since.
    let moved = {
        let mut m = iota::session::SessionMeta {
            provider: "a".to_owned(),
            model: "claude-x".to_owned(),
            ..iota::session::SessionMeta::default()
        };
        m.stamp(
            None,
            &resolve(&ceiling_config(None, Some(16_000)))
                .declared(None)
                .resume(&ceiling_meta(9_000, "config"), false),
        );
        assert_eq!(
            m.max_tokens,
            Some(16_000),
            "the move evaluated the target's entry"
        );
        m
    };
    let cases: [(&str, Fixture, &str, &str); 6] = [
        (
            "agent declaration added since",
            Fixture::new(vec![]).resumed(
                &ceiling_config(Some(64_000), Some(48_000)),
                &ceiling_meta(16_000, "config"),
                true,
            ),
            RECORDED,
            EDIT_AGENT,
        ),
        (
            "agent declaration removed since",
            Fixture::new(vec![]).resumed(
                &ceiling_config(None, Some(48_000)),
                &ceiling_meta(16_000, "config"),
                true,
            ),
            RECORDED,
            EDIT_MODEL,
        ),
        (
            "model declaration removed since",
            Fixture::new(vec![]).resumed(
                &ceiling_config(None, None),
                &ceiling_meta(16_000, "config"),
                true,
            ),
            RECORDED,
            EDIT_MODEL,
        ),
        (
            "both layers declare the same value",
            Fixture::new(vec![]).under(&ceiling_config(Some(16_000), Some(16_000))),
            RECORDED,
            EDIT_AGENT,
        ),
        (
            "a moved config value, resumed under a changed config",
            Fixture::new(vec![]).resumed(&ceiling_config(Some(64_000), None), &moved, true),
            RECORDED,
            EDIT_AGENT,
        ),
        (
            "the session's own value while the agent declares",
            Fixture::new(vec![]).resumed(
                &ceiling_config(Some(1234), Some(64_000)),
                &ceiling_meta(16_000, "user"),
                true,
            ),
            OWN,
            EDIT_AGENT,
        ),
    ];
    let squeeze = |s: &str| s.split_whitespace().collect::<String>();
    for (name, fx, runs, edit) in cases {
        assert_eq!(
            fx.params.max_tokens.value,
            Some(16_000),
            "{name}: the ceiling the session runs under"
        );
        let fx = Fixture {
            ui: ScriptedUi::new(vec![input("q"), Reply::Closed]),
            ..fx
        };
        fx.run(
            FakeProvider::new().round(Round::cut_off("", "half a reply")),
            None,
            &[],
        )
        .await;
        // The hint row wraps mid-word, so the block is compared with its whitespace taken out.
        let block = squeeze(&fx.printed().join(" "));
        assert!(
            block.contains(&squeeze(runs)),
            "{name}: {:#?}",
            fx.printed()
        );
        assert!(
            block.contains(&squeeze(edit)),
            "{name}: {:#?}",
            fx.printed()
        );
        for claim in [
            "This ceiling is max_tokens:",
            "which overrides the model's — raise it there",
        ] {
            assert!(
                !block.contains(&squeeze(claim)),
                "{name}: an entry credited with the value: {:#?}",
                fx.printed()
            );
        }
        if edit == EDIT_AGENT {
            assert!(
                !block.contains("`models:`"),
                "{name}: the model entry is overridden: {:#?}",
                fx.printed()
            );
        }
        assert!(
            block.contains(&squeeze(
                "quit, resume this session, then use /model to switch to another model and back"
            )),
            "{name}: how an edit reaches this session"
        );
    }
}

/// R2, the steps the hint gives, pinned. A RUNNING chat never re-reads the config file: a `/model` switch
/// away and back re-evaluates the snapshot it started with, so an edit on disk changes nothing.
#[tokio::test]
async fn a_config_edit_on_disk_never_reaches_a_running_chat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".iota.yaml");
    let yaml = ceiling_config(None, Some(1234));
    std::fs::write(&path, &yaml).expect("write config");
    // The closing turn materialises the bundle, so its meta can be read back.
    let script = vec![
        input("/model"),
        pick(1),
        input("/model"),
        pick(0),
        input("q"),
        Reply::Closed,
    ];
    let fx = Fixture::new(script).under(&std::fs::read_to_string(&path).expect("read config"));
    // Edited AFTER the run loaded it, as a user would while the chat is open.
    std::fs::write(&path, yaml.replace("max_tokens: 1234", "max_tokens: 64000"))
        .expect("edit config");
    let writer = fx
        .store
        .create(iota::session::NewSession::new(
            iota::provider::ProviderKind::Anthropic,
            "claude-x",
        ))
        .expect("writer");
    let id = writer.id().to_owned();
    let p = FakeProvider::new()
        .with_model("claude-x")
        .with_models(&["claude-x", "claude-y"])
        .with_max_tokens(Some(1234))
        .round(Round::reply("ok"));
    fx.run_on(Box::new(p), None, &[], Some(writer)).await;
    assert_eq!(
        fx.printed_count("Max tokens: 2222"),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(
        fx.printed_count("Max tokens: 1234"),
        1,
        "back on the snapshot's value"
    );
    assert_eq!(fx.printed_count("64000"), 0, "{:#?}", fx.printed());
    let saved = fx
        .store
        .load(&id, iota::provider::ProviderKind::Anthropic)
        .expect("load");
    assert_eq!(saved.meta.max_tokens, Some(1234));
}

/// R2, the other half: after a restart the edit IS loaded, yet a resume keeps the ceiling the session
/// recorded; a `/model` switch to another model and back is what adopts the new value.
#[tokio::test]
async fn after_a_restart_a_switch_away_and_back_adopts_the_edit() {
    let edited = ceiling_config(None, Some(64_000));
    // The closing turn materialises the bundle, so its meta can be read back.
    let script = vec![
        input("/model"),
        pick(1),
        input("/model"),
        pick(0),
        input("q"),
        Reply::Closed,
    ];
    let fx = Fixture::new(script).resumed(&edited, &ceiling_meta(1234, "config"), true);
    assert_eq!(
        fx.params.max_tokens.value,
        Some(1234),
        "a resume alone keeps the recorded ceiling"
    );
    let writer = fx
        .store
        .create(iota::session::NewSession::new(
            iota::provider::ProviderKind::Anthropic,
            "claude-x",
        ))
        .expect("writer");
    let id = writer.id().to_owned();
    let p = FakeProvider::new()
        .with_model("claude-x")
        .with_models(&["claude-x", "claude-y"])
        .with_max_tokens(Some(1234))
        .round(Round::reply("ok"));
    fx.run_on(Box::new(p), None, &[], Some(writer)).await;
    assert_eq!(
        fx.printed_count("Max tokens: 2222"),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(
        fx.printed_count("Max tokens: 64000"),
        1,
        "{:#?}",
        fx.printed()
    );
    let saved = fx
        .store
        .load(&id, iota::provider::ProviderKind::Anthropic)
        .expect("load");
    assert_eq!(saved.meta.max_tokens, Some(64_000));
}

/// The Model tab's answer: row `cursor` picked.
fn pick(cursor: usize) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![iota::ui::facade::PanelResult {
            cursor,
            ..iota::ui::facade::PanelResult::default()
        }],
    })
}

/// A cut that lands after the model started its REPLY keeps that text — it is what the model said —
/// marked cut short like a stall's partial, under the same error.
#[tokio::test]
async fn a_reply_cut_off_by_the_ceiling_keeps_its_text_marked_cut_short() {
    let log = Log::default();
    let provider = FakeProvider::new()
        .with_tools()
        .round(Round::cut_off("weigh it", "Cloudflare's DNS has no geo"))
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![input("geo dns?"), input("next"), Reply::Closed]);
    fx.run(provider, None, &[]).await;

    assert_eq!(log.calls(), 2);
    let second: Vec<_> = log.send(1).iter().map(shape).collect();
    let tail = &second[second.len() - 3..];
    assert_eq!(tail[0], (Role::User, "geo dns?".to_owned(), vec![]));
    assert_eq!(
        (tail[1].0, tail[1].1.as_str()),
        (Role::Assistant, "Cloudflare's DNS has no geo"),
        "the text that streamed before the cut is kept"
    );
    assert_eq!(tail[2], (Role::User, "next".to_owned(), vec![]));
    assert_eq!(
        fx.printed_count("Output truncated"),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(
        fx.printed_count("What arrived before the cut-off is kept"),
        1
    );
}

/// One Anthropic SSE event.
fn event(v: &serde_json::Value) -> String {
    format!(
        "event: {}\ndata: {v}\n\n",
        v["type"].as_str().expect("type")
    )
}

/// A complete Anthropic response: `lead` (the thinking block, in whichever of its three shapes) at
/// index 0, `text` at index 1, then the stop — `max_tokens` when `cut`.
fn anthropic_answer(lead: &[serde_json::Value], text: &str, cut: bool) -> String {
    use serde_json::json;
    let mut out = event(&json!({"type":"message_start","message":{"usage":{"input_tokens":23}}}));
    for e in lead {
        out.push_str(&event(e));
    }
    for e in [
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason": if cut {"max_tokens"} else {"end_turn"}},"usage":{"output_tokens":32000}}),
        json!({"type":"message_stop"}),
    ] {
        out.push_str(&event(&e));
    }
    out
}

/// M1 (X-69): text a TRUNCATED round wrote is kept together with the thinking block the endpoint
/// sealed — the response completed, so its raw blocks are whole. Through the real dialect, the REPL
/// and the session: the next request replays the block ahead of the kept text, and the persisted
/// record carries it. All three thinking shapes: a signed block, a signature-only block, a
/// `redacted_thinking` block.
#[tokio::test]
async fn a_reply_cut_off_by_the_ceiling_keeps_its_sealed_thinking() {
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};

    let shapes: [(&str, Vec<serde_json::Value>, serde_json::Value); 3] = [
        (
            "signed",
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"sealed thought"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-keep-me"}}),
            ],
            json!({"type":"thinking","thinking":"sealed thought","signature":"sig-keep-me"}),
        ),
        (
            "signature-only",
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SEAL"}}),
            ],
            json!({"type":"thinking","thinking":"","signature":"SEAL"}),
        ),
        (
            "redacted",
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"OPAQUE"}}),
            ],
            json!({"type":"redacted_thinking","data":"OPAQUE"}),
        ),
    ];
    for (name, lead, want) in shapes {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let first = anthropic_answer(&lead, "partial answer", true);
        let next = anthropic_answer(&lead, "next answer", false);
        Mock::given(method("POST"))
            .respond_with(move |_: &Request| {
                let body = if c.fetch_add(1, Ordering::SeqCst) == 0 {
                    first.clone()
                } else {
                    next.clone()
                };
                ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
            })
            .mount(&server)
            .await;
        let provider = iota::provider::anthropic::AnthropicProvider::new(
            "k",
            &server.uri(),
            "m",
            None,
            reqwest::Client::new(),
        );
        let fx = Fixture::new(vec![input("first"), input("next"), Reply::Closed]);
        let writer = fx
            .store
            .create(iota::session::NewSession::new(
                iota::provider::ProviderKind::Anthropic,
                "m",
            ))
            .expect("writer");
        let id = writer.id().to_owned();
        fx.run_on(Box::new(provider), None, &[], Some(writer)).await;

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2, "{name}");
        let body: serde_json::Value = serde_json::from_slice(&requests[1].body).expect("json");
        let kept = body["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap_or_else(|| panic!("{name}: no assistant replayed: {body}"));
        assert_eq!(
            kept["content"],
            json!([want, {"type":"text","text":"partial answer"}]),
            "{name}: the sealed block must lead the kept text on the next request"
        );
        assert_eq!(fx.printed_count("Output truncated"), 1, "{name}");

        // The session holds the same: the kept record carries the block and the round's cost.
        let loaded = fx
            .store
            .load(&id, iota::provider::ProviderKind::Anthropic)
            .expect("load");
        let kept = loaded
            .messages
            .iter()
            .find(|m| m.role() == Role::Assistant)
            .expect("kept record");
        assert_eq!(kept.content, "partial answer", "{name}");
        assert!(kept.interrupted(), "{name}: still marked cut short");
        assert!(
            kept.raw_content().is_some(),
            "{name}: the persisted record lost its raw blocks"
        );
        assert_eq!(
            kept.usage().map(|u| u.output),
            Some(32_000),
            "{name}: the round's cost rides the kept record"
        );
    }
}

/// R1's tail (X-69): a cut reported, then a usage-only `message_delta` (no `stop_reason`), then — when `stop`
/// — `message_stop`. `text` is the reply the model had started, if any.
fn cut_then_usage(thought: &str, text: Option<&str>, stop: bool) -> String {
    use serde_json::json;
    let mut events = vec![
        json!({"type":"message_start","message":{"usage":{"input_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":thought}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}),
        json!({"type":"content_block_stop","index":0}),
    ];
    if let Some(text) = text {
        events.extend([
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":text}}),
            json!({"type":"content_block_stop","index":1}),
        ]);
    }
    events.extend([
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4096}}),
        json!({"type":"message_delta","delta":{},"usage":{"input_tokens":23,"output_tokens":4096}}),
    ]);
    if stop {
        events.push(json!({"type":"message_stop"}));
    }
    events.iter().map(event).collect()
}

/// R1 through the REPL: a usage-only delta AFTER `max_tokens` (with or without `message_stop`) does not turn
/// the cut into a finished round. A half-thought is never the answer — nothing of it reaches the next
/// request as text, the turn rolls back, `Output truncated` is shown, no retry — and text the model did
/// write is kept only as a cut-short record (its sealed block ahead of it).
#[tokio::test]
async fn a_later_usage_delta_never_turns_a_cut_into_an_answer() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};

    for stop in [true, false] {
        for text in [None, Some("half an answer")] {
            let server = MockServer::start().await;
            let calls = Arc::new(AtomicUsize::new(0));
            let c = Arc::clone(&calls);
            let first = cut_then_usage("FORBIDDEN HALF THOUGHT", text, stop);
            let next = anthropic_answer(
                &[
                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
                ],
                "ok",
                false,
            );
            Mock::given(method("POST"))
                .respond_with(move |_: &Request| {
                    let body = if c.fetch_add(1, Ordering::SeqCst) == 0 {
                        first.clone()
                    } else {
                        next.clone()
                    };
                    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
                })
                .mount(&server)
                .await;
            let provider = iota::provider::anthropic::AnthropicProvider::new(
                "k",
                &server.uri(),
                "m",
                None,
                reqwest::Client::new(),
            );
            let fx = Fixture::new(vec![input("first"), input("next"), Reply::Closed]);
            fx.run_on(Box::new(provider), None, &[], None).await;

            let case = format!("stop={stop} text={text:?}");
            let requests = server.received_requests().await.expect("requests");
            assert_eq!(requests.len(), 2, "{case}: no retry");
            let wire = String::from_utf8_lossy(&requests[1].body).into_owned();
            assert!(
                !wire.contains(r#""text":"FORBIDDEN HALF THOUGHT""#),
                "{case}: the half-thought replayed as text: {wire}"
            );
            assert_eq!(fx.printed_count("Output truncated"), 1, "{case}");
            assert_eq!(fx.printed_count("FORBIDDEN HALF THOUGHT"), 0, "{case}");
            let body: serde_json::Value = serde_json::from_slice(&requests[1].body).expect("json");
            let assistants: Vec<_> = body["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .filter(|m| m["role"] == "assistant")
                .collect();
            match text {
                None => {
                    assert!(assistants.is_empty(), "{case}: rolled back: {body}");
                    assert!(!wire.contains(r#""text":"first""#), "{case}: {wire}");
                }
                Some(t) => {
                    assert_eq!(assistants.len(), 1, "{case}");
                    assert_eq!(assistants[0]["content"][0]["type"], "thinking", "{case}");
                    assert_eq!(assistants[0]["content"][1]["text"], t, "{case}");
                    assert_eq!(
                        fx.printed_count("What arrived before the cut-off is kept"),
                        1,
                        "{case}"
                    );
                }
            }
        }
    }
}

/// M5 (X-68): the title pass runs on its own instance, built from the endpoint alone — and must still carry
/// the ceiling the chat runs under, or an endpoint that only accepts a lower `max_tokens:` refuses every
/// title. Both instances are the real dialect; the chat's carries the configured 4096 (what startup tuning
/// installs), the title's is untuned (what `title_instance` builds), and both requests carry 4096.
#[tokio::test]
async fn the_title_request_carries_the_chats_ceiling() {
    use iota::provider::MaxTokensTunable;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let ok = anthropic_answer(
        &[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
        ],
        "Geo DNS on Cloudflare",
        false,
    );
    // The chat's answer is held back a second, so the title request — sent the moment the turn starts —
    // is on the server before the turn ends and the exit gives the title pass up.
    Mock::given(method("POST"))
        .respond_with(move |r: &wiremock::Request| {
            let title = String::from_utf8_lossy(&r.body).contains("short title");
            let t = ResponseTemplate::new(200).set_body_raw(ok.clone(), "text/event-stream");
            if title {
                t
            } else {
                t.set_delay(Duration::from_secs(1))
            }
        })
        .mount(&server)
        .await;
    let anthropic = || {
        iota::provider::anthropic::AnthropicProvider::new(
            "k",
            &server.uri(),
            "glm-5.3",
            None,
            reqwest::Client::new(),
        )
    };
    let mut chat = anthropic();
    chat.set_max_tokens(Some(4096));
    let title = anthropic();
    let fx = Fixture::new(vec![input("geo dns?"), Reply::Interrupted]);
    let params_title: Box<dyn iota::provider::Provider> = Box::new(title);
    fx.run_on_with_title(Box::new(chat), Some(params_title))
        .await;

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2, "the chat's request and the title's");
    for r in &requests {
        let body: serde_json::Value = serde_json::from_slice(&r.body).expect("json");
        assert_eq!(body["max_tokens"], 4096, "{body}");
    }
}

/// The answer arrives at once and only the title pass hangs: the next message goes out at once —
/// the loop does not wait on a name.
#[tokio::test]
async fn a_hung_title_pass_never_holds_the_next_message() {
    let sent: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let at = Arc::clone(&sent);
    let log = Log::default();
    let provider = FakeProvider::new()
        .with_tools()
        .replying("an answer")
        .on_call(move |_, _| at.lock().unwrap().push(Instant::now()))
        .with_log(log.clone());
    let title = FakeProvider::new().tail(Round::hanging());
    let fx = Fixture::new(vec![input("first"), input("second"), Reply::Closed]);
    fx.run(provider, Some(title), &[]).await;

    assert_eq!(log.prompts(), ["first", "second"]);
    let sent = sent.lock().unwrap();
    assert!(
        sent[1] - sent[0] < Duration::from_secs(5),
        "the second message waited {:?}",
        sent[1] - sent[0]
    );
}

/// The same hung title pass, then the exit: it comes at once, the placeholder name standing.
#[tokio::test]
async fn a_hung_title_pass_never_holds_the_exit() {
    let provider = FakeProvider::new().with_tools().replying("an answer");
    let title = FakeProvider::new().tail(Round::hanging());
    let fx = Fixture::new(vec![input("first"), Reply::Interrupted]);
    let started = Instant::now();
    fx.run(provider, Some(title), &[]).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the exit waited {:?}",
        started.elapsed()
    );
}

/// ESC on a first turn that already streamed text keeps the partial — and the title pass with it:
/// the model's name still lands (an unconditional abort left the placeholder for good).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_that_keeps_a_partial_keeps_the_title_pass() {
    let ui_slot: Arc<Mutex<Option<Arc<ScriptedUi>>>> = Arc::default();
    let seen = Arc::clone(&ui_slot);
    let landed = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&landed);
    // The second turn holds its call until the model's name is on the window — or gives up.
    let provider = FakeProvider::new()
        .with_tools()
        .round(Round::text("half an answer").interrupting(Interrupt::Call))
        .replying("ok")
        .on_call(move |n, _| {
            if n != 2 {
                return;
            }
            let ui = seen.lock().unwrap().clone().expect("the ui");
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let titled = ui
                    .events()
                    .iter()
                    .any(|e| matches!(e, UiEvent::Title(t) if t.contains("Model Name")));
                if titled {
                    *flag.lock().unwrap() = true;
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    // The name takes longer than the first turn, so the ESC lands while the pass is in flight.
    let title = FakeProvider::new()
        .replying("Model Name")
        .answering_after(Duration::from_millis(300));
    let fx = Fixture::new(vec![input("first"), input("second"), Reply::Closed]);
    *ui_slot.lock().unwrap() = Some(Arc::clone(&fx.ui));
    fx.run(provider, Some(title), &[]).await;

    assert_eq!(fx.printed_count("Interrupted."), 1);
    assert!(
        *landed.lock().unwrap(),
        "the model's title never landed after an ESC that kept the partial"
    );
}

/// A command right after the first answer, while its title pass is still out, does not give the
/// pass up: the model's name still lands (giving it up before every command left the placeholder
/// for good). What keeps a pass off the bundle `/session` swaps in is the title lock, not this.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_during_the_title_pass_keeps_it() {
    let ui_slot: Arc<Mutex<Option<Arc<ScriptedUi>>>> = Arc::default();
    let seen = Arc::clone(&ui_slot);
    let landed = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&landed);
    // The second turn holds its call until the model's name is on the window — or gives up.
    let provider = FakeProvider::new()
        .with_tools()
        .with_models(&["fake-model"])
        .replying("ok")
        .on_call(move |n, _| {
            if n != 2 {
                return;
            }
            let ui = seen.lock().unwrap().clone().expect("the ui");
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let titled = ui
                    .events()
                    .iter()
                    .any(|e| matches!(e, UiEvent::Title(t) if t.contains("Model Name")));
                if titled {
                    *flag.lock().unwrap() = true;
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    // The name takes longer than the first turn, so `/model` runs while the pass is in flight.
    let title = FakeProvider::new()
        .replying("Model Name")
        .answering_after(Duration::from_millis(300));
    let fx = Fixture::new(vec![
        input("first"),
        input("/model"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        input("second"),
        Reply::Closed,
    ]);
    *ui_slot.lock().unwrap() = Some(Arc::clone(&fx.ui));
    fx.run(provider, Some(title), &[]).await;

    assert!(
        *landed.lock().unwrap(),
        "the model's title never landed after a command ran during its pass"
    );
}

/// The responses dialect's cut (X-72) takes the anthropic one's path end to end: shown as `Output truncated` with
/// the hint for a ceiling iota never sent, NOT retried — with or without tools advertised, with or without text
/// before the cut — and the text that arrived before it is kept, marked cut short, never passed off as the
/// answer. Before X-72 a cut with nothing before it was a retryable failure (11 calls with tools, 121
/// without), and one after text was silently the answer.
#[tokio::test]
async fn a_responses_cut_is_shown_once_and_never_retried() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    const TEXT: &str =
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Cloudflare has no\"}\n\n";
    const CUT: &str = concat!(
        "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",",
        "\"incomplete_details\":{\"reason\":\"max_output_tokens\"},",
        "\"usage\":{\"input_tokens\":9,\"output_tokens\":512}}}\n\n",
    );
    for (tools, text) in [
        (&[][..], true),
        (&["read_file"][..], true),
        (&[][..], false),
        (&["read_file"][..], false),
    ] {
        let body = if text {
            format!("{TEXT}{CUT}")
        } else {
            CUT.to_owned()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let provider = iota::provider::openresponses::OpenResponsesProvider::new(
            "k",
            &server.uri(),
            "gpt-x",
            None,
            reqwest::Client::new(),
        );
        let fx = Fixture::new(vec![input("geo dns?"), Reply::Closed]);
        fx.run_on(Box::new(provider), None, tools, None).await;

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(
            requests.len(),
            1,
            "tools {tools:?}, text {text}: the cut is not retried"
        );
        assert_eq!(
            fx.printed_count("Output truncated"),
            1,
            "tools {tools:?}: {:#?}",
            fx.printed()
        );
        assert_eq!(
            fx.printed_count(
                "response.incomplete: max_output_tokens: the output hit its token limit"
            ),
            1
        );
        assert_eq!(
            fx.printed_count("The endpoint's own output limit cut it"),
            1
        );
        assert_eq!(fx.printed_count("max_tokens:"), 0, "no ceiling was sent");
        assert_eq!(fx.printed_count("retrying"), 0);
        assert_eq!(
            fx.printed_count("What arrived before the cut-off is kept"),
            usize::from(text),
            "tools {tools:?}, text {text}: {:#?}",
            fx.printed()
        );
    }
}

/// X-73: ONE retry budget per model call. A failure the table calls transient is retried `MAX_RETRIES` times
/// — 1 call + 10 retries — whether the tool loop advertised tools or ran with an empty set; the turn-level
/// replay no longer climbs a second ladder over the loop's (11 × 11 = 121 calls before).
#[tokio::test(start_paused = true)]
async fn a_transient_failure_climbs_one_ladder_with_or_without_tools() {
    for tools in [&[][..], &["noop"][..]] {
        let log = Log::default();
        let provider = FakeProvider::new()
            .with_tools()
            .tail(Round::failing("connection reset by peer"))
            .with_log(log.clone());
        let fx = Fixture::new(vec![input("q"), Reply::Closed]).patient();
        fx.run(provider, None, tools).await;

        assert_eq!(log.calls(), 11, "tools {tools:?}: 1 call + 10 retries");
        assert_eq!(
            fx.printed_count("connection reset by peer"),
            1,
            "{:#?}",
            fx.printed()
        );
    }
}

/// The other half of X-73: the ladder is still there — a transient failure that clears is retried until it
/// does, empty tool set included, and the turn answers.
#[tokio::test(start_paused = true)]
async fn a_transient_failure_is_still_retried_until_it_clears() {
    for tools in [&[][..], &["noop"][..]] {
        let log = Log::default();
        let provider = FakeProvider::new()
            .with_tools()
            .rounds([
                Round::failing("connection reset by peer"),
                Round::failing("connection reset by peer"),
            ])
            .replying("an answer")
            .with_log(log.clone());
        let fx = Fixture::new(vec![input("q"), Reply::Closed]).patient();
        fx.run(provider, None, tools).await;

        assert_eq!(log.calls(), 3, "tools {tools:?}");
        assert_eq!(
            fx.printed_count("⟳ Request failed — recovered after 2 attempt(s)"),
            1,
            "{:#?}",
            fx.printed()
        );
    }
}

/// A provider without tool calling takes the unary fallback, which no `retry_round` covers: there the
/// turn-level replay IS the ladder, and it still climbs exactly once (X-73).
#[tokio::test(start_paused = true)]
async fn the_unary_fallback_is_replayed_on_the_same_budget() {
    let log = Log::default();
    let provider = FakeProvider::new()
        .tail(Round::failing("connection reset by peer"))
        .with_log(log.clone());
    let fx = Fixture::new(vec![input("q"), Reply::Closed]).patient();
    fx.run(provider, None, &[]).await;

    assert_eq!(log.calls(), 11, "1 call + 10 replays");
}
