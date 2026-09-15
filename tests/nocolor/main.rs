//! The `NO_COLOR` exit gate for the chat side (MIGRATION-ROADMAP §3 #2, DIVERGENCES X-27):
//! under [`ColorMode::Off`] the transcript, the markdown renderer, the diff renderer and the
//! `repl::styles` helpers emit **zero** escape sequences — attributes included.
//!
//! A binary of its own because the decision is process-wide and made once: every test here
//! calls [`iota::app::color::init`] with the answer `NO_COLOR=1` gives, and every other test
//! binary in the tree (which never decides) keeps rendering with color on — those are the
//! control for the assertions below. The assertions scan the BYTES the facade received for
//! `\x1b[` (and `\x1b]`, the OSC family); no flag is inspected.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use iota::BoxFuture;
use iota::app::color::ColorMode;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::model::{JsonObject, ToolDef};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, SessionStore};
use iota::testing::{FakeProvider, Reply, Round, ScriptedUi, UiEvent, tool_call};
use iota::tool::context::RunCtx;
use iota::tool::{Artifact, ArtifactKind, Dispatcher, Presentation, ToolOutput, ToolResult};
use iota::ui::facade::{Input, Ui};
use tokio_util::sync::CancellationToken;

/// `NO_COLOR=1` on a real terminal: the variable alone decides. The first call wins for the
/// whole process, so every test calls this first and they all agree.
fn no_color() {
    let env = |name: &str| (name == "NO_COLOR").then(|| "1".to_owned());
    let mode = iota::app::color::init(ColorMode::detect(&env, true));
    assert_eq!(mode, ColorMode::Off);
}

/// One of every construct the markdown renderer styles: an H1 (`1;4`), emphasis, inline
/// code, a link (OSC 8 + faint URL), a fenced block WITH a language (syntect colors), a
/// table and a list.
const DOC: &str = "# Heading\n\nsome *text* with `code` and [a link](https://iota.sh)\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n\ndone.\n";

/// Every string an event carried, so a scan sees the transcript exactly as the facade did.
fn texts(events: &[UiEvent]) -> Vec<String> {
    let mut out = Vec::new();
    for e in events {
        match e {
            UiEvent::Print(lines) | UiEvent::CallBody(lines) => out.extend(lines.iter().cloned()),
            UiEvent::UserBlock(s)
            | UiEvent::Preview(s)
            | UiEvent::PreviewLine(s)
            | UiEvent::Busy(s)
            | UiEvent::BusyDetail(s)
            | UiEvent::CallPreview(s)
            | UiEvent::CallDetail(s)
            | UiEvent::CallLine(s)
            | UiEvent::Notify(s)
            | UiEvent::Title(s) => out.push(s.clone()),
            _ => {}
        }
    }
    out
}

/// Fails on the first byte sequence that opens a CSI (`\x1b[`) or an OSC (`\x1b]`).
fn assert_escape_free(what: &str, lines: &[String]) {
    for line in lines {
        assert!(
            !line.contains("\x1b[") && !line.contains("\x1b]"),
            "{what}: an escape sequence reached the facade under NO_COLOR: {line:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// A streaming tool provider: call 1 streams a sentence and asks for `edit`; call 2 streams
/// [`DOC`]; every later call fails — one of every block the chat side styles, in one run.
fn scripted() -> FakeProvider {
    let mut edit = Round::text("Let me edit that.\n");
    edit.result.tool_calls = vec![tool_call("c1", "edit")];
    FakeProvider::new()
        .with_tools()
        .round(edit)
        .round(Round::text(DOC))
        .tail(Round::permanent("the model is gone"))
}

/// One tool, `edit`, that posts a unified diff artifact — the row shape `render_diff` shades.
struct Editor;

impl Dispatcher for Editor {
    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "edit".to_owned(),
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
            if let Some(slot) = &cx.artifact {
                slot.post(Artifact {
                    kind: ArtifactKind::Diff,
                    title: "main.rs".to_owned(),
                    lines: vec![
                        "@@ -1,2 +1,2 @@".to_owned(),
                        "-fn main() {}".to_owned(),
                        "+fn main() { run(); }".to_owned(),
                        " // unchanged".to_owned(),
                    ],
                });
            }
            Ok(ToolOutput::ok("edited main.rs"))
        })
    }

    /// The standalone block: that is where the diff artifact renders (T-35).
    fn presentation(&self, _name: &str) -> Presentation {
        Presentation::Expanded
    }
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
    })
}

fn params(ui: &Arc<ScriptedUi>, store: &SessionStore) -> RunParams {
    RunParams {
        ui: Arc::clone(ui) as Arc<dyn Ui>,
        provider: Box::new(scripted()),
        title_provider: None,
        system: String::new(),
        imported_history: Vec::new(),
        dispatch: Arc::new(Editor),
        jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
        mcp: McpHooks {
            servers: None,
            events: None,
        },
        session: SessionCtx {
            writer: Some(
                store
                    .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
                    .expect("create writer"),
            ),
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
        reqlog: Arc::new(RequestLog::new()),
        pres: Arc::new(Presenter::with_hosts(Vec::new(), false)),
    }
}

// ---------------------------------------------------------------------------
// the gate
// ---------------------------------------------------------------------------

/// Gate (i), end to end: a whole interactive run — banner, a streamed sentence, a tool
/// call with its cyan header and its shaded diff, a markdown document with every styled
/// block, a red error block — reaches the facade without one escape byte.
#[tokio::test]
async fn a_whole_run_reaches_the_facade_escape_free() {
    no_color();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let ui = ScriptedUi::new(vec![
        input("go"),
        Reply::Queued(Vec::new()), // the round boundary's steering drain after the tool call
        input("again"),
        Reply::Interrupted,
    ]);
    iota::repl::run(params(&ui, &store))
        .await
        .expect("clean exit");

    let lines = texts(&ui.events());
    assert_escape_free("interactive run", &lines);

    // Not vacuous: each styled surface actually rendered — the text is there, bare.
    let joined = lines.join("\n");
    for needle in [
        "Let me edit that.",      // the streamed sentence
        "edit",                   // the tool-call header (cyan when painted)
        "+ fn main() { run(); }", // a diff row (256-color block when painted)
        "- fn main() {}",
        "Heading", // H1 (bold+underline when painted)
        "some text with code and a link (https://iota.sh)", // emphasis, code, OSC 8 + faint URL
        "• one",
        "│ a   │ b   │",
        "  fn main() {}",    // the rust fence (syntect colors when painted)
        "the model is gone", // the error block (red when painted)
    ] {
        assert!(
            joined.contains(needle),
            "{needle:?} missing from:\n{joined}"
        );
    }
}

/// A `Write` the piped markdown writer can own while the test keeps a handle on the bytes.
struct Shared(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Gate (i) on the entry points the run above does not walk: the piped markdown writer
/// (`new_writer_to`) and an OSC 8 hyperlink, both reading the process decision. (`render_diff` takes the
/// decision as an argument — its colour-off shape is pinned beside it in `repl::diff`, and the run above
/// walks the group renderer that feeds it.)
#[test]
fn the_standalone_renderers_are_escape_free() {
    no_color();

    // new_writer_to reads the process decision (Go's NewWriterTo read color.NoColor).
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut w = iota::markdown::new_writer_to(Box::new(Shared(Arc::clone(&out))), 80);
    w.write(DOC.as_bytes());
    w.flush();
    let rendered = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    assert_escape_free("new_writer_to", std::slice::from_ref(&rendered));
    assert!(rendered.contains("Heading") && rendered.contains("│ 1   │ 2   │"));

    assert_eq!(
        iota::markdown::hyperlink("https://iota.sh", "iota", iota::app::color::enabled()),
        "iota"
    );
}
