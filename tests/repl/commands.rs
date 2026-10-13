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
use iota::session::{NewSession, SessionStore, SessionWriter};
use iota::testing::{FakeProvider, Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::text::width::str_width;
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

    /// A bundle on the provider the fixture's catalog names — none (`ModelCatalog::default()`), so `/session`
    /// resumes it where the chat already is.
    fn writer(&self) -> SessionWriter {
        self.store
            .create(NewSession {
                provider: String::new(),
                ..NewSession::new(ProviderKind::OpenAi, "gpt-test")
            })
            .expect("create writer")
    }

    fn params(&self, provider: FakeProvider, session: SessionCtx) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs::default(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: no_mcp(),
            session,
            params: iota::session::LayeredParams::default(),
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            endpoints: None,
            agent: iota::headless::AgentOptions::default(),
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
        let cfg =
            iota::cmd::Config::parse(yaml.as_bytes(), &iota::app::env::Env::default(), &mut |w| {
                panic!("unexpected config warning: {w}")
            })
            .expect("the config loads");
        let resolved = cfg.resolve_agent("default").expect("the agent resolves");
        let catalog = iota::repl::ModelCatalog::new(
            &cfg,
            &resolved,
            &iota::app::env::Env::default(),
            &iota::provider::HttpTransport::from(iota::llm::default_http_client()),
        );
        RunParams {
            catalog,
            endpoints: None,
            ..self.params(provider, session)
        }
    }

    fn session(&self, writer: Option<SessionWriter>) -> SessionCtx {
        SessionCtx {
            writer,
            store: self.store.clone(),
            new_session: None,
            scope: None,
            bot: false,
            notices: Vec::new(),
            recorded_notices: Vec::new(),
            memory: None,
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
    McpHooks::default()
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

/// A row of the banner card with its frame taken off: the edges, the blank beside each, and
/// the padding to the card's widest row.
fn card_row(row: &str) -> String {
    row.strip_prefix("│ ")
        .and_then(|r| r.strip_suffix(" │"))
        .unwrap_or_else(|| panic!("not a card row: {row:?}"))
        .trim_end()
        .to_owned()
}

/// The banner's mode row, bare: the card's second row.
fn mode_row(ui: &ScriptedUi) -> String {
    card_row(&printed(ui)[2])
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

/// The completion row the composer was handed: the values, in order.
fn completion_row(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .find_map(|e| match e {
            UiEvent::Commands(c) => Some(c),
            _ => None,
        })
        .expect("the loop publishes the command table")
        .into_iter()
        .map(|s| s.value)
        .collect()
}

/// The banner card — the mark and the version, the mode row and the directory, in a frame
/// that hugs the widest of them — then ONE blank between the environment and the first
/// transcript block. The ONE-TABLE law is the completion row's (X-46): what `/` offers is
/// exactly the registered table, in dispatch order.
#[tokio::test]
async fn banner_order_and_completion_row() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let writer = f.writer();
    let id = writer.id().to_owned();
    let session = f.session(Some(writer));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("clean exit");

    // No agent options: the directory row is the process cwd, and with no home nothing is
    // shortened.
    let cwd = std::env::current_dir().expect("cwd").display().to_string();
    let rows = [
        format!("ι> iota  v{}", env!("CARGO_PKG_VERSION")),
        format!("chat · session {id}"),
        cwd,
    ];
    let inner = rows.iter().map(|r| str_width(r)).max().expect("three rows");
    let edge = "─".repeat(inner + 2);
    let lines = printed(&f.ui);
    assert_eq!(
        lines,
        vec![
            format!("╭{edge}╮"),
            format!("│ {:<inner$} │", rows[0]),
            format!("│ {:<inner$} │", rows[1]),
            format!("│ {:<inner$} │", rows[2]),
            format!("╰{edge}╯"),
            String::new(),
        ]
    );
    assert_eq!(
        completion_row(&f.ui),
        [
            "/file", "/session", "/model", "/export", "/status", "/tools", "/debug"
        ]
    );
}

/// An ephemeral chat's mode row says how to keep it, and `/save` joins the completion row.
#[tokio::test]
async fn banner_offers_save_for_an_ephemeral_chat() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    let store = f.store.clone();
    let session = SessionCtx {
        writer: None,
        store: f.store.clone(),
        new_session: Some(Box::new(move || {
            store.create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
        })),
        scope: None,
        bot: false,
        notices: Vec::new(),
        recorded_notices: Vec::new(),
        memory: None,
    };
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("clean exit");

    assert_eq!(mode_row(&f.ui), "chat · not saved · /save keeps it");
    assert_eq!(
        completion_row(&f.ui),
        [
            "/file", "/session", "/model", "/export", "/status", "/tools", "/debug", "/save"
        ]
    );
}

/// `/jobs` is the one row that flips at run time, and with no background job it is not in the table —
/// so typing it is a message like any unregistered `/xyz`: no panel, a `❯` block, the model's answer.
/// (The other half — a running job puts the row in the table and the command opens the Jobs panel — is
/// `tests/repl/jobs.rs`, which can start a real job.)
#[tokio::test]
async fn jobs_is_a_plain_message_while_nothing_runs() {
    let f = Fixture::new(vec![input("/jobs"), Reply::Interrupted]);
    let writer = f.writer();
    let session = f.session(Some(writer));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("clean exit");

    assert!(
        !completion_row(&f.ui).iter().any(|v| v == "/jobs"),
        "/jobs advertised with nothing running"
    );
    assert!(
        surfaces(&f.ui).is_empty(),
        "a panel opened for an unregistered command"
    );
    assert!(
        f.ui.events()
            .iter()
            .any(|e| matches!(e, UiEvent::UserBlock(s) if s == "/jobs")),
        "the text was not sent as a message"
    );
    assert!(printed(&f.ui).iter().any(|l| l.contains("an answer")));
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
/// reachable twice is one row. The set spans two endpoints, so every row names its own — the current one too —
/// a row carries the name of the choice that serves it, and a row on another endpoint says what picking it does
/// to this chat (this one has no endpoint factory, so it cannot move: a new session).
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
            // the `models:` entry, which is also what the chat is running, with the name it is chosen by
            "mock:a-model [m] (current)",
            // an inline reference to ANOTHER endpoint: written as `-M` writes it, and nothing about what picking
            // it does — in this chat, which never moves, the pick is refused and says so
            "relay:vendor/y",
            // the wildcard's own listing, minus the id the entry already offered
            "mock:b-model",
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

/// The combo's field reads a typed word as `-M` reads it: a choice's NAME is the model its entry stands for,
/// never a wire id spelled like the name — and the row says that name, so what the user sees is what they type.
/// `provider:*` names no model, so committing it changes nothing.
#[tokio::test]
async fn model_reads_a_typed_choice_by_name() {
    const NAMED: &str = r"
providers:
  mock: {type: openai, key: k}
models:
  m: mock:a-model
  fast: mock:b-model
agents:
  default:
    model: m
    choices: [m, fast]
";
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            // One past the two rows: the typed text.
            cursor: 2,
            text: "fast".to_owned(),
            ..PanelResult::default()
        }),
        input("/model"),
        commit(PanelResult {
            cursor: 2,
            text: "mock:*".to_owned(),
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    writer
        .append_messages(&[Message::user("earlier")])
        .expect("materialise");
    let dir = writer.dir().to_path_buf();
    let session = f.session(Some(writer));
    let provider = provider("a-model", Ok(vec![]));
    let params = f.params_with_catalog(provider, session, NAMED);
    iota::repl::run(params).await.expect("exit");

    // One endpoint, so the rows are bare; each carries the name it is chosen by.
    assert_eq!(
        surfaces(&f.ui)[0].panels[0].items,
        ["a-model [m] (current)", "b-model [fast]"]
    );
    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"Model switched to b-model".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("switched to fast")),
        "{lines:?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("No changes."),
        "`mock:*` names no model: {lines:?}"
    );
    let meta = iota::session::SessionMeta::read(&dir).expect("meta written");
    assert_eq!(meta.model, "b-model");
}

/// A row on another provider is listed but not taken once a model has replied: the history it would carry
/// over is that dialect's, so the picker says so and names the command that would start a run there, rather
/// than half-applying a switch whose next request would go to the wrong API — and no endpoint is built.
/// Two endpoints of different types; `relay` declares its model's window, which a move must pick up.
const TWO_ENDPOINTS: &str = r#"
providers:
  mock: {type: openai, key: k}
  relay: {type: openresponses, key: k, url: "http://127.0.0.1:9"}
models:
  m: mock:a-model
  y: {provider: relay, id: vendor/y, context_window: 1m}
agents:
  default:
    model: m
    choices: [m, y]
"#;

/// The call logs of what a [`relay_endpoints`] factory builds.
#[derive(Clone, Default)]
struct Built {
    /// The conversation instance's calls.
    main: iota::testing::Log,
    /// The title twin's calls.
    title: iota::testing::Log,
    /// The `provider:id` pairs asked for.
    asked: Arc<std::sync::Mutex<Vec<String>>>,
}

/// The endpoint factory the binary would hand in, over `yaml`, with fakes standing in for the providers:
/// `relay` answers as an `openresponses` endpoint, any name the config does not know is refused the way the
/// real factory refuses it. The layers and the catalog are the real ones, built for the run moved the way
/// `-M provider:id` moves it.
fn relay_endpoints(yaml: &str, built: &Built) -> iota::repl::EndpointFactory {
    let cfg =
        iota::cmd::Config::parse(yaml.as_bytes(), &iota::app::env::Env::default(), &mut |w| {
            panic!("unexpected config warning: {w}")
        })
        .expect("the config loads");
    let built = built.clone();
    Box::new(move |provider, id, _warn| {
        built.asked.lock().unwrap().push(format!("{provider}:{id}"));
        if !cfg.knows_provider(provider) {
            return Err(format!(
                "provider {provider:?} is not defined in the config"
            ));
        }
        let mut resolved = cfg.resolve_agent("default").expect("the agent resolves");
        resolved.move_to_provider(&cfg, provider);
        // As `-M provider:id` resolves it: the `models:` entry serving that pair, with its own knobs.
        if let Some((name, entry)) = cfg
            .models
            .iter()
            .find(|(name, m)| m.provider_or(name) == provider && m.id == id)
        {
            let mut entry = entry.clone();
            entry.anchor_provider(name);
            resolved.model = entry;
        }
        let fake = |log: &iota::testing::Log, reply: &str| {
            FakeProvider::new()
                .with_kind(ProviderKind::OpenResponses)
                .with_model(id)
                .reporting_usage()
                .replying(reply)
                .with_log(log.clone())
        };
        Ok(iota::repl::Endpoint {
            // The turn takes 50 ms so the title pass, which rides beside it, settles before the exit.
            provider: Box::new(
                fake(&built.main, "moved answer")
                    .answering_after(std::time::Duration::from_millis(50)),
            ),
            title_provider: Some(Box::new(fake(&built.title, "A moved title"))),
            layers: iota::cmd::ParamLayers::new(&cfg, &resolved),
            catalog: iota::repl::ModelCatalog::new(
                &cfg,
                &resolved,
                &iota::app::env::Env::default(),
                &iota::provider::HttpTransport::from(iota::llm::default_http_client()),
            ),
        })
    })
}

/// Before its first reply a chat MOVES to a row on another provider — the end state of `-M relay:vendor/y`:
/// the next send goes to the new endpoint, the title pass rides the new endpoint's own instance with the new
/// model (never the old instance with the new model's id — codex P7), the declarations the switch reads are
/// the new endpoint's (its window lands), the picker is relative to it, and the bundle names it.
#[tokio::test]
async fn model_moves_a_chat_with_no_reply_to_another_provider() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        input("hello"),
        input("/model"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let old = provider("a-model", Ok(vec![])).reporting_usage();
    let old_log = old.log();
    let old_title = FakeProvider::new().replying("An old title");
    let old_title_log = old_title.log();
    let built = Built::default();
    let mut params = f.params_with_catalog(old, session, TWO_ENDPOINTS);
    params.title_provider = Some(Box::new(old_title));
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Context window: ≈0 / 1m (0%)".to_owned()),
        "the relay entry's window landed: {lines:?}"
    );
    assert_eq!(*built.asked.lock().unwrap(), ["relay:vendor/y"]);
    // The send went to the new endpoint, with the history the chat held.
    assert_eq!(old_log.calls(), 0, "nothing reached the old endpoint");
    let sent = built.main.records();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].model, "vendor/y");
    assert!(sent[0].messages.iter().any(|m| m.content == "hello"));
    // P7: the title pass rode the new endpoint's twin, with the new model.
    assert_eq!(
        old_title_log.calls(),
        0,
        "the old title instance was not asked"
    );
    let titles = built.title.records();
    assert_eq!(titles.len(), 1);
    assert_eq!(titles[0].model, "vendor/y");
    // Every row keeps its name across the move — `relay:`, never a foretelling of what picking it does (the
    // switch above said where the chat went); only `(current)` follows the chat.
    let opened = surfaces(&f.ui);
    assert_eq!(
        opened[0].panels[0].items,
        ["mock:a-model [m] (current)", "relay:vendor/y [y]"]
    );
    assert_eq!(
        opened[1].panels[0].items,
        ["mock:a-model [m]", "relay:vendor/y [y] (current)"]
    );
    // The bundle names the endpoint the reply came from.
    let infos = f.store.list(None).expect("list");
    assert_eq!(infos.len(), 1);
    let meta = iota::session::SessionMeta::read(&f.store.dir(&infos[0].id).unwrap()).unwrap();
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("relay", "vendor/y")
    );
}

/// The dispatcher is not rebuilt by a move, so the target's layers keep the `defer_mode` it was ASSEMBLED with:
/// a target model that asks for another mode is told it does not get it — exactly as a switch on one endpoint
/// says so — rather than the layers pretending the target's own mode is running.
#[tokio::test]
async fn a_move_keeps_the_dispatchers_defer_mode() {
    const DEFER: &str = r#"
providers:
  mock: {type: openai, key: k}
  relay: {type: openresponses, key: k, url: "http://127.0.0.1:9"}
models:
  m: mock:a-model
  y: {provider: relay, id: vendor/y, defer_mode: tool-search}
agents:
  default:
    model: m
    choices: [m, y]
"#;
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        session,
        DEFER,
    );
    params.layers = {
        let cfg = iota::cmd::Config::parse(
            DEFER.as_bytes(),
            &iota::app::env::Env::default(),
            &mut |w| panic!("unexpected config warning: {w}"),
        )
        .expect("the config loads");
        iota::cmd::ParamLayers::new(&cfg, &cfg.resolve_agent("default").expect("the agent"))
    };
    params.endpoints = Some(relay_endpoints(DEFER, &built));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l.starts_with(
            "Note: vendor/y asks for defer_mode \"tool-search\"; this session keeps \"normal\""
        )),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
}

/// The move is ONE write, made before anything is installed: when an existing bundle cannot be told — its
/// meta cannot be written — the chat does not move. It says so, the bundle still names where it was, and the
/// next send goes to the endpoint the chat was on. (A system-only bundle is on disk before any reply: a
/// `--no-save` chat that `/save`d before its first send has one.)
#[tokio::test]
async fn a_move_the_bundle_cannot_record_is_not_made() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        input("hello"),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    writer
        .append_messages(&[Message::system("be brief")])
        .expect("put the bundle on disk");
    let dir = writer.dir().to_path_buf();
    // The meta's atomic rewrite goes through `meta.json.tmp`: a directory there makes every write fail.
    std::fs::create_dir(dir.join(iota::session::META_TMP_FILE)).expect("block the meta");
    let session = f.session(Some(writer));
    let old = provider("a-model", Ok(vec![])).reporting_usage();
    let old_log = old.log();
    let built = Built::default();
    let mut params = f.params_with_catalog(old, session, TWO_ENDPOINTS);
    params.system = "be brief".to_owned();
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Error: the session could not be moved: ")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Model switched")),
        "{lines:?}"
    );
    assert_eq!(old_log.calls(), 1, "the send stayed on the old endpoint");
    assert_eq!(built.main.calls(), 0);
    let meta = iota::session::SessionMeta::read(&dir).expect("meta");
    assert_eq!(
        meta.provider, "",
        "the bundle still names where the chat is"
    );
}

/// The `/model` surface's capability tabs were opened on the endpoint a move leaves. Committed afterwards, one
/// the target has no place for lands nowhere — no "Image generation: on", no `image: true` in the bundle —
/// even though the tab still shows the old endpoint's switch on (codex review F7).
#[tokio::test]
async fn a_move_does_not_claim_a_capability_the_target_lacks() {
    let all_on = || PanelResult {
        on: true,
        ..PanelResult::default()
    };
    let f = Fixture::new(vec![
        input("/model"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 0,
            panels: std::iter::once(PanelResult {
                cursor: 1,
                ..PanelResult::default()
            })
            .chain(std::iter::repeat_with(all_on).take(6))
            .collect(),
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(Some(f.writer()));
    let old = provider("a-model", Ok(vec![])).with_image_output(true);
    let mut params = f.params_with_catalog(old, session, TWO_ENDPOINTS);
    params.endpoints = Some(Box::new(|_: &str, id: &str, _: &mut dyn FnMut(String)| {
        Ok(iota::repl::Endpoint {
            provider: Box::new(FakeProvider::new().with_model(id)),
            title_provider: None,
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
        })
    }));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Image generation")),
        "{lines:?}"
    );
}

/// The startup pick moves the same way: a chat with no model yet takes a row on another endpoint.
#[tokio::test]
async fn the_startup_pick_moves_to_another_provider() {
    let f = Fixture::new(vec![
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let session = f.session(None);
    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("", Ok(vec![])).reporting_usage(),
        session,
        TWO_ENDPOINTS,
    );
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"Using model: vendor/y on relay".to_owned()),
        "{lines:?}"
    );
}

/// A move that cannot be built — here an endpoint of another kind (no token accounting where the chat has
/// it), and one whose construction fails — is reported and changes NOTHING: the next send still goes to the
/// endpoint the chat is on.
#[tokio::test]
async fn a_move_that_cannot_be_made_changes_nothing() {
    for (factory, said) in [
        (
            Box::new(|_: &str, _: &str, _: &mut dyn FnMut(String)| {
                Ok(iota::repl::Endpoint {
                    provider: Box::new(FakeProvider::new().with_model("vendor/y")),
                    title_provider: None,
                    layers: iota::cmd::ParamLayers::default(),
                    catalog: iota::repl::ModelCatalog::default(),
                })
            }) as iota::repl::EndpointFactory,
            "relay:vendor/y runs on another provider; it is another kind of model",
        ),
        (
            Box::new(|_: &str, _: &str, _: &mut dyn FnMut(String)| {
                Err(
                    "API key is required: set OPENAI_API_KEY or providers.relay.key in your config"
                        .to_owned(),
                )
            }) as iota::repl::EndpointFactory,
            "Error: API key is required",
        ),
    ] {
        let f = Fixture::new(vec![
            input("/model"),
            commit(PanelResult {
                cursor: 1,
                ..PanelResult::default()
            }),
            input("hello"),
            Reply::Interrupted,
        ]);
        let session = f.session(Some(f.writer()));
        let old = provider("a-model", Ok(vec![])).reporting_usage();
        let old_log = old.log();
        let mut params = f.params_with_catalog(old, session, TWO_ENDPOINTS);
        params.endpoints = Some(factory);
        iota::repl::run(params).await.expect("exit");
        let lines = printed(&f.ui);
        assert!(lines.iter().any(|l| l.starts_with(said)), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.starts_with("Model switched")),
            "{lines:?}"
        );
        assert_eq!(old_log.calls(), 1, "the send stayed on the old endpoint");
    }
}

/// An ephemeral chat that moved mints its bundle on the endpoint it moved to: `/save` records the new
/// provider, not the one the chat started on.
#[tokio::test]
async fn save_after_a_move_mints_the_bundle_on_the_new_provider() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        input("/save"),
        input("hello"),
        Reply::Interrupted,
    ]);
    let store = f.store.clone();
    let session = SessionCtx {
        new_session: Some(Box::new(move || {
            store.create(NewSession {
                provider: "mock".to_owned(),
                ..NewSession::new(ProviderKind::OpenAi, "a-model")
            })
        })),
        ..f.session(None)
    };
    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        session,
        TWO_ENDPOINTS,
    );
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");
    let infos = f.store.list(None).expect("list");
    assert_eq!(infos.len(), 1, "{:?}", printed(&f.ui));
    let meta = iota::session::SessionMeta::read(&f.store.dir(&infos[0].id).unwrap()).unwrap();
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("relay", "vendor/y")
    );
}

/// `/session` resumes a bundle on the provider it records, as `iota resume` does: the chat moves there
/// first — and a bundle on a provider the config does not define is refused, with nothing swapped.
#[tokio::test]
async fn session_resumes_a_bundle_on_its_own_provider() {
    for (provider_name, moved) in [("relay", true), ("gone", false)] {
        let f = Fixture::new(vec![
            input("/session"),
            Reply::Tabbed(TabbedResult {
                cancelled: false,
                focused: 0,
                panels: vec![PanelResult::default(), PanelResult::default()],
            }),
            input("hello"),
            Reply::Interrupted,
        ]);
        let mut other = f
            .store
            .create(NewSession {
                provider: provider_name.to_owned(),
                ..NewSession::new(ProviderKind::OpenResponses, "vendor/y")
            })
            .unwrap();
        other
            .append_messages(&[Message::user("older"), Message::assistant("answer")])
            .unwrap();
        let other_id = other.id().to_owned();
        drop(other);
        let session = f.session(None);
        let old = provider("a-model", Ok(vec![])).reporting_usage();
        let old_log = old.log();
        let built = Built::default();
        let mut params = f.params_with_catalog(old, session, TWO_ENDPOINTS);
        params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
        iota::repl::run(params).await.expect("exit");

        let lines = printed(&f.ui);
        // A provider the config does not define is refused before any endpoint is built.
        let asked: &[String] = if moved {
            &[format!("{provider_name}:vendor/y")]
        } else {
            &[]
        };
        assert_eq!(*built.asked.lock().unwrap(), asked);
        let resumed = lines
            .iter()
            .any(|l| l.starts_with(&format!("Resumed session {other_id}")));
        assert_eq!(resumed, moved, "{lines:?}");
        if moved {
            assert_eq!(old_log.calls(), 0);
            assert_eq!(built.main.records()[0].model, "vendor/y");
        } else {
            assert!(
                lines.contains(&format!(
                    "Error: session {other_id} runs on provider \"gone\", which the config does not define; \
                     resume it elsewhere with: iota resume {other_id} -M <provider>:<model>"
                )),
                "{lines:?}"
            );
            assert_eq!(old_log.calls(), 1, "the chat stayed where it was");
        }
    }
}

/// `/session` onto a bundle whose provider is another KIND of model than the chat was started for: nothing is
/// swapped, and the way out named is resuming THAT bundle in a new run — not starting a new session.
#[tokio::test]
async fn session_onto_another_kind_of_model_points_at_resume() {
    let f = Fixture::new(vec![
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: false,
            focused: 0,
            panels: vec![PanelResult::default(), PanelResult::default()],
        }),
        Reply::Interrupted,
    ]);
    let mut other = f
        .store
        .create(NewSession {
            provider: "relay".to_owned(),
            ..NewSession::new(ProviderKind::OpenResponses, "vendor/y")
        })
        .unwrap();
    other
        .append_messages(&[Message::user("older"), Message::assistant("answer")])
        .unwrap();
    let other_id = other.id().to_owned();
    drop(other);
    // The chat counts no tokens; the relay's endpoint does.
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])),
        f.session(None),
        TWO_ENDPOINTS,
    );
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &Built::default()));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l
            .starts_with("relay:vendor/y runs on another provider; it is another kind of model")
            && l.ends_with(&format!(
                "Nothing was switched. It resumes there in a new run: iota resume {other_id}"
            ))),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with(&format!("Resumed session {other_id}"))),
        "{lines:?}"
    );
}

/// "Has replied" is a fact of the SESSION, not of the view: a resumed bundle whose compaction left no assistant
/// message in the view has still been replied in — its log says so — and is asked before it moves like any
/// other (codex review F6). What it would lose is counted on the view, which holds no reply.
#[tokio::test]
async fn a_compacted_session_is_not_a_new_one() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        agree(false),
        Reply::Interrupted,
    ]);
    let mut w = f.writer();
    w.append_messages(&[
        Message::user("first"),
        Message::assistant("an answer"),
        Message::user("second"),
    ])
    .unwrap();
    // A compaction that retains only the last user message: the view is the summary and that message.
    w.append_compaction("we talked", 1, None, false).unwrap();
    let id = w.id().to_owned();
    drop(w);
    let (writer, session) = f.store.resume(&id, ProviderKind::OpenAi).unwrap();
    assert!(
        !session
            .messages
            .iter()
            .any(|m| m.role() == iota::provider::model::Role::Assistant),
        "the view shows no reply"
    );
    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        f.session(Some(writer)),
        TWO_ENDPOINTS,
    );
    params.imported_history = session.messages;
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.contains(
            &"No earlier reply carries payload that openresponses does not replay.".to_owned()
        ),
        "{lines:?}"
    );
    assert_eq!(
        surfaces(&f.ui)[1].panels[0].title,
        "Move this chat to relay:vendor/y?"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Model switched")),
        "declined: {lines:?}"
    );
}

/// A raw payload for a test record.
fn raw(s: &str) -> iota::provider::model::Raw {
    iota::provider::model::Raw::from_string(s.to_owned()).expect("valid JSON")
}

/// A chat-completions reply carrying its dialect's own payload.
fn openai_reply(content: &str) -> Message {
    Message::assistant_with_calls(
        content,
        Vec::new(),
        Some(iota::provider::model::RawContent::OpenAi(raw(&format!(
            r#"{{"role":"assistant","content":"{content}"}}"#
        )))),
    )
}

/// A saved bundle on `mock` whose model has replied (with chat-completions payload), resumed: the writer and
/// the view a run takes over.
fn replied_bundle(f: &Fixture) -> (SessionWriter, Vec<Message>) {
    let mut w = f
        .store
        .create(NewSession {
            provider: "mock".to_owned(),
            ..NewSession::new(ProviderKind::OpenAi, "a-model")
        })
        .unwrap();
    w.append_messages(&[Message::user("q"), openai_reply("a")])
        .unwrap();
    let id = w.id().to_owned();
    drop(w);
    let (writer, session) = f.store.resume(&id, ProviderKind::OpenAi).unwrap();
    (writer, session.messages)
}

/// The meta of the one bundle in the store.
fn only_meta(f: &Fixture) -> iota::session::SessionMeta {
    let infos = f.store.list(None).expect("list");
    assert_eq!(infos.len(), 1);
    iota::session::SessionMeta::read(&f.store.dir(&infos[0].id).unwrap()).unwrap()
}

/// The confirmation a move asks for: the move, or staying where the chat is.
fn agree(yes: bool) -> Reply {
    commit(PanelResult {
        cursor: usize::from(!yes),
        ..PanelResult::default()
    })
}

/// A chat a model has replied in moves to a row on another provider as `iota resume <id> -M relay:vendor/y`
/// would move it — once the user agreed. Before asking, three things are said: what the target does not replay
/// (how many replies, and what of them), where `iota resume` continues the chat afterwards, and the way back.
/// Agreed, the history goes to the new endpoint and the bundle names it; declined, nothing changes and the
/// next send stays where it was.
#[tokio::test]
async fn a_chat_that_has_replied_moves_once_the_user_agrees() {
    for yes in [true, false] {
        let f = Fixture::new(vec![
            input("/model"),
            commit(PanelResult {
                cursor: 1,
                ..PanelResult::default()
            }),
            agree(yes),
            input("hello"),
            Reply::Interrupted,
        ]);
        let (writer, history) = replied_bundle(&f);
        let id = writer.id().to_owned();
        let old = provider("a-model", Ok(vec![])).reporting_usage();
        let old_log = old.log();
        let built = Built::default();
        let mut params = f.params_with_catalog(old, f.session(Some(writer)), TWO_ENDPOINTS);
        params.imported_history = history;
        params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
        iota::repl::run(params).await.expect("exit");

        let lines = printed(&f.ui);
        for said in [
            "1 earlier reply carries openai-specific payload (reasoning, provider-side tool state) that openresponses does not replay; the model sees their text and tool calls only.".to_owned(),
            format!("Once moved, iota resume {id} continues this chat on relay:vendor/y."),
            "mock:a-model stays reachable: picking it again moves the chat back, and the replies made there replay their payload again.".to_owned(),
        ] {
            assert!(lines.contains(&said), "{said:?} in {lines:?}");
        }
        let asked = &surfaces(&f.ui)[1].panels[0];
        assert_eq!(asked.title, "Move this chat to relay:vendor/y?");
        assert_eq!(asked.items, ["Move it", "Stay on mock:a-model"]);
        assert_eq!(
            lines.contains(&"Model switched to vendor/y on relay".to_owned()),
            yes,
            "{lines:?}"
        );
        let meta = only_meta(&f);
        if yes {
            assert_eq!(old_log.calls(), 0, "nothing reached the old endpoint");
            let sent = built.main.send(0);
            let said: Vec<&str> = sent
                .iter()
                .filter(|m| m.role() != iota::provider::model::Role::System)
                .map(|m| m.content.as_str())
                .collect();
            assert_eq!(said, ["q", "a", "hello"]);
            assert_eq!(
                (meta.provider.as_str(), meta.model.as_str()),
                ("relay", "vendor/y")
            );
        } else {
            assert_eq!(old_log.calls(), 1, "the send stayed on the old endpoint");
            assert_eq!(built.main.calls(), 0);
            assert_eq!(
                meta.provider, "mock",
                "the bundle still names where the chat is"
            );
        }
    }
}

/// The view a run holds was read from the log for the type it runs on: a session that once ran on the target's
/// dialect — here moved to `relay` by `iota resume -M`, replied there, then resumed back on `mock` — has that
/// reply in its log WITH its payload, and in the view without it. A move reads the log again for the target,
/// as `iota resume -M relay:vendor/y` would: the reply relay made goes back to relay with its reasoning item,
/// and only the reply made on `mock` is counted as not replayed.
#[tokio::test]
async fn a_move_back_to_a_dialect_the_session_ran_on_replays_its_payload() {
    const REASONING: &str =
        r#"{"id":"rs_2","type":"reasoning","summary":[],"encrypted_content":"OPAQUE"}"#;
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        agree(true),
        input("hello"),
        Reply::Interrupted,
    ]);
    let to = |kind, provider: &str, model: &str| iota::session::Moved {
        kind,
        provider: provider.to_owned(),
        model: model.to_owned(),
        params: iota::session::LayeredParams::default(),
        window: None,
        image: iota::session::ImageSettings::default(),
    };
    let mut w = f
        .store
        .create(NewSession {
            provider: "mock".to_owned(),
            ..NewSession::new(ProviderKind::OpenAi, "a-model")
        })
        .unwrap();
    w.append_messages(&[Message::user("q1"), openai_reply("a1")])
        .unwrap();
    w.move_to(&to(ProviderKind::OpenResponses, "relay", "vendor/y"))
        .unwrap();
    let relays = iota::provider::model::RawContent::OpenResponses(vec![raw(REASONING)]);
    w.append_messages(&[
        Message::user("q2"),
        Message::assistant_with_calls("a2", Vec::new(), Some(relays.clone())),
    ])
    .unwrap();
    w.move_to(&to(ProviderKind::OpenAi, "mock", "a-model"))
        .unwrap();
    let id = w.id().to_owned();
    drop(w);
    let (writer, session) = f.store.resume(&id, ProviderKind::OpenAi).unwrap();
    let reply = |history: &[Message], content: &str| {
        history
            .iter()
            .find(|m| m.content == content)
            .and_then(|m| m.raw_content().cloned())
    };
    assert_eq!(
        reply(&session.messages, "a2"),
        None,
        "loaded for mock, relay's reply lost its payload on the way in"
    );

    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        f.session(Some(writer)),
        TWO_ENDPOINTS,
    );
    params.imported_history = session.messages;
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"1 earlier reply carries openai-specific payload (reasoning, provider-side tool state) that openresponses does not replay; the model sees their text and tool calls only.".to_owned()),
        "only the reply made on mock is not replayed: {lines:?}"
    );
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
    let sent = built.main.send(0);
    assert_eq!(
        reply(&sent, "a2"),
        Some(relays),
        "relay's own reply goes back to it with its reasoning item"
    );
    assert_eq!(
        reply(&sent, "a1"),
        None,
        "mock's reply is not replayed there"
    );
}

/// A chat a model has replied in moves only once everything it holds is in its log, saved under the type it was
/// said in: a reply saved after the writer's type changed would keep its text and lose its payload. While the
/// save fails the move is refused, before any endpoint is built; once the disk is back the backlog is saved
/// first — the reply with its chat-completions payload — and the chat moves.
#[tokio::test]
async fn a_move_that_cannot_save_first_is_not_made() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let up = Arc::clone(&down);
    let f = Fixture::new(vec![
        input("hello"),
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Pause(Arc::new(move || {
            up.store(false, std::sync::atomic::Ordering::SeqCst);
        })),
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        agree(true),
        Reply::Interrupted,
    ]);
    let mut writer = f.writer();
    writer.fail_log_writes_while(Arc::clone(&down));
    let dir = writer.dir().to_path_buf();
    let old = provider("a-model", Ok(vec![]))
        .reporting_usage()
        .with_tools()
        .round(iota::testing::Round::result(iota::provider::RoundResult {
            content: "an answer".to_owned(),
            raw_content: openai_reply("an answer").raw_content().cloned(),
            ..iota::provider::RoundResult::default()
        }));
    let built = Built::default();
    let mut params = f.params_with_catalog(old, f.session(Some(writer)), TWO_ENDPOINTS);
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l.starts_with(
            "relay:vendor/y runs on another provider; what this chat holds could not be saved first"
        ) && l
            .ends_with("Nothing was switched. Pick the model again once the session saves.")),
        "{lines:?}"
    );
    assert_eq!(
        *built.asked.lock().unwrap(),
        ["relay:vendor/y"],
        "the refused pick built nothing"
    );
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
    let log = std::fs::read_to_string(dir.join("messages.jsonl")).expect("the log");
    let reply = log
        .lines()
        .find(|l| l.contains(r#""content":"an answer""#))
        .expect("the reply was saved");
    assert!(
        reply.contains(r#""raw":{"provider":"openai","#),
        "saved under the type it was said in: {reply}"
    );
}

/// An ephemeral chat a model has replied in has no log to save its replies to before a move — `/save`d after
/// it, they would lose their payload — so it is told to `/save` first; once it has, it moves like any other.
#[tokio::test]
async fn an_ephemeral_chat_that_has_replied_moves_once_saved() {
    let f = Fixture::new(vec![
        input("hello"),
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        input("/save"),
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        agree(true),
        Reply::Interrupted,
    ]);
    let store = f.store.clone();
    let session = SessionCtx {
        new_session: Some(Box::new(move || {
            store.create(NewSession {
                provider: "mock".to_owned(),
                ..NewSession::new(ProviderKind::OpenAi, "a-model")
            })
        })),
        ..f.session(None)
    };
    let built = Built::default();
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        session,
        TWO_ENDPOINTS,
    );
    params.endpoints = Some(relay_endpoints(TWO_ENDPOINTS, &built));
    iota::repl::run(params).await.expect("exit");

    let lines = printed(&f.ui);
    assert!(
        lines.contains(&"relay:vendor/y runs on another provider; this chat is not saved, and saved after the move its replies would lose their payload. Nothing was switched. Save it with /save, then pick the model again.".to_owned()),
        "{lines:?}"
    );
    assert_eq!(
        *built.asked.lock().unwrap(),
        ["relay:vendor/y"],
        "the first pick built nothing"
    );
    assert!(
        lines.contains(&"Model switched to vendor/y on relay".to_owned()),
        "{lines:?}"
    );
    let meta = only_meta(&f);
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("relay", "vendor/y")
    );
}

/// A chat a model has replied in cannot move to another KIND of model (token accounting, image generation) —
/// no confirmation re-installs what a run settles at startup. The way out named is THIS chat continued there
/// in a new run, which `iota resume -M` moves — not a new session.
#[tokio::test]
async fn a_replied_chat_onto_another_kind_of_model_points_at_resume_with_the_move() {
    let f = Fixture::new(vec![
        input("/model"),
        commit(PanelResult {
            cursor: 1,
            ..PanelResult::default()
        }),
        Reply::Interrupted,
    ]);
    let (writer, history) = replied_bundle(&f);
    let id = writer.id().to_owned();
    // The chat counts tokens; the endpoint built does not.
    let mut params = f.params_with_catalog(
        provider("a-model", Ok(vec![])).reporting_usage(),
        f.session(Some(writer)),
        TWO_ENDPOINTS,
    );
    params.imported_history = history;
    params.endpoints = Some(Box::new(|_: &str, id: &str, _: &mut dyn FnMut(String)| {
        Ok(iota::repl::Endpoint {
            provider: Box::new(FakeProvider::new().with_model(id)),
            title_provider: None,
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
        })
    }));
    iota::repl::run(params).await.expect("exit");
    let lines = printed(&f.ui);
    assert!(
        lines.iter().any(|l| l
            .starts_with("relay:vendor/y runs on another provider; it is another kind of model")
            && l.ends_with(&format!(
                "Nothing was switched. This chat continues there in a new run: iota resume {id} -M relay:vendor/y"
            ))),
        "{lines:?}"
    );
    assert_eq!(surfaces(&f.ui).len(), 1, "nothing was asked");
    assert_eq!(only_meta(&f).provider, "mock");
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
    model: m
    choices: [m, "relay:vendor/y", "mock:*"]
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

/// A checked bundle another iota process holds open (its writer is alive) is skipped with a
/// notice, not removed and not counted (docs/design/bot-mode.md §2.3).
#[tokio::test]
async fn session_delete_tab_skips_a_bundle_open_elsewhere() {
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
    // Standing in for the other process: a live writer holds the bundle lock throughout the run.
    let mut busy = f.writer();
    busy.append_messages(&[Message::user("still running")])
        .expect("materialise");
    let busy_id = busy.id().to_owned();
    let busy_dir = busy.dir().to_path_buf();
    let mut current = f.writer();
    current
        .append_messages(&[Message::user("hi")])
        .expect("materialise");
    let session = f.session(Some(current));
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");

    let lines = printed(&f.ui);
    let skipped = format!(
        "Skipped: session {busy_id} is open in another iota process (pid {})",
        std::process::id()
    );
    assert!(lines.contains(&skipped), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("Deleted ")),
        "{lines:?}"
    );
    assert!(busy_dir.exists(), "a held bundle survives");
    drop(busy);
}

/// A frozen-mode defer dispatcher: one `loader` tool whose first call leaves a deferred definition pending,
/// handed out ONCE at the next round boundary — the shape that pushes `Message::system_tools` into history.
#[derive(Default)]
struct MountingDispatch {
    called: std::sync::Mutex<bool>,
    handed: std::sync::Mutex<bool>,
}

impl Dispatcher for MountingDispatch {
    fn tools(&self) -> Vec<iota::provider::model::ToolDef> {
        vec![iota::testing::tool_def("loader")]
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a iota::tool::context::RunCtx,
        _name: &'a str,
        _args: iota::provider::model::JsonObject,
    ) -> iota::BoxFuture<'a, iota::tool::ToolResult> {
        Box::pin(async move {
            *self.called.lock().unwrap() = true;
            Ok(iota::tool::ToolOutput::ok("loaded"))
        })
    }

    fn take_pending_loads(&self) -> Vec<iota::provider::model::ToolDef> {
        let mut handed = self.handed.lock().unwrap();
        if *handed || !*self.called.lock().unwrap() {
            return Vec::new();
        }
        *handed = true;
        vec![iota::testing::tool_def("late_tool")]
    }
}

/// tool-defer.md: a frozen-mode mount is runtime state. The first turn mounts one mid-turn (the model's
/// next round SEES it), `persist_turn` hands it to the writer, and the writer skips it: the log carries no
/// system record at all (this chat has no system prompt). The watermark still moves past the mount — the
/// second turn appends only its own two messages, nothing is written twice — and a resume replays the six
/// conversation messages.
#[tokio::test]
async fn a_frozen_tools_mount_is_skipped_on_write_and_the_watermark_passes_it() {
    let f = Fixture::new(vec![
        input("load it"),
        Reply::Queued(Vec::new()), // the round boundary's steer drain: nothing typed meanwhile
        input("again"),
        Reply::Interrupted,
    ]);
    let saw_mount = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&saw_mount);
    let p = FakeProvider::new()
        .with_tools()
        .on_call(move |_, history| {
            if history.iter().any(|m| !m.tools().is_empty()) {
                seen.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
        .rounds([
            iota::testing::Round::calls(vec![iota::testing::tool_call("c1", "loader")]),
            iota::testing::Round::text("first"),
            iota::testing::Round::text("second"),
        ]);
    let writer = f.writer();
    let id = writer.id().to_owned();
    let dir = writer.dir().to_path_buf();
    let mut params = f.params(p, f.session(Some(writer)));
    params.dispatch = Arc::new(MountingDispatch::default());
    iota::repl::run(params).await.expect("exit");
    assert!(
        saw_mount.load(std::sync::atomic::Ordering::Relaxed),
        "the mount never reached the history — the test would prove nothing"
    );

    let recs: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("messages.jsonl"))
        .expect("log")
        .lines()
        .map(|l| serde_json::from_str(l).expect("record"))
        .collect();
    let shape: Vec<(String, String)> = recs
        .iter()
        .map(|r| {
            (
                r["role"].as_str().unwrap_or_default().to_owned(),
                r["content"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let row = |role: &str, content: &str| (role.to_owned(), content.to_owned());
    assert_eq!(
        shape,
        vec![
            row("user", "load it"),
            row("assistant", ""),
            row("tool", "loaded"),
            row("assistant", "first"),
            row("user", "again"),
            row("assistant", "second"),
        ]
    );
    let (_w, resumed) = f.store.resume(&id, ProviderKind::OpenAi).expect("resume");
    assert_eq!(resumed.messages.len(), 6);
    assert_eq!(resumed.meta.message_count, 6);
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
            store.create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
        })),
        scope: None,
        bot: false,
        notices: Vec::new(),
        recorded_notices: Vec::new(),
        memory: None,
    };
    // A provider that counts tokens: one that does not has no window to record, minted early or late.
    let mut params = f.params(provider("gpt-4o", Ok(vec![])).reporting_usage(), session);
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
        view.line_count, 9,
        "Provider, Model, Messages, Tools, MCP, Version, Mode, Session, Title — no token rows \
         in T1 (T-10)"
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
        .create(NewSession {
            cwd: "/work/p1".to_owned(),
            project: true,
            ..NewSession::new(ProviderKind::OpenAi, "m")
        })
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
/// synchronously and the model pass (on the SECOND provider instance) upgrades it while the
/// turn runs — the turn takes 50 ms here, the pass none. The exit never waits on the pass (an
/// unfinished one is given up, X-64), so it is the turn's length that lets both sinks settle.
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
    let mut params = f.params(
        provider("gpt-4o", Ok(vec![])).answering_after(std::time::Duration::from_millis(50)),
        session,
    );
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
    params.agent = iota::headless::AgentOptions {
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
    // The banner says the chat runs in agent mode (no bundle, no factory: the mode alone), in
    // the project root — whose tail survives whatever the row's width cut (the card gives the
    // directory 76 columns at 80, and a temp dir on macOS can be longer).
    let lines = printed(&f.ui);
    assert_eq!(card_row(&lines[2]), "agent", "{lines:?}");
    let leaf = root.file_name().expect("leaf").to_string_lossy();
    assert!(
        card_row(&lines[3]).ends_with(&format!("{}{leaf}", std::path::MAIN_SEPARATOR)),
        "{lines:?}"
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
        .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
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
        harness: iota::agents::harness::HarnessInputs::default(),
        imported_history: Vec::new(),
        dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
        jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
        mcp: no_mcp(),
        session: SessionCtx {
            writer: Some(writer),
            store,
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

// ---------------------------------------------------------------------------
// bot mode (docs/design/bot-mode.md §2.2, §2.7)
// ---------------------------------------------------------------------------

/// A store that knows a bots root, beside the fixture's sessions root.
fn bot_store(f: &Fixture) -> (SessionStore, PathBuf) {
    let bots = f
        .store
        .root()
        .parent()
        .expect("sessions root has a parent")
        .join("bots");
    (f.store.clone().with_bots(&bots), bots)
}

/// Opens bot `coder`'s session through the store (which names a new one after the bot).
fn bot_writer(store: &SessionStore, bots: &Path) -> SessionWriter {
    match store
        .open_bot(
            &bots.join("coder"),
            NewSession::new(ProviderKind::OpenAi, "gpt-test"),
            ProviderKind::OpenAi,
        )
        .expect("open the bot")
    {
        iota::session::BotOpen::Fresh(writer) | iota::session::BotOpen::Resumed(writer, _) => {
            writer
        }
    }
}

/// A bot's process serves its own session only: `/session` is not in the completion row and typing it is
/// a plain message (no picker opens). Its bundle keeps the bot's name — no placeholder, no title pass —
/// the wiring's notices open the transcript, and the pointer names the bundle the message went to.
#[tokio::test]
async fn a_bot_run_has_no_session_command_and_keeps_its_name() {
    let f = Fixture::new(vec![input("/session"), Reply::Interrupted]);
    let (store, bots) = bot_store(&f);
    let writer = bot_writer(&store, &bots);
    let dir = writer.dir().to_path_buf();
    let session = SessionCtx {
        writer: Some(writer),
        store,
        new_session: None,
        scope: None,
        bot: true,
        notices: vec!["system prompt updated from config".to_owned()],
        recorded_notices: Vec::new(),
        memory: None,
    };
    let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
    params.title_provider = Some(Box::new(
        provider("gpt-4o", Ok(vec![])).replying("A Model Title"),
    ));
    iota::repl::run(params).await.expect("exit");

    assert!(
        !completion_row(&f.ui).contains(&"/session".to_owned()),
        "{:?}",
        completion_row(&f.ui)
    );
    assert!(surfaces(&f.ui).is_empty(), "no picker opened");
    assert!(
        printed(&f.ui).contains(&"system prompt updated from config".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    let meta = iota::session::SessionMeta::read(&dir).expect("meta");
    assert!(meta.message_count > 0, "the message was persisted");
    assert_eq!(meta.title, "coder", "never renamed after the first message");
    let ptr = iota::session::BotPointer::read(&bots.join("coder"))
        .expect("pointer")
        .expect("present");
    assert_eq!(ptr.session, meta.id);
}

/// §2.2: a bot's RESUMED bundle is stamped with the parameters the chat starts under (the config's, for a
/// bot), where a normal resume leaves the meta as the session recorded it.
#[tokio::test]
async fn a_resumed_bot_is_stamped_with_the_running_parameters() {
    for bot in [false, true] {
        let f = Fixture::new(vec![Reply::Interrupted]);
        let mut writer = f.writer();
        writer
            .update_meta(|m| "high".clone_into(&mut m.effort))
            .expect("meta");
        writer
            .append_messages(&[Message::user("materialise")])
            .expect("materialise");
        let id = writer.id().to_owned();
        let dir = writer.dir().to_path_buf();
        drop(writer);
        let (writer, _) = f.store.resume(&id, ProviderKind::OpenAi).expect("resume");
        let mut session = f.session(Some(writer));
        session.bot = bot;
        let mut params = f.params(provider("gpt-4o", Ok(vec![])), session);
        params.params = iota::session::LayeredParams {
            context_window: iota::session::Param::config(400_000),
            ..iota::session::LayeredParams::default()
        };
        iota::repl::run(params).await.expect("exit");
        let meta = iota::session::SessionMeta::read(&dir).expect("meta");
        if bot {
            // This fake has no tuning capability, so the running effort is none at all.
            assert_eq!(
                meta.effort, "",
                "the running value replaced the recorded one"
            );
            assert!(meta.records_params());
        } else {
            assert_eq!(meta.effort, "high", "a normal resume is not re-stamped");
        }
    }
}

/// §2.7 I1: a session a bot's pointer names is in neither tab of the normal `/session` picker.
#[tokio::test]
async fn session_picker_hides_bot_sessions() {
    let f = Fixture::new(vec![
        input("/session"),
        Reply::Tabbed(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        }),
        Reply::Interrupted,
    ]);
    let (store, bots) = bot_store(&f);
    let mut body = bot_writer(&store, &bots);
    body.append_messages(&[Message::user("the bot's")])
        .expect("materialise");
    drop(body);
    let mut other = f.writer();
    other
        .append_messages(&[Message::user("someone else's")])
        .expect("materialise");
    other
        .update_meta(|m| "other chat".clone_into(&mut m.title))
        .expect("title");
    drop(other);
    let mut current = f.writer();
    current
        .append_messages(&[Message::user("hi")])
        .expect("materialise");
    let session = SessionCtx {
        store,
        ..f.session(Some(current))
    };
    iota::repl::run(f.params(provider("gpt-4o", Ok(vec![])), session))
        .await
        .expect("exit");

    let panels = &surfaces(&f.ui)[0].panels;
    assert_eq!(panels[0].items.len(), 2, "{:?}", panels[0].items);
    assert!(
        panels[0].items.iter().all(|r| !r.starts_with("coder · ")),
        "{:?}",
        panels[0].items
    );
    assert_eq!(panels[1].items.len(), 1, "{:?}", panels[1].items);
    assert!(panels[1].items[0].starts_with("other chat · "));
}

/// bot-mode.md §3.7 item 1: a turn that wrote to the bot's memory is followed by ONE notice record — in the
/// history and in the log, `notice: true` — and the next turn does not record it again.
#[tokio::test]
async fn a_memory_write_is_recorded_once_after_its_turn() {
    let f = Fixture::new(vec![
        input("remember that I like tabs"),
        Reply::Queued(Vec::new()), // the round boundary's steer drain: nothing typed meanwhile
        input("again"),
        Reply::Interrupted,
    ]);
    let (store, bots) = bot_store(&f);
    let writer = bot_writer(&store, &bots);
    let dir = writer.dir().to_path_buf();
    let memory = iota::agents::memory::BotMemory::new("coder", bots.join("coder"));
    let env = iota::tool::ToolEnv {
        memory: Some(memory.clone()),
        ..iota::tool::ToolEnv::default()
    };
    let mut registry = iota::tool::Registry::default();
    registry.enable_set(&env, iota::tool::sets::MEMORY_SET, &mut |w| {
        panic!("unexpected warning {w}")
    });
    let p = FakeProvider::new().with_tools().rounds([
        iota::testing::Round::calls(vec![iota::testing::tool_call_with(
            "c1",
            "remember",
            &[
                ("action", "add"),
                ("text", "prefers tabs"),
                ("source", "user"),
            ],
        )]),
        iota::testing::Round::text("noted"),
        iota::testing::Round::text("second"),
    ]);
    let session = SessionCtx {
        writer: Some(writer),
        store,
        new_session: None,
        scope: None,
        bot: true,
        notices: Vec::new(),
        recorded_notices: Vec::new(),
        memory: Some(memory.clone()),
    };
    let mut params = f.params(p, session);
    params.dispatch = Arc::new(registry);
    iota::repl::run(params).await.expect("exit");

    let today = iota::agents::harness::today();
    let notice = format!("memory: MEMORY.md ## User +1 line: [user] prefers tabs ({today})");
    let recs: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("messages.jsonl"))
        .expect("log")
        .lines()
        .map(|l| serde_json::from_str(l).expect("record"))
        .collect();
    let shape: Vec<(String, String, bool)> = recs
        .iter()
        .filter(|r| r["role"] != "system")
        .map(|r| {
            (
                r["role"].as_str().unwrap_or_default().to_owned(),
                r["content"].as_str().unwrap_or_default().to_owned(),
                r["notice"].as_bool().unwrap_or(false),
            )
        })
        .filter(|(role, _, _)| role != "tool")
        .collect();
    let row =
        |role: &str, content: &str, notice: bool| (role.to_owned(), content.to_owned(), notice);
    assert_eq!(
        shape,
        vec![
            row("user", "remember that I like tabs", false),
            row("assistant", "", false),
            row("assistant", "noted", false),
            row("user", &notice, true),
            row("user", "again", false),
            row("assistant", "second", false),
        ]
    );
    // The transcript shows it as the dim line a resume replays it as.
    assert_eq!(
        printed(&f.ui)
            .iter()
            .filter(|l| l.contains(&notice))
            .count(),
        1,
        "{:?}",
        printed(&f.ui)
    );
    let file = std::fs::read_to_string(memory.path()).expect("MEMORY.md");
    assert!(
        file.contains(&format!("- [user] prefers tabs ({today})")),
        "{file}"
    );
    assert!(memory.writes().take().is_empty());
}

/// A bot `coder` running in project `proj` (an AGENTS.md of its own) with `memory` as its MEMORY.md: the
/// store, the session, the memory, the project root, and a dispatcher carrying `remember`.
fn bot_in_project(
    f: &Fixture,
    memory: &str,
) -> (
    SessionCtx,
    iota::agents::memory::BotMemory,
    PathBuf,
    Arc<dyn Dispatcher>,
) {
    let (store, bots) = bot_store(f);
    let writer = bot_writer(&store, &bots);
    let bot = iota::agents::memory::BotMemory::new("coder", bots.join("coder"));
    std::fs::write(bot.path(), memory).expect("MEMORY.md");
    let root = bots.parent().expect("tmp").join("proj");
    std::fs::create_dir_all(&root).expect("project root");
    std::fs::write(root.join("AGENTS.md"), "PROJECT RULES").expect("AGENTS.md");
    let env = iota::tool::ToolEnv {
        memory: Some(bot.clone()),
        ..iota::tool::ToolEnv::default()
    };
    let mut registry = iota::tool::Registry::default();
    registry.enable_set(&env, iota::tool::sets::MEMORY_SET, &mut |w| {
        panic!("unexpected warning {w}")
    });
    let session = SessionCtx {
        writer: Some(writer),
        store,
        new_session: None,
        scope: None,
        bot: true,
        notices: Vec::new(),
        recorded_notices: Vec::new(),
        memory: Some(bot.clone()),
    };
    (session, bot, root, Arc::new(registry))
}

/// The block a fresh read of `memory`'s file gives in project `proj`.
fn memory_block(memory: &iota::agents::memory::BotMemory) -> String {
    iota::agents::memory::Snapshot::load(iota::agents::memory::BotMemory::new(
        "coder",
        memory.path().parent().expect("bot dir").to_path_buf(),
    ))
    .block(Some("proj"))
}

/// bot-mode.md §3.4: a bot's `<memory>` block is the last part of the overlay, after the AGENTS.md chain.
/// It is frozen: the `remember` tool's own write does not change it, an edit from outside does — picked up
/// before the next send, with ONE dim `MEMORY.md reloaded` — and nothing reloads again after that.
#[tokio::test]
async fn a_bots_memory_closes_the_overlay_and_reloads_on_an_outside_edit_only() {
    let f = Fixture::new(vec![
        input("one"),
        Reply::Queued(Vec::new()), // the round boundary's steer drain: nothing typed meanwhile
        input("two"),
        input("three"),
        Reply::Interrupted,
    ]);
    let (session, memory, root, dispatch) =
        bot_in_project(&f, "---\nbot: coder\n---\n## User\n- 不要用 rebase\n");
    let before = memory_block(&memory);
    let path = memory.path();
    let p = FakeProvider::new()
        .with_tools()
        .rounds([
            iota::testing::Round::calls(vec![iota::testing::tool_call_with(
                "c1",
                "remember",
                &[
                    ("action", "add"),
                    ("text", "prefers tabs"),
                    ("source", "user"),
                ],
            )]),
            iota::testing::Round::text("noted"),
            iota::testing::Round::text("second"),
            iota::testing::Round::text("third"),
        ])
        // Inside turn one, after the tool's write: the user edits the file by hand.
        .on_call(move |n, _| {
            if n == 2 {
                let text = std::fs::read_to_string(&path)
                    .expect("MEMORY.md")
                    .replace("不要用 rebase", "不要用 rebase，也不要 force push");
                std::fs::write(&path, text).expect("edit");
                let t = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .expect("open")
                    .set_modified(t)
                    .expect("stamp");
            }
        });
    let log = p.log();
    let mut params = f.params(p, session);
    params.dispatch = dispatch;
    params.agent = iota::headless::AgentOptions {
        enabled: true,
        root: root.clone(),
        cwd: Some(root),
        home: None,
    };
    iota::repl::run(params).await.expect("exit");

    let after = memory_block(&memory);
    assert!(
        after.contains("- 不要用 rebase，也不要 force push\n"),
        "{after}"
    );
    assert!(after.contains("prefers tabs"), "{after}");
    let systems: Vec<String> = log.sent().iter().map(|h| h[0].content.clone()).collect();
    assert_eq!(systems.len(), 4, "{systems:?}");
    for (i, system) in systems.iter().enumerate() {
        let block = if i < 2 { &before } else { &after };
        assert!(system.starts_with("PROJECT RULES"), "{system}");
        assert!(
            system.ends_with(&format!("\n\n{block}")),
            "call {i}: {system}"
        );
    }
    assert_eq!(
        printed(&f.ui)
            .iter()
            .filter(|l| l.starts_with("MEMORY.md reloaded"))
            .count(),
        1,
        "{:?}",
        printed(&f.ui)
    );
}

/// bot-mode.md §3.5's last row, end to end: a hand-edited file over the cap is announced once at startup,
/// the model is sent it cut short with the marker, and the file stays as it was.
#[tokio::test]
async fn a_bots_memory_over_the_cap_is_announced_and_cut_short() {
    let f = Fixture::new(vec![input("hi"), Reply::Interrupted]);
    let text: String = std::iter::once("## User\n".to_owned())
        .chain((0..100).map(|i| format!("- line {i:03} {}\n", "x".repeat(80))))
        .collect();
    let over = text.len() - iota::agents::memory::MEMORY_CAP;
    let (session, memory, root, dispatch) = bot_in_project(&f, &text);
    let p = provider("gpt-4o", Ok(vec![]));
    let log = p.log();
    let mut params = f.params(p, session);
    params.dispatch = dispatch;
    params.agent = iota::headless::AgentOptions {
        enabled: true,
        root: root.clone(),
        cwd: Some(root),
        home: None,
    };
    iota::repl::run(params).await.expect("exit");

    let warning = format!(
        "⚠ MEMORY.md is {over} bytes over its 8 KiB cap: the model is shown it cut short — consolidate it (the file was not changed)"
    );
    assert_eq!(
        printed(&f.ui).iter().filter(|l| **l == warning).count(),
        1,
        "{:?}",
        printed(&f.ui)
    );
    let system = log.sent()[0][0].content.clone();
    assert!(
        system.ends_with(&format!(
            "\n[memory truncated: {over} bytes over the cap — consolidate]\n</memory>"
        )),
        "{system}"
    );
    assert!(!system.contains("- line 099"), "{system}");
    assert_eq!(std::fs::read_to_string(memory.path()).expect("file"), text);
}
