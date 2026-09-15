//! WP50 L3 suite: the run loop's command layer driven end to end against
//! `iota::testing::ScriptedUi` — the orderings Go could never unit-test, because
//! `chat.Run` needed a terminal.
//!
//! Everything here goes through the PUBLIC entry point (`iota::repl::run`): the banner, the
//! dispatch chain, `/model`, `/session`, `/save`, `/status`, `/tools` and the MCP failure
//! relay are asserted from the facade's recorded event log and from what actually landed
//! on disk. The row builders and the command table are unit-tested beside their source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::repl::{McpEvent, McpHooks, RunParams, SessionCtx};
use iota::session::{SessionStore, SessionWriter};
use iota::testing::{FakeProvider, Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, PanelResult, TabbedResult, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// the doubles
// ---------------------------------------------------------------------------

/// A provider with a scripted model listing and a canned one-shot reply (the title pass's only
/// need). No `ToolProvider` capability: no scenario here runs a turn. A test that must change the
/// world mid-run does it from `on_call` — the loop is one straight-line task.
fn provider(model: &str, models: Result<Vec<String>, String>) -> FakeProvider {
    let p = FakeProvider::new().with_model(model).replying("an answer");
    match models {
        Ok(ids) => p.with_models(&ids.iter().map(String::as_str).collect::<Vec<_>>()),
        Err(e) => p.with_models_failing(&e),
    }
}

/// The scenario under test: a scripted facade, a temp session store, and the parameters
/// `run` is handed.
struct Fixture {
    ui: Arc<ScriptedUi>,
    /// Kept alive: the store's root lives under it.
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
            .create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
            .expect("create writer")
    }

    fn params(&self, provider: FakeProvider, session: SessionCtx) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: no_mcp(),
            session,
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

    /// The same, over a real candidate set: the picker's rows then come from `agents:` rather
    /// than from the live provider alone.
    fn params_with_catalog(
        &self,
        provider: FakeProvider,
        session: SessionCtx,
        yaml: &str,
    ) -> RunParams {
        let cfg = iota::cmd::Config::parse(
            yaml.as_bytes(),
            &iota::testing::map_resolver(&[]),
            &mut |w| panic!("unexpected config warning: {w}"),
        )
        .expect("the config loads");
        let resolved = cfg.resolve_agent("default").expect("the agent resolves");
        let catalog = iota::repl::ModelCatalog::new(
            &cfg,
            &resolved,
            &iota::testing::map_env(&[]),
            &iota::provider::HttpTransport::from(iota::llm::default_http_client()),
        );
        RunParams {
            catalog,
            ..self.params(provider, session)
        }
    }

    fn session(&self, writer: Option<SessionWriter>) -> SessionCtx {
        SessionCtx {
            writer,
            store: self.store.clone(),
            new_session: None,
            scope: None,
        }
    }
}

/// Rewrites a bundle's `updated_at` so a listing's sort order is deterministic.
fn touch_updated(dir: &Path, stamp: &str) {
    let mut meta = iota::session::SessionMeta::read(dir).expect("meta");
    stamp.clone_into(&mut meta.updated_at);
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_vec(&meta).expect("encode meta"),
    )
    .expect("write meta");
}

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
        ..Input::default()
    })
}

/// A tabbed commit of one panel.
fn commit(panel: PanelResult) -> Reply {
    Reply::Tabbed(TabbedResult {
        cancelled: false,
        focused: 0,
        panels: vec![panel],
    })
}

/// Every dim/red notice line the loop printed, ANSI stripped, in order.
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

// ---------------------------------------------------------------------------
// the banner
// ---------------------------------------------------------------------------

/// The banner's rows in order, ONE blank between the environment
/// and the first transcript block, and the ONE-TABLE law: the advertised commands are
/// exactly the registered ones.
#[tokio::test]
async fn banner_order_and_command_table() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let writer = f.writer();
    let id = writer.id().to_owned();
    let session = f.session(Some(writer));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("clean exit");

    let lines = printed(&f.ui);
    assert_eq!(
        lines,
        vec![
            "Chat started. Press Ctrl+C to exit.".to_owned(),
            "Commands: /file, /session, /model, /export, /status, /tools, /debug".to_owned(),
            format!("Session: {id}"),
            String::new(),
        ]
    );
    // The completion table matches the banner exactly (one table, no drift).
    let commands =
        f.ui.events()
            .into_iter()
            .find_map(|e| match e {
                UiEvent::Commands(c) => Some(c),
                _ => None,
            })
            .expect("the loop publishes the command table");
    let values: Vec<String> = commands.into_iter().map(|s| s.value).collect();
    assert_eq!(
        values,
        [
            "/file", "/session", "/model", "/export", "/status", "/tools", "/debug"
        ]
    );
}

/// An ephemeral chat says how to keep itself, and `/save` joins the table.
#[tokio::test]
async fn banner_offers_save_for_an_ephemeral_chat() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let store = f.store.clone();
    let session = SessionCtx {
        writer: None,
        store: f.store.clone(),
        new_session: Some(Box::new(move || {
            store.create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
        })),
        scope: None,
    };
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("clean exit");

    let lines = printed(&f.ui);
    assert_eq!(
        lines[1],
        "Commands: /file, /session, /model, /export, /status, /tools, /debug, /save"
    );
    assert_eq!(
        lines[2],
        "Session: not saved — /save [title] keeps this chat"
    );
}

// ---------------------------------------------------------------------------
// /model
// ---------------------------------------------------------------------------

/// The picker's shape, the commit notice, and the untouched-tab
/// no-op law (the current model is the cursor row).
#[tokio::test]
async fn model_picks_from_the_listing() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let writer = f.writer();
    let session = f.session(Some(writer));
    let provider = provider(
        "a-model",
        Ok(vec!["a-model".to_owned(), "b-model".to_owned()]),
    );
    iota::repl::run(f.params(provider, session))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(
        panels.len(),
        1,
        "a capability-less provider gets the Model tab alone (WP54's tabs assemble by \
         capability; the full questionnaire is tests/settings.rs)"
    );
    assert_eq!(panels[0].title, "Model");
    assert_eq!(panels[0].kind, PanelKind::List);
    assert!(
        panels[0].combo,
        "the picker is a combo: the input row is always there, the list completes it"
    );
    assert_eq!(panels[0].placeholder, "model name (e.g. gpt-4o)");
    assert!(
        panels[0].prompt.is_empty(),
        "nothing failed, nothing to say"
    );
    assert_eq!(panels[0].items, ["a-model (current)", "b-model"]);
    assert_eq!(
        panels[0].cursor, 0,
        "the cursor starts on the current model"
    );

    assert!(
        printed(&f.ui).contains(&"Model switched to b-model".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    // The fetch ran under its own scope with a busy row (run.go:1112-1121).
    let events = f.ui.events();
    let busy = events
        .iter()
        .position(|e| *e == UiEvent::Busy("Fetching available models".to_owned()))
        .expect("the fetch shows a busy row");
    let push = events
        .iter()
        .position(|e| *e == UiEvent::ScopePush)
        .expect("the fetch pushes a cancel scope");
    assert!(push < busy, "the scope is pushed before the busy row");
}

/// Committing the current row changes nothing and says so.
#[tokio::test]
async fn model_reports_no_changes() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult::default()),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let provider = provider("a-model", Ok(vec!["a-model".to_owned()]));
    iota::repl::run(f.params(provider, session))
        .await
        .expect("exit");
    assert!(printed(&f.ui).contains(&"No changes.".to_owned()));
}

/// A listing that fails does NOT take the picker away: the panel is the same combo it always is,
/// the failure is one line in its prompt row (not a line printed over the transcript), and the
/// text the user types commits through the `use "…" as typed` row — the row one past the last
/// listed one (brain page `config-three-layers`).
#[tokio::test]
async fn model_keeps_the_picker_when_a_listing_fails() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            // Row 1 = one past the only row (the current model): the typed text.
            cursor: 1,
            text: "  gpt-5-custom  ".to_owned(),
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    // Materialise the bundle: a PENDING one holds its meta in memory until the first
    // append (chat/session.go:363-378), so there would be nothing on disk to assert.
    writer
        .append_messages(&[Message::user("earlier")])
        .expect("materialise");
    let dir = writer.dir().to_path_buf();
    let session = f.session(Some(writer));
    let provider = provider("old-model", Err("no such endpoint".to_owned()));
    iota::repl::run(f.params(provider, session))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(panels[0].kind, PanelKind::List);
    assert!(panels[0].combo, "a failed listing is still a combo");
    assert_eq!(panels[0].placeholder, "model name (e.g. gpt-4o)");
    assert_eq!(
        panels[0].items,
        ["old-model (current)"],
        "the current model is the only row a failed listing can offer"
    );
    assert_eq!(
        panels[0].prompt, "no such endpoint",
        "the failure is the panel's subtitle, not a line over the transcript"
    );
    let lines = printed(&f.ui);
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("enter a model name manually")),
        "the interrupting error line is gone: {lines:?}"
    );
    assert!(lines.contains(&"Model switched to gpt-5-custom".to_owned()));
    // The commit reaches session meta, not just the live provider.
    let meta = iota::session::SessionMeta::read(&dir).expect("meta written");
    assert_eq!(meta.model, "gpt-5-custom");
}

/// The candidate set IS the picker's row list: a `models:` entry and an inline `provider:id` are
/// rows on the spot, `provider:*` on the session's own endpoint is whatever it lists, and a model
/// reachable twice is one row. Rows on another endpoint carry it; rows on this one do not.
#[tokio::test]
async fn model_lists_the_agents_candidate_set() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 2,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let provider = provider(
        "a-model",
        Ok(vec!["a-model".to_owned(), "b-model".to_owned()]),
    );
    let params = f.params_with_catalog(provider, session, CANDIDATE_SET);
    iota::repl::run(params).await.expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(
        panels[0].items,
        [
            // the `models:` entry, which is also what the chat is running
            "a-model (current)",
            // an inline reference to ANOTHER endpoint: written as `-M` writes it
            "relay:vendor/y",
            // the wildcard's own listing, minus the id the entry already offered
            "b-model",
        ]
    );
    assert_eq!(
        panels[0].cursor, 0,
        "the cursor starts on the current model"
    );
    assert!(
        printed(&f.ui).contains(&"Model switched to b-model".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
}

/// A row on another provider is listed but not taken: a session keeps the endpoint it started on
/// (its history is that dialect's), so the picker says so and names the command that would start
/// a run there, rather than half-applying a switch whose next request would go to the wrong API.
#[tokio::test]
async fn model_reports_a_candidate_on_another_provider() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let provider = provider("a-model", Ok(vec!["a-model".to_owned()]));
    let params = f.params_with_catalog(provider, session, CANDIDATE_SET);
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l.starts_with(
            "relay:vendor/y runs on another provider; a session keeps the endpoint it started on"
        ) && l.contains("iota run default -M relay:vendor/y")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Model switched")),
        "nothing was switched: {lines:?}"
    );
}

/// An agent whose candidate set spans two endpoints; only `mock` is the one the session talks to.
const CANDIDATE_SET: &str = r#"
providers:
  mock: {type: openai, key: k}
  relay: {type: openai, key: k, url: "http://127.0.0.1:9"}
models:
  m: mock:a-model
agents:
  default:
    models: [m, "relay:vendor/y", "mock:*"]
"#;

/// The startup pick fires when no model is configured, and its
/// notice is `ensureModel`'s, not `/model`'s.
#[tokio::test]
async fn startup_pick_runs_when_no_model_is_configured() {
    let f = Fixture::new(vec![
        // the startup Select (sugar over tabbed)
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let provider = provider("", Ok(vec!["a".to_owned(), "b".to_owned()]));
    iota::repl::run(f.params(provider, session))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(panels[0].title, "Select a model");
    assert_eq!(panels[0].items, ["a", "b"]);
    assert!(printed(&f.ui).contains(&"Using model: b".to_owned()));
}

// ---------------------------------------------------------------------------
// /session
// ---------------------------------------------------------------------------

/// The picker's two tabs, the exact swap ordering's observable
/// half (title → notice → echo → status) and the resumed history.
#[tokio::test]
async fn session_resume_swaps_and_echoes() {
    // A bundle to resume: two rounds on disk.
    let f = Fixture::new(vec![
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 0,
            panels: vec![PanelResult::default(), PanelResult::default()],
        }),
        Reply::Interrupted,
    ]);
    let mut other = f.writer();
    other
        .append_messages(&[
            Message::user("an older question"),
            Message::assistant("an older answer"),
        ])
        .expect("seed the bundle");
    other
        .update_meta(|m| m.title = "older chat".to_owned())
        .expect("title it");
    let other_id = other.id().to_owned();
    let other_dir = other.dir().to_path_buf();
    drop(other);
    // Listings sort by `updated_at` DESC; pin it so the cursor's row is deterministic.
    touch_updated(&other_dir, "2030-01-01T00:00:00+00:00");

    let mut current = f.writer();
    current
        .append_messages(&[Message::user("still here")])
        .expect("materialise the current bundle");
    let current_id = current.id().to_owned();
    let session = f.session(Some(current));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(panels.len(), 2);
    assert_eq!(panels[0].title, "Resume");
    assert_eq!(panels[0].kind, PanelKind::List);
    assert_eq!(panels[1].title, "Delete");
    assert_eq!(panels[1].kind, PanelKind::Multi);
    assert!(panels[0].search && panels[1].search);
    // The Delete tab excludes the session being written to.
    assert_eq!(panels[0].items.len(), 2);
    assert_eq!(panels[1].items.len(), 1);
    assert!(
        panels[0]
            .items
            .iter()
            .any(|r| r.starts_with("older chat · ")),
        "{:?}",
        panels[0].items
    );

    // The swap's observable order: window title, notice, echo.
    let events: Vec<UiEvent> = f.ui.events();
    let title_at = events
        .iter()
        .rposition(|e| *e == UiEvent::Title("older chat".to_owned()))
        .expect("the window title follows the swap");
    let notice = format!("Resumed session {other_id} (2 messages)");
    let notice_at = events
        .iter()
        .position(|e| matches!(e, UiEvent::Print(l) if l.iter().any(|x| strip_sgr(x) == notice)))
        .expect("the resume notice");
    let echo_at = events
        .iter()
        .position(
            |e| matches!(e, UiEvent::Print(l) if l.iter().any(|x| strip_sgr(x).contains("an older answer"))),
        )
        .expect("the echo of the last rounds");
    assert!(title_at < notice_at, "the title is set before the notice");
    assert!(notice_at < echo_at, "the notice precedes the echo");
    assert_ne!(other_id, current_id);
}

/// Resuming the session already being written to says so and swaps nothing.
#[tokio::test]
async fn session_resume_of_the_current_session_is_a_no_op() {
    let f = Fixture::new(vec![
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 0,
            panels: vec![PanelResult::default(), PanelResult::default()],
        }),
        Reply::Interrupted,
    ]);
    let mut current = f.writer();
    current
        .append_messages(&[Message::user("hi")])
        .expect("materialise the bundle");
    let session = f.session(Some(current));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");
    assert!(printed(&f.ui).contains(&"Already in this session.".to_owned()));
}

/// An empty store says so instead of opening an empty picker.
#[tokio::test]
async fn session_reports_an_empty_store() {
    let f = Fixture::new(vec![input("/session"), Reply::Interrupted]);
    let session = f.session(None);
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");
    assert!(printed(&f.ui).contains(&"No sessions yet.".to_owned()));
    assert!(surfaces(&f.ui).is_empty(), "no picker is opened");
}

/// The Delete tab removes the checked bundles and reports one
/// aggregate count.
#[tokio::test]
async fn session_delete_tab_removes_the_checked_bundles() {
    let f = Fixture::new(vec![
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 1,
            panels: vec![
                PanelResult::default(),
                PanelResult {
                    checked: vec![0],
                    ..PanelResult::default()
                },
            ],
        }),
        Reply::Interrupted,
    ]);
    let mut doomed = f.writer();
    doomed
        .append_messages(&[Message::user("bye")])
        .expect("materialise");
    let doomed_dir = doomed.dir().to_path_buf();
    drop(doomed);
    let mut current = f.writer();
    current
        .append_messages(&[Message::user("hi")])
        .expect("materialise");
    let session = f.session(Some(current));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");

    assert!(printed(&f.ui).contains(&"Deleted 1 session(s).".to_owned()));
    assert!(!doomed_dir.exists(), "the bundle is gone from disk");
}

// ---------------------------------------------------------------------------
// /save
// ---------------------------------------------------------------------------

/// MOMENT 1, written down: a session that CREATES its bundle records the four layered parameters it
/// evaluated and the source of each, so a resume finds the session as it was even if the config moved in
/// between (brain page `model-param-layering`). A resumed bundle is not re-stamped.
#[tokio::test]
async fn a_new_bundle_records_what_the_session_runs_under() {
    let f = Fixture::new(vec![input("a question"), Reply::Interrupted]);
    let writer = f.writer();
    let dir = writer.dir().to_path_buf();
    let session = f.session(Some(writer));
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.params = iota::session::LayeredParams {
        context_window: iota::session::Param::config(400_000),
        effort: iota::session::Param::config("high".to_owned()),
        temperature: iota::session::Param::user(Some(0.3)),
        ..iota::session::LayeredParams::default()
    };
    iota::repl::run(params).await.expect("exit");

    // Every value is read back from where it actually LIVES, so a knob the dialect cannot act on is
    // recorded as the nothing it is: this fake reports no usage and has no tuning capability at all.
    let meta = iota::session::SessionMeta::read(&dir).expect("meta");
    assert_eq!(
        meta.context_window, 0,
        "a session that cannot count tokens has no window to record"
    );
    assert_eq!(meta.effort, "");
    assert_eq!(meta.temperature, None);
    assert_eq!(
        meta.sources().context_window,
        iota::session::ParamSource::Config
    );
    assert!(
        meta.records_params(),
        "and the record is complete, so a resume restores it instead of evaluating"
    );
}

/// The trio: mint late, flush the WHOLE backlog in one append,
/// and settle the user's title.
#[tokio::test]
async fn save_mints_late_and_flushes_the_backlog() {
    let f = Fixture::new(vec![
        input("a question"), // a turn while nothing is persisting
        input("/save my chat"),
        input("/save again"),
        Reply::Interrupted,
    ]);
    let store = f.store.clone();
    let session = SessionCtx {
        writer: None,
        store: f.store.clone(),
        new_session: Some(Box::new(move || {
            store.create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
        })),
        scope: None,
    };
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.params.context_window = iota::session::Param::config(200_000);
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    let saved = lines
        .iter()
        .find(|l| l.starts_with("Session saved: "))
        .expect("the save notice");
    assert!(saved.ends_with(" — auto-saving from now on."), "{saved}");
    let id = saved
        .trim_start_matches("Session saved: ")
        .trim_end_matches(" — auto-saving from now on.")
        .to_owned();
    // A second /save reports the live bundle instead of minting another.
    assert!(lines.contains(&format!("Session already saving ({id}).")));

    let dir = f
        .store
        .find_dir(&id)
        .expect("the minted bundle exists on disk");
    let meta = iota::session::SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.title, "my chat", "an explicit /save title settles");
    assert_eq!(meta.context_window, 200_000, "the window is stamped");
    // …and so is where each layered parameter came from, exactly as a bundle that existed from the start
    // gets it: a `/save` bundle must resume like any other (brain page `model-param-layering`).
    assert_eq!(
        meta.sources().context_window,
        iota::session::ParamSource::Config
    );
    assert!(meta.records_params(), "the record is complete: {meta:?}");
    // The LIVE provider settles the model, not whatever the factory was minted with —
    // Go reads the provider inside the factory (root.go:360-366), so a mid-chat `/model`
    // change must reach the meta. The factory here deliberately says "gpt-test".
    assert_eq!(
        meta.model, "gpt-4o",
        "the live model is stamped over the factory's"
    );
    // The whole turn landed even though the watermark never advanced while ephemeral.
    let log = std::fs::read_to_string(dir.join("messages.jsonl")).expect("log");
    assert_eq!(log.lines().count(), 2, "the whole backlog in one append");
}

// ---------------------------------------------------------------------------
// /status and /tools
// ---------------------------------------------------------------------------

/// Capability-gated rows, a padded bold name column, and the
/// token-less shape this tier renders (T-10).
#[tokio::test]
async fn status_renders_the_capability_rows() {
    let f = Fixture::new(vec![
        input("/status"),
        Reply::Tabbed(TabbedResult::default()),
        Reply::Interrupted,
    ]);
    let session = f.session(None);
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.dispatch = Arc::new(StaticDispatcher::new(&["shell", "read_file"]));
    iota::repl::run(params).await.expect("exit");

    let view = &surfaces(&f.ui)[0].panels[0];
    assert_eq!(view.title, "Status");
    assert_eq!(view.kind, PanelKind::View);
    // The summary reduces a View's lines to a count; the row CONTENT is pinned beside
    // `status_lines` itself (src/commands/status.rs), which this asserts the wiring of.
    assert_eq!(
        view.line_count, 6,
        "Provider, Model, Messages, Tools, MCP, Session — no token rows in T1 (T-10)"
    );
}

/// Two live tabs on a 500 ms refresh, and the result discarded.
#[tokio::test]
async fn tools_opens_two_live_tabs() {
    let f = Fixture::new(vec![
        input("/tools"),
        Reply::Tabbed(TabbedResult::default()),
        Reply::Interrupted,
    ]);
    let session = f.session(None);
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.dispatch = Arc::new(StaticDispatcher::new(&["shell"]));
    iota::repl::run(params).await.expect("exit");

    let s = &surfaces(&f.ui)[0];
    assert_eq!(s.refresh_every_ms, 500);
    assert_eq!(s.panels.len(), 2);
    assert_eq!(s.panels[0].title, "Tools");
    assert_eq!(s.panels[1].title, "MCP");
    assert!(s.panels[1].wrap, "the MCP tab wraps");
    assert!(
        s.panels[0].has_refresh && s.panels[1].has_refresh,
        "both tabs re-render while open"
    );
}

// ---------------------------------------------------------------------------
// the MCP failure relay
// ---------------------------------------------------------------------------

/// A background connect failure lands in the scrollback once,
/// FIRST LINE ONLY; a successful connect says nothing — unless its merge skipped a
/// duplicate wire name, which is one dim notice per line (DIVERGENCES X-29; it used to be a
/// `tracing::warn!` nobody received).
#[tokio::test]
async fn mcp_failures_and_warnings_reach_the_transcript() {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let f = Fixture::new(vec![input(""), Reply::Interrupted]);
    let session = f.session(None);
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.mcp.events = Some(rx);
    tx.send(McpEvent {
        name: "docs".to_owned(),
        error: None,
        warnings: Vec::new(),
    })
    .await
    .expect("send");
    tx.send(McpEvent {
        name: "fs".to_owned(),
        error: Some("dial tcp: refused\nstack line 2".to_owned()),
        warnings: Vec::new(),
    })
    .await
    .expect("send");
    tx.send(McpEvent {
        name: "gh".to_owned(),
        error: None,
        warnings: vec!["duplicate wire tool name mcp__gh__echo, skipping".to_owned()],
    })
    .await
    .expect("send");
    drop(tx);
    iota::repl::run(params).await.expect("exit");

    // The reporter is a task: give it a scheduling window before asserting.
    tokio::task::yield_now().await;
    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"⚠ MCP fs failed: dial tcp: refused".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("docs")),
        "a successful connect says nothing: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("stack line 2")),
        "only the first line of the error is shown: {lines:?}"
    );
    assert!(
        lines.contains(&"⚠ MCP gh: duplicate wire tool name mcp__gh__echo, skipping".to_owned()),
        "the merge warning reaches the user: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// the store seam
// ---------------------------------------------------------------------------

/// A bucketed bundle
/// deletes by bare id, and the `projects/` container itself can never be removed (the
/// locator only matches real bundles).
#[test]
fn deleting_a_bucketed_session_removes_its_bundle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::new(tmp.path().join("sessions"));
    let root = Path::new("/work/p1");
    let mut w = store
        .create(ProviderKind::OpenAi, "m", None, "", "/work/p1", true, "")
        .expect("create in bucket");
    w.append_messages(&[Message::user("hi")])
        .expect("materialise");
    let id = w.id().to_owned();
    let dir = w.dir().to_path_buf();
    drop(w);
    assert!(dir.exists());
    assert_eq!(store.list(Some(root)).expect("scoped list").len(), 1);

    store.delete(&id).expect("delete");
    assert!(!dir.exists(), "the bundle survived delete");

    let projects: PathBuf = store.root().join("projects");
    assert!(projects.exists());
    assert!(
        store.delete("projects").is_err(),
        "deleting the projects container must fail"
    );
    assert!(projects.exists(), "the projects container is gone");
    // Path-escaping ids are refused before the locator ever runs.
    let err = store.delete("../etc").expect_err("escaping id");
    assert_eq!(err.to_string(), "invalid session id \"../etc\"");
}

// ---------------------------------------------------------------------------
// the message path: the title pass, the overlay probe, the persist watermark
// ---------------------------------------------------------------------------

/// The session is named at SEND time: the placeholder lands
/// synchronously and the model pass (on the SECOND provider instance) upgrades it. The
/// loop joins the pass before it exits, so both sinks have settled by then.
#[tokio::test]
async fn title_pass_names_the_session_on_the_second_provider() {
    let f = Fixture::new(vec![
        input("how do I profile allocations"),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    writer
        .append_messages(&[Message::user("materialise")])
        .expect("materialise");
    let dir = writer.dir().to_path_buf();
    let session = f.session(Some(writer));
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.title_provider = Some(Box::new(
        provider("gpt-4o", Ok(vec![])).replying("Profiling Allocations"),
    ));
    iota::repl::run(params).await.expect("exit");

    let titles: Vec<String> =
        f.ui.events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Title(t) => Some(t),
                _ => None,
            })
            .collect();
    assert_eq!(
        titles,
        [
            "iota".to_owned(),                         // the pre-loop fallback
            "how do I profile allocations".to_owned(), // the synchronous placeholder
            "Profiling Allocations".to_owned(),        // the model pass
        ]
    );
    let meta = iota::session::SessionMeta::read(&dir).expect("meta");
    assert_eq!(meta.title, "Profiling Allocations");
}

/// The overlay is re-probed before every
/// send, but the reload notices fire ONLY on a real change, and a changed skill catalog
/// re-issues the command table.
#[tokio::test]
async fn overlay_refresh_notices_fire_only_on_change() {
    let f = Fixture::new(vec![input("one"), input("two"), Reply::Interrupted]);
    let root = f.store.root().parent().expect("tmp root").to_path_buf();
    std::fs::write(root.join("AGENTS.md"), "RULES v1").expect("write AGENTS.md");
    let session = f.session(None);
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.agent = iota::chat::AgentOptions {
        enabled: true,
        root: root.clone(),
        cwd: Some(root.clone()),
        home: None,
    };
    // The first turn rewrites the chain, so the SECOND message's probe sees a change.
    let touched = root.clone();
    params.provider = Box::new(provider("gpt-4o", Ok(vec![])).on_call(move |n, _| {
        if n == 1 {
            std::fs::write(touched.join("AGENTS.md"), "RULES v2").expect("rewrite");
            let t = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
            let times = std::fs::FileTimes::new().set_modified(t).set_accessed(t);
            std::fs::File::options()
                .write(true)
                .open(touched.join("AGENTS.md"))
                .expect("open")
                .set_times(times)
                .expect("stamp");
        }
    }));
    iota::repl::run(params).await.expect("exit");

    let reloads: Vec<String> = printed(&f.ui)
        .into_iter()
        .filter(|l| l.starts_with("AGENTS.md reloaded"))
        .collect();
    assert_eq!(
        reloads,
        ["AGENTS.md reloaded (1 files)"],
        "exactly one notice, for the ONE turn that changed the chain"
    );
    // The banner reported the chain that was loaded at startup.
    assert!(
        printed(&f.ui)
            .iter()
            .any(|l| l.starts_with("Agent mode: AGENTS.md loaded (1 files,")),
        "{:?}",
        printed(&f.ui)
    );
}

/// A failed append warns and does NOT advance the watermark, so
/// the next successful persist carries the WHOLE backlog.
#[tokio::test]
async fn persist_warns_and_retries_the_backlog() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let jail = tmp.path().join("jail");
    std::fs::create_dir(&jail).expect("mkdir jail");
    let store = SessionStore::new(jail.join("sessions"));
    let writer = store
        .create(ProviderKind::OpenAi, "gpt-test", None, "", "", false, "")
        .expect("create");
    let dir = writer.dir().to_path_buf();
    set_mode(&jail, 0o555);
    if std::fs::create_dir(jail.join("probe")).is_ok() {
        eprintln!("SKIP: this user can write through a read-only directory (root?)");
        return;
    }

    let ui = ScriptedUi::new(vec![
        input("first question"),
        input("second question"),
        Reply::Interrupted,
    ]);
    let unjail = jail.clone();
    let provider = provider("gpt-4o", Ok(vec![])).on_call(move |n, _| {
        if n == 2 {
            set_mode(&unjail, 0o755); // the disk comes back before the second persist
        }
    });
    let params = RunParams {
        ui: Arc::clone(&ui) as Arc<dyn Ui>,
        provider: Box::new(provider),
        title_provider: None,
        system: String::new(),
        imported_history: Vec::new(),
        dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
        jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
        mcp: no_mcp(),
        session: SessionCtx {
            writer: Some(writer),
            store,
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
    };
    iota::repl::run(params).await.expect("exit");

    let warnings: Vec<String> = printed(&ui)
        .into_iter()
        .filter(|l| l.starts_with("Warning: failed to save session:"))
        .collect();
    assert_eq!(warnings.len(), 1, "one warning, for the turn that failed");
    let log = std::fs::read_to_string(dir.join("messages.jsonl")).expect("log");
    assert_eq!(
        log.lines().count(),
        4,
        "the retried persist carries BOTH turns: {log}"
    );
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}
