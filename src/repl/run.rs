//! `RunParams` + the interactive REPL loop (chat.Run port; `TUI_CONTRACTS` §7,
//! `TUI_DESIGN` §8.3).
//!
//! The loop is deliberately IMPERATIVE and deliberately outside `crate::ui`: it talks to the
//! terminal only through the [`Ui`](crate::ui::facade::Ui) facade, and everything below the facade (the frame
//! engine, the composer, the surfaces) knows nothing about chat. Its shape is Go's, in
//! Go's order:
//!
//! 1. the banner — the card: the mark and the version, the mode and the directory — then the
//!    resume echo OR one blank line, so exactly one blank separates the environment from
//!    the first transcript block;
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
use std::sync::{Arc, Mutex};

use crate::agents::Overlay;
use crate::agents::memory::Snapshot;
use crate::agents::skills::Skill;
use crate::host::{Event, Kind, Presenter, State};
use crate::llm::reqlog::RequestLog;
use crate::markdown::CodeTheme;
use crate::provider::model::{AssistantBody, Body, Message};
use crate::session::{SessionError, SessionWriter};
use crate::sync::lock;
use crate::ui::facade::{InputKind, StatusData};
use tokio_util::sync::CancellationToken;

use crate::config::AgentMode;
use crate::repl::ReplError;
use crate::repl::bot::{Action as BotAction, Event as BotEvent};
use crate::repl::commands::edit::EditOutcome;
use crate::repl::commands::skills::SkillsOutcome;
use crate::repl::commands::{
    CmdFlags, CommandTable, SkillEntry, debug, edit, export, file, jobs, match_cmd, model, save,
    session, skills, status, tools,
};
use crate::repl::context::meter::{ContextBudget, CtxMeter};
use crate::repl::render::banner::{BannerFacts, banner_lines, overlay_warnings};
use crate::repl::render::mcpreport::report_mcp_failures;
use crate::repl::render::replay::{RESUME_ECHO_ROUNDS, echo_rounds, last_rounds};
use crate::repl::render::transcript::{Transcript, notify_digest};
use crate::repl::state::{BotState, Conversation, SessionSlot, UiHandles};
use crate::repl::title::{
    SessionTitle, TITLE_TIMEOUT, WriterSlot, generate_title_text, status_model_label, window_title,
};
use crate::repl::turn::approval::ApprovalGate;
use crate::repl::turn::interrupt::{InterruptDecision, finalize_interrupt};
use crate::repl::turn::{TurnCtx, TurnEngine, TurnFailure, TurnReport, collect_images};

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
#[derive(Default)]
pub struct McpHooks {
    /// The live server snapshot, re-read on every refresh tick. `None` = a build with no
    /// MCP at all, which renders the "No MCP servers configured." tab and every tool as a
    /// built-in.
    pub servers: Option<Arc<dyn Fn() -> Vec<crate::mcp::ServerStatus> + Send + Sync>>,
    /// Terminal connect statuses (the MCP reporter task drains it).
    pub events: Option<tokio::sync::mpsc::Receiver<McpEvent>>,
    /// The manager itself: the in-session entry to `Manager::login`/`logout`/`reconnect` (a reconnect changes
    /// the live tool set, which only the manager can do). No slash command calls it since `/mcp` left (X-42);
    /// it is wired for the config toolset (brain page `config-tools`), which logs a server in from inside a
    /// chat. `None` = no servers in this run.
    pub manager: Option<Arc<crate::mcp::Manager>>,
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
    /// The writer is a bot's session (docs/design/bot-mode.md §2.2): `/session` is not registered, the
    /// title pass never runs (the bundle is named after the bot), and the meta is stamped with the
    /// parameters the chat actually starts under even when the bundle was resumed — for a bot the config,
    /// not the session, is what they come from.
    pub bot: bool,
    /// Dim lines the transcript opens with, after the banner and the resume echo — what the wiring
    /// learned about the session that the model's user should see (a bot's pointer that never saved, a
    /// system prompt taken over from the config).
    pub notices: Vec<String>,
    /// What the model must be told at startup, not only the user (bot-mode.md §2.5: a resumed bot's time
    /// away and a changed project — across a restart its only sense of either). Each is shown as a dim line
    /// AND recorded as a notice message into the history and the log, like a memory write (§3.7).
    pub recorded_notices: Vec<String>,
    /// A bot's memory (bot-mode.md §3.4, §3.7): the loop injects its snapshot as the last part of every
    /// send's overlay and records what the `remember` tool announced during a turn as notice messages once
    /// the turn is over. `None` outside a bot's session.
    pub memory: Option<crate::agents::memory::BotMemory>,
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
    /// The built-in harness prompt's inputs (`agents::harness`): the loop composes the text once the presenter
    /// is here — the hosts add their facts to `<environment>` — and again on the first send of a new day
    /// (docs/design/bot-mode.md §2.5). It goes ahead of `system` on every send and never into the history; an
    /// agent without tools composes `""`.
    pub harness: crate::agents::harness::HarnessInputs,
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
    /// The agent's choices, as `/model` offers them (`repl::catalog`).
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
/// can live in their own files instead of one 900-line function — in its three parts
/// (`crate::repl::state`).
pub(crate) struct Repl {
    /// What is being said, to which model, under which parameters.
    pub(crate) conv: Conversation,
    /// The bundle it is persisted into, and the name it carries.
    pub(crate) session: SessionSlot,
    /// The facade, the transcript, the presenter and the run's shared handles.
    pub(crate) handles: UiHandles,
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
        let model = status_model_label(
            self.conv.provider.model(),
            self.conv.provider.kind().as_str(),
        );
        if self.conv.ctxm.publish_status(&model) {
            return;
        }
        self.handles.ui.set_status(StatusData {
            model,
            debug: self.handles.reqlog.verbose(),
            ..StatusData::default()
        });
    }

    /// Appends whatever the bundle has not seen (chat/run.go:221-229 `persistTurn`).
    ///
    /// A failure warns and does NOT advance the watermark, so the next successful persist
    /// carries the backlog — which is also how `/save` flushes a whole ephemeral chat in
    /// one append. A batch that reached the log with only its meta rewrite failing is
    /// saved: it warns and advances.
    ///
    /// `false`: a backlog is left that the log does not have. An ephemeral chat has none.
    pub(crate) fn persist_turn(&mut self) -> bool {
        let mut slot = lock(&self.session.writer);
        let Some(w) = slot.as_mut() else { return true };
        if self.session.persisted >= self.conv.history.len() {
            return true;
        }
        let res = w.append_messages(&self.conv.history[self.session.persisted..]);
        drop(slot);
        let saved = matches!(res, Ok(()) | Err(SessionError::MetaNotSaved(_)));
        if saved {
            self.session.persisted = self.conv.history.len();
        }
        if let Err(e) = res {
            self.handles
                .tr
                .error(&format!("Warning: failed to save session: {e}"));
        }
        saved
    }

    /// Records the memory writes the turn just made (bot-mode.md §3.7 item 1): one dim line and one notice
    /// message each, a remove's included, persisted at once so the log says when what was written. The queue
    /// is drained, so a write is recorded exactly once. Returns how many lines they saved — adds and replaces
    /// only: a remove saved nothing, and a flush that only removed must tell the summary pass so (§3.6.2
    /// item 3), not "saved 1 line".
    pub(crate) fn record_memory_writes(&mut self) -> u32 {
        let Some(log) = &self.session.memory_writes else {
            return 0;
        };
        let written = log.take();
        let saved = written.iter().filter(|w| w.saved).count();
        self.record_notices(written.into_iter().map(|w| w.notice).collect());
        u32::try_from(saved).unwrap_or(u32::MAX)
    }

    /// Tells the model, not only the user: each line is shown dim and joins the history as a notice
    /// message, persisted at once so the log says when it was said. Nothing for no lines.
    pub(crate) fn record_notices(&mut self, notices: Vec<String>) {
        if notices.is_empty() {
            return;
        }
        for notice in notices {
            self.handles.tr.notice(&notice);
            self.conv.history.push(Message::notice(notice));
        }
        self.persist_turn();
    }

    /// Tells the hosts which session the chat persists into — the live writer's id and bundle
    /// directory. Called wherever the writer is settled: at start-up, after `/save` mints one, and
    /// after `/session` swaps it. Nothing is said for an ephemeral chat.
    pub(crate) fn report_session(&self) {
        if let Some(w) = lock(&self.session.writer).as_ref() {
            self.handles.pres.set_session(w.id(), w.dir());
        }
    }
}

/// The project a bot's memory block is cut to: the agent root's directory name (bot-mode.md §3.2), `None`
/// outside agent mode.
fn memory_project(agent: &crate::headless::AgentOptions) -> Option<String> {
    agent
        .enabled
        .then(|| agent.root.file_name())
        .flatten()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The overlay's parts in send order, a blank line between them; an empty part is left out.
fn join_overlay(agent: String, memory: String) -> String {
    match (agent.is_empty(), memory.is_empty()) {
        (_, true) => agent,
        (true, false) => memory,
        (false, false) => format!("{agent}\n\n{memory}"),
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
        harness,
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
        bot,
        notices,
        recorded_notices,
        memory,
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
    // A bot's memory is read once here (bot-mode.md §3.4's first refresh moment); its writes are recorded
    // through the log the tool shares.
    let memory_writes = memory.as_ref().map(|m| m.writes().clone());
    let bot_state = memory.map(|m| BotState {
        name: m.name().to_owned(),
        memory: Snapshot::load(m),
        flush: crate::repl::bot::Flush::default(),
        tool_tokens: crate::repl::context::tokens::TokenCounter::new()
            .count_tools(&dispatch.tools()),
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
    // The harness, now that the hosts it names in `<environment>` are known; the day it was composed on is
    // what the first send of the next one compares against.
    let harness_day = harness.today();
    let harness_text = harness.compose(&harness_day, pres.environment());
    let mut budget = ContextBudget::new(params.context_window.value);
    if bot_state.is_some() {
        // A bot compacts with a larger reserve: the flush turn, and maybe a user turn, come first (§3.6.1).
        budget.set_bot_reserve();
    }
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
    // A resumed bot is seeded once its loop state is assembled (below): its overhead needs the memory copy,
    // and its measurement the writer.
    if !history.is_empty() && bot_state.is_none() {
        budget.update(&history);
    }
    let writer: WriterSlot = Arc::new(Mutex::new(writer));
    // A bundle this run CREATED must be told what the chat is running under (below, once the loop state is
    // assembled); one it RESUMED already says.
    let mut fresh_bundle = false;
    {
        let mut slot = lock(&writer);
        if let Some(w) = slot.as_mut() {
            // A resumed session's cumulative ↑/↓ figures are what its own log adds up to.
            ctxm.seed_totals(w.usage());
            fresh_bundle = !w.on_disk() || bot;
        }
    }

    // chat/run.go:140-146 `setActiveCommands(agent, save, compact, image)` + the skill rows.
    let mut table = CommandTable::new(CmdFlags {
        save: new_session.is_some(),
        compact: token_aware && ctxm.is_enabled(),
        agent: overlay.is_some(),
        image: image_provider,
        jobs: false,
        bot,
    });
    if let Some(o) = overlay.as_ref() {
        table.set_skills(skill_entries(o.skills()));
    }
    let table = Arc::new(Mutex::new(table));
    // The thinking meter counts with the chat's ONE tokenizer (Go handed `newTranscript`
    // the budget's own counter).
    let estimator: Option<crate::repl::render::transcript::TokenEstimator> = {
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

    // A bot has an agent's overlay; what tells it apart is its session. The banner and `/status`
    // both read this one value.
    let mode = if bot {
        AgentMode::Bot
    } else if overlay.is_some() {
        AgentMode::Agent
    } else {
        AgentMode::Chat
    };

    // ---- the banner, then EITHER the resume echo OR one blank (chat/run.go:86-116) ----
    // Exactly one blank separates the environment from the first transcript block: an echo
    // ends with its own round separator, so it is not followed by another.
    {
        let session_id = lock(&writer).as_ref().map(|w| w.id().to_owned());
        // The directory the chat runs in: the project root in agent mode, else the cwd.
        let dir = if overlay.is_some() {
            agent.root.clone()
        } else {
            agent
                .cwd
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default()
        };
        let mut lines = banner_lines(
            &BannerFacts {
                mode,
                session_id: session_id.as_deref(),
                ephemeral: new_session.is_some(),
                resumed,
                dir: &dir,
                home: agent.home.as_deref(),
                host: pres.detected_host(),
            },
            ui.width(),
        );
        lines.extend(overlay_warnings(overlay.as_ref()));
        ui.print_lines(lines);
    }
    ui.print_lines(vec![String::new()]);
    if resumed {
        let msgs = last_rounds(&history, RESUME_ECHO_ROUNDS);
        if !msgs.is_empty() {
            let d = Arc::clone(&dispatch);
            let img_dir = lock(&writer).as_ref().map(SessionWriter::images_path);
            let lines = echo_rounds(
                msgs,
                |n| d.presentation(n) == crate::tool::Presentation::Surface,
                usize::from(ui.width()),
                img_dir.as_deref(),
            );
            ui.print_lines(lines);
        }
    }
    for notice in &notices {
        tr.notice(notice);
    }
    if let Some(warn) = bot_state.as_ref().and_then(|b| b.memory.warning()) {
        tr.notice(&format!("⚠ {warn}"));
    }

    let titler = Arc::new(SessionTitle::new(
        Arc::clone(&writer),
        {
            let ui = Arc::clone(&ui);
            Box::new(move |name: &str| ui.set_title(&window_title(name)))
        },
        // A bot's bundle is named after the bot from the start (§2.2): nothing is seeded over that name.
        resumed || bot,
    ));
    ui.set_title(&window_title(
        &lock(&writer)
            .as_ref()
            .map_or_else(String::new, |w| w.meta().title.clone()),
    ));
    ui.set_slash_commands(lock(&table).active());

    let gate = Arc::new(ApprovalGate::new(
        Arc::clone(&ui),
        Arc::clone(&tr),
        Arc::clone(&pres),
    ));
    let images_dir: crate::repl::turn::ImagesDir = {
        let slot = Arc::clone(&writer);
        Arc::new(move || lock(&slot).as_mut().and_then(SessionWriter::images_dir))
    };
    let title_provider = title_provider.map(|p| Arc::new(tokio::sync::Mutex::new(p)));
    let mut repl = Repl {
        conv: Conversation {
            provider,
            dispatch: Arc::clone(&dispatch),
            history,
            pending: Vec::new(),
            budget,
            ctxm,
            param_sources: params.sources(),
            layers,
            catalog,
            compact_declined: 0,
            harness: harness_text,
            harness_day,
            harness_inputs: harness,
            overlay,
            agent,
            mode,
            bot: bot_state,
            image_provider,
        },
        session: SessionSlot {
            writer: Arc::clone(&writer),
            store,
            scope,
            new_session,
            persisted,
            titler: Arc::clone(&titler),
            title_provider,
            title_task: None,
            images_dir,
            memory_writes,
        },
        handles: UiHandles {
            ui: Arc::clone(&ui),
            tr: Arc::clone(&tr),
            pres: Arc::clone(&pres),
            reqlog: Arc::clone(&reqlog),
            table,
            gate: Arc::clone(&gate),
            mcp,
            dark,
            cancel: root_cancel.clone(),
            jobs: Arc::clone(&jobs),
        },
    };
    // What a resumed bot's model is told before anything else is said (bot-mode.md §2.5).
    repl.record_notices(recorded_notices);
    if repl.conv.bot.is_some() {
        repl.price_bot_overhead();
        if resumed {
            // What the last run's meter held (§4.1): the last answer's measurement, the rest estimated on
            // top — and a flush that run had queued is owed again.
            let measured = lock(&writer).as_ref().and_then(SessionWriter::measured);
            let history = std::mem::take(&mut repl.conv.history);
            repl.conv.budget.seed_resumed(&history, measured);
            repl.conv.history = history;
            let over = repl
                .conv
                .budget
                .should_offer_compact(0, repl.conv.compact_declined);
            if let Some(bot) = repl.conv.bot.as_mut() {
                let actions = bot.flush.step(BotEvent::Resumed { over });
                bot_perform(&mut repl, actions, Vec::new(), "").await;
            }
        } else {
            let history = std::mem::take(&mut repl.conv.history);
            repl.conv.budget.reseed(&history);
            repl.conv.history = history;
        }
    }
    repl.push_status();
    // The loop is up and waiting: the hosts hear `Idle` NOW, not at the first turn — a herdr pane
    // is listed from here — and then which session it writes into (herdr keys its records on it).
    // Announced in that order: a host learns the chat exists before it learns what it saves.
    repl.handles.pres.set_state(State::Idle);
    repl.report_session();

    // What the chat is running under, and where each value came from, into a bundle this run created — so a
    // resume finds the session as it was rather than re-deriving it from a config that may have moved since
    // (brain page `model-param-layering`). The values are read back from the provider and the budget, so a
    // knob the dialect cannot act on is never recorded as though it had applied; the window is left out
    // entirely for a provider with no token accounting.
    if fresh_bundle {
        let running = crate::repl::liveparams::current(&mut repl);
        let window = token_aware.then(|| repl.conv.budget.window());
        crate::repl::liveparams::stamp_bundle(&repl, window, &running);
    }

    // A finished job becomes the next input: the facade serves it to a parked `read_input` at once (an idle
    // loop wakes and answers it) or queues it behind what is already typed ahead, and `Steerer::drain` takes
    // it at the next round boundary when a turn is running. ONE delivery path, no second queue.
    {
        let ui_sink = Arc::clone(&ui);
        repl.handles.jobs.set_sink(Some(Box::new(move |done| {
            ui_sink.enqueue(job_notice(&done));
        })));
    }
    // The running set is what `/jobs` hangs off: the watch hears every change, in the registry's order — a
    // call that yielded, a `background: true` start, a job gone (with the job, when it ended: its notice is
    // the next thing delivered) — and flips the row, re-issuing the table the `/save` way when the flag
    // actually moved. The one-table law holds because the same flag gates the dispatch arm below.
    {
        let ui_watch = Arc::clone(&ui);
        let table_watch = Arc::clone(&repl.handles.table);
        let pres_watch = Arc::clone(&repl.handles.pres);
        repl.handles.jobs.set_watch(Some(Box::new(move |set, end| {
            let any = !set.is_empty();
            // An idle chat with a job running is Busy to the host, and so is one owed an ended
            // job's notice (`host` module doc): the end is a fact, not a drop in the count.
            match end {
                Some(_) => pres_watch.job_ended(set.len()),
                None => pres_watch.set_jobs(set.len()),
            }
            // The status row's job segment follows the same set (and ticks while it is non-empty).
            ui_watch.set_jobs(set);
            let mut table = lock(&table_watch);
            if table.set_jobs(any) {
                ui_watch.set_slash_commands(table.active());
            }
        })));
    }

    if let Some(events) = repl.handles.mcp.events.take() {
        tokio::spawn(report_mcp_failures(events, Arc::clone(&tr), ui.done()));
    }

    // ---- pre-loop interactions, INSIDE the facade (chat/run.go:349-371) ----
    // Go also offered to type a system prompt here, behind `-S`. That flag is gone: it described
    // configuration one keystroke at a time, and an `agents:` entry carries a prompt permanently (brain page
    // `cli-surface-agent-first`).
    if repl.conv.provider.model().is_empty() {
        // v1 offered the pick at startup; ESC defers — the first message re-prompts.
        model::ensure_model(&mut repl, &root_cancel).await;
        repl.push_status();
    }

    // ---- the main loop ----
    // Every exit path breaks out with its outcome so the presenter is closed exactly once
    // below (Go's `defer pres.Close()`).
    let outcome: Result<(), ReplError> = 'main: loop {
        let Ok(input) = ui.read_input(&root_cancel).await else {
            // ErrInterrupted (idle Ctrl+C / Ctrl+D), ErrClosed, or shutdown. A landed name is
            // already written; an unfinished title pass is given up — the placeholder stands
            // rather than the exit waiting on a model that may never answer.
            repl.session.abort_title();
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
        // A bot's memory-flush notice (bot-mode.md §3.6.1) runs as the flush turn — unless the compaction it
        // was queued for already happened (a message typed ahead of it compacted first): then it is dropped,
        // neither sent nor persisted.
        let flush = notice && crate::repl::bot::is_flush_notice(&input.text);
        if flush && !bot_notice_arrived(&mut repl).await {
            continue;
        }
        // Whatever the input is, the loop is awake for the user now (run.go:381).
        repl.handles.pres.set_state(State::Idle);
        // A flush notice is the loop's own, not a job's: it never held the host.
        if notice && !flush {
            // The notice's turn holds the host from here (`host` module doc).
            repl.handles.pres.notice_taken();
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
            if lock(&repl.handles.table).image_enabled()
                && let Some(arg) = match_cmd(&line, "/edit")
            {
                match edit::cmd_edit(&mut repl, arg).await {
                    EditOutcome::Continue => continue,
                    EditOutcome::Send(c) => content = c,
                }
            }
            if lock(&repl.handles.table).image_enabled()
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
            if lock(&repl.handles.table).session_enabled() && match_cmd(&line, "/session").is_some()
            {
                session::cmd_session(&mut repl).await;
                continue;
            }
            // /compact sits between /session and /export in Go's chain, and exists only while
            // the meter is live — the same gate its table row hangs off, so it can never be
            // advertised without dispatching (chat/run.go:797-802).
            if repl.conv.ctxm.is_enabled()
                && let Some(hint) = match_cmd(&line, "/compact")
            {
                crate::repl::commands::compact::compact_now(&mut repl, hint, true).await;
                continue;
            }
            if let Some(arg) = match_cmd(&line, "/export") {
                export::cmd_export(&mut repl, arg).await;
                continue;
            }
            if lock(&repl.handles.table).save_enabled()
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
            // /jobs exists while a background job runs — the same flag as its table row, read at the
            // moment of typing, so a job that ended a second ago leaves the text to the model.
            if lock(&repl.handles.table).jobs_enabled() && match_cmd(&line, "/jobs").is_some() {
                jobs::cmd_jobs(&repl).await;
                continue;
            }
            // /skills exists only in agent mode (run.go:923-937); the same gate as its rows.
            if repl.conv.overlay.is_some()
                && lock(&repl.handles.table).agent_enabled()
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
            repl.handles.tr.notice(&input.display);
        } else {
            repl.handles.tr.user(&input.display);
        }
        if repl.conv.provider.model().is_empty() {
            let cancel = repl.handles.cancel.clone();
            if !model::ensure_model(&mut repl, &cancel).await {
                continue;
            }
        }
        roll_day(&mut repl).await;

        // The auto-compaction offer runs on the message about to be sent, BEFORE it joins
        // the history: what it asks about is the projected occupancy, and compacting after
        // the append would summarize the very message being sent (chat/run.go:979-989).
        // The flush notice alone skips it: the flush turn runs over the threshold by design, and the
        // compaction follows it at once.
        if !flush {
            crate::repl::commands::compact::offer_before_send(&mut repl, &content).await;
        }
        // The overlay is read AFTER the offer: a compaction there refreshes a bot's memory copy, and this
        // send must carry the refreshed block — composed before, it would carry the old one and the next send
        // would change the system segment again (two cache misses where one was meant, bot-mode.md §3.4).
        let send_overlay = repl.refresh_overlay();

        repl.conv.history.push(Message {
            content,
            attachments: std::mem::take(&mut repl.conv.pending),
            body: if notice { Body::Notice } else { Body::User },
        });
        let hist0 = repl.conv.history.len();
        // Name the session NOW — before the turn, which may spend minutes in tool calls.
        repl.title_now();
        let turn_snap = repl.conv.budget.snap();

        // The code theme is refreshed BETWEEN turns (chat/run.go:1007 `applyCodeTheme`): a
        // host that knows the background tone re-shades the code blocks, the diff shades
        // and the composer; a theme flip never lands inside a streaming block.
        // (chat/theme.go `applyCodeTheme`: the hosts are asked per turn — the cmux RPC child runs
        // under its own deadline; `None` = no host knows, keep the pre-loop probe's answer.)
        if let Some(known) = pres.dark_background().await {
            repl.handles.dark = known;
            repl.handles.tr.set_dark(known);
            ui.set_dark_background(known);
        }
        let mut engine = TurnEngine::new(repl.turn_ctx(send_overlay, flush), root_cancel.clone());
        let outcome = match engine.run(&mut repl.conv, hist0, turn_snap).await {
            Ok(report) => report,
            Err(ui_err) => break 'main Err(ReplError::Ui(ui_err)),
        };
        let held = engine.take_held();
        let mut landed = false;

        let interrupted = outcome.is_interrupted();
        let stalled = outcome.is_stalled();
        let TurnReport {
            outcome: turn_result,
            partial,
            partial_reasoning,
            ..
        } = outcome;
        match turn_result {
            Err(_) if interrupted => {
                let saved = interrupt_turn(&mut repl, hist0 - 1, &partial, &partial_reasoning);
                // A turn the interrupt kept is in the history and the log like any other, and its usage
                // counts toward the threshold: it lands (fable M2), or crossing the threshold with it would
                // skip the flush. An interrupted flush turn is still one that did not finish.
                landed = saved && !flush;
                // The user did the interrupting: back to idle, no ping (run.go:1070-1074).
                repl.handles.pres.set_state(State::Idle);
            }
            Err(failure) => {
                // A `Ui` failure broke out above; classifying one costs nothing and keeps
                // this total.
                let report = match &failure {
                    TurnFailure::Chat(c) => crate::repl::errors::describe_error(c),
                    TurnFailure::Ui(u) => crate::repl::errors::ErrorReport::request_failed(u),
                };
                // The host hears about the failure BEFORE the red block lands (run.go:1076-1080). A failed
                // flush is not the host's business: it is best-effort, and the compaction runs anyway.
                if !flush {
                    repl.handles.pres.set_state(State::Error);
                    repl.handles.pres.notify(Event {
                        kind: Kind::Failed,
                        text: report.headline.clone(),
                    });
                }
                repl.handles
                    .tr
                    .error_block(&report.headline, &report.lines());
                // A stall keeps what the turn completed; anything else — or a stall with
                // nothing to keep — rolls the turn back WITH its user message, and the name
                // derived from it, so a title pass still in flight could only land a name
                // `unseed` already discards. A kept stall lands like a kept interrupt (fable M2).
                if stalled && keep_stalled_turn(&mut repl, hist0 - 1, &partial, &partial_reasoning)
                {
                    landed = !flush;
                } else {
                    repl.conv.history.truncate(hist0 - 1);
                    if repl.session.titler.unseed(&repl.conv.history) {
                        repl.session.abort_title();
                    }
                    repl.conv.ctxm.reset();
                    repl.conv.budget.restore(turn_snap);
                    repl.push_status();
                }
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
                repl.conv.ctxm.record(Some(&mut amsg));
                collect_images(
                    &repl.handles.tr,
                    usize::from(ui.width()),
                    (repl.session.images_dir)().as_deref(),
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
                repl.conv.history.push(amsg);
                repl.persist_turn();
                repl.conv.ctxm.reset();
                let history = std::mem::take(&mut repl.conv.history);
                repl.conv.budget.update(&history);
                repl.conv.history = history;
                repl.push_status();
                repl.handles.pres.set_state(State::Idle);
                // The flush turn is the loop talking to itself: no `Done` ping (bot-mode.md §4.3).
                if !flush {
                    repl.handles.pres.notify(Event {
                        kind: Kind::Done,
                        text: digest,
                    });
                }
                landed = true;
            }
        }
        // Whatever became of the turn, a memory write it made is on disk: it is recorded now.
        let memory_writes = repl.record_memory_writes();
        after_bot_turn(&mut repl, flush, landed, memory_writes, held).await;
    };
    // The loop is over: a background job has no one left to report to, and `background` never promised to
    // outlive iota. `kill_all` is synchronous `killpg`, so nothing depends on a task being polled again.
    repl.handles.jobs.kill_all();
    // Go's `defer pres.Close()`: the hosts are cleared before the facade goes down.
    repl.handles.pres.close().await;
    outcome
}

impl Repl {
    /// The turn's context for one message (chat/run.go:1007-1012): the shared handles plus
    /// the harness and the overlay this message composed.
    /// A bot's flush turn (`flush`) sees the memory set alone and takes no steering (bot-mode.md §3.6.1).
    fn turn_ctx(&self, overlay: String, flush: bool) -> TurnCtx {
        const MEMORY_ONLY: &[&str] = &[crate::tool::builtins::memory::REMEMBER];
        let dispatch = if flush {
            crate::tool::only(Arc::clone(&self.conv.dispatch), MEMORY_ONLY)
        } else {
            Arc::clone(&self.conv.dispatch)
        };
        TurnCtx {
            ui: Arc::clone(&self.handles.ui),
            tr: Arc::clone(&self.handles.tr),
            dispatch,
            gate: Arc::clone(&self.handles.gate),
            harness: self.conv.harness.clone(),
            overlay,
            images_dir: Arc::clone(&self.session.images_dir),
            can_retry: !self.conv.image_provider,
            code_theme: code_theme_of(self.handles.dark),
            pres: Arc::clone(&self.handles.pres),
            steering: !flush,
            mounts_only: flush.then_some(MEMORY_ONLY),
        }
    }

    /// Re-prices a bot's overhead — its memory block, as the next send will carry it, and its tool definitions
    /// — for the budget's local counts (`ContextBudget::set_overhead`). Called at startup and whenever the
    /// memory copy changes. Nothing outside a bot's session.
    pub(crate) fn price_bot_overhead(&mut self) {
        let project = memory_project(&self.conv.agent);
        let Some(bot) = self.conv.bot.as_ref() else {
            return;
        };
        let block = bot.memory.block(project.as_deref());
        let tokens = self.conv.budget.counter().count(&block) + bot.tool_tokens;
        self.conv.budget.set_overhead(tokens);
    }

    /// Re-probes the agent-mode overlay for this message; the notices fire ONLY on a real
    /// change (chat/run.go:965-976; the D-27 lift, T-37). Returns the overlay text to send:
    /// the AGENTS.md chain and the skills catalog, then a bot's `<memory>` block — last, as the
    /// part that changes most often (bot-mode.md §3.4); `""` when there is none of them.
    fn refresh_overlay(&mut self) -> String {
        let content = self.refresh_agent_overlay();
        let project = memory_project(&self.conv.agent);
        let Some(bot) = self.conv.bot.as_mut() else {
            return content;
        };
        // The fourth refresh moment: an edit from outside this process, picked up before the send. The other
        // three are startup (`run`), a successful compaction and the harness's day-change re-composition
        // (`roll_day`) — the flush machine's `RefreshMemory`, which calls `bot.memory.reload()`.
        if bot.memory.refresh() {
            self.handles.tr.notice("MEMORY.md reloaded");
            if let Some(warn) = bot.memory.warning() {
                self.handles.tr.notice(&format!("⚠ {warn}"));
            }
            self.price_bot_overhead();
        }
        let Some(bot) = self.conv.bot.as_ref() else {
            return content;
        };
        join_overlay(content, bot.memory.block(project.as_deref()))
    }

    /// The agent-mode half of [`Self::refresh_overlay`]: `""` outside agent mode.
    fn refresh_agent_overlay(&mut self) -> String {
        let Some(o) = self.conv.overlay.as_mut() else {
            return String::new();
        };
        let (agents_changed, skills_changed) = o.refresh();
        if agents_changed {
            self.handles
                .tr
                .notice(&format!("AGENTS.md reloaded ({} files)", o.file_count()));
        }
        if skills_changed {
            // A changed catalog re-derives the completion table: the per-skill rows are
            // rebuilt and the table re-issued (completion.go:63-73, run.go:971).
            {
                let mut table = lock(&self.handles.table);
                table.set_skills(skill_entries(o.skills()));
                self.handles.ui.set_slash_commands(table.active());
            }
            self.handles
                .tr
                .notice(&format!("Skills reloaded ({} skill(s))", o.skill_count()));
            for warn in o.warnings() {
                self.handles.tr.notice(&format!("⚠ {warn}"));
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
        let Some((first_user, generation)) = self.session.titler.seed(&self.conv.history) else {
            return;
        };
        let Some(tp) = self.session.title_provider.as_ref().map(Arc::clone) else {
            return;
        };
        let model = self.conv.provider.model().to_owned();
        let titler = Arc::clone(&self.session.titler);
        let cancel = self.handles.cancel.child_token();
        self.session.title_task = Some(tokio::spawn(async move {
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
}

/// A bot's bookkeeping once a turn is over (bot-mode.md §3.6.1); nothing outside a bot's session. The flush
/// machine decides (`repl::bot`), [`bot_perform`] does: after the flush turn the compaction runs at once; after
/// any other turn that landed at the (snoozed) threshold the flush notice is queued; flush notices the turn's
/// steering took off the queue go back on it while one is still owed.
async fn after_bot_turn(
    repl: &mut Repl,
    flush: bool,
    landed: bool,
    writes: u32,
    held: Vec<crate::ui::facade::Input>,
) {
    let over = repl
        .conv
        .budget
        .should_offer_compact(0, repl.conv.compact_declined);
    let Some(bot) = repl.conv.bot.as_mut() else {
        return;
    };
    let actions = bot.flush.step(BotEvent::TurnEnded {
        flush,
        landed,
        writes,
        over,
    });
    bot_perform(repl, actions, held, "").await;
}

/// Before every send (bot-mode.md §2.5): the harness's `date:` is today's. On the first send of a new day the
/// harness is re-composed — the system segment changes, so the prompt cache misses at most once a day — and a
/// bot's memory copy is refreshed in the same breath (§3.4's third moment), so the two changes cost one miss,
/// not two. The same day changes nothing.
async fn roll_day(repl: &mut Repl) {
    let today = repl.conv.harness_inputs.today();
    if today == repl.conv.harness_day {
        return;
    }
    repl.conv.harness = repl
        .conv
        .harness_inputs
        .compose(&today, repl.handles.pres.environment());
    repl.conv.harness_day = today;
    let Some(bot) = repl.conv.bot.as_mut() else {
        return;
    };
    let actions = bot.flush.step(BotEvent::DayChanged);
    bot_perform(repl, actions, Vec::new(), "").await;
}

/// A flush notice is the next input. `true`: run it as the flush turn. `false`: drop it — the compaction it
/// was queued for already happened, or this is not a bot's session at all.
async fn bot_notice_arrived(repl: &mut Repl) -> bool {
    let Some(bot) = repl.conv.bot.as_mut() else {
        return false;
    };
    let actions = bot.flush.step(BotEvent::NoticeArrived);
    bot_perform(repl, actions, Vec::new(), "").await
}

/// Carries out, in order, what a bot's flush machine answered (`repl::bot`, bot-mode.md §3.6.1, §4.1): the
/// machine decides, this only does. `held` are the flush notices a turn's steering took off the queue
/// ([`BotAction::Requeue`] puts them back); `error` is a failed compaction's text ([`BotAction::Alarm`] names
/// it). `false`: the input at hand is dropped ([`BotAction::DropNotice`]). Nothing outside a bot's session.
pub(crate) async fn bot_perform(
    repl: &mut Repl,
    actions: Vec<BotAction>,
    mut held: Vec<crate::ui::facade::Input>,
    error: &str,
) -> bool {
    let mut keep = true;
    for action in actions {
        match action {
            BotAction::QueueFlush => {
                let Some(bot) = repl.conv.bot.as_ref() else {
                    continue;
                };
                let consolidate = crate::agents::memory::soft_warning(bot.memory.current().len);
                repl.handles.ui.enqueue(crate::ui::facade::Input {
                    display: crate::repl::bot::FLUSH_HEADLINE.to_owned(),
                    text: crate::repl::bot::flush_notice(consolidate.as_deref()),
                    kind: InputKind::Notice,
                });
            }
            BotAction::Requeue => {
                for input in std::mem::take(&mut held) {
                    repl.handles.ui.enqueue(input);
                }
            }
            BotAction::DropNotice => keep = false,
            // Boxed: a compaction's outcome comes back through here.
            BotAction::Compact => {
                Box::pin(crate::repl::commands::compact::compact_now(repl, "", false)).await;
            }
            BotAction::RefreshMemory => {
                let Some(bot) = repl.conv.bot.as_mut() else {
                    continue;
                };
                bot.memory.reload();
                if let Some(warn) = bot.memory.warning() {
                    repl.handles.tr.notice(&format!("⚠ {warn}"));
                }
                repl.price_bot_overhead();
            }
            BotAction::Alarm => {
                let Some(bot) = repl.conv.bot.as_ref() else {
                    continue;
                };
                let text = format!("bot {}: compaction failing — {error}", bot.name);
                repl.handles.pres.set_state(State::Error);
                repl.handles.pres.notify(Event {
                    kind: Kind::Failed,
                    text,
                });
            }
            // The next attempt — and the next flush — waits for the usage to grow by 5% of the window.
            BotAction::Snooze => repl.conv.compact_declined = repl.conv.budget.used(),
        }
    }
    keep
}

/// Applies the three-state interrupt table and its bookkeeping (chat/run.go:281-307
/// `interruptTurn`).
///
/// The user did the interrupting, so this is not an error path: no red block, no
/// notification. A discarded turn hands its attachments BACK — cancelling a send must not
/// silently strip the file the user attached — and gives the session name back with them.
///
/// `true` when the turn was kept (and saved), `false` when it was discarded whole.
fn interrupt_turn(
    repl: &mut Repl,
    watermark: usize,
    partial: &str,
    partial_reasoning: &str,
) -> bool {
    let InterruptDecision {
        history,
        persist,
        dropped_attachments: dropped,
    } = finalize_interrupt(
        std::mem::take(&mut repl.conv.history),
        watermark,
        partial,
        partial_reasoning,
    );
    repl.conv.history = history;
    // Calls the interrupt left unanswered are answered now, before the turn is saved and before any notice
    // follows it: behind a notice they would sit mid-history, where no reload can repair them — and the
    // next send carries this history too.
    if persist {
        crate::session::repair_tail(&mut repl.conv.history);
    }
    repl.handles.tr.notice("Interrupted.");
    if !dropped.is_empty() {
        let n = dropped.len();
        let mut kept = dropped;
        kept.append(&mut repl.conv.pending);
        repl.conv.pending = kept;
        repl.handles
            .tr
            .notice(&format!("{n} attachment(s) kept for your next message."));
    }
    if persist {
        // The title pass keeps running: the message it names survives, and nothing waits on
        // it (`SessionSlot::abort_title`).
        persist_kept_turn(repl);
    } else {
        // A discarded turn gives its name back, so its pass could only land a name nobody keeps:
        // it goes, and its request with it. A later turn's discard leaves the first message —
        // and a pass still naming the session after it — alone.
        if repl.session.titler.unseed(&repl.conv.history) {
            repl.session.abort_title();
        }
    }
    rebudget_kept_turn(repl, watermark, persist);
    persist
}

/// Keeps what a stalled turn completed (`LlmError::StreamIdle`, the stream idle bound) — the
/// interrupt table's kept branches, under the error the caller already showed: the completed
/// tool rounds stay (their side effects happened, and a history without them would have the
/// model run them again), and the partial text lands as an assistant message marked cut short.
/// Returns `false`, touching nothing, when there is nothing to keep — the plain rollback then
/// runs. The stall stays an error (red block, `State::Error`, no retry); only the bookkeeping
/// follows ESC's — all of it: unanswered calls answered before the save, the rounds' measured
/// usage kept for a bot's threshold, and the caller counts the turn as landed.
fn keep_stalled_turn(
    repl: &mut Repl,
    watermark: usize,
    partial: &str,
    partial_reasoning: &str,
) -> bool {
    let InterruptDecision {
        history, persist, ..
    } = finalize_interrupt(
        repl.conv.history.clone(),
        watermark,
        partial,
        partial_reasoning,
    );
    if !persist {
        return false;
    }
    repl.conv.history = history;
    crate::session::repair_tail(&mut repl.conv.history);
    repl.handles
        .tr
        .notice("What arrived before the stall is kept — the reply may be incomplete.");
    persist_kept_turn(repl);
    rebudget_kept_turn(repl, watermark, true);
    true
}

/// Persists a turn cut short with something worth keeping. A cut-short call rarely reports
/// usage, but when it did (the figures arrived before the cut) the partial message carries them
/// like any other.
fn persist_kept_turn(repl: &mut Repl) {
    if let Some(last) = repl.conv.history.last_mut()
        && last.interrupted()
    {
        repl.conv.ctxm.record(Some(last));
    }
    repl.persist_turn();
}

/// Re-derives the budget and the status row from the history a cut-short turn left; `persist`
/// says whether the turn was kept, `watermark` where it starts.
fn rebudget_kept_turn(repl: &mut Repl, watermark: usize, persist: bool) {
    repl.conv.ctxm.reset();
    let history = std::mem::take(&mut repl.conv.history);
    // A kept turn keeps what its rounds measured: a bot's threshold check reads it (fable M2).
    if persist {
        repl.conv.budget.update_kept(&history, watermark);
    } else {
        repl.conv.budget.update(&history);
    }
    repl.conv.history = history;
    repl.push_status();
}
