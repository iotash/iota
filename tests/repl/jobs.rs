//! Background jobs in the INTERACTIVE loop (`repl/run.rs`) over the scripted facade: how a completion
//! reaches the conversation, what it looks like when it does, and what happens to a job on the way out.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iota::BoxFuture;
use iota::provider::error::ProviderError;
use iota::provider::model::{Message, Role};
use iota::provider::{ChatResult, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::shell::exec::Options;
use iota::shell::jobs::Jobs;
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, InputKind, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// A provider with one canned reply and no tool capability (a notice needs no tools to land).
struct Replier;

impl Provider for Replier {
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
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(std::future::ready(Ok(ChatResult {
            text: "noted".to_owned(),
            ..ChatResult::default()
        })))
    }
}

/// The loop's parameters over a scripted facade, a temp store and `jobs`.
fn params(
    ui: &Arc<ScriptedUi>,
    store: &SessionStore,
    writer: Option<SessionWriter>,
    jobs: Arc<Jobs>,
) -> RunParams {
    RunParams {
        ui: Arc::clone(ui) as Arc<dyn Ui>,
        provider: Box::new(Replier),
        title_provider: None,
        system: String::new(),
        imported_history: Vec::new(),
        dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
        jobs,
        mcp: McpHooks {
            servers: None,
            events: None,
        },
        session: SessionCtx {
            writer,
            store: store.clone(),
            new_session: None,
            scope: None,
        },
        context_window: 0,
        agent: iota::chat::AgentOptions::default(),
        dark_background: true,
        root_cancel: CancellationToken::new(),
        reqlog: Arc::new(iota::llm::reqlog::RequestLog::new()),
        pres: Arc::new(iota::host::Presenter::with_hosts(Vec::new(), true)),
    }
}

/// Every line the loop committed to scrollback, SGR stripped.
fn printed(ui: &Arc<ScriptedUi>) -> Vec<String> {
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

fn opts(command: &str) -> Options {
    Options {
        command: command.to_owned(),
        dir: PathBuf::new(),
        timeout: None,
        sandbox: None,
    }
}

// New (phase C): a notice answering `read_input` is the IDLE WAKE-UP — the loop treats it as a message
// (one turn runs) but never as something the user typed (no `❯` block), and it persists flagged.
#[tokio::test]
async fn an_idle_notice_runs_a_turn_without_the_user_echo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let writer = store
        .create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
        .expect("writer");
    let id = writer.id().to_owned();
    let dir = writer.dir().to_path_buf();

    let headline = "[background job b1 finished: exit 0 after 42s] make test";
    let ui = ScriptedUi::new(vec![
        Reply::Input(Input {
            display: headline.to_owned(),
            text: format!("{headline}\nall green\n"),
            kind: InputKind::Notice,
        }),
        Reply::Interrupted,
    ]);
    let jobs = Jobs::new(tmp.path());
    iota::repl::run(params(&ui, &store, Some(writer), jobs))
        .await
        .expect("clean exit");

    // Not the user speaking: no reverse-video block, one dim headline instead.
    assert!(
        !ui.events()
            .iter()
            .any(|e| matches!(e, UiEvent::UserBlock(_))),
        "a notice must not echo as a typed message"
    );
    let lines = printed(&ui);
    assert!(
        lines.iter().any(|l| l.contains(headline)),
        "the headline never reached the transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("all green")),
        "the job's output belongs to the model, not the scrollback:\n{lines:#?}"
    );
    // The turn RAN: the model answered it.
    assert!(lines.iter().any(|l| l.contains("noted")), "{lines:#?}");

    // …and the log carries it as a notice, whole text and all.
    let sess = store.load(&id, ProviderKind::OpenAi).expect("load");
    let notice = sess
        .messages
        .iter()
        .find(|m| m.is_notice())
        .expect("no notice was persisted");
    assert_eq!(notice.role(), Role::User);
    assert!(
        notice.content.ends_with("all green"),
        "{:?}",
        notice.content
    );
    assert!(
        std::fs::read_to_string(dir.join("messages.jsonl"))
            .expect("log")
            .contains(r#""notice":true"#)
    );
}

// New (phase C): the loop installs the delivery sink on the registry at startup, so a completion — even
// one that landed before the loop was up — arrives as an `enqueue`d input in the notice shape.
#[tokio::test]
async fn the_loop_installs_the_sink_that_enqueues_a_completion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let jobs = Jobs::new(tmp.path());

    // A job that has already finished and parked: `set_sink` must hand it over.
    jobs.spawn(&opts("echo all green")).expect("spawn");
    for _ in 0..200 {
        if jobs.running() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ui = ScriptedUi::new(vec![Reply::Interrupted]);
    iota::repl::run(params(&ui, &store, None, Arc::clone(&jobs)))
        .await
        .expect("clean exit");

    let enqueued: Vec<Input> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Enqueue(i) => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(enqueued.len(), 1, "the completion was not enqueued");
    let input = &enqueued[0];
    assert_eq!(input.kind, InputKind::Notice);
    assert!(
        input
            .display
            .starts_with("[background job b1 finished: exit 0 after "),
        "{:?}",
        input.display
    );
    assert!(
        input.display.ends_with("] echo all green"),
        "{:?}",
        input.display
    );
    // The model gets the output; the echo does not.
    assert!(input.text.ends_with("all green\n"), "{:?}", input.text);
    assert!(!input.display.contains("all green\n"));
}

// New (phase C): `background` never promised to outlive iota — leaving the loop kills what is left, at once.
#[tokio::test]
async fn leaving_the_loop_kills_every_running_job() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let jobs = Jobs::new(tmp.path());
    let started = jobs.spawn(&opts("sleep 60")).expect("spawn");
    assert_eq!(jobs.running(), 1);

    let ui = ScriptedUi::new(vec![Reply::Interrupted]);
    iota::repl::run(params(&ui, &store, None, Arc::clone(&jobs)))
        .await
        .expect("clean exit");

    assert_eq!(jobs.running(), 0, "a job survived the loop");
    if let Some(pid) = started.pid {
        // Linux can be checked directly; macOS has no /proc, and the registry's own bookkeeping above is
        // the portable claim.
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists() || cfg!(target_os = "macos"),
            "pid {pid} survived the loop"
        );
    }
}
