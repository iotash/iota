//! Progressive image frames end to end over `ScriptedUi` (WP63; `T3_TEST_PLAN` §2): the `image`
//! widget rises on the first frame, its body carries the half-block thumbnail, and the final
//! picture morphs the widget in place — one separator for the whole generation, never two.
//!
//! The provider double is the `images` dialect's shape: no tool support (so the turn takes the
//! unary [`iota::provider::Provider::chat`] path) plus the progressive-frame capability, which is
//! what makes `run` call `chat_observed` instead. A double WITHOUT the capability proves the
//! headless/unary path is unchanged.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex, PoisonError};

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::error::ProviderError;
use iota::provider::model::{Attachment, Message};
use iota::provider::{ChatResult, ImagePartialProvider, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// The checked-in 2×2 fixture (top row red, bottom row blue) — the rasteriser's byte golden.
const RB_2X2_PNG: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");

/// The one rendered row a 2×2 picture produces.
const RB_ROW: &str =
    "\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[0m";

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// The `images` dialect's shape: one unary `chat`, no tools, and (optionally) the
/// progressive-frame capability that makes the run loop hand it an observer.
struct FakeImageProvider {
    /// Frames handed to the observer, in order, before the final result.
    frames: Vec<Vec<u8>>,
    /// The final generated image's bytes.
    final_png: Vec<u8>,
    /// Whether the double advertises `ImagePartialProvider`.
    observed: bool,
    /// Whether `chat_observed` (rather than plain `chat`) ran.
    took_observed_path: Arc<Mutex<bool>>,
}

impl FakeImageProvider {
    fn new(frames: Vec<Vec<u8>>, final_png: Vec<u8>) -> Self {
        Self {
            frames,
            final_png,
            observed: true,
            took_observed_path: Arc::new(Mutex::new(false)),
        }
    }

    /// A backend that ignores the streaming flags: no capability, no observer, no frames.
    fn unary(final_png: Vec<u8>) -> Self {
        Self {
            observed: false,
            ..Self::new(Vec::new(), final_png)
        }
    }

    fn result(&self) -> ChatResult {
        ChatResult {
            text: String::new(),
            images: vec![Attachment {
                filename: "image-1.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: self.final_png.clone(),
            }],
            ..ChatResult::default()
        }
    }
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
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(std::future::ready(Ok(self.result())))
    }

    fn as_image_partial_provider(&self) -> Option<&dyn ImagePartialProvider> {
        self.observed.then_some(self)
    }
}

impl ImagePartialProvider for FakeImageProvider {
    fn chat_observed<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
        on_partial: &'a mut (dyn FnMut(&[u8]) + Send),
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        *self
            .took_observed_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        for f in &self.frames {
            on_partial(f);
        }
        Box::pin(std::future::ready(Ok(self.result())))
    }
}

// ---------------------------------------------------------------------------
// the harness
// ---------------------------------------------------------------------------

fn no_mcp() -> McpHooks {
    McpHooks {
        servers: None,
        events: None,
    }
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
    })
}

/// One scripted turn: `"draw"` then EOF, against a session bundle under `tmp`.
async fn run_one(
    p: FakeImageProvider,
) -> (Arc<ScriptedUi>, tempfile::TempDir, Arc<Mutex<bool>>, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let writer: SessionWriter = store
        .create(ProviderKind::Images, "gpt-image-1", None, "", "", false)
        .expect("create writer");
    let images_dir = writer.images_path().to_string_lossy().into_owned();
    let ui = ScriptedUi::new(vec![input("draw"), Reply::Interrupted]);
    let took = Arc::clone(&p.took_observed_path);
    iota::repl::run(RunParams {
        ui: Arc::clone(&ui) as Arc<dyn Ui>,
        provider: Box::new(p),
        title_provider: None,
        system: String::new(),
        system_interactive: false,
        imported_history: Vec::new(),
        dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
        delegator: None,
        mcp: no_mcp(),
        session: SessionCtx {
            writer: Some(writer),
            store,
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
    (ui, tmp, took, images_dir)
}

/// The facade events from the user block onward — the banner and status noise dropped.
fn after_user_block(ui: &ScriptedUi) -> Vec<UiEvent> {
    let all = ui.events();
    let start = all
        .iter()
        .position(|e| matches!(e, UiEvent::UserBlock(_)))
        .expect("the turn's user block");
    all[start..]
        .iter()
        .filter(|e| {
            !matches!(
                e,
                UiEvent::Status(_)
                    | UiEvent::Title(_)
                    | UiEvent::Commands(_)
                    | UiEvent::Progress(_)
                    | UiEvent::Notify(_)
                    | UiEvent::DarkBackground(_)
                    | UiEvent::ScopePush
                    | UiEvent::ScopePop
                    | UiEvent::ReadInput
                    | UiEvent::Close
                    | UiEvent::Busy(_)
                    | UiEvent::BusyOff
                    | UiEvent::BusyDetail(_)
            )
        })
        .cloned()
        .collect()
}

/// Every `Print`ed line, flattened and stripped of SGR.
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

// ---------------------------------------------------------------------------
// the flows
// ---------------------------------------------------------------------------

/// L3 (`T3_TEST_PLAN` §2, chat/images.go:187-208 + transcript.go:335-383): a streaming image
/// turn raises the `image` widget on the FIRST frame, repaints its body per frame, then morphs
/// it into the committed picture — one separator, one `ClosePreview`, no second separator.
#[tokio::test]
async fn progressive_frames_raise_the_widget_and_morph_into_the_picture() {
    let (ui, _tmp, took, dir) = run_one(FakeImageProvider::new(
        vec![RB_2X2_PNG.to_vec(), RB_2X2_PNG.to_vec()],
        RB_2X2_PNG.to_vec(),
    ))
    .await;
    assert!(
        *took.lock().unwrap_or_else(PoisonError::into_inner),
        "the capability must route the turn through chat_observed"
    );

    let events = after_user_block(&ui);
    let want_body = vec![RB_ROW.to_owned()];
    let saved = std::fs::read_dir(&dir)
        .expect("images dir")
        .map(|e| e.expect("entry").path())
        .next()
        .expect("one saved image");
    let saved = saved.to_string_lossy().into_owned();

    // The turn tears down BEFORE the final image is collected (`sink.done()` → `reset_turn()`
    // → `collect_images`, chat/run.go:1069-1094), so the widget is already gone by then and
    // `reset_turn` hands its separator on: the picture reuses it rather than paying a second
    // one, which is the whole point of the morph.
    assert_eq!(
        events,
        vec![
            UiEvent::UserBlock("draw".to_owned()),
            UiEvent::StreamStart,
            UiEvent::Print(vec![String::new()]), // the group's ONE separator, paid at the raise
            UiEvent::CallPreview("image".to_owned()),
            UiEvent::CallBody(want_body.clone()),
            UiEvent::CallBody(want_body), // each frame replaces the last, wholesale
            UiEvent::Done,                // the widget is dropped, its separator reclaimed…
            UiEvent::Print(vec![format!("  {RB_ROW}")]), // …so the picture pays none
            UiEvent::Print(vec![format!(
                "  \x1b[2m🖼 saved: \x1b]8;;file://{saved}\x1b\\{saved}\x1b]8;;\x1b\\\x1b[0m"
            )]),
        ],
    );
}

/// L3: a final image the rasteriser cannot decode still SAVED — the caption line with the
/// reason appended is the fallback rendering (chat/images.go:143), and it is a notice, not a
/// picture block.
#[tokio::test]
async fn an_undecodable_final_image_falls_back_to_the_caption_notice() {
    let (ui, _tmp, _took, _dir) = run_one(FakeImageProvider::new(Vec::new(), vec![1, 2, 3])).await;
    let lines = printed(&ui);
    let caption = lines
        .iter()
        .find(|l| l.contains("🖼 saved:"))
        .expect("a caption line");
    assert!(
        caption.ends_with(')') && caption.contains(" ("),
        "the decode reason must be appended in parens: {caption:?}"
    );
    assert!(
        !caption.contains("decode image: "),
        "the `decode image: ` prefix is stripped: {caption:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains('▀')),
        "nothing was drawn: {lines:?}"
    );
}

/// L3: a backend WITHOUT the capability keeps the unary body — no observer, no widget, and the
/// picture still commits (the `-m` shape, `T3_DESIGN` §3.2).
#[tokio::test]
async fn a_provider_without_the_capability_takes_the_unary_path() {
    let (ui, _tmp, took, _dir) = run_one(FakeImageProvider::unary(RB_2X2_PNG.to_vec())).await;
    assert!(
        !*took.lock().unwrap_or_else(PoisonError::into_inner),
        "no capability, no observed call"
    );
    let events = after_user_block(&ui);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, UiEvent::CallBody(_) | UiEvent::CallPreview(_))),
        "no widget without frames: {events:?}"
    );
    assert!(
        printed(&ui).iter().any(|l| l.contains('▀')),
        "the picture still commits"
    );
}
