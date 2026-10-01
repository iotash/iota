//! A stalled stream and a hung title pass through the public entry point (`iota::repl::run`;
//! DIVERGENCES X-64, `docs/history/request-cancellation-review-*.md`).
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
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
        }
    }

    /// Runs the script to its end with `provider` answering the turns, `title` the title pass and
    /// `tools` advertised; panics when the run outlives [`PROMPT`].
    async fn run(&self, provider: FakeProvider, title: Option<FakeProvider>, tools: &[&str]) {
        let params = RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: title.map(|t| Box::new(t) as Box<dyn iota::provider::Provider>),
            system: String::new(),
            harness: String::new(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(tools)) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: McpHooks::default(),
            session: SessionCtx {
                writer: None,
                store: self.store.clone(),
                new_session: None,
                scope: None,
            },
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        };
        tokio::time::timeout(PROMPT, iota::repl::run(params))
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
