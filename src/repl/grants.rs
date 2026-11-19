//! The grants' door-side service (docs/design/grants.md §5, §9 Q10): what every door of the chat — the
//! approval dialog, `/allow`, `/save`, `/new` and `/session` — and the start of every mode do to
//! the ONE store (`tool::grants::Grants`) and to where its two levels are kept.
//!
//! A change goes through `Door::apply`: the session level changes in memory and is then written into the
//! bundle's meta (`SessionWriter::update_meta`; an ephemeral chat keeps it in memory until `/save` stamps it);
//! the project level is written to the project's `grants.json` FIRST ([`ProjectFile`]) and only then takes
//! effect — `--no-save` included: it is not conversation. Entering a conversation — a start, a resume, `/new`,
//! a `/session` switch — is `ProjectFile::enter`: its session grants replace the store's, the project's are
//! read afresh. The project file is built ONCE, at assembly, keyed by the root the tools read paths against
//! (`ToolEnv::root`), and handed to the chat with the static write roots ([`Footing`]).

use std::path::PathBuf;
use std::sync::Arc;

use crate::mcp::config::parse_mcp_flag;
use crate::repl::title::WriterSlot;
use crate::session::{SessionError, SessionStore};
use crate::sync::lock;
use crate::tool::grants::{GrantSet, Grants, Level, ServerId, ToolKey};

/// Where the project level is kept: the store's `grants.json` for the project root the tools read paths
/// against (`ToolEnv::root`), in every mode.
#[derive(Clone, Debug)]
pub struct ProjectFile {
    /// The sessions root the project's bucket is in.
    pub store: SessionStore,
    /// The project root.
    pub root: PathBuf,
}

impl ProjectFile {
    /// Replaces the project level of `grants` with what the file holds now. A file that cannot be read grants
    /// nothing: the level is emptied, and the warning to show is returned (without its `⚠`).
    pub(crate) fn load(&self, grants: &Grants) -> Option<String> {
        match self.store.project_grants(&self.root) {
            Ok(set) => {
                grants.replace_project(set);
                None
            }
            Err(e) => {
                grants.replace_project(GrantSet::default());
                Some(format!("grants: {e}; no project grants apply"))
            }
        }
    }

    /// Enters a conversation: its `session` grants REPLACE the store's — a merge would carry one conversation's
    /// approvals into another — and the project's are read afresh: they follow the project, and another
    /// process may have changed them.
    pub(crate) fn enter(&self, grants: &Grants, session: GrantSet) -> Entered {
        grants.replace_session(session);
        let warning = self.load(grants);
        Entered {
            warning,
            count: summary(grants),
        }
    }

    /// Changes the project level, the file FIRST (read–modify–write) and then memory, which takes what the file
    /// now holds — a grant another process made meanwhile included. Nothing changes when the file cannot be
    /// read or written.
    fn update(
        &self,
        grants: &Grants,
        change: impl FnOnce(&mut GrantSet),
    ) -> Result<(), SessionError> {
        let set = self.store.update_project_grants(&self.root, change)?;
        grants.replace_project(set);
        Ok(())
    }
}

/// What entering a conversation found ([`ProjectFile::enter`]).
pub(crate) struct Entered {
    /// The project file could not be read: why, without its `⚠`.
    pub(crate) warning: Option<String>,
    /// How many grants each level now holds ([`counted`]); `None` when neither holds any.
    pub(crate) count: Option<String>,
}

impl Entered {
    /// The line a `-m` start says it with ([`loaded`]).
    pub(crate) fn line(&self) -> Option<String> {
        self.count.as_deref().map(loaded)
    }

    /// The line a chat says it with ([`loaded_in_chat`]).
    pub(crate) fn line_in_chat(&self) -> Option<String> {
        self.count.as_deref().map(loaded_in_chat)
    }
}

/// What a chat's grant doors stand on, resolved ONCE at assembly beside the project load (`cmd`): the project's
/// file and the static write roots, kept apart by where they were declared so `/allow` can say which one already
/// covers a directory.
#[derive(Clone, Debug)]
pub struct Footing {
    /// The project's grants file; its root is what a typed relative directory hangs off.
    pub project: ProjectFile,
    /// The top-level `write_roots:`, resolved (`ToolEnv::write_roots`).
    pub write_roots: Vec<PathBuf>,
    /// The `--add-dir` values, resolved (`ToolEnv::added_dirs`).
    pub added_dirs: Vec<PathBuf>,
}

impl Footing {
    /// Both static lists, `write_roots:` first — as the tools hold them (`ToolEnv::static_roots`).
    pub(crate) fn static_roots(&self) -> Vec<PathBuf> {
        [self.write_roots.as_slice(), self.added_dirs.as_slice()].concat()
    }
}

/// The chat's door-side service: the store, the session's bundle and the project's file — the one path from a
/// door to a grant that lasts.
#[derive(Clone)]
pub(crate) struct Door {
    /// The run's grant store.
    pub(crate) grants: Arc<Grants>,
    /// The session's bundle (`None` inside while ephemeral): the session level is written into its meta.
    pub(crate) writer: WriterSlot,
    /// The project's grants file.
    pub(crate) project: ProjectFile,
}

/// What [`Door::apply`] could not keep.
#[derive(Debug)]
pub(crate) enum Unsaved {
    /// The session level changed, but the bundle's meta could not be written ([`unsaved_session`]).
    Session(SessionError),
    /// The project's file could not be read or written: nothing changed.
    Project(SessionError),
}

impl Door {
    /// Changes `level` through `change` and keeps it: the session level in memory, then into the bundle's meta
    /// (under its single-writer lock; a pending bundle keeps it for its first write, an ephemeral chat has
    /// none); the project level into its file FIRST, then memory.
    pub(crate) fn apply(
        &self,
        level: Level,
        change: impl FnOnce(&mut GrantSet),
    ) -> Result<(), Unsaved> {
        match level {
            Level::Session => {
                self.grants.update(Level::Session, change);
                let set = self.grants.snapshot(Level::Session);
                lock(&self.writer)
                    .as_mut()
                    .map_or(Ok(()), |w| w.update_meta(|m| m.grants = set))
                    .map_err(Unsaved::Session)
            }
            Level::Project => self
                .project
                .update(&self.grants, change)
                .map_err(Unsaved::Project),
        }
    }
}

/// A session level that changed but could not be written, as every failed meta write is said.
pub(crate) fn unsaved_session(e: &SessionError) -> String {
    format!("Warning: failed to save session: {e}")
}

/// How many grants each level holds, as `/status` and the start's line say it — `2 for this session, 3 for this
/// project`, a level with none left out: `None` when neither holds any.
pub(crate) fn counted(session: usize, project: usize) -> Option<String> {
    let parts: Vec<String> = [(session, "this session"), (project, "this project")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, level)| format!("{n} for {level}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// [`counted`] over the store.
pub(crate) fn summary(grants: &Grants) -> Option<String> {
    counted(
        grants.snapshot(Level::Session).len(),
        grants.snapshot(Level::Project).len(),
    )
}

/// The line a `-m` start prints for the grants it loaded, `count` being [`counted`]'s.
pub(crate) fn loaded(count: &str) -> String {
    format!("grants: {count}")
}

/// [`loaded`] as a chat says it: with where to look.
pub(crate) fn loaded_in_chat(count: &str) -> String {
    format!("{} (/allow lists them)", loaded(count))
}

/// The name a server's identity is typed as: an entry's name; a `--mcp` server's `argv[0]` (or URL), as it is
/// named in `/tools`.
pub(crate) fn server_name(id: &ServerId) -> String {
    match id {
        ServerId::Config(name) => name.clone(),
        ServerId::Flag(spec) => parse_mcp_flag(spec).map_or_else(|_| spec.clone(), |c| c.name),
    }
}

/// A tool grant as the dialog and the notices name it: a built-in by its name, an MCP tool as
/// `<server>:<tool>` (`<server>:*` for every tool).
pub(crate) fn label(key: &ToolKey) -> String {
    match key {
        ToolKey::Builtin(name) => name.clone(),
        ToolKey::Mcp { server, tool } => format!("{}:{}", server_name(server), tool.as_str()),
    }
}
