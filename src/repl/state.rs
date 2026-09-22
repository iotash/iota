//! The interactive loop's state in three parts (chat/run.go's `Run` stack, made addressable so the
//! command handlers can live in their own files): the [`Conversation`] — what is being said, to which
//! model, under which parameters; the [`SessionSlot`] — the bundle it is persisted into and the name it
//! carries; the [`UiHandles`] — the facade, the transcript, the presenter and the run's shared handles.
//! `run::Repl` composes the three; a turn runs over the [`Conversation`] alone
//! (`turn::TurnEngine`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agents::Overlay;
use crate::host::Presenter;
use crate::llm::reqlog::RequestLog;
use crate::provider::Provider;
use crate::provider::model::{Attachment, Message};
use crate::session::{SessionStore, SessionWriter};
use crate::sync::lock;
use crate::tool::Dispatcher;
use crate::ui::facade::Ui;

use crate::repl::commands::CommandTable;
use crate::repl::context::meter::{ContextBudget, CtxMeter};
use crate::repl::render::transcript::Transcript;
use crate::repl::run::{McpHooks, SessionFactory};
use crate::repl::title::{SessionTitle, WriterSlot};
use crate::repl::turn::ImagesDir;
use crate::repl::turn::approval::ApprovalGate;

/// What is being said, to which model, under which parameters — the state a turn runs over.
pub(crate) struct Conversation {
    /// The conversation provider.
    pub(crate) provider: Box<dyn Provider>,
    /// The tool dispatcher (LIVE — never cached).
    pub(crate) dispatch: Arc<dyn Dispatcher>,
    /// The conversation.
    pub(crate) history: Vec<Message>,
    /// Attachments riding the next user message.
    pub(crate) pending: Vec<Attachment>,
    /// The context window (and, from WP53, its occupancy).
    pub(crate) budget: ContextBudget,
    /// The token meter (inert for a provider without usage — T-10).
    pub(crate) ctxm: CtxMeter,
    /// Where each of the four layered parameters got the value the chat is running under. The values
    /// themselves are read from the budget and the provider (`crate::repl::liveparams`); this is the only piece
    /// of the layering with nowhere else to live.
    pub(crate) param_sources: crate::session::ParamSources,
    /// What a `/model` model switch re-evaluates those four against.
    pub(crate) layers: crate::config::ParamLayers,
    /// What `/model` offers: the agent's choices and the listers its wildcards need.
    pub(crate) catalog: crate::repl::ModelCatalog,
    /// The auto-compaction snooze watermark: the projected usage at which the user last
    /// said "Not now" (0 = never asked). Cleared by any successful compaction.
    pub(crate) compact_declined: u64,
    /// The built-in harness prompt ahead of every send (`""` for an agent without tools; brain page
    /// `harness-prompt`). Composed once at startup; never part of `history`.
    pub(crate) harness: String,
    /// The agent-mode overlay woven into every send (`None` outside agent mode).
    pub(crate) overlay: Option<Overlay>,
    /// Agent-mode options: the project root and the skills home.
    pub(crate) agent: crate::headless::AgentOptions,
    /// A dedicated image provider bills per attempt (a relay 5xx can arrive AFTER a charged
    /// generation), so its turns are never auto-retried.
    pub(crate) image_provider: bool,
}

/// The bundle the chat is persisted into, and the name it carries.
pub(crate) struct SessionSlot {
    /// The session writer, shared with the title pass (`None` while ephemeral).
    pub(crate) writer: WriterSlot,
    /// The store behind `/session`.
    pub(crate) store: SessionStore,
    /// The agent-mode project bucket the listing is scoped to.
    pub(crate) scope: Option<PathBuf>,
    /// Mints the writer late (`/save`); `Some` = the chat started ephemeral.
    pub(crate) new_session: Option<SessionFactory>,
    /// How much of the history is on disk — never advanced by a failed append, so the next
    /// successful persist retries the backlog.
    pub(crate) persisted: usize,
    /// The session's name across the turn lifecycle.
    pub(crate) titler: Arc<SessionTitle>,
    /// The title pass's own provider instance (the conversation's is mid-call while a turn
    /// streams); `None` for a dedicated image provider, which asked for a title would paint one.
    pub(crate) title_provider: Option<Arc<tokio::sync::Mutex<Box<dyn Provider>>>>,
    /// The in-flight title pass.
    pub(crate) title_task: Option<JoinHandle<()>>,
    /// Where generated images are saved, resolved LAZILY: a bundle materialises on first
    /// use, and an image-less chat must not create one (chat/images.go:115).
    pub(crate) images_dir: ImagesDir,
}

impl SessionSlot {
    /// The live session id, or `""` while the chat is ephemeral.
    pub(crate) fn session_id(&self) -> String {
        self.with_writer(|w| w.id().to_owned())
    }

    /// The live session title, or `""`.
    pub(crate) fn session_title(&self) -> String {
        self.with_writer(|w| w.meta().title.clone())
    }

    /// A path of the live writer (`images_path()`), or `None` while the chat is ephemeral.
    pub(crate) fn with_writer_path(
        &self,
        f: impl FnOnce(&SessionWriter) -> PathBuf,
    ) -> Option<PathBuf> {
        lock(&self.writer).as_ref().map(f)
    }

    fn with_writer(&self, f: impl FnOnce(&SessionWriter) -> String) -> String {
        lock(&self.writer).as_ref().map_or_else(String::new, f)
    }

    /// Waits for an in-flight title pass, so a landed name is written before anything
    /// touches the writer.
    pub(crate) async fn join_title(&mut self) {
        if let Some(h) = self.title_task.take() {
            let _ = h.await;
        }
    }
}

/// The facade, the transcript, the presenter — and the run's shared handles the loop hands around.
pub(crate) struct UiHandles {
    /// The facade.
    pub(crate) ui: Arc<dyn Ui>,
    /// The single writer to the chat area.
    pub(crate) tr: Arc<Transcript>,
    /// The host presenter.
    pub(crate) pres: Arc<Presenter>,
    /// The `/debug` request log (recording toggle + the inspector's rows).
    pub(crate) reqlog: Arc<RequestLog>,
    /// The ONE command table (the completion list and the dispatch chain read it). Shared with the job
    /// registry's watch, which flips the `/jobs` row from a supervisor task; every re-issue of the table
    /// (`ui.set_slash_commands(table.active())`) happens under this lock, so two re-issues from two tasks
    /// cannot cross and leave the composer holding the older one.
    pub(crate) table: Arc<Mutex<CommandTable>>,
    /// The conversation's ONE approval gate: the "allow for this session" grant is one grant
    /// for one person (chat/run.go:172-186).
    pub(crate) gate: Arc<ApprovalGate>,
    /// MCP display hooks.
    pub(crate) mcp: McpHooks,
    /// Whether the terminal background is dark; a host that knows better re-answers it
    /// between turns.
    pub(crate) dark: bool,
    /// Root cancellation (the SIGTERM path).
    pub(crate) cancel: CancellationToken,
    /// The run's background jobs (their notices arrive through the facade's queue, not through here).
    pub(crate) jobs: Arc<crate::shell::jobs::Jobs>,
}
