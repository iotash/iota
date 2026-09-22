//! Why the command failed (cmd/root.go, config.go and the headless-only rules), in the three stages a run
//! goes through: what the invocation itself got wrong ([`ArgsError`]), what the run could not be set up with
//! ([`SetupError`]) and what went wrong once it ran ([`RunError`]). [`CliError`] is their sum — what
//! [`crate::cmd::run`] answers and `main` prints as `Error: {e}` with exit 1, or 130 for
//! [`RunError::Interrupted`]. Every text is the Go line it ports, byte for byte (CONTRACTS §7.8); until
//! 2026-09-16 the thirty-three variants were one flat enum, and the split moved no character of any of them.

use crate::config::ConfigError;
use crate::provider::error::{ProviderError, UnknownProviderType};
use crate::text::go_float;

/// The invocation itself: a flag, a name or a value that clap accepted and the command refuses.
#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    /// An interactive-only flag was given headlessly (DIVERGENCES D-23): `--no-save` is the last one.
    #[error("flag {0} is not supported in headless mode")]
    UnsupportedFlag(&'static str),
    /// `iota resume -m "…"` with no id: what is missing headlessly is the picker, not the command.
    #[error(
        "iota resume needs a session id with -m (the picker is interactive): try `iota list sessions`"
    )]
    ResumeIdRequired,
    /// No agent named and no `agents.default` to fall back to.
    #[error(
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)"
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
    /// `-m -` could not read stdin.
    #[error("failed to read from stdin: {0}")]
    Stdin(#[source] std::io::Error),
    /// `-m -` read only whitespace.
    #[error("no message provided via stdin")]
    EmptyStdin,
    /// `-m ""` (POLICY F-03).
    #[error("--message must not be empty")]
    MessageEmpty,
    /// `-m` with no model from `-M` or `agents.<name>.model` — headless has no picker to open (X-48; it was
    /// Go's `--model/-M is required when using --message/-m`).
    #[error("no model chosen: set agents.{agent}.model or pass -M")]
    ModelRequired {
        /// The `agents:` entry the run resolved to.
        agent: String,
    },
    /// `--output-format` without `-m` (root.go:253).
    #[error("--output-format applies to -m runs only")]
    OutputFormatWithoutMessage,
    /// `--mcp ""` (POLICY F-02).
    #[error(transparent)]
    McpFlag(#[from] crate::mcp::config::McpFlagError),
    /// `iota list <what> <name>` where `<what>` is not `models` — only the choices belong to one agent.
    #[error("iota list {0} takes no argument (only `iota list models <agent>` does)")]
    ListTakesNoName(String),
    /// `--no-save` with a resume: an ephemeral start and a resumed bundle are opposite intents (root.go:285).
    #[error("--no-save cannot be combined with iota resume")]
    NoSaveWithResume,
    /// `iota mcp … --scope` beside `-c`: the explicit file IS the scope.
    #[error("--scope does not apply with -c: the file given is the only scope")]
    McpScopeWithConfig,
    /// `iota mcp add` with neither form (`"neither"`) or both (`"both"`).
    #[error("{}", mcp_add_target(.0))]
    McpAddTarget(&'static str),
    /// A flag of the other `add` form: `--header`/`--auth` on a command server, `-e` on a `--url` one.
    #[error("mcp add: {flag} applies to {form} servers only")]
    McpAddFlag {
        /// The flag as typed.
        flag: &'static str,
        /// The form it belongs to: `--url` or `command`.
        form: &'static str,
    },
    /// `--client-id` / `--client-secret-env` / `--redirect-port` beside `--auth none`: the flags describe an
    /// OAuth login, and `none` forbids one. (Without `--auth` they stand on their own: the login is `auto`.)
    #[error(
        "mcp add: --client-id, --client-secret-env and --redirect-port describe an OAuth login, which --auth none rules out"
    )]
    McpClientFlagsWithNone,
    /// `--client-secret-env` without `--client-id`, or a variable name that is not one.
    #[error(
        "mcp add: --client-secret-env wants the NAME of an environment variable, beside --client-id; got {0:?}"
    )]
    McpClientSecretEnv(String),
    /// `--redirect-port` without `--client-id`: only a pre-registered client has a fixed redirect URI.
    #[error(
        "mcp add: --redirect-port goes with --client-id (a pre-registered client's redirect URI must match exactly; a registered-on-the-spot or metadata-document client gets a random port)"
    )]
    McpRedirectPortNeedsClientId,
    /// A server name that is not a plain word (it is a YAML key and a wire-name segment).
    #[error("mcp add: a server name is letters, digits, `_`, `-` and `.`: {0:?}")]
    McpName(String),
    /// `--url` without an `http(s)://` scheme.
    #[error("mcp add: --url wants http:// or https://, got {0:?}")]
    McpUrlScheme(String),
    /// `--header` without a `Name: value` shape.
    #[error("mcp add: --header wants 'Name: value', got {0:?}")]
    McpBadHeader(String),
    /// `-e` without a `NAME=value` shape.
    #[error("mcp add: -e wants NAME=value, got {0:?}")]
    McpBadEnv(String),
    /// A project-scope header or env value that is not a `${…}` reference: the file is shared, the secret is
    /// not.
    #[error(
        "mcp: a project-scope value must reference an environment variable (${{NAME}}), not the secret itself: {0}"
    )]
    McpProjectSecret(String),
}

/// The [`ArgsError::McpAddTarget`] texts.
fn mcp_add_target(which: &str) -> &'static str {
    if which == "both" {
        "mcp add: give a command after `--` or --url, not both"
    } else {
        "mcp add: a server needs a command after `--`, or --url"
    }
}

/// What the run could not be set up with: the config, the key, the provider, the session store, the
/// terminal — everything between a good invocation and the first turn.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// A session-store failure on the resume path (resolution, location, meta or log read). Every `Display`
    /// is byte-equal to the Go line it ports (CONTRACTS S§5).
    #[error(transparent)]
    Session(#[from] crate::session::SessionError),
    /// No key in the environment and none in the config; names both places.
    #[error("API key is required: set {env} or providers.{provider}.key in your config")]
    ApiKeyRequired {
        /// The env var of the resolved provider TYPE.
        env: &'static str,
        /// The `providers:` entry the run landed on.
        provider: String,
    },
    /// The config `temperature:` is outside 0.0-2.0 (the `-t` flag is never checked).
    #[error("config temperature {}: want 0.0-2.0", go_float(*.0))]
    ConfigTemperature(f64),
    /// The config `effort:` is not one of the five levels.
    #[error("config effort {0:?}: want low|medium|high|xhigh|max")]
    ConfigEffort(String),
    /// The config `top_p:` is outside 0.0-1.0.
    #[error("config top_p {}: want 0.0-1.0", go_float(*.0))]
    ConfigTopP(f64),
    /// The working directory could not be resolved (agent mode).
    #[error("failed to resolve working directory: {0}")]
    Cwd(#[source] std::io::Error),
    /// The resolved type string is not a built-in kind (provider.go:329 text).
    #[error(transparent)]
    UnknownType(#[from] UnknownProviderType),
    /// Provider construction failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// A config-level failure (`system_file`, unknown `mcp_servers` name).
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The interactive branch was reached without a terminal on stdin/stdout (root.go:400).
    #[error("interactive mode requires a terminal; use -m/--message for piped input")]
    NotATerminal,
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
        source: crate::config::window::WindowSizeError,
    },
    /// The interactive session picker, or `iota list sessions`, could not read the store (root.go:305).
    #[error("failed to list sessions: {0}")]
    ListSessions(#[source] crate::session::SessionError),
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
    /// The `mcp_servers:` block could not be read or rewritten (`config::edit`).
    #[error(transparent)]
    McpEdit(#[from] crate::config::edit::EditError),
    /// `iota mcp add` of a name the target file already declares: an entry is replaced by removing it first,
    /// never silently.
    #[error(
        "mcp: a server named {name:?} already exists in {file} (remove it first, or pick another name)"
    )]
    McpExists {
        /// The name as typed.
        name: String,
        /// The file that declares it.
        file: String,
    },
    /// `iota mcp get|remove <name>` of a name no file declares.
    #[error("mcp: no server named {name:?}{}", mcp_server_hint(.servers))]
    McpUnknown {
        /// The name as typed.
        name: String,
        /// Every declared server, sorted.
        servers: Vec<String>,
    },
    /// `iota mcp remove <name> --scope <s>` where that file does not declare the name.
    #[error("mcp: no server named {name:?} in {file}")]
    McpNotInScope {
        /// The name as typed.
        name: String,
        /// The scope's file.
        file: String,
    },
    /// `iota mcp login|logout <name>` on a server no login applies to: a stdio server, `auth: none`, or an
    /// entry that sends its own `Authorization` header (`ServerConfig::login_refusal` says which).
    #[error("mcp: {name:?} {reason}")]
    McpNoLogin {
        /// The name as typed.
        name: String,
        /// The sentence after the name.
        reason: &'static str,
    },
    /// `iota mcp login|logout` with no home directory to keep the token file in.
    #[error("$HOME is not defined: there is nowhere to keep the token")]
    McpNoHomeForToken,
    /// `iota mcp remove <name>` where both tiers declare the name.
    #[error("mcp: {name:?} is declared in more than one file; say which with --scope:\n  {}", files.join("\n  "))]
    McpAmbiguous {
        /// The name as typed.
        name: String,
        /// The files that declare it, in merge order.
        files: Vec<String>,
    },
}

/// The [`SetupError::McpUnknown`] hint: the servers there are, or that there are none.
fn mcp_server_hint(servers: &[String]) -> String {
    if servers.is_empty() {
        " (none are configured)".to_owned()
    } else {
        format!("\n  configured servers: {}", servers.join(", "))
    }
}

/// What went wrong once the run was under way: the loop, the facade, the process.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The run loop failed (incl. `unknown output format …`).
    #[error(transparent)]
    Chat(#[from] crate::headless::ChatError),
    /// SIGINT/SIGTERM cancelled the run (exit 130; DIVERGENCES I-03).
    #[error("interrupted")]
    Interrupted,
    /// The interactive facade failed.
    #[error(transparent)]
    Ui(#[from] crate::ui::facade::UiError),
    /// A `spawn_blocking` worker could not be joined.
    #[error("{0}")]
    Join(String),
    /// An I/O failure with no more specific home (runtime construction, output streams).
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// `iota mcp login <name>` did not finish: discovery, registration, the browser round trip, the exchange.
    #[error("mcp login {name}: {source}")]
    McpLogin {
        /// The server.
        name: String,
        /// What went wrong.
        #[source]
        source: crate::mcp::auth::LoginError,
    },
    /// The login `iota mcp add <name> --url …` started did not finish. The entry it wrote stays, so the
    /// text says what to retry — the login alone, not the `add`.
    #[error("mcp login {name}: {source}\n  Retry with: iota mcp login {name}")]
    McpAddLogin {
        /// The server.
        name: String,
        /// What went wrong.
        #[source]
        source: crate::mcp::auth::LoginError,
    },
    /// `iota mcp logout <name>` could not read or remove the token file.
    #[error("mcp logout {name}: {source}")]
    McpLogout {
        /// The server.
        name: String,
        /// The file failure.
        #[source]
        source: std::io::Error,
    },
}

/// The command's error: one of the three stages, printed as it is.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The invocation was wrong.
    #[error(transparent)]
    Args(#[from] ArgsError),
    /// The run could not be set up.
    #[error(transparent)]
    Setup(#[from] SetupError),
    /// The run failed.
    #[error(transparent)]
    Run(#[from] RunError),
}

/// `?` from a leaf error straight into the sum: the stage each leaf belongs to is fixed here, once.
macro_rules! via {
    ($($leaf:ty => $stage:ident),* $(,)?) => {
        $(
            impl From<$leaf> for CliError {
                fn from(e: $leaf) -> Self {
                    Self::$stage(e.into())
                }
            }
        )*
    };
}

via! {
    crate::mcp::config::McpFlagError => Args,
    crate::session::SessionError => Setup,
    UnknownProviderType => Setup,
    ProviderError => Setup,
    ConfigError => Setup,
    crate::headless::ChatError => Run,
    crate::ui::facade::UiError => Run,
    std::io::Error => Run,
}

/// The [`ArgsError::UnknownAgent`] hint: the agents there are, or where to add one. (A run with NO config
/// file never gets here: it writes the starter first, `cmd::config_cmd::auto_init`.)
fn agent_hint(agents: &[String]) -> String {
    if agents.is_empty() {
        "\n  no agents are configured — add an `agents:` entry to your config (`iota config path` names the file)".to_owned()
    } else {
        format!("\n  configured agents: {}", agents.join(", "))
    }
}
