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
        harness: String::new(),
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
pub(crate) fn skip_unless_posix(test: &str) -> bool {
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

// The `/jobs` row and panel exist while a job runs: the registry's watch, installed by the loop, flips the
// table (re-issued through `set_slash_commands`, the one-table law's seam), and the command opens two
// live tabs — `Jobs`, a single-select list with one row per job, and `Kill`, the same rows with
// checkboxes — refreshed once a second; Enter on `Jobs` opens the row's detail page (live too), Esc there
// returns to the tabs, Esc on the tabs closes them. A job running before the loop was up is heard at
// install.
#[tokio::test]
async fn a_running_job_puts_jobs_in_the_table_and_the_panel_lists_it() {
    if skip_unless_posix("a_running_job_puts_jobs_in_the_table_and_the_panel_lists_it") {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let jobs = Jobs::new(tmp.path());
    jobs.spawn(&opts("sleep 30")).expect("spawn");
    jobs.spawn(&opts("sleep 31")).expect("spawn");

    let closed = || {
        Reply::Tabbed(iota::ui::facade::TabbedResult {
            cancelled: true,
            ..iota::ui::facade::TabbedResult::default()
        })
    };
    let ui = ScriptedUi::new(vec![
        Reply::Input(Input {
            display: "/jobs".to_owned(),
            text: "/jobs".to_owned(),
            kind: InputKind::Typed,
        }),
        // The list: Enter on the first row (the default result — cursor 0, not cancelled).
        Reply::Tabbed(iota::ui::facade::TabbedResult::default()),
        // The page: Esc goes back to the list.
        closed(),
        // The list again: Esc closes it.
        closed(),
        Reply::Interrupted,
    ]);
    iota::repl::run(params(&ui, &store, None, Arc::clone(&jobs)))
        .await
        .expect("clean exit");

    // The table was re-issued with the row once the watch heard the jobs — after the startup table.
    let tables: Vec<Vec<String>> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Commands(c) => Some(c.into_iter().map(|s| s.value).collect()),
            _ => None,
        })
        .collect();
    assert!(tables.len() >= 2, "{tables:?}");
    assert!(!tables[0].iter().any(|v| v == "/jobs"), "{:?}", tables[0]);
    let last = tables.last().expect("a table");
    let at = last
        .iter()
        .position(|v| v == "/jobs")
        .expect("/jobs joined the table");
    assert_eq!(last[at - 1], "/debug", "/jobs follows /debug: {last:?}");

    // The command opened the list, then the first job's page, then the list again — not the model: no
    // `❯` block, no answer.
    let panels: Vec<iota::testing::TabbedSummary> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(panels.len(), 3, "tabs, page, tabs: {panels:?}");
    for tabs in [&panels[0], &panels[2]] {
        assert_eq!(tabs.refresh_every_ms, 1000, "the tabs tick once a second");
        assert_eq!(tabs.panels.len(), 2, "Jobs and Kill: {tabs:?}");
        assert_eq!(tabs.panels[0].kind, iota::ui::facade::PanelKind::List);
        assert_eq!(tabs.panels[1].kind, iota::ui::facade::PanelKind::Multi);
        for (panel, title) in tabs.panels.iter().zip(["Jobs", "Kill"]) {
            assert_eq!(panel.title, title);
            assert_eq!(panel.prompt, "2 jobs running");
            assert!(panel.search, "the list searches past the fold");
            assert!(panel.has_refresh, "{title} is live");
            let rows: Vec<String> = panel
                .items
                .iter()
                .map(|r| iota::text::ansi::strip_sgr(r))
                .collect();
            assert_eq!(rows.len(), 2, "one row per job, no count row: {rows:?}");
            assert!(
                rows[0].starts_with("b1  ") && rows[0].ends_with("  sleep 30"),
                "{rows:?}"
            );
            assert!(
                rows[1].starts_with("b2  ") && rows[1].ends_with("  sleep 31"),
                "{rows:?}"
            );
            assert!(
                !rows.iter().any(|r| r.contains(".log")),
                "the log path is the page's: {rows:?}"
            );
        }
    }
    assert_eq!(
        panels[1].refresh_every_ms, 1000,
        "the page ticks once a second"
    );
    let page = &panels[1].panels[0];
    assert_eq!(page.title, "job b1");
    assert_eq!(page.kind, iota::ui::facade::PanelKind::View);
    assert!(page.wrap, "the page wraps: nothing on it is cut");
    assert!(page.has_refresh, "the page is live");
    assert_eq!(
        page.line_count, 6,
        "command, running, pid, output, the rule, (no output yet)"
    );
    assert!(
        !ui.events()
            .iter()
            .any(|e| matches!(e, UiEvent::UserBlock(s) if s == "/jobs")),
        "/jobs was sent to the model"
    );
    assert!(!printed(&ui).iter().any(|l| l.contains("noted")));
    // The status row's segment was handed the same set, oldest first.
    let shown: Vec<Vec<String>> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Jobs(j) => Some(j.into_iter().map(|j| j.id).collect()),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![vec!["b1".to_owned(), "b2".to_owned()]]);
    // The loop killed both on the way out.
    assert_eq!(jobs.running(), 0);
}

// The Kill tab: Enter with rows checked kills those jobs and only those, the surface closes without a line
// of its own, and each killed job's notice arrives as any job's does — `finished: killed` — and runs a
// turn; the other job runs on until the loop's exit takes it.
#[tokio::test]
async fn the_kill_tab_ends_the_checked_job_and_its_notice_lands() {
    if skip_unless_posix("the_kill_tab_ends_the_checked_job_and_its_notice_lands") {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let jobs = Jobs::new(tmp.path());
    jobs.spawn(&opts("sleep 30")).expect("spawn");
    jobs.spawn(&opts("sleep 31")).expect("spawn");

    let ui = ScriptedUi::new(vec![
        Reply::Input(Input {
            display: "/jobs".to_owned(),
            text: "/jobs".to_owned(),
            kind: InputKind::Typed,
        }),
        // Tab to Kill, Space on b1, Enter.
        Reply::Tabbed(iota::ui::facade::TabbedResult {
            cancelled: false,
            focused: 1,
            panels: vec![
                iota::ui::facade::PanelResult::default(),
                iota::ui::facade::PanelResult {
                    checked: vec![0],
                    ..iota::ui::facade::PanelResult::default()
                },
            ],
        }),
        // The notice, when it comes, is the next input: a turn runs on it.
        Reply::Enqueued,
        Reply::Interrupted,
    ]);
    iota::repl::run(params(&ui, &store, None, Arc::clone(&jobs)))
        .await
        .expect("clean exit");

    let lines = printed(&ui);
    let notice = "[background job b1 finished: killed] sleep 30";
    assert!(
        lines.iter().any(|l| l.contains(notice)),
        "the killed job's notice never landed:\n{lines:#?}"
    );
    assert!(lines.iter().any(|l| l.contains("noted")), "{lines:#?}");
    assert_eq!(
        lines.iter().filter(|l| l.contains("b1")).count(),
        1,
        "the notice is the whole report — the command prints nothing of its own:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("b2")),
        "b2 was not killed by the tab:\n{lines:#?}"
    );
    // One notice was enqueued before the loop left — b1's; b2 went with the exit, and the exit's kill
    // delivers nothing.
    let enqueued: Vec<Input> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Enqueue(i) => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(enqueued.len(), 1, "{enqueued:?}");
    assert_eq!(enqueued[0].display, notice);
    // The registry's record: b1 killed, and the status row was told the set shrank to b2.
    let b1 = jobs.ended("b1").expect("b1's end is remembered");
    assert!(b1.killed && b1.exit.is_none(), "{b1:?}");
    let shown: Vec<Vec<String>> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Jobs(j) => Some(j.into_iter().map(|j| j.id).collect()),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            vec!["b1".to_owned(), "b2".to_owned()],
            vec!["b2".to_owned()]
        ]
    );
    assert_eq!(jobs.running(), 0, "the loop killed the rest on the way out");
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
