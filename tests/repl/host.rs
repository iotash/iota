//! The host presenter's turn anchors end to end (`chat/run.go:384,1008-1107,1513-1527`,
//! `chat/approval.go:58-69`) over a `RecordingHost` and `iota::testing::ScriptedUi`.
//!
//! Go could not test this: `chat.Run` needed a terminal, so `internal/host` was only unit-tested
//! against fakes and the anchors were never exercised as a sequence. Here the whole loop runs, so
//! what is asserted is the ORDER the states and the pings actually reach a host — including the
//! per-capability laws (`notify: false` silences pings but not states) and the two texts a turn
//! can end with (`notify_digest(reply)` and `"Image ready"`). The last two tests run the same
//! loop over the REAL herdr host and a stand-in socket (`tests/common/herdr_mock.rs`).

use std::sync::{Arc, PoisonError};

use iota::host::{Caps, Event, Kind, Presenter, State};
use iota::llm::reqlog::RequestLog;
use iota::provider::model::Attachment;
use iota::provider::{Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::testing::{
    FakeProvider, Interrupt, RecordingHost, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent,
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

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

fn no_mcp() -> McpHooks {
    McpHooks::default()
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
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
                    session: true,
                    environment: false,
                },
                ..RecordingHost::new("recorder")
            },
            _tmp: tmp,
            store,
        }
    }

    fn writer(&self) -> SessionWriter {
        self.store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
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
                sessions: Arc::clone(&self.host.sessions),
                closed: Arc::clone(&self.host.closed),
                dark: self.host.dark,
                env: self.host.env.clone(),
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
            harness: String::new(),
            imported_history: Vec::new(),
            dispatch,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: no_mcp(),
            session: SessionCtx {
                writer: Some(self.writer()),
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

/// Every line the loop printed, bare — the banner card's rows first.
fn printed(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Print(lines) => Some(lines),
            _ => None,
        })
        .flatten()
        .map(|l| iota::text::ansi::strip_sgr(&l))
        .collect()
}

/// The banner's mode row, bare: the card's second row with its frame taken off — the edges,
/// the blank beside each, and the padding to the card's widest row.
fn mode_row(ui: &ScriptedUi) -> String {
    let row = &printed(ui)[2];
    row.strip_prefix("│ ")
        .and_then(|r| r.strip_suffix(" │"))
        .unwrap_or_else(|| panic!("not a card row: {row:?}"))
        .trim_end()
        .to_owned()
}

/// A capability-less provider (no `ToolProvider`) giving the same answer to every call.
fn plain(answer: Answer) -> Box<dyn Provider> {
    let round = match answer {
        Answer::Text(t) => Round::reply(&t),
        Answer::Image => Round::reply("").images(vec![Attachment {
            filename: "canvas.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: b"not-a-real-png".to_vec(),
        }]),
        Answer::Fail(msg) => Round::permanent(msg),
        // The turn's own token: cancelling it is what makes `stream_turn` classify the failure as
        // the user's interrupt.
        Answer::Interrupt => Round::failing("dropped").interrupting(Interrupt::Call),
    };
    Box::new(FakeProvider::new().tail(round))
}

fn no_tools() -> Arc<dyn Dispatcher> {
    Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>
}

// ---------------------------------------------------------------------------
// the anchors
// ---------------------------------------------------------------------------

/// One successful turn walks Idle → Busy → Idle, and the
/// ping carries a DIGEST of the answer, never a fixed phrase. The loop reports `Idle` once the
/// banner is up (the host lists the chat from then on); the `Idle` the input dispatch re-asserts
/// is deduplicated away, so a host that pays per update (cmux spawns a process) sees exactly
/// three.
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

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Idle]);
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

/// An image-only reply pings `"Image ready"`, decided AFTER
/// `collectImages` attached the pictures (an empty reply with no attachments is still a digest).
#[tokio::test]
async fn an_image_only_reply_pings_image_ready() {
    let f = Fixture::new(vec![input("draw me a cat"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Image), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Idle]);
    assert_eq!(f.pings(), vec![(Kind::Done, "Image ready".to_owned())]);
}

/// A failed turn leaves the host in Error ("it stands until the user
/// acts") and pings the error's headline, BEFORE the red block lands.
#[tokio::test]
async fn a_failed_turn_reports_error_and_pings_the_headline() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Fail("the model is gone")), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Error]);
    let pings = f.pings();
    assert_eq!(pings.len(), 1, "one ping per failed turn: {pings:?}");
    assert_eq!(pings[0].0, Kind::Failed);
    assert!(
        !pings[0].1.is_empty(),
        "the ping must carry the error headline"
    );
}

/// The user did the interrupting, so the host goes back to Idle and
/// NOTHING pings: a bell for something they just did themselves is noise.
#[tokio::test]
async fn an_interrupted_turn_is_silent() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    iota::repl::run(f.params(plain(Answer::Interrupt), no_tools(), true))
        .await
        .expect("clean exit");

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Idle]);
    assert!(
        f.events().is_empty(),
        "an interrupt pinged: {:?}",
        f.events()
    );
}

/// End to end: config `notify: false` silences every ping while leaving the state channel untouched.
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

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Idle]);
    assert!(
        f.events().is_empty(),
        "notify: false still pinged: {:?}",
        f.events()
    );
}

/// The approval gate is the loop's `NeedsInput` anchor: the host
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
    iota::repl::run(f.params(Box::new(FakeProvider::reporting(1, None)), dispatch, true))
        .await
        .expect("clean exit");

    assert_eq!(
        f.states(),
        vec![
            State::Idle,
            State::Busy,
            State::NeedsInput,
            State::Busy,
            State::Idle
        ]
    );
    assert_eq!(
        f.pings(),
        vec![
            (Kind::NeedsInput, "noop wants to modify files".to_owned()),
            (Kind::Done, "final answer".to_owned()),
        ]
    );
    // An explicit host list is no detected host: the banner's mode row names none.
    let mode = mode_row(&f.ui);
    assert!(mode.starts_with("chat · session "), "{mode:?}");
    assert!(!mode.contains(" · in "), "{mode:?}");
}

/// A call that yielded leaves a job running while the loop sits idle: the host hears `Idle` once —
/// the banner — then `Busy` for as long as the job runs, and NOT `Idle` again when the job ends and
/// its notice is on the way; the turn the notice runs is what ends in `Idle`. (The yield is driven
/// through the registry before the loop starts, the way `tests/repl/jobs.rs` drives its jobs.)
#[tokio::test]
async fn a_yielded_job_keeps_the_host_busy_until_its_notice_turn_ends() {
    if crate::jobs::skip_unless_posix(
        "a_yielded_job_keeps_the_host_busy_until_its_notice_turn_ends",
    ) {
        return;
    }
    let f = Fixture::new(vec![
        // The job's notice, when it comes, wakes the loop and runs a turn.
        Reply::Enqueued,
        Reply::Interrupted,
    ]);
    let temp = tempfile::tempdir().expect("tempdir");
    let jobs = iota::shell::jobs::Jobs::new(temp.path());
    let yielded = jobs
        .run(
            &CancellationToken::new(),
            &iota::shell::exec::Options {
                command: "sleep 0.5".to_owned(),
                dir: std::path::PathBuf::new(),
                timeout: None,
                sandbox: None,
            },
            std::time::Duration::from_millis(50),
        )
        .await;
    assert!(
        matches!(yielded, iota::shell::jobs::CallEnd::Yielded { .. }),
        "the call did not yield"
    );
    let mut params = f.params(plain(Answer::Text("noted".to_owned())), no_tools(), true);
    params.jobs = jobs;
    iota::repl::run(params).await.expect("clean exit");

    assert_eq!(f.states(), vec![State::Idle, State::Busy, State::Idle]);
    assert_eq!(f.pings(), vec![(Kind::Done, "noted".to_owned())]);
}

// ---------------------------------------------------------------------------
// the herdr host, over the mock socket (tests/common/herdr_mock.rs)
// ---------------------------------------------------------------------------

/// A presenter over the REAL herdr host, detected from an injected environment pointed at `mock`
/// (no ANSI fallback: the socket is what is asserted).
#[cfg(unix)]
fn herdr_presenter(mock: &crate::common::HerdrMock, pane: &str) -> Arc<Presenter> {
    let vars = mock.env(pane);
    let borrowed: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let pres = Arc::new(Presenter::new(
        &iota::host::Probe {
            env: iota::app::env::Env::fixed(&borrowed),
            look_path: Box::new(|_| None),
        },
        None,
        true,
    ));
    assert_eq!(pres.host_names(), vec!["herdr"]);
    pres
}

/// The approval turn of `the_approval_gate_walks_needs_input_and_back`, heard by a herdr pane: the
/// pane is `idle` — listed — as soon as the banner is up, THEN told the session the loop writes
/// into; the turn walks `working → blocked → working → idle` — the `blocked` without a message,
/// herdr words its own notification — and the exit releases the pane. Every request names the pane
/// and `iota`, and `seq` only ever grows.
#[cfg(unix)]
#[tokio::test]
async fn a_herdr_pane_hears_the_session_the_turn_and_the_release() {
    let mock = crate::common::HerdrMock::start();
    let f = Fixture::new(vec![
        input("write the file"),
        commit(0),                 // the gate's "Allow once"
        Reply::Queued(Vec::new()), // the round boundary's steering drain
        Reply::Interrupted,
    ]);
    let writer = f.writer();
    let (id, dir) = (writer.id().to_owned(), writer.dir().to_path_buf());
    let dispatch =
        Arc::new(StaticDispatcher::new(&["noop"]).with_approval(&["noop"])) as Arc<dyn Dispatcher>;
    let mut params = f.params(Box::new(FakeProvider::reporting(1, None)), dispatch, true);
    params.session.writer = Some(writer);
    params.pres = herdr_presenter(&mock, "w1:p2");
    iota::repl::run(params).await.expect("clean exit");

    assert_eq!(
        mock.summaries(),
        [
            "report_agent idle".to_owned(),
            format!("report_agent_session {id}"),
            "report_agent working".to_owned(),
            "report_agent blocked".to_owned(),
            "report_agent working".to_owned(),
            "report_agent idle".to_owned(),
            "release_agent".to_owned(),
        ]
    );
    let requests = mock.requests();
    assert_eq!(
        requests[1].param("agent_session_path"),
        dir.to_string_lossy(),
        "{:?}",
        requests[1]
    );
    let seqs: Vec<u64> = requests.iter().map(|r| r.seq().expect("a seq")).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is not strictly increasing: {seqs:?}"
    );
    for r in &requests {
        assert_eq!(r.param("pane_id"), "w1:p2", "{r:?}");
        assert_eq!(r.param("source"), "iota", "{r:?}");
        assert_eq!(r.param("agent"), "iota", "{r:?}");
        assert!(
            r.params.get("message").is_none(),
            "a message went out: {r:?}"
        );
    }
    // The recording host of the fixture was replaced by the herdr presenter: nothing reached it.
    assert!(f.states().is_empty());
    // The banner's mode row names the detected host.
    assert_eq!(mode_row(&f.ui), format!("chat · session {id} · in herdr"));
}

/// An ephemeral chat is listed `idle` at start-up but tells herdr nothing about a session — there
/// is none — until `/save` mints the bundle: the report follows the mint, with the id and directory
/// the factory produced.
#[cfg(unix)]
#[tokio::test]
async fn save_reports_the_minted_session_to_herdr() {
    use std::sync::Mutex;

    let mock = crate::common::HerdrMock::start();
    let f = Fixture::new(vec![input("/save"), Reply::Interrupted]);
    let minted: Arc<Mutex<Option<(String, std::path::PathBuf)>>> = Arc::default();
    let store = f.store.clone();
    let sink = Arc::clone(&minted);
    let mut params = f.params(plain(Answer::Text("unused".to_owned())), no_tools(), true);
    params.session = SessionCtx {
        writer: None,
        store: f.store.clone(),
        new_session: Some(Box::new(move || {
            let w = store.create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))?;
            *sink.lock().unwrap_or_else(PoisonError::into_inner) =
                Some((w.id().to_owned(), w.dir().to_path_buf()));
            Ok(w)
        })),
        scope: None,
    };
    params.pres = herdr_presenter(&mock, "w1:p2");
    iota::repl::run(params).await.expect("clean exit");

    let (id, dir) = minted
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("/save minted a writer");
    assert_eq!(
        mock.summaries(),
        [
            "report_agent idle".to_owned(),
            format!("report_agent_session {id}"),
            "release_agent".to_owned()
        ]
    );
    assert_eq!(
        mode_row(&f.ui),
        "chat · not saved · /save keeps it · in herdr"
    );
    assert_eq!(
        mock.requests()[1].param("agent_session_path"),
        dir.to_string_lossy()
    );
}
