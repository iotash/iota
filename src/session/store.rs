//! The sessions root and everything addressed through it (chat/session.go:154-305, :335-402, :904-1028).
//!
//! Bundles live in two layouts: flat `<root>/<id>/` (normal mode) and `<root>/projects/<slug>/<id>/`
//! (agent mode, where `<slug>` encodes the project root). The DISPLAY views are mode-isolated; only
//! `iota resume` id resolution ever merges them.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use crate::app::HostDirs;
use crate::app::{fs, paths};
use crate::provider::ProviderKind;
use crate::provider::model::Message;

use crate::session::bot::{BOT_POINTER_FILE, BotPointer};
use crate::session::error::SessionError;
use crate::session::id::{generate_id, resolve_in};
use crate::session::loader::{Session, load_full_history, load_log, repair_tail};
use crate::session::lock::{lock_bot, lock_bundle};
use crate::session::meta::{
    META_FILE, SESSION_SCHEMA_VERSION, SessionMeta, now_rfc3339, parse_rfc3339,
};
use crate::session::record::LOG_FILE;
use crate::session::writer::{SessionWriter, open_append_0644};
use crate::tool::grants::GrantSet;

/// The `sessionsDir` subdirectory holding project-scoped buckets (chat/session.go:167).
pub const PROJECTS_DIR_NAME: &str = "projects";

/// The project grants' file in a bucket (docs/design/grants.md §4.2). A file, never a directory: every session
/// listing reads subdirectories only, and the locator wants `<id>/meta.json`, so no view ever sees it.
pub const GRANTS_FILE: &str = "grants.json";

/// The one version of [`GRANTS_FILE`] this build reads and writes.
const GRANTS_VERSION: u32 = 1;

/// [`GRANTS_FILE`] on disk: one entry per REAL project root, because the bucket's slug is lossy — two roots
/// that collide on it share the file and nothing else.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct GrantsFile {
    v: u32,
    #[serde(default)]
    roots: BTreeMap<PathBuf, GrantSet>,
}

/// A lightweight session summary (chat/session.go:132-143) minus the picker-only `Project` hint.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    /// The session id.
    pub id: String,
    /// The generated title (may be empty).
    pub title: String,
    /// The model recorded in meta.
    pub model: String,
    /// `meta.provider`: the provider name — or, for a v1 bundle the listing still shows but no resume reads,
    /// a type.
    pub provider: String,
    /// `meta.updated_at`; `None` when unparsable — Go's zero time, which sorts last.
    pub updated_at: Option<jiff::Timestamp>,
    /// `meta.message_count`.
    pub message_count: i64,
}

/// What a new bundle records at creation: the provider and model it runs under, the working directory it
/// belongs to, and the agent. `NewSession::new(kind, model)` is the bare bundle every other field defaulted —
/// on the built-in provider of that type, no temperature, no cwd, flat (not project-scoped), no agent.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSession {
    /// The provider type the bundle's raw payloads are written under.
    pub kind: ProviderKind,
    /// The provider name recorded in meta ([`SessionMeta::provider`]).
    pub provider: String,
    /// The model id recorded in meta.
    pub model: String,
    /// The temperature recorded in meta (`None` omits the key).
    pub temperature: Option<f64>,
    /// The working directory recorded in meta (`""` omits the key), and — with `project` — the bucket.
    pub cwd: String,
    /// Project-scoped: `cwd` is non-empty and the bundle lives in `projects/<slug(cwd)>/<id>`.
    pub project: bool,
    /// The `agents:` entry the run was under (`""` = none).
    pub agent: String,
}

impl NewSession {
    /// The bare bundle: `kind` and `model`, everything else at its default.
    pub fn new(kind: ProviderKind, model: &str) -> Self {
        Self {
            kind,
            provider: kind.as_str().to_owned(),
            model: model.to_owned(),
            temperature: None,
            cwd: String::new(),
            project: false,
            agent: String::new(),
        }
    }
}

/// What [`SessionStore::open_bot`] found (docs/design/bot-mode.md §2.2).
#[derive(Debug)]
pub enum BotOpen {
    /// The bot's session, resumed (the view boxed: it is the large half).
    Resumed(SessionWriter, Box<Session>),
    /// A new session: its bundle is already on disk and the pointer names it.
    Fresh(SessionWriter),
}

/// The sessions root (`<home>/.iota/sessions`) as a value. Constructed from a path in tests and from
/// [`HostDirs`] in the binary — it NEVER reads the process environment.
#[derive(Clone, Debug)]
pub struct SessionStore {
    root: PathBuf,
    /// `<app home>/bots` — the pointers that protect bot sessions (§2.7). `None` for a store built from a
    /// bare path, which knows of no bots.
    bots: Option<PathBuf>,
    /// `<app home>/images` — where a chat with no bundle yet keeps the pictures it generates. `None` for a
    /// store built from a bare path.
    images: Option<PathBuf>,
}

impl SessionStore {
    /// The sessions root directory verbatim (tests pass a temp dir).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            bots: None,
            images: None,
        }
    }

    /// The same store, protecting the sessions the pointers under `bots` name.
    #[must_use]
    pub fn with_bots(mut self, bots: impl Into<PathBuf>) -> Self {
        self.bots = Some(bots.into());
        self
    }

    /// The same store, with the directory a chat without a bundle saves its pictures in.
    #[must_use]
    pub fn with_images_dir(mut self, images: impl Into<PathBuf>) -> Self {
        self.images = Some(images.into());
        self
    }

    /// Where a chat without a bundle saves its pictures: an ephemeral chat has no bundle of its own, and
    /// the picture is already paid for. The same directory a stateless `-m` run uses.
    pub fn images_dir(&self) -> Option<&Path> {
        self.images.as_deref()
    }

    /// `<app home>/sessions` (chat/session.go:156-162), aware of `<app home>/bots`;
    /// [`SessionError::HomeNotDefined`] when the host has no home directory.
    pub fn from_dirs(dirs: &HostDirs) -> Result<Self, SessionError> {
        let (Some(sessions), Some(bots)) = (dirs.sessions_dir(), dirs.bots_dir()) else {
            return Err(SessionError::HomeNotDefined);
        };
        let store = Self::new(sessions).with_bots(bots);
        Ok(match dirs.images_dir() {
            Some(images) => store.with_images_dir(images),
            None => store,
        })
    }

    /// The bots root, when this store knows one.
    pub fn bots_dir(&self) -> Option<&Path> {
        self.bots.as_deref()
    }

    /// The bot whose pointer names session `id` (§2.7): a scan of `<bots>/*/bot.json`, O(bots). `None` for
    /// an id no bot points at, and always for a store that knows no bots root. A pointer that cannot be read
    /// is [`SessionError::BotOwnerUnknown`]: it may name `id`.
    pub fn bot_owner(&self, id: &str) -> Result<Option<String>, SessionError> {
        let Some(bots) = self.bots.as_deref() else {
            return Ok(None);
        };
        let pointers = crate::session::bot::pointers(bots).map_err(|(path, e)| {
            SessionError::BotOwnerUnknown {
                id: id.to_owned(),
                path,
                source: e,
            }
        })?;
        Ok(pointers
            .into_iter()
            .find(|(_, p)| p.session == id)
            .map(|(name, _)| name))
    }

    /// Every session id some bot points at — what a normal-mode picker leaves out (§2.7). Only a listing:
    /// the pointers that cannot be read are left out here, and the gate
    /// ([`check_not_bot_owned`](Self::check_not_bot_owned)) refuses what they might name.
    pub fn bot_sessions(&self) -> Vec<String> {
        let Some(bots) = self.bots.as_deref() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(bots) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| BotPointer::read(&e.path()).ok().flatten())
            .map(|p| p.session)
            .collect()
    }

    /// Refuses a session a bot owns with [`SessionError::BotOwned`], and every session while a pointer
    /// cannot be read ([`SessionError::BotOwnerUnknown`]) — the gate `iota resume <id>`, `/session` and
    /// [`delete`](Self::delete) pass through.
    pub fn check_not_bot_owned(&self, id: &str) -> Result<(), SessionError> {
        match self.bot_owner(id)? {
            Some(bot) => Err(SessionError::BotOwned {
                id: id.to_owned(),
                bot,
            }),
            None => Ok(()),
        }
    }

    /// The sessions root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `projectSlug` (chat/session.go:169-174): the CLEANED root with every path separator replaced by
    /// `'-'`, Claude Code style — `/Users/x/proj` → `-Users-x-proj`, `/` → `-`.
    ///
    /// On Windows the separator is not the only character in the way. A root there opens with a DRIVE
    /// (`C:\Users\x\proj`), and `:` — like `* ? " < > |`, and like the `\\?\` prefix `fs::canonicalize`
    /// hands back — may not appear in a directory name at all: `create_dir_all` refuses the bucket with
    /// `ERROR_INVALID_NAME` (123) and `resume --workspace` has no bucket to write to. Every one of them
    /// folds into the same `'-'`, so the example above is spelled `C--Users-x-proj` (again Claude Code's
    /// own). Unix is untouched: `:` is a legal file-name character there and the Go rule stands byte for
    /// byte.
    pub fn project_slug(root: &Path) -> String {
        let cleaned = paths::clean(root);
        let cleaned = cleaned.to_string_lossy();
        if cfg!(windows) {
            cleaned.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "-")
        } else {
            cleaned.replace(std::path::MAIN_SEPARATOR, "-")
        }
    }

    /// `findSessionDir` (chat/session.go:181-196): the flat `<root>/<id>` first, then every
    /// `projects/<bucket>/<id>`. A bundle is recognised ONLY by containing `meta.json`, which is what
    /// keeps the `projects/` container (or any stray directory) from masquerading as a session.
    pub fn find_dir(&self, id: &str) -> Option<PathBuf> {
        let mut candidates = vec![self.root.join(id)];
        for bucket in self.buckets() {
            candidates.push(bucket.join(id));
        }
        candidates
            .into_iter()
            .find(|dir| dir.join(META_FILE).exists())
    }

    /// `sessionDir` (chat/session.go:200-209): [`find_dir`](Self::find_dir) or
    /// [`SessionError::NotFound`].
    pub fn dir(&self, id: &str) -> Result<PathBuf, SessionError> {
        self.find_dir(id)
            .ok_or_else(|| SessionError::NotFound(id.to_owned()))
    }

    /// `sessionIDTaken` (chat/session.go:235-252): any entry with this name, flat or in any bucket —
    /// `meta.json` is NOT required, so a half-created bundle still reserves its id.
    pub fn id_taken(&self, id: &str) -> bool {
        if std::fs::metadata(self.root.join(id)).is_ok() {
            return true;
        }
        self.buckets()
            .into_iter()
            .any(|b| std::fs::metadata(b.join(id)).is_ok())
    }

    /// `newSessionID` (chat/session.go:223-230): regenerate until the id is free in BOTH layouts.
    pub fn new_id(&self) -> String {
        loop {
            let id = generate_id();
            if !self.id_taken(&id) {
                return id;
            }
        }
    }

    /// `ListSessions` (chat/session.go:948-964), MODE-ISOLATED: `None` lists the flat root only,
    /// `Some(root)` only `projects/<slug(root)>/`. Sorted by `updated_at` DESC with unparsable timestamps
    /// last; a missing directory is an EMPTY list, not an error.
    pub fn list(&self, project_root: Option<&Path>) -> Result<Vec<SessionInfo>, SessionError> {
        let dir = match project_root {
            Some(root) => self.bucket_of(root),
            None => self.root.clone(),
        };
        let mut infos = list_bucket(&dir)?;
        sort_by_updated_desc(&mut infos);
        Ok(infos)
    }

    /// `listAllSessions` (chat/session.go:970-991) — the RESOLUTION view: the flat root plus every
    /// bucket, same sort. Only id resolution consults it; the display views stay mode-isolated.
    pub fn list_all(&self) -> Result<Vec<SessionInfo>, SessionError> {
        let mut infos = list_bucket(&self.root)?;
        for bucket in self.buckets() {
            infos.extend(list_bucket(&bucket).unwrap_or_default());
        }
        sort_by_updated_desc(&mut infos);
        Ok(infos)
    }

    /// `ResolveSessionID` (chat/session.go:288-305): the mode's OWN view is tried first, so a short
    /// prefix means "one of mine"; ONLY a [`NoMatch`](SessionError::NoMatch) widens to
    /// [`list_all`](Self::list_all). An ambiguity inside the scoped view is FINAL — it can only get more
    /// ambiguous in the superset.
    pub fn resolve_id(
        &self,
        fragment: &str,
        project_root: Option<&Path>,
    ) -> Result<String, SessionError> {
        match resolve_in(&self.list(project_root)?, fragment) {
            Ok(id) => Ok(id),
            Err(SessionError::NoMatch(_)) => resolve_in(&self.list_all()?, fragment),
            Err(e) => Err(e),
        }
    }

    /// A new bundle from what [`NewSession`] records. Touches NO disk — the bundle is created lazily by
    /// the first append. `project && !cwd.is_empty()` places it in `projects/<slug(cwd)>/<id>`, otherwise
    /// it stays flat; `cwd` is recorded in meta either way (empty omits the key).
    pub fn create(&self, session: NewSession) -> Result<SessionWriter, SessionError> {
        let NewSession {
            kind,
            provider,
            model,
            temperature,
            cwd,
            project,
            agent,
        } = session;
        let id = self.new_id();
        let bucket = if project && !cwd.is_empty() {
            self.bucket_of(Path::new(&cwd))
        } else {
            self.root.clone()
        };
        let now = now_rfc3339();
        let meta = SessionMeta {
            version: SESSION_SCHEMA_VERSION,
            id: id.clone(),
            created_at: now.clone(),
            updated_at: now,
            provider,
            model,
            temperature,
            cwd,
            agent,
            ..SessionMeta::default()
        };
        Ok(SessionWriter::pending(bucket.join(&id), meta, kind))
    }

    /// `ResumeSession` (chat/session.go:383-402): locate the bundle, read its meta (failures become
    /// [`SessionError::CannotRead`]), load the log, and open `messages.jsonl` for appending. The writer
    /// comes back seeded with the log's `conv_count` and usage; the [`Session`] carries the derived view.
    ///
    /// The bundle lock is taken FIRST — a bundle another process holds is refused with
    /// [`SessionError::Locked`] before anything is read — and the returned writer keeps it. The view's
    /// unanswered tail is then repaired ([`repair_tail`]) and the synthesised results appended, so the
    /// session never comes back in a shape every provider rejects.
    pub fn resume(
        &self,
        id: &str,
        kind: ProviderKind,
    ) -> Result<(SessionWriter, Session), SessionError> {
        let dir = self.dir(id)?;
        let lock = lock_bundle(&dir, id)?;
        let meta = read_meta(&dir, id)?;
        let mut log = load_log(&dir, kind)?;
        let path = dir.join(LOG_FILE);
        let mut file = open_append_0644(&path)?;
        terminate_last_line(&path, &mut file)?;
        let repaired = repair_tail(&mut log.view);
        let last_written = meta.updated_at.clone();
        let mut writer = SessionWriter::resumed(dir, meta, kind, file, &log, lock);
        // The repair is in the log once the batch is; a meta that did not follow catches up with the next write
        // and is no reason to refuse the session.
        match writer.append_messages(&log.view[log.view.len() - repaired..]) {
            Ok(()) | Err(SessionError::MetaNotSaved(_)) => {}
            Err(e) => return Err(e),
        }
        let session = Session {
            meta: writer.meta().clone(),
            last_written,
            messages: log.view,
            usage: log.usage,
            repaired,
            foreign: log.foreign,
        };
        Ok((writer, session))
    }

    /// Who holds bundle `id` open: `None` when nobody does (the lock was free, and is free again), else the
    /// holder's pid as it wrote it (`Some(None)` when it has not yet). What `iota resume <id> -m` asks before
    /// it either runs the turn itself or hands the message to the holder (docs/design/agent-messaging.md §5).
    #[allow(clippy::option_option)] // "nobody" and "somebody, pid unknown" are different answers
    pub fn holder(&self, id: &str) -> Result<Option<Option<u32>>, SessionError> {
        match lock_bundle(&self.dir(id)?, id) {
            Ok(_free) => Ok(None),
            Err(SessionError::Locked { pid, .. }) => Ok(Some(pid)),
            Err(e) => Err(e),
        }
    }

    /// [`Self::holder`] for a bot: who runs the bot in `bot_dir`. A bot that never ran has no directory, and
    /// asking creates none.
    #[allow(clippy::option_option)]
    pub fn bot_holder(bot_dir: &Path, bot: &str) -> Result<Option<Option<u32>>, SessionError> {
        if !bot_dir.is_dir() {
            return Ok(None);
        }
        match lock_bot(bot_dir, bot) {
            Ok(_free) => Ok(None),
            Err(SessionError::Locked { pid, .. }) => Ok(Some(pid)),
            Err(e) => Err(e),
        }
    }

    /// A bundle's meta alone — what a resume reads FIRST, to learn which provider the session runs on before
    /// anything is opened for it.
    pub fn meta(&self, id: &str) -> Result<SessionMeta, SessionError> {
        read_meta(&self.dir(id)?, id)
    }

    /// `LoadSession` (chat/session.go:904-918): [`resume`](Self::resume) without the writer — nothing is
    /// opened for writing.
    pub fn load(&self, id: &str, kind: ProviderKind) -> Result<Session, SessionError> {
        let dir = self.dir(id)?;
        let meta = read_meta(&dir, id)?;
        let log = load_log(&dir, kind)?;
        Ok(Session {
            last_written: meta.updated_at.clone(),
            meta,
            messages: log.view,
            usage: log.usage,
            repaired: 0,
            foreign: log.foreign,
        })
    }

    /// `LoadFullHistory` (chat/session.go:920-941) by id: the bucket-aware locator plus
    /// [`load_full_history`](crate::session::load_full_history), the way [`load`](Self::load)
    /// pairs the locator with `load_log`.
    ///
    /// This is `/export`'s source for a saved session — the whole log, compaction markers
    /// skipped, so an export is never truncated by a `/compact`.
    pub fn load_full(&self, id: &str, kind: ProviderKind) -> Result<Vec<Message>, SessionError> {
        let dir = self.dir(id)?;
        load_full_history(&dir, kind)
    }

    /// `DeleteSession` (chat/session.go:1065-1074): removes a bundle wherever it lives.
    ///
    /// The id must be a bare directory name — no separators, no `..` — so it can never
    /// escape the sessions root; and because the locator only matches REAL bundles (a
    /// directory holding `meta.json`), the `projects/` container itself can never be
    /// removed by id.
    ///
    /// A bundle another process holds open is refused with [`SessionError::Locked`]; the lock is kept
    /// until the bundle is gone, so nobody can open it halfway through the removal.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err(SessionError::InvalidId(id.to_owned()));
        }
        self.check_not_bot_owned(id)?;
        let dir = self.dir(id)?;
        let _lock = lock_bundle(&dir, id)?;
        std::fs::remove_dir_all(dir).map_err(SessionError::Io)
    }

    /// The bot's resume-or-create (docs/design/bot-mode.md §2.2). `bot_dir` is `<bots>/<name>`; `fresh`
    /// describes the bundle to create when there is none (its `project` is overridden: a bot's session is
    /// always flat, §1.3).
    ///
    /// The bot lock is taken FIRST and handed to the returned writer, which keeps it for its lifetime: two
    /// first launches of the same bot are serialised by it, and a running bot is refused. Then:
    ///
    /// - no pointer: a new bundle — titled after the bot — is put on disk (the bundle lock taken), and only
    ///   then is the pointer published, atomically: [`BotOpen::Fresh`]. A bundle that could not be created
    ///   publishes nothing; one whose pointer could not be written is left behind unnamed, an ordinary empty
    ///   session, and the next launch creates another;
    /// - a pointer whose bundle resumes: [`BotOpen::Resumed`];
    /// - a pointer whose bundle is not found: [`SessionError::BotMissing`];
    /// - anything else the resume reports (an unreadable meta or log, a held bundle lock) is returned as it
    ///   is — a damaged body is never replaced by a new one.
    pub fn open_bot(
        &self,
        bot_dir: &Path,
        fresh: NewSession,
        kind: ProviderKind,
    ) -> Result<BotOpen, SessionError> {
        let bot = bot_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bot_lock = lock_bot(bot_dir, &bot)?;
        if let Some(ptr) = BotPointer::read(bot_dir)? {
            return match self.resume(&ptr.session, kind) {
                Ok((mut writer, session)) => {
                    writer.hold_bot_lock(bot_lock);
                    Ok(BotOpen::Resumed(writer, Box::new(session)))
                }
                Err(SessionError::NotFound(_)) => Err(SessionError::BotMissing {
                    bundle: self.root.join(&ptr.session),
                    pointer: bot_dir.join(BOT_POINTER_FILE),
                    id: ptr.session,
                    bot,
                }),
                Err(e) => Err(e),
            };
        }
        let mut writer = self.create(NewSession {
            project: false,
            ..fresh
        })?;
        // No titler for a bot: its one session is named after it (§2.2).
        writer.update_meta(|m| bot.clone_into(&mut m.title))?;
        writer.materialize()?;
        BotPointer::new(writer.id()).write(bot_dir)?;
        writer.hold_bot_lock(bot_lock);
        Ok(BotOpen::Fresh(writer))
    }

    /// Where the grants of the project rooted at `project_root` are kept: [`GRANTS_FILE`] in the bucket of its
    /// REAL location, so every spelling of one project (a symlink to it, a trailing `/.`) finds the same file.
    pub fn grants_path(&self, project_root: &Path) -> PathBuf {
        self.bucket_of(&real_root(project_root)).join(GRANTS_FILE)
    }

    /// The project level of the grants (docs/design/grants.md §4.2): this root's entry of its bucket's
    /// [`GRANTS_FILE`] — and only that entry, whatever else the file holds. No file is no grants; a file that
    /// is not v1 grants is [`SessionError::GrantsUnreadable`], and the caller then applies none.
    pub fn project_grants(&self, project_root: &Path) -> Result<GrantSet, SessionError> {
        let mut file = read_grants(&self.grants_path(project_root))?;
        Ok(file
            .roots
            .remove(&real_root(project_root))
            .unwrap_or_default())
    }

    /// Changes the project level by `change`, read–modify–write: the file is read afresh, this root's entry
    /// changed (and dropped once empty), and the whole file written back with `write_atomic`, 0600 — the
    /// bucket created on the first write. Returns the entry as it now is. A file that cannot be read is left
    /// as it is and refused ([`SessionError::GrantsUnreadable`]), never replaced.
    ///
    /// There is no lock across the read and the write: two processes granting in the same instant can lose
    /// one grant, and its user is asked again. That window is accepted (§4.2).
    pub fn update_project_grants(
        &self,
        project_root: &Path,
        change: impl FnOnce(&mut GrantSet),
    ) -> Result<GrantSet, SessionError> {
        let path = self.grants_path(project_root);
        let mut file = read_grants(&path)?;
        file.v = GRANTS_VERSION;
        let key = real_root(project_root);
        let mut set = file.roots.remove(&key).unwrap_or_default();
        change(&mut set);
        if !set.is_empty() {
            file.roots.insert(key, set.clone());
        }
        let mut bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        fs::write_atomic(&path, &bytes, Some(0o600))?;
        Ok(set)
    }

    /// `<root>/projects/<slug(project_root)>`.
    fn bucket_of(&self, project_root: &Path) -> PathBuf {
        self.root
            .join(PROJECTS_DIR_NAME)
            .join(Self::project_slug(project_root))
    }

    /// Every `projects/<bucket>` directory, name-sorted; empty when `projects/` does not exist.
    fn buckets(&self) -> Vec<PathBuf> {
        sorted_subdirs(&self.root.join(PROJECTS_DIR_NAME))
            .into_iter()
            .flatten()
            .map(|e| e.path())
            .collect()
    }
}

/// Ends the log with `'\n'` when a torn write left its last line unterminated, so the next append starts
/// a record of its own instead of gluing onto the fragment (and being skipped with it on the next load).
fn terminate_last_line(path: &Path, append: &mut std::fs::File) -> Result<(), SessionError> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::File::open(path)?;
    if f.metadata()?.len() == 0 {
        return Ok(());
    }
    let mut last = [0u8; 1];
    f.seek(SeekFrom::End(-1))?;
    f.read_exact(&mut last)?;
    if last[0] != b'\n' {
        append.write_all(b"\n")?;
    }
    Ok(())
}

/// A project root as [`GRANTS_FILE`] keys it: its real location, or — when even that cannot be worked out —
/// the path cleaned.
fn real_root(project_root: &Path) -> PathBuf {
    fs::physical(project_root).unwrap_or_else(|| paths::clean(project_root))
}

/// [`GRANTS_FILE`] at `path`: empty when there is none, [`SessionError::GrantsUnreadable`] when it cannot be
/// read or is not version 1.
fn read_grants(path: &Path) -> Result<GrantsFile, SessionError> {
    let unreadable = |reason: String| SessionError::GrantsUnreadable {
        path: path.to_path_buf(),
        reason,
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GrantsFile {
                v: GRANTS_VERSION,
                roots: BTreeMap::new(),
            });
        }
        Err(e) => return Err(unreadable(e.to_string())),
    };
    let file: GrantsFile = serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))?;
    if file.v != GRANTS_VERSION {
        return Err(unreadable(format!(
            "version {}, this iota reads {GRANTS_VERSION}",
            file.v
        )));
    }
    Ok(file)
}

/// `loadMeta` wrapped in Go's `cannot read session %s: %w` (chat/session.go:388-391, :909-912). A bundle
/// older than v2 is refused ([`SessionError::OldFormat`]): every way a session is opened reads its meta here.
fn read_meta(dir: &Path, id: &str) -> Result<SessionMeta, SessionError> {
    let meta = SessionMeta::read(dir).map_err(|e| SessionError::CannotRead {
        id: id.to_owned(),
        source: Box::new(e),
    })?;
    if meta.version < SESSION_SCHEMA_VERSION {
        return Err(SessionError::OldFormat {
            id: id.to_owned(),
            version: meta.version,
        });
    }
    Ok(meta)
}

/// The subdirectories of `dir`, sorted by name — `os.ReadDir`'s contract, which Go's locator and both
/// listing views inherit, so candidate order (and therefore the `Ambiguous` text) is deterministic.
fn sorted_subdirs(dir: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)?
        .flatten()
        // `DirEntry::file_type` does not follow symlinks, like Go's `DirEntry.IsDir`.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries)
}

/// `listBucket` (chat/session.go:996-1028): one directory of bundles as summaries. A missing directory is
/// an empty bucket, not an error; an unreadable `meta.json` skips that entry.
fn list_bucket(dir: &Path) -> Result<Vec<SessionInfo>, SessionError> {
    let entries = match sorted_subdirs(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(SessionError::Io(e)),
    };
    let mut infos = Vec::new();
    for entry in entries {
        if entry.file_name() == PROJECTS_DIR_NAME {
            continue;
        }
        let Ok(meta) = SessionMeta::read(&entry.path()) else {
            continue;
        };
        infos.push(SessionInfo {
            id: meta.id,
            title: meta.title,
            model: meta.model,
            provider: meta.provider,
            updated_at: parse_rfc3339(&meta.updated_at),
            message_count: meta.message_count,
        });
    }
    Ok(infos)
}

/// Most-recently-updated first; an unparsable timestamp is Go's zero time and sorts last
/// (chat/session.go:962).
fn sort_by_updated_desc(infos: &mut [SessionInfo]) {
    infos.sort_by(|a, b| match (a.updated_at, b.updated_at) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use super::{GRANTS_FILE, PROJECTS_DIR_NAME, SessionStore};
    use crate::session::SessionError;
    use crate::tool::grants::GrantSet;

    /// A temp dir holding the sessions root and `proj/`, the project; the project's real location.
    fn fixture() -> (tempfile::TempDir, SessionStore, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(t.path().join("proj")).expect("proj");
        let root = crate::app::canonical(&t.path().join("proj")).expect("real proj");
        let store = SessionStore::new(t.path().join("sessions"));
        (t, store, root)
    }

    fn dirs(d: &[&Path]) -> GrantSet {
        GrantSet {
            dirs: d.iter().map(|p| p.to_path_buf()).collect(),
            ..GrantSet::default()
        }
    }

    fn file(store: &SessionStore, root: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(store.grants_path(root)).expect("the file"))
            .expect("json")
    }

    /// The file sits in the project's bucket, beside its bundles and invisible to every listing, and holds the
    /// grant under the project's REAL root: a symlink to the project reads and writes the same entry.
    #[test]
    fn project_grants_are_keyed_by_the_real_root() {
        let (t, store, root) = fixture();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&root, &link).expect("link");
        assert_eq!(store.project_grants(&root).unwrap(), GrantSet::default());

        let lib = Path::new("/data/lib");
        let set = store
            .update_project_grants(&link, |s| s.dirs.push(lib.to_path_buf()))
            .unwrap();
        assert_eq!(set, dirs(&[lib]));
        let path = store.grants_path(&root);
        assert_eq!(path, store.grants_path(&link));
        assert_eq!(
            path,
            store
                .root()
                .join(PROJECTS_DIR_NAME)
                .join(SessionStore::project_slug(&root))
                .join(GRANTS_FILE)
        );
        assert_eq!(
            file(&store, &root),
            serde_json::json!({"v": 1, "roots": {root.to_str().unwrap(): {"dirs": ["/data/lib"]}}})
        );
        assert_eq!(store.project_grants(&root).unwrap(), dirs(&[lib]));
        assert_eq!(store.project_grants(&link).unwrap(), dirs(&[lib]));

        // No session view mistakes the file for a bundle.
        assert!(store.list(Some(&root)).unwrap().is_empty());
        assert!(store.list_all().unwrap().is_empty());
        assert_eq!(store.find_dir(GRANTS_FILE), None);

        // Emptied, the entry goes; the file stays a valid, empty one.
        let set = store
            .update_project_grants(&root, |s| s.dirs.clear())
            .unwrap();
        assert!(set.is_empty());
        assert_eq!(
            file(&store, &root),
            serde_json::json!({"v": 1, "roots": {}})
        );
    }

    /// `/a-b/c` and `/a/b/c` share a bucket, so they share the file — and nothing in it: each reads and writes
    /// its own entry, and a write by one keeps the other's.
    #[test]
    fn two_roots_with_one_slug_share_a_file_and_nothing_else() {
        let t = tempfile::tempdir().expect("tempdir");
        let real = crate::app::canonical(t.path()).expect("real t");
        let one = real.join("a-b/c");
        let two = real.join("a/b/c");
        for d in [&one, &two] {
            std::fs::create_dir_all(d).expect("root");
        }
        assert_eq!(
            SessionStore::project_slug(&one),
            SessionStore::project_slug(&two)
        );
        let store = SessionStore::new(real.join("sessions"));
        assert_eq!(store.grants_path(&one), store.grants_path(&two));

        store
            .update_project_grants(&one, |s| s.dirs.push("/x/one".into()))
            .unwrap();
        store
            .update_project_grants(&two, |s| s.dirs.push("/x/two".into()))
            .unwrap();
        assert_eq!(
            store.project_grants(&one).unwrap(),
            dirs(&[Path::new("/x/one")])
        );
        assert_eq!(
            store.project_grants(&two).unwrap(),
            dirs(&[Path::new("/x/two")])
        );
        let roots = file(&store, &one)["roots"].clone();
        assert_eq!(roots.as_object().unwrap().len(), 2);

        store
            .update_project_grants(&one, |s| s.dirs.clear())
            .unwrap();
        assert!(store.project_grants(&one).unwrap().is_empty());
        assert_eq!(
            store.project_grants(&two).unwrap(),
            dirs(&[Path::new("/x/two")])
        );
    }

    /// A file that is not v1 grants — garbage, or another version — grants nothing, says where and why, and
    /// is never replaced by a write: the write refuses and the bytes stay as they were.
    #[test]
    fn a_broken_grants_file_grants_nothing() {
        let (_t, store, root) = fixture();
        let path = store.grants_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).expect("bucket");
        for bytes in [
            &b"{not json"[..],
            br#"{"v": 2, "roots": {}}"#,
            br#"{"roots": {}}"#,
        ] {
            std::fs::write(&path, bytes).expect("write");
            let err = store.project_grants(&root).unwrap_err();
            assert!(
                matches!(&err, SessionError::GrantsUnreadable { path: p, .. } if *p == path),
                "{err:?}"
            );
            assert!(
                err.to_string()
                    .starts_with(&format!("cannot read {} (", path.display())),
                "{err}"
            );
            assert!(matches!(
                store.update_project_grants(&root, |s| s.dirs.push("/x".into())),
                Err(SessionError::GrantsUnreadable { .. })
            ));
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "left as it was");
        }
        std::fs::write(&path, br#"{"v": 2, "roots": {}}"#).expect("write");
        let err = store.project_grants(&root).unwrap_err().to_string();
        assert!(err.ends_with("(version 2, this iota reads 1)"), "{err}");
    }

    /// The file is the user's alone (0600) — also when it was there wider — and is written whole: no temp
    /// file is left beside it, and every write reads the file afresh, so a grant made by another process in
    /// between is kept.
    #[test]
    fn the_grants_file_is_private_and_written_whole() {
        let (_t, store, root) = fixture();
        store
            .update_project_grants(&root, |s| s.dirs.push("/x/a".into()))
            .unwrap();
        let path = store.grants_path(&root);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // Another process's grant, written between this one's load and its next write.
        let key = root.to_str().unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!(
                {"v": 1, "roots": {key: {"dirs": ["/x/a", "/x/other"]}}}
            ))
            .unwrap(),
        )
        .expect("write");
        let set = store
            .update_project_grants(&root, |s| s.dirs.push("/x/b".into()))
            .unwrap();
        assert_eq!(
            set,
            dirs(&[Path::new("/x/a"), Path::new("/x/other"), Path::new("/x/b")])
        );
        assert_eq!(mode(&path), 0o600, "a rewrite is private again");
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [GRANTS_FILE], "no temp file is left behind");
    }
}
