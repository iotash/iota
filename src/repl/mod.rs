//! The imperative interactive chat loop (chat/run.go and friends) over the
//! [`Ui`](crate::ui::facade::Ui) facade (`TUI_CONTRACTS` §7). Talks ONLY to the facade; never
//! names a terminal crate (ci.sh grep gate: only `ui` may); reuses the provider/tool/MCP/session machinery
//! (`TUI_DESIGN` §8.1) instead of forking the headless loop in `crate::repl::chat`.

pub(crate) mod approval;
pub(crate) mod banner;
/// The agent's candidate set as the `/model` picker sees it (brain page `config-three-layers`).
pub mod catalog;
pub(crate) mod commands;
pub(crate) mod diff;
pub(crate) mod editpicker;
pub(crate) mod errors;
pub(crate) mod group;
pub(crate) mod interact;
pub(crate) mod interrupt;
pub(crate) mod mcpreport;
pub(crate) mod meter;
/// The layered model parameters a `/model` switch re-evaluates (brain page `model-param-layering`).
pub(crate) mod params;
pub(crate) mod phases;
pub(crate) mod replay;
pub(crate) mod retry;
pub mod run;
pub(crate) mod steer;
pub(crate) mod styles;
/// The `/model` surface's read-only System tab (WP54).
pub(crate) mod systemtab;
pub(crate) mod title;
/// The `o200k_base` token counter behind the meter (WP53).
pub(crate) mod tokens;
pub(crate) mod toolloop;
pub(crate) mod transcript;
pub(crate) mod turn;
pub(crate) mod uisink;

pub use catalog::ModelCatalog;

pub(crate) use interact::Interactor;
pub use run::{McpEvent, McpHooks, RunParams, SessionCtx, SessionFactory, run};

// Loop-internal seams, crate-public for tests (TUI_CONTRACTS §7 "frozen for testability").
#[doc(hidden)]
pub use commands::session::session_label;
#[doc(hidden)]
pub use meter::{ContextBudget, CtxMeter};
#[doc(hidden)]
pub use retry::retry_round;
#[doc(hidden)]
pub use title::is_read_only_viewer;

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
