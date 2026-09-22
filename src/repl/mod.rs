//! The imperative interactive chat loop (chat/run.go and friends) over the
//! [`Ui`](crate::ui::facade::Ui) facade (`TUI_CONTRACTS` §7). Talks ONLY to the facade; never
//! names a terminal crate (ci.sh grep gate: only `ui` may); reuses the provider/tool/MCP/session machinery
//! (`TUI_DESIGN` §8.1) instead of forking the headless loop in `crate::repl::chat`.

/// The agent's choices as the `/model` picker sees them (brain page `config-three-layers`).
pub mod catalog;
pub(crate) mod commands;
/// The context meter and the token counter behind it (WP53).
pub(crate) mod context;
pub(crate) mod editpicker;
pub(crate) mod errors;
/// The layered model parameters as the chat runs them: read, re-evaluated on a `/model` switch, written
/// back (brain page `model-param-layering`).
pub(crate) mod liveparams;
/// What the loop draws.
pub(crate) mod render;
pub mod run;
/// The loop's state in three parts: the conversation, the session slot, the UI handles.
pub(crate) mod state;
/// The `/model` surface's read-only System tab (WP54).
pub(crate) mod systemtab;
pub(crate) mod title;
/// One turn: the tool loop, retries, phases, steering, interrupts and the approval gate.
pub(crate) mod turn;

pub use catalog::ModelCatalog;

pub use run::{McpEvent, McpHooks, RunParams, SessionCtx, SessionFactory, run};
pub(crate) use turn::interact::Interactor;

// Loop internals the command layer reads.
pub(crate) use commands::session::session_label;

/// Why the interactive loop failed (loop errors never exit — only these do).
#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    /// The facade failed (closed under the loop).
    #[error(transparent)]
    Ui(#[from] crate::ui::facade::UiError),
    /// The session store failed on a loop-fatal path.
    #[error(transparent)]
    Session(#[from] crate::session::SessionError),
    /// Terminal/loop I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
