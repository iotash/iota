//! The sessions root and everything addressed through it (chat/session.go:154-305, :335-402, :904-1028).
//!
//! Bundles live in two layouts: flat `<root>/<id>/` (normal mode) and `<root>/projects/<slug>/<id>/`
//! (agent mode, where `<slug>` encodes the project root). The DISPLAY views are mode-isolated; only
//! `--resume` id resolution ever merges them.

use std::path::{Path, PathBuf};

use crate::app::HostDirs;
use crate::paths;
use crate::provider::ProviderKind;
use crate::provider::model::Message;

use crate::session::error::SessionError;
use crate::session::id::{generate_id, resolve_in};
use crate::session::loader::{Session, load_full_history, load_log};
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

/// The sessions root (`<home>/.iota/sessions`) as a value. Constructed from a path in tests and from
/// [`HostDirs`] in the binary — it NEVER reads the process environment.
#[derive(Clone, Debug)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// The sessions root directory verbatim (tests pass a temp dir).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `<app home>/sessions` (chat/session.go:156-162); [`SessionError::HomeNotDefined`] when the host
    /// has no home directory.
    pub fn from_dirs(dirs: &HostDirs) -> Result<Self, SessionError> {
        dirs.app_home()
            .map(|home| Self::new(home.join("sessions")))
            .ok_or(SessionError::HomeNotDefined)
    }

    /// The sessions root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `projectSlug` (chat/session.go:169-174): the CLEANED root with every path separator replaced by
    /// `'-'`, Claude Code style — `/Users/x/proj` → `-Users-x-proj`, `/` → `-`.
    pub fn project_slug(root: &Path) -> String {
        paths::clean(root)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "-")
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

    /// `NewSessionWriter` (chat/session.go:335-361). Touches NO disk — the bundle is created lazily by
    /// the first append. `project && !cwd.is_empty()` places it in `projects/<slug(cwd)>/<id>`, otherwise
    /// it stays flat; `cwd` is recorded in meta either way (empty omits the key).
    pub fn create(
        &self,
        kind: ProviderKind,
        model: &str,
        temperature: Option<f64>,
        base_url: &str,
        cwd: &str,
        project: bool,
    ) -> Result<SessionWriter, SessionError> {
        let id = self.new_id();
        let bucket = if project && !cwd.is_empty() {
            self.bucket_of(Path::new(cwd))
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
            model: model.to_owned(),
            temperature,
            base_url: base_url.to_owned(),
            cwd: cwd.to_owned(),
            ..SessionMeta::default()
        };
        Ok(SessionWriter::pending(bucket.join(&id), meta, kind))
    }

    /// `ResumeSession` (chat/session.go:383-402): locate the bundle, read its meta (failures become
    /// [`SessionError::CannotRead`]), load the log, and open `messages.jsonl` for appending. The writer
    /// comes back seeded with the log's `conv_count` and usage; the [`Session`] carries the derived view.
    pub fn resume(
        &self,
        id: &str,
        kind: ProviderKind,
    ) -> Result<(SessionWriter, Session), SessionError> {
        let dir = self.dir(id)?;
        let meta = read_meta(&dir, id)?;
        let log = load_log(&dir, kind)?;
        let file = open_append_0644(&dir.join(LOG_FILE))?;
        let writer =
            SessionWriter::resumed(dir, meta.clone(), kind, file, log.conv_count, log.usage);
        let session = Session {
            meta,
            messages: log.view,
            usage: log.usage,
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
            meta,
            messages: log.view,
            usage: log.usage,
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
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err(SessionError::InvalidId(id.to_owned()));
        }
        let dir = self.dir(id)?;
        std::fs::remove_dir_all(dir).map_err(SessionError::Io)
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
