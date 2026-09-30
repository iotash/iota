//! The sessions root and everything addressed through it (chat/session.go:154-305, :335-402, :904-1028).
//!
//! Bundles live in two layouts: flat `<root>/<id>/` (normal mode) and `<root>/projects/<slug>/<id>/`
//! (agent mode, where `<slug>` encodes the project root). The DISPLAY views are mode-isolated; only
//! `iota resume` id resolution ever merges them.

use std::path::{Path, PathBuf};

use crate::app::HostDirs;
use crate::app::paths;
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

/// The `sessionsDir` subdirectory holding project-scoped buckets (chat/session.go:167).
pub const PROJECTS_DIR_NAME: &str = "projects";

/// A lightweight session summary (chat/session.go:132-143) minus the picker-only `Project` hint.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    /// The session id.
    pub id: String,
    /// The generated title (may be empty).
    pub title: String,
    /// The model recorded in meta.
    pub model: String,
    /// The provider type recorded in meta.
    pub provider: String,
    /// `meta.updated_at`; `None` when unparsable — Go's zero time, which sorts last.
    pub updated_at: Option<jiff::Timestamp>,
    /// `meta.message_count`.
    pub message_count: i64,
}

/// What a new bundle records at creation: the provider and model it runs under, the endpoint, the
/// working directory it belongs to, and the agent. `NewSession::new(kind, model)` is the bare bundle
/// every other field defaulted — no temperature, no base URL, no cwd, flat (not project-scoped), no agent.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSession {
    /// The provider type the bundle is written under.
    pub kind: ProviderKind,
    /// The model id recorded in meta.
    pub model: String,
    /// The temperature recorded in meta (`None` omits the key).
    pub temperature: Option<f64>,
    /// The endpoint recorded in meta (`""` omits the key).
    pub base_url: String,
    /// The working directory recorded in meta (`""` omits the key), and — with `project` — the bucket.
    pub cwd: String,
    /// Project-scoped: `cwd` is non-empty and the bundle lives in `projects/<slug(cwd)>/<id>`.
    pub project: bool,
    /// The `agents:` entry the run was under (`""` = none).
    pub agent: String,
    /// The id to create the bundle under; `None` mints a fresh one. A bot's session id is fixed by its
    /// pointer before the bundle exists (docs/design/bot-mode.md §2.2).
    pub id: Option<String>,
}

impl NewSession {
    /// The bare bundle: `kind` and `model`, everything else at its default.
    pub fn new(kind: ProviderKind, model: &str) -> Self {
        Self {
            kind,
            model: model.to_owned(),
            temperature: None,
            base_url: String::new(),
            cwd: String::new(),
            project: false,
            agent: String::new(),
            id: None,
        }
    }
}

/// What [`SessionStore::open_bot`] found (docs/design/bot-mode.md §2.2).
#[derive(Debug)]
pub enum BotOpen {
    /// The bot's session, resumed (the view boxed: it is the large half).
    Resumed(SessionWriter, Box<Session>),
    /// A new (still pending) bundle under the pointer's id. `never_saved`: a pointer was already there but
    /// its bundle never reached the disk — the last run ended before its first message — so this is the
    /// same empty session starting over, and the transcript says so.
    Fresh {
        /// The pending writer.
        writer: SessionWriter,
        /// A pointer existed whose bundle was never materialised.
        never_saved: bool,
    },
}

/// The sessions root (`<home>/.iota/sessions`) as a value. Constructed from a path in tests and from
/// [`HostDirs`] in the binary — it NEVER reads the process environment.
#[derive(Clone, Debug)]
pub struct SessionStore {
    root: PathBuf,
    /// `<app home>/bots` — the pointers that protect bot sessions (§2.7). `None` for a store built from a
    /// bare path, which knows of no bots.
    bots: Option<PathBuf>,
}

impl SessionStore {
    /// The sessions root directory verbatim (tests pass a temp dir).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            bots: None,
        }
    }

    /// The same store, protecting the sessions the pointers under `bots` name.
    #[must_use]
    pub fn with_bots(mut self, bots: impl Into<PathBuf>) -> Self {
        self.bots = Some(bots.into());
        self
    }

    /// `<app home>/sessions` (chat/session.go:156-162), aware of `<app home>/bots`;
    /// [`SessionError::HomeNotDefined`] when the host has no home directory.
    pub fn from_dirs(dirs: &HostDirs) -> Result<Self, SessionError> {
        let (Some(home), Some(bots)) = (dirs.app_home(), dirs.bots_dir()) else {
            return Err(SessionError::HomeNotDefined);
        };
        Ok(Self::new(home.join("sessions")).with_bots(bots))
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
                source: Box::new(e),
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
            model,
            temperature,
            base_url,
            cwd,
            project,
            agent,
            id,
        } = session;
        let id = id.unwrap_or_else(|| self.new_id());
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
            provider: kind.as_str().to_owned(),
            model,
            temperature,
            base_url,
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
        writer.append_messages(&log.view[log.view.len() - repaired..])?;
        let session = Session {
            meta: writer.meta().clone(),
            last_written,
            messages: log.view,
            usage: log.usage,
            repaired,
        };
        Ok((writer, session))
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
    /// describes the bundle to create when there is none (its `id` and `project` are the pointer's business
    /// and are overridden: a bot's session is always flat, §1.3).
    ///
    /// The bot lock is taken FIRST and handed to the returned writer, which keeps it for its lifetime — it
    /// covers the window where the pointer is written and the bundle does not exist yet. Then:
    ///
    /// - no pointer: a new id is written into one (`materialized: false`) BEFORE anything else, and the
    ///   bundle is created lazily under that id;
    /// - a pointer whose bundle resumes: [`BotOpen::Resumed`];
    /// - a pointer whose bundle is not found and was never materialised: the same empty session again
    ///   (`Fresh { never_saved: true }`);
    /// - a pointer whose bundle is not found but WAS materialised: [`SessionError::BotMissing`];
    /// - anything else the resume reports (an unreadable meta or log, a held bundle lock) is returned as it
    ///   is — a damaged body is never replaced by a new one.
    ///
    /// A `Fresh` writer marks the pointer `materialized` once its first write has created the bundle
    /// ([`SessionWriter::on_created`]).
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
        let (id, never_saved) = match BotPointer::read(bot_dir)? {
            None => {
                let id = self.new_id();
                BotPointer::new(&id).write(bot_dir)?;
                (id, false)
            }
            Some(ptr) => match self.resume(&ptr.session, kind) {
                Ok((mut writer, session)) => {
                    // A bundle that exists is materialised, whatever a crash between its creation and the
                    // pointer's rewrite left in the file.
                    if !ptr.materialized {
                        BotPointer {
                            materialized: true,
                            ..ptr
                        }
                        .write(bot_dir)?;
                    }
                    writer.hold_bot_lock(bot_lock);
                    return Ok(BotOpen::Resumed(writer, Box::new(session)));
                }
                Err(SessionError::NotFound(_)) if !ptr.materialized => (ptr.session, true),
                Err(SessionError::NotFound(_)) => {
                    return Err(SessionError::BotMissing {
                        bundle: self.root.join(&ptr.session),
                        pointer: bot_dir.join(BOT_POINTER_FILE),
                        id: ptr.session,
                        bot,
                    });
                }
                Err(e) => return Err(e),
            },
        };
        let mut writer = self.create(NewSession {
            id: Some(id.clone()),
            project: false,
            ..fresh
        })?;
        let dir = bot_dir.to_path_buf();
        writer.on_created(Box::new(move || {
            BotPointer {
                materialized: true,
                ..BotPointer::new(&id)
            }
            .write(&dir)
        }));
        writer.hold_bot_lock(bot_lock);
        Ok(BotOpen::Fresh {
            writer,
            never_saved,
        })
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

/// `loadMeta` wrapped in Go's `cannot read session %s: %w` (chat/session.go:388-391, :909-912).
fn read_meta(dir: &Path, id: &str) -> Result<SessionMeta, SessionError> {
    SessionMeta::read(dir).map_err(|e| SessionError::CannotRead {
        id: id.to_owned(),
        source: Box::new(e),
    })
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
