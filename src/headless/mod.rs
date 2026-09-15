//! The headless (non-interactive) run loop (chat/chat.go, chat/output.go, chat/parallel.go, chat/images.go,
//! chat/agentmode.go). The run context every loop shares (chat/turns.go) is `crate::tool::context`.
//! The interactive loop over the same machinery is `crate::repl`.
//!
//! `once` is the single entry point the binary calls: it builds the run context (`RunCtx` with the run's
//! `TurnBudget`), runs one message through `run_once` (unary chat or the tool loop `execute_with_tools`),
//! saves generated images, prints either the bare reply or the JSON `RunReport`, and hands back the turn's
//! message delta for a caller that persists a session.

pub mod batch;
pub(crate) mod error;
pub mod images;
pub mod once;
pub mod report;
pub mod run;

use std::path::PathBuf;

pub use error::ChatError;
pub use once::{OnceOptions, OnceOutcome, once};
pub use report::{RoundReport, RunRecorder, RunReport, TokenUsage, write_report};
pub use run::{
    LoopOutcome, QuietHost, RunOutcome, RunRequest, TurnParams, execute_with_tools,
    install_tool_searcher, run_once,
};

/// `--output-format`: the bare reply (`text`) or the JSON run report (`json`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// `text` — the reply, image lines and image errors, nothing else (the default).
    #[default]
    Text,
    /// `json` — one pretty-printed `RunReport` object, written on success AND on failure.
    Json,
}

impl OutputFormat {
    /// The flag spelling: `"text"` | `"json"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// trim; "" | "text" → Text; "json" → Json; else `ChatError::BadFormat(ORIGINAL untrimmed s)`
/// (chat/output.go:44-53).
pub fn parse_output_format(s: &str) -> Result<OutputFormat, ChatError> {
    match s.trim() {
        "" | "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        _ => Err(ChatError::BadFormat(s.to_owned())),
    }
}

/// chat/agentmode.go + injected cwd/home.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentOptions {
    /// Whether agent mode is on (`agents.<name>.workspace: true`): compose the AGENTS.md + skills overlay.
    pub enabled: bool,
    /// The project root the overlay chain starts from.
    pub root: PathBuf,
    /// The working directory the chain ends at; `None` → `root`.
    pub cwd: Option<PathBuf>,
    /// The user's home directory (user-level skill roots); `None` → no home-level skills.
    pub home: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::{OutputFormat, parse_output_format};

    #[test]
    fn output_format_spellings() {
        assert_eq!(OutputFormat::Text.as_str(), "text");
        assert_eq!(OutputFormat::Json.as_str(), "json");
        assert_eq!(OutputFormat::default(), OutputFormat::Text);
        // The error quotes the ORIGINAL, untrimmed input (output.go:51).
        assert_eq!(
            parse_output_format(" yaml ").unwrap_err().to_string(),
            "unknown output format \" yaml \" (want text or json)"
        );
    }
}
