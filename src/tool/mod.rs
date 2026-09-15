//! Tool and dispatcher contracts (tool/tool.go:33-330): `ToolOutput`, `Presentation`, `DeferState`, the `Tool`
//! and `Dispatcher` traits with their optional capabilities, the `PrefixOf` oracle and the toolset `Env` —
//! plus, in the submodules, the run context every call takes (`context`), the tool framework (`registry`,
//! `merge`, `defer`, `defer_mode`, `yaml11`, `args`, `sets`) and the four built-in sets (`shell`, `code`,
//! `agent`, and `ask`, which contributes tools only when the `Env` carries an interactor).

use std::{path::PathBuf, sync::Arc};

pub mod agent;
pub(crate) mod args;
pub mod ask;
pub mod code;
pub mod context;
pub mod defer;
pub(crate) mod defer_mode;
pub mod error;
pub mod fmt;
pub mod merge;
pub(crate) mod registry;
pub mod sets;
pub mod shell;
pub(crate) mod yaml11;

pub use defer::{DeferredGroup, SEARCH_TOOL_NAME, defer};
pub use defer_mode::DeferMode;
pub use merge::merge;
pub use registry::{Registry, set_disabled};

use crate::BoxFuture;
use crate::app::HostDirs;
use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use error::ToolError;

/// Model-facing result of a tool call: text plus whether it is an error the model should see.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolOutput {
    /// Result text.
    pub text: String,
    /// Whether the call failed (surfaced to the model, not a hard error).
    pub is_error: bool,
}

impl ToolOutput {
    /// A successful result.
    pub fn ok(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            is_error: false,
        }
    }

    /// A failed result (`is_error = true`).
    pub fn err(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            is_error: true,
        }
    }
}

/// Outcome of a tool call: a model-facing `ToolOutput`, or a hard `ToolError`.
pub type ToolResult = Result<ToolOutput, ToolError>;

/// Prefix of a hard tool failure as the model sees it (chat.go:367, parallel.go:119).
pub const TOOL_ERROR_PREFIX: &str = "Error calling tool: ";

/// What the model reads back from one call: the output's text and error flag, or — when the call
/// failed outright — the error under [`TOOL_ERROR_PREFIX`], flagged as an error.
pub fn model_text(result: ToolResult) -> (String, bool) {
    match result {
        Ok(out) => (out.text, out.is_error),
        Err(e) => (format!("{TOOL_ERROR_PREFIX}{e}"), true),
    }
}

/// How a tool call's lifecycle is displayed (interactive-only; headless keeps the defaults).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    /// Folded into the activity panel — the default.
    #[default]
    Group,
    /// The call puts its own surface in front of the user.
    Surface,
    /// The outcome deserves a standalone, expanded block.
    Expanded,
}

/// State of a deferred tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferState {
    /// Hidden until searched.
    Deferred,
    /// Loaded into the advertised set.
    Loaded,
    /// Deferred by the provider protocol (reference / tool-search).
    DeferredProtocol,
}

impl DeferState {
    /// `"deferred"` | `"loaded"` | `"deferred (protocol)"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Loaded => "loaded",
            Self::DeferredProtocol => "deferred (protocol)",
        }
    }
}

impl std::fmt::Display for DeferState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One deferred tool as reported by `Dispatcher::deferred_tools`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredToolStatus {
    /// Wire name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Defer group (e.g. the MCP server name).
    pub group: String,
    /// Current state.
    pub state: DeferState,
}

/// A single built-in tool.
pub trait Tool: Send + Sync {
    /// The definition advertised to the model.
    fn def(&self) -> ToolDef;
    /// Executes one call.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult>;
    /// Whether each call needs interactive approval (a set may report false when configured to auto-approve).
    fn requires_approval(&self) -> bool {
        false
    }
    /// Presentation class of the tool's calls.
    fn presentation(&self) -> Presentation {
        Presentation::Group
    }
    /// Per-CALL answer; `None` = Go nil args (`readOnlyRegistry` probe).
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        false
    }
    /// The tool's own header text for a call, verbatim (an empty one is a bare `[name]`);
    /// `None` = the tool writes none, and the caller falls back to the argument digest.
    fn header_summary(&self, _args: &JsonObject) -> Option<String> {
        None
    }
}

/// A dispatcher that can vouch for a name without a `tools()` scan — including tools it hides
/// (the deferred set), which is why [`merge()`] asks before scanning.
pub trait Owner {
    /// Whether `name` is this dispatcher's to call.
    fn owns(&self, name: &str) -> bool;
}

/// `defer_mode: tool-search`: ranks the deferred tools for a query.
pub trait ToolSearcher {
    /// The best matches for `query`, best first; empty when nothing matches.
    fn search_tools(&self, query: &str) -> Vec<ToolDef>;
}

/// The surface the chat loop uses to advertise tools and execute the model's calls.
pub trait Dispatcher: Send + Sync {
    /// LIVE view: wrappers never cache.
    fn tools(&self) -> Vec<ToolDef>;
    /// Executes a tool call.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult>;
    /// Whether the named tool's calls need interactive approval.
    fn requires_approval(&self, _name: &str) -> bool {
        false
    }
    /// Presentation class of the named tool.
    fn presentation(&self, _name: &str) -> Presentation {
        Presentation::Group
    }
    /// Whether this CALL may run concurrently with the round's other calls.
    fn supports_parallel(&self, _name: &str, _args: Option<&JsonObject>) -> bool {
        false
    }
    /// The named tool's own header text, verbatim; `None` = it writes none (the caller falls
    /// back to the argument digest).
    fn header_summary(&self, _name: &str, _args: &JsonObject) -> Option<String> {
        None
    }
    /// The ownership oracle, when this dispatcher has one (`merge` scans `tools()` otherwise).
    fn as_owner(&self) -> Option<&dyn Owner> {
        None
    }
    /// The tool searcher, when this dispatcher has one (`merge` forwards to the FIRST part that does).
    fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher> {
        None
    }
    /// The deferred tools and their states.
    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        Vec::new()
    }
    /// Drains the tools loaded since the last call (system-tools mount).
    fn take_pending_loads(&self) -> Vec<ToolDef> {
        Vec::new()
    }
}

/// Deferred-group prefix oracle: `"mcp__<segment>__"` once the named server connected, `""` before; queried lazily
/// per call. Produced by `crate::mcp::Manager::prefix_of`, consumed by `crate::tool::defer`.
pub type PrefixOf = Arc<dyn Fn(&str) -> String + Send + Sync>;

// ---- the ask seam (tool/tool.go:290-326 shapes; TUI_CONTRACTS §4) ----

/// One offered answer of an ask question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskOption {
    /// Row label.
    pub label: String,
    /// Dim description column.
    pub description: String,
}

/// One question of an ask wizard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskQuestion {
    /// Chip/tab header.
    pub header: String,
    /// The question text (the panel prompt).
    pub question: String,
    /// The offered answers.
    pub options: Vec<AskOption>,
    /// Whether several answers may be selected.
    pub multiple: bool,
    /// Whether a free-form `"Other…"` answer is offered.
    pub allow_custom: bool,
}

/// An ask-tool request: 1..=4 questions (the tool layer enforces the bound).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskSpec {
    /// The questions, in wizard order.
    pub questions: Vec<AskQuestion>,
}

/// One answered question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskAnswer {
    /// The selected option labels.
    pub selected: Vec<String>,
    /// The free-form answer when `"Other…"` was taken; `""` otherwise.
    pub custom: String,
}

/// Result of one ask wizard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskResult {
    /// Whether the user declined (cancel, or no interactor bound).
    pub declined: bool,
    /// Per-question answers (empty when declined).
    pub answers: Vec<AskAnswer>,
}

/// The interactive question seam the ask toolset calls into (implemented by
/// `crate::repl::Interactor` over the ui facade; absent headlessly).
pub trait Interactor: Send + Sync {
    /// Runs one ask wizard; an unbound or cancelled wizard resolves declined.
    fn ask<'a>(&'a self, cx: &'a RunCtx, spec: AskSpec) -> BoxFuture<'a, AskResult>;
}

// ---- the artifact side channel (tool/tool.go:160-198; the D-19 lift, T-35) ----

/// A call's display payload: for the USER's eyes only, never in the model-facing result
/// text (a diff there would cost tokens). Rendered by kind — `Diff` feeds the showcase's
/// diff renderer, `Note` feeds the classic finish-call event-row note (transcript.go).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// How the payload renders.
    pub kind: ArtifactKind,
    /// Display title (e.g. the edited file's display path).
    pub title: String,
    /// The payload rows.
    pub lines: Vec<String>,
}

/// Go used the strings `"diff"`/`"note"`; a closed enum — same two producers, typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// Unified-hunk diff rows (`edit_file`/`write_file` postDiff, code.go:665-675).
    Diff,
    /// Accounting note rows (transcript.go's finish-call note).
    Note,
}

/// `PostArtifact` twin: a no-op when `cx.artifact` is `None` (headless loops, tests — Go
/// parity).
pub(crate) fn post_artifact(cx: &RunCtx, a: Artifact) {
    if let Some(slot) = &cx.artifact {
        slot.post(a);
    }
}

/// Host seams a toolset factory receives (tool/tool.go:202-231).
#[derive(Clone, Default)]
pub struct Env {
    /// `agents::project_root(cwd)` in every mode; None only in tests.
    pub project_root: Option<PathBuf>,
    /// Process-level directories.
    pub dirs: HostDirs,
    /// The run's background-job registry, which `shell` starts `background: true` calls in. Bound by both
    /// entry points; None only in tests, where a `background` call is refused rather than silently run in
    /// the foreground.
    pub jobs: Option<Arc<crate::shell::jobs::Jobs>>,
    /// Some only interactively: `new_ask_set` contributes the ask tools when it is bound
    /// (headless stays empty, tool/ask.go parity). `Env` is built with `..Env::default()`
    /// literals across the workspace, so this field lands non-breaking (`TUI_CONTRACTS` §4).
    pub interactor: Option<Arc<dyn Interactor>>,
}

impl Env {
    /// `project_root`, else `dirs.cwd`, else `std::env::current_dir()`; then `std::path::absolute` (cleaned, like
    /// Go's `filepath.Abs`).
    pub fn root(&self) -> std::io::Result<PathBuf> {
        let root = match (&self.project_root, &self.dirs.cwd) {
            (Some(p), _) => p.clone(),
            (None, Some(c)) => c.clone(),
            (None, None) => std::env::current_dir()?,
        };
        std::path::absolute(root).map(|p| crate::app::paths::clean(&p))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DeferState, Env, Presentation, ToolOutput};
    use crate::app::HostDirs;

    #[test]
    fn env_root_precedence() {
        let both = Env {
            project_root: Some(PathBuf::from("/proj/./x/..")),
            dirs: HostDirs {
                cwd: Some(PathBuf::from("/cwd")),
                ..HostDirs::default()
            },
            ..Env::default()
        };
        // `root()` runs its answer through `std::path::absolute`, which on Windows turns a rooted
        // `/proj` into a drive-qualified one — so the expectation is spelled by the same call.
        let abs = |p: &str| std::path::absolute(p).expect("absolute");
        assert_eq!(both.root().expect("root"), abs("/proj"));
        let cwd_only = Env {
            project_root: None,
            dirs: HostDirs {
                cwd: Some(PathBuf::from("/cwd/")),
                ..HostDirs::default()
            },
            ..Env::default()
        };
        assert_eq!(cwd_only.root().expect("root"), abs("/cwd"));
        let neither = Env::default();
        let got = neither.root().expect("process cwd");
        assert!(got.is_absolute());
        assert_eq!(got, std::env::current_dir().expect("cwd"));
        // A relative project root is made absolute against the process cwd.
        let relative = Env {
            project_root: Some(PathBuf::from("sub")),
            ..Env::default()
        };
        assert_eq!(
            relative.root().expect("root"),
            std::env::current_dir().expect("cwd").join("sub")
        );
    }

    #[test]
    fn small_types() {
        assert_eq!(
            ToolOutput::ok("x"),
            ToolOutput {
                text: "x".to_owned(),
                is_error: false
            }
        );
        assert_eq!(
            ToolOutput::err("y"),
            ToolOutput {
                text: "y".to_owned(),
                is_error: true
            }
        );
        assert_eq!(Presentation::default(), Presentation::Group);
        assert_eq!(DeferState::Deferred.to_string(), "deferred");
        assert_eq!(DeferState::Loaded.to_string(), "loaded");
        assert_eq!(
            DeferState::DeferredProtocol.to_string(),
            "deferred (protocol)"
        );
    }

    // Go: tool/ask_test.go:85 TestArtifactSideChannel — the artifact side channel (tool/tool.go:160-198):
    // a call posts its display payload into the injected slot (last wins); without an injection
    // `post_artifact` is a silent no-op (the D-19 lift, T-35).
    #[test]
    fn test_post_artifact_headless_no_op() {
        use crate::tool::context::{ArtifactSlot, RunCtx};

        use super::{Artifact, ArtifactKind, post_artifact};

        let a = Artifact {
            kind: ArtifactKind::Diff,
            title: "f.txt".to_owned(),
            lines: vec!["+1".to_owned()],
        };
        // Headless: no slot in the context — the post is a silent no-op (Go parity).
        let headless = RunCtx::default();
        post_artifact(&headless, a.clone());
        // Interactive: a fresh slot injected per call receives the post; a clone of the
        // context posts into the SAME slot.
        let slot = ArtifactSlot::default();
        let cx = RunCtx {
            artifact: Some(slot.clone()),
            ..RunCtx::default()
        };
        post_artifact(&cx.clone(), a.clone());
        assert_eq!(slot.take(), Some(a));
        assert!(slot.take().is_none());
    }
}
