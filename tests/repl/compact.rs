//! WP53 L3 suite: `/compact`, the pre-send auto-offer and the compaction marker, driven
//! end to end through `iota::repl::run` (`chat/run.go:253-280`, :796-800, :979-989).
//!
//! Three laws are worth the whole file:
//!
//! 1. **The persisted marker and the live view agree.** Compaction rebuilds the in-memory
//!    conversation AND writes a marker; a reload weaves the same summary into the same
//!    message by the same rule. A test that only checked one side would let them drift.
//! 2. **The offer is a question, not an action.** Declining sends the message as it stands
//!    and SNOOZES: the same conversation must not be asked about again until it has
//!    materially grown.
//! 3. **Nothing to compact is not a failure.** The typed command says so; the automatic
//!    offer never had a reason to speak.

use std::sync::Arc;

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::provider::usage::Usage;
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::testing::{FakeProvider, Reply, Round, ScriptedUi, StaticDispatcher, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelResult, TabbedResult, TurnBusy, Ui};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// The marker the summary prompt is recognised by — its first words, which are also what
/// makes the prompt injection-hardened.
const SUMMARY_MARK: &str = "You are compressing a conversation";

/// A usage-reporting unary provider that answers the SUMMARY pass and an ordinary turn
/// differently; `summary: None` = the summary pass fails (the "Compaction failed" path). Every
/// prompt it was sent is its log's `prompts()`.
fn summarizer(summary: Option<&str>) -> FakeProvider {
    let summary = summary.map(str::to_owned);
    FakeProvider::new()
        .with_model("gpt-4o")
        .reporting_usage()
        .answering(move |_, messages| {
            let prompt = messages
                .last()
                .map(|m| m.content.as_str())
                .unwrap_or_default();
            if !prompt.starts_with(SUMMARY_MARK) {
                return Round::reply("an answer").usage(Usage {
                    input: 40,
                    output: 5,
                    ..Usage::default()
                });
            }
            match &summary {
                None => Round::failing("upstream is down"),
                Some(s) => Round::reply(s).usage(Usage {
                    input: 900,
                    output: 60,
                    ..Usage::default()
                }),
            }
        })
}

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

    fn writer(&self) -> SessionWriter {
        self.store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-4o"))
            .expect("create writer")
    }

    fn params(
        &self,
        provider: FakeProvider,
        writer: Option<SessionWriter>,
        imported: Vec<Message>,
        context_window: u64,
    ) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: imported,
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: McpHooks::default(),
            session: SessionCtx {
                writer,
                store: self.store.clone(),
                new_session: None,
                scope: None,
                bot: false,
                notices: Vec::new(),
                recorded_notices: Vec::new(),
                memory: None,
            },
            params: iota::session::LayeredParams {
                context_window: iota::session::Param::config(context_window),
                ..iota::session::LayeredParams::default()
            },
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(
                Presenter::with_hosts(Vec::new(), true)
                    .show_turn_on(Arc::clone(&self.ui) as Arc<dyn Ui>),
            ),
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

/// A `Confirm` answer: cursor 0 = the "yes" item, 1 = "no".
fn answer(cursor: usize) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![PanelResult {
            cursor,
            ..PanelResult::default()
        }],
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

/// The summary pass's choreography, in order: the transcript widget it runs under (`widget:` with its
/// label), the widget coming down (`close`), the printed lines (`print:`) and any status-row busy segment
/// (`busy:`).
fn lifecycle(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .flat_map(|e| match e {
            UiEvent::CallPreview(l) => vec![format!("widget:{}", strip_sgr(&l))],
            UiEvent::ClosePreview => vec!["close".to_owned()],
            UiEvent::Print(lines) => lines
                .iter()
                .map(|l| format!("print:{}", strip_sgr(l)))
                .collect(),
            UiEvent::Busy(l) => vec![format!("busy:{l}")],
            _ => Vec::new(),
        })
        .collect()
}

/// The summary pass ran under the transcript's spinner widget — raised, then taken down, then the outcome
/// starting `outcome` — and no static `Compacting context…` line. The status row carries no busy segment of
/// the pass's own up to the outcome (a turn after it has its own): with `phase` — the pre-send paths, which
/// run inside the turn the message starts (DIVERGENCES X-74, X-76) — the one busy there is that turn's phase
/// name, `Compacting`, raised before the widget and over it.
fn assert_ran_under_the_widget(ui: &ScriptedUi, outcome: &str, phase: Option<&str>) {
    let ev = lifecycle(ui);
    let raised = ev
        .iter()
        .position(|e| e == "widget:Compacting context…")
        .unwrap_or_else(|| panic!("the summary pass raises the widget: {ev:?}"));
    let closed = ev[raised..].iter().position(|e| e == "close").map_or_else(
        || panic!("the widget is taken down: {ev:?}"),
        |i| raised + i,
    );
    let landed = ev[closed..]
        .iter()
        .position(|e| e.starts_with(&format!("print:{outcome}")))
        .map_or_else(
            || panic!("the outcome follows the widget: {ev:?}"),
            |i| closed + i,
        );
    assert!(
        ev[raised + 1..landed]
            .iter()
            .all(|e| e == "close" || e == "print:"),
        "nothing but the close lands between the raise and the outcome: {ev:?}"
    );
    let busy: Vec<&String> = ev[..landed]
        .iter()
        .filter(|e| e.starts_with("busy:"))
        .collect();
    match phase {
        None => assert!(
            busy.is_empty(),
            "the status row carries no busy segment for the pass: {ev:?}"
        ),
        Some(p) => {
            assert_eq!(
                busy,
                [&format!("busy:{p}")],
                "only the turn's phase: {ev:?}"
            );
            let named = ev.iter().position(|e| e == &format!("busy:{p}")).unwrap();
            assert!(named < raised, "the phase is named before the pass: {ev:?}");
        }
    }
    assert!(
        !ev.iter().any(|e| e == "print:Compacting context…"),
        "the label is the widget's, never a printed line: {ev:?}"
    );
}

/// Every blocking surface title, in order — the auto-offer's confirmation is one.
fn surface_titles(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => s.panels.first().map(|p| p.title.clone()),
            _ => None,
        })
        .collect()
}

/// A five-message conversation whose middle (u1/a1) is compactable.
fn conversation() -> Vec<Message> {
    vec![
        Message::system("sys"),
        Message::user("u1"),
        Message::assistant("a1"),
        Message::user("u2"),
        Message::assistant("a2"),
    ]
}

/// Roughly `n` tokens of filler, so a small window can be pushed over its threshold
/// without a megabyte of text.
fn filler(n: usize) -> String {
    "context ".repeat(n)
}

// ---------------------------------------------------------------------------
// the command
// ---------------------------------------------------------------------------

/// The typed command runs the shared flow: the summary pass under the transcript's spinner widget (the status
/// row carries no busy segment for it), the rebuilt view, the persisted marker, and the
/// reclaimed-window notice carrying `budget.status()`.
///
/// And reloading the bundle must
/// rebuild EXACTLY the view the loop is now holding. The live weave and the reload weave
/// are the same rule (`iota::session::summary_preamble`); this is where the two are
/// compared against each other rather than each against itself.
#[tokio::test]
async fn compact_rebuilds_the_view_and_the_reload_agrees() {
    let f = Fixture::new(vec![input("/compact"), Reply::Interrupted]);
    let mut writer = f.writer();
    let id = writer.id().to_owned();
    let history = conversation();
    writer.append_messages(&history).expect("append");

    iota::repl::run(f.params(summarizer(Some("THE-SUMMARY")), Some(writer), history, 0))
        .await
        .expect("clean exit");

    assert_ran_under_the_widget(&f.ui, "Context compacted → ", None);

    // The bundle now reconstructs the compacted view from its marker.
    let reloaded = f.store.load(&id, ProviderKind::OpenAi).expect("load");
    let view = &reloaded.messages;
    assert_eq!(view.len(), 3, "system + summary-on-u2 + a2: {view:?}");
    assert_eq!(view[0].content, "sys");
    assert_eq!(
        view[1].content,
        format!("{}u2", iota::session::summary_preamble("THE-SUMMARY")),
        "the reload weave must be the live weave"
    );
    assert_eq!(view[2].content, "a2");
}

/// Nothing older than the last turn is not an error, and only
/// the user who ASKED is told. The history is left exactly as it was.
#[tokio::test]
async fn nothing_to_compact_is_reported_only_for_the_typed_command() {
    let f = Fixture::new(vec![input("/compact"), Reply::Interrupted]);
    let mut writer = f.writer();
    let id = writer.id().to_owned();
    let history = vec![Message::system("sys"), Message::user("u1")];
    writer.append_messages(&history).expect("append");
    let p = summarizer(Some("THE-SUMMARY"));
    let prompts = p.log();

    iota::repl::run(f.params(p, Some(writer), history, 0))
        .await
        .expect("clean exit");

    assert!(
        printed(&f.ui).contains(&"Nothing to compact yet.".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(
        prompts.prompts().is_empty(),
        "an empty middle must not cost an API call"
    );
    let ev = lifecycle(&f.ui);
    assert!(
        !ev.iter()
            .any(|e| e.contains("Compacting context…") || e == "close"),
        "no pass runs, so no widget rises: {ev:?}"
    );
    let reloaded = f.store.load(&id, ProviderKind::OpenAi).expect("load");
    assert_eq!(reloaded.messages.len(), 2, "the history was rewritten");
}

/// A failed summary pass is reported and changes nothing. The
/// conversation the user was about to lose is still there.
#[tokio::test]
async fn a_failed_summary_pass_leaves_the_conversation_alone() {
    let f = Fixture::new(vec![input("/compact"), Reply::Interrupted]);
    let mut writer = f.writer();
    let id = writer.id().to_owned();
    let history = conversation();
    writer.append_messages(&history).expect("append");

    iota::repl::run(f.params(summarizer(None), Some(writer), history, 0))
        .await
        .expect("clean exit");

    assert_ran_under_the_widget(&f.ui, "Compaction failed: ", None);
    let reloaded = f.store.load(&id, ProviderKind::OpenAi).expect("load");
    assert_eq!(
        reloaded.messages.len(),
        5,
        "a failed pass must not write a marker"
    );
}

/// `/compact <hint>` forwards the hint to the summary pass where Go puts it — after the
/// instruction, before the fenced conversation.
#[tokio::test]
async fn a_hint_reaches_the_summary_prompt() {
    let f = Fixture::new(vec![
        input("/compact keep the file paths"),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    let history = conversation();
    writer.append_messages(&history).expect("append");
    let p = summarizer(Some("THE-SUMMARY"));
    let prompts = p.log();

    iota::repl::run(f.params(p, Some(writer), history, 0))
        .await
        .expect("clean exit");

    let sent = prompts.prompts();
    let summary_prompt = sent
        .iter()
        .find(|s| s.starts_with(SUMMARY_MARK))
        .expect("the summary pass ran");
    assert!(summary_prompt.contains(
        "\n\nExtra guidance from the user — emphasize this: keep the file paths\n\n--- CONVERSATION START ---\n"
    ));
    assert!(summary_prompt.ends_with("--- CONVERSATION END ---"));
}

// ---------------------------------------------------------------------------
// the pre-send auto-offer
// ---------------------------------------------------------------------------

/// Over the threshold the loop ASKS before sending, with the
/// projected occupancy in the question. Accepting compacts and the message still goes.
#[tokio::test]
async fn the_auto_offer_fires_over_the_threshold_and_compacts_when_accepted() {
    // Window 2k → threshold max(1600, 2000−16000) = 1600.
    let f = Fixture::new(vec![input("next question"), answer(0), Reply::Interrupted]);
    let mut writer = f.writer();
    let id = writer.id().to_owned();
    let history = vec![
        Message::system("sys"),
        Message::user(filler(900)),
        Message::assistant(filler(900)),
        Message::user("u2"),
        Message::assistant("a2"),
    ];
    writer.append_messages(&history).expect("append");

    iota::repl::run(f.params(
        summarizer(Some("THE-SUMMARY")),
        Some(writer),
        history,
        2_000,
    ))
    .await
    .expect("clean exit");

    let titles = surface_titles(&f.ui);
    assert_eq!(titles.len(), 1, "exactly one question: {titles:?}");
    assert!(
        titles[0].starts_with("Context ≈") && titles[0].ends_with(" — compact before sending?"),
        "the question must carry the projected occupancy: {titles:?}"
    );
    assert!(
        printed(&f.ui)
            .iter()
            .any(|l| l.starts_with("Context compacted → ")),
        "accepting must run the shared flow"
    );
    // The automatic path runs under the same widget.
    assert_ran_under_the_widget(&f.ui, "Context compacted → ", Some("Compacting"));
    // The offer is the turn's (busy-span point 1): it rises before the question, which waits on the user
    // with the turn's clock stopped (point 4), and runs again for the pass and the request.
    let ev = f.ui.events();
    let turn: Vec<(usize, TurnBusy)> = ev
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            UiEvent::Turn(t) => Some((i, *t)),
            _ => None,
        })
        .collect();
    let asked = ev
        .iter()
        .position(|e| matches!(e, UiEvent::Tabbed(_)))
        .expect("the question");
    assert_eq!(
        turn.iter().map(|(_, t)| *t).collect::<Vec<_>>()[..3],
        [TurnBusy::Running, TurnBusy::Waiting, TurnBusy::Running],
        "{turn:?}"
    );
    assert!(
        turn[1].0 < asked && asked < turn[2].0,
        "the question is the wait"
    );
    let reloaded = f.store.load(&id, ProviderKind::OpenAi).expect("load");
    // system + summary-on-u2 + a2, then the sent message and its answer.
    assert_eq!(reloaded.messages.len(), 5);
    assert!(reloaded.messages[1].content.contains("THE-SUMMARY"));
    assert_eq!(reloaded.messages[3].content, "next question");
}

/// Declining sends the message untouched and SNOOZES: the offer
/// does not return until usage has grown by 5% of the window. Two messages, one question.
#[tokio::test]
async fn declining_snoozes_the_offer_until_usage_grows() {
    let f = Fixture::new(vec![
        input("first"),
        answer(1), // "Not now"
        input("second"),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    let history = vec![
        Message::system("sys"),
        Message::user(filler(900)),
        Message::assistant(filler(900)),
        Message::user("u2"),
        Message::assistant("a2"),
    ];
    writer.append_messages(&history).expect("append");
    let p = summarizer(Some("THE-SUMMARY"));
    let prompts = p.log();

    iota::repl::run(f.params(p, Some(writer), history, 2_000))
        .await
        .expect("clean exit");

    assert_eq!(
        surface_titles(&f.ui).len(),
        1,
        "the declined offer must not come straight back"
    );
    let sent = prompts.prompts();
    assert!(
        !sent.iter().any(|s| s.starts_with(SUMMARY_MARK)),
        "declining must not compact anything"
    );
    assert_eq!(sent.len(), 2, "both messages were sent: {}", sent.len());
}

/// Below the threshold nothing is asked at all — the offer is not a per-message
/// confirmation.
#[tokio::test]
async fn no_offer_below_the_threshold() {
    let f = Fixture::new(vec![input("hello"), Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(summarizer(Some("THE-SUMMARY")), Some(writer), Vec::new(), 0))
        .await
        .expect("clean exit");
    assert!(surface_titles(&f.ui).is_empty(), "an unasked question");
}

/// An ephemeral chat compacts too: the flow must not depend on a writer existing (Go's
/// `AppendCompaction` is nil-receiver safe for exactly this).
#[tokio::test]
async fn compaction_works_without_a_session_writer() {
    let f = Fixture::new(vec![input("/compact"), Reply::Interrupted]);
    iota::repl::run(f.params(summarizer(Some("THE-SUMMARY")), None, conversation(), 0))
        .await
        .expect("clean exit");
    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l.starts_with("Context compacted → ")),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("failed to persist compaction marker")),
        "an ephemeral chat has no marker to fail to write: {lines:?}"
    );
}

/// The snooze belongs to the conversation that was declined: a `/session` swap to another one — as full as
/// the first, so the watermark would still be holding it back — is asked about on its first message.
#[tokio::test]
async fn a_session_swap_does_not_carry_the_snooze() {
    let f = Fixture::new(vec![
        input("first"),
        answer(1), // "Not now"
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 0,
            panels: vec![PanelResult::default(), PanelResult::default()],
        }),
        input("second"),
        answer(1),
        Reply::Interrupted,
    ]);
    let history = vec![
        Message::system("sys"),
        Message::user(filler(900)),
        Message::assistant(filler(900)),
        Message::user("u2"),
        Message::assistant("a2"),
    ];
    // The session swapped to: the same conversation under the same window, resumable in place (the
    // fixture's catalog names no provider) and listed first.
    let mut other = f
        .store
        .create(NewSession {
            provider: String::new(),
            ..NewSession::new(ProviderKind::OpenAi, "gpt-4o")
        })
        .expect("create the other bundle");
    other.append_messages(&history).expect("append");
    other
        .update_meta(|m| m.set_context_window(2_000))
        .expect("its window");
    let other_dir = other.dir().to_path_buf();
    drop(other);
    let mut meta = iota::session::SessionMeta::read(&other_dir).expect("meta");
    "2030-01-01T00:00:00+00:00".clone_into(&mut meta.updated_at);
    std::fs::write(
        other_dir.join("meta.json"),
        serde_json::to_vec(&meta).expect("encode meta"),
    )
    .expect("write meta");

    let mut writer = f.writer();
    writer.append_messages(&history).expect("append");
    iota::repl::run(f.params(
        summarizer(Some("THE-SUMMARY")),
        Some(writer),
        history,
        2_000,
    ))
    .await
    .expect("clean exit");

    let offers: Vec<String> = surface_titles(&f.ui)
        .into_iter()
        .filter(|t| t.starts_with("Context "))
        .collect();
    assert_eq!(
        offers.len(),
        2,
        "the swapped-to session is asked about on its own: {offers:?}"
    );
}
