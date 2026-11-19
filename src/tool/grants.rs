//! The grant store (docs/design/grants.md §3): the ONE memory of what the user allowed, read by every
//! enforcement point of a run. Two kinds of grant, kept apart:
//!
//! - a **directory grant** — WHERE: a write root added at run time, the real directory and its subtree. The
//!   code writers' jail reads [`Grants::live_dirs`] at every call, beside the static `write_roots:`, so a write
//!   under a granted directory is INSIDE (asked or not by `auto_write`, like one in the project root);
//! - a **tool grant** — WHETHER: the question is not asked again for that tool. It never answers a call bound
//!   to a location ([`crate::tool::approval::gate`]).
//!
//! One `Arc<Grants>` per process, created with the [`ToolEnv`](crate::tool::ToolEnv) and shared by the tools,
//! both walks and the chat state; `/new` and a `/session` switch swap its contents, never the handle. The
//! store persists nothing and holds its lock only to copy or change its lists — never across an await, never
//! while it looks at the filesystem.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::app::fs;
use crate::sync::lock;

/// How long a grant lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// This conversation: emptied by `/new` and a `/session` switch.
    Session,
}

/// What a tool grant names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolKey {
    /// A tool by its wire name.
    Builtin(String),
}

/// One level's grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrantSet {
    /// Granted directories: absolute real paths, each covering its subtree.
    pub dirs: Vec<PathBuf>,
    /// Granted tools.
    pub tools: Vec<ToolKey>,
}

/// The store. `Default` is empty.
#[derive(Debug, Default)]
pub struct Grants {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    session: GrantSet,
}

impl Inner {
    fn level(&mut self, level: Level) -> &mut GrantSet {
        match level {
            Level::Session => &mut self.session,
        }
    }

    fn levels(&self) -> [&GrantSet; 1] {
        [&self.session]
    }
}

impl Grants {
    /// The granted directories still in force, every level, each once: a directory is LIVE while its real
    /// location is the path that was granted. One replaced by a symlink, or under an ancestor that was, is
    /// stale — no real location lies under it any more — and left out until the directory comes back.
    pub fn live_dirs(&self) -> Vec<PathBuf> {
        let dirs: Vec<PathBuf> = {
            let inner = lock(&self.inner);
            let mut dirs: Vec<PathBuf> = Vec::new();
            for d in inner.levels().into_iter().flat_map(|s| &s.dirs) {
                if !dirs.contains(d) {
                    dirs.push(d.clone());
                }
            }
            dirs
        };
        dirs.into_iter().filter(|d| is_live(d)).collect()
    }

    /// Whether any level grants the tool `key`.
    pub fn allows(&self, key: &ToolKey) -> bool {
        lock(&self.inner)
            .levels()
            .into_iter()
            .any(|s| s.tools.contains(key))
    }

    /// Grants the directory `dir` — an absolute REAL path, resolved by the caller when the user chose it — at
    /// `level`. False when that level already held it.
    pub fn grant_dir(&self, level: Level, dir: PathBuf) -> bool {
        let mut inner = lock(&self.inner);
        let set = inner.level(level);
        if set.dirs.contains(&dir) {
            return false;
        }
        set.dirs.push(dir);
        true
    }

    /// Grants the tool `key` at `level`. False when that level already held it.
    pub fn grant_tool(&self, level: Level, key: ToolKey) -> bool {
        let mut inner = lock(&self.inner);
        let set = inner.level(level);
        if set.tools.contains(&key) {
            return false;
        }
        set.tools.push(key);
        true
    }

    /// Takes the directory `dir` back from `level`. False when that level did not hold it.
    pub fn revoke_dir(&self, level: Level, dir: &Path) -> bool {
        let mut inner = lock(&self.inner);
        let set = inner.level(level);
        let before = set.dirs.len();
        set.dirs.retain(|d| d != dir);
        set.dirs.len() != before
    }

    /// A copy of what `level` holds, in the order it was granted — stale directories included.
    pub fn snapshot(&self, level: Level) -> GrantSet {
        let mut inner = lock(&self.inner);
        inner.level(level).clone()
    }

    /// Replaces the session level's grants: `/new` and a `/session` switch start the next conversation with
    /// `set` and nothing of the last one.
    pub fn replace_session(&self, set: GrantSet) {
        lock(&self.inner).session = set;
    }
}

/// Whether the granted `dir` is still where it was granted ([`Grants::live_dirs`]).
pub fn is_live(dir: &Path) -> bool {
    fs::physical(dir).is_some_and(|real| real == dir)
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{GrantSet, Grants, Level, ToolKey};
    use crate::tool::builtins::code::{CodeSet, Outside, Write};

    /// A temp dir holding `proj/` (the project) and `out/` beside it; `out`'s real location.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(t.path().join("proj")).expect("proj");
        std::fs::create_dir_all(t.path().join("out/sub/deeper")).expect("out");
        let out = std::fs::canonicalize(t.path().join("out")).expect("real out");
        let root = std::fs::canonicalize(t.path().join("proj")).expect("real proj");
        (t, root, out)
    }

    fn jail(root: &Path, grants: &Arc<Grants>) -> Arc<CodeSet> {
        CodeSet::new(
            root.to_path_buf(),
            Vec::new(),
            true,
            Outside::Ask,
            None,
            Arc::clone(grants),
        )
        .expect("set")
    }

    fn inside(cs: &CodeSet, p: &Path) -> bool {
        matches!(
            cs.write_path(p.to_str().expect("utf-8")),
            Ok(Write::Inside(_))
        )
    }

    /// A directory grant is the directory and everything under it, as the jail reads it at the next call —
    /// and nothing beside it: not its parent, not a sibling whose name it prefixes.
    #[test]
    fn a_directory_grant_covers_its_subtree() {
        let (t, root, out) = fixture();
        std::fs::create_dir(t.path().join("out-sibling")).expect("sibling");
        let grants = Arc::new(Grants::default());
        let cs = jail(&root, &grants);
        assert!(!inside(&cs, &out.join("x.txt")), "nothing granted yet");

        assert!(grants.grant_dir(Level::Session, out.clone()));
        assert!(
            !grants.grant_dir(Level::Session, out.clone()),
            "granted once"
        );
        assert_eq!(grants.live_dirs(), std::slice::from_ref(&out));
        for p in [
            out.clone(),
            out.join("x.txt"),
            out.join("sub/deeper/y.txt"),
            out.join("new/dir/z.txt"),
        ] {
            assert!(inside(&cs, &p), "{p:?}");
        }
        let real_t = std::fs::canonicalize(t.path()).expect("real t");
        for p in [real_t.join("x.txt"), real_t.join("out-sibling/x.txt")] {
            assert!(!inside(&cs, &p), "{p:?}");
        }

        // A tool grant is another axis: it opens no directory.
        assert!(grants.grant_tool(Level::Session, ToolKey::Builtin("write_file".to_owned())));
        assert!(grants.allows(&ToolKey::Builtin("write_file".to_owned())));
        assert!(!grants.allows(&ToolKey::Builtin("edit_file".to_owned())));
        assert_eq!(grants.live_dirs(), std::slice::from_ref(&out));

        // Emptying the session level takes both back.
        grants.replace_session(GrantSet::default());
        assert!(grants.live_dirs().is_empty());
        assert!(!grants.allows(&ToolKey::Builtin("write_file".to_owned())));
        assert!(!inside(&cs, &out.join("x.txt")));
    }

    /// The grant is for the location, not the name: once the granted directory is replaced by a symlink —
    /// even one leading back into a directory of the same content — it is stale, and nothing written through
    /// the name lands anywhere. When the directory comes back, the grant is live again.
    #[test]
    fn a_directory_replaced_by_a_link_is_stale() {
        let (t, root, out) = fixture();
        let elsewhere = t.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        let grants = Arc::new(Grants::default());
        let cs = jail(&root, &grants);
        grants.grant_dir(Level::Session, out.clone());
        assert_eq!(grants.live_dirs(), std::slice::from_ref(&out));

        std::fs::rename(&out, t.path().join("moved")).expect("move away");
        std::os::unix::fs::symlink(&elsewhere, &out).expect("link in its place");
        assert!(grants.live_dirs().is_empty(), "a stale grant is not a root");
        assert!(!inside(&cs, &out.join("x.txt")));
        assert!(matches!(
            cs.write_path(out.join("x.txt").to_str().expect("utf-8")),
            Ok(Write::Outside { .. })
        ));

        std::fs::remove_file(&out).expect("unlink");
        std::fs::create_dir(&out).expect("the directory again");
        assert_eq!(grants.live_dirs(), std::slice::from_ref(&out));
        assert!(inside(&cs, &out.join("x.txt")));
    }
}
