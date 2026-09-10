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

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use crate::host::{AnsiHost, Env as HostEnv, Presenter};
use crate::provider::ProviderParams;
use crate::provider::{Provider, ProviderKind};
use crate::repl::{McpEvent, McpHooks, RunParams, SessionCtx, session_label};
use crate::session::{SessionInfo, SessionStore, SessionWriter};
use crate::tool::DeferredGroup;
use crate::tool::{Dispatcher, Env};
use crate::ui::facade::{Panel, TabbedResult, TabbedSpec, Ui};

use crate::cmd::cli::{Invocation, Resume};
use crate::cmd::resolve::{CliError, RunSettings};
use crate::cmd::{RunContext, ToolAssembly};

/// chat/session.go:1091 — the `iota resume` picker's only panel.
const PICK_SESSION_TITLE: &str = "Select a session to resume";

/// chat/session.go:1092 — the picker shows 15 rows (`PANEL_HEIGHT` View default).
const PICK_SESSION_HEIGHT: usize = 15;

/// chat/run.go:122 — push the terminal's title stack, on plain stdout, before the event loop.
const TITLE_STACK_PUSH: &str = "\x1b[22;0t";

/// chat/run.go:123 — pop it, deferred until after the facade has released the terminal.
const TITLE_STACK_POP: &str = "\x1b[23;0t";

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
    /// ONE OSC-11 round-trip; blocking, so it runs on a blocking thread.
    fn detect_background(&self) -> bool;
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

/// The production seam: `iota_tui` itself.
struct LiveTerminal;

impl TerminalSeam for LiveTerminal {
    fn detect_background(&self) -> bool {
        // The host probes first (a multiplexer that KNOWS its background), the terminal's own
        // OSC 11 answer as the fallback (internal/host/background.go:30-37).
        crate::host::detect_background(&host_env(), crate::ui::detect_background)
    }

    fn run_surface(&self, spec: TabbedSpec, dark: bool) -> std::io::Result<TabbedResult> {
        crate::ui::run_surface(spec, dark)
    }

    fn push_title_stack(&self) {
        write_raw(TITLE_STACK_PUSH);
    }

    fn pop_title_stack(&self) {
        write_raw(TITLE_STACK_POP);
    }

    fn start(&self, dark: bool) -> std::io::Result<Box<dyn UiSession>> {
        crate::ui::Tui::start(crate::ui::TuiOptions { dark })
            .map(|tui| Box::new(LiveUi(tui)) as Box<dyn UiSession>)
    }
}

/// The host detectors' view of the process environment (host.go:71-74 `SystemEnv`): `getenv` and the
/// `PATH` lookup, built HERE so `crate::host` never reads the environment itself (G16: the 15-line PATH
/// scan of `shell::exec` stands in for `exec.LookPath`).
fn host_env() -> HostEnv {
    HostEnv {
        getenv: Box::new(|name| std::env::var(name).unwrap_or_default()),
        look_path: Box::new(crate::shell::exec::find_in_path),
    }
}

/// The title-stack OSCs go to plain stdout: they are emitted before the facade exists and after it is gone.
fn write_raw(seq: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
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
    /// Flag > session meta > config > 0 (root.go:365-380).
    context_window: u64,
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
    let probe = Arc::clone(seam);
    let dark = tokio::task::spawn_blocking(move || probe.detect_background())
        .await
        .unwrap_or(true);

    let chosen = match picker {
        None => None,
        Some(spec) => {
            let surface = Arc::clone(seam);
            let result = tokio::task::spawn_blocking(move || surface.run_surface(spec, dark))
                .await
                .map_err(|e| CliError::Join(e.to_string()))??;
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
            Err(CliError::Io(e))
        }
    }
}

/// The `iota resume` picker's spec (chat/session.go:1091-1094): ONE searchable list of `session_label` rows.
fn picker_spec(rows: &[SessionInfo], project: Option<&str>) -> TabbedSpec {
    TabbedSpec {
        panels: vec![
            Panel::list(
                PICK_SESSION_TITLE.to_owned(),
                rows.iter().map(|s| session_label(s, project)).collect(),
            )
            .with_search(true)
            .with_height(PICK_SESSION_HEIGHT),
        ],
        ..TabbedSpec::default()
    }
}

/// The `[project]` hint every row of a SCOPED listing shares (chat/session.go:1033-1040 `projectHint`; the
/// bucket IS the project — DEVIATIONS3 `[WP50]`).
fn project_hint(scope: Option<&std::path::Path>) -> Option<String> {
    scope.map(|root| {
        root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    })
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
    let ToolAssembly {
        mcp_configs,
        mcp_defers,
        tool_env,
        interactor,
        jobs,
        agent,
    } = tools;
    let interactor = interactor.unwrap_or_else(crate::repl::Interactor::new);
    // root.go:284-286 — a pure argument error, and Go raises it before the terminal check, so it still wins.
    if inv.args.no_save && inv.resume.is_some() {
        return Err(CliError::NoSaveWithResume);
    }
    // root.go:397-401. Hoisted above everything with a SIDE EFFECT, unlike Go — which starts the MCP servers,
    // opens the raw-mode session picker and creates a bundle before noticing that the run cannot proceed. The
    // refusal itself is byte-identical; what changes is that a piped run with a bad resume id now reports
    // the missing terminal rather than the missing session (DEVIATIONS3 `[WP51]`).
    if !std::io::stdout().is_terminal() {
        return Err(CliError::NotATerminal);
    }
    // root.go:287-290: the config's `no_save:` starts ephemeral too, except an explicit resume outranks it.
    let ephemeral = inv.args.no_save || (settings.resolved.agent.no_save && inv.resume.is_none());

    // Raw mode owns Ctrl+C from the OSC-11 probe on (`TUI_DESIGN` §8.4 step 7).
    crate::cmd::signals::ignore_sigint();

    // root.go:292-334 (the picker half): the store is listed BEFORE anything is spawned, so an empty bucket or
    // an unreadable store fails with nothing to clean up, and the spec the picker will show is ready.
    let store = SessionStore::from_dirs(&ctx.dirs)?;
    let scope: Option<PathBuf> = settings.agent_mode.then(|| agent.root.clone());
    let resume_given = inv.resume.is_some();
    // `iota resume` with no id IS the picker; `iota resume <id>` resolves the fragment instead.
    let picker_rows: Vec<SessionInfo> = if inv.resume == Some(Resume::Pick) {
        store
            .list(scope.as_deref())
            .map_err(CliError::ListSessions)?
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
            crate::mcp::ManagerOptions::new(ctx.http.clone(), Arc::clone(&ctx.resolver)),
        );
        let events = configured.then(|| manager.connect_background(&connect_cancel));
        let part = configured.then(|| crate::cmd::assemble::McpPart::of(&manager));
        (manager, part, events)
    };

    let seam: Arc<dyn TerminalSeam> = Arc::new(LiveTerminal);
    let opened = open_ui(&seam, picker, |_dark, picked| {
        wire_session(
            cfg,
            &settings,
            kind,
            &mut *provider,
            &ctx,
            &tool_env,
            io,
            &store,
            scope.as_deref(),
            WireInput {
                resume_given,
                ephemeral,
                picked: picked.and_then(|i| picker_rows.get(i).map(|info| info.id.clone())),
                mcp_part,
                mcp_defers,
            },
        )
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
        &host_env(),
        Some(Box::new(AnsiHost::new(Arc::clone(&ui)))),
        notify,
    ));

    let mcp = mcp_hooks(&manager, mcp_events);

    let outcome = crate::repl::run(RunParams {
        ui: Arc::clone(&ui),
        provider,
        title_provider: wiring.title_provider,
        // root.go:341 — trimmed once, here, exactly like Go.
        system: settings.system.trim().to_owned(),
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
        context_window: wiring.context_window,
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
        Ok(()) => closed.map_err(CliError::Io),
    }
}

/// What [`wire_session`] needs from the picker stage.
struct WireInput {
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
#[allow(clippy::too_many_arguments)]
fn wire_session(
    cfg: &crate::config::Config,
    settings: &RunSettings,
    kind: ProviderKind,
    provider: &mut dyn Provider,
    ctx: &RunContext,
    tool_env: &Env,
    io: &mut crate::cmd::io::Streams,
    store: &SessionStore,
    scope: Option<&std::path::Path>,
    input: WireInput,
) -> Result<Wiring, CliError> {
    let mut history = Vec::new();
    let mut writer: Option<SessionWriter> = None;
    let mut session_window: Option<u64> = None;

    if input.resume_given {
        // root.go:294-306: a bare `iota resume` took the picker; an id resolves as a prefix.
        let id = match &settings.resume {
            Some(fragment) => store.resolve_id(fragment, scope)?,
            None => input.picked.unwrap_or_default(),
        };
        if id.is_empty() {
            return Err(CliError::NoSessionToResume);
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
        session_window = crate::session::replay_session_settings(
            &resumed.meta,
            &mut *provider,
            kind,
            &crate::session::Overrides {
                model: !settings.model.is_empty(),
                ..crate::session::Overrides::default()
            },
            &mut |w| io.warning(&w),
        );
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
            ctx.dirs
                .cwd
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        |root| root.to_string_lossy().into_owned(),
    );
    if writer.is_none() && !input.ephemeral {
        writer = Some(
            store
                .create(
                    kind,
                    provider.model(),
                    settings.temperature,
                    &settings.base_url,
                    &session_cwd,
                    settings.agent_mode,
                    &settings.resolved.agent_name,
                )
                .map_err(CliError::CreateSession)?,
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
            store.create(
                kind,
                &model,
                temperature,
                &base_url,
                &session_cwd,
                project,
                &agent_name,
            )
        }))
    } else {
        None
    };

    // root.go:365-385: session meta > config > 0 (the chat default).
    let context_window = resolve_context_window(settings, provider, session_window, io)?;

    // root.go:390 + 588-592.
    let dispatch = crate::cmd::assemble::build_dispatcher(
        &settings.resolved.agent,
        &settings.resolved.model,
        input.mcp_part,
        input.mcp_defers,
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
        context_window,
        dispatch,
        title_provider,
    })
}

/// root.go:365-385 — the resumed bundle's meta > `models.<name>.context_window` > 0 (the chat's own 128k
/// default), plus Go's warning for a provider that cannot count tokens at all. The `--context-window` flag
/// that used to precede both is gone: the window is a property of the model, and `/model`'s Context tab is
/// where one run changes it.
fn resolve_context_window(
    settings: &RunSettings,
    provider: &dyn Provider,
    session_window: Option<u64>,
    io: &mut crate::cmd::io::Streams,
) -> Result<u64, CliError> {
    let parse = |raw: &str, label: &str| -> Result<u64, CliError> {
        crate::cmd::window::parse_window_size(raw).map_err(|source| CliError::ContextWindow {
            label: label.to_owned(),
            source,
        })
    };
    let window = if let Some(w) = session_window.filter(|w| *w > 0) {
        w
    } else if settings.resolved.model.context_window.is_empty() {
        0
    } else {
        parse(
            &settings.resolved.model.context_window,
            "config context_window",
        )?
    };
    if window > 0 && !provider.reports_usage() {
        io.warning(&format!(
            "Warning: context window does not apply to provider type {} (no token accounting)",
            provider.kind().as_str()
        ));
    }
    Ok(window)
}

/// `crate::repl::ReplError` → the process's exit mapping. A facade failure has no `CliError` of its own, so it
/// travels as its text (`ui: closed` / `ui: interrupted`).
fn repl_error(e: crate::repl::ReplError) -> CliError {
    match e {
        crate::repl::ReplError::Session(e) => CliError::Session(e),
        crate::repl::ReplError::Io(e) => CliError::Io(e),
        crate::repl::ReplError::Ui(e) => CliError::Ui(e),
    }
}

/// The MCP display hooks: `/tools` and `/status` re-read the manager's live status snapshot through a
/// closure, as data — the rendering lives with the commands.
fn mcp_hooks(
    manager: &Arc<crate::mcp::Manager>,
    events: Option<tokio::sync::mpsc::Receiver<crate::mcp::ServerStatus>>,
) -> McpHooks {
    let manager = Arc::clone(manager);
    let servers = Arc::new(move || manager.servers())
        as Arc<dyn Fn() -> Vec<crate::mcp::ServerStatus> + Send + Sync>;
    McpHooks {
        servers: Some(servers),
        events: events.map(map_events),
    }
}

/// `ServerStatus` → `McpEvent` (chat/run.go:1159-1177): the loop only ever reports failures, so a connected
/// server carries `None`. The mapping runs in its own task so the receiver handed to `RunParams` is the
/// `McpEvent` channel the reporter expects.
fn map_events(
    mut statuses: tokio::sync::mpsc::Receiver<crate::mcp::ServerStatus>,
) -> tokio::sync::mpsc::Receiver<McpEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        while let Some(st) = statuses.recv().await {
            let event = McpEvent {
                name: st.name,
                error: st.err.filter(|_| !st.connected),
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
    use std::sync::{Arc, Mutex, PoisonError};

    use crate::session::SessionInfo;
    use crate::testing::ScriptedUi;
    use crate::ui::facade::{PanelKind, PanelResult, TabbedResult, TabbedSpec, Ui};
    use pretty_assertions::assert_eq;

    use super::{
        CliError, PICK_SESSION_HEIGHT, PICK_SESSION_TITLE, TerminalSeam, UiSession, open_ui,
        picker_spec, project_hint,
    };

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
            self.log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.into());
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
        fn detect_background(&self) -> bool {
            self.push("detect");
            false
        }

        fn run_surface(&self, spec: TabbedSpec, _dark: bool) -> std::io::Result<TabbedResult> {
            let title = spec
                .panels
                .first()
                .map_or(String::new(), |p| p.title.clone());
            self.push(format!("surface:open:{title}"));
            let committed = self
                .surface
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .unwrap_or_default();
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
        log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn info(id: &str, title: &str) -> SessionInfo {
        SessionInfo {
            id: id.to_owned(),
            title: title.to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            updated_at: None,
            message_count: 4,
        }
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

    /// The `iota resume` picker is `chat.PickSession` byte for byte: ONE searchable list panel titled
    /// "Select a session to resume", 15 rows high, whose items are the `session_label` rows of the LISTING
    /// (chat/session.go:1091-1094).
    #[test]
    fn picker_spec_matches_pick_session() {
        let rows = [info("aaa", "first chat"), info("bbb", "")];
        let spec = picker_spec(&rows, Some("proj"));
        assert_eq!(spec.panels.len(), 1);
        assert!(!spec.enter_advances);
        assert_eq!(spec.refresh_every_ms, 0);
        let panel = &spec.panels[0];
        assert_eq!(panel.title, PICK_SESSION_TITLE);
        assert_eq!(panel.title, "Select a session to resume");
        assert_eq!(panel.kind(), PanelKind::List);
        assert_eq!(panel.height, PICK_SESSION_HEIGHT);
        assert!(panel.search);
        assert_eq!(
            panel.items(),
            vec![
                "first chat · gpt-4o · unknown · 4 msgs [proj]".to_owned(),
                "(untitled) · gpt-4o · unknown · 4 msgs [proj]".to_owned(),
            ]
        );
        // An unscoped listing carries no hint at all.
        assert_eq!(
            picker_spec(&rows, None).panels[0].items()[0],
            "first chat · gpt-4o · unknown · 4 msgs"
        );
    }

    #[test]
    fn project_hint_is_the_bucket_name() {
        assert_eq!(
            project_hint(Some(std::path::Path::new("/work/my-app"))),
            Some("my-app".to_owned())
        );
        assert_eq!(project_hint(None), None);
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
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("wire".to_owned());
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
            Err::<(), _>(CliError::NoSessionToResume)
        })
        .await;
        assert!(matches!(outcome, Err(CliError::NoSessionToResume)));
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
            CliError::NotATerminal.to_string(),
            "interactive mode requires a terminal; use -m/--message for piped input"
        );
        assert_eq!(
            CliError::NoSaveWithResume.to_string(),
            "--no-save cannot be combined with iota resume"
        );
        assert_eq!(
            CliError::NoSessionToResume.to_string(),
            "no session to resume"
        );
        assert_eq!(super::TITLE_STACK_PUSH, "\u{1b}[22;0t");
        assert_eq!(super::TITLE_STACK_POP, "\u{1b}[23;0t");
    }
}
