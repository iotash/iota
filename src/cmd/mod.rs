//! The command (cmd/root.go): the clap `Cli` verb set, pure run resolution, tuning warnings,
//! MCP/dispatcher assembly, the listings, the config command, the interactive branch, and [`run`], which
//! `main.rs` awaits via `block_on` and maps to an exit code. The YAML config model is `crate::config`. ONE
//! binary carries everything, exactly like the Go binary (decision of 2026-09-01; ARCHITECTURE §11).

pub(crate) mod assemble;
pub(crate) mod cli;
pub(crate) mod config_cmd;
pub(crate) mod interactive;
pub mod io;
pub mod list;
pub(crate) mod resolve;
pub mod signals;
pub(crate) mod tuning;
pub mod window;

pub use crate::config::{
    AgentConfig, BadModelRef, Config, ConfigError, DEFAULT_AGENT, Declared, McpServerConfig,
    ModelConfig, ModelEntry, ModelRef, ParamLayers, ProviderConfig, Resolved, WindowDecl,
};
pub use cli::{Cli, Command, ConfigAction, Invocation, ListWhat, Resume, RunArgs};
pub use resolve::CliError;
pub use resolve::{RunSettings, resolve_run};

/// The version `--version` and `iota version` print.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use std::{path::PathBuf, sync::Arc};

use crate::app::HostDirs;
use crate::chat::{AgentOptions, OnceOptions, OutputFormat};
use crate::llm::reqlog::RequestLog;
use crate::mcp::config::ServerConfig;
use crate::provider::ProviderKind;
use crate::provider::{HttpTransport, ProviderParams};
use crate::tool::{DeferredGroup, Env};
use crate::vars::{EnvSource, VarResolver};
use tokio_util::sync::CancellationToken;

/// The process-wide, read-only run environment both branches share (root.go:125-131): ONE HTTP client for
/// the whole run — the provider and the MCP streamable-HTTP transports — and ONE `/debug` request log
/// (root.go:128 `reqLog.HTTPClient()`) that the provider and the title instance record into. The MCP manager
/// keeps the bare client: Go never records MCP traffic.
#[derive(Clone)]
pub(crate) struct RunContext {
    /// Process directories (the session store's root, the images dir).
    pub(crate) dirs: HostDirs,
    /// The bare client (the MCP transports take it so).
    pub(crate) http: reqwest::Client,
    /// The same client with the run's `/debug` recorder.
    pub(crate) transport: HttpTransport,
    /// The `/debug` request log the loop toggles and browses.
    pub(crate) reqlog: Arc<RequestLog>,
    /// `${…}` resolver for the MCP configs — the manager expands every server config through it.
    pub(crate) resolver: Arc<dyn VarResolver>,
    /// The run's root cancellation (the SIGTERM path).
    pub(crate) cancel: CancellationToken,
}

impl RunContext {
    fn new(dirs: HostDirs, cancel: CancellationToken, resolver: Arc<dyn VarResolver>) -> Self {
        let http = crate::llm::default_http_client();
        let reqlog = Arc::new(RequestLog::new());
        let transport = HttpTransport {
            client: http.clone(),
            recorder: Some(Arc::clone(&reqlog)),
        };
        Self {
            dirs,
            http,
            transport,
            reqlog,
            resolver,
            cancel,
        }
    }
}

/// root.go:187-241 — the tool side, assembled once and consumed by whichever branch runs.
pub(crate) struct ToolAssembly {
    /// MCP server configs in config order.
    pub(crate) mcp_configs: Vec<ServerConfig>,
    /// Deferred MCP groups.
    pub(crate) mcp_defers: Vec<DeferredGroup>,
    /// The tool environment (its `interactor` is bound to the live facade by the interactive branch).
    pub(crate) tool_env: Env,
    /// The ask-seam bridge, created UNBOUND (the dispatcher is built long before the UI exists); `None`
    /// headlessly, so `new_ask_set` contributes no tools and the model never sees them (root.go:221-232).
    pub(crate) interactor: Option<Arc<crate::repl::Interactor>>,
    /// The run's background-job registry, also held by `tool_env` — the `shell` tool starts jobs through it,
    /// the branch that takes it over delivers their notices and kills what is left on the way out.
    pub(crate) jobs: Arc<crate::shell::jobs::Jobs>,
    /// Agent-mode options.
    pub(crate) agent: AgentOptions,
}

/// Process-level outcome mapping (main.rs): Ok → 0; `Err(CliError::Interrupted)` → 130; other Err → `Error: {e}` on
/// stderr, 1. Clap parse errors are handled by clap (`Error::exit`, code 2) before `run` is called.
/// Awaited ONLY via `rt.block_on(run(..))` in main.rs — it borrows `io` and is never `tokio::spawn`ed.
///
/// The verb decides everything: `version` answers without reading a file, `list` and `config` read the config
/// and stop, and `run`/`resume` are the same pipeline (`run_agent`) with and without a session to start
/// from.
pub async fn run(
    cli: Cli,
    dirs: HostDirs,
    env: Arc<dyn EnvSource>,
    cancel: CancellationToken,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    use std::io::Write as _;

    let resolver: Arc<dyn VarResolver> = Arc::new(EnvResolver {
        env: Arc::clone(&env),
        dirs: dirs.clone(),
    });
    let (command, config) = cli.into_command();
    match command {
        Command::Version => {
            writeln!(io.stdout, "iota {VERSION}")?;
            Ok(())
        }
        Command::Config(cmd) => {
            config_cmd::run_config(&cmd, config.as_deref(), &dirs, resolver.as_ref(), io)
        }
        Command::List(cmd) => {
            let cfg = Config::load(config.as_deref(), &dirs, resolver.as_ref(), &mut |w| {
                io.warning(&w);
            })?;
            list::run_list(&cmd, &cfg, &dirs, env.as_ref(), io)
        }
        Command::Run(cmd) => {
            let inv = Invocation::of_run(cmd, config);
            run_agent(inv, dirs, env, resolver, cancel, io).await
        }
        Command::Resume(cmd) => {
            let inv = Invocation::of_resume(cmd, config);
            run_agent(inv, dirs, env, resolver, cancel, io).await
        }
    }
}

/// The run pipeline, in Go's order (cmd/root.go:46-268, kept EXACTLY so every byte-pinned error Go raises
/// before it decides headless-vs-interactive still wins over the interactive branch's own refusals):
/// `RunArgs::reject_unsupported` (`-m` runs only) → `Config::load` → `resolve_run` → provider construction
/// (`ProviderKind::from_str`, `new_provider`) →
/// `tuning::apply` → `assemble::build_mcp_configs` → `tuning::warn_tools_without_calling` → cwd/root
/// (`CliError::Cwd` in agent mode) → `Env` → output format parse
/// (`crate::chat::parse_output_format` runs HERE, root.go:249-252, so `unknown output format …` loses to every
/// earlier provider/tuning/MCP error exactly as in Go) → `OutputFormatWithoutMessage` when the flag
/// was given and `message.is_none()` →
/// the root.go:259 branch — the `None` arm IS `interactive::run_interactive` (`TUI_CONTRACTS` §11; a non-TTY
/// stdout is refused there with Go's `interactive mode requires a terminal…`) → the resume stage
/// (root.go:284-334 on the `-m` path, DIVERGENCES D-41: resolve the fragment, load the bundle, replay its model
/// and tuning, re-raise the deferred `ModelRequired`, print the banner) → MCP connect (`connect_mcp`) →
/// `assemble::build_dispatcher` →
/// `crate::chat::once` → the turn's delta appended to the resumed bundle on SUCCESS only (D-43) →
/// `Manager::close()` always (also on error/cancel).
async fn run_agent(
    inv: Invocation,
    dirs: HostDirs,
    env: Arc<dyn EnvSource>,
    resolver: Arc<dyn VarResolver>,
    cancel: CancellationToken,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    // The only step that precedes Go: what a headless run cannot mean (D-23/D-54) is refused before anything
    // is read, and `-m` is the flag that decides Go's branch at root.go:259. The interactive branch reads
    // both for real (`TUI_CONTRACTS` §11).
    if inv.args.message.is_some() {
        inv.args.reject_unsupported()?;
        if inv.resume == Some(Resume::Pick) {
            return Err(CliError::ResumeIdRequired);
        }
    }

    // root.go:45
    let cfg = Config::load(inv.config.as_deref(), &dirs, resolver.as_ref(), &mut |w| {
        io.warning(&w);
    })?;

    // root.go:52-123
    let mut stdin = std::io::stdin();
    let mut settings = resolve_run(&inv, &cfg, env.as_ref(), &mut stdin, &mut |w| {
        io.warning(&w);
    })?;

    // root.go:125-131
    let ctx = RunContext::new(dirs, cancel, resolver);
    // root.go:132-181
    let (kind, provider) = open_provider(&settings, &ctx, io)?;
    // root.go:187-241
    let tools = assemble_tools(&inv, &cfg, &settings, &*provider, &ctx, io)?;

    // root.go:249-255: `--output-format` describes a single `-m` run. Parsed HERE — after tuning/MCP
    // assembly — so a bad value keeps Go's precedence (a bad config `effort`/`top_p` or `mcp_servers` name wins
    // over a bad format); "" is the text default without a parse, and a misplaced flag is an error rather than
    // a quiet fall back to text.
    let format = match settings.output_format_raw.as_deref() {
        None => OutputFormat::Text,
        Some(s) => crate::chat::parse_output_format(s)?,
    };
    if settings.output_format_raw.is_some() && settings.message.is_none() {
        return Err(CliError::OutputFormatWithoutMessage);
    }

    // root.go:259 — Go's headless-vs-interactive branch: no `-m` IS the interactive run (`TUI_CONTRACTS` §11).
    let Some(message) = settings.message.take() else {
        return interactive::run_interactive(
            interactive::Interactive {
                inv: &inv,
                cfg: &cfg,
                env: env.as_ref(),
                settings,
                kind,
                provider,
                ctx,
                tools,
            },
            io,
        )
        .await;
    };
    run_headless(
        message, &cfg, settings, kind, provider, tools, ctx, format, io,
    )
    .await
}

/// root.go:132-181 — the conversation provider, constructed and tuned.
fn open_provider(
    settings: &RunSettings,
    ctx: &RunContext,
    io: &mut io::Streams,
) -> Result<(ProviderKind, Box<dyn crate::provider::Provider>), CliError> {
    let kind: ProviderKind = settings.raw_type.parse()?;
    let mut provider = crate::provider::new_provider(
        kind,
        ProviderParams {
            api_key: &settings.api_key,
            base_url: &settings.base_url,
            model: &settings.model,
            temperature: settings.temperature,
        },
        Some(ctx.transport.clone()),
    )?;
    tuning::apply(
        &mut *provider,
        &settings.resolved,
        settings.temperature,
        &mut |w| io.warning(&w),
    )?;
    Ok((kind, provider))
}

/// root.go:187-241 — MCP configs, the agent options and the tool environment, in Go's order (each step's
/// byte-pinned error keeps its precedence).
fn assemble_tools(
    inv: &Invocation,
    cfg: &Config,
    settings: &RunSettings,
    provider: &dyn crate::provider::Provider,
    ctx: &RunContext,
    io: &mut io::Streams,
) -> Result<ToolAssembly, CliError> {
    let dirs = &ctx.dirs;
    // root.go:187-194
    let (mcp_configs, mcp_defers) =
        assemble::build_mcp_configs(cfg, &settings.resolved.agent, &inv.args.mcp, &mut |w| {
            io.warning(&w);
        })?;

    // root.go:195-199
    tuning::warn_tools_without_calling(
        provider,
        &settings.resolved.agent,
        mcp_configs.len(),
        &mut |w| {
            io.warning(&w);
        },
    );

    // root.go:206-217: the project root anchors the AGENTS.md/skills overlay and the agent set's skill
    // discovery, so it is resolved in every mode; only agent mode makes a missing cwd fatal.
    let project_root = dirs.cwd.as_deref().map(crate::agents::project_root);
    let agent = if settings.agent_mode {
        let root = project_root
            .clone()
            .ok_or_else(|| CliError::Cwd(cwd_err()))?;
        AgentOptions {
            enabled: true,
            root,
            cwd: dirs.cwd.clone(),
            home: dirs.home.clone(),
        }
    } else {
        AgentOptions::default()
    };

    // root.go:221-232, plus the run's job registry: the `shell` tool needs it to exist before the dispatcher
    // is built, and both branches need the same one afterwards.
    let jobs = crate::shell::jobs::Jobs::new(&dirs.temp);
    let mut tool_env = Env {
        project_root: project_root.clone(),
        dirs: dirs.clone(),
        jobs: Some(Arc::clone(&jobs)),
        ..Env::default()
    };
    let interactor = settings
        .message
        .is_none()
        .then(crate::repl::Interactor::new);
    if let Some(it) = &interactor {
        tool_env.interactor = Some(Arc::clone(it) as Arc<dyn crate::tool::Interactor>);
    }

    Ok(ToolAssembly {
        mcp_configs,
        mcp_defers,
        tool_env,
        interactor,
        jobs,
        agent,
    })
}

/// The `-m` branch (root.go:259-268 plus the resume stage D-41 moved onto it).
#[allow(clippy::too_many_arguments)]
async fn run_headless(
    message: String,
    cfg: &Config,
    settings: RunSettings,
    kind: ProviderKind,
    mut provider: Box<dyn crate::provider::Provider>,
    tools: ToolAssembly,
    ctx: RunContext,
    format: OutputFormat,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let ToolAssembly {
        mcp_configs,
        mcp_defers,
        tool_env,
        interactor: _,
        jobs,
        agent,
    } = tools;
    let RunContext {
        dirs,
        http,
        resolver,
        cancel,
        ..
    } = ctx;

    // root.go:284-334, moved onto the `-m` path (DIVERGENCES D-41). It sits HERE — after the interactive
    // branch, before the MCP connect — so `iota resume <id>` without `-m` belongs to the interactive branch (which
    // resumes it itself), every earlier byte-pinned error still wins, and a bad session id never spawns an MCP
    // server.
    let mut session = match &settings.resume {
        None => None,
        Some(fragment) => {
            let store = crate::session::SessionStore::from_dirs(&dirs)?;
            // root.go:298: agent mode tries the project's own bucket first and only widens on no match; normal
            // mode looks at the flat root (Go passes an empty `agentOpts.Root`).
            let scope = settings.agent_mode.then_some(agent.root.as_path());
            let id = store.resolve_id(fragment, scope)?;
            let (writer, resumed) = store.resume(&id, kind)?;
            // The bundle records the agent it ran under; one that has since been deleted is announced, and
            // the run falls back to the provider and model the meta carries (Phase 1b step 9).
            crate::session::warn_if_session_agent_is_gone(
                &resumed.meta,
                cfg.agents.contains_key(&resumed.meta.agent),
                &mut |w| io.warning(&w),
            );
            // root.go:323-325: the session supplies the model only when `-M`/`model:` did not, and only for a
            // session recorded under this provider type. An explicit `-M` does NOT rewrite `meta.model`.
            if settings.model.is_empty()
                && resumed.meta.provider == kind.as_str()
                && !resumed.meta.model.is_empty()
            {
                provider.set_model(resumed.meta.model.clone());
            }
            // root.go:107-109, deferred out of `resolve_run` and re-raised byte-identically here (D-52).
            if provider.model().is_empty() {
                return Err(CliError::ModelRequired);
            }
            // root.go:326-333: `-M` is the only flag left that a session must not overwrite, so temperature,
            // effort and the window always replay — a resumed run is the run it resumes. The replayed window
            // is discarded: headless has no context budget to route it into.
            let _window = crate::session::apply_session_tuning(
                &resumed.meta,
                &mut *provider,
                kind,
                &crate::session::Overrides {
                    model: !settings.model.is_empty(),
                    ..crate::session::Overrides::default()
                },
                &mut |w| io.warning(&w),
            );
            // root.go:334, on stderr with a single newline (DIVERGENCES D-44): headless stdout is the reply or
            // the JSON report alone, but with prefix resolution the user must see which session was picked.
            io.warning(&format!(
                "Resumed session {id} ({} messages)",
                resumed.messages.len()
            ));
            Some((writer, resumed))
        }
    };

    // root.go:261-268: connect MCP synchronously (the single request needs the full tool set before it is sent).
    let (manager, mcp_part) = connect_mcp(
        mcp_configs,
        crate::mcp::ManagerOptions::new(http, Arc::clone(&resolver)),
        &cancel,
        io,
    )
    .await;

    let dispatch = assemble::build_dispatcher(
        &settings.resolved.agent,
        &settings.resolved.model,
        mcp_part,
        mcp_defers,
        settings.agent_mode,
        &tool_env,
        &mut |m| io.caution(&m),
    );

    let opts = OnceOptions {
        message,
        system: settings.system,
        agent,
        max_turns: settings.max_turns,
        format,
        // chat/run.go:1094 (DIVERGENCES D-53): a resumed run saves its generated images INSIDE the bundle, so
        // the attachments it persists point at files the next resume can still read. Stateless `-m` keeps
        // `~/.iota/images`.
        images_dir: session
            .as_ref()
            .map_or_else(|| dirs.images_dir(), |(w, _)| Some(w.images_path())),
        // chat/run.go:69-74: a non-empty imported history wins over `-s` — a resumed session keeps the system
        // message from its own log.
        history: session
            .as_ref()
            .map(|(_, s)| s.messages.clone())
            .unwrap_or_default(),
        jobs: Some(jobs),
    };
    let outcome = crate::chat::once(cancel, &mut *provider, dispatch, opts, &mut *io.stdout).await;

    // chat/run.go:221-229, once (DIVERGENCES D-43): a headless run IS exactly one turn, so a SUCCESSFUL one
    // persists its delta in a single batch — one fsync, one meta rewrite. A failed or cancelled run persists
    // nothing, and a failure to save is a warning, never the run's exit status.
    if let (Ok(done), Some((writer, _))) = (&outcome, &mut session)
        && let Err(e) = writer.append_messages(&done.delta)
    {
        io.warning(&format!("Warning: failed to save session: {e}"));
    }

    // Go's `defer manager.Close()`: the servers are closed on every exit path, error and cancel included.
    manager.close().await;

    // `once` already turned every failure under a cancelled token into `ChatError::Interrupted` (CONTRACTS §6.2);
    // it has to arrive at `main` as `CliError::Interrupted` to become exit 130 rather than a generic 1
    // (DIVERGENCES I-03).
    outcome.map(|_| ()).map_err(|e| match e {
        crate::chat::ChatError::Interrupted => CliError::Interrupted,
        other => CliError::Chat(other),
    })
}

/// `os.Getwd()`'s error (root.go:207). `HostDirs` already swallowed it, so the OS is asked again for its text; a
/// second call that unexpectedly succeeds falls back to a generic message rather than an `unwrap`.
fn cwd_err() -> std::io::Error {
    std::env::current_dir()
        .err()
        .unwrap_or_else(|| std::io::Error::other("working directory is unavailable"))
}

/// Bridges the run's `EnvSource` seam to the `VarResolver` that `Config::load` and the MCP manager expand `${…}`
/// with, so ONE injected environment answers every lookup (nothing reads the process environment behind the
/// caller's back).
struct EnvResolver {
    env: Arc<dyn EnvSource>,
    dirs: HostDirs,
}

impl VarResolver for EnvResolver {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env.var(name)
    }

    fn cwd(&self) -> Option<PathBuf> {
        self.dirs.cwd.clone()
    }

    fn home(&self) -> Option<PathBuf> {
        self.dirs.home.clone()
    }
}

/// The MCP stage of `run` (root.go:216-233, headless half): `Manager::new` + `connect_all`, then one
/// `Warning: mcp server {name}: {err}` per failed server (DIVERGENCES I-05). Returns the manager (so `run` can
/// `close()` it on every exit path) and the [`assemble::McpPart`] `build_dispatcher` takes — `None` when no
/// server is configured.
async fn connect_mcp(
    configs: Vec<ServerConfig>,
    opts: crate::mcp::ManagerOptions,
    cancel: &CancellationToken,
    io: &mut io::Streams,
) -> (
    std::sync::Arc<crate::mcp::Manager>,
    Option<assemble::McpPart>,
) {
    let configured = !configs.is_empty();
    let manager = crate::mcp::Manager::new(configs, opts);
    if configured {
        for status in manager.connect_all(cancel).await {
            // Go drained the events silently on the `-m` path; a server the user configured and did not get is
            // worth one line (DIVERGENCES I-05).
            if !status.connected {
                io.warning(&format!(
                    "Warning: mcp server {}: {}",
                    status.name,
                    status.err.as_deref().unwrap_or_default()
                ));
            }
        }
    }
    let part = configured.then(|| assemble::McpPart::of(&manager));
    (manager, part)
}
