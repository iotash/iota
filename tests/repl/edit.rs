//! `/edit` and `/redo` end to end over `ScriptedUi` (WP64; `T3_TEST_PLAN` §2): the picker shape and commit, the no-images notice, the canvas attachment and the redo echo.
//!
//! What only exists end to end is the FALL-THROUGH: `/edit <prompt>` and `/redo` are the two
//! commands that send instead of returning to the composer, so the only way to prove the canvas
//! rode the request — and that the echo still shows the line the user typed, not the expansion —
//! is to read the message the provider actually received.
//!
//! The double is an image provider (`ImageGenTunable`), because that capability is what registers
//! both commands at all (chat/run.go:54-55, completion.go:34-37).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex, PoisonError};

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::error::ProviderError;
use iota::provider::model::{Attachment, Message, Role};
use iota::provider::{
    ChatResult, ImageGenOptions, ImageGenParams, ImageGenTunable, Provider, ProviderKind,
};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::SessionStore;
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// The checked-in 2×2 fixture — real PNG bytes, so the picker's previewer decodes rather than
/// reporting `(cannot preview …)`.
const RB_2X2_PNG: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// Records every message it is sent, so the canvas attachment and the expanded prompt can be read
/// off the wire rather than inferred.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Message>>>);

impl Recorder {
    fn sent(&self) -> Vec<Message> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// An image provider that never returns a picture: the turn is valid (it answers text), and the
/// picker's history comes from `imported_history` instead — no image is ever written to disk.
struct FakeImageProvider {
    seen: Recorder,
    params: ImageGenParams,
}

impl Provider for FakeImageProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Images
    }

    fn model(&self) -> &'static str {
        "gpt-image-1"
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
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        if let Some(m) = messages.last() {
            self.seen
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(m.clone());
        }
        Box::pin(std::future::ready(Ok(ChatResult {
            text: "ok".to_owned(),
            ..ChatResult::default()
        })))
    }

    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        Some(self)
    }
}

impl ImageGenTunable for FakeImageProvider {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        self.params = p;
    }

    fn image_gen_params(&self) -> &ImageGenParams {
        &self.params
    }

    fn image_gen_options(&self) -> ImageGenOptions {
        ImageGenOptions {
            aspect_ratios: Vec::new(),
            image_sizes: vec!["auto"],
            negative_prompt: false,
        }
    }
}

// ---------------------------------------------------------------------------
// the harness
// ---------------------------------------------------------------------------

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
    })
}

/// A picker commit on row `cursor`.
fn commit(cursor: usize) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor,
            ..PanelResult::default()
        }],
    })
}

fn png(name: &str) -> Attachment {
    Attachment {
        filename: name.to_owned(),
        mime_type: "image/png".to_owned(),
        data: RB_2X2_PNG.to_vec(),
    }
}

fn msg(role: Role, content: &str, atts: Vec<Attachment>) -> Message {
    Message {
        attachments: atts,
        ..Message::of_role(role, content.to_owned())
    }
}

/// Two generated pictures in history, newest last (the picker reverses them).
fn two_generations() -> Vec<Message> {
    vec![
        msg(Role::User, "a cat", Vec::new()),
        msg(Role::Assistant, "", vec![png("20260725-093820-0.png")]),
        msg(Role::User, "add a hat", Vec::new()),
        msg(Role::Assistant, "", vec![png("20260725-101500-0.png")]),
    ]
}

struct Fixture {
    ui: Arc<ScriptedUi>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
    seen: Recorder,
    history: Vec<Message>,
}

impl Fixture {
    fn new(script: Vec<Reply>, history: Vec<Message>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
            seen: Recorder::default(),
            history,
        }
    }

    /// Runs the scripted session to EOF; the chat stays ephemeral (no writer), so the picker's
    /// detail column is empty and nothing touches the filesystem.
    async fn run(&self) {
        iota::repl::run(RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(FakeImageProvider {
                seen: self.seen.clone(),
                params: ImageGenParams::default(),
            }),
            title_provider: None,
            system: String::new(),
            imported_history: self.history.clone(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: McpHooks {
                servers: None,
                events: None,
            },
            session: SessionCtx {
                writer: None,
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
        })
        .await
        .expect("clean exit");
    }
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

/// The lines the transcript echoed as USER blocks (what the user typed, never the expansion).
fn echoed(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::UserBlock(s) => Some(s),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// bare /edit — the picker
// ---------------------------------------------------------------------------

/// Go: chat/run.go:458-485 — bare `/edit` opens a single `Picker` panel with the byte-exact title
/// and prompt, the rows newest first, a preview closure and row search; committing attaches the
/// chosen canvas to the NEXT message and prints the byte-exact notice.
#[tokio::test]
async fn bare_edit_opens_the_picker_and_attaches_the_choice() {
    let f = Fixture::new(
        vec![
            input("/edit"),
            commit(1), // the OLDER picture (rows are newest first)
            input("now make it blue"),
            Reply::Interrupted,
        ],
        two_generations(),
    );
    f.run().await;

    let s = surfaces(&f.ui);
    assert_eq!(s.len(), 1, "one surface: {s:?}");
    let p = &s[0].panels[0];
    assert_eq!(p.title, "Edit an image");
    assert_eq!(p.kind, PanelKind::Picker);
    assert_eq!(p.prompt, "Pick the image to edit, then type your prompt");
    assert!(p.has_preview, "the picker carries a preview closure");
    assert!(p.search, "the picker offers row search");
    // Newest first, each labelled by the prompt that made it (editpicker.go:90).
    assert_eq!(p.items.len(), 2);
    assert!(p.items[0].ends_with(" · add a hat"), "{:?}", p.items);
    assert!(p.items[1].ends_with(" · a cat"), "{:?}", p.items);
    // No session bundle → no on-disk path → no link (never a dead one).
    assert_eq!(p.details, vec![String::new(), String::new()]);

    assert!(
        printed(&f.ui).contains(&"Editing 20260725-093820-0.png — type your prompt.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );

    // The canvas rides the NEXT message — the /file rhythm.
    let sent = f.seen.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].content, "now make it blue");
    assert_eq!(sent[0].attachments.len(), 1);
    assert_eq!(sent[0].attachments[0].filename, "20260725-093820-0.png");
}

/// A cancelled picker attaches nothing and prints nothing (chat/run.go:476).
#[tokio::test]
async fn cancelled_picker_is_a_no_op() {
    let f = Fixture::new(
        vec![
            input("/edit"),
            Reply::Tabbed(TabbedResult {
                cancelled: true,
                ..TabbedResult::default()
            }),
            input("hello"),
            Reply::Interrupted,
        ],
        two_generations(),
    );
    f.run().await;

    assert!(
        !printed(&f.ui).iter().any(|l| l.starts_with("Editing ")),
        "{:?}",
        printed(&f.ui)
    );
    assert!(f.seen.sent()[0].attachments.is_empty());
}

/// Go: chat/run.go:462 — with nothing generated yet, bare `/edit` prints the notice and opens no
/// surface at all.
#[tokio::test]
async fn bare_edit_without_images_says_so() {
    let f = Fixture::new(vec![input("/edit"), Reply::Interrupted], Vec::new());
    f.run().await;

    assert!(
        printed(&f.ui).contains(&"Nothing to edit yet — generate an image first.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(surfaces(&f.ui).is_empty(), "no picker without candidates");
}

// ---------------------------------------------------------------------------
// /edit <prompt> — the fall-through send
// ---------------------------------------------------------------------------

/// Go: chat/run.go:486-492 — `/edit <prompt>` attaches the last generated images and FALLS
/// THROUGH to the message path: the provider sees the stripped prompt with the canvas, while the
/// transcript echoes the line the user typed.
#[tokio::test]
async fn edit_with_a_prompt_sends_the_canvas() {
    let f = Fixture::new(
        vec![input("/edit add a scarf"), Reply::Interrupted],
        two_generations(),
    );
    f.run().await;

    let sent = f.seen.sent();
    assert_eq!(sent.len(), 1, "the arm must send, not continue");
    assert_eq!(sent[0].content, "add a scarf", "the prompt is stripped");
    assert_eq!(sent[0].attachments.len(), 1);
    assert_eq!(
        sent[0].attachments[0].filename, "20260725-101500-0.png",
        "the canvas is the NEWEST generated image"
    );
    assert_eq!(
        echoed(&f.ui),
        vec!["/edit add a scarf".to_owned()],
        "the echo shows what was typed, not the expansion"
    );
    assert!(
        surfaces(&f.ui).is_empty(),
        "the prompt form skips the picker"
    );
}

/// Go: chat/run.go:487-490 — with no generated image the arm refuses and sends NOTHING.
#[tokio::test]
async fn edit_with_a_prompt_and_no_images_sends_nothing() {
    let f = Fixture::new(
        vec![input("/edit add a scarf"), Reply::Interrupted],
        vec![
            msg(Role::User, "hi", Vec::new()),
            msg(Role::Assistant, "hello", Vec::new()),
        ],
    );
    f.run().await;

    assert!(
        printed(&f.ui).contains(&"Nothing to edit yet — generate an image first.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(f.seen.sent().is_empty(), "nothing may reach the provider");
}

// ---------------------------------------------------------------------------
// /redo
// ---------------------------------------------------------------------------

/// Go: chat/run.go:499-516 — bare `/redo` re-sends the last USER turn's prompt AND its
/// attachments (the canvas that produced the rejected result, never the result), announcing the
/// byte-exact `Redoing: …` line.
#[tokio::test]
async fn redo_reuses_the_last_request() {
    let mut history = two_generations();
    // The /edit turn: the canvas rode the user message.
    history.push(msg(
        Role::User,
        "add a heart",
        vec![png("20260725-101500-0.png")],
    ));
    history.push(msg(Role::Assistant, "", vec![png("20260725-110000-0.png")]));

    let f = Fixture::new(vec![input("/redo"), Reply::Interrupted], history);
    f.run().await;

    assert!(
        printed(&f.ui).contains(&"Redoing: add a heart".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    let sent = f.seen.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].content, "add a heart");
    assert_eq!(
        sent[0].attachments[0].filename, "20260725-101500-0.png",
        "the canvas, never the rejected result"
    );
}

/// A reworded `/redo` keeps the canvas and swaps the prompt; the echo shows the typed line and
/// the notice truncates at 60 runes (chat/run.go:508-515).
#[tokio::test]
async fn redo_with_a_prompt_rewords_from_the_same_canvas() {
    let mut history = two_generations();
    history.push(msg(
        Role::User,
        "add a heart",
        vec![png("20260725-101500-0.png")],
    ));

    let long = "b".repeat(70);
    let f = Fixture::new(
        vec![input(&format!("/redo {long}")), Reply::Interrupted],
        history,
    );
    f.run().await;

    let want = format!("Redoing: {}…", "b".repeat(60));
    assert!(printed(&f.ui).contains(&want), "{:?}", printed(&f.ui));
    let sent = f.seen.sent();
    assert_eq!(sent[0].content, long);
    assert_eq!(sent[0].attachments[0].filename, "20260725-101500-0.png");
    assert_eq!(echoed(&f.ui), vec![format!("/redo {long}")]);
}

/// Go: chat/run.go:502 — with no user turn at all, `/redo` says so and sends nothing.
#[tokio::test]
async fn redo_without_a_user_turn_says_so() {
    let f = Fixture::new(
        vec![input("/redo"), Reply::Interrupted],
        vec![msg(Role::Assistant, "hi", Vec::new())],
    );
    f.run().await;

    assert!(
        printed(&f.ui).contains(&"Nothing to redo yet.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(f.seen.sent().is_empty());
}

/// Go: chat/run.go:510 — a last user turn with no text (an attachment-only message) has nothing
/// to redo, and the refusal names that rather than sending a blank prompt.
#[tokio::test]
async fn redo_with_a_blank_prompt_says_so() {
    let f = Fixture::new(
        vec![input("/redo"), Reply::Interrupted],
        vec![msg(Role::User, "   ", vec![png("a.png")])],
    );
    f.run().await;

    assert!(
        printed(&f.ui)
            .contains(&"Nothing to redo yet — the last turn carried no prompt.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(f.seen.sent().is_empty());
}
