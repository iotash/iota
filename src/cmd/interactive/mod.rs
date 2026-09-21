//! The interactive entry (cmd/root.go:270-414; `TUI_CONTRACTS` §11, `TUI_DESIGN` §8.4) — everything Go does
//! between "this run has no `-m`" and `chat.Run`.
//!
//! The order below is the ONLY legal one and the reason most of it lives inside [`open_ui`]: every step that
//! can fail, print, or claim the terminal must be finished before the event loop starts, because after
//! `Tui::start` nothing may write to the terminal except through the facade.
//!
//! 1. `--no-save` vs `iota resume` (root.go:284-286) — a pure argument error, so it precedes everything;
//! 2. the byte-exact non-TTY refusal (root.go:397-401) — a piped run has no interactive mode to offer, and
//!    hoisting it above every side effect keeps a doomed run from spawning servers or opening a picker;
//! 3. the session listing a bare `iota resume` needs, so an empty or unreadable store fails with nothing to
//!    clean up and the picker's spec is ready before any terminal work;
//! 4. the MCP background connect (root.go:270-281) — it overlaps the picker instead of blocking it, and from
//!    here on every exit path closes the manager (Go's `defer manager.Close()`);
//! 5. ONE OSC-11 background probe, on a blocking thread, BEFORE anything claims stdin;
//! 6. the `iota resume` picker (`chat.PickSession`), also on a blocking thread, its raw mode fully released
//!    before the session is resumed;
//! 7. the session wiring — resume / create / ephemeral factory, the context window, the dispatcher, the second
//!    provider instance the async title pass needs;
//! 8. the title-stack push, `Tui::start`, `crate::repl::run`, `ui.close()`, the title-stack pop, `manager.close()`.
//!
//! Signals: an interactive run does NOT keep the SIGINT→token handler ([`crate::cmd::signals::ignore_sigint`]) —
//! raw mode delivers Ctrl+C as a key event and the composer's cancel ladder answers it. SIGTERM still cancels
//! the root token, which fails every facade waiter and lets the interrupt table persist the turn.

mod picker;
mod title;

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use crate::BoxFuture;
use crate::app::env::Env;
use crate::host::{AnsiHost, Presenter};
use crate::provider::ProviderParams;
use crate::provider::{Provider, ProviderKind};
use crate::repl::{McpEvent, McpHooks, RunParams, SessionCtx};
use crate::session::{NewSession, SessionInfo, SessionStore, SessionWriter};
use crate::tool::DeferredGroup;
use crate::tool::{Dispatcher, ToolEnv};
use crate::ui::facade::{TabbedResult, TabbedSpec, Ui};

use crate::cmd::args::{Invocation, Resume};
use crate::cmd::error::{ArgsError, CliError, RunError, SetupError};
use crate::cmd::resolve::RunSettings;
use crate::cmd::{RunContext, ToolAssembly, host_probe};
use picker::{picker_spec, project_hint};

/// Everything `run` has resolved by the time it reaches Go's headless-vs-interactive branch (root.go:259):
/// one item per phase of `cmd::run`.
pub(crate) struct Interactive<'a> {
    /// The invocation (the interactive-only flags are read HERE, never headlessly).
    pub(crate) inv: &'a Invocation,
    /// The merged config (a resumed bundle's agent is looked up in it).
    pub(crate) cfg: &'a crate::config::Config,
    /// The resolved run settings.
    pub(crate) settings: RunSettings,
    /// The resolved provider type.
    pub(crate) kind: ProviderKind,
    /// The conversation provider, already tuned.
    pub(crate) provider: Box<dyn Provider>,
    /// The shared run environment.
    pub(crate) ctx: RunContext,
    /// The tool side.
    pub(crate) tools: ToolAssembly,
}

/// The three terminal-owning calls of the interactive entry, behind a seam.
///
/// It exists for ONE reason: the ordering law of `TUI_DESIGN` §8.4 — the picker's raw mode is fully released,
/// and every fallible pre-loop step is finished, BEFORE the event loop claims the terminal — is otherwise only
/// provable on a real tty. With the seam it is a unit test (`open_ui_releases_the_picker_before_tui_start`).
pub(crate) trait TerminalSeam: Send + Sync + 'static {
    /// The host probes (cmux, an RPC child under a deadline), then ONE OSC-11 round-trip — the
    /// latter blocking, so the implementation puts it on a blocking thread.
    fn detect_background(&self) -> BoxFuture<'_, bool>;
    /// The one-shot pre-REPL surface; blocking for as long as the user takes.
    fn run_surface(&self, spec: TabbedSpec, dark: bool) -> std::io::Result<TabbedResult>;
    /// Raw `\x1b[22;0t` on plain stdout.
    fn push_title_stack(&self);
    /// Raw `\x1b[23;0t` on plain stdout.
    fn pop_title_stack(&self);
    /// Starts the event loop and hands back the facade owner.
    fn start(&self, dark: bool) -> std::io::Result<Box<dyn UiSession>>;
}

/// The live UI, kept alive for the length of the chat.
pub(crate) trait UiSession: Send {
    /// The facade handle (an `Arc` clone per consumer).
    fn handle(&self) -> Arc<dyn Ui>;
}

/// The production seam: `iota_tui` itself, over the run's environment (the host probe reads it).
struct LiveTerminal {
    env: Env,
}

impl TerminalSeam for LiveTerminal {
    fn detect_background(&self) -> BoxFuture<'_, bool> {
        Box::pin(async {
            // The host probes first (a multiplexer that KNOWS its background), the terminal's own
            // OSC 11 answer as the fallback (internal/host/background.go:30-37) — a blocking tty
            // round-trip, so it runs on a blocking thread; a lost thread reads as dark.
            let osc = async {
                tokio::task::spawn_blocking(crate::ui::detect_background)
                    .await
                    .unwrap_or(true)
            };
            crate::host::detect_background(&host_probe(&self.env), osc).await
        })
    }

    fn run_surface(&self, spec: TabbedSpec, dark: bool) -> std::io::Result<TabbedResult> {
        crate::ui::run_surface(spec, dark)
    }

    fn push_title_stack(&self) {
        title::write_raw(title::TITLE_STACK_PUSH);
    }

    fn pop_title_stack(&self) {
        title::write_raw(title::TITLE_STACK_POP);
    }

    fn start(&self, dark: bool) -> std::io::Result<Box<dyn UiSession>> {
        crate::ui::Tui::start(crate::ui::TuiOptions { dark })
            .map(|tui| Box::new(LiveUi(tui)) as Box<dyn UiSession>)
    }
}

/// Owns the running `Tui` (dropping it is what joins the loop thread after `close()`).
struct LiveUi(crate::ui::Tui);

impl UiSession for LiveUi {
    fn handle(&self) -> Arc<dyn Ui> {
        self.0.handle()
    }
}

/// What [`open_ui`]'s continuation produced: everything `crate::repl::run` needs that only the binary can build.
struct Wiring {
    /// The live session writer (`None` = the chat started ephemeral).
    writer: Option<SessionWriter>,
    /// Mints the writer late (`/save`); `Some` = started ephemeral.
    new_session: Option<crate::repl::SessionFactory>,
    /// The resumed conversation, or empty.
    history: Vec<crate::provider::model::Message>,
    /// The four layered parameters the chat starts under, each with the source that put it there
    /// (root.go:365-380 for the window's half; brain page `model-param-layering` for the rest).
    params: crate::session::LayeredParams,
    /// What a `/model` switch re-evaluates those four against.
    layers: crate::config::ParamLayers,
    /// The live tool dispatcher.
    dispatch: Arc<dyn Dispatcher>,
    /// The SECOND provider instance the async title pass runs on (`None` for image providers).
    title_provider: Option<Box<dyn Provider>>,
}

/// `TUI_DESIGN` §8.4 steps 2, 3, 5 and 6, in one function so their order is a local invariant.
///
/// The OSC-11 probe and the picker are blocking raw-mode I/O and never run inline on a runtime worker; `wire`
/// then does every remaining fallible step (session resume/create, the context window, the dispatcher, the
/// title instance) while the terminal is still the shell's. Only when all of that has succeeded is the title
/// stack pushed and the event loop started — and a failing `start` pops the stack again, so a run that never
/// got a UI does not leave the tab title stacked.
async fn open_ui<T, F>(
    seam: &Arc<dyn TerminalSeam>,
    picker: Option<TabbedSpec>,
    wire: F,
) -> Result<(bool, T, Box<dyn UiSession>), CliError>
where
    F: FnOnce(bool, Option<usize>) -> Result<T, CliError>,
{
    let dark = seam.detect_background().await;

    let chosen = match picker {
        None => None,
        Some(spec) => {
            let surface = Arc::clone(seam);
            let result = tokio::task::spawn_blocking(move || surface.run_surface(spec, dark))
                .await
                .map_err(|e| RunError::Join(e.to_string()))??;
            if result.cancelled {
                None
            } else {
                result.panels.first().map(|p| p.cursor)
            }
        }
    };

    let wired = wire(dark, chosen)?;

    seam.push_title_stack();
    match seam.start(dark) {
        Ok(ui) => Ok((dark, wired, ui)),
        Err(e) => {
            seam.pop_title_stack();
            Err(RunError::Io(e).into())
        }
    }
}

/// The interactive branch, invoked at lib.rs's `settings.message.is_none()` arm (root.go:259).
pub(crate) async fn run_interactive(
    s: Interactive<'_>,
    io: &mut crate::cmd::io::Streams,
) -> Result<(), CliError> {
    let Interactive {
        inv,
        cfg,
        settings,
        kind,
        mut provider,
        ctx,
        tools,
    } = s;
    // What `/model` will offer. Built here, before anything claims the terminal: it needs the
    // config and the environment (the keys of the endpoints the candidate set names besides the one
    // this run talks to), which the loop deliberately knows nothing about, and constructing a
    // wildcard's endpoint is pure (the listings happen when the picker opens).
    let catalog = crate::repl::ModelCatalog::new(cfg, &settings.resolved, &ctx.env, &ctx.transport);
    let ToolAssembly {
        mcp_configs,
        mcp_defers,
        tool_env,
        interactor,
        jobs,
        agent,
        harness,
    } = tools;
    let interactor = interactor.unwrap_or_else(crate::repl::Interactor::new);
    // root.go:284-286 — a pure argument error, and Go raises it before the terminal check, so it still wins.
    if inv.args.no_save && inv.resume.is_some() {
        return Err(ArgsError::NoSaveWithResume.into());
    }
    // root.go:397-401. Hoisted above everything with a SIDE EFFECT, unlike Go — which starts the MCP servers,
    // opens the raw-mode session picker and creates a bundle before noticing that the run cannot proceed. The
    // refusal itself is byte-identical; what changes is that a piped run with a bad resume id now reports
    // the missing terminal rather than the missing session (DEVIATIONS3 `[WP51]`).
    if !std::io::stdout().is_terminal() {
        return Err(SetupError::NotATerminal.into());
    }
    // root.go:287-290: the config's `no_save:` starts ephemeral too, except an explicit resume outranks it.
    let ephemeral = inv.args.no_save || (settings.resolved.agent.no_save && inv.resume.is_none());

    // Raw mode owns Ctrl+C from the OSC-11 probe on (`TUI_DESIGN` §8.4 step 7).
    crate::cmd::signals::ignore_sigint();

    // root.go:292-334 (the picker half): the store is listed BEFORE anything is spawned, so an empty bucket or
    // an unreadable store fails with nothing to clean up, and the spec the picker will show is ready.
    let store = SessionStore::from_dirs(&ctx.env.dirs)?;
    let scope: Option<PathBuf> = settings.agent_mode.then(|| agent.root.clone());
    let resume_given = inv.resume.is_some();
    // `iota resume` with no id IS the picker; `iota resume <id>` resolves the fragment instead.
    let picker_rows: Vec<SessionInfo> = if inv.resume == Some(Resume::Pick) {
        store
            .list(scope.as_deref())
            .map_err(SetupError::ListSessions)?
    } else {
        Vec::new()
    };
    let picker = (!picker_rows.is_empty())
        .then(|| picker_spec(&picker_rows, project_hint(scope.as_deref()).as_deref()));

    // root.go:270-281: the servers start connecting NOW so the connect overlaps the (potentially slow) picker
    // and model selection instead of only starting once the loop is up. The child token means SIGTERM aborts
    // an in-flight connect; from here on EVERY exit path closes the manager, exactly like Go's deferred
    // `defer manager.Close()` / `defer cancelConnect()`.
    let connect_cancel = ctx.cancel.child_token();
    let (manager, mcp_part, mcp_events) = {
        let configured = !mcp_configs.is_empty();
        let manager = crate::mcp::Manager::new(
            mcp_configs,
            crate::mcp::ManagerOptions::new(ctx.http.clone(), ctx.env.clone()),
        );
        let events = configured.then(|| manager.connect_background(&connect_cancel));
        let part = configured.then(|| crate::cmd::assemble::McpPart::of(&manager));
        (manager, part, events)
    };

    let seam: Arc<dyn TerminalSeam> = Arc::new(LiveTerminal {
        env: ctx.env.clone(),
    });
    let opened = open_ui(&seam, picker, |_dark, picked| {
        wire_session(Wire {
            cfg,
            settings: &settings,
            kind,
            provider: &mut *provider,
            ctx: &ctx,
            tool_env: &tool_env,
            io,
            store: &store,
            scope: scope.as_deref(),
            resume_given,
            ephemeral,
            picked: picked.and_then(|i| picker_rows.get(i).map(|info| info.id.clone())),
            mcp_part,
            mcp_defers,
        })
    })
    .await;
    let (dark, wiring, ui_session) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            // Go's `defer manager.Close()`: a cancelled picker or a bad session id still hands the servers back.
            connect_cancel.cancel();
            manager.close().await;
            return Err(e);
        }
    };

    let ui = ui_session.handle();
    interactor.bind(Arc::clone(&ui));

    // chat/run.go:139 `host.NewPresenter(host.SystemEnv(), host.NewANSI(u), notify)`: the detected hosts
    // plus the ANSI fallback over the live facade; `notify:` defaults to on (config.go, T-14). Built inside
    // the runtime — a detected cmux host spawns its worker task.
    let notify = settings.resolved.agent.notify.unwrap_or(true);
    let pres = Arc::new(Presenter::new(
        &host_probe(&ctx.env),
        Some(Box::new(AnsiHost::new(Arc::clone(&ui)))),
        notify,
    ));
    // The harness prompt, now that the hosts it names in `<environment>` are known.
    let harness = harness.compose(&pres);

    let mcp = mcp_hooks(&manager, mcp_events);

    let outcome = crate::repl::run(RunParams {
        ui: Arc::clone(&ui),
        provider,
        title_provider: wiring.title_provider,
        // root.go:341 — trimmed once, here, exactly like Go.
        system: settings.system.trim().to_owned(),
        harness,
        imported_history: wiring.history,
        dispatch: Arc::clone(&wiring.dispatch),
        jobs,
        mcp,
        session: SessionCtx {
            writer: wiring.writer,
            store,
            new_session: wiring.new_session,
            scope,
        },
        params: wiring.params,
        layers: wiring.layers,
        catalog,
        agent,
        dark_background: dark,
        root_cancel: ctx.cancel.clone(),
        reqlog: Arc::clone(&ctx.reqlog),
        pres,
    })
    .await;

    // Teardown, in the pinned order (`TUI_DESIGN` §8.4 step 6): flush the staging tail and join the loop
    // thread, THEN hand the tab title back, THEN close the servers — each unconditional.
    let closed = ui.close().await;
    drop(ui_session);
    seam.pop_title_stack();
    connect_cancel.cancel();
    manager.close().await;

    match outcome {
        Err(e) => Err(repl_error(e)),
        // The loop exits cleanly even when the terminal died under it (every waiter fails `Closed`), so the
        // draw error the loop thread stored is the only witness left — Go had none to surface.
        Ok(()) => closed.map_err(|e| RunError::Io(e).into()),
    }
}

/// Everything [`wire_session`] reads: the resolved run, borrowed for the one call between the picker and
/// `Tui::start`, and what the picker stage decided — the twin of [`Interactive`] for that stage.
struct Wire<'a> {
    /// The merged config (a resumed bundle's agent is looked up in it).
    cfg: &'a crate::config::Config,
    /// The resolved run settings.
    settings: &'a RunSettings,
    /// The resolved provider type.
    kind: ProviderKind,
    /// The conversation provider: a resume replays the bundle's model and tuning onto it.
    provider: &'a mut dyn Provider,
    /// The run-wide context (the environment, the transport the title instance shares).
    ctx: &'a RunContext,
    /// The tool environment the dispatcher is built over.
    tool_env: &'a ToolEnv,
    /// The process streams: the resume announcement and the warnings.
    io: &'a mut crate::cmd::io::Streams,
    /// The session store.
    store: &'a SessionStore,
    /// The project root in agent mode (the bucket), else `None`.
    scope: Option<&'a std::path::Path>,
    /// The verb was `resume`, with or without an id.
    resume_given: bool,
    /// `--no-save`, or the config's `no_save:` without a resume.
    ephemeral: bool,
    /// The id the picker committed (`None` = no picker, empty store, or cancelled).
    picked: Option<String>,
    /// The MCP dispatcher part.
    mcp_part: Option<crate::cmd::assemble::McpPart>,
    /// Deferred MCP groups, consumed by the dispatcher.
    mcp_defers: Vec<DeferredGroup>,
}

/// root.go:292-412 — every fallible pre-loop step, run while the terminal is still the shell's.
///
/// Resume replays the bundle's model and tuning (explicit flags win, exactly as headlessly), a fresh run
/// creates its bundle eagerly, and `--no-save` gets the DEFERRED factory `/save` mints from. Then the context
/// window (flag > session meta > config), the dispatcher, and the second provider instance.
fn wire_session(wire: Wire<'_>) -> Result<Wiring, CliError> {
    let Wire {
        cfg,
        settings,
        kind,
        provider,
        ctx,
        tool_env,
        io,
        store,
        scope,
        resume_given,
        ephemeral,
        picked,
        mcp_part,
        mcp_defers,
    } = wire;
    let mut history = Vec::new();
    let mut writer: Option<SessionWriter> = None;
    // The bundle a resume replayed, kept for the layering: it is the record of what the session was running
    // under, and a resume RESTORES those values rather than evaluating the config again.
    let mut resumed_meta: Option<crate::session::SessionMeta> = None;

    if resume_given {
        // root.go:294-306: a bare `iota resume` took the picker; an id resolves as a prefix.
        let id = match &settings.resume {
            Some(fragment) => store.resolve_id(fragment, scope)?,
            None => picked.unwrap_or_default(),
        };
        if id.is_empty() {
            return Err(SetupError::NoSessionToResume.into());
        }
        let (w, resumed) = store.resume(&id, kind)?;
        // The bundle records the agent it ran under; one that has since been deleted is announced, and the
        // run falls back to the provider and model the meta carries (Phase 1b step 9).
        crate::session::warn_if_session_agent_is_gone(
            &resumed.meta,
            cfg.agents.contains_key(&resumed.meta.agent),
            &mut |w| io.warning(&w),
        );
        // root.go:317-331: `-M` is the only flag a session must not overwrite; temperature, effort and the
        // window always replay, because a resumed chat is the chat it resumes.
        crate::session::replay_session_settings(
            &resumed.meta,
            &mut *provider,
            kind,
            &crate::session::Overrides {
                model: !settings.model.is_empty(),
                ..crate::session::Overrides::default()
            },
            &mut |w| io.warning(&w),
        );
        resumed_meta = Some(resumed.meta);
        history = resumed.messages;
        // root.go:333, on plain stdout with the trailing blank Go prints — the last thing written before the
        // banner the facade puts in the scrollback.
        let _ = writeln!(
            io.stdout,
            "Resumed session {id} ({} messages)\n",
            history.len()
        );
        // The facade takes stdout a few steps from here; nothing may still be sitting in a buffer then.
        let _ = io.stdout.flush();
        writer = Some(w);
    }

    // root.go:342-351: agent-mode bundles land in the project's bucket keyed by its root; normal-mode ones
    // stay flat but still record where they started.
    // `scope` IS the project root in agent mode (the bucket is the project).
    let session_cwd = scope.map_or_else(
        || {
            ctx.env
                .dirs
                .cwd
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        |root| root.to_string_lossy().into_owned(),
    );
    if writer.is_none() && !ephemeral {
        writer = Some(
            store
                .create(NewSession {
                    temperature: settings.temperature,
                    base_url: settings.base_url.clone(),
                    cwd: session_cwd.clone(),
                    project: settings.agent_mode,
                    agent: settings.resolved.agent_name.clone(),
                    ..NewSession::new(kind, provider.model())
                })
                .map_err(SetupError::CreateSession)?,
        );
    }
    // root.go:353-363: ephemeral mode gets a DEFERRED factory instead — `/save` mints the bundle then, with
    // the tuning the chat is actually running under.
    let new_session: Option<crate::repl::SessionFactory> = if writer.is_none() {
        let store = store.clone();
        let model = provider.model().to_owned();
        // Go re-read the live provider at mint time; the provider is owned by the loop by then, so the
        // startup values seed the bundle and `iota_repl`'s `/save` re-stamps the LIVE model/temperature
        // onto the meta straight after minting it (chat/run.go:833-839 twin).
        let temperature = provider
            .as_tunable()
            .and_then(|t| t.temperature())
            .or(settings.temperature);
        let base_url = settings.base_url.clone();
        let project = settings.agent_mode;
        let agent_name = settings.resolved.agent_name.clone();
        Some(Box::new(move || {
            store.create(NewSession {
                temperature,
                base_url: base_url.clone(),
                cwd: session_cwd.clone(),
                project,
                agent: agent_name.clone(),
                ..NewSession::new(kind, &model)
            })
        }))
    } else {
        None
    };

    // root.go:365-385, widened to all four layered parameters: a resumed bundle's own values (with the
    // sources it recorded), else the two config layers, else the built-in defaults.
    let params = resolve_params(settings, provider, kind, resumed_meta.as_ref(), io)?;

    // root.go:390 + 588-592.
    let dispatch = crate::cmd::assemble::build_dispatcher(
        &settings.resolved.agent,
        &settings.resolved.model,
        mcp_part,
        mcp_defers,
        settings.agent_mode,
        tool_env,
        &mut |m| io.caution(&m),
    );
    // root.go:402-412: the async title pass runs while a turn is still streaming and provider instances keep
    // per-call state, so titles ride their own instance. Dedicated image providers get none — asked for a
    // title they would paint one.
    let title_provider = if provider.as_image_gen_tunable().is_some() {
        None
    } else {
        Some(crate::provider::new_provider(
            kind,
            ProviderParams {
                api_key: &settings.api_key,
                base_url: &settings.base_url,
                model: &settings.model,
                temperature: settings.temperature,
            },
            Some(ctx.transport.clone()),
        )?)
    };

    Ok(Wiring {
        writer,
        new_session,
        history,
        params,
        layers: crate::config::ParamLayers::new(cfg, &settings.resolved),
        dispatch,
        title_provider,
    })
}

/// root.go:365-385, generalised to the four layered parameters (brain page `model-param-layering`): a
/// resumed bundle's recorded values come back as they were recorded, everything else is evaluated
/// `agents:` → `models:` → the built-in default. Plus Go's warning for a window on a provider that cannot
/// count tokens at all.
///
/// This is the ONE evaluating moment on the way in; the other is a `/model` model switch. The
/// `--context-window` flag that used to precede the config is gone: the window is a property of the model,
/// and `/model`'s Context tab is where one run changes it.
///
/// A `context_window:` that does not parse aborts the run here — at startup there is nothing yet to keep
/// going with, and the label says which layer wrote it.
fn resolve_params(
    settings: &RunSettings,
    provider: &dyn Provider,
    kind: ProviderKind,
    resumed: Option<&crate::session::SessionMeta>,
    io: &mut crate::cmd::io::Streams,
) -> Result<crate::session::LayeredParams, CliError> {
    let window = match settings.resolved.window_decl() {
        None => None,
        Some(decl) => Some(crate::config::window::parse_window_size(decl.raw).map_err(
            |source| SetupError::ContextWindow {
                label: decl.label.to_owned(),
                source,
            },
        )?),
    };
    let declared = settings.resolved.declared(window);
    let params = match resumed {
        // `apply_session_tuning` replays nothing for a bundle recorded under another provider type, so for
        // this run that bundle recorded nothing either.
        Some(meta) => declared.resume(meta, meta.provider == kind.as_str()),
        None => declared.evaluate(&crate::session::LayeredParams::default()),
    };
    if params.context_window.value > 0 && !provider.reports_usage() {
        io.warning(&format!(
            "Warning: context window does not apply to provider type {} (no token accounting)",
            provider.kind().as_str()
        ));
    }
    Ok(params)
}

/// `crate::repl::ReplError` → the process's exit mapping. A facade failure has no variant of its own, so it
/// travels as its text (`ui: closed` / `ui: interrupted`).
fn repl_error(e: crate::repl::ReplError) -> CliError {
    match e {
        crate::repl::ReplError::Session(e) => SetupError::Session(e).into(),
        crate::repl::ReplError::Io(e) => RunError::Io(e).into(),
        crate::repl::ReplError::Ui(e) => RunError::Ui(e).into(),
    }
}

/// The MCP display hooks: `/tools` and `/status` re-read the manager's live status snapshot through a
/// closure, as data — the rendering lives with the commands.
fn mcp_hooks(
    manager: &Arc<crate::mcp::Manager>,
    events: Option<tokio::sync::mpsc::Receiver<crate::mcp::ServerStatus>>,
) -> McpHooks {
    let snapshot = Arc::clone(manager);
    let servers = Arc::new(move || snapshot.servers())
        as Arc<dyn Fn() -> Vec<crate::mcp::ServerStatus> + Send + Sync>;
    McpHooks {
        servers: Some(servers),
        events: events.map(map_events),
        manager: Some(Arc::clone(manager)),
    }
}

/// `ServerStatus` → `McpEvent` (chat/run.go:1159-1177): a connected server carries `None` for the error and
/// whatever non-fatal warnings its merge produced (X-29). The mapping runs in its own task so the receiver
/// handed to `RunParams` is the `McpEvent` channel the reporter expects.
fn map_events(
    mut statuses: tokio::sync::mpsc::Receiver<crate::mcp::ServerStatus>,
) -> tokio::sync::mpsc::Receiver<McpEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        while let Some(st) = statuses.recv().await {
            let event = McpEvent {
                warnings: st.warnings(),
                error: st.error().map(str::to_owned),
                name: st.name,
            };
            if tx.send(event).await.is_err() {
                return;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use crate::sync::lock;
    use std::sync::{Arc, Mutex};

    use crate::BoxFuture;

    use crate::testing::ScriptedUi;
    use crate::ui::facade::{PanelResult, TabbedResult, TabbedSpec, Ui};
    use pretty_assertions::assert_eq;

    use super::picker::{info, picker_spec};
    use super::{ArgsError, CliError, SetupError, TerminalSeam, UiSession, open_ui};

    /// A `TerminalSeam` that records the ORDER of the terminal-owning calls instead of making them.
    struct FakeTerminal {
        log: Arc<Mutex<Vec<String>>>,
        /// What `run_surface` commits (`None` = the run never opens one).
        surface: Mutex<Option<TabbedResult>>,
        /// Whether `Tui::start` fails (no terminal left to take).
        start_fails: bool,
    }

    impl FakeTerminal {
        fn new(log: &Arc<Mutex<Vec<String>>>, surface: Option<TabbedResult>) -> Arc<Self> {
            Arc::new(Self {
                log: Arc::clone(log),
                surface: Mutex::new(surface),
                start_fails: false,
            })
        }

        fn push(&self, event: impl Into<String>) {
            lock(&self.log).push(event.into());
        }
    }

    /// The facade double the fake `start` hands back.
    struct FakeUi(Arc<ScriptedUi>);

    impl UiSession for FakeUi {
        fn handle(&self) -> Arc<dyn Ui> {
            Arc::clone(&self.0) as Arc<dyn Ui>
        }
    }

    impl TerminalSeam for FakeTerminal {
        fn detect_background(&self) -> BoxFuture<'_, bool> {
            self.push("detect");
            Box::pin(std::future::ready(false))
        }

        fn run_surface(&self, spec: TabbedSpec, _dark: bool) -> std::io::Result<TabbedResult> {
            let title = spec
                .panels
                .first()
                .map_or(String::new(), |p| p.title.clone());
            self.push(format!("surface:open:{title}"));
            let committed = lock(&self.surface).take().unwrap_or_default();
            // The live seam's `RawGuard` restores the terminal on the way out of `run_surface`; the double
            // records that release at the same point, so the ordering assertion is about the real boundary.
            self.push("surface:released");
            Ok(committed)
        }

        fn push_title_stack(&self) {
            self.push("title:push");
        }

        fn pop_title_stack(&self) {
            self.push("title:pop");
        }

        fn start(&self, dark: bool) -> std::io::Result<Box<dyn UiSession>> {
            self.push(format!("tui:start:dark={dark}"));
            if self.start_fails {
                return Err(std::io::Error::other("no terminal"));
            }
            Ok(Box::new(FakeUi(ScriptedUi::new(Vec::new()))))
        }
    }

    fn events(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        lock(log).clone()
    }

    fn commit(cursor: usize) -> TabbedResult {
        TabbedResult {
            cancelled: false,
            focused: 0,
            panels: vec![PanelResult {
                cursor,
                ..PanelResult::default()
            }],
        }
    }

    /// The ordering law of `TUI_DESIGN` §8.4: the OSC-11 probe runs first, the picker's raw mode is RELEASED,
    /// and every fallible pre-loop step has finished, before the title stack is pushed and the event loop
    /// claims the terminal. `wire` sees the committed row.
    #[tokio::test]
    async fn open_ui_releases_the_picker_before_tui_start() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let seam: Arc<dyn TerminalSeam> = FakeTerminal::new(&log, Some(commit(2)));
        let rows = [info("aaa", "a"), info("bbb", "b"), info("ccc", "c")];

        let (dark, chosen, _ui) = open_ui(&seam, Some(picker_spec(&rows, None)), |_dark, row| {
            lock(&log).push("wire".to_owned());
            Ok(row.and_then(|i| rows.get(i).map(|s| s.id.clone())))
        })
        .await
        .expect("open_ui");

        assert!(!dark, "the probe's answer reaches the caller");
        assert_eq!(
            chosen.as_deref(),
            Some("ccc"),
            "the committed row is the id"
        );
        assert_eq!(
            events(&log),
            vec![
                "detect",
                "surface:open:Select a session to resume",
                "surface:released",
                "wire",
                "title:push",
                "tui:start:dark=false",
            ]
        );
    }

    /// No resume: nothing opens a surface, and the probe still precedes the loop.
    #[tokio::test]
    async fn open_ui_without_a_picker_never_opens_a_surface() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let seam: Arc<dyn TerminalSeam> = FakeTerminal::new(&log, None);
        let (_dark, row, _ui) = open_ui(&seam, None, |_dark, row| Ok(row))
            .await
            .expect("open_ui");
        assert_eq!(row, None);
        assert_eq!(
            events(&log),
            vec!["detect", "title:push", "tui:start:dark=false"]
        );
    }

    /// A cancelled picker commits nothing, and a `wire` that then refuses ("no session to resume") must leave
    /// the terminal alone: no title stack, no event loop.
    #[tokio::test]
    async fn a_failed_wiring_never_starts_the_loop() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let seam: Arc<dyn TerminalSeam> = FakeTerminal::new(
            &log,
            Some(TabbedResult {
                cancelled: true,
                ..TabbedResult::default()
            }),
        );
        let rows = [info("aaa", "a")];
        let outcome = open_ui(&seam, Some(picker_spec(&rows, None)), |_dark, row| {
            assert_eq!(row, None, "a cancelled surface commits nothing");
            Err::<(), _>(SetupError::NoSessionToResume.into())
        })
        .await;
        assert!(matches!(
            outcome,
            Err(CliError::Setup(SetupError::NoSessionToResume))
        ));
        assert_eq!(
            outcome.err().map(|e| e.to_string()),
            Some("no session to resume".to_owned())
        );
        assert_eq!(
            events(&log),
            vec![
                "detect",
                "surface:open:Select a session to resume",
                "surface:released",
            ],
            "the title stack is untouched and the loop never starts"
        );
    }

    /// `Tui::start` failing after the push pops the stack again — a run that never got a UI must not leave the
    /// tab title stacked.
    #[tokio::test]
    async fn a_failed_start_pops_the_title_stack() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let seam: Arc<dyn TerminalSeam> = Arc::new(FakeTerminal {
            log: Arc::clone(&log),
            surface: Mutex::new(None),
            start_fails: true,
        });
        let outcome = open_ui(&seam, None, |_dark, _row| Ok(())).await;
        assert!(outcome.is_err());
        assert_eq!(
            events(&log),
            vec!["detect", "title:push", "tui:start:dark=false", "title:pop"]
        );
    }

    /// The non-TTY refusal and the two combination errors are the Go texts, byte for byte (cmd/root.go:285,
    /// :314, :400). `main` prints them as `Error: {msg}` with exit 1.
    #[test]
    fn refusal_texts_are_byte_exact() {
        assert_eq!(
            SetupError::NotATerminal.to_string(),
            "interactive mode requires a terminal; use -m/--message for piped input"
        );
        assert_eq!(
            ArgsError::NoSaveWithResume.to_string(),
            "--no-save cannot be combined with iota resume"
        );
        assert_eq!(
            SetupError::NoSessionToResume.to_string(),
            "no session to resume"
        );
    }
}
