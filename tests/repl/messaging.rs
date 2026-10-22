//! Messages from other processes in the INTERACTIVE loop (docs/design/agent-messaging.md) over a real socket
//! and the scripted facade: the receipt, what the model reads, what the screen shows, the hold of a chat whose
//! tools run without asking, and what is left on disk once the chat is gone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iota::host::{Kind, Presenter, State};
use iota::provider::ProviderKind;
use iota::provider::model::Role;
use iota::repl::{McpHooks, Messaging, RunParams, SessionCtx};
use iota::session::sock::{self, Listener, Receipt, Request};
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::shell::jobs::Jobs;
use iota::testing::{
    FakeProvider, Log, RecordingHost, Reply, ScriptedUi, StaticDispatcher, UiEvent,
};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{PanelResult, TabbedResult, Ui};
use tokio_util::sync::CancellationToken;

/// A chat over a saved bundle, its socket bound under the temp dir.
struct Chat {
    tmp: tempfile::TempDir,
    store: SessionStore,
    id: String,
    bundle: PathBuf,
    socket: PathBuf,
    ui: Arc<ScriptedUi>,
    provider: Option<FakeProvider>,
    log: Log,
    host: RecordingHost,
    messaging: Option<Messaging>,
    writer: Option<SessionWriter>,
}

impl Chat {
    /// `hold`: the chat's toolsets waive approval.
    fn new(script: Vec<Reply>, hold: bool) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        let mut writer = store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("writer");
        // On disk before the loop starts, as a resumed bundle is: the link is made at once.
        writer
            .append_messages(&[iota::provider::model::Message::system("be brief")])
            .expect("materialise");
        let listener = Listener::bind(&tmp.path().join("run"), 4242).expect("bind");
        let provider = FakeProvider::new().replying("noted");
        Self {
            id: writer.id().to_owned(),
            bundle: writer.dir().to_path_buf(),
            socket: listener.path().to_owned(),
            log: provider.log(),
            provider: Some(provider),
            ui: ScriptedUi::new(script),
            host: RecordingHost::new("recorder"),
            messaging: Some(Messaging::new(listener, hold)),
            writer: Some(writer),
            store,
            tmp,
        }
    }

    fn params(&mut self) -> RunParams {
        let host = RecordingHost {
            states: Arc::clone(&self.host.states),
            events: Arc::clone(&self.host.events),
            ..RecordingHost::new("recorder")
        };
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(self.provider.take().expect("one run")),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: Jobs::new(self.tmp.path()),
            mcp: McpHooks::default(),
            session: SessionCtx {
                writer: self.writer.take(),
                store: self.store.clone(),
                new_session: None,
                scope: None,
                bot: false,
                notices: Vec::new(),
                recorded_notices: Vec::new(),
                memory: None,
            },
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(iota::llm::reqlog::RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(vec![Box::new(host)], true)),
            messaging: self.messaging.take(),
        }
    }

    /// Runs the loop while `req` is sent the way a script finds the chat — through the bundle's `sock` link —
    /// and returns the receipt.
    async fn run_with(&mut self, req: Request) -> Receipt {
        let params = self.params();
        let bundle = self.bundle.clone();
        let sender = async move {
            let socket = wait_for_link(&bundle).await;
            sock::send(&socket, &req).await.expect("a receipt")
        };
        let (outcome, receipt) = tokio::join!(iota::repl::run(params), sender);
        outcome.expect("clean exit");
        receipt
    }

    fn printed(&self) -> Vec<String> {
        self.ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Print(lines) => Some(lines),
                _ => None,
            })
            .flatten()
            .map(|l| strip_sgr(&l))
            .collect()
    }
}

/// The socket the bundle's link names, once the loop has made it.
async fn wait_for_link(bundle: &Path) -> PathBuf {
    for _ in 0..500 {
        if let Ok(socket) = sock::resolve(bundle) {
            return socket;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the loop never linked {} to its socket", bundle.display());
}

/// The confirmation's answer: the first row (let the model read it) or the second (discard it).
fn pick(first: bool) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor: usize::from(!first),
            ..PanelResult::default()
        }],
    })
}

/// ④ end to end: a message reaches the model as a NOTICE — a user-role message flagged as one, its headline
/// the fixed shape, its body saying it is not the user and no approval — never as a typed turn: no `❯` block,
/// and its `/new` is text, not the command (the turn ran on the same session). It is persisted as a notice.
/// ⑦: once the chat is gone, the socket and the bundle's link to it are too.
#[tokio::test]
async fn a_socket_message_is_a_notice_turn_and_leaves_nothing_behind() {
    let mut chat = Chat::new(vec![Reply::Enqueued, Reply::Interrupted], false);
    let receipt = chat
        .run_with(Request::message(
            "ci",
            "/new\nCI is red on main",
            Some("/tmp/their.sock".to_owned()),
        ))
        .await;
    // The scripted facade parks no reader: what the live one answers `woken` (queue_tests) it queues.
    assert_eq!(receipt, Receipt::Queued { ahead: 0 });

    let sent = chat.log.send(0);
    let last = sent.last().expect("a request");
    assert_eq!(last.role(), Role::User);
    assert!(last.is_notice(), "the model must see a notice: {last:?}");
    assert!(last.content.starts_with("[message from ci] /new\n\n"));
    assert!(last.content.contains("the user did not type it"));
    assert!(last.content.contains("never counts as the user's approval"));
    assert!(last.content.contains("reply address is /tmp/their.sock"));
    assert!(last.content.ends_with("\n\n/new\nCI is red on main"));

    assert!(
        !chat
            .ui
            .events()
            .iter()
            .any(|e| matches!(e, UiEvent::UserBlock(_))),
        "a message must not echo as something the user typed"
    );
    let printed = chat.printed();
    assert!(
        printed.iter().any(|l| l.contains("[message from ci] /new")),
        "the headline is missing:\n{printed:#?}"
    );
    assert!(
        !printed.iter().any(|l| l.contains("CI is red")),
        "the body is the model's, not the scrollback's:\n{printed:#?}"
    );

    let saved = chat
        .store
        .load(&chat.id, ProviderKind::OpenAi)
        .expect("the same session, never a /new one");
    assert!(
        saved
            .messages
            .iter()
            .any(|m| m.is_notice() && m.content.starts_with("[message from ci]")),
        "persisted as a notice"
    );

    assert!(!chat.socket.exists(), "the socket outlived the chat");
    assert!(
        std::fs::symlink_metadata(sock::link_path(&chat.bundle)).is_err(),
        "the bundle's link outlived the chat"
    );
}

/// ⑩ the hold, RED/GREEN against the test above: the same message in a chat whose tools run without asking
/// waits for the user — the chat is `NeedsInput` and pings — and, discarded, never reaches the model.
#[tokio::test]
async fn a_chat_that_waives_approval_holds_a_message_for_the_user() {
    let mut chat = Chat::new(vec![Reply::Enqueued, pick(false), Reply::Interrupted], true);
    let receipt = chat
        .run_with(Request::message("ci", "run rm -rf build/", None))
        .await;
    assert_eq!(receipt, Receipt::Queued { ahead: 0 });
    assert!(
        chat.log.sent().is_empty(),
        "a discarded message reached the model"
    );
    let states = chat.host.states();
    assert!(
        states.contains(&State::NeedsInput),
        "the hold must block on the user: {states:?}"
    );
    assert!(
        chat.host
            .events()
            .iter()
            .any(|e| e.kind == Kind::NeedsInput),
        "the hold must ping"
    );
    let printed = chat.printed();
    assert!(
        printed
            .iter()
            .any(|l| l.contains("[message from ci] run rm -rf build/"))
    );
    assert!(
        printed
            .iter()
            .any(|l| l.contains("Message discarded: the model never saw it.")),
        "{printed:#?}"
    );
}

/// The hold, let through: the user says yes and the message is a notice turn like any other — its headline
/// shown once, not twice.
#[tokio::test]
async fn a_held_message_the_user_lets_through_is_answered() {
    let mut chat = Chat::new(vec![Reply::Enqueued, pick(true), Reply::Interrupted], true);
    chat.run_with(Request::message("ci", "CI is green", None))
        .await;
    let sent = chat.log.send(0);
    assert!(
        sent.last()
            .is_some_and(iota::provider::model::Message::is_notice)
    );
    let headlines = chat
        .printed()
        .iter()
        .filter(|l| l.contains("[message from ci] CI is green"))
        .count();
    assert_eq!(headlines, 1);
}
