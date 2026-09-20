//! WP54 L3 suite for `/file` (chat/run.go:390-446 + chat/file.go, T-12).
//!
//! `read_attachment`'s own refusals and the label formats are unit-tested beside their
//! source (`commands/file.rs`); what this file drives is the COMMAND, through the public
//! entry point (`iota::repl::run` over `iota::testing::ScriptedUi`), because the parts
//! that only exist end to end are the interesting ones:
//!
//! - the path form attaches immediately and the queue survives to the next send, which is
//!   asserted from the message the provider actually receives;
//! - the bare form opens `Attached` (Multi) + `Add` (Browser) and the FOCUSED tab is the
//!   verb — commit from tab 0 removes the checked rows, from tab 1 attaches the browsed
//!   file, and a cancel does neither;
//! - removal indexes the ORIGINAL rows (the engine's commit contract), so a filtered
//!   `Attached` tab still drops the rows the user checked.

use std::path::Path;
use std::sync::Arc;

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::model::Attachment;
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::SessionStore;
use iota::testing::{
    FakeProvider, Log, Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent,
};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// The attachments of the LAST message of every call the provider saw — the only way to prove the queue
/// actually rode the next send.
fn attachments_sent(log: &Log) -> Vec<Vec<Attachment>> {
    log.sent()
        .iter()
        .map(|history| {
            history
                .last()
                .map(|m| m.attachments.clone())
                .unwrap_or_default()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

struct Fixture {
    ui: Arc<ScriptedUi>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
    /// The provider's call log.
    seen: Log,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
            seen: Log::default(),
        }
    }

    fn params(&self) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(
                FakeProvider::new()
                    .replying("ok")
                    .with_log(self.seen.clone()),
            ),
            title_provider: None,
            system: String::new(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
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

/// A commit of the two-tab `/file` surface, focused on `focused`.
fn commit(focused: usize, panels: Vec<PanelResult>) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused,
        panels,
    })
}

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

fn surfaces(ui: &ScriptedUi) -> Vec<TabbedSummary> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Writes a fixture file and returns its path as the string `/file` would be given.
fn fixture(dir: &Path, name: &str, body: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).expect("write fixture");
    p.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// the path form
// ---------------------------------------------------------------------------

/// `"/file <path>"` attaches immediately with the byte-exact notice, and the queue rides the NEXT
/// user message.
#[tokio::test]
async fn file_path_form_attaches_and_rides_the_next_message() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = fixture(tmp.path(), "notes.md", b"# hi\n");

    let f = Fixture::new(vec![
        input(&format!("/file {path}")),
        input("what is in it?"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    assert!(
        printed(&f.ui).contains(&"Attached: notes.md (text/plain, 5 bytes)".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(
        surfaces(&f.ui).is_empty(),
        "the path form must not open a surface"
    );

    let sent = attachments_sent(&f.seen);
    assert_eq!(sent.len(), 1, "one turn ran");
    assert_eq!(sent[0].len(), 1, "the attachment rode the message");
    assert_eq!(sent[0][0].filename, "notes.md");
    assert_eq!(sent[0][0].mime_type, "text/plain");
    assert_eq!(sent[0][0].data, b"# hi\n");
}

/// A path that cannot be attached prints the red `"Error: %v"` line and queues nothing —
/// the loop continues, as every command failure does.
#[tokio::test]
async fn file_path_form_reports_the_failure_and_queues_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = fixture(tmp.path(), "tool.exe", b"MZ");

    let f = Fixture::new(vec![
        input(&format!("/file {path}")),
        input("hello"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    assert!(
        printed(&f.ui).contains(&"Error: unsupported file type: .exe".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(
        attachments_sent(&f.seen)[0].is_empty(),
        "a failed attach must queue nothing"
    );
}

// ---------------------------------------------------------------------------
// the surface
// ---------------------------------------------------------------------------

/// The bare form opens `Attached` (Multi over the queue, its
/// rows the `attachmentLabel` form) beside `Add` (a Browser), both searchable.
#[tokio::test]
async fn file_surface_opens_attached_and_add() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = fixture(tmp.path(), "shot.png", &[0u8; 2048]);

    let f = Fixture::new(vec![
        input(&format!("/file {path}")),
        input("/file"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    let s = &surfaces(&f.ui)[0];
    assert_eq!(s.panels.len(), 2);
    assert_eq!(s.panels[0].title, "Attached");
    assert_eq!(s.panels[0].kind, PanelKind::Multi);
    assert!(s.panels[0].search);
    assert_eq!(s.panels[0].items, ["shot.png (image/png, 2.0 KB)"]);
    assert_eq!(s.panels[1].title, "Add");
    assert_eq!(s.panels[1].kind, PanelKind::Browser);
    assert!(s.panels[1].search);
    assert!(
        !s.enter_advances,
        "the focused tab is the verb — Enter commits from anywhere"
    );

    // A cancelled surface neither removes nor attaches.
    assert!(!printed(&f.ui).iter().any(|l| l.starts_with("Removed ")));
}

/// Committing from tab 0 REMOVES the checked rows, counted in
/// the byte-exact notice. Checks index the ORIGINAL queue (the engine's commit contract),
/// so a filtered tab still drops what the user checked.
#[tokio::test]
async fn file_surface_removes_the_checked_attachments() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = fixture(tmp.path(), "a.md", b"aaa");
    let b = fixture(tmp.path(), "b.md", b"bb");
    let c = fixture(tmp.path(), "c.md", b"c");

    let f = Fixture::new(vec![
        input(&format!("/file {a}")),
        input(&format!("/file {b}")),
        input(&format!("/file {c}")),
        input("/file"),
        commit(
            0,
            vec![
                PanelResult {
                    checked: vec![0, 2],
                    ..PanelResult::default()
                },
                PanelResult::default(),
            ],
        ),
        input("go"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    assert!(
        printed(&f.ui).contains(&"Removed 2 attachment(s).".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    let sent = attachments_sent(&f.seen);
    let names: Vec<&str> = sent[0].iter().map(|x| x.filename.as_str()).collect();
    assert_eq!(names, ["b.md"], "the unchecked row is the survivor");
}

/// Committing tab 0 with nothing checked is a no-op — no notice, no change. The user
/// opened the list to look at it.
#[tokio::test]
async fn file_surface_removing_nothing_says_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = fixture(tmp.path(), "a.md", b"aaa");

    let f = Fixture::new(vec![
        input(&format!("/file {a}")),
        input("/file"),
        commit(0, vec![PanelResult::default(), PanelResult::default()]),
        input("go"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    assert!(!printed(&f.ui).iter().any(|l| l.starts_with("Removed ")));
    assert_eq!(
        attachments_sent(&f.seen)[0].len(),
        1,
        "the queue is untouched"
    );
}

/// Committing from tab 1 attaches the BROWSED file, with the
/// same `"Attached:"` notice the path form prints (one attach path, one message).
#[tokio::test]
async fn file_surface_attaches_the_browsed_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = fixture(tmp.path(), "picked.txt", b"body");

    let f = Fixture::new(vec![
        input("/file"),
        commit(
            1,
            vec![
                PanelResult::default(),
                PanelResult {
                    path: path.clone(),
                    ..PanelResult::default()
                },
            ],
        ),
        input("go"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    assert!(
        printed(&f.ui).contains(&"Attached: picked.txt (text/plain, 4 bytes)".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert_eq!(attachments_sent(&f.seen)[0][0].filename, "picked.txt");
}

/// Committing tab 1 without having chosen a file (the browser was only scrolled) attaches
/// nothing, and an unreadable choice reports the failure without queueing.
#[tokio::test]
async fn file_surface_browser_without_a_choice_or_with_a_bad_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad = fixture(tmp.path(), "thing.bin", b"\0\0");

    let f = Fixture::new(vec![
        input("/file"),
        commit(1, vec![PanelResult::default(), PanelResult::default()]),
        input("/file"),
        commit(
            1,
            vec![
                PanelResult::default(),
                PanelResult {
                    path: bad,
                    ..PanelResult::default()
                },
            ],
        ),
        input("go"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params()).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(!lines.iter().any(|l| l.starts_with("Attached: ")));
    assert!(
        lines.contains(&"Error: unsupported file type: .bin".to_owned()),
        "{lines:?}"
    );
    assert!(attachments_sent(&f.seen)[0].is_empty());
}

/// `/file` is a REGISTERED command now (T-12): it heads Go's base table, so it is both
/// advertised and dispatched — the one-table law, seen from the banner.
#[tokio::test]
async fn file_is_advertised_and_dispatched() {
    let f = Fixture::new(vec![input("/filex"), Reply::Interrupted]);
    iota::repl::run(f.params()).await.expect("exit");

    let lines = printed(&f.ui);
    assert_eq!(
        lines[1],
        "Commands: /file, /session, /model, /export, /status, /tools, /debug"
    );
    // A longer name is NOT the command: "/filex" falls through as a plain message.
    assert_eq!(
        attachments_sent(&f.seen).len(),
        1,
        "/filex must be sent as text"
    );
}
