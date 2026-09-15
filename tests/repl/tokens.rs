//! WP53 L3 suite: token accounting through the PUBLIC entry point (`iota::repl::run`).
//!
//! What is asserted here is what the loop PUBLISHES, not what the arithmetic computes: the
//! status row's token segments (`chat/run.go:148-165` `pushStatus`), the capability gate
//! that keeps `/compact` and those segments out of a chat with no token accounting at all
//! (`chat/run.go:53` `tokenAware`), and the resume path that seeds a session's cumulative
//! figures from its own log. The arithmetic itself — thresholds, the snooze, the settled /
//! pending split, the tiktoken goldens — is unit-tested beside its source.

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
use iota::ui::facade::{Input, StatusData, Ui};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// What every call of a reporting provider accounts: `{input, output, cache_read, total}`.
const USAGE: Usage = Usage {
    input: 1_200,
    output: 300,
    cache_read: 400,
    cache_write: 0,
    total: 1_500,
};

/// A unary provider whose every `chat` answers `"an answer"` and reports [`USAGE`]. No `ToolProvider`
/// capability, so a turn takes the one-shot `stream_turn` path and the whole flow is a straight line.
fn reporting() -> FakeProvider {
    FakeProvider::new()
        .with_model("gpt-4o")
        .reporting_usage()
        .tail(Round::reply("an answer").usage(USAGE))
}

/// A reporting provider that STREAMS reasoning through the tool-loop path, so the thinking meter
/// has something to count: one round, [`REASONING_DELTAS`] then the answer, no tool calls.
fn thinking() -> FakeProvider {
    let mut round = Round::text("an answer").usage(USAGE);
    round.reasoning = REASONING_DELTAS.iter().map(|d| (*d).to_owned()).collect();
    round.result.reasoning = REASONING_DELTAS.concat();
    FakeProvider::new()
        .with_model("gpt-4o")
        .reporting_usage()
        .with_tools()
        .tail(round)
}

/// A provider with no token accounting at all — a dedicated image provider's shape.
fn token_less() -> FakeProvider {
    FakeProvider::new()
        .with_model("gpt-4o")
        .replying("an answer")
}

/// What the thinking provider streams before it answers.
const REASONING_DELTAS: &[&str] = &[
    "weighing the options here, ",
    "and then some more thinking about it",
];

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
    ) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            imported_history: imported,
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
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
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
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
        ..Input::default()
    })
}

/// Every status row the loop published, in order.
fn statuses(ui: &ScriptedUi) -> Vec<StatusData> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Status(s) => Some(s),
            _ => None,
        })
        .collect()
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

/// The completion table the composer was handed.
fn commands(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Commands(c) => Some(c),
            _ => None,
        })
        .flatten()
        .map(|s| s.value)
        .collect()
}

// ---------------------------------------------------------------------------
// the status row
// ---------------------------------------------------------------------------

/// For a usage-reporting provider the row carries
/// the whole token half: the context occupancy, the window, whether the figure was
/// measured, and the session's cumulative ↑/↓ with the cache share that qualifies the
/// input figure. The last row of a completed turn is the MEASURED one.
#[tokio::test]
async fn status_row_carries_the_token_segments_for_a_reporting_provider() {
    let f = Fixture::new(vec![input("hello"), Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(reporting(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");

    let rows = statuses(&f.ui);
    let first = rows.first().expect("a status row at startup");
    assert_eq!(first.model, "gpt-4o");
    assert_eq!(first.ctx_window, 128_000, "the default window");
    assert_eq!(first.ctx_used, 0);
    assert!(first.estimated, "nothing has been measured yet");

    let last = rows.last().expect("a status row after the turn");
    // The turn's own call settled the figure: `Usage::context_tokens` of {total: 1500}.
    assert_eq!(last.ctx_used, 1_500);
    assert!(
        !last.estimated,
        "a provider-reported figure is not an estimate"
    );
    assert_eq!(last.in_tokens, 1_200);
    assert_eq!(last.out_tokens, 300);
    assert!(
        (last.cache_hit_pct - 400.0 / 1_200.0 * 100.0).abs() < 1e-9,
        "cache share = {}",
        last.cache_hit_pct
    );

    // The figure MOVED during the turn rather than appearing once at the end: the user's
    // message was booked as a pending estimate the moment it landed.
    assert!(
        rows.iter()
            .any(|s| s.ctx_used > 0 && s.ctx_used < 1_500 && s.estimated),
        "the meter never published a live estimate: {rows:?}"
    );
}

/// `/status` gains the whole token block for a provider
/// that accounts tokens, and stays at the token-LESS shape for one that does not (T-10).
/// The row CONTENT is pinned beside `status_lines` itself; this asserts the wiring, i.e.
/// that the loop's gate is the provider capability and that the live budget/meter reach it.
#[tokio::test]
async fn status_gains_the_token_block_for_a_reporting_provider() {
    let with = |p: FakeProvider| async move {
        let f = Fixture::new(vec![
            input("hello"),
            input("/status"),
            Reply::Tabbed(iota::ui::facade::TabbedResult::default()),
            Reply::Interrupted,
        ]);
        let writer = f.writer();
        iota::repl::run(f.params(p, Some(writer), Vec::new()))
            .await
            .expect("clean exit");
        f.ui.events()
            .into_iter()
            .find_map(|e| match e {
                UiEvent::Tabbed(t) => Some(t),
                _ => None,
            })
            .expect("the /status viewer")
    };

    let view = &with(reporting()).await.panels[0];
    assert_eq!(view.title, "Status");
    assert_eq!(view.kind, iota::ui::facade::PanelKind::View);
    assert_eq!(
        view.line_count, 12,
        "Provider, Model, Context, Token count, Last turn, Session in/out/cache, \
         Messages, Tools, MCP, Session"
    );

    let view = &with(token_less()).await.panels[0];
    assert_eq!(
        view.line_count, 6,
        "a token-less provider keeps Go's token-less shape (T-10)"
    );
}

/// A provider with no token accounting publishes NO token half, so
/// the frame has nothing to render a context segment from (WP44's
/// `status_line_hides_ctx_without_tokens`, driven from the loop end).
#[tokio::test]
async fn a_token_less_provider_publishes_no_token_half() {
    let f = Fixture::new(vec![input("hello"), Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(token_less(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");

    for s in statuses(&f.ui) {
        assert_eq!(s.model, "gpt-4o");
        assert_eq!(
            (s.ctx_used, s.ctx_window, s.in_tokens, s.out_tokens),
            (0, 0, 0, 0),
            "a token-less provider leaked a token figure: {s:?}"
        );
    }
}

/// The thinking meter counts streamed reasoning with the chat's OWN tokenizer, and the
/// figure reaches the widget's status row as `"<tok> tokens"`.
///
/// WP48 pinned the meter against an injected estimator; what is pinned HERE is that the
/// run loop actually injects one. A `None` estimator would leave every detail row
/// token-less and nothing else would notice.
#[tokio::test]
async fn the_thinking_meter_counts_with_the_chats_tokenizer() {
    let f = Fixture::new(vec![input("think about it"), Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(thinking(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");

    let details: Vec<String> =
        f.ui.events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::CallDetail(d) => Some(strip_sgr(&d)),
                _ => None,
            })
            .collect();
    // The estimator IS the o200k counter, so the published figure is a running prefix sum
    // of the deltas' counts — which prefix depends on the meter's 150ms publish throttle
    // (`chat/transcript.go:762-768`: every delta is counted, only some are published), so
    // every prefix is accepted. What is NOT accepted is the absence of a token segment,
    // which is what a `None` estimator would produce.
    let mut running = 0;
    let want: Vec<String> = REASONING_DELTAS
        .iter()
        .map(|d| {
            running += iota::testing::count_tokens(d);
            format!("{} tokens", iota::text::tokens(running))
        })
        .collect();
    assert!(
        details.iter().any(|d| want.contains(d)),
        "the thinking meter published no token detail (want one of {want:?}): {details:?}"
    );
}

// ---------------------------------------------------------------------------
// the capability gate
// ---------------------------------------------------------------------------

/// `/compact` exists exactly for a provider
/// whose usage the meter can settle against. The ONE-TABLE law holds on both sides: the
/// banner, the completion list and the dispatch chain agree.
#[tokio::test]
async fn compact_is_registered_only_with_token_accounting() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(reporting(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");
    assert_eq!(
        commands(&f.ui),
        [
            "/file", "/session", "/model", "/compact", "/export", "/status", "/tools", "/debug"
        ],
        "/compact keeps Go's position, between /model and /export"
    );
    assert!(
        printed(&f.ui)
            .iter()
            .any(|l| l.contains("/compact") && l.starts_with("Commands: ")),
        "the banner must advertise what the table registered"
    );

    let f = Fixture::new(vec![Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(token_less(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");
    assert_eq!(
        commands(&f.ui),
        [
            "/file", "/session", "/model", "/export", "/status", "/tools", "/debug"
        ]
    );
    assert!(
        !printed(&f.ui).iter().any(|l| l.contains("/compact")),
        "an unregistered command must also be invisible"
    );
}

/// The other half of the invisibility law: without token accounting `/compact` is not a
/// command at all, so the text is SENT — the turn runs and the model answers it.
#[tokio::test]
async fn compact_falls_through_as_a_message_without_token_accounting() {
    let f = Fixture::new(vec![input("/compact"), Reply::Interrupted]);
    let writer = f.writer();
    iota::repl::run(f.params(token_less(), Some(writer), Vec::new()))
        .await
        .expect("clean exit");
    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l.contains("an answer")),
        "'/compact' should have been sent as a message: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("Nothing to compact")),
        "a command that is not registered must not dispatch"
    );
}

// ---------------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------------

/// A resumed session's cumulative ↑/↓
/// figures are what its OWN log adds up to, not zero and not the previous chat's.
#[tokio::test]
async fn a_resumed_session_seeds_its_totals_from_its_log() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let mut writer = f.writer();
    let history = vec![
        Message::user("earlier question"),
        Message::assistant("earlier answer").with_usage(Some(Usage {
            input: 5_000,
            output: 700,
            ..Usage::default()
        })),
    ];
    writer.append_messages(&history).expect("append");
    assert_eq!(writer.usage().input, 5_000);

    iota::repl::run(f.params(reporting(), Some(writer), history))
        .await
        .expect("clean exit");

    let first = statuses(&f.ui).first().cloned().expect("a status row");
    assert_eq!(first.in_tokens, 5_000);
    assert_eq!(first.out_tokens, 700);
    assert!(
        first.ctx_used > 0 && first.estimated,
        "an imported history is LOCALLY counted until the first call settles it: {first:?}"
    );
}
