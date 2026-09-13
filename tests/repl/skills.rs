//! `/skills` end to end over `ScriptedUi` (WP68; `T3_TEST_PLAN` §6): the catalog view, the named expansion, the error path and the table re-derivation on a catalog change.
//!
//! The helpers (`skills_status_lines`, `expand_skill`, `byte_size`) are unit-tested beside their
//! source; what only the run loop can show is the WIRING — that the arm is gated on agent mode,
//! that a viewer neither reaches the provider nor waits on the title pass, and above all the
//! EXPANSION split: the echo shows the line the user typed while the provider receives the
//! `<skill …>` block (chat/run.go:923-937,959,992).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use iota::BoxFuture;
use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::error::ProviderError;
use iota::provider::model::Message;
use iota::provider::{ChatResult, Provider, ProviderKind};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::SessionStore;
use iota::testing::{Reply, ScriptedUi, StaticDispatcher, TabbedSummary, UiEvent};
use iota::text::ansi::strip_sgr;
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, PanelKind, Ui};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// Runs inside the Nth `chat` call — the only place a test can change the world mid-run.
type ChatHook = Box<dyn Fn(usize) + Send + Sync>;

/// A provider that records the composed history of every turn, so the test can read what the
/// message path actually sent.
struct RecordingProvider {
    model: String,
    /// The `content` of every message of every `chat` call, in order.
    sent: Arc<Mutex<Vec<Vec<String>>>>,
    on_chat: Option<ChatHook>,
}

impl RecordingProvider {
    fn new() -> Self {
        Self {
            model: "gpt-4o".to_owned(),
            sent: Arc::new(Mutex::new(Vec::new())),
            on_chat: None,
        }
    }

    fn on_chat(mut self, f: impl Fn(usize) + Send + Sync + 'static) -> Self {
        self.on_chat = Some(Box::new(f));
        self
    }
}

impl Provider for RecordingProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
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
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        let n = {
            let mut s = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
            s.push(messages.iter().map(|m| m.content.clone()).collect());
            s.len()
        };
        if let Some(f) = &self.on_chat {
            f(n);
        }
        Box::pin(std::future::ready(Ok(ChatResult {
            text: "an answer".to_owned(),
            ..ChatResult::default()
        })))
    }
}

/// The scenario: a scripted facade, a temp store, and a project root carrying `.agents/skills`.
struct Fixture {
    ui: Arc<ScriptedUi>,
    _tmp: tempfile::TempDir,
    store: SessionStore,
    root: PathBuf,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).expect("project root");
        let store = SessionStore::new(tmp.path().join("sessions"));
        Self {
            ui: ScriptedUi::new(script),
            _tmp: tmp,
            store,
            root,
        }
    }

    /// `<root>/.agents/skills/<name>/SKILL.md` — the "project" discovery root.
    fn write_skill(&self, name: &str, description: &str, body: &str) -> PathBuf {
        write_skill_in(
            &self.root.join(".agents").join("skills"),
            name,
            description,
            body,
        )
    }

    fn params(&self, provider: RecordingProvider) -> RunParams {
        RunParams {
            ui: Arc::clone(&self.ui) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            imported_history: Vec::new(),
            dispatch: Arc::new(StaticDispatcher::new(&[])) as Arc<dyn Dispatcher>,
            jobs: iota::shell::jobs::Jobs::new(std::path::Path::new("")),
            mcp: McpHooks {
                servers: None,
                events: None,
            },
            session: SessionCtx {
                writer: None,
                store: self.store.clone(),
                new_session: None,
                scope: None,
            },
            context_window: 0,
            // Agent mode ON with NO home: the user-level roots are never scanned, so the
            // fixture's catalog is exactly what was written under the project root.
            agent: iota::chat::AgentOptions {
                enabled: true,
                root: self.root.clone(),
                cwd: Some(self.root.clone()),
                home: None,
            },
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(Vec::new(), true)),
        }
    }
}

/// A valid `SKILL.md` under `<dir>/<name>/`.
fn write_skill_in(dir: &Path, name: &str, description: &str, body: &str) -> PathBuf {
    let sd = dir.join(name);
    std::fs::create_dir_all(&sd).expect("skill dir");
    let path = sd.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("write SKILL.md");
    path
}

fn input(s: &str) -> Reply {
    Reply::Input(Input {
        display: s.to_owned(),
        text: s.to_owned(),
        ..Input::default()
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

fn surfaces(ui: &ScriptedUi) -> Vec<TabbedSummary> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Every `user_block` echo, in order.
fn echoes(ui: &ScriptedUi) -> Vec<String> {
    ui.events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::UserBlock(s) => Some(s),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// bare /skills — the catalog viewer
// ---------------------------------------------------------------------------

/// Go: chat/run.go:923-929 — bare `/skills` opens the read-only `Skills` view over the discovered
/// catalog and sends nothing. The rows themselves are pinned beside `skills_status_lines`.
#[tokio::test]
async fn skills_bare_opens_the_skills_view() {
    let f = Fixture::new(vec![
        input("/skills"),
        Reply::Interrupted, // the viewer is dismissed
        Reply::Interrupted, // and the loop exits
    ]);
    f.write_skill("brain-page", "Read and write brain pages", "Do the thing.");
    let provider = RecordingProvider::new();
    let sent = Arc::clone(&provider.sent);
    iota::repl::run(f.params(provider))
        .await
        .expect("clean exit");

    let views = surfaces(&f.ui);
    assert_eq!(views.len(), 1, "exactly one surface: {views:?}");
    let panel = &views[0].panels[0];
    assert_eq!(panel.title, "Skills");
    assert_eq!(panel.kind, PanelKind::View);
    // One skill = the name/tag row plus its description row.
    assert_eq!(panel.line_count, 2);
    assert!(!panel.has_refresh, "the catalog view is not live");
    // A viewer never reaches the provider.
    assert!(sent.lock().unwrap().is_empty());
}

/// New: with nothing discovered the view says where it LOOKED — the overlay's own roots, which is
/// why `Overlay::skill_dirs()` exists (agentmode.go:29-32). With no home injected that is the one
/// project root.
#[tokio::test]
async fn skills_bare_with_an_empty_catalog_names_the_roots() {
    let f = Fixture::new(vec![
        input("/skills"),
        Reply::Interrupted,
        Reply::Interrupted,
    ]);
    iota::repl::run(f.params(RecordingProvider::new()))
        .await
        .expect("clean exit");
    let panel = surfaces(&f.ui).remove(0).panels.remove(0);
    assert_eq!(panel.title, "Skills");
    // "No skills discovered. Searched:" + the single project root.
    assert_eq!(panel.line_count, 2);
}

// ---------------------------------------------------------------------------
// /skills <name> — the input expansion
// ---------------------------------------------------------------------------

/// Go: chat/run.go:930-936,959,992 — a named skill is an input EXPANSION: the notice names it and
/// its size, the transcript echoes what the user TYPED, and the provider receives the `<skill …>`
/// block with the trailing instructions after it.
#[tokio::test]
async fn skills_named_expands_into_the_message_path() {
    let f = Fixture::new(vec![input("/skills brain-page do it"), Reply::Interrupted]);
    let path = f.write_skill("brain-page", "Read and write brain pages", "Read the page.");
    let provider = RecordingProvider::new();
    let sent = Arc::clone(&provider.sent);
    iota::repl::run(f.params(provider))
        .await
        .expect("clean exit");

    // The expansion's byte length is what the notice reports. The location is Go-quoted, so a
    // Windows path arrives with its separators escaped — `display()`'s raw spelling is neither
    // what the block carries nor the length the notice counts.
    let expected = format!(
        "<skill name=\"brain-page\" location={:?}>\nReferences are relative to {}.\n\nRead the page.\n</skill>\n\ndo it",
        path.to_string_lossy(),
        path.parent().unwrap().display()
    );
    assert!(
        printed(&f.ui).contains(&format!("Skill brain-page loaded ({} B).", expected.len())),
        "missing the loaded notice: {:?}",
        printed(&f.ui)
    );
    // The echo shows the typed line — a whole SKILL.md in the ❯ block would bury the screen.
    assert_eq!(echoes(&f.ui), ["/skills brain-page do it"]);
    // The provider got the expansion, not the command.
    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "exactly one turn");
    assert_eq!(sent[0].last().expect("a user message"), &expected);
    // No surface: the named form never opens the viewer.
    assert!(surfaces(&f.ui).is_empty());
}

/// Go: chat/run.go:931-934 — a name the catalog does not have prints the error verbatim (no
/// `Error: ` prefix) and sends NOTHING: a typo must not spend a turn.
#[tokio::test]
async fn skills_unknown_name_errors_and_sends_nothing() {
    let f = Fixture::new(vec![input("/skills nope"), Reply::Interrupted]);
    f.write_skill("brain-page", "Read and write brain pages", "Read the page.");
    let provider = RecordingProvider::new();
    let sent = Arc::clone(&provider.sent);
    iota::repl::run(f.params(provider))
        .await
        .expect("clean exit");

    assert!(
        printed(&f.ui)
            .contains(&"no skill named \"nope\" — /skills lists what is available".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
    assert!(
        sent.lock().unwrap().is_empty(),
        "an error must send nothing"
    );
    assert!(echoes(&f.ui).is_empty(), "an error is not a message");
}

/// New: outside agent mode `/skills` is not registered, so the line is an ordinary message — the
/// gate is the SAME flag the completion row hangs off, so the table and the chain cannot drift.
#[tokio::test]
async fn skills_is_a_plain_message_without_agent_mode() {
    let f = Fixture::new(vec![input("/skills"), Reply::Interrupted]);
    let provider = RecordingProvider::new();
    let sent = Arc::clone(&provider.sent);
    let mut params = f.params(provider);
    params.agent = iota::chat::AgentOptions::default();
    iota::repl::run(params).await.expect("clean exit");

    assert!(surfaces(&f.ui).is_empty(), "no viewer without agent mode");
    assert_eq!(
        sent.lock().unwrap().last().expect("a turn ran")[0],
        "/skills",
        "the line travelled as a plain message"
    );
    let commands: Vec<String> =
        f.ui.events()
            .into_iter()
            .find_map(|e| match e {
                UiEvent::Commands(c) => Some(c.into_iter().map(|s| s.value).collect()),
                _ => None,
            })
            .expect("the loop publishes the command table");
    assert!(
        !commands.iter().any(|v| v.starts_with("/skill")),
        "skill commands leaked outside agent mode: {commands:?}"
    );
}

// ---------------------------------------------------------------------------
// the table follows the catalog
// ---------------------------------------------------------------------------

/// Go: chat/run.go:131-134,969-971 — the per-skill rows are installed at startup and RE-DERIVED
/// whenever the catalog changes on disk, so the completion list can never advertise a skill that
/// is gone (or hide one that just landed).
#[tokio::test]
async fn a_catalog_change_re_issues_the_table_with_the_new_row() {
    let f = Fixture::new(vec![input("one"), input("two"), Reply::Interrupted]);
    f.write_skill("brain-page", "Read and write brain pages", "Read the page.");
    let skills_dir = f.root.join(".agents").join("skills");
    let provider = RecordingProvider::new().on_chat(move |n| {
        if n == 1 {
            write_skill_in(&skills_dir, "code-review", "Review the diff", "Review it.");
        }
    });
    iota::repl::run(f.params(provider))
        .await
        .expect("clean exit");

    let tables: Vec<Vec<String>> =
        f.ui.events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Commands(c) => Some(c.into_iter().map(|s| s.value).collect()),
                _ => None,
            })
            .collect();
    assert_eq!(tables.len(), 2, "startup, then the catalog change");
    // Startup: /skills plus the one discovered row, last in the table.
    assert_eq!(
        tables[0].iter().rev().take(2).rev().collect::<Vec<_>>(),
        ["/skills", "/skills brain-page"]
    );
    // After the change: both rows, in catalog order (directory name minor).
    assert_eq!(
        tables[1].iter().rev().take(3).rev().collect::<Vec<_>>(),
        ["/skills", "/skills brain-page", "/skills code-review"]
    );
    // …and the loop said so once.
    assert!(
        printed(&f.ui).contains(&"Skills reloaded (2 skill(s))".to_owned()),
        "{:?}",
        printed(&f.ui)
    );
}

/// New: the per-skill rows carry the skill's description and a bare label, and the banner (which
/// reads `names()`) never lists them — they are completions, not commands.
#[tokio::test]
async fn per_skill_rows_are_labelled_bare_and_stay_out_of_the_banner() {
    let f = Fixture::new(vec![Reply::Interrupted]);
    f.write_skill("brain-page", "Read and write brain pages", "Read the page.");
    iota::repl::run(f.params(RecordingProvider::new()))
        .await
        .expect("clean exit");

    let row =
        f.ui.events()
            .into_iter()
            .find_map(|e| match e {
                UiEvent::Commands(c) => Some(c),
                _ => None,
            })
            .expect("the loop publishes the command table")
            .into_iter()
            .find(|s| s.value == "/skills brain-page")
            .expect("the per-skill row");
    assert_eq!(row.label, "brain-page");
    assert_eq!(row.desc, "Read and write brain pages");

    let banner = printed(&f.ui);
    let commands = banner
        .iter()
        .find(|l| l.starts_with("Commands: "))
        .expect("the banner's command row");
    assert!(
        commands.contains("/skills") && !commands.contains("/skills brain-page"),
        "{commands}"
    );
}
