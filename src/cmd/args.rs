//! The command line: a closed set of verbs (`run`, `list`, `resume`, `config`, `mcp`, `version`) plus the nine
//! flags that describe ONE invocation.
//!
//! The rule the surface follows (brain page `cli-surface-agent-first`): a flag stays on the command line only
//! when it describes THIS call; anything that describes configuration lives in `~/.iota.yaml`'s three layers.
//! That retired `-k/--key`, `-u/--url`, `-t/--temperature`, `-S/--system-input`, `--context-window` and the
//! boolean `--agent`. The positional argument stopped being a name looked up in four namespaces — where a
//! collision silently changed what ran — and became the verb, so `--help` is now the complete map of what iota
//! can do.
//!
//! Argument errors are clap's own (exit 2, DIVERGENCES D-24); everything else is decided by
//! [`crate::cmd::run`].

use std::path::PathBuf;

/// `iota [command] [flags]`. With no command it IS `run`, so `iota` and `iota -m "hi"` keep working.
#[derive(clap::Parser, Debug, Clone)]
#[command(
    name = "iota",
    about = "An agent CLI for the terminal",
    version = env!("CARGO_PKG_VERSION")
)]
pub struct Cli {
    /// The verb; `None` = `run` with no agent name.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// `-c/--config`: GLOBAL, so it is valid before or after the verb (`iota -c f.yaml list` and
    /// `iota list -c f.yaml` are the same invocation). WHICH file to read is a property of the whole
    /// command rather than of one verb, and `git -C` / `cargo --config` taught the same shape.
    #[arg(
        short = 'c',
        long = "config",
        global = true,
        value_name = "PATH",
        help = "Path to config file (default: ~/.iota.yaml, then ./.iota.yaml)"
    )]
    pub config: Option<PathBuf>,
    /// The flags a bare `iota` takes — `run`'s own.
    #[command(flatten)]
    pub run: RunArgs,
}

impl Cli {
    /// `run`'s flags are meaningful only for a run, and giving one BEFORE another verb (`iota -m hi list`)
    /// is a mistake rather than a shape with a meaning.
    ///
    /// clap says this itself with `args_conflicts_with_subcommands`, but that setting and a GLOBAL `-c` are
    /// mutually exclusive: once any root argument is seen the setting stops the verb from being recognised at
    /// all, so `iota -c f.yaml list` fails with a message about `--config` that reads as if the two could
    /// never be combined. A config path that works in both positions is worth more than the setting (`git -C`
    /// and `cargo --config` take theirs before the verb), so the check lives here — and reports through
    /// clap's own error, keeping the usage block and exit 2 (DIVERGENCES D-24).
    pub fn check_flag_placement(&self) -> Result<(), clap::Error> {
        use clap::CommandFactory as _;

        let (Some(command), Some(flag)) = (&self.command, self.run.first_given()) else {
            return Ok(());
        };
        Err(Self::command().error(
            clap::error::ErrorKind::ArgumentConflict,
            format!(
                "'{flag}' is a flag of `iota run`; put it after the '{}' command",
                command.verb()
            ),
        ))
    }

    /// The command this invocation means — the verb as typed, or `run` with no agent when none was given —
    /// and the `-c/--config` path, which belongs to all of them.
    pub fn into_command(self) -> (Command, Option<PathBuf>) {
        let command = self.command.unwrap_or(Command::Run(RunCmd {
            agent: None,
            args: self.run,
        }));
        (command, self.config)
    }
}

/// The verbs. The set is CLOSED and never grows into user data: an `agents:` entry may be called `list`
/// without the run it names ever being shadowed by a future verb.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum Command {
    /// Run an agent — interactively, or headlessly with -m
    Run(RunCmd),
    /// List what the config declares (agents, models, providers) or the saved sessions
    List(ListCmd),
    /// Resume a saved session
    Resume(ResumeCmd),
    /// Check, locate or create the config file
    Config(ConfigCmd),
    /// Add, list, inspect or remove MCP servers
    Mcp(McpCmd),
    /// Print the version
    Version,
}

impl Command {
    /// The word the user typed for this verb.
    fn verb(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
            Self::List(_) => "list",
            Self::Resume(_) => "resume",
            Self::Config(_) => "config",
            Self::Mcp(_) => "mcp",
            Self::Version => "version",
        }
    }
}

/// `iota run [<agent>] [flags]`.
#[derive(clap::Args, Debug, Clone)]
pub struct RunCmd {
    /// The `agents:` entry to run (default: the agent named `default`)
    pub agent: Option<String>,
    /// The run flags.
    #[command(flatten)]
    pub args: RunArgs,
}

/// `iota resume [<id>] [flags]`.
#[derive(clap::Args, Debug, Clone)]
pub struct ResumeCmd {
    /// The session id, or any unique prefix of one; omit it to pick from a list
    pub id: Option<String>,
    /// The run flags — a resume is a run that starts from a saved bundle.
    #[command(flatten)]
    pub args: RunArgs,
}

/// `iota list [what] [<agent>]`.
#[derive(clap::Args, Debug, Clone)]
pub struct ListCmd {
    /// What to list (default: agents)
    pub what: Option<ListWhat>,
    /// `list models <agent>`: the agent whose candidate set to show
    pub agent: Option<String>,
}

/// The four things `iota list` can show.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListWhat {
    /// The `agents:` entries — what a run may name.
    Agents,
    /// The `models:` entries, or one agent's candidate set.
    Models,
    /// The `providers:` entries and where each one's key comes from.
    Providers,
    /// The saved sessions, newest first.
    Sessions,
}

/// `iota config [check|path|init]`.
#[derive(clap::Args, Debug, Clone)]
pub struct ConfigCmd {
    /// What to do (default: check)
    pub action: Option<ConfigAction>,
}

/// The three things `iota config` can do.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    /// Load the config and report what it says (the default).
    Check,
    /// Print the file(s) this invocation reads.
    Path,
    /// Write a commented starter config.
    Init,
}

/// `iota mcp <action>` — the servers a config declares, edited from the command line (brain page
/// `mcp-cli-and-oauth`). Every action reads or writes the `mcp_servers:` block of ONE file: the scope's
/// (`--scope user` = `~/.iota.yaml`, the default; `--scope project` = `./.iota.yaml`), or the `-c` file alone.
#[derive(clap::Args, Debug, Clone)]
pub struct McpCmd {
    /// What to do.
    #[command(subcommand)]
    pub action: McpAction,
}

/// The `iota mcp` actions.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Add a server: `add <name> -- <command> [args…]` or `add <name> --url <url>`
    Add(McpAddCmd),
    /// List the configured servers: name, transport, file, auth
    List(McpListCmd),
    /// Show one server's entry and the file it comes from
    Get {
        /// The server name
        name: String,
    },
    /// Remove a server from the file that declares it
    Remove {
        /// The server name
        name: String,
        /// Which file to remove it from, when both declare it
        #[arg(long, value_name = "user|project")]
        scope: Option<McpScope>,
    },
}

/// `iota mcp add <name> [flags] -- <command> [args…]` / `iota mcp add <name> --url <url> [flags]`.
#[derive(clap::Args, Debug, Clone)]
pub struct McpAddCmd {
    /// The server name (a plain word: letters, digits, `_`, `-`, `.`)
    pub name: String,
    /// Which file to write: user (~/.iota.yaml, the default) or project (./.iota.yaml)
    #[arg(long, value_name = "user|project")]
    pub scope: Option<McpScope>,
    /// Environment for a command server (repeatable)
    #[arg(short = 'e', long = "env", value_name = "NAME=value", action = clap::ArgAction::Append)]
    pub env: Vec<String>,
    /// Defer the server's tools behind a search; the value is the one-line summary the model sees
    #[arg(long, value_name = "SUMMARY")]
    pub defer: Option<String>,
    /// The streamable-HTTP endpoint (instead of a command)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,
    /// A header for a --url server, as 'Name: value' (repeatable)
    #[arg(long = "header", value_name = "'Name: value'", action = clap::ArgAction::Append)]
    pub headers: Vec<String>,
    /// How a --url server is authenticated: oauth (then `iota mcp login <name>`) or none
    #[arg(long, value_name = "oauth|none")]
    pub auth: Option<McpAuthArg>,
    /// The command and its arguments, after `--`
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

/// `iota mcp list [--scope user|project|all] [--json] [--probe]`.
#[derive(clap::Args, Debug, Clone)]
pub struct McpListCmd {
    /// Which file(s) to read (default: all, the project file winning a name)
    #[arg(long, value_name = "user|project|all")]
    pub scope: Option<McpListScope>,
    /// One JSON array instead of the table
    #[arg(long)]
    pub json: bool,
    /// Connect to every server and report the outcome beside its row
    #[arg(long)]
    pub probe: bool,
}

/// The two files a writing action can target.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpScope {
    /// `~/.iota.yaml`.
    User,
    /// `./.iota.yaml`.
    Project,
}

/// What `iota mcp list` reads.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpListScope {
    /// `~/.iota.yaml` alone.
    User,
    /// `./.iota.yaml` alone.
    Project,
    /// Both, merged the way a run merges them.
    All,
}

/// `--auth` on `iota mcp add --url`.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpAuthArg {
    /// OAuth 2.1: `iota mcp login <name>` afterwards.
    Oauth,
    /// The headers as written, nothing more (the default).
    None,
}

/// The flags of one run. Every one of them describes THIS invocation; nothing here is configuration.
///
/// `-c/--config` is NOT among them: which file to read belongs to every verb, so it lives on [`Cli`] as a
/// global argument.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct RunArgs {
    /// `-m/--message`: the single message; `-` reads stdin; `""` is `ArgsError::MessageEmpty` (F-03).
    #[arg(
        short = 'm',
        long = "message",
        help = "Send a single message and print the response (non-interactive, use '-' to read from stdin)"
    )]
    pub message: Option<String>,
    /// `-M/--model`: a candidate's name, a bare model id, or `provider:id` / `provider:*`.
    #[arg(
        short = 'M',
        long = "model",
        help = "Model to use: a models: entry, a bare id, or provider:id (provider:* opens the picker)"
    )]
    pub model: Option<String>,
    /// `-s/--system`: the system prompt (beats the agent's `system:` / `system_file:`).
    #[arg(short = 's', long = "system", help = "System prompt for this run")]
    pub system: Option<String>,
    /// `--mcp`: extra MCP servers (command string or URL), in flag order after the config's servers.
    #[arg(
        long = "mcp",
        action = clap::ArgAction::Append,
        help = "MCP server (command string or URL, repeatable)"
    )]
    pub mcp: Vec<String>,
    /// `--no-save`: interactive-only; rejected by [`RunArgs::reject_unsupported`].
    #[arg(
        long = "no-save",
        help = "Start ephemeral: nothing persists unless you run /save in the chat"
    )]
    pub no_save: bool,
    /// `--max-turns`: the run-wide tool-turn budget; `0` (or a negative value) = unlimited (ARCHITECTURE G26).
    #[arg(
        long = "max-turns",
        default_value_t = 0,
        allow_hyphen_values = true,
        help = "Limit agentic tool turns for the whole run (-m only; 0 = unlimited)"
    )]
    pub max_turns: i64,
    /// `--output-format`: `text` (default) or `json`; `-m` runs only.
    #[arg(
        long = "output-format",
        help = "Non-interactive output: text (default, the reply alone) or json (one result object with token usage)"
    )]
    pub output_format: Option<String>,
}

impl RunArgs {
    /// The first flag this set carries, named as the user would type it; `None` when none was given. It is
    /// what [`Cli::check_flag_placement`] reports, so the list is exactly the nine minus the global `-c`.
    fn first_given(&self) -> Option<&'static str> {
        [
            (self.message.is_some(), "-m/--message"),
            (self.model.is_some(), "-M/--model"),
            (self.system.is_some(), "-s/--system"),
            (!self.mcp.is_empty(), "--mcp"),
            (self.no_save, "--no-save"),
            (self.max_turns != 0, "--max-turns"),
            (self.output_format.is_some(), "--output-format"),
        ]
        .into_iter()
        .find_map(|(given, name)| given.then_some(name))
    }

    /// `--no-save` headlessly → `Err(ArgsError::UnsupportedFlag)`. It is the last of the interactive-only flags
    /// (`-S/--system-input` is gone and the blank `--resume` became `iota resume` with no id, whose headless
    /// refusal is [`ArgsError::ResumeIdRequired`](crate::cmd::ArgsError::ResumeIdRequired)).
    ///
    /// The function is PURE: it answers "would a headless run accept these flags?". The INTERACTIVE LIFT
    /// (`TUI_CONTRACTS` §11) lives at the ONE call site instead — [`crate::cmd::run`] asks only for a run that
    /// carries `-m`, the flag that decides Go's branch at root.go:259.
    pub fn reject_unsupported(&self) -> Result<(), crate::cmd::ArgsError> {
        if self.no_save {
            return Err(crate::cmd::ArgsError::UnsupportedFlag("--no-save"));
        }
        Ok(())
    }
}

/// One `run` or `resume` invocation with the verb already decided: everything [`crate::cmd::resolve_run`]
/// needs, and nothing about which word the user typed to get here.
#[derive(Debug, Clone, Default)]
pub struct Invocation {
    /// The `agents:` entry named on the command line; `None` = `agents.default`.
    pub agent: Option<String>,
    /// The session to start from, when the verb was `resume`.
    pub resume: Option<Resume>,
    /// `-c/--config`, from wherever on the command line it was given.
    pub config: Option<PathBuf>,
    /// The run flags.
    pub args: RunArgs,
}

/// Which session `iota resume` meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// `iota resume` — the interactive picker chooses.
    Pick,
    /// `iota resume <id>` — that id, or any unique prefix of one.
    Id(String),
}

impl Invocation {
    /// `iota [run [<agent>]]`.
    pub fn of_run(cmd: RunCmd, config: Option<PathBuf>) -> Self {
        Self {
            agent: cmd.agent,
            resume: None,
            config,
            args: cmd.args,
        }
    }

    /// `iota resume [<id>]`. A blank id is the picker: it is what the user typed, not a fragment to resolve.
    pub fn of_resume(cmd: ResumeCmd, config: Option<PathBuf>) -> Self {
        let id = cmd
            .id
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        Self {
            agent: None,
            resume: Some(id.map_or(Resume::Pick, Resume::Id)),
            config,
            args: cmd.args,
        }
    }
}
