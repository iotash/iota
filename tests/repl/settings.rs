//! WP54 L3 suite for the full `/model` questionnaire (chat/run.go:513-716, T-12).
//!
//! The row builders and the capability assembly are unit-tested beside their source
//! (`commands/settings.rs`, `systemtab.rs`); what this file drives is the whole command
//! through the PUBLIC entry point — `iota::repl::run` over `iota::testing::ScriptedUi`
//! — so the three laws that only exist end to end can be asserted:
//!
//! 1. the surface OPENS with a tab per capability, each cursor on the provider's current
//!    value (the untouched-tab-is-a-no-op law, seen from outside);
//! 2. a commit applies each moved knob to the provider AND to the session bundle, with one
//!    dim notice each, and prints `"No changes."` when nothing moved;
//! 3. what `/model` wrote into `meta.json` is exactly what
//!    `iota::session::apply_session_tuning` replays on resume — the round trip. That
//!    function's own subtests live in `iota-session/tests/tuning.rs`; this file VERIFIES
//!    against it rather than re-implementing the replay.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ImageGenTunable;
use iota::provider::error::ProviderError;
use iota::provider::model::Message;
use iota::provider::{
    ChatResult, Effort, ImageEditJsonTunable, ImageGenOptions, ImageGenParams, ImageTunable,
    Provider, ProviderKind, Tunable,
};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{Overrides, SessionMeta, SessionStore, SessionWriter, apply_session_tuning};
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the double
// ---------------------------------------------------------------------------

/// A provider whose optional capabilities are switched on per scenario, so each tab can be
/// traced to exactly the probe that offers it.
#[derive(Clone, Default)]
struct Knobs {
    kind: Option<ProviderKind>,
    model: String,
    models: Vec<String>,
    usage: bool,
    tunable: bool,
    temperature: Option<f64>,
    effort: Option<Effort>,
    image: Option<bool>,
    image_gen: Option<(ImageGenOptions, ImageGenParams)>,
    json_edits: Option<bool>,
}

impl Knobs {
    fn text(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            models: vec![model.to_owned(), "b-model".to_owned()],
            usage: true,
            tunable: true,
            ..Self::default()
        }
    }
}

impl Provider for Knobs {
    fn kind(&self) -> ProviderKind {
        self.kind.unwrap_or(ProviderKind::OpenAi)
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn set_model(&mut self, model: String) {
        self.model = model;
    }
    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(std::future::ready(Ok(self.models.clone())))
    }
    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(std::future::ready(Ok(ChatResult::default())))
    }
    fn reports_usage(&self) -> bool {
        self.usage
    }
    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        self.tunable.then_some(self as &mut dyn Tunable)
    }
    fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
        self.image
            .is_some()
            .then_some(self as &mut dyn ImageTunable)
    }
    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        self.image_gen
            .is_some()
            .then_some(self as &mut dyn ImageGenTunable)
    }
    fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
        self.json_edits
            .is_some()
            .then_some(self as &mut dyn ImageEditJsonTunable)
    }
}

impl Tunable for Knobs {
    fn set_temperature(&mut self, t: Option<f64>) {
        self.temperature = t;
    }
    fn temperature(&self) -> Option<f64> {
        self.temperature
    }
    fn set_effort(&mut self, e: Option<Effort>) {
        self.effort = e;
    }
    fn effort(&self) -> Option<Effort> {
        self.effort
    }
}

impl ImageTunable for Knobs {
    fn set_image_output(&mut self, on: bool) {
        self.image = Some(on);
    }
    fn image_output(&self) -> bool {
        self.image.unwrap_or(false)
    }
}

impl ImageGenTunable for Knobs {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        if let Some(slot) = self.image_gen.as_mut() {
            slot.1 = p;
        }
    }
    fn image_gen_params(&self) -> &ImageGenParams {
        static EMPTY: std::sync::OnceLock<ImageGenParams> = std::sync::OnceLock::new();
        self.image_gen
            .as_ref()
            .map_or_else(|| EMPTY.get_or_init(ImageGenParams::default), |g| &g.1)
    }
    fn image_gen_options(&self) -> ImageGenOptions {
        self.image_gen
            .as_ref()
            .map_or_else(ImageGenOptions::default, |g| g.0.clone())
    }
}

impl ImageEditJsonTunable for Knobs {
    fn set_json_edits(&mut self, on: bool) {
        self.json_edits = Some(on);
    }
    fn json_edits(&self) -> bool {
        self.json_edits.unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

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

    /// A MATERIALISED bundle: a pending one keeps its meta in memory until the first
    /// append (chat/session.go:363-378), and this suite reads `meta.json` off disk.
    fn writer(&self) -> (SessionWriter, PathBuf) {
        let mut w = self
            .store
            .create(ProviderKind::OpenAi, "gpt-test", None, "", "", false)
            .expect("create writer");
        w.append_messages(&[Message::user("earlier")])
            .expect("materialise");
        let dir = w.dir().to_path_buf();
        (w, dir)
    }

    fn params(&self, provider: Knobs, writer: SessionWriter, history: Vec<Message>) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
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
                writer: Some(writer),
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

/// A commit of the whole questionnaire: one [`PanelResult`] per opened tab.
fn commit(panels: Vec<PanelResult>) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
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

fn titles(s: &TabbedSummary) -> Vec<String> {
    s.panels.iter().map(|p| p.title.clone()).collect()
}

/// A commit that repeats every tab's opening state — the "user pressed Enter without
/// touching anything" reply, which must be a no-op by construction. Reconstructed from
/// what the surface OPENED with, sliders and switches included.
fn unchanged(s: &TabbedSummary) -> Vec<PanelResult> {
    s.panels
        .iter()
        .map(|p| PanelResult {
            cursor: p.cursor,
            text: p.text.clone(),
            value: p.value,
            on: p.on,
            ..PanelResult::default()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the surface's shape
// ---------------------------------------------------------------------------

/// Go: chat/run.go:544-630 — the full questionnaire opens with a tab per capability, in
/// Go's fixed order, each on the provider's current value. `enter_advances` stays FALSE:
/// Enter on any tab commits everything (only the ask wizard advances).
#[tokio::test]
async fn model_opens_a_tab_per_capability() {
    let f = Fixture::new(vec![
        input("/model"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        Reply::Interrupted,
    ]);
    let (writer, _dir) = f.writer();
    let provider = Knobs {
        effort: Some(Effort::High),
        temperature: Some(0.7),
        ..Knobs::text("a-model")
    };
    let history = vec![Message::system("You are terse.\nBe brief.")];
    iota::repl::run(f.params(provider, writer, history))
        .await
        .expect("exit");

    let s = &surfaces(&f.ui)[0];
    assert_eq!(
        titles(s),
        ["Model", "Context", "Effort", "Temperature", "System"]
    );
    assert!(
        !s.enter_advances,
        "/model commits from anywhere — only the ask wizard advances"
    );

    let p = &s.panels;
    assert_eq!(p[0].kind, PanelKind::List);
    assert!(p[0].search, "the model picker is searchable");
    assert_eq!(p[0].items, ["a-model (current)", "b-model"]);

    assert_eq!(p[1].kind, PanelKind::List);
    assert_eq!(p[1].items[p[1].cursor], "128k (current)");

    assert_eq!(p[2].kind, PanelKind::List);
    assert_eq!(p[2].items[p[2].cursor], "high (current)");

    // The Temperature slider OPENS on the provider's live value, over the dialect's own
    // range: 0.0-2.0 in 0.1 steps for everything but Anthropic (pinned at 1.0 beside
    // `temperature_rows` itself).
    assert_eq!(p[3].kind, PanelKind::Slider);
    assert_eq!(p[3].value, Some(0.7), "the slider opens on the live value");
    assert_eq!(p[3].range, (0.0, 2.0, 0.1));

    // The System tab is a read-only View that wraps, carrying the prompt AS SENT.
    assert_eq!(p[4].kind, PanelKind::View);
    assert!(p[4].wrap);
    assert_eq!(p[4].line_count, 2);
    assert_eq!(p[4].prompt, "System prompt in effect (read-only)");

    // A cancelled surface changes nothing and says nothing.
    assert!(!printed(&f.ui).iter().any(|l| l.contains("No changes.")));
}

/// A provider with no optional capability shows the Model tab alone, and a chat with no
/// system prompt gets no System tab: tabs are offered, never padded in.
#[tokio::test]
async fn model_shows_only_the_tabs_the_provider_has() {
    let f = Fixture::new(vec![
        input("/model"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        Reply::Interrupted,
    ]);
    let (writer, _dir) = f.writer();
    let provider = Knobs {
        model: "a-model".to_owned(),
        models: vec!["a-model".to_owned()],
        ..Knobs::default()
    };
    iota::repl::run(f.params(provider, writer, Vec::new()))
        .await
        .expect("exit");
    assert_eq!(titles(&surfaces(&f.ui)[0]), ["Model"]);
}

// ---------------------------------------------------------------------------
// the commit
// ---------------------------------------------------------------------------

/// Go: chat/run.go:637-712 — every knob that MOVED applies to the provider, persists into
/// the bundle, and prints its own dim notice, in tab order.
#[tokio::test]
async fn model_commits_every_moved_knob_with_its_own_notice() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            // Model: row 1 = "b-model".
            PanelResult {
                cursor: 1,
                ..PanelResult::default()
            },
            // Context: row 3 = 200k (presets are 8k/32k/128k/200k/256k/1m).
            PanelResult {
                cursor: 3,
                ..PanelResult::default()
            },
            // Effort: row 4 = "xhigh" (levels are ""/low/medium/high/xhigh/max).
            PanelResult {
                cursor: 4,
                ..PanelResult::default()
            },
            // Temperature.
            PanelResult {
                value: Some(0.3),
                ..PanelResult::default()
            },
            // System: read-only; whatever it returns is ignored.
            PanelResult::default(),
        ]),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    let provider = Knobs {
        effort: Some(Effort::High),
        temperature: Some(0.7),
        ..Knobs::text("a-model")
    };
    let history = vec![Message::system("You are terse.")];
    iota::repl::run(f.params(provider, writer, history))
        .await
        .expect("exit");

    let lines = printed(&f.ui);
    let notices: Vec<&String> = lines
        .iter()
        .filter(|l| {
            l.starts_with("Model switched")
                || l.starts_with("Context window:")
                || l.starts_with("Effort:")
                || l.starts_with("Temperature:")
        })
        .collect();
    // The Context notice carries `budget.status()`, whose USED half is whatever the meter
    // counts (the system prompt's estimate), so only the window half — the thing this tab changed — is pinned here. The figure
    // itself is `TestContextBudgetStatus`'s (`meter.rs` / WP53's `tests/tokens.rs`).
    assert_eq!(notices.len(), 4, "one notice per moved knob: {notices:?}");
    assert_eq!(notices[0], "Model switched to b-model");
    assert!(
        notices[1].starts_with("Context window: ") && notices[1].ends_with(" / 200k (0%)"),
        "{:?}",
        notices[1]
    );
    assert_eq!(notices[2], "Effort: xhigh");
    assert_eq!(notices[3], "Temperature: 0.3");
    assert!(
        !lines.contains(&"No changes.".to_owned()),
        "a commit that moved four knobs is not 'No changes.'"
    );

    // …and every one of them landed in the bundle, through the one `update_meta` site.
    let meta = SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.model, "b-model");
    assert_eq!(meta.context_window, 200_000);
    assert_eq!(meta.effort, "xhigh");
    assert_eq!(meta.temperature, Some(0.3));
}

/// Committing the questionnaire untouched moves nothing: the current value IS the cursor
/// row of every tab, so an accidental Enter cannot reset a session's tuning.
#[tokio::test]
async fn model_untouched_questionnaire_reports_no_changes() {
    // Open once to learn the opening cursors, then replay them as the commit.
    let opening = {
        let f = Fixture::new(vec![
            input("/model"),
            Reply::Tabbed(TabbedResult {
                cancelled: true,
                ..TabbedResult::default()
            }),
            Reply::Interrupted,
        ]);
        let (writer, _dir) = f.writer();
        let provider = Knobs {
            effort: Some(Effort::High),
            temperature: Some(0.7),
            ..Knobs::text("a-model")
        };
        iota::repl::run(f.params(provider, writer, vec![Message::system("sys")]))
            .await
            .expect("exit");
        surfaces(&f.ui).remove(0)
    };

    let f = Fixture::new(vec![
        input("/model"),
        // Tab 3 is the Temperature slider: its untouched reply is the provider's own
        // figure, which the shape summary does not carry.
        commit(unchanged(&opening)),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    let provider = Knobs {
        effort: Some(Effort::High),
        temperature: Some(0.7),
        ..Knobs::text("a-model")
    };
    iota::repl::run(f.params(provider, writer, vec![Message::system("sys")]))
        .await
        .expect("exit");

    assert!(
        printed(&f.ui).contains(&"No changes.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    // Nothing was written: the bundle keeps the tuning it had.
    let meta = SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.effort, "", "an untouched Effort tab must not persist");
    assert_eq!(meta.temperature, None);
}

/// A dedicated image provider's knobs commit TOGETHER — one `"Image params:"` notice for
/// the three of them — and the JSON-edits switch reports its wire format in words.
#[tokio::test]
async fn model_commits_the_image_knobs_together() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            PanelResult::default(), // Model
            // Aspect: rows are ""/1:1/3:2 → row 2 = "3:2".
            PanelResult {
                cursor: 2,
                ..PanelResult::default()
            },
            // Size: rows are ""/1K/2K → row 1 = "1K".
            PanelResult {
                cursor: 1,
                ..PanelResult::default()
            },
            // Negative prompt (trimmed).
            PanelResult {
                text: "  blurry  ".to_owned(),
                ..PanelResult::default()
            },
            // JSON edits.
            PanelResult {
                on: true,
                ..PanelResult::default()
            },
        ]),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    let provider = Knobs {
        kind: Some(ProviderKind::Imagen),
        model: "imagen-4".to_owned(),
        models: vec!["imagen-4".to_owned()],
        image_gen: Some((
            ImageGenOptions {
                aspect_ratios: vec!["1:1", "3:2"],
                image_sizes: vec!["1K", "2K"],
                negative_prompt: true,
            },
            ImageGenParams::default(),
        )),
        json_edits: Some(false),
        ..Knobs::default()
    };
    // A system prompt in history must NOT produce a System tab for an image provider.
    let history = vec![Message::system("You are terse.")];
    iota::repl::run(f.params(provider, writer, history))
        .await
        .expect("exit");

    let s = &surfaces(&f.ui)[0];
    assert_eq!(
        titles(s),
        ["Model", "Aspect", "Size", "Negative", "JSON edits"]
    );
    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"Image params: aspect 3:2 · size 1K · negative blurry".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Image edits sent as: JSON".to_owned()),
        "{lines:?}"
    );

    let meta = SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.aspect_ratio, "3:2");
    assert_eq!(meta.image_size, "1K");
    assert_eq!(meta.negative_prompt, "blurry");
    assert!(meta.json_edits);
}

/// The image-output switch is its own knob with its own on/off notice.
#[tokio::test]
async fn model_commits_the_image_output_switch() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            PanelResult::default(),
            PanelResult {
                on: true,
                ..PanelResult::default()
            },
        ]),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    let provider = Knobs {
        model: "gemini".to_owned(),
        models: vec!["gemini".to_owned()],
        image: Some(false),
        ..Knobs::default()
    };
    iota::repl::run(f.params(provider, writer, Vec::new()))
        .await
        .expect("exit");

    assert_eq!(titles(&surfaces(&f.ui)[0]), ["Model", "Image"]);
    assert!(printed(&f.ui).contains(&"Image generation: on".to_owned()));
    assert!(SessionMeta::read(&dir).expect("meta").image);
}

// ---------------------------------------------------------------------------
// the round trip
// ---------------------------------------------------------------------------

/// The persistence law, end to end: what `/model` writes into `meta.json` is exactly what
/// a resume replays. `apply_session_tuning` is `iota-session`'s (its own subtests live in
/// `iota-session/tests/tuning.rs`); this test VERIFIES the questionnaire against it rather
/// than restating the replay rules — the two halves cannot drift if they meet on one file.
#[tokio::test]
async fn session_meta_tuning_round_trip() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            PanelResult {
                cursor: 1,
                ..PanelResult::default()
            }, // Model  → b-model
            PanelResult {
                cursor: 4,
                ..PanelResult::default()
            }, // Context → 256k
            PanelResult {
                cursor: 2,
                ..PanelResult::default()
            }, // Effort → medium
            PanelResult {
                value: Some(1.2),
                ..PanelResult::default()
            }, // Temperature
        ]),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    iota::repl::run(f.params(Knobs::text("a-model"), writer, Vec::new()))
        .await
        .expect("exit");

    let meta = SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.model, "b-model");
    assert_eq!(meta.context_window, 256_000);
    assert_eq!(meta.effort, "medium");
    assert_eq!(meta.temperature, Some(1.2));

    // Resume: a FRESH provider of the same type replays the recorded tuning.
    let mut fresh = Knobs::text("a-model");
    let mut warnings = Vec::new();
    let window = apply_session_tuning(
        &meta,
        &mut fresh,
        ProviderKind::OpenAi,
        &Overrides::default(),
        &mut |w| warnings.push(w),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(window, Some(256_000));
    assert_eq!(fresh.effort, Some(Effort::Medium));
    assert_eq!(fresh.temperature, Some(1.2));
    // The model is replayed by the session loader, not by `apply_session_tuning`; the
    // bundle carries it, which is what the resume path reads.
    assert_eq!(meta.model, "b-model");
}

/// Picking the `"default"` row of the Effort tab CLEARS the parameter rather than leaving
/// the old level in place — `""` means "omit it from requests", and the bundle records
/// the same emptiness so a resume does not resurrect it.
#[tokio::test]
async fn model_effort_default_row_clears_the_parameter() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            PanelResult::default(),
            PanelResult::default(),
            PanelResult::default(), // Effort row 0 = "" (default)
            PanelResult::default(),
        ]),
        Reply::Interrupted,
    ]);
    let (writer, dir) = f.writer();
    let provider = Knobs {
        effort: Some(Effort::High),
        ..Knobs::text("a-model")
    };
    iota::repl::run(f.params(provider, writer, Vec::new()))
        .await
        .expect("exit");

    assert!(printed(&f.ui).contains(&"Effort: default".to_owned()));
    assert_eq!(SessionMeta::read(&dir).expect("meta").effort, "");
}

/// The temperature knob's `None` state is a VALUE, not a missing one: sliding back to
/// "default" clears the parameter and says so (the `float_ptr_equal` comparison).
#[tokio::test]
async fn model_temperature_slider_returns_to_default() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(vec![
            PanelResult::default(),
            PanelResult::default(),
            PanelResult::default(),
            PanelResult {
                value: None,
                ..PanelResult::default()
            },
        ]),
        Reply::Interrupted,
    ]);
    let (writer, _dir) = f.writer();
    let provider = Knobs {
        temperature: Some(0.7),
        ..Knobs::text("a-model")
    };
    iota::repl::run(f.params(provider, writer, Vec::new()))
        .await
        .expect("exit");
    assert!(printed(&f.ui).contains(&"Temperature: default".to_owned()));
}
