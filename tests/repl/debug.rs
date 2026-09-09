//! `/debug` end to end over `ScriptedUi` (WP66; `T3_TEST_PLAN` §4): on/off notices +
//! `Status{debug}`, the two-tab inspector shape, the drill-down views and the loop.
//!
//! The row builders, `json_indent` and the URL→action map are unit-tested beside their source
//! (`repl/commands/debug.rs`); the recording seam is pinned over wiremock in
//! `tests/provider/reqlog.rs`. What only exists END TO END, and is therefore asserted here, is the
//! command's SHAPE: which surface opens with which panels, that Enter commits ALL tabs (the
//! Verbose switch lands wherever focus was), that the switch tab has nothing to drill into, and
//! that the drill-down returns to the list instead of ending the command.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::{RequestEntry, RequestLog, ResponseHalf};
use iota::provider::error::ProviderError;
use iota::provider::model::Message;
use iota::provider::{ChatResult, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, StatusData, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// Notice after `/debug on` (chat/run.go:867,894).
const RECORDING_ON: &str = "Request recording ON — activity groups stay expanded";
/// Notice after `/debug off` (chat/run.go:872,896).
const RECORDING_OFF: &str = "Request recording OFF";

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// A provider with no capabilities at all — `/debug` never touches one, and a provider without
/// usage reporting keeps the meter disabled so `push_status` publishes the row itself (T-10).
struct BareProvider;

impl Provider for BareProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }
    fn model(&self) -> &'static str {
        "gpt-4o"
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
        Box::pin(std::future::ready(Ok(ChatResult::default())))
    }
}

/// A scripted facade, a temp session store and the run's `RequestLog`.
struct Fixture {
    ui: Arc<ScriptedUi>,
    reqlog: Arc<RequestLog>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            reqlog: Arc::new(RequestLog::new()),
            _tmp: tmp,
            store,
        }
    }

    fn writer(&self) -> SessionWriter {
        self.store
            .create(ProviderKind::OpenAi, "gpt-4o", None, "", "", false)
            .expect("create writer")
    }

    fn params(&self) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(BareProvider),
            title_provider: None,
            system: String::new(),
            system_interactive: false,
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            delegator: None,
            mcp: McpHooks {
                servers: None,
                events: None,
            },
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
            reqlog: Arc::clone(&self.reqlog),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        }
    }

    /// Runs the loop to a clean exit.
    async fn run(&self) {
        iota::repl::run(self.params()).await.expect("clean exit");
    }

    /// A finished round-trip in the ring, so the list has something to drill into.
    fn record(&self, url: &str, body: &[u8], status: &str) {
        self.reqlog.add(Arc::new(RequestEntry::completed(
            "POST",
            url,
            body,
            ResponseHalf {
                status: status.to_owned(),
                resp_body: br#"{"ok":true}"#.to_vec(),
                duration: std::time::Duration::from_millis(1200),
                ..ResponseHalf::default()
            },
        )));
    }
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
    })
}

/// A commit of the two-tab inspector: which tab had focus, which row, and the switch's state.
fn commit(focused: usize, cursor: usize, on: bool) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused,
        panels: vec![
            PanelResult {
                cursor,
                ..PanelResult::default()
            },
            PanelResult {
                on,
                ..PanelResult::default()
            },
        ],
    })
}

fn cancelled() -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: true,
        ..TabbedResult::default()
    })
}

/// Every notice the loop printed, ANSI stripped, in order.
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

/// The shapes of every blocking surface call, in order.
fn surfaces(ui: &ScriptedUi) -> Vec<TabbedSummary> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Every status row published, in order.
fn statuses(ui: &ScriptedUi) -> Vec<StatusData> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Status(s) => Some(s),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// /debug on | off
// ---------------------------------------------------------------------------

/// Go: chat/run.go:864-874 — `on` and `off` flip recording, print their dim notice and republish
/// the status row, without ever opening a surface.
#[tokio::test]
async fn debug_on_and_off_toggle_recording_and_the_status_segment() {
    let f = Fixture::new(vec![
        input("/debug on"),
        input("/debug off"),
        Reply::Interrupted,
    ]);
    f.run().await;

    let notices: Vec<String> = printed(&f.ui)
        .into_iter()
        .filter(|l| l.starts_with("Request recording"))
        .collect();
    assert_eq!(
        notices,
        vec![RECORDING_ON.to_owned(), RECORDING_OFF.to_owned()]
    );
    assert!(!f.reqlog.verbose(), "the last flip wins");
    assert!(
        surfaces(&f.ui).is_empty(),
        "on/off never open the inspector"
    );

    // The status row carries `debug` while recording is on and drops it when it goes off.
    let debug_flags: Vec<bool> = statuses(&f.ui).into_iter().map(|s| s.debug).collect();
    assert_eq!(
        debug_flags,
        vec![false, true, false],
        "startup, then /debug on, then /debug off"
    );
}

// ---------------------------------------------------------------------------
// the two-tab inspector
// ---------------------------------------------------------------------------

/// Go: chat/run.go:876-884 — bare `/debug` opens a searchable, self-refreshing `Messages` list
/// beside a `Verbose` switch that opens on the log's current state.
#[tokio::test]
async fn bare_debug_opens_the_messages_list_and_the_verbose_switch() {
    let f = Fixture::new(vec![input("/debug"), cancelled(), Reply::Interrupted]);
    f.reqlog.set_verbose(true);
    f.record(
        "https://api.anthropic.com/v1/messages",
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
        "200 OK",
    );
    f.run().await;

    let s = surfaces(&f.ui);
    assert_eq!(s.len(), 1, "cancelling the list ends the command");
    let spec = &s[0];
    assert_eq!(spec.refresh_every_ms, 500);
    assert_eq!(spec.panels.len(), 2);

    let list = &spec.panels[0];
    assert_eq!(list.title, "Messages");
    assert_eq!(list.kind, PanelKind::List);
    assert!(list.search, "the list is searchable");
    assert!(list.has_refresh, "and refreshes itself while it is open");
    assert_eq!(list.items.len(), 1, "one row per captured round-trip");
    let row = strip_sgr(&list.items[0]);
    assert!(row.contains("Chat"), "row: {row:?}");
    assert!(row.contains("hi"), "row: {row:?}");
    assert!(row.contains("200"), "row: {row:?}");

    let switch = &spec.panels[1];
    assert_eq!(switch.title, "Verbose");
    assert_eq!(switch.kind, PanelKind::Switch);
    assert!(switch.on, "the switch opens on the log's state");
    assert!(!switch.has_refresh);
}

/// Any argument that is not `on`/`off` is treated exactly like the bare form (run.go:864-875 falls
/// through the `switch` to the surface loop).
#[tokio::test]
async fn an_unknown_argument_opens_the_inspector() {
    let f = Fixture::new(vec![input("/debug foo"), cancelled(), Reply::Interrupted]);
    f.run().await;
    assert_eq!(surfaces(&f.ui).len(), 1);
    assert!(
        printed(&f.ui)
            .iter()
            .all(|l| !l.starts_with("Request recording")),
        "no notice for a surface open"
    );
}

/// Go: chat/run.go:888-899 — Enter commits ALL tabs, so a flipped Verbose switch applies even when
/// focus was on the list. The switch tab itself has nothing to drill into, so committing there
/// ends the command.
#[tokio::test]
async fn a_flipped_switch_commits_from_either_tab() {
    // Focus on the LIST, switch flipped ON, and an empty ring — so the drill-down is skipped and
    // the command ends after applying the flip.
    let f = Fixture::new(vec![
        input("/debug"),
        commit(0, 0, true),
        Reply::Interrupted,
    ]);
    f.run().await;
    assert!(f.reqlog.verbose(), "the flip landed from the list tab");
    assert!(
        printed(&f.ui).contains(&RECORDING_ON.to_owned()),
        "the flip prints the same notice as `/debug on`"
    );
    assert_eq!(surfaces(&f.ui).len(), 1, "an empty ring ends the loop");

    // Focus on the SWITCH tab, flipped back off: the notice lands and the command ends.
    let g = Fixture::new(vec![
        input("/debug"),
        commit(1, 0, false),
        Reply::Interrupted,
    ]);
    g.reqlog.set_verbose(true);
    g.record("https://api.openai.com/v1/models", b"", "200 OK");
    g.run().await;
    assert!(!g.reqlog.verbose());
    assert!(printed(&g.ui).contains(&RECORDING_OFF.to_owned()));
    assert_eq!(
        surfaces(&g.ui).len(),
        1,
        "focused == 1 ends: nothing to drill into from the switch"
    );
}

/// An unchanged switch prints nothing — the notice is tied to the FLIP, not to the commit.
#[tokio::test]
async fn an_unchanged_switch_prints_no_notice() {
    let f = Fixture::new(vec![
        input("/debug"),
        commit(1, 0, false),
        Reply::Interrupted,
    ]);
    f.run().await;
    assert!(
        printed(&f.ui)
            .iter()
            .all(|l| !l.starts_with("Request recording")),
        "no flip, no notice"
    );
}

// ---------------------------------------------------------------------------
// the drill-down
// ---------------------------------------------------------------------------

/// Go: chat/run.go:900-913 — Enter on a Messages row opens the two pretty-printed views, and the
/// loop then REOPENS the list (the v1 shape) rather than ending the command.
#[tokio::test]
async fn enter_drills_into_the_entry_and_reopens_the_list() {
    let f = Fixture::new(vec![
        input("/debug"),
        commit(0, 1, false), // drill into the SECOND (oldest) row
        cancelled(),         // ESC out of the drill-down …
        cancelled(),         // … and out of the reopened list
        Reply::Interrupted,
    ]);
    f.reqlog.set_verbose(true);
    f.record(
        "https://api.openai.com/v1/chat/completions",
        br#"{"messages":[{"role":"user","content":"older"}]}"#,
        "200 OK",
    );
    f.record(
        "https://api.anthropic.com/v1/messages",
        br#"{"messages":[{"role":"user","content":"newer"}]}"#,
        "429 Too Many Requests",
    );
    f.run().await;

    let s = surfaces(&f.ui);
    assert_eq!(s.len(), 3, "list → drill-down → list again");

    let drill = &s[1];
    assert_eq!(drill.refresh_every_ms, 0, "a viewer does not self-refresh");
    assert_eq!(drill.panels.len(), 2);
    for (panel, title) in drill.panels.iter().zip(["↑ Request", "↓ Response"]) {
        assert_eq!(panel.title, title);
        assert_eq!(panel.kind, PanelKind::View);
        assert!(panel.wrap, "{title} wraps long lines");
        assert!(!panel.has_refresh);
        assert!(panel.line_count >= 3, "{title} has a head plus a body");
    }

    // Cursor 1 = the second row of a NEWEST-FIRST list: the older `chat/completions` entry.
    assert_eq!(s[0].panels[0].items.len(), 2, "both round-trips are listed");
    assert_eq!(s[2].panels[0].items.len(), 2, "the reopened list is live");
}

/// A cursor past the end of the ring — the list was empty, or entries were evicted while it was
/// open — ends the command instead of panicking (run.go:904-907).
#[tokio::test]
async fn a_cursor_past_the_end_ends_the_command() {
    let f = Fixture::new(vec![
        input("/debug"),
        commit(0, 7, false),
        Reply::Interrupted,
    ]);
    f.record("https://api.openai.com/v1/models", b"", "200 OK");
    f.run().await;
    assert_eq!(surfaces(&f.ui).len(), 1, "no drill-down, no reopen");
}

/// A closed surface (the facade failing, not the user cancelling) also ends the loop — Go breaks
/// on `serr != nil` exactly as it breaks on `Cancelled`.
#[tokio::test]
async fn a_failed_surface_ends_the_command() {
    let f = Fixture::new(vec![input("/debug"), Reply::Closed, Reply::Interrupted]);
    f.run().await;
    assert_eq!(surfaces(&f.ui).len(), 1);
}
