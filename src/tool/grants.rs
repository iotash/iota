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
//! store does no I/O and holds its lock only to copy or change its lists — never across an await, never
//! while it looks at the filesystem. The session level is kept in the bundle's `meta.json`
//! ([`SessionMeta::grants`](crate::session::SessionMeta::grants)), written by the chat's doors; the project
//! level in the project's bucket ([`SessionStore::project_grants`](crate::session::SessionStore::project_grants)).
//! Levels only add: a grant at either one is in force, and revoking at one leaves the other alone.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::app::fs;
use crate::sync::lock;

/// How long a grant lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// This conversation: kept in its bundle, emptied by `/new`, replaced by a resume and a `/session` switch.
    Session,
    /// This project, in every session and every mode: kept in the bucket's `grants.json`, reloaded at every
    /// start, `/new` and resume.
    Project,
}

/// What a tool grant names (docs/design/grants.md §3.2). On disk, `{"builtin": "<name>"}`,
/// `{"server": "<configured name>", "tool": "<raw name>"}` or `{"flag": "<--mcp spec>", "tool": "*"}`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "KeyRepr", into = "KeyRepr")]
pub enum ToolKey {
    /// A built-in tool by its wire name. Session level only: its permanent form is the config's.
    Builtin(String),
    /// One MCP tool, or every tool of the server.
    Mcp {
        /// Which server: never the wire segment, which is sanitized and suffixed by connect order.
        server: ServerId,
        /// Which of its tools.
        tool: McpTool,
    },
}

/// An MCP server's identity in a grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerId {
    /// An `mcp_servers:` entry, by its CONFIGURED name.
    Config(String),
    /// A `--mcp` server, by the flag's whole value, trimmed: its name is only `argv[0]` (or the URL), and a
    /// grant to `npx` would approve whatever `npx` starts next.
    Flag(String),
}

/// The tool part of an MCP grant. On disk, the raw name or `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTool {
    /// One tool, by the server's raw (un-namespaced) name.
    Named(String),
    /// Every tool of the server, those it exposes later included: matched at call time by server identity.
    Any,
}

impl McpTool {
    /// `*` is [`McpTool::Any`]; anything else names a tool.
    pub fn parse(s: &str) -> Self {
        if s == "*" {
            Self::Any
        } else {
            Self::Named(s.to_owned())
        }
    }

    /// The raw name, or `*`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Named(t) => t,
            Self::Any => "*",
        }
    }
}

/// [`ToolKey`]'s on-disk shape: an object told apart by its keys.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum KeyRepr {
    Builtin { builtin: String },
    Config { server: String, tool: String },
    Flag { flag: String, tool: String },
}

impl From<KeyRepr> for ToolKey {
    fn from(r: KeyRepr) -> Self {
        let mcp = |server, tool: String| Self::Mcp {
            server,
            tool: McpTool::parse(&tool),
        };
        match r {
            KeyRepr::Builtin { builtin } => Self::Builtin(builtin),
            KeyRepr::Config { server, tool } => mcp(ServerId::Config(server), tool),
            KeyRepr::Flag { flag, tool } => mcp(ServerId::Flag(flag), tool),
        }
    }
}

impl From<ToolKey> for KeyRepr {
    fn from(k: ToolKey) -> Self {
        match k {
            ToolKey::Builtin(builtin) => Self::Builtin { builtin },
            ToolKey::Mcp { server, tool } => {
                let tool = tool.as_str().to_owned();
                match server {
                    ServerId::Config(server) => Self::Config { server, tool },
                    ServerId::Flag(flag) => Self::Flag { flag, tool },
                }
            }
        }
    }
}

/// One level's grants. On disk, each list is omitted when empty (docs/design/grants.md §4.1).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GrantSet {
    /// Granted directories: absolute real paths, each covering its subtree.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<PathBuf>,
    /// Granted tools.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolKey>,
}

impl GrantSet {
    /// Whether the set grants nothing.
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.tools.is_empty()
    }

    /// How many grants the set holds, directories and tools together.
    pub fn len(&self) -> usize {
        self.dirs.len() + self.tools.len()
    }
}

/// The store. `Default` is empty.
#[derive(Debug, Default)]
pub struct Grants {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    session: GrantSet,
    project: GrantSet,
}

impl Inner {
    fn level(&mut self, level: Level) -> &mut GrantSet {
        match level {
            Level::Session => &mut self.session,
            Level::Project => &mut self.project,
        }
    }

    fn levels(&self) -> [&GrantSet; 2] {
        [&self.session, &self.project]
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

    /// Whether any level grants the tool `key` — for an MCP tool, by name or by its server's `*`.
    pub fn allows(&self, key: &ToolKey) -> bool {
        let any = match key {
            ToolKey::Mcp { server, .. } => Some(ToolKey::Mcp {
                server: server.clone(),
                tool: McpTool::Any,
            }),
            ToolKey::Builtin(_) => None,
        };
        lock(&self.inner)
            .levels()
            .into_iter()
            .any(|s| s.tools.contains(key) || any.as_ref().is_some_and(|a| s.tools.contains(a)))
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

    /// Takes the tool `key` back from `level`. False when that level did not hold it.
    pub fn revoke_tool(&self, level: Level, key: &ToolKey) -> bool {
        let mut inner = lock(&self.inner);
        let set = inner.level(level);
        let before = set.tools.len();
        set.tools.retain(|k| k != key);
        set.tools.len() != before
    }

    /// A copy of what `level` holds, in the order it was granted — stale directories included.
    pub fn snapshot(&self, level: Level) -> GrantSet {
        let mut inner = lock(&self.inner);
        inner.level(level).clone()
    }

    /// Replaces the session level's grants: `/new` starts the next conversation with none, a resume and a
    /// `/session` switch with the bundle's — and nothing of the last one, ever merged.
    pub fn replace_session(&self, set: GrantSet) {
        lock(&self.inner).session = set;
    }

    /// Replaces the project level's grants with what the project's file holds now: at every start, `/new` and
    /// resume, and after every project-level write (which read the file afresh).
    pub fn replace_project(&self, set: GrantSet) {
        lock(&self.inner).project = set;
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

    use super::{GrantSet, Grants, Level, McpTool, ServerId, ToolKey};
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

    fn mcp(server: ServerId, tool: &str) -> ToolKey {
        ToolKey::Mcp {
            server,
            tool: McpTool::parse(tool),
        }
    }

    /// `server:*` is matched at call time by the server's identity, so it covers a tool the server did not
    /// expose when it was granted — and nothing of another server, nor of a `--mcp` server whose name is the
    /// same. On disk each key is the object §4.1 shows.
    #[test]
    fn any_covers_a_tool_named_later() {
        let g = Grants::default();
        let github = || ServerId::Config("github".to_owned());
        assert!(!g.allows(&mcp(github(), "search_issues")));
        assert!(g.grant_tool(Level::Project, mcp(github(), "*")));
        for tool in ["search_issues", "a_tool_added_next_month"] {
            assert!(g.allows(&mcp(github(), tool)), "{tool}");
        }
        assert!(!g.allows(&mcp(ServerId::Config("gitlab".to_owned()), "search_issues")));
        assert!(!g.allows(&mcp(ServerId::Flag("github".to_owned()), "search_issues")));
        assert!(!g.allows(&ToolKey::Builtin("search_issues".to_owned())));

        // One tool by name is that tool only.
        let flag = || ServerId::Flag("npx -y @acme/mcp".to_owned());
        g.grant_tool(Level::Session, mcp(flag(), "push"));
        assert!(g.allows(&mcp(flag(), "push")));
        assert!(!g.allows(&mcp(flag(), "pull")));
        assert!(!g.allows(&mcp(ServerId::Flag("npx -y @other/mcp".to_owned()), "push")));

        let set = GrantSet {
            dirs: Vec::new(),
            tools: vec![
                ToolKey::Builtin("edit_file".to_owned()),
                mcp(github(), "search_issues"),
                mcp(flag(), "*"),
            ],
        };
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"tools": [
                {"builtin": "edit_file"},
                {"server": "github", "tool": "search_issues"},
                {"flag": "npx -y @acme/mcp", "tool": "*"},
            ]})
        );
        assert_eq!(serde_json::from_value::<GrantSet>(json).unwrap(), set);
        assert!(g.revoke_tool(Level::Project, &mcp(github(), "*")));
        assert!(!g.allows(&mcp(github(), "search_issues")));
    }
}
