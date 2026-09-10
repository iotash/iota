//! The host presenter's turn anchors end to end (`chat/run.go:384,1008-1107,1513-1527`,
//! `chat/approval.go:58-69`) over a `RecordingHost` and `iota::testing::ScriptedUi`.
//!
//! Go could not test this: `chat.Run` needed a terminal, so `internal/host` was only unit-tested
//! against fakes and the anchors were never exercised as a sequence. Here the whole loop runs, so
//! what is asserted is the ORDER the states and the pings actually reach a host — including the
//! per-capability laws (`notify: false` silences pings but not states) and the two texts a turn
//! can end with (`notify_digest(reply)` and `"Image ready"`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, PoisonError};

use iota::BoxFuture;
use iota::host::{Caps, Event, Kind, Presenter, State};
use iota::llm::reqlog::RequestLog;
use iota::provider::error::ProviderError;
use iota::provider::model::{Attachment, Message};
use iota::provider::{ChatResult, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::testing::{
    FakeToolProvider, RecordingHost, Reply, ScriptedUi, StaticDispatcher, UiEvent,
};
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// What the one scripted turn answers with.
enum Answer {
    /// A plain text reply.
    Text(String),
    /// An image-only reply: no text, one attachment.
    Image,
    /// A failure retrying cannot fix (so the loop reports it on the first attempt).
    Fail(&'static str),
    /// The provider cancels the turn's token and fails — Go's `errInterrupted` shape.
    Interrupt,
}

/// A capability-less provider (no `ToolProvider`) answering exactly once.
struct OneShot(Answer);

impl Provider for OneShot {
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
        cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            match &self.0 {
                Answer::Text(t) => Ok(ChatResult {
                    text: t.clone(),
                    ..ChatResult::default()
                }),
                Answer::Image => Ok(ChatResult {
                    text: String::new(),
                    images: vec![Attachment {
                        filename: "canvas.png".to_owned(),
                        mime_type: "image/png".to_owned(),
                        data: b"not-a-real-png".to_vec(),
                    }],
                    ..ChatResult::default()
                }),
                Answer::Fail(msg) => Err(ProviderError::permanent_msg(*msg)),
                Answer::Interrupt => {
                    // The turn's own token: cancelling it is what makes `stream_turn`
                    // classify the failure as the user's interrupt (turn.rs:280).
                    cancel.cancel();
                    Err(ProviderError::other("dropped"))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// the fixture
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

/// A tabbed commit of one panel (the approval gate's `select`).
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

/// The one host the presenter fans out to, plus the scripted facade and a temp store.
struct Fixture {
    ui: Arc<ScriptedUi>,
    host: RecordingHost,
    _tmp: tempfile::TempDir,
    store: SessionStore,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            host: RecordingHost {
                caps: Caps {
                    state: true,
                    notify: true,
                    background: false,
                    close: true,
                },
                ..RecordingHost::new("recorder")
            },
            _tmp: tmp,
            store,
        }
    }

    fn writer(&self) -> SessionWriter {
        self.store
            .create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
            .expect("create writer")
    }

    /// A presenter over the fixture's host alone (no ANSI fallback: the facade events are
    /// asserted separately, and one host keeps the per-capability laws visible).
    fn presenter(&self, notify: bool) -> Arc<Presenter> {
        Arc::new(Presenter::with_hosts(
            vec![Box::new(RecordingHost {
                caps: self.host.caps,
                name: self.host.name,
                states: Arc::clone(&self.host.states),
                events: Arc::clone(&self.host.events),
                closed: Arc::clone(&self.host.closed),
                dark: self.host.dark,
            })],
            notify,
        ))
    }

    fn params(
        &self,
        provider: Box<dyn Provider>,
        dispatch: Arc<dyn Dispatcher>,
        notify: bool,
    ) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider,
            title_provider: None,
            system: String::new(),
            system_interactive: false,
            imported_history: Vec::new(),
            dispatch,
            delegator: None,
            mcp: no_mcp(),
            session: SessionCtx {
                writer: Some(self.writer()),
                store: self.store.clone(),
                new_session: None,
                scope: None,
            },
            context_window: 0,
            agent: iota::chat::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: self.presenter(notify),
        }
    }

    fn states(&self) -> Vec<State> {
        self.host
            .states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn events(&self) -> Vec<Event> {
        self.host
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn pings(&self) -> Vec<(Kind, String)> {
        self.events()
            .into_iter()
            .map(|e| (e.kind, e.text))
            .collect()
    }

    fn closed(&self) -> Vec<&'static str> {
        self.host
            .closed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Every busy label the loop raised, in order (the upload phase's row).
fn busy_labels(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Busy(l) => Some(l),
            _ => None,
        })
        .collect()
}

fn plain(answer: Answer) -> Box<dyn Provider> {
    Box::new(OneShot(answer))
}

fn no_tools() -> Arc<dyn Dispatcher> {
    Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>
}

// ---------------------------------------------------------------------------
// the anchors
// ---------------------------------------------------------------------------

/// Go: chat/run.go:384,1010,1100-1107 — one successful turn walks Idle → Busy → Idle, and the
/// ping carries a DIGEST of the answer, never a fixed phrase. The leading `SetState(Idle)` the
/// input dispatch re-asserts is deduplicated away (the presenter starts at Idle), so a host that
/// pays per update (cmux spawns a process) sees exactly two.
#[tokio::test]
async fn a_successful_turn_reports_busy_then_idle_with_a_digest() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(
        plain(Answer::Text("The fix landed\n\nmore".to_owned())),
        no_tools(),
        true,
    ))
    .await
    .expect("clean exit");

    assert_eq!(f.states(), vec![State::Busy, State::Idle]);
    assert_eq!(f.pings(), vec![(Kind::Done, "The fix landed".to_owned())]);
    // Go's `defer pres.Close()` runs before the facade goes down.
    assert_eq!(f.closed(), vec!["recorder"]);
    // The turn's upload phase narrated the wait, and nothing else (a small body).
    assert!(
        busy_labels(&f.ui).contains(&"Waiting for the model".to_owned()),
        "the round never raised the waiting phase: {:?}",
        busy_labels(&f.ui)
    );
}

/// Go: chat/run.go:1103-1106 — an image-only reply pings `"Image ready"`, decided AFTER
/// `collectImages` attached the pictures (an empty reply with no attachments is still a digest).
#[tokio::test]
async fn an_image_only_reply_pings_image_ready() {
    let f = Fixture::new(vec![input("draw me a cat"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Image), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Busy, State::Idle]);
    assert_eq!(f.pings(), vec![(Kind::Done, "Image ready".to_owned())]);
}

/// Go: chat/run.go:1078-1089 — a failed turn leaves the host in Error ("it stands until the user
/// acts") and pings the error's headline, BEFORE the red block lands.
#[tokio::test]
async fn a_failed_turn_reports_error_and_pings_the_headline() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Fail("the model is gone")), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Busy, State::Error]);
    let pings = f.pings();
    assert_eq!(pings.len(), 1, "one ping per failed turn: {pings:?}");
    assert_eq!(pings[0].0, Kind::Failed);
    assert!(
        !pings[0].1.is_empty(),
        "the ping must carry the error headline"
    );
}

/// Go: chat/run.go:1070-1076 — the user did the interrupting, so the host goes back to Idle and
/// NOTHING pings: a bell for something they just did themselves is noise.
#[tokio::test]
async fn an_interrupted_turn_is_silent() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Interrupt), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Busy, State::Idle]);
    assert!(
        f.events().is_empty(),
        "an interrupt pinged: {:?}",
        f.events()
    );
}

/// Go: `internal/host/host_test.go:61` `TestPresenterNotifySwitch`, end to end — config
/// `notify: false` silences every ping while leaving the state channel untouched.
#[tokio::test]
async fn notify_false_silences_pings_but_not_states() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(
        plain(Answer::Text("The fix landed".to_owned())),
        no_tools(),
        false,
    ))
    .await
    .expect("clean exit");

    assert_eq!(f.states(), vec![State::Busy, State::Idle]);
    assert!(
        f.events().is_empty(),
        "notify: false still pinged: {:?}",
        f.events()
    );
}

/// Go: `chat/approval.go:58-69` — the approval gate is the loop's `NeedsInput` anchor: the host
/// hears `NeedsInput` with `"<label> wants to modify files"` before the prompt opens and `Busy`
/// again once it resolves, either way. The full sequence a tool turn walks is
/// `Idle → Busy → NeedsInput → Busy → Idle` with two pings.
#[tokio::test]
async fn the_approval_gate_walks_needs_input_and_back() {
    let f = Fixture::new(vec![
        input("write the file"),
        commit(0),                 // the gate's "Allow once"
        Reply::Queued(Vec::new()), // the round boundary's steering drain
        Reply::Interrupted,
    ]);
    let dispatch =
        Arc::new(StaticDispatcher::new(&["noop"]).with_approval(&["noop"])) as Arc<dyn Dispatcher>;
    iota::repl::run(f.params(
        Box::new(FakeToolProvider::reporting(1, None)),
        dispatch,
        true,
    ))
    .await
    .expect("clean exit");

    assert_eq!(
        f.states(),
        vec![State::Busy, State::NeedsInput, State::Busy, State::Idle]
    );
    assert_eq!(
        f.pings(),
        vec![
            (Kind::NeedsInput, "noop wants to modify files".to_owned()),
            (Kind::Done, "final answer".to_owned()),
        ]
    );
}
