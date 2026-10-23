//! The notice gate end to end (DIVERGENCES X-84, docs/design/rate-limit-and-interrupt.md §7.2): a 429 the
//! ladder could not clear, or an interrupt, holds the notices of finished background jobs instead of letting
//! each open a turn; a typed message carries them, a rate-limit hold releases them on its own as one probe,
//! and `/model` probes at once. Driven through `iota::repl::run` over the scripted facade, under tokio's
//! paused clock: the ladder's backoff and the probe's minutes pass in virtual time.

use std::sync::Arc;
use std::time::Duration;

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::testing::{
    FakeProvider, Interrupt, Log, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent,
};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, InputKind, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// GLM's refusal at its 5-hour cap, as the error detail carries it: no `retry-after`, a generic type.
const QUOTA: &str = r#"{"type":"rate_limit_error","code":"1308","message":"[1308][已达到 5 小时的使用上限。您的限额将在 2026-10-23 20:22:40 重置。]"}"#;

/// The ladder a first 429 climbs: 1 call + 3 retries.
const LADDER: usize = 4;

struct Fixture {
    ui: Arc<ScriptedUi>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
    /// The config the run resolves its `/model` choices from (`None`: none).
    yaml: Option<String>,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
            yaml: None,
        }
    }

    /// A run under the config `yaml` (its `default` agent).
    fn under(mut self, yaml: &str) -> Self {
        self.yaml = Some(yaml.to_owned());
        self
    }

    /// A writer the turns persist through.
    fn writer(&self) -> SessionWriter {
        self.store
            .create(NewSession::new(ProviderKind::Anthropic, "claude-x"))
            .expect("writer")
    }

    /// Runs the script to its end on `provider`; panics when the loop waits past a virtual hour.
    async fn run(&self, provider: FakeProvider, writer: Option<SessionWriter>) {
        let (params, layers) = match &self.yaml {
            Some(yaml) => {
                let cfg = iota::cmd::Config::parse(
                    yaml.as_bytes(),
                    &iota::app::env::Env::default(),
                    &mut |w| panic!("unexpected config warning: {w}"),
                )
                .expect("the config loads");
                let resolved = cfg.resolve_agent("default").expect("the agent resolves");
                (
                    resolved
                        .declared(None)
                        .evaluate(&iota::session::LayeredParams::default()),
                    iota::cmd::ParamLayers::new(&cfg, &resolved),
                )
            }
            None => Default::default(),
        };
        let run = RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
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
            params,
            layers,
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        };
        tokio::time::timeout(Duration::from_hours(3), iota::repl::run(run))
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

    fn count(&self, needle: &str) -> usize {
        self.printed().iter().filter(|l| l.contains(needle)).count()
    }
}

/// Job `n`'s headline.
fn headline(n: u32) -> String {
    format!("[background job b{n} finished: exit 0 after 1s] make")
}

/// Job `n`'s notice, as the job sink enqueues it.
fn notice(n: u32) -> Reply {
    Reply::Input(Input {
        display: headline(n),
        text: format!("{}\nout {n}", headline(n)),
        kind: InputKind::Notice,
    })
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
    })
}

/// The 429 rounds of a first refusal's whole ladder.
fn refused() -> Vec<Round> {
    vec![Round::rate_limited(QUOTA, None); LADDER]
}

/// What a sent history says, past the composed system segments: `n:<job>` for a notice, the text of the rest.
fn said(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .filter(|m| m.is_notice() || m.role() == iota::provider::model::Role::User)
        .map(|m| {
            if m.is_notice() {
                let job = m.content.rsplit(' ').next().unwrap_or_default();
                format!("n:{job}")
            } else {
                m.content.clone()
            }
        })
        .collect()
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

/// The symptom: job b1's notice meets the cap and climbs the ladder once; b2 and b3, finishing behind it,
/// open no turn and send nothing — one error block, one dim line each.
#[tokio::test(start_paused = true)]
async fn a_rate_limited_notice_holds_the_ones_behind_it() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .rounds(refused())
        .with_log(log.clone());
    let fx = Fixture::new(vec![notice(1), notice(2), notice(3), Reply::Closed]);
    fx.run(p, None).await;

    assert_eq!(log.calls(), LADDER, "the notices behind it sent nothing");
    assert_eq!(fx.count("✗ Rate limited (429)"), 1, "{:#?}", fx.printed());
    // The provider's own sentence, reset time and all, and what iota does next.
    assert_eq!(fx.count("您的限额将在 2026-10-23 20:22:40 重置"), 1);
    assert_eq!(
        fx.count("Background-job notices wait until "),
        1,
        "{:#?}",
        fx.printed()
    );
    for n in [2, 3] {
        assert_eq!(
            fx.count(&format!("⏸ {} — held until ", headline(n))),
            1,
            "{:#?}",
            fx.printed()
        );
    }
}

/// A minute after the refusal the gate lets every held notice out as ONE turn, in the order the jobs ended,
/// with no ladder; it answers, the hold is gone, and the next notice opens its own turn at once.
#[tokio::test(start_paused = true)]
async fn the_probe_releases_every_held_notice_in_one_turn() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .rounds(refused())
        .replying("noted")
        .with_log(log.clone());
    let fx = Fixture::new(vec![
        notice(1),
        notice(2),
        notice(3),
        Reply::Idle,
        notice(4),
        Reply::Closed,
    ]);
    let t0 = tokio::time::Instant::now();
    fx.run(p, None).await;

    assert_eq!(log.calls(), LADDER + 2, "{:?}", log.prompts());
    assert_eq!(said(&log.send(LADDER)), ["n:1", "n:2", "n:3"]);
    assert_eq!(said(&log.send(LADDER + 1)), ["n:1", "n:2", "n:3", "n:4"]);
    assert_eq!(fx.count("▶ Trying again with 3 held notice(s)"), 1);
    assert_eq!(fx.count("held until"), 2, "b4 was not held");
    // The ladder's 2 + 4 + 8 s, then the minute.
    assert_eq!(t0.elapsed(), Duration::from_secs(14 + 60));
}

/// A probe refused again leaves one dim line — no second block — and the next waits twice as long.
#[tokio::test(start_paused = true)]
async fn a_failed_probe_backs_off_and_prints_one_line() {
    let log = Log::default();
    let mut rounds = refused();
    rounds.push(Round::rate_limited(QUOTA, None));
    let p = FakeProvider::new()
        .with_tools()
        .rounds(rounds)
        .replying("noted")
        .with_log(log.clone());
    let fx = Fixture::new(vec![
        notice(1),
        notice(2),
        Reply::Idle,
        Reply::Idle,
        Reply::Closed,
    ]);
    let t0 = tokio::time::Instant::now();
    fx.run(p, None).await;

    assert_eq!(log.calls(), LADDER + 2, "a probe gets no ladder");
    assert_eq!(fx.count("✗ Rate limited (429)"), 1, "{:#?}", fx.printed());
    assert_eq!(
        fx.count("⏸ Still rate limited (429) — 2 notice(s) held; next try "),
        1,
        "{:#?}",
        fx.printed()
    );
    assert_eq!(said(&log.send(LADDER + 1)), ["n:1", "n:2"]);
    assert_eq!(t0.elapsed(), Duration::from_secs(14 + 60 + 120));
}

/// A typed message under the hold goes at once, carrying the held notices ahead of it — on screen too: their
/// headlines, then the user's block.
#[tokio::test(start_paused = true)]
async fn a_typed_message_carries_the_held_notices_ahead_of_it() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .rounds(refused())
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![notice(1), notice(2), input("go"), Reply::Closed]);
    fx.run(p, None).await;

    assert_eq!(log.calls(), LADDER + 1);
    assert_eq!(said(&log.send(LADDER)), ["n:1", "n:2", "go"]);
    let events = fx.ui.events();
    let at = |pred: &dyn Fn(&UiEvent) -> bool| events.iter().rposition(pred).expect("event");
    let b2 =
        at(&|e| matches!(e, UiEvent::Print(l) if l.iter().any(|l| strip_sgr(l) == headline(2))));
    let go = at(&|e| matches!(e, UiEvent::UserBlock(d) if d == "go"));
    assert!(b2 < go, "the carried headlines come first");
}

/// `/model` onto another model ends the wait: the held notices go at once, to the new model.
#[tokio::test(start_paused = true)]
async fn a_model_switch_probes_at_once() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .with_model("claude-x")
        .with_models(&["claude-x", "claude-y"])
        .rounds(refused())
        .replying("ok")
        .with_log(log.clone());
    let yaml = "providers:\n  a: {type: anthropic, key: x}\nmodels:\n  ma: {provider: a, id: claude-x}\n  mb: {provider: a, id: claude-y}\nagents:\n  default: {model: ma}\n";
    let fx = Fixture::new(vec![
        notice(1),
        notice(2),
        input("/model"),
        pick(1),
        Reply::Closed,
    ])
    .under(yaml);
    let t0 = tokio::time::Instant::now();
    fx.run(p, None).await;

    let records = log.records();
    assert_eq!(records.len(), LADDER + 1, "{:?}", log.prompts());
    assert_eq!(records[LADDER].model, "claude-y");
    assert_eq!(said(&records[LADDER].messages), ["n:1", "n:2"]);
    assert_eq!(fx.count("▶ Trying again with 2 held notice(s)"), 1);
    assert_eq!(
        t0.elapsed(),
        Duration::from_secs(14),
        "no wait for the minute"
    );
}

/// ESC mid-stream: the notices queued behind the turn are put aside, not answered, until the user speaks.
#[tokio::test(start_paused = true)]
async fn esc_holds_the_notices_queued_behind_the_turn() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .round(Round::text("half an answer").interrupting(Interrupt::Call))
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![
        notice(1),
        notice(2),
        notice(3),
        input("go"),
        Reply::Closed,
    ]);
    fx.run(p, None).await;

    assert_eq!(log.calls(), 2, "{:?}", log.prompts());
    assert_eq!(fx.count("Interrupted."), 1);
    for n in [2, 3] {
        assert_eq!(
            fx.count(&format!(
                "⏸ {} — held; it goes with your next message",
                headline(n)
            )),
            1,
            "{:#?}",
            fx.printed()
        );
    }
    // b1 stays with the partial the interrupt kept; b2 and b3 ride ahead of the message.
    assert_eq!(said(&log.send(1)), ["n:1", "n:2", "n:3", "go"]);
}

/// A notice turn the interrupt discards whole gives its notice back: it goes with the next message, and
/// the session records it once.
#[tokio::test(start_paused = true)]
async fn an_interrupted_notice_turn_keeps_its_notice() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .round(Round::failing("dropped").interrupting(Interrupt::Call))
        .replying("ok")
        .with_log(log.clone());
    let fx = Fixture::new(vec![notice(1), input("go"), Reply::Closed]);
    let writer = fx.writer();
    let id = writer.id().to_owned();
    fx.run(p, Some(writer)).await;

    assert_eq!(said(&log.send(1)), ["n:1", "go"]);
    let saved = fx.store.load(&id, ProviderKind::Anthropic).expect("load");
    assert_eq!(said(&saved.messages), ["n:1", "go"]);
}

/// A notice turn refused with 429 is rolled back — and its notice is not lost with it: the probe carries it,
/// and the session records it once.
#[tokio::test(start_paused = true)]
async fn a_failed_rate_limited_turn_never_loses_its_notice() {
    let log = Log::default();
    let p = FakeProvider::new()
        .with_tools()
        .rounds(refused())
        .replying("noted")
        .with_log(log.clone());
    let fx = Fixture::new(vec![notice(1), Reply::Idle, Reply::Closed]);
    let writer = fx.writer();
    let id = writer.id().to_owned();
    fx.run(p, Some(writer)).await;

    assert_eq!(log.calls(), LADDER + 1);
    assert_eq!(said(&log.send(LADDER)), ["n:1"]);
    let saved = fx.store.load(&id, ProviderKind::Anthropic).expect("load");
    assert_eq!(said(&saved.messages), ["n:1"]);
}
