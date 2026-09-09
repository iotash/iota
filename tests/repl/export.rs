//! `/export` end to end over `ScriptedUi` (WP65; `T3_TEST_PLAN` §3): the format picker, the
//! generated name, `already exists`, the ephemeral vs on-disk sources and the two full-document
//! goldens.
//!
//! The builders and every pure helper are unit-tested beside their source
//! (`src/repl/commands/export.rs`); what only exists end to end is here:
//!
//! - the picker (`Export format`, `HTML`/`Markdown`) and the fact that a cancel prints NOTHING;
//! - the generated `iota-<slug>-<stamp>.<ext>` name, created in the process's working directory
//!   exactly as Go's `filepath.Abs` of a bare name does;
//! - the source rule — a saved session exports the FULL on-disk log (past a `/compact`), an
//!   ephemeral one the in-memory history;
//! - `O_EXCL`: a second export to the same path is an error, never a silent overwrite;
//! - the two checked-in full-document goldens, compared byte for byte against what the command
//!   actually wrote (the per-run header line and, for HTML, the token-palette `<style>` block
//!   are masked — the session id, the clock and the syntect palette are the only parts that
//!   cannot be pinned).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::error::ProviderError;
use iota::provider::model::{Attachment, Body, Message, ToolBody, ToolCall};
use iota::provider::{ChatResult, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// The Markdown golden (`T3_CONTRACTS` §8), built from Go's `exportTestHistory`.
const SAMPLE_MD: &str = include_str!("../fixtures/export/sample.md");
/// The HTML golden (`T3_CONTRACTS` §8).
const SAMPLE_HTML: &str = include_str!("../fixtures/export/sample.html");

const KIND: ProviderKind = ProviderKind::OpenAi;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// A provider that never runs a turn — `/export` only reads its model name.
struct FakeProvider;

impl Provider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        KIND
    }
    fn model(&self) -> &'static str {
        "gpt-x"
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
            text: "ok".to_owned(),
            ..ChatResult::default()
        })))
    }
}

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

struct Fixture {
    ui: Arc<ScriptedUi>,
    tmp: tempfile::TempDir,
    store: SessionStore,
}

impl Fixture {
    /// A scenario whose temp directory exists before the script is written, so a target path
    /// inside it can be named in the very input that exercises it.
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(Vec::new()),
            tmp,
            store,
        }
    }

    fn script(mut self, script: Vec<Reply>) -> Self {
        self.ui = ScriptedUi::new(script);
        self
    }

    /// A path inside the fixture's temp directory, as `/export` would be given it.
    fn path(&self, name: &str) -> String {
        self.tmp.path().join(name).to_string_lossy().into_owned()
    }

    fn writer(&self, title: &str) -> SessionWriter {
        let mut w = self
            .store
            .create(KIND, "gpt-x", None, "", "", false)
            .expect("create writer");
        w.update_meta(|m| title.clone_into(&mut m.title))
            .expect("set title");
        w
    }

    fn params(&self, writer: Option<SessionWriter>, history: Vec<Message>) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(FakeProvider),
            title_provider: None,
            system: String::new(),
            system_interactive: false,
            imported_history: history,
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            delegator: None,
            mcp: McpHooks {
                servers: None,
                events: None,
            },
            session: SessionCtx {
                writer,
                store: self.store.clone(),
                new_session: None,
                scope: None,
            },
            context_window: 0,
            agent: iota::chat::AgentOptions::default(),
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
    })
}

/// A one-panel commit (the picker's shape).
fn pick(cursor: usize) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor,
            ..PanelResult::default()
        }],
    })
}

fn cancelled() -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: true,
        ..TabbedResult::default()
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

/// The notice lines `/export` produces — the three shapes it can print (chat/export.go:557,
/// :576, :597, :611), picked out of the banner and the session replay echo around them.
fn export_lines(ui: &ScriptedUi) -> Vec<String> {
    printed(ui)
        .into_iter()
        .filter(|l| {
            l.starts_with("Exported ") || l.starts_with("Error: ") || l == "Nothing to export yet."
        })
        .collect()
}

/// Every line printed AFTER the first blocking surface call — the exact "a cancelled picker
/// prints nothing" assertion.
fn printed_after_the_picker(ui: &ScriptedUi) -> Vec<String> {
    let events = ui.events();
    let after = events
        .iter()
        .position(|e| matches!(e, UiEvent::Tabbed(_)))
        .map_or(events.len(), |i| i + 1);
    events[after..]
        .iter()
        .filter_map(|e| match e {
            UiEvent::Print(lines) => Some(lines.clone()),
            _ => None,
        })
        .flatten()
        .map(|l| strip_sgr(&l))
        .collect()
}

/// `Go: chat/export_test.go:78 exportTestHistory` — the fixture both goldens were built from.
fn export_test_history() -> Vec<Message> {
    vec![
        Message::system("be terse"),
        Message {
            attachments: vec![Attachment {
                filename: "a.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                data: b"x".to_vec(),
            }],
            ..Message::user("read the file")
        },
        Message::assistant("")
            .with_reasoning("let me think about this".to_owned())
            .with_tool_calls(vec![ToolCall {
                id: "c1".to_owned(),
                name: "load_skill".to_owned(),
                arguments: [("path".to_owned(), serde_json::json!("a.txt"))]
                    .into_iter()
                    .collect(),
            }]),
        Message {
            content: "file contents".to_owned(),
            body: Body::Tool(ToolBody {
                call_id: "c1".to_owned(),
                call_name: "load_skill".to_owned(),
                ..ToolBody::default()
            }),
            ..Message::default()
        },
        Message::assistant("It says **x**."),
        Message::user("thanks"),
        Message::assistant("partial…").with_interrupted(true),
    ]
}

/// Replaces the one line that cannot be pinned (`Session <random id> · gpt-x · <now>`) with
/// `@@` and, for HTML, the whole token-palette `<style>` block (T-42's structure-only region).
/// Everything else must match the golden byte for byte.
fn mask(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let rest = match (doc.find("<style>\n"), doc.find("</style>")) {
        (Some(a), Some(b)) => {
            out.push_str(&doc[..a + "<style>\n".len()]);
            out.push_str("@@\n");
            &doc[b..]
        }
        _ => doc,
    };
    for line in rest.split_inclusive('\n') {
        if line.starts_with("> Session ") || line.starts_with("<p class=\"meta\">Session ") {
            out.push_str("@@\n");
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Removes a generated export file when the test ends, however it ends — the no-argument form
/// writes into the process's working directory by construction (Go does the same).
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// the picker (chat/run.go:804-820)
// ---------------------------------------------------------------------------

/// Go: chat/run.go:806-814 — a bare `/export` asks for the format first; a CANCELLED picker
/// returns to the prompt in silence (`continue`: nothing written, nothing printed).
#[tokio::test]
async fn bare_export_opens_the_format_picker_and_a_cancel_is_silent() {
    let f = Fixture::new().script(vec![input("/export"), cancelled(), Reply::Interrupted]);
    let writer = f.writer("My Chat");
    iota::repl::run(f.params(Some(writer), vec![Message::user("hi")]))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].title, "Export format");
    assert_eq!(panels[0].kind, PanelKind::List);
    assert_eq!(panels[0].items, ["HTML", "Markdown"]);
    assert!(panels[0].search, "`Ui::select` always offers row search");
    assert_eq!(
        printed_after_the_picker(&f.ui),
        Vec::<String>::new(),
        "a cancelled picker prints nothing at all"
    );
}

/// Go: chat/run.go:812-815 — row 1 is Markdown, and the target name is generated from the
/// session title into the process's working directory (`filepath.Abs` of a bare name).
#[tokio::test]
async fn picking_markdown_writes_the_generated_name() {
    let f = Fixture::new().script(vec![input("/export"), pick(1), Reply::Interrupted]);
    let mut writer = f.writer("Fix the Build!");
    writer
        .append_messages(&[Message::user("q"), Message::assistant("a")])
        .expect("append");
    iota::repl::run(f.params(Some(writer), Vec::new()))
        .await
        .expect("exit");

    let lines = export_lines(&f.ui);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let path = PathBuf::from(
        lines[0]
            .strip_prefix("Exported 2 messages → ")
            .unwrap_or_else(|| panic!("unexpected notice: {}", lines[0])),
    );
    let _cleanup = Cleanup(path.clone());

    assert!(path.is_absolute(), "the notice prints the absolute path");
    assert_eq!(
        path.parent().expect("a parent"),
        std::env::current_dir().expect("cwd"),
        "a bare generated name lands in the working directory"
    );
    let name = path.file_name().expect("a name").to_string_lossy();
    assert!(
        name.starts_with("iota-fix-the-build-") && name.ends_with(".md"),
        "generated name: {name}"
    );
    assert_eq!(name.len(), "iota-fix-the-build-20260707-093015.md".len());
    let doc = std::fs::read_to_string(&path).expect("the file exists");
    assert!(doc.starts_with("# Fix the Build!\n\n> Session "), "{doc}");
}

/// Go: chat/run.go:810-812 — any row but 1 is HTML, the default format.
#[tokio::test]
async fn picking_html_generates_an_html_name() {
    let f = Fixture::new().script(vec![input("/export"), pick(0), Reply::Interrupted]);
    let mut writer = f.writer("Html Pick");
    writer
        .append_messages(&[Message::user("q")])
        .expect("append");
    iota::repl::run(f.params(Some(writer), Vec::new()))
        .await
        .expect("exit");

    let lines = export_lines(&f.ui);
    let path = PathBuf::from(
        lines[0]
            .strip_prefix("Exported 1 messages → ")
            .unwrap_or_else(|| panic!("unexpected notice: {}", lines[0])),
    );
    let _cleanup = Cleanup(path.clone());
    let name = path.file_name().expect("a name").to_string_lossy();
    assert!(
        name.starts_with("iota-html-pick-") && name.ends_with(".html"),
        "generated name: {name}"
    );
    assert!(
        std::fs::read_to_string(&path)
            .expect("the file exists")
            .starts_with("<!DOCTYPE html>\n")
    );
}

// ---------------------------------------------------------------------------
// the argument form (chat/export.go:548-612)
// ---------------------------------------------------------------------------

/// Go: chat/export.go:65-74 + :592-611 — a name with no extension gets `.html`, and the notice
/// carries the ABSOLUTE path and the conversation count (the system prompt does not count).
#[tokio::test]
async fn a_bare_name_becomes_html_and_the_notice_is_absolute() {
    let f = Fixture::new();
    let target = f.path("chat");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let history = vec![
        Message::system("be terse"),
        Message::user("hi"),
        Message::assistant("yo"),
    ];
    iota::repl::run(f.params(None, history))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [format!("Exported 2 messages → {target}.html")]
    );
    let doc = std::fs::read_to_string(format!("{target}.html")).expect("the file exists");
    assert!(doc.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n"));
    assert!(doc.ends_with("</body>\n</html>\n"));
}

/// Go: chat/export.go:592-598 — `O_EXCL`: an existing target is an error, never an overwrite.
#[tokio::test]
async fn a_second_export_to_the_same_path_refuses() {
    let f = Fixture::new();
    let target = f.path("twice.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params(None, vec![Message::user("only me")]))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [
            format!("Exported 1 messages → {target}"),
            format!("Error: {target} already exists"),
        ]
    );
    // The first document survived untouched.
    assert!(
        std::fs::read_to_string(&target)
            .expect("the file exists")
            .contains("only me")
    );
}

/// Go: chat/export.go:50-59 — both refusals, in Go's `%q` quoting, before anything is opened.
#[tokio::test]
async fn invalid_targets_are_refused_before_any_write() {
    let f = Fixture::new();
    let dir = f.tmp.path().to_string_lossy().into_owned();
    let f = f.script(vec![
        input(&format!("/export {dir}/")),
        input("/export .md"),
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params(None, vec![Message::user("hi")]))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [
            format!("Error: \"{dir}/\" is a directory path; give a file name"),
            "Error: \".md\" has no usable file name".to_owned(),
        ]
    );
    assert_eq!(
        std::fs::read_dir(f.tmp.path())
            .expect("readable")
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "sessions")
            .count(),
        0,
        "nothing was created"
    );
}

/// Go: chat/export.go:575-578 — a chat with no conversation messages refuses, dimly.
#[tokio::test]
async fn an_empty_chat_has_nothing_to_export() {
    let f = Fixture::new();
    let target = f.path("empty.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    // A system prompt is a header section, not a turn — it does not count.
    iota::repl::run(f.params(None, vec![Message::system("be terse")]))
        .await
        .expect("exit");

    assert_eq!(export_lines(&f.ui), ["Nothing to export yet."]);
    assert!(!Path::new(&target).exists(), "nothing was written");
}

// ---------------------------------------------------------------------------
// the source rule (chat/export.go:566-578)
// ---------------------------------------------------------------------------

/// Go: chat/export.go:567-573 — an ephemeral chat exports the IN-MEMORY history. A writer that
/// exists but has never been appended to is NOT on disk yet (`sw.onDisk()`,
/// chat/session.go:477-479), so it takes the same branch.
#[tokio::test]
async fn an_ephemeral_chat_exports_the_in_memory_history() {
    let f = Fixture::new();
    let target = f.path("mem.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let writer = f.writer("Ephemeral");
    let history = vec![
        Message::user("only in memory"),
        Message::assistant("never persisted"),
    ];
    iota::repl::run(f.params(Some(writer), history))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [format!("Exported 2 messages → {target}")]
    );
    let doc = std::fs::read_to_string(&target).expect("the file exists");
    assert!(
        doc.contains("only in memory") && doc.contains("never persisted"),
        "{doc}"
    );
    assert!(doc.starts_with("# Ephemeral\n"), "{doc}");
}

/// Go: chat/export.go:568-573 + chat/session.go:920-941 — a SAVED session exports the full
/// on-disk log, so a `/compact` can never hide an archived round from the archive.
#[tokio::test]
async fn a_saved_session_exports_the_full_log_past_a_compaction() {
    let f = Fixture::new();
    let target = f.path("full.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer("Saved");
    writer
        .append_messages(&[
            Message::user("first question"),
            Message::assistant("first answer"),
        ])
        .expect("round 1");
    writer
        .append_compaction("SUMMARY", 0, None)
        .expect("marker");
    writer
        .append_messages(&[
            Message::user("second question"),
            Message::assistant("second answer"),
        ])
        .expect("round 2");

    // The in-memory history is the COMPACTED view — the export must ignore it.
    let view = vec![
        Message::user("second question"),
        Message::assistant("second answer"),
    ];
    iota::repl::run(f.params(Some(writer), view))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [format!("Exported 4 messages → {target}")],
        "the count is the FULL log's, not the view's"
    );
    let doc = std::fs::read_to_string(&target).expect("the file exists");
    for want in [
        "first question",
        "first answer",
        "second question",
        "second answer",
    ] {
        assert!(doc.contains(want), "full log missing {want:?}:\n{doc}");
    }
    assert!(
        !doc.contains("SUMMARY"),
        "the compaction marker leaked into the export:\n{doc}"
    );
    assert_eq!(doc.matches("---").count(), 1, "two rounds, one rule");
}

/// Go: chat/export.go:569-572 — a load failure prints `Error: <e>` and writes nothing, rather
/// than exporting an empty document.
#[tokio::test]
async fn a_load_failure_is_reported_and_writes_nothing() {
    let f = Fixture::new();
    let target = f.path("broken.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer("Broken");
    writer
        .append_messages(&[Message::user("q")])
        .expect("append");
    // The bundle is on disk (so the full-log branch is taken) but its log has gone.
    std::fs::remove_file(writer.dir().join("messages.jsonl")).expect("remove log");
    iota::repl::run(f.params(Some(writer), vec![Message::user("q")]))
        .await
        .expect("exit");

    let lines = export_lines(&f.ui);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("Error: "), "{}", lines[0]);
    assert!(!Path::new(&target).exists(), "nothing was written");
}

// ---------------------------------------------------------------------------
// the full-document goldens (`T3_CONTRACTS` §8)
// ---------------------------------------------------------------------------

/// The Markdown document `/export` writes for Go's `exportTestHistory` is the checked-in
/// golden, byte for byte, once the per-run header line is masked.
#[tokio::test]
async fn markdown_document_matches_the_golden() {
    let f = Fixture::new();
    let target = f.path("golden.md");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let writer = f.writer("My Chat");
    iota::repl::run(f.params(Some(writer), export_test_history()))
        .await
        .expect("exit");

    let doc = std::fs::read_to_string(&target).expect("the file exists");
    assert_eq!(mask(&doc), mask(SAMPLE_MD));
}

/// The HTML document is the checked-in golden with the header line and the token-palette
/// `<style>` block masked — every hand-written tag, every escape, the data URI and the whole
/// toggle script are pinned.
#[tokio::test]
async fn html_document_matches_the_golden() {
    let f = Fixture::new();
    let target = f.path("golden.html");
    let f = f.script(vec![
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let writer = f.writer("My Chat");
    iota::repl::run(f.params(Some(writer), export_test_history()))
        .await
        .expect("exit");

    let doc = std::fs::read_to_string(&target).expect("the file exists");
    assert_eq!(mask(&doc), mask(SAMPLE_HTML));
    // The masked region is real, not empty: the palette carries both dark scopes.
    assert!(doc.contains("html[data-theme=\"dark\"] .chroma"));
    assert!(doc.contains("@media (prefers-color-scheme: dark) {"));
}

/// The writer slot is never held across the picker await: a second `/export` right after a
/// cancelled picker still reaches the writer (chat/run.go:806 — Go reads `sw` around the call,
/// never through it).
#[tokio::test]
async fn the_writer_slot_is_free_after_the_picker() {
    let f = Fixture::new();
    let target = f.path("after-picker.md");
    let f = f.script(vec![
        input("/export"),
        cancelled(),
        input(&format!("/export {target}")),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer("Slot");
    writer
        .append_messages(&[Message::user("q"), Message::assistant("a")])
        .expect("append");
    iota::repl::run(f.params(Some(writer), Vec::new()))
        .await
        .expect("exit");

    assert_eq!(
        export_lines(&f.ui),
        [format!("Exported 2 messages → {target}")]
    );
}
