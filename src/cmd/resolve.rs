//! Pure run resolution (cmd/root.go:46-123, 534-566): the agent lookup, the key/model/system precedence, the
//! `-m -` stdin read, the message/model rules and the config temperature range check — everything `run`
//! decides before it constructs a provider — plus `CliError`, the command's error type.

use crate::provider::error::{ProviderError, UnknownProviderType};
use crate::provider::provider_env_key;
use crate::text::go_float;

use crate::cmd::cli::{Invocation, Resume};
use crate::config::{Config, ConfigError, ModelRef, ProviderConfig, Resolved};

use crate::vars::EnvSource;

/// What `resolve_run` decided for one invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSettings {
    /// The `agents:` entry this run is (as typed, or `default`).
    pub name: String,
    /// The resolved provider type string.
    pub raw_type: String,
    /// The three config layers this run landed on.
    pub resolved: Resolved,
    /// The API key: the env var of the RESOLVED provider type, else `providers.<name>.key`. Never a flag —
    /// a key on the command line lands in the shell history and in `ps`, and iota hands the model a shell
    /// tool (brain page `cli-surface-agent-first`).
    pub api_key: String,
    /// The base URL from `providers.<name>.url` (`""` = dialect default).
    pub base_url: String,
    /// The model (`-M` > the agent's default model; may be `""` without `-m`).
    pub model: String,
    /// The system prompt (`-s` > `system:` > `system_file:`; `""` = none).
    pub system: String,
    /// `None` = `-m` not given (Go: `chatMessage == ""` → the interactive branch, taken at root.go:259's
    /// position in `run`). `Some` is never empty (`MessageEmpty`).
    pub message: Option<String>,
    /// The temperature the agent or its model declares (range-checked).
    pub temperature: Option<f64>,
    /// The agent's `workspace: true` — the AGENTS.md overlay and the skills toolset.
    pub agent_mode: bool,
    /// `--max-turns` as a cap (`None` = unlimited; the flag's `<= 0`).
    pub max_turns: Option<std::num::NonZeroU32>,
    /// `--output-format` as typed (`None` = flag absent). Carried RAW: `run` parses it at exactly Go's
    /// position (root.go:249-252, after tuning/MCP assembly), so `unknown output format …` keeps
    /// Go's precedence; `Some` also drives `OutputFormatWithoutMessage` there (root.go:253).
    pub output_format_raw: Option<String>,
    /// The id fragment `iota resume <id>` named; `None` for `iota run` AND for the bare `iota resume`, whose
    /// session the interactive picker chooses.
    pub resume: Option<String>,
}

/// Pure. Order (root.go:53-123): the agent — the name given, else `agents.default` when the config declares
/// one, else `NoAgent` → the agent lookup (`resolve_agent`) → `-M` (which may move the run to another
/// provider) → key (env of the RESOLVED type > `providers.<name>.key`) → system (`-s` > the agent's) →
/// `ApiKeyRequired` → `-m -` reads stdin (trim; `failed to read from stdin: {e}`; `no message provided via
/// stdin`) → `-m ""` → `MessageEmpty` → model required ONLY when message is `Some` AND no resume is in play
/// (D-52: a resume supplies the model from meta, so `run` re-raises `ModelRequired` after the replay) →
/// temperature (config range).
/// `--output-format` is NOT parsed here and `OutputFormatWithoutMessage` is NOT raised here (Go does both after
/// tuning/MCP, root.go:249-255) — the raw flag rides `output_format_raw` and `run` does both.
///
/// `-M` is applied BEFORE the key because `provider:id` names an endpoint: a model the run was not started on
/// brings its own key variable and base URL with it.
pub fn resolve_run(
    inv: &Invocation,
    cfg: &Config,
    env: &dyn EnvSource,
    stdin: &mut dyn std::io::Read,
    warn: &mut dyn FnMut(String),
) -> Result<RunSettings, CliError> {
    // root.go:53-60, as the agent-first surface reads it: `iota run <agent>` names one, everything else takes
    // `agents.default` when the config declares it.
    let name = match inv.agent.as_deref() {
        Some(name) => name,
        None => cfg.default_agent().ok_or(CliError::NoAgent)?,
    };
    let mut resolved = resolve_agent(cfg, name)?;
    if let Some(flag) = &inv.args.model {
        apply_model_flag(&mut resolved, cfg, flag, warn);
    }
    let raw_type = resolved.provider_type.clone();

    // root.go:62-87, minus the flag: the env var of the RESOLVED type, else the config `key:`.
    let env_key = provider_env_key(&raw_type);
    let api_key = resolve_key_from_env_or_config(env_key, &resolved.provider, env);
    let base_url = resolved.provider.url.clone();
    let model = resolved.model.id.clone();
    let system = match &inv.args.system {
        Some(flag) => flag.clone(),
        None => resolved.agent.resolve_system()?,
    };

    // root.go:89-92
    if api_key.is_empty() {
        return Err(CliError::ApiKeyRequired {
            env: env_key,
            provider: resolved.provider_name.clone(),
        });
    }

    // root.go:95-104 (`-m -`), then POLICY F-03 (`-m ""`).
    let message = match inv.args.message.as_deref() {
        None => None,
        Some("-") => Some(read_stdin_message(stdin)?),
        Some("") => return Err(CliError::MessageEmpty),
        Some(m) => Some(m.to_owned()),
    };

    // root.go:107-109: non-interactive mode requires a model — DEFERRED for a resume, because a resumed
    // session can supply it from its meta (root.go:323-325). `run` re-raises this exact error after the model
    // replay, so a provider-mismatched or model-less bundle still fails with Go's text (DIVERGENCES D-52).
    if message.is_some() && model.is_empty() && inv.resume.is_none() {
        return Err(CliError::ModelRequired);
    }

    // root.go:114-123: the config default is range-checked (the `-t` flag that skipped the check is gone).
    let temperature = match resolved.temperature() {
        Some(t) if !(0.0..=2.0).contains(&t) => return Err(CliError::ConfigTemperature(t)),
        t => t,
    };

    Ok(RunSettings {
        name: name.to_owned(),
        raw_type,
        api_key,
        base_url,
        model,
        system,
        message,
        temperature,
        agent_mode: resolved.agent.workspace,
        max_turns: crate::chat::turns::turn_cap(inv.args.max_turns),
        output_format_raw: inv.args.output_format.clone(),
        resume: match &inv.resume {
            Some(Resume::Id(id)) => Some(id.clone()),
            Some(Resume::Pick) | None => None,
        },
        resolved,
    })
}

/// The agent lookup, with the unknown-name error that lists the agents there ARE.
pub(crate) fn resolve_agent(cfg: &Config, name: &str) -> Result<Resolved, CliError> {
    cfg.resolve_agent(name)
        .ok_or_else(|| CliError::UnknownAgent {
            name: name.to_owned(),
            agents: cfg.agent_names(),
        })
}

/// `-M`, in the three forms it takes:
///
/// - `provider:id` — an endpoint AND a model, so the run moves to that provider (only when `provider` is a
///   name iota knows; a bare id that happens to contain a colon is left alone, which is what a relay's
///   `vendor:model` ids need);
/// - `provider:*` — that provider with no model chosen, i.e. start in the picker;
/// - a bare value — a `models:` entry from the agent's own candidate set if it names one, else a raw model id
///   on the provider the run already resolved to.
///
/// A model outside the agent's candidate set is a WARNING, never a refusal (decision of 2026-09-10): the set
/// is advice about what is good here, not a whitelist.
fn apply_model_flag(r: &mut Resolved, cfg: &Config, flag: &str, warn: &mut dyn FnMut(String)) {
    if flag.is_empty() {
        // `-M ""` is verbatim, like every other flag: no model was chosen.
        r.model.id.clear();
        return;
    }
    match flag.split_once(':') {
        Some((provider, id)) if cfg.knows_provider(provider) && !id.is_empty() => {
            r.move_to_provider(cfg, provider);
            r.model.id = if id == "*" {
                String::new()
            } else {
                id.to_owned()
            };
        }
        _ => match cfg.models.get(flag) {
            // A candidate by name brings its own provider and protocol with it.
            Some(entry)
                if r.agent
                    .models
                    .iter()
                    .any(|c| matches!(c, ModelRef::Entry(n) if n == flag)) =>
            {
                let mut model = entry.clone();
                model.anchor_provider(flag);
                let provider = model.provider.clone();
                r.model = model;
                r.move_to_provider(cfg, &provider);
            }
            // Everything else is a raw model id: the entry's protocol and knobs still apply, only the id moves.
            _ => flag.clone_into(&mut r.model.id),
        },
    }
    if !r.agent.models.is_empty()
        && !r.model.id.is_empty()
        && !r.covers(cfg, &r.provider_name, &r.model.id)
    {
        warn(format!(
            "Warning: model {}:{} is not in agent {:?}'s models (using it anyway)",
            r.provider_name, r.model.id, r.name
        ));
    }
}

/// root.go:64-69 / 492-497: the env var of the resolved type when set and non-empty, else the config `key:`
/// (possibly `""`).
pub(crate) fn resolve_key_from_env_or_config(
    env_key: &str,
    provider_cfg: &ProviderConfig,
    env: &dyn EnvSource,
) -> String {
    env.var(env_key)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| provider_cfg.key.clone())
}

/// root.go:95-104: read ALL of stdin, trim, reject an empty message.
fn read_stdin_message(stdin: &mut dyn std::io::Read) -> Result<String, CliError> {
    let mut data = Vec::new();
    stdin.read_to_end(&mut data).map_err(CliError::Stdin)?;
    let message = String::from_utf8_lossy(&data).trim().to_owned();
    if message.is_empty() {
        return Err(CliError::EmptyStdin);
    }
    Ok(message)
}

/// Why the command failed (cmd/root.go, config.go and the headless-only rules). `main` prints
/// `Error: {e}` and exits 1, or 130 for `Interrupted`.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// An interactive-only flag was given headlessly (DIVERGENCES D-23): `--no-save` is the last one.
    #[error("flag {0} is not supported in headless mode")]
    UnsupportedFlag(&'static str),
    /// `iota resume -m "…"` with no id: what is missing headlessly is the picker, not the command.
    #[error(
        "iota resume needs a session id with -m (the picker is interactive): try `iota list sessions`"
    )]
    ResumeIdRequired,
    /// A session-store failure on the resume path (resolution, location, meta or log read). Every `Display`
    /// is byte-equal to the Go line it ports (CONTRACTS S§5).
    #[error(transparent)]
    Session(#[from] crate::session::SessionError),
    /// No agent named and no `agents.default` to fall back to.
    #[error(
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry — `iota config init` writes a starter config"
    )]
    NoAgent,
    /// `iota run <name>` where `name` is not an `agents:` entry. It used to fall through to `models:`,
    /// `providers:` and the built-in types; an agent is the only thing a run can name now.
    #[error("unknown agent {name:?}{}", agent_hint(.agents))]
    UnknownAgent {
        /// The name as typed.
        name: String,
        /// Every configured agent, sorted (`BTreeMap` keys).
        agents: Vec<String>,
    },
    /// No key in the environment and none in the config; names both places.
    #[error("API key is required: set {env} or providers.{provider}.key in your config")]
    ApiKeyRequired {
        /// The env var of the resolved provider TYPE.
        env: &'static str,
        /// The `providers:` entry the run landed on.
        provider: String,
    },
    /// `-m -` could not read stdin.
    #[error("failed to read from stdin: {0}")]
    Stdin(#[source] std::io::Error),
    /// `-m -` read only whitespace.
    #[error("no message provided via stdin")]
    EmptyStdin,
    /// `-m ""` (POLICY F-03).
    #[error("--message must not be empty")]
    MessageEmpty,
    /// `-m` without a model from `-M` or the agent's candidate set.
    #[error("--model/-M is required when using --message/-m")]
    ModelRequired,
    /// The config `temperature:` is outside 0.0-2.0 (the `-t` flag is never checked).
    #[error("config temperature {}: want 0.0-2.0", go_float(*.0))]
    ConfigTemperature(f64),
    /// The config `effort:` is not one of the five levels.
    #[error("config effort {0:?}: want low|medium|high|xhigh|max")]
    ConfigEffort(String),
    /// The config `top_p:` is outside 0.0-1.0.
    #[error("config top_p {}: want 0.0-1.0", go_float(*.0))]
    ConfigTopP(f64),
    /// `--output-format` without `-m` (root.go:253).
    #[error("--output-format applies to -m runs only")]
    OutputFormatWithoutMessage,
    /// The working directory could not be resolved (agent mode).
    #[error("failed to resolve working directory: {0}")]
    Cwd(#[source] std::io::Error),
    /// `--mcp ""` (POLICY F-02).
    #[error(transparent)]
    McpFlag(#[from] crate::mcp::config::McpFlagError),
    /// The resolved type string is not a built-in kind (provider.go:329 text).
    #[error(transparent)]
    UnknownType(#[from] UnknownProviderType),
    /// Provider construction or the run's provider failure.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// A config-level failure (`system_file`, unknown `mcp_servers` name).
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The run loop failed (incl. `unknown output format …`).
    #[error(transparent)]
    Chat(#[from] crate::chat::ChatError),
    /// SIGINT/SIGTERM cancelled the run (exit 130; DIVERGENCES I-03).
    #[error("interrupted")]
    Interrupted,
    /// The interactive branch was reached without a terminal on stdin/stdout (root.go:400).
    #[error("interactive mode requires a terminal; use -m/--message for piped input")]
    NotATerminal,
    /// `--no-save` with a resume: an ephemeral start and a resumed bundle are opposite intents (root.go:285).
    #[error("--no-save cannot be combined with iota resume")]
    NoSaveWithResume,
    /// `iota resume` whose picker found nothing, or that the user walked away from (root.go:314).
    #[error("no session to resume")]
    NoSessionToResume,
    /// A resumed bundle's meta or `context_window:` failed to parse (root.go:365-385).
    #[error("{label}: {source}")]
    ContextWindow {
        /// Which of the three sources failed, as Go names it.
        label: String,
        /// The parse failure.
        #[source]
        source: crate::cmd::window::WindowSizeError,
    },
    /// The interactive session picker, or `iota list sessions`, could not read the store (root.go:305).
    #[error("failed to list sessions: {0}")]
    ListSessions(#[source] crate::session::SessionError),
    /// `iota list <what> <name>` where `<what>` is not `models` — only a candidate set belongs to one agent.
    #[error("iota list {0} takes no argument (only `iota list models <agent>` does)")]
    ListTakesNoName(String),
    /// `iota config init` where the file already exists: a config is hand-written state, so it is never
    /// overwritten.
    #[error("{0} already exists (use -c <path> to write somewhere else)")]
    ConfigExists(String),
    /// `iota config init` with no `$HOME` to put `~/.iota.yaml` in.
    #[error("$HOME is not defined: use -c <path> to say where the config should go")]
    NoHome,
    /// A new interactive bundle could not be created (root.go:340).
    #[error("failed to create session: {0}")]
    CreateSession(#[source] crate::session::SessionError),
    /// The interactive facade failed.
    #[error(transparent)]
    Ui(#[from] crate::ui::facade::UiError),
    /// A `spawn_blocking` worker could not be joined.
    #[error("{0}")]
    Join(String),
    /// An I/O failure with no more specific home (runtime construction, output streams).
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// The [`CliError::UnknownAgent`] hint: the agents there are, or the one command that creates some.
fn agent_hint(agents: &[String]) -> String {
    if agents.is_empty() {
        "\n  no agents are configured — run `iota config init` to write a starter config".to_owned()
    } else {
        format!("\n  configured agents: {}", agents.join(", "))
    }
}
