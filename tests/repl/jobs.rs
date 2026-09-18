//! Background jobs in the INTERACTIVE loop (`repl/run.rs`) over the scripted facade: how a completion
//! reaches the conversation, what it looks like when it does, and what happens to a job on the way out.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iota::provider::ProviderKind;
use iota::provider::model::Role;
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::shell::exec::Options;
use iota::shell::jobs::Jobs;
use iota::testing::{FakeProvider, Reply, ScriptedUi, StaticDispatcher, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, InputKind, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// The loop's parameters over a scripted facade, a temp store and `jobs`.
fn params(
    ui: &Arc<ScriptedUi>,
    store: &SessionStore,
    writer: Option<SessionWriter>,
    jobs: Arc<Jobs>,
) -> RunParams {
    RunParams {
        ui: Arc::clone(ui) as Arc<dyn Ui>,
        // One canned reply and no tool capability: a notice needs no tools to land.
        provider: Box::new(FakeProvider::new().replying("noted")),
        title_provider: None,
        system: String::new(),
        imported_history: Vec::new(),
        dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
        jobs,
        mcp: McpHooks::default(),
        session: SessionCtx {
            writer,
            store: store.clone(),
            new_session: None,
            scope: None,
        },
        params: iota::session::LayeredParams::default(),
        layers: iota::cmd::ParamLayers::default(),
        catalog: iota::repl::ModelCatalog::default(),
        agent: iota::headless::AgentOptions::default(),
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

/// Whether to skip a test whose command line is POSIX. The interpreter is `bash` on Unix and, on Windows,
/// whatever `shell::interp`'s ladder found — under PowerShell or `cmd.exe` these scripts would not parse, so
/// the test prints a `SKIP:` line instead (the twin of `tests/tool/shell.rs::skip_unless_posix`).
fn skip_unless_posix(test: &str) -> bool {
    let shell =
        iota::shell::interp::resolve().expect("this machine has no shell interpreter at all");
    if shell.is_posix() {
        return false;
    }
    println!(
        "SKIP: {test} — the resolved interpreter is {}, not a POSIX shell",
        shell.program.display()
    );
    true
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
        .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
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
    if skip_unless_posix("leaving_the_loop_kills_every_running_job") {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let jobs = Jobs::new(tmp.path());
    let started = jobs.spawn(&opts("sleep 60")).expect("spawn");
    assert_eq!(jobs.running(), 1);
    // Windows has no process group to kill — `exec::kill_group` is a no-op there and the registry's own
    // bookkeeping is the whole claim — so the pid checks below are the Unix half of this test. The first
    // one is here so the second means something: `alive` has to be able to say "yes" too.
    let pid = if cfg!(unix) { started.pid } else { None };
    if let Some(pid) = pid {
        assert!(alive(pid), "pid {pid} never started");
    }

    let ui = ScriptedUi::new(vec![Reply::Interrupted]);
    iota::repl::run(params(&ui, &store, None, Arc::clone(&jobs)))
        .await
        .expect("clean exit");

    assert_eq!(jobs.running(), 0, "a job survived the loop");
    if let Some(pid) = pid {
        // `kill_all` is synchronous `killpg(SIGKILL)`, so the group is dead by the time the loop returns —
        // but REAPING is the supervisor task's job, and that task may not have been polled yet. Wait for
        // the state to settle rather than asserting on the first observation.
        for _ in 0..100 {
            if !alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "pid {pid} survived the loop");
    }
}

/// Whether `pid` is still a LIVE process. Not `/proc/<pid>`, and not `kill(pid, 0)`: a killed child stays a
/// zombie until its parent reaps it, and a zombie keeps its pid, its `/proc` entry and its answer to signal
/// 0 — so both of those report "alive" for a process that is already dead. The process STATE is the thing
/// being asked about, and `ps -o stat=` is the one spelling of it macOS and Linux share (empty output: gone;
/// `Z`: dead, not yet reaped).
fn alive(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}
