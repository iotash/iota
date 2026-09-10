//! The command line (cmd/root.go:22-39,418-436): clap derive `Cli` with Go's flag names, shorts and help texts,
//! plus the headless rejection of the interactive-only flags (DIVERGENCES D-23, now `-S/--system-input` and
//! `--no-save` only — `--resume=<id>` is supported, its blank form has its own error, D-41/D-42). Argument
//! errors are clap's own (exit 2, DIVERGENCES D-24); everything else is decided by `run`.

use std::path::PathBuf;

use crate::cmd::resolve::CliError;

/// `iota [provider] [flags]` — the one command (no subcommands). The positional is a built-in provider type or a
/// configured alias; every flag mirrors cmd/root.go, including the three that headless mode rejects at run time.
#[derive(clap::Parser, Debug, Clone)]
#[command(
    name = "iota",
    about = "A lightweight cross-platform AI chat CLI",
    override_usage = "iota [openai|anthropic|gemini|vertexai|openresponses|imagen|images]",
    disable_version_flag = true
)]
pub struct Cli {
    /// Provider type or config alias
    pub provider: Option<String>,
    /// `-k/--key`: the API key (verbatim, even `""`; beats the environment and the config).
    #[arg(short = 'k', long = "key", help = "API key (required)")]
    pub key: Option<String>,
    /// `-u/--url`: the base URL (beats the config).
    #[arg(short = 'u', long = "url", help = "Base URL (optional)")]
    pub url: Option<String>,
    /// `-M/--model`: the model name (required with `-m`).
    #[arg(
        short = 'M',
        long = "model",
        help = "Model name (optional, interactive selection if omitted)"
    )]
    pub model: Option<String>,
    /// `-t/--temperature`: NOT range-checked (parity with Go; only the config value is).
    #[arg(
        short = 't',
        long = "temperature",
        help = "Sampling temperature (0.0-2.0)"
    )]
    pub temperature: Option<f64>,
    /// `-m/--message`: the single message; `-` reads stdin; `""` is `CliError::MessageEmpty` (F-03).
    #[arg(
        short = 'm',
        long = "message",
        help = "Send a single message and print the response (non-interactive, use '-' to read from stdin)"
    )]
    pub message: Option<String>,
    /// `-s/--system`: the system prompt (beats `system:` / `system_file:`).
    #[arg(short = 's', long = "system", help = "System prompt")]
    pub system: Option<String>,
    /// `-S/--system-input`: interactive-only; rejected by `reject_unsupported`.
    #[arg(
        short = 'S',
        long = "system-input",
        help = "Enter system prompt interactively"
    )]
    pub system_input: bool,
    /// `-l/--list`: list configured providers, or the models of the given provider.
    #[arg(
        short = 'l',
        long = "list",
        help = "List configured providers, or models for a given provider"
    )]
    pub list: bool,
    /// `-c/--config`: an explicit config file (the only file read when given).
    #[arg(
        short = 'c',
        long = "config",
        help = "Path to config file (default: ~/.iota.yaml)"
    )]
    pub config: Option<PathBuf>,
    /// `--mcp`: extra MCP servers (command string or URL), in flag order after the config's servers.
    #[arg(
        long = "mcp",
        action = clap::ArgAction::Append,
        help = "MCP server (command string or URL, repeatable)"
    )]
    pub mcp: Vec<String>,
    /// `--resume[=<id>]`: `--resume=<id>` resumes that session headlessly (DIVERGENCES D-41); the BLANK form
    /// (bare `--resume`, which yields `" "`) wants the interactive picker and is rejected by
    /// `reject_unsupported` (D-42).
    ///
    /// `require_equals` is pflag's `NoOptDefVal` argument binding (CONTRACTS S§4.1): Go binds a value only in
    /// the `--resume=<id>` form and leaves a space-separated token as a positional, so `iota --resume abc -m hi`
    /// reads `abc` as the PROVIDER and a bare resume. Without it clap would swallow `abc` as the flag's value.
    #[arg(
        long = "resume",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = " ",
        help = "Resume a saved session: --resume to pick interactively, or --resume=<id>"
    )]
    pub resume: Option<String>,
    /// `--no-save`: interactive-only; rejected by `reject_unsupported`.
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
    /// `--context-window`: parsed (`window::parse_window_size`), no headless effect.
    #[arg(
        long = "context-window",
        help = "Context window size for compaction accounting (e.g. 200k, 1m); default 128k"
    )]
    pub context_window: Option<String>,
    /// `--agent`: agent mode (AGENTS.md + skills system-prompt overlay).
    #[arg(
        long = "agent",
        help = "Enable agent mode (AGENTS.md system-prompt overlay)"
    )]
    pub agent: bool,
}

impl Cli {
    /// `-S/--system-input` and `--no-save` → `Err(CliError::UnsupportedFlag(name))`; a BLANK `--resume` (bare,
    /// i.e. the `" "` sentinel, or `--resume=""`) → `Err(CliError::ResumeNeedsId)` — what is missing headlessly
    /// is the interactive picker, not the flag (DIVERGENCES D-42). A `--resume=<id>` with a non-blank value
    /// passes through to the resume stage of `run` (D-41).
    ///
    /// The check ORDER is Go's (cmd/root.go:284-286, which reports `--no-save` against `--resume`), so
    /// `--no-save --resume=x` still reports `--no-save` and `-S --resume=x` still reports `-S`.
    ///
    /// The function is PURE: it answers "would a headless run accept these flags?". The INTERACTIVE LIFT
    /// (`TUI_CONTRACTS` §11) lives at the ONE call site instead — [`crate::cmd::run`] asks only for a run that
    /// carries `-m`, the flag that decides Go's branch at root.go:259; `-l` and an interactive run take the
    /// flags at face value, exactly as Go does.
    pub fn reject_unsupported(&self) -> Result<(), CliError> {
        if self.system_input {
            return Err(CliError::UnsupportedFlag("-S/--system-input"));
        }
        if self.no_save {
            return Err(CliError::UnsupportedFlag("--no-save"));
        }
        if let Some(v) = &self.resume
            && v.trim().is_empty()
        {
            return Err(CliError::ResumeNeedsId);
        }
        Ok(())
    }
}
