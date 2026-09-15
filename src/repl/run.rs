//! `RunParams` + the interactive REPL loop (chat.Run port; `TUI_CONTRACTS` §7,
//! `TUI_DESIGN` §8.3).
//!
//! The loop is deliberately IMPERATIVE and deliberately outside `crate::ui`: it talks to the
//! terminal only through the [`Ui`] facade, and everything below the facade (the frame
//! engine, the composer, the surfaces) knows nothing about chat. Its shape is Go's, in
//! Go's order:
//!
//! 1. the banner — what the chat is, what it can be told, where it is saved, what agent
//!    mode loaded — then the resume echo OR one blank line, so exactly one blank separates
//!    the environment from the first transcript block;
//! 2. the pre-loop interactions: the `-S` system prompt (only with no imported history)
//!    and the startup model pick (ESC defers — the first message re-prompts);
//! 3. `read_input` → the dispatch chain in Go's FIXED order → the message path.
//!
//! After the facade is up NOTHING may write to the terminal except through it — no
//! spinner, no raw OSC, no `println!`. The banner Go printed to plain stdout therefore
//! goes through `print_lines` here; the bytes and their order are the same.
//!
//! The host presenter (T3) is told what the loop is doing at Go's anchors — `Idle` while
//! waiting for input, `Busy` from `start_stream`, `NeedsInput` around approvals and
//! interactive tools, `Error` + a `Failed` ping on a failed turn, `Idle` + a `Done` ping on a
//! landed reply — and is closed on every exit path before the facade is.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::agents::Overlay;
use crate::agents::skills::Skill;
use crate::host::{Event, Kind, Presenter, State};
use crate::llm::reqlog::RequestLog;
use crate::markdown::CodeTheme;
use crate::provider::Provider;
use crate::provider::model::{AssistantBody, Attachment, Body, Message};
use crate::session::{SessionStore, SessionWriter};
use crate::tool::Dispatcher;
use crate::ui::facade::{InputKind, StatusData, Ui};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::repl::ReplError;
use crate::repl::approval::ApprovalGate;
use crate::repl::banner::banner_lines;
use crate::repl::commands::edit::EditOutcome;
use crate::repl::commands::skills::SkillsOutcome;
use crate::repl::commands::{
    CmdFlags, CommandTable, SkillEntry, debug, edit, export, file, match_cmd, model, save, session,
    skills, status, tools,
};
use crate::repl::interrupt::{InterruptDecision, finalize_interrupt};
use crate::repl::mcpreport::report_mcp_failures;
use crate::repl::meter::{ContextBudget, CtxMeter};
use crate::repl::replay::{RESUME_ECHO_ROUNDS, echo_rounds, last_rounds};
use crate::repl::retry::{MAX_RETRIES, RETRY_BACKOFF, is_retryable};
use crate::repl::steer::Steerer;
use crate::repl::title::{
    SessionTitle, TITLE_TIMEOUT, WriterSlot, generate_title_text, is_read_only_viewer,
    status_model_label, window_title,
};
use crate::repl::transcript::{Transcript, notify_digest};
use crate::repl::turn::{TurnCtx, TurnFailure, TurnReport, collect_images, run_turn};

/// The `Done` ping of an image-only reply (chat/run.go:1105).
const IMAGE_READY: &str = "Image ready";

/// A finished background job as ONE input: `display` is the headline the transcript prints, `text` is the
/// headline plus the job's output — what the model reads. The split is exactly `Input`'s own (the echo is
/// bounded, the send is not).
pub(crate) fn job_notice(done: &crate::shell::jobs::JobDone) -> crate::ui::facade::Input {
    crate::ui::facade::Input {
        display: crate::shell::jobs::notice_headline(done),
        text: crate::shell::jobs::notice_text(done),
        kind: crate::ui::facade::InputKind::Notice,
    }
}

/// One MCP server's terminal connect status (what the connect reporter drains).
pub struct McpEvent {
    /// Server name.
    pub name: String,
    /// The failure, when the connect failed.
    pub error: Option<String>,
    /// Non-fatal warnings a connected server merged with (`ServerStatus::warnings`: a skipped duplicate wire
    /// name), each relayed as one dim `⚠ MCP <name>: …` notice (DIVERGENCES X-29).
    pub warnings: Vec<String>,
}

/// MCP display hooks handed in by the binary.
pub struct McpHooks {
    /// The live server snapshot, re-read on every refresh tick. `None` = a build with no
    /// MCP at all, which renders the "No MCP servers configured." tab and every tool as a
    /// built-in.
    pub servers: Option<Arc<dyn Fn() -> Vec<crate::mcp::ServerStatus> + Send + Sync>>,
    /// Terminal connect statuses (the MCP reporter task drains it).
    pub events: Option<tokio::sync::mpsc::Receiver<McpEvent>>,
}

/// Mints the session writer on demand (/save). `Some` = the chat STARTED ephemeral.
pub type SessionFactory =
    Box<dyn FnMut() -> Result<crate::session::SessionWriter, crate::session::SessionError> + Send>;

/// Session wiring of one interactive run.
pub struct SessionCtx {
    /// The live writer (None while ephemeral).
    pub writer: Option<crate::session::SessionWriter>,
    /// The store (listing, resume, delete).
    pub store: crate::session::SessionStore,
    /// Mints the writer late (/save); `Some` = started ephemeral.
    pub new_session: Option<SessionFactory>,
    /// Agent-mode project bucket for /session (mode-isolated listing).
    pub scope: Option<PathBuf>,
}

/// Everything `run()` needs (`TUI_CONTRACTS` §7).
pub struct RunParams {
    /// The facade.
    pub ui: Arc<dyn crate::ui::facade::Ui>,
    /// The conversation provider.
    pub provider: Box<dyn crate::provider::Provider>,
    /// The SECOND provider instance for the async title pass (None for image providers).
    pub title_provider: Option<Box<dyn crate::provider::Provider>>,
    /// The system prompt.
    pub system: String,
    /// Resumed/imported history.
    pub imported_history: Vec<crate::provider::model::Message>,
    /// The tool dispatcher.
    pub dispatch: Arc<dyn crate::tool::Dispatcher>,
    /// The run's background-job registry: the loop installs the delivery sink on it and kills whatever is
    /// still running on the way out.
    pub jobs: Arc<crate::shell::jobs::Jobs>,
    /// MCP glue.
    pub mcp: McpHooks,
    /// Session wiring.
    pub session: SessionCtx,
    /// The four layered parameters the chat starts under, each with the source that put it there: the
    /// startup evaluation, or a resumed bundle's own record (brain page `model-param-layering`). The VALUES
    /// of the three tunables must already be on the provider — the loop reads them back from it — and the
    /// window is what the budget is built over (`0` → the `128_000` default).
    pub params: crate::session::LayeredParams,
    /// The config declarations a `/model` model switch re-evaluates those four against.
    pub layers: crate::config::ParamLayers,
    /// The agent's candidate set, as `/model` offers it (`repl::catalog`).
    pub catalog: crate::repl::ModelCatalog,
    /// Agent-mode options.
    pub agent: crate::headless::AgentOptions,
    /// Whether the terminal's background is dark, as the ONE pre-loop OSC-11 probe answered
    /// it. The detected background must reach the code theme and the diff shades, and the
    /// loop never probes the terminal itself.
    pub dark_background: bool,
    /// The SIGTERM path (root cancellation).
    pub root_cancel: CancellationToken,
    /// The `/debug` request log shared with the title provider (root.go:128,410).
    pub reqlog: Arc<RequestLog>,
    /// The host presenter (run.go:139 `host.NewPresenter(host.SystemEnv(), host.NewANSI(u),
    /// notify)`).
    pub pres: Arc<Presenter>,
}

/// The loop's mutable state — Go's `Run` stack, made addressable so the command handlers
/// can live in their own files instead of one 900-line function.
pub(crate) struct Repl {
    /// The facade.
    pub(crate) ui: Arc<dyn Ui>,
    /// The single writer to the chat area.
    pub(crate) tr: Arc<Transcript>,
    /// The conversation provider.
    pub(crate) provider: Box<dyn Provider>,
    /// The tool dispatcher (LIVE — never cached).
    pub(crate) dispatch: Arc<dyn Dispatcher>,
    /// The run's background jobs (their notices arrive through the facade's queue, not through here).
    pub(crate) jobs: Arc<crate::shell::jobs::Jobs>,
    /// MCP display hooks.
    pub(crate) mcp: McpHooks,
    /// The session writer, shared with the title pass (`None` while ephemeral).
    pub(crate) writer: WriterSlot,
    /// The store behind `/session`.
    pub(crate) store: SessionStore,
    /// The agent-mode project bucket the listing is scoped to.
    pub(crate) scope: Option<PathBuf>,
    /// Mints the writer late (`/save`); `Some` = the chat started ephemeral.
    pub(crate) new_session: Option<SessionFactory>,
    /// The conversation.
    pub(crate) history: Vec<Message>,
    /// How much of `history` is on disk — never advanced by a failed append, so the next
    /// successful persist retries the backlog.
    pub(crate) persisted: usize,
    /// Attachments riding the next user message.
    pub(crate) pending: Vec<Attachment>,
    /// The context window (and, from WP53, its occupancy).
    pub(crate) budget: ContextBudget,
    /// The token meter (inert for a provider without usage — T-10).
    pub(crate) ctxm: CtxMeter,
    /// The session's name across the turn lifecycle.
    pub(crate) titler: Arc<SessionTitle>,
    /// Root cancellation (the SIGTERM path).
    pub(crate) cancel: CancellationToken,
    /// The `/debug` request log (recording toggle + the inspector's rows).
    pub(crate) reqlog: Arc<RequestLog>,
    /// The host presenter.
    pub(crate) pres: Arc<Presenter>,
    /// The ONE command table (completion list, banner and dispatch read it).
    pub(crate) table: CommandTable,
    /// The conversation's ONE approval gate: the "allow for this session" grant is one grant
    /// for one person (chat/run.go:172-186).
    pub(crate) gate: Arc<ApprovalGate>,
    /// Where generated images are saved, resolved LAZILY: a bundle materialises on first
    /// use, and an image-less chat must not create one (chat/images.go:115).
    pub(crate) images_dir: crate::repl::turn::ImagesDir,
    /// A dedicated image provider bills per attempt (a relay 5xx can arrive AFTER a charged
    /// generation), so its turns are never auto-retried.
    pub(crate) image_provider: bool,
    /// Whether the terminal background is dark; a host that knows better re-answers it
    /// between turns.
    pub(crate) dark: bool,
    /// The agent-mode overlay woven into every send (`None` outside agent mode).
    pub(crate) overlay: Option<Overlay>,
    /// Agent-mode options: the project root and the skills home.
    pub(crate) agent: crate::headless::AgentOptions,
    /// The title pass's own provider instance (the conversation's is mid-call while a turn
    /// streams); `None` for a dedicated image provider, which asked for a title would paint one.
    pub(crate) title_provider: Option<Arc<tokio::sync::Mutex<Box<dyn Provider>>>>,
    /// The in-flight title pass.
    pub(crate) title_task: Option<JoinHandle<()>>,
    /// The auto-compaction snooze watermark: the projected usage at which the user last
    /// said "Not now" (0 = never asked). Cleared by any successful compaction.
    pub(crate) compact_declined: u64,
    /// Where each of the four layered parameters got the value the chat is running under. The values
    /// themselves are read from the budget and the provider (`crate::repl::params`); this is the only piece
    /// of the layering with nowhere else to live.
    pub(crate) param_sources: crate::session::ParamSources,
    /// What a `/model` model switch re-evaluates those four against.
    pub(crate) layers: crate::config::ParamLayers,
    /// What `/model` offers: the agent's candidate set and the listers its wildcards need.
    pub(crate) catalog: crate::repl::ModelCatalog,
}

impl Repl {
    /// Publishes the status row (chat/run.go:148-165 `pushStatus`). The provider type
    /// stands in for the model until one is chosen; `debug` mirrors the request log's
    /// recording state (the `/debug on` segment).
    ///
    /// A LIVE meter owns the row: it repaints the token half from inside a streaming turn,
    /// where this method cannot reach, so it is handed the model label and publishes both
    /// halves itself (reading the SAME log for the `debug` segment). A disabled meter owns
    /// nothing and says so, and the model goes out alone — the shape of every
    /// non-token-accounting provider.
    pub(crate) fn push_status(&self) {
        let model = status_model_label(self.provider.model(), self.provider.kind().as_str());
        if self.ctxm.publish_status(&model) {
            return;
        }
        self.ui.set_status(StatusData {
            model,
            debug: self.reqlog.verbose(),
            ..StatusData::default()
        });
    }

    /// Appends whatever the bundle has not seen (chat/run.go:221-229 `persistTurn`).
    ///
    /// A failure warns and does NOT advance the watermark, so the next successful persist
    /// carries the backlog — which is also how `/save` flushes a whole ephemeral chat in
    /// one append.
    pub(crate) fn persist_turn(&mut self) {
        let mut slot = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(w) = slot.as_mut() else { return };
        if self.persisted >= self.history.len() {
            return;
        }
        if let Err(e) = w.append_messages(&self.history[self.persisted..]) {
            drop(slot);
            self.tr
                .error(&format!("Warning: failed to save session: {e}"));
            return;
        }
        self.persisted = self.history.len();
    }

    /// The live session id, or `""` while the chat is ephemeral.
    pub(crate) fn session_id(&self) -> String {
        self.with_writer(|w| w.id().to_owned())
    }

    /// The live session title, or `""`.
    pub(crate) fn session_title(&self) -> String {
        self.with_writer(|w| w.meta().title.clone())
    }

    /// A path of the live writer (`images_path()`), or `None` while the chat is ephemeral.
    pub(crate) fn with_writer_path(
        &self,
        f: impl FnOnce(&SessionWriter) -> PathBuf,
    ) -> Option<PathBuf> {
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(f)
    }

    fn with_writer(&self, f: impl FnOnce(&SessionWriter) -> String) -> String {
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(String::new, f)
    }
}

/// The completion table's view of the discovered skills (completion.go:63-73).
fn skill_entries(skills: &[Skill]) -> Vec<SkillEntry> {
    skills
        .iter()
        .map(|s| SkillEntry {
            name: s.name.clone(),
            description: s.description.clone(),
        })
        .collect()
}

/// The code theme of a background tone.
fn code_theme_of(dark: bool) -> CodeTheme {
    if dark {
        CodeTheme::Monokai
    } else {
        CodeTheme::Github
    }
}

/// The interactive REPL (chat.Run port). After `Tui::start` NOTHING may write to the
/// terminal except through `ui`.
///
/// The binary owns everything around this call: the pre-loop background probe, the title
/// stack push/pop, `Tui::start`, binding the `Interactor` to the live facade,
/// `ui.close()` and the MCP manager's shutdown (`TUI_DESIGN` §8.4). A turn error never
/// exits — it prints and the loop continues; only a closed facade or a fatal store failure
/// ends the run. The host presenter is closed here on every exit path, before the facade
/// (Go's defer order: `pres.Close()` runs before `u.Close()`).
pub async fn run(params: RunParams) -> Result<(), ReplError> {
    let RunParams {
        ui,
        mut provider,
        title_provider,
        system,
        imported_history,
        dispatch,
        jobs,
        mcp,
        session,
        params,
        layers,
        catalog,
        agent,
        dark_background,
        root_cancel,
        reqlog,
        pres,
    } = params;
    let crate::repl::SessionCtx {
        writer,
        store,
        new_session,
        scope,
    } = session;

    // ---- capability probes (chat/run.go:53-55) ----
    // Token accounting (the meter, /compact, the auto-offer) exists only for a provider
    // that reports usage.
    let token_aware = provider.reports_usage();
    // A dedicated image provider bills per attempt (a relay 5xx can arrive AFTER a charged
    // generation), so its turns are never auto-retried — and it is what registers
    // `/edit`/`/redo` (the SAME bool, so the table and the chain cannot drift).
    let image_provider = provider.as_image_gen_tunable().is_some();

    // The code theme and the diff shades follow the terminal the binary probed BEFORE the
    // event loop claimed stdin (chat/theme.go `applyCodeTheme`); a host that knows better
    // re-answers between turns.
    let dark = dark_background;

    let overlay = agent.enabled.then(|| {
        let root = agent.root.as_path();
        let cwd = agent.cwd.as_deref().unwrap_or(root);
        Overlay::new(root, cwd, agent.home.as_deref())
    });

    // ---- history seeding (chat/run.go:67-79) ----
    let resumed = !imported_history.is_empty();
    let history = if resumed {
        imported_history
    } else if system.is_empty() {
        Vec::new()
    } else {
        vec![Message::system(system)]
    };
    let persisted = if resumed { history.len() } else { 0 };
    let mut budget = ContextBudget::new(params.context_window.value);
    // The live meter exists only for a provider whose usage it can settle against
    // (chat/run.go:161-164); everything else keeps Go's nil meter, whose methods are all
    // no-ops — which is why every `ctxm.…` call below is unconditional (T-10).
    let mut ctxm = if token_aware {
        budget.meter(
            Arc::clone(&ui),
            status_model_label(provider.model(), provider.kind().as_str()),
            Arc::clone(&reqlog),
        )
    } else {
        CtxMeter::disabled()
    };
    if !history.is_empty() {
        budget.update(&history);
    }
    let writer: WriterSlot = Arc::new(Mutex::new(writer));
    // A bundle this run CREATED must be told what the chat is running under (below, once the loop state is
    // assembled); one it RESUMED already says.
    let mut fresh_bundle = false;
    {
        let mut slot = writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = slot.as_mut() {
            // A resumed session's cumulative ↑/↓ figures are what its own log adds up to.
            ctxm.seed_totals(w.usage());
            fresh_bundle = !w.on_disk();
        }
    }

    // chat/run.go:140-146 `setActiveCommands(agent, save, compact, image)` + the skill rows.
    let mut table = CommandTable::new(CmdFlags {
        save: new_session.is_some(),
        compact: token_aware && ctxm.is_enabled(),
        agent: overlay.is_some(),
        image: image_provider,
    });
    if let Some(o) = overlay.as_ref() {
        table.set_skills(skill_entries(o.skills()));
    }
    // The thinking meter counts with the chat's ONE tokenizer (Go handed `newTranscript`
    // the budget's own counter).
    let estimator: Option<crate::repl::transcript::TokenEstimator> = {
        let counter = budget.counter();
        Some(Box::new(move |s: &str| counter.count(s)))
    };
    let tr = Arc::new(Transcript::new(Arc::clone(&ui), estimator));
    tr.set_dark(dark);
    // `/debug on` keeps activity groups expanded: the transcript reads the log live.
    {
        let log = Arc::clone(&reqlog);
        tr.set_verbose(Some(Box::new(move || log.verbose())));
    }

    // ---- the banner, then EITHER the resume echo OR one blank (chat/run.go:86-116) ----
    // Exactly one blank separates the environment from the first transcript block: an echo
    // ends with its own round separator, so it is not followed by another.
    ui.print_lines(banner_lines(
        &table.names(),
        &writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(String::new, |w| w.id().to_owned()),
        new_session.is_some(),
        overlay.as_ref(),
    ));
    ui.print_lines(vec![String::new()]);
    if resumed {
        let msgs = last_rounds(&history, RESUME_ECHO_ROUNDS);
        if !msgs.is_empty() {
            let d = Arc::clone(&dispatch);
            let img_dir = writer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(SessionWriter::images_path);
            let lines = echo_rounds(
                msgs,
                |n| d.presentation(n) == crate::tool::Presentation::Surface,
                usize::from(ui.width()),
                img_dir.as_deref(),
            );
            ui.print_lines(lines);
        }
    }

    let titler = Arc::new(SessionTitle::new(
        Arc::clone(&writer),
        {
            let ui = Arc::clone(&ui);
            Box::new(move |name: &str| ui.set_title(&window_title(name)))
        },
        resumed,
    ));
    ui.set_title(&window_title(
        &writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(String::new, |w| w.meta().title.clone()),
    ));
    ui.set_slash_commands(table.active());

    let gate = Arc::new(ApprovalGate::new(
        Arc::clone(&ui),
        Arc::clone(&tr),
        Arc::clone(&pres),
    ));
    let images_dir: crate::repl::turn::ImagesDir = {
        let slot = Arc::clone(&writer);
        Arc::new(move || {
            slot.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .and_then(SessionWriter::images_dir)
        })
    };
    let title_provider = title_provider.map(|p| Arc::new(tokio::sync::Mutex::new(p)));
    let mut repl = Repl {
        ui: Arc::clone(&ui),
        tr: Arc::clone(&tr),
        provider,
        dispatch: Arc::clone(&dispatch),
        jobs: Arc::clone(&jobs),
        mcp,
        writer: Arc::clone(&writer),
        store,
        scope,
        new_session,
        history,
        persisted,
        pending: Vec::new(),
        budget,
        ctxm,
        titler: Arc::clone(&titler),
        cancel: root_cancel.clone(),
        reqlog: Arc::clone(&reqlog),
        pres: Arc::clone(&pres),
        table,
        gate: Arc::clone(&gate),
        images_dir,
        image_provider,
        dark,
        overlay,
        agent,
        title_provider,
        title_task: None,
        compact_declined: 0,
        param_sources: params.sources(),
        layers,
        catalog,
    };
    repl.push_status();

    // What the chat is running under, and where each value came from, into a bundle this run created — so a
    // resume finds the session as it was rather than re-deriving it from a config that may have moved since
    // (brain page `model-param-layering`). The values are read back from the provider and the budget, so a
    // knob the dialect cannot act on is never recorded as though it had applied; the window is left out
    // entirely for a provider with no token accounting.
    if fresh_bundle {
        let running = crate::repl::params::current(&mut repl);
        let window = token_aware.then(|| repl.budget.window());
        crate::repl::params::stamp_bundle(&repl, window, &running);
    }

    // A finished job becomes the next input: the facade serves it to a parked `read_input` at once (an idle
    // loop wakes and answers it) or queues it behind what is already typed ahead, and `Steerer::drain` takes
    // it at the next round boundary when a turn is running. ONE delivery path, no second queue.
    {
        let ui_sink = Arc::clone(&ui);
        repl.jobs.set_sink(Some(Box::new(move |done| {
            ui_sink.enqueue(job_notice(&done));
        })));
    }

    if let Some(events) = repl.mcp.events.take() {
        tokio::spawn(report_mcp_failures(events, Arc::clone(&tr), ui.done()));
    }

    // ---- pre-loop interactions, INSIDE the facade (chat/run.go:349-371) ----
    // Go also offered to type a system prompt here, behind `-S`. That flag is gone: it described
    // configuration one keystroke at a time, and an `agents:` entry carries a prompt permanently (brain page
    // `cli-surface-agent-first`).
    if repl.provider.model().is_empty() {
        // v1 offered the pick at startup; ESC defers — the first message re-prompts.
        model::ensure_model(&mut repl, &root_cancel).await;
        repl.push_status();
    }

    // ---- the main loop ----
    // Every exit path breaks out with its outcome so the presenter is closed exactly once
    // below (Go's `defer pres.Close()`).
    let outcome: Result<(), ReplError> = 'main: loop {
        let Ok(input) = ui.read_input(&root_cancel).await else {
            // ErrInterrupted (idle Ctrl+C / Ctrl+D), ErrClosed, or shutdown: join the
            // title pass so a landed name is written before the writer is dropped.
            repl.join_title().await;
            break Ok(());
        };
        // A host notice (a finished background job) answers the same `read_input` a typed line does — that
        // is what wakes an idle loop — but it is not something the user said: it skips the echo, and the
        // dispatch chain below is inert for it anyway (its text can never be a slash command).
        let notice = input.kind == InputKind::Notice;
        let line = input.text.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        // Whatever the input is, the loop is awake for the user now (run.go:381).
        repl.pres.set_state(State::Idle);
        if !is_read_only_viewer(&line) {
            // A read-only viewer neither calls the provider nor mutates the writer, so it
            // need not wait; anything else must not race a writer swap or mint.
            repl.join_title().await;
        }

        // ---- the dispatch chain, in Go's fixed order; first match wins ----
        // The input EXPANSIONS (`/edit`, `/redo`, `/skills <name>`) rewrite `content` and
        // fall through into the message path; the echo still shows what was typed.
        let mut content = line.clone();
        // A notice never reaches the dispatch chain: it is not something the user typed, and its
        // text (a `[background job …]` headline) could not be a command anyway.
        if !notice {
            // /file heads Go's chain (chat/run.go:390): the path form attaches immediately,
            // the bare form opens the Attached/Add surface (WP54, T-12).
            if let Some(arg) = match_cmd(&line, "/file") {
                file::cmd_file(&mut repl, arg).await;
                continue;
            }
            // /edit and /redo exist only on a dedicated image provider (run.go:450-516).
            if repl.table.image_enabled()
                && let Some(arg) = match_cmd(&line, "/edit")
            {
                match edit::cmd_edit(&mut repl, arg).await {
                    EditOutcome::Continue => continue,
                    EditOutcome::Send(c) => content = c,
                }
            }
            if repl.table.image_enabled()
                && let Some(arg) = match_cmd(&line, "/redo")
            {
                match edit::cmd_redo(&mut repl, arg) {
                    EditOutcome::Continue => continue,
                    EditOutcome::Send(c) => content = c,
                }
            }
            if match_cmd(&line, "/model").is_some() {
                model::cmd_model(&mut repl).await;
                repl.push_status();
                continue;
            }
            if match_cmd(&line, "/session").is_some() {
                session::cmd_session(&mut repl).await;
                continue;
            }
            // /compact sits between /session and /export in Go's chain, and exists only while
            // the meter is live — the same gate its table row hangs off, so it can never be
            // advertised without dispatching (chat/run.go:797-802).
            if repl.ctxm.is_enabled()
                && let Some(hint) = match_cmd(&line, "/compact")
            {
                crate::repl::commands::compact::compact_now(&mut repl, hint, true).await;
                continue;
            }
            if let Some(arg) = match_cmd(&line, "/export") {
                export::cmd_export(&mut repl, arg).await;
                continue;
            }
            if repl.table.save_enabled()
                && let Some(arg) = match_cmd(&line, "/save")
            {
                save::cmd_save(&mut repl, arg);
                continue;
            }
            if match_cmd(&line, "/tools").is_some() {
                tools::cmd_tools(&repl).await;
                continue;
            }
            if let Some(arg) = match_cmd(&line, "/debug") {
                debug::cmd_debug(&mut repl, arg).await;
                continue;
            }
            // /skills exists only in agent mode (run.go:923-937); the same gate as its rows.
            if repl.overlay.is_some()
                && repl.table.agent_enabled()
                && let Some(arg) = match_cmd(&line, "/skills")
            {
                match skills::cmd_skills(&mut repl, arg).await {
                    SkillsOutcome::Continue => continue,
                    SkillsOutcome::Send(c) => content = c,
                }
            }
            if match_cmd(&line, "/status").is_some() {
                status::cmd_status(&mut repl).await;
                continue;
            }
        }

        // ---- the message path (chat/run.go:958-998) ----
        // The ❯ echo comes FIRST, so what the user typed is on screen even if the model
        // pick below fails. A notice prints its ONE headline instead — its output belongs to the model,
        // not to the scrollback (the file is there for whoever wants all of it).
        if notice {
            repl.tr.notice(&input.display);
        } else {
            repl.tr.user(&input.display);
        }
        if repl.provider.model().is_empty() {
            let cancel = repl.cancel.clone();
            if !model::ensure_model(&mut repl, &cancel).await {
                continue;
            }
        }
        let send_overlay = repl.refresh_overlay();

        // The auto-compaction offer runs on the message about to be sent, BEFORE it joins
        // the history: what it asks about is the projected occupancy, and compacting after
        // the append would summarize the very message being sent (chat/run.go:979-989).
        crate::repl::commands::compact::offer_before_send(&mut repl, &content).await;

        repl.history.push(Message {
            content,
            attachments: std::mem::take(&mut repl.pending),
            body: if notice { Body::Notice } else { Body::User },
        });
        let hist0 = repl.history.len();
        // Name the session NOW — before the turn, which may spend minutes in tool calls.
        repl.title_now();
        let turn_snap = repl.budget.snap();

        // The code theme is refreshed BETWEEN turns (chat/run.go:1007 `applyCodeTheme`): a
        // host that knows the background tone re-shades the code blocks, the diff shades
        // and the composer; a theme flip never lands inside a streaming block.
        // (chat/theme.go `applyCodeTheme`: the hosts are asked per turn — the cmux RPC child runs
        // under its own deadline; `None` = no host knows, keep the pre-loop probe's answer.)
        if let Some(known) = pres.dark_background().await {
            repl.dark = known;
            repl.tr.set_dark(known);
            ui.set_dark_background(known);
        }
        let cx = repl.turn_ctx(send_overlay);
        let mut steer = Steerer::new(Arc::clone(&ui), Arc::clone(&tr));

        // Turn-level retry (chat/run.go:1030-1068). Each attempt RESETS the turn's
        // messages and live estimates, then re-lands the injections a previous attempt
        // took off the queue — the queue no longer holds them.
        let mut attempt: u32 = 0;
        let outcome = loop {
            repl.history.truncate(hist0);
            let injected = steer.injected().to_vec();
            repl.history.extend(injected.iter().cloned());
            repl.ctxm.reset();
            repl.budget.restore(turn_snap);
            if let Some(user) = repl.history.get(hist0 - 1) {
                repl.ctxm.note(user); // the send moves the meter immediately
            }
            for m in &injected {
                repl.ctxm.note(m);
            }
            let report = run_turn(
                &cx,
                &root_cancel,
                &*repl.provider,
                &mut repl.history,
                &mut repl.ctxm,
                &mut steer,
            )
            .await;
            // A closed facade is the ONE failure a turn cannot survive: there is nothing
            // left to print the error on.
            let chat_err = match &report.outcome {
                Ok(_) => break report,
                Err(TurnFailure::Chat(c)) => c,
                Err(TurnFailure::Ui(ui_err)) => break 'main Err(ReplError::Ui(*ui_err)),
            };
            if repl.image_provider || attempt >= MAX_RETRIES || !is_retryable(chat_err) {
                break report;
            }
            if report.used_tools {
                // The tool loop already retried its own failing calls with the completed
                // rounds — and their side effects — kept in place: its errors are final.
                break report;
            }
            if report.side_fx > 0 {
                // An invariant, not a live path: a plain streamed turn executes no tools.
                // If one ever lands here, refuse loudly rather than re-run the user's
                // tools.
                repl.tr.notice(&format!(
                    "⟳ {} — not replaying the turn: {} executed tool call(s) would run again",
                    crate::repl::errors::describe_error(chat_err).headline,
                    report.side_fx
                ));
                break report;
            }
            attempt += 1;
            let headline = crate::repl::errors::describe_error(chat_err).headline;
            let busy = ui.busy(&format!(
                "{headline} — retrying (attempt {attempt}/{MAX_RETRIES})"
            ));
            tokio::select! {
                biased;
                () = root_cancel.cancelled() => { busy.stop(); break report; }
                () = tokio::time::sleep(RETRY_BACKOFF * attempt) => {}
            }
            busy.stop();
        };

        let interrupted = outcome.is_interrupted();
        let TurnReport {
            outcome: turn_result,
            partial,
            partial_reasoning,
            ..
        } = outcome;
        match turn_result {
            Err(_) if interrupted => {
                interrupt_turn(&mut repl, hist0 - 1, &partial, &partial_reasoning);
                // The user did the interrupting: back to idle, no ping (run.go:1070-1074).
                repl.pres.set_state(State::Idle);
            }
            Err(failure) => {
                // A `Ui` failure broke out above; classifying one costs nothing and keeps
                // this total.
                let report = match &failure {
                    TurnFailure::Chat(c) => crate::repl::errors::describe_error(c),
                    TurnFailure::Ui(u) => crate::repl::errors::ErrorReport::request_failed(u),
                };
                // The host hears about the failure BEFORE the red block lands (run.go:1076-1080).
                repl.pres.set_state(State::Error);
                repl.pres.notify(Event {
                    kind: Kind::Failed,
                    text: report.headline.clone(),
                });
                repl.tr.error_block(&report.headline, &report.lines());
                // The turn rolls back WITH its user message — and the name derived from it.
                repl.history.truncate(hist0 - 1);
                repl.titler.unseed(&repl.history);
                repl.ctxm.reset();
                repl.budget.restore(turn_snap);
                repl.push_status();
            }
            Ok(out) => {
                let mut amsg = Message::assistant_body(
                    out.content,
                    AssistantBody {
                        reasoning: out.reasoning,
                        // The final round's cost rides its own message (chat/run.go:1093), so
                        // a resumed session recomputes exactly what the live figures showed.
                        usage: out.usage,
                        // And its raw blocks (chat/run.go:1092-1099): a thinking block must go
                        // back on every later request that replays this turn, and a turn ending
                        // in text is the common case, not the tool-round one.
                        raw_content: out.raw_content,
                        ..AssistantBody::default()
                    },
                );
                repl.ctxm.record(Some(&mut amsg));
                collect_images(
                    &repl.tr,
                    usize::from(ui.width()),
                    (repl.images_dir)().as_deref(),
                    &out.images,
                    &mut amsg,
                );
                // The ping's text (run.go:1100-1106): the reply's first line, or `Image ready`
                // for an image-only reply — decided AFTER `collect_images` attached them.
                let digest = if amsg.content.is_empty() && !amsg.attachments.is_empty() {
                    IMAGE_READY.to_owned()
                } else {
                    notify_digest(&amsg.content)
                };
                repl.history.push(amsg);
                repl.persist_turn();
                repl.ctxm.reset();
                let history = std::mem::take(&mut repl.history);
                repl.budget.update(&history);
                repl.history = history;
                repl.push_status();
                repl.pres.set_state(State::Idle);
                repl.pres.notify(Event {
                    kind: Kind::Done,
                    text: digest,
                });
            }
        }
    };
    // The loop is over: a background job has no one left to report to, and `background` never promised to
    // outlive iota. `kill_all` is synchronous `killpg`, so nothing depends on a task being polled again.
    repl.jobs.kill_all();
    // Go's `defer pres.Close()`: the hosts are cleared before the facade goes down.
    repl.pres.close().await;
    outcome
}

impl Repl {
    /// The turn's context for one message (chat/run.go:1007-1012): the shared handles plus
    /// the overlay this message composed.
    fn turn_ctx(&self, overlay: String) -> TurnCtx {
        TurnCtx {
            ui: Arc::clone(&self.ui),
            tr: Arc::clone(&self.tr),
            dispatch: Arc::clone(&self.dispatch),
            gate: Arc::clone(&self.gate),
            overlay,
            images_dir: Arc::clone(&self.images_dir),
            can_retry: !self.image_provider,
            code_theme: code_theme_of(self.dark),
            pres: Arc::clone(&self.pres),
        }
    }

    /// Re-probes the agent-mode overlay for this message; the notices fire ONLY on a real
    /// change (chat/run.go:965-976; the D-27 lift, T-37). Returns the overlay text to send,
    /// `""` outside agent mode.
    fn refresh_overlay(&mut self) -> String {
        let Some(o) = self.overlay.as_mut() else {
            return String::new();
        };
        let (agents_changed, skills_changed) = o.refresh();
        if agents_changed {
            self.tr
                .notice(&format!("AGENTS.md reloaded ({} files)", o.file_count()));
        }
        if skills_changed {
            // A changed catalog re-derives the completion table: the per-skill rows are
            // rebuilt and the table re-issued (completion.go:63-73, run.go:971).
            self.table.set_skills(skill_entries(o.skills()));
            self.ui.set_slash_commands(self.table.active());
            self.tr
                .notice(&format!("Skills reloaded ({} skill(s))", o.skill_count()));
            for warn in o.warnings() {
                self.tr.notice(&format!("⚠ {warn}"));
            }
        }
        o.content()
    }

    /// Seeds the session's name and fires the async model pass (chat/run.go:222-251
    /// `titleNow`).
    ///
    /// The placeholder lands SYNCHRONOUSLY; the model pass runs while the turn streams, on
    /// the title provider (the conversation's is mid-call, and its per-call state is not
    /// safe for a concurrent request). No title provider leaves the placeholder standing.
    fn title_now(&mut self) {
        let Some((first_user, generation)) = self.titler.seed(&self.history) else {
            return;
        };
        let Some(tp) = self.title_provider.as_ref().map(Arc::clone) else {
            return;
        };
        let model = self.provider.model().to_owned();
        let titler = Arc::clone(&self.titler);
        let cancel = self.cancel.child_token();
        self.title_task = Some(tokio::spawn(async move {
            let mut guard = tp.lock().await;
            guard.set_model(model);
            let name = tokio::time::timeout(
                TITLE_TIMEOUT,
                generate_title_text(&cancel, &**guard, &first_user),
            )
            .await
            .unwrap_or_default();
            drop(guard);
            titler.land(generation, &name);
        }));
    }

    /// Waits for an in-flight title pass, so a landed name is written before anything
    /// touches the writer.
    async fn join_title(&mut self) {
        if let Some(h) = self.title_task.take() {
            let _ = h.await;
        }
    }
}

/// Applies the three-state interrupt table and its bookkeeping (chat/run.go:281-307
/// `interruptTurn`).
///
/// The user did the interrupting, so this is not an error path: no red block, no
/// notification. A discarded turn hands its attachments BACK — cancelling a send must not
/// silently strip the file the user attached — and gives the session name back with them.
fn interrupt_turn(repl: &mut Repl, watermark: usize, partial: &str, partial_reasoning: &str) {
    let InterruptDecision {
        history,
        persist,
        dropped_attachments: dropped,
    } = finalize_interrupt(
        std::mem::take(&mut repl.history),
        watermark,
        partial,
        partial_reasoning,
    );
    repl.history = history;
    repl.tr.notice("Interrupted.");
    if !dropped.is_empty() {
        let n = dropped.len();
        let mut kept = dropped;
        kept.append(&mut repl.pending);
        repl.pending = kept;
        repl.tr
            .notice(&format!("{n} attachment(s) kept for your next message."));
    }
    if persist {
        // A cancelled call rarely reports usage, but when it did (the figures arrived
        // before ESC) the partial message carries them like any other.
        if let Some(last) = repl.history.last_mut()
            && last.interrupted()
        {
            repl.ctxm.record(Some(last));
        }
        repl.persist_turn();
    } else {
        repl.titler.unseed(&repl.history);
    }
    repl.ctxm.reset();
    let history = std::mem::take(&mut repl.history);
    repl.budget.update(&history);
    repl.history = history;
    repl.push_status();
}
