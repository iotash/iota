//! `/new` (and its hidden alias `/clear`) through the PUBLIC entry point (`iota::repl::run`) —
//! docs/design/new-session-command.md §3.
//!
//! What is pinned: the old session stays on disk and resumes, the new one has its own id in the same
//! bucket, the five layered parameters and their sources carry over, the meter starts from zero, the
//! divider's exact text, the alias, the bot refusal, the argument refusal, the ephemeral confirmation's
//! two branches, and the second `/new` in a row doing nothing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::usage::Usage;
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, ParamSource, SessionMeta, SessionStore, SessionWriter};
use iota::testing::{FakeProvider, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelResult, StatusData, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

const USAGE: Usage = Usage {
    input: 1_200,
    output: 300,
    cache_read: 400,
    cache_write: 0,
    total: 1_500,
};

/// A provider that answers every send with `"an answer"`, reporting [`USAGE`].
fn reporting() -> FakeProvider {
    FakeProvider::new()
        .with_model("gpt-4o")
        .reporting_usage()
        .tail(Round::reply("an answer").usage(USAGE))
}

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

    fn writer(&self) -> SessionWriter {
        self.store
            .create(NewSession {
                provider: String::new(),
                ..NewSession::new(ProviderKind::OpenAi, "gpt-4o")
            })
            .expect("create writer")
    }

    fn session(&self, writer: Option<SessionWriter>) -> SessionCtx {
        SessionCtx {
            writer,
            store: self.store.clone(),
            new_session: None,
            scope: None,
            bot: false,
            notices: Vec::new(),
            recorded_notices: Vec::new(),
            memory: None,
        }
    }

    /// An ephemeral chat: no writer, a factory for `/save`.
    fn ephemeral(&self) -> SessionCtx {
        let store = self.store.clone();
        SessionCtx {
            new_session: Some(Box::new(move || {
                store.create(NewSession {
                    provider: String::new(),
                    ..NewSession::new(ProviderKind::OpenAi, "gpt-4o")
                })
            })),
            ..self.session(None)
        }
    }

    fn params(&self, provider: FakeProvider, session: SessionCtx) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(Path::new("")),
            mcp: McpHooks::default(),
            session,
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        }
    }
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
    })
}

/// The confirmation's answer: the first row (Discard) or the second (Keep).
fn pick(first: bool) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor: usize::from(!first),
            ..PanelResult::default()
        }],
    })
}

/// Every printed line, ANSI stripped, in order.
fn printed(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Print(lines) => Some(lines),
            _ => None,
        })
        .flatten()
        .map(|l| strip_sgr(&l))
        .collect()
}

/// The `New session …` dividers, in order.
fn dividers(ui: &ScriptedUi) -> Vec<String> {
    printed(ui)
        .into_iter()
        .filter(|l| l.starts_with("New session"))
        .collect()
}

/// The id a `New session <id>` divider names.
fn divider_id(line: &str) -> String {
    line.trim_start_matches("New session ")
        .split_whitespace()
        .next()
        .expect("an id")
        .to_owned()
}

/// The completion row the composer was LAST handed.
fn completion_row(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Commands(c) => Some(c),
            _ => None,
        })
        .next_back()
        .expect("the loop publishes the command table")
        .into_iter()
        .map(|s| s.value)
        .collect()
}

/// Every status row published, in order.
fn statuses(ui: &ScriptedUi) -> Vec<StatusData> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Status(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// The shapes of every blocking surface call, in order.
fn surfaces(ui: &ScriptedUi) -> Vec<iota::testing::TabbedSummary> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// `/new` is in the completion row, between `/file` and `/session`; `/clear` is not (it only matches).
#[tokio::test]
async fn new_is_listed_and_clear_is_not() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let session = f.session(Some(f.writer()));
    iota::repl::run(f.params(reporting(), session))
        .await
        .expect("exit");
    let row = completion_row(&f.ui);
    assert!(row.contains(&"/new".to_owned()), "{row:?}");
    assert!(!row.contains(&"/clear".to_owned()), "{row:?}");
}

/// §3.2: the old session is persisted, its lock let go, and it resumes as it was — WHILE the run that
/// left it is still up; the new session has another id, in the same bucket, and is what the next
/// message lands in.
#[tokio::test]
async fn new_leaves_the_old_session_on_disk_and_starts_another_in_the_same_bucket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    std::fs::create_dir_all(&root).expect("project root");
    let resumed_old: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let store = SessionStore::new(tmp.path().join("sessions"));
    let writer = store
        .create(NewSession {
            provider: String::new(),
            cwd: root.to_string_lossy().into_owned(),
            project: true,
            ..NewSession::new(ProviderKind::OpenAi, "gpt-4o")
        })
        .expect("create");
    let old_id = writer.id().to_owned();
    let old_dir = writer.dir().to_path_buf();
    let probe = {
        let store = store.clone();
        let id = old_id.clone();
        let seen = Arc::clone(&resumed_old);
        Arc::new(move || {
            // `iota resume <old id>` from another process would take the bundle lock: the run must have
            // let it go.
            let (_w, s) = store
                .resume(&id, ProviderKind::OpenAi)
                .expect("the old session resumes while the run is still up");
            *seen.lock().unwrap() = Some(s.messages.len());
        })
    };
    let ui = ScriptedUi::new(vec![
        input("first question"),
        input("/new"),
        Reply::Pause(probe),
        input("second question"),
        Reply::Interrupted,
    ]);
    let f = Fixture {
        ui,
        _tmp: tempfile::tempdir().expect("tempdir"),
        store: store.clone(),
    };
    let mut params = f.params(
        reporting(),
        SessionCtx {
            scope: Some(root.clone()),
            ..f.session(Some(writer))
        },
    );
    params.agent = iota::headless::AgentOptions {
        enabled: true,
        root: root.clone(),
        cwd: Some(root.clone()),
        home: None,
    };
    iota::repl::run(params).await.expect("exit");

    assert_eq!(
        *resumed_old.lock().unwrap(),
        Some(2),
        "the old session came back with its question and answer"
    );
    let dividers = dividers(&f.ui);
    assert_eq!(dividers.len(), 1, "{:?}", printed(&f.ui));
    let new_id = divider_id(&dividers[0]);
    assert_ne!(new_id, old_id);
    let new_dir = store.find_dir(&new_id).expect("the new bundle is on disk");
    assert_eq!(
        new_dir.parent(),
        old_dir.parent(),
        "the same bucket: {new_dir:?} vs {old_dir:?}"
    );
    let old_log = std::fs::read_to_string(old_dir.join("messages.jsonl")).expect("old log");
    assert!(old_log.contains("first question"));
    assert!(!old_log.contains("second question"));
    let new_log = std::fs::read_to_string(new_dir.join("messages.jsonl")).expect("new log");
    assert!(new_log.contains("second question"));
    assert!(!new_log.contains("first question"));
    // And `iota resume <old id>` after the run: the same.
    let (_w, s) = store
        .resume(&old_id, ProviderKind::OpenAi)
        .expect("old resumes after exit");
    assert_eq!(s.messages.len(), 2);
}

/// §3.2 (and §3.7 #5): the new bundle records what the chat runs under NOW — every layered parameter and
/// the source it came from, a hand-set one included — not a re-evaluation.
#[tokio::test]
async fn new_carries_the_parameters_and_their_sources() {
    let f = Fixture::new(vec![
        input("first question"),
        input("/new"),
        input("second question"),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let provider = reporting()
        .tunable()
        .with_temperature(Some(0.3))
        .with_effort(Some(iota::provider::Effort::High))
        .with_max_tokens(Some(4_096));
    let mut params = f.params(provider, session);
    params.params = iota::session::LayeredParams {
        context_window: iota::session::Param::config(400_000),
        effort: iota::session::Param::config("high".to_owned()),
        // Hand-set (a `/model` tab writes `User`).
        temperature: iota::session::Param::user(Some(0.3)),
        top_p: iota::session::Param::default(),
        max_tokens: iota::session::Param::user(Some(4_096)),
    };
    iota::repl::run(params).await.expect("exit");

    let new_id = divider_id(&dividers(&f.ui)[0]);
    let meta = SessionMeta::read(&f.store.find_dir(&new_id).expect("new bundle")).expect("meta");
    assert_eq!(meta.context_window, 400_000);
    assert_eq!(meta.effort, "high");
    assert_eq!(meta.temperature, Some(0.3));
    assert_eq!(meta.max_tokens, Some(4_096));
    assert_eq!(meta.model, "gpt-4o");
    let src = meta.sources();
    assert_eq!(src.context_window, ParamSource::Config);
    assert_eq!(src.effort, ParamSource::Config);
    assert_eq!(src.temperature, ParamSource::User);
    assert_eq!(src.top_p, ParamSource::Builtin);
    assert_eq!(src.max_tokens, Some(ParamSource::User));
    assert!(meta.records_params(), "{meta:?}");
}

/// §3.2: the meter starts from zero — the ↑/↓ totals and the occupancy — and the title starts over.
#[tokio::test]
async fn new_zeroes_the_meter() {
    let f = Fixture::new(vec![
        input("first question"),
        input("/new"),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let mut params = f.params(reporting(), session);
    params.params.context_window = iota::session::Param::config(100_000);
    iota::repl::run(params).await.expect("exit");

    let all = statuses(&f.ui);
    assert!(
        all.iter().any(|s| s.in_tokens == 1_200),
        "the turn was counted: {all:?}"
    );
    let last = all.last().expect("a status row");
    assert_eq!((last.in_tokens, last.out_tokens), (0, 0), "{last:?}");
    assert_eq!(last.ctx_used, 0, "{last:?}");
    assert_eq!(last.ctx_window, 100_000);
    // The window title went back to the application name.
    let last_title =
        f.ui.events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Title(t) => Some(t),
                _ => None,
            })
            .next_back()
            .expect("a title");
    assert_eq!(last_title, "iota", "the new session is unnamed");
}

/// §3.4: the divider's exact text — a saved chat names the new id, and attachments left behind are
/// counted; the ephemeral form is pinned in `an_ephemeral_chat_that_replied_asks_first`.
#[tokio::test]
async fn the_divider_says_what_started() {
    let f = Fixture::new(vec![input("q"), input("/new"), Reply::Interrupted]);
    iota::repl::run(f.params(reporting(), f.session(Some(f.writer()))))
        .await
        .expect("exit");
    let d = dividers(&f.ui);
    assert_eq!(d.len(), 1);
    let id = divider_id(&d[0]);
    assert_eq!(id.len(), 12, "a session id: {id}");
    assert_eq!(d[0], format!("New session {id}"));

    // An attachment waiting for the next message does not ride into the new session.
    let file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("file");
    std::fs::write(file.path(), "hello").expect("write");
    let f = Fixture::new(vec![
        input("q"),
        input(&format!("/file {}", file.path().display())),
        input("/new"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params(reporting(), f.session(Some(f.writer()))))
        .await
        .expect("exit");
    let d = dividers(&f.ui);
    assert_eq!(d.len(), 1, "{:?}", printed(&f.ui));
    let id = divider_id(&d[0]);
    assert_eq!(
        d[0],
        format!("New session {id} (1 attachment dropped)"),
        "{:?}",
        printed(&f.ui)
    );
}

/// §3.5: an ephemeral chat a model has replied in is not thrown away unasked — Discard starts the new
/// one, Keep changes nothing.
#[tokio::test]
async fn an_ephemeral_chat_that_replied_asks_first() {
    for discard in [true, false] {
        let f = Fixture::new(vec![
            input("q"),
            input("/new"),
            pick(discard),
            Reply::Interrupted,
        ]);
        let provider = reporting();
        let log = provider.log();
        iota::repl::run(f.params(provider, f.ephemeral()))
            .await
            .expect("exit");
        let asked = surfaces(&f.ui);
        assert_eq!(asked.len(), 1, "one confirmation");
        assert_eq!(
            asked[0].panels[0].title,
            "Discard this unsaved chat and start a new one?"
        );
        assert_eq!(asked[0].panels[0].items, ["Discard", "Keep"]);
        let d = dividers(&f.ui);
        if discard {
            assert_eq!(d, ["New session · not saved · /save keeps it"]);
        } else {
            assert!(d.is_empty(), "{d:?}");
        }
        assert_eq!(log.calls(), 1);
        // Nothing reached the disk either way: the chat was and stays ephemeral.
        assert!(f.store.list(None).expect("list").is_empty());
    }
}

/// §3.5: an ephemeral chat with no reply starts over without asking. (A recorded notice is a message no
/// model has answered — the shape of a chat whose only content is what the loop told it.)
#[tokio::test]
async fn an_ephemeral_chat_with_no_reply_starts_over_unasked() {
    let f = Fixture::new(vec![input("/new"), Reply::Interrupted]);
    let mut session = f.ephemeral();
    session.recorded_notices = vec!["something the model was told".to_owned()];
    iota::repl::run(f.params(reporting(), session))
        .await
        .expect("exit");
    assert!(surfaces(&f.ui).is_empty(), "nothing asked");
    assert_eq!(
        dividers(&f.ui),
        ["New session · not saved · /save keeps it"]
    );
}

/// §3.7 #2: `/clear` is `/new` under another name — same divider, same swap.
#[tokio::test]
async fn clear_is_new() {
    let f = Fixture::new(vec![input("q"), input("/clear"), Reply::Interrupted]);
    let writer = f.writer();
    let old = writer.id().to_owned();
    let provider = reporting();
    let log = provider.log();
    iota::repl::run(f.params(provider, f.session(Some(writer))))
        .await
        .expect("exit");
    let d = dividers(&f.ui);
    assert_eq!(d.len(), 1, "{:?}", printed(&f.ui));
    assert_ne!(divider_id(&d[0]), old);
    assert_eq!(log.calls(), 1, "`/clear` never reached the model");
}

/// §3.3 / §3.7 #10: a bot's session never ends — `/new` (and `/clear`) are refused with the reason, not
/// sent to the model, and not offered.
#[tokio::test]
async fn a_bot_refuses_new() {
    let f = Fixture::new(vec![input("/new"), input("/clear"), Reply::Interrupted]);
    let bots = f.store.root().parent().expect("parent").join("bots");
    let store = f.store.clone().with_bots(&bots);
    let writer = match store
        .open_bot(
            &bots.join("coder"),
            NewSession::new(ProviderKind::OpenAi, "gpt-4o"),
            ProviderKind::OpenAi,
        )
        .expect("open the bot")
    {
        iota::session::BotOpen::Fresh(w) | iota::session::BotOpen::Resumed(w, _) => w,
    };
    let id = writer.id().to_owned();
    let provider = reporting();
    let log = provider.log();
    let session = SessionCtx {
        writer: Some(writer),
        store,
        bot: true,
        ..f.session(None)
    };
    iota::repl::run(f.params(provider, session))
        .await
        .expect("exit");
    let refusal = "A bot's session never ends: to start over, run another agent or rename the bot.";
    let lines = printed(&f.ui);
    assert_eq!(
        lines.iter().filter(|l| *l == refusal).count(),
        2,
        "{lines:?}"
    );
    assert!(dividers(&f.ui).is_empty());
    assert_eq!(log.calls(), 0, "nothing was sent to the model");
    assert!(!completion_row(&f.ui).contains(&"/new".to_owned()));
    let ptr = iota::session::BotPointer::read(&bots.join("coder"))
        .expect("pointer")
        .expect("present");
    assert_eq!(ptr.session, id, "the bot still points at its one session");
}

/// §3.7 #9: v1 takes no argument.
#[tokio::test]
async fn new_takes_no_argument() {
    let f = Fixture::new(vec![
        input("q"),
        input("/new my next thing"),
        input("/clear x"),
        Reply::Interrupted,
    ]);
    let provider = reporting();
    let log = provider.log();
    iota::repl::run(f.params(provider, f.session(Some(f.writer()))))
        .await
        .expect("exit");
    let lines = printed(&f.ui);
    assert_eq!(
        lines.iter().filter(|l| *l == "Usage: /new").count(),
        2,
        "{lines:?}"
    );
    assert!(dividers(&f.ui).is_empty());
    assert_eq!(log.calls(), 1, "neither was sent to the model");
}

/// §3.5: a second `/new` on a session nobody has said anything in yet is a no-op — no id is minted —
/// and so is `/new` in a chat that just started.
#[tokio::test]
async fn new_twice_is_a_no_op() {
    let f = Fixture::new(vec![
        input("/new"),
        input("q"),
        input("/new"),
        input("/new"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params(reporting(), f.session(Some(f.writer()))))
        .await
        .expect("exit");
    let lines = printed(&f.ui);
    assert_eq!(dividers(&f.ui).len(), 1, "{lines:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|l| *l == "Already a new session.")
            .count(),
        2,
        "{lines:?}"
    );
    // One bundle on disk: the first session's (the new one never got a message).
    assert_eq!(f.store.list(None).expect("list").len(), 1);
}

/// The system prompt is carried into the new session (§3.2: `[system]` seeds it), and the new session's
/// first send carries none of the old conversation.
#[tokio::test]
async fn new_keeps_the_system_prompt_and_nothing_else() {
    let f = Fixture::new(vec![
        input("first question"),
        input("/new"),
        input("second question"),
        Reply::Interrupted,
    ]);
    let provider = reporting();
    let log = provider.log();
    let mut params = f.params(provider, f.session(Some(f.writer())));
    params.system = "be brief".to_owned();
    iota::repl::run(params).await.expect("exit");
    let second = log.send(1);
    let texts: Vec<&str> = second.iter().map(|m| m.content.as_str()).collect();
    assert!(
        !texts.iter().any(|t| t.contains("first question")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("second question")));
    let new_id = divider_id(&dividers(&f.ui)[0]);
    let dir: PathBuf = f.store.find_dir(&new_id).expect("new bundle");
    let log = std::fs::read_to_string(dir.join("messages.jsonl")).expect("log");
    assert!(
        log.contains("be brief"),
        "the new bundle starts with the system prompt"
    );
}
