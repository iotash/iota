//! The `code` toolset: configuration, the shared `CodeSet` (open reads, the physical write jail, read ledger)
//! and the size/binary helpers. The six tools live in `tools`, the gitignore-aware walk in `walk`.

pub(crate) mod tools;
pub mod udiff;
pub(crate) mod walk;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use crate::app::protected::{self, Why};
use crate::app::{fs, paths};
use crate::tool::context::{Approved, RunCtx};
use crate::tool::grants::Grants;
use crate::tool::{ApprovalSubject, Tool, ToolEnv, ToolOutput};
use serde::Deserialize;

use crate::sync::lock;
use crate::tool::sets::{RawNode, SetError};
use crate::tool::yaml11;

pub(crate) use tools::{EditFile, Glob, Grep, ListDir, ReadFile, WriteFile};

/// Largest file `read_file` serves (the rest is dropped with a marker).
pub(crate) const CODE_MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Output cap of every code tool.
pub const CODE_MAX_OUTPUT: usize = 65536;
/// `glob` returns at most this many (newest) matches.
pub(crate) const CODE_MAX_GLOB_RESULTS: usize = 200;
/// `glob` stops collecting candidates past this many.
pub(crate) const CODE_GLOB_COLLECT_CAP: usize = 10_000;
/// `grep` stops after this many matches.
pub(crate) const CODE_MAX_GREP_MATCHES: usize = 100;
/// `grep` skips files larger than this.
pub(crate) const CODE_GREP_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// `grep` stops once it has read this many bytes of files (per call).
pub(crate) const CODE_GREP_MAX_TOTAL_BYTES: u64 = 256 << 20;
/// `grep` clips matched lines to this many bytes.
pub(crate) const CODE_GREP_MAX_LINE_LEN: usize = 500;
/// `grep` context lines are clamped to this.
pub(crate) const CODE_MAX_GREP_CONTEXT: i64 = 10;
/// A `glob`/`grep` walk stops after visiting this many entries (files, directories, links — per call).
pub(crate) const CODE_WALK_MAX_ENTRIES: usize = 100_000;
/// `list_dir` lists at most this many entries.
pub(crate) const CODE_MAX_DIR_ENTRIES: usize = 500;
/// Bytes sniffed for a NUL by `looks_binary`.
pub(crate) const CODE_BINARY_SNIFF_LEN: usize = 8000;
/// The mark on a write outside every root — in its header and so in its approval prompt, the way the shell
/// marks a call `(outside the sandbox)`.
pub const OUTSIDE_ROOT_MARK: &str = "(outside the project root)";
/// The mark on a write to a protected path ([`crate::app::protected`]) in its header and approval prompt,
/// followed by why it is protected.
pub const PROTECTED_MARK: &str = "(protected)";

/// `tools.code` configuration.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct CodeConfig {
    /// Whether `edit_file`/`write_file` run without approval.
    #[serde(deserialize_with = "yaml11::deserialize_bool")]
    pub(crate) auto_write: bool,
    /// Whether the writers are left out entirely.
    #[serde(deserialize_with = "yaml11::deserialize_bool")]
    pub(crate) read_only: bool,
}

/// One root of the write jail: the path as configured (what messages name) and its real location (see
/// [`fs::physical`]) — what the jail compares the targets' real locations against.
struct Root {
    path: PathBuf,
    real: PathBuf,
}

impl Root {
    /// `None` when the real location cannot be worked out.
    fn new(path: PathBuf) -> Option<Self> {
        let real = fs::physical(&path)?;
        Some(Self { path, real })
    }
}

/// Where a write argument lands, classified by its real location (see [`fs::physical`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Write {
    /// A root's real location holds it: the cleaned lexical path, which is what is opened.
    Inside(PathBuf),
    /// Outside every root and the real location is known: approvable. `path` is the cleaned lexical path
    /// (the read ledger's key), `real` where the write would land — what the prompt names and what is opened.
    Outside { path: PathBuf, real: PathBuf },
    /// On the protected table ([`protected::table`]), inside a root or not: never written without the
    /// user's approval of this one file, whatever `auto_write`, `allow:` or a grant says, and the
    /// trust record and the sessions tree never at all ([`Why::approvable`]). `path` and `real` as for
    /// [`Write::Outside`].
    Protected {
        path: PathBuf,
        real: PathBuf,
        why: Why,
    },
    /// The real location cannot be worked out: refused, never approvable.
    Unverifiable,
}

/// State shared by the six tools: the project root, the grant store, whether anyone can be asked and the read
/// ledger.
pub(crate) struct CodeSet {
    /// The project root. Relative arguments and every display stay anchored on it.
    root: Root,
    /// The run's grant store: its directories — the static level's and the live grants — are the further roots
    /// the write jail accepts, read at every call ([`Grants::roots`]), each one a real path, so its configured
    /// and real locations are one.
    grants: Arc<Grants>,
    /// The process working directory — the display anchor of the header-path ladder
    /// (captured at set construction), not an execution dir.
    cwd: PathBuf,
    auto_write: bool,
    /// Whether the run has somebody to ask (an interactor is bound). A write outside every root, or to an
    /// approvable protected path, is put to the user when there is one, every time; with nobody to ask (`-m`,
    /// the SDK path) it is refused, saying how to allow it.
    attended: bool,
    /// The user's home, whose config and trust record the protected table names; None without one.
    home: Option<PathBuf>,
    reads: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl CodeSet {
    /// A set rooted at `root` (the header-path anchor is the process cwd; when it cannot be read it stays
    /// empty and the ladder skips its rungs). A root whose real location cannot be worked out (see
    /// [`fs::physical`]) fails the set with `CodeRoot`, there being nothing to jail paths to. `attended` is
    /// whether the run has somebody to ask; `home` is the user's home, for the protected table; `grants` the
    /// run's store, whose directories the jail reads per call.
    pub(crate) fn new(
        root: PathBuf,
        auto_write: bool,
        attended: bool,
        home: Option<PathBuf>,
        grants: Arc<Grants>,
    ) -> Result<Arc<Self>, SetError> {
        let root = Root::new(root.clone()).ok_or(SetError::CodeRoot(root))?;
        Ok(Arc::new(Self {
            root,
            grants,
            cwd: std::env::current_dir().unwrap_or_default(),
            auto_write,
            attended,
            home,
            reads: Mutex::new(HashMap::new()),
        }))
    }

    /// The project root as configured — the anchor of relative arguments, walks and displays.
    pub(crate) fn root(&self) -> &Path {
        &self.root.path
    }

    /// The root a walk from `base` is anchored on — the project root or the static directory that
    /// lexically contains it, the project root first: its `.gitignore` filters the walk and its
    /// relative paths are what patterns match. `base` itself when no root contains it lexically
    /// (reads are open, so a walk may start anywhere). `base` is a [`Self::walk_base`], so this
    /// lexical test decides by the real location.
    pub(crate) fn anchor<'a>(&'a self, base: &'a Path) -> &'a Path {
        self.fixed_roots()
            .map(|(path, _)| path)
            .find(|root| paths::within(root, base))
            .unwrap_or(base)
    }

    /// The roots fixed at assembly, `(as configured, real)`: the project root, then the store's static
    /// directories (`allow:`, `--allow`), each its own real location.
    fn fixed_roots(&self) -> impl Iterator<Item = (&Path, &Path)> {
        std::iter::once((self.root.path.as_path(), self.root.real.as_path())).chain(
            self.grants
                .statics()
                .dirs
                .iter()
                .map(|d| (d.as_path(), d.as_path())),
        )
    }

    /// Where a walk from the existing directory `base` really starts: its real location, expressed
    /// under the configured path of the root whose real location contains it (the project root
    /// first), else the real location itself. So every spelling of one directory — a link to it, a
    /// path through a link, `..`, the absolute path — walks the same tree under the same
    /// `.gitignore` ([`Self::anchor`]) and reports the same paths, and a root reached through a
    /// link keeps its own spelling. `base` unchanged when it cannot be canonicalized.
    pub(crate) fn walk_base(&self, base: PathBuf) -> PathBuf {
        let Some(real) = crate::app::canonical(&base) else {
            return base;
        };
        self.fixed_roots()
            .find_map(|(path, root_real)| {
                paths::within(root_real, &real)
                    .then(|| paths::rel(root_real, &real))
                    .flatten()
                    .map(|rel| paths::clean(&path.join(rel)))
            })
            .unwrap_or(real)
    }

    /// The `"path"` argument rendered for a call header. A missing path (a malformed call)
    /// yields `""` — a bare
    /// `"[read_file]"` — rather than falling back to a digest of whatever else the model
    /// sent.
    pub(crate) fn header_arg(&self, args: &crate::provider::model::JsonObject) -> String {
        crate::tool::fmt::header_path(
            crate::tool::args::str_arg(args, "path"),
            &self.cwd,
            &self.root.path,
        )
    }

    /// Where a read goes: a relative argument joined to the project root, an absolute one taken as
    /// is, cleaned lexically. Reads are not jailed — the sandboxed shell already reads the whole
    /// machine without asking, so a jail here would only push the model into `cat`. Err text:
    /// `missing required argument: path`.
    pub(crate) fn read_path(&self, arg: &str) -> Result<PathBuf, ToolOutput> {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return Err(ToolOutput::err("missing required argument: path"));
        }
        Ok(paths::clean(&self.root.path.join(trimmed)))
    }

    /// The write jail's classification. The path is [`Self::read_path`]'s (Err: `missing required
    /// argument: path`); the decision is physical: the path's real location (symlinks resolved, see
    /// [`fs::physical`]) inside the real location of the project root or of a directory of the store (static
    /// or live, read now) is [`Write::Inside`]; a known location outside them all — a symlink under a root that points out of
    /// every root, dangling or not, included — is [`Write::Outside`]; a location that cannot be worked out
    /// is [`Write::Unverifiable`]. Before any of that, a real location on the protected table is
    /// [`Write::Protected`] — a root, a granted directory included, never admits one. The target need not exist,
    /// so `write_file` can create one.
    ///
    /// A `..` in the argument is folded before any link is resolved (`link/../x` is `x`, not
    /// the shell's "`x` next to where `link` points"); check and open use that same path.
    pub(crate) fn write_path(&self, arg: &str) -> Result<Write, ToolOutput> {
        let p = self.read_path(arg)?;
        let roots = self.roots();
        Ok(match fs::physical(&p) {
            Some(real) => match self.protected_at(&real, &roots) {
                Some(why) => Write::Protected { path: p, real, why },
                None if self.admits_real(&real, &roots) => Write::Inside(p),
                None => Write::Outside { path: p, real },
            },
            None => Write::Unverifiable,
        })
    }

    /// Why `real` is protected, if it is: compared with each table entry's real location, worked out now (a
    /// `.git/hooks` linked elsewhere since the set was built is matched where it points now). A
    /// [`protected::Reach::Tree`] entry holds everything under it, a [`protected::Reach::Entry`] only itself. The table's extra
    /// roots are `roots` ([`Self::roots`]), so a granted repository keeps its git rows.
    fn protected_at(&self, real: &Path, roots: &[PathBuf]) -> Option<Why> {
        protected::why_protected(
            &protected::table(&self.root.path, roots, self.home.as_deref()),
            real,
        )
    }

    /// The extra roots in force now: the static level's directories, then the live directory grants
    /// ([`Grants::roots`]).
    fn roots(&self) -> Vec<PathBuf> {
        self.grants.roots()
    }

    /// Where a writer's call may write, decided at call time — after any prompt, so a link swapped since is
    /// caught: `(ledger path, target)`. Inside a root (a directory grant included) both are the lexical path.
    /// Outside every root the call is admitted only when the run has somebody to ask and
    /// [`RunCtx::approved`] names exactly the real location resolved NOW ("allow once"); then the target is
    /// that real location. Err texts: `missing required argument: path`,
    /// `path is outside the project root ({root}): {arg} — ` followed by why it was not approved (this call
    /// has no approval, or nobody can be asked and how to allow it), `cannot verify {arg} inside the project
    /// root ({root})` — `{root}` followed,
    /// when a directory is allowed (`allow:`, `--allow`) or granted, by `) and the extra write roots ({e1}, {e2}`.
    ///
    /// A protected path ([`Write::Protected`]) is admitted only when "allow once" named exactly its real
    /// location — never by a grant — and the trust record and the sessions tree never ([`Why::approvable`]):
    /// `{arg} is protected: {reason} — …`, the rest saying what the user can do instead ([`protected_refusal`]).
    pub(crate) fn write_target(
        &self,
        cx: &RunCtx,
        arg: &str,
    ) -> Result<(PathBuf, PathBuf), ToolOutput> {
        let roots = self.roots_clause();
        match self.write_path(arg)? {
            Write::Inside(p) => Ok((p.clone(), p)),
            Write::Outside { path, real } => {
                let refusal = format!("path is outside the project root ({roots}): {arg}");
                let why = if !self.attended {
                    " — writing there needs the user's approval, which this \
                    non-interactive run cannot ask for; pass --allow <dir>, or list its \
                    directory in the agent's `allow:`, to allow it"
                        .to_owned()
                } else if cx.approved.as_ref() == Some(&Approved::Once(real.clone())) {
                    return Ok((path, real));
                } else {
                    format!(
                        " — writing there needs the user's approval, and this call has none for {}",
                        real.display()
                    )
                };
                Err(ToolOutput::err(format!("{refusal}{why}")))
            }
            Write::Protected { path, real, why } => {
                if why.approvable()
                    && self.attended
                    && cx.approved.as_ref() == Some(&Approved::Once(real.clone()))
                {
                    return Ok((path, real));
                }
                Err(ToolOutput::err(protected_refusal(
                    arg,
                    &real,
                    why,
                    self.attended,
                )))
            }
            Write::Unverifiable => Err(ToolOutput::err(format!(
                "cannot verify {arg} inside the project root ({roots})"
            ))),
        }
    }

    /// What a write the user would be asked about is bound to, from one resolution of its real location:
    /// `Some` only in a run with somebody to ask, for a path outside every root whose location
    /// is known (named [`OUTSIDE_ROOT_MARK`]; its real directory offered as a directory grant unless that is
    /// `/`, the home directory or an ancestor of it — [`offers_root`]) or a protected path that is
    /// [`Why::approvable`] (named [`PROTECTED_MARK`] and why, "allow once" only).
    pub(crate) fn approvable(&self, arg: &str) -> Option<ApprovalSubject> {
        if !self.attended {
            return None;
        }
        match self.write_path(arg) {
            Ok(Write::Outside { real, .. }) => Some(ApprovalSubject {
                detail: outside_summary(&real),
                scope: Some(grant_dir(&real))
                    .filter(|d| offers_root(self.home.as_deref(), d))
                    .map(Path::to_path_buf),
                target: real,
            }),
            Ok(Write::Protected { real, why, .. }) if why.approvable() => Some(ApprovalSubject {
                detail: format!("{} {PROTECTED_MARK}: {}", real.display(), why.reason()),
                scope: None,
                target: real,
            }),
            _ => None,
        }
    }

    /// Whether the writers' calls are put to the user: inside the roots unless `auto_write`, outside
    /// them always when the run has somebody to ask; an unverifiable or empty path never
    /// (it is refused at the call). `None` (the argument-less probe) is `!auto_write`: whether an inside
    /// write asks — the one a tool grant answers.
    pub(crate) fn write_asks(&self, args: Option<&crate::provider::model::JsonObject>) -> bool {
        let Some(args) = args else {
            return !self.auto_write;
        };
        let arg = crate::tool::args::str_arg(args, "path");
        match self.write_path(arg) {
            Ok(Write::Inside(_)) => !self.auto_write,
            Ok(Write::Outside { .. }) => self.attended,
            Ok(Write::Protected { why, .. }) => why.approvable() && self.attended,
            Ok(Write::Unverifiable) | Err(_) => false,
        }
    }

    /// `auto_write`: what the writers waive is the approval of writes INSIDE the roots. A write outside
    /// them, or to a protected path, is still asked (or refused when nobody can be asked), never waived.
    pub(crate) fn waives(&self) -> bool {
        self.auto_write
    }

    /// The writers' description sentence on writes outside the roots: in a run with somebody to ask, that
    /// such a write asks first (so a model does not take a refusal as final); else nothing.
    pub(crate) fn outside_clause(&self) -> &'static str {
        if self.attended {
            " Outside that, the user is asked to approve a write there first."
        } else {
            ""
        }
    }

    /// `the project root`, or `the project root or the extra write roots ({e1}, {e2})` — where the
    /// writers' descriptions say a write may land: the store's static directories.
    pub(crate) fn write_scope(&self) -> String {
        match extra_list(&self.grants.statics().dirs) {
            None => "the project root".to_owned(),
            Some(extra) => format!("the project root or the extra write roots ({extra})"),
        }
    }

    /// `{root}`, or `{root}) and the extra write roots ({e1}, {e2}` — the inside of the error
    /// texts' parentheses: the static directories and the live directory grants, read now.
    fn roots_clause(&self) -> String {
        let root = self.root.path.display();
        match extra_list(&self.roots()) {
            None => root.to_string(),
            Some(extra) => format!("{root}) and the extra write roots ({extra}"),
        }
    }

    /// Whether a real location lies inside the real location of any root: the project root or one of `roots`
    /// ([`Self::roots`], each already a real path).
    fn admits_real(&self, real: &Path, roots: &[PathBuf]) -> bool {
        std::iter::once(self.root.real.as_path())
            .chain(roots.iter().map(PathBuf::as_path))
            .any(|r| paths::within(r, real))
    }

    /// Root-relative slash path; `"."` for the root; absolute outside the project root (a read
    /// anywhere, a write under a granted directory) or when `rel` fails.
    pub(crate) fn display(&self, abs: &Path) -> String {
        if !paths::within(&self.root.path, abs) {
            return abs.display().to_string();
        }
        paths::rel(&self.root.path, abs)
            .map_or_else(|| abs.display().to_string(), |r| paths::to_slash(&r))
    }

    /// Records the mtime of a file `read_file` served.
    pub(crate) fn note_read(&self, abs: &Path) {
        let Ok(meta) = std::fs::metadata(abs) else {
            return;
        };
        let Ok(mtime) = meta.modified() else {
            return;
        };
        lock(&self.reads).insert(abs.to_path_buf(), mtime);
    }

    /// `{d} has not been read in this session — read it with read_file before modifying it` / `cannot access {d}:
    /// {e}` / `{d} changed on disk after it was read — read it again before modifying it`.
    pub(crate) fn require_fresh_read(&self, abs: &Path) -> Result<(), ToolOutput> {
        let stamp = lock(&self.reads).get(abs).copied();
        let Some(stamp) = stamp else {
            return Err(ToolOutput::err(format!(
                "{} has not been read in this session — read it with read_file before modifying it",
                self.display(abs)
            )));
        };
        let mtime = std::fs::metadata(abs)
            .and_then(|m| m.modified())
            .map_err(|e| ToolOutput::err(format!("cannot access {}: {e}", self.display(abs))))?;
        if mtime == stamp {
            Ok(())
        } else {
            Err(ToolOutput::err(format!(
                "{} changed on disk after it was read — read it again before modifying it",
                self.display(abs)
            )))
        }
    }
}

/// Order `glob`, `grep`, `list_dir`, `read_file`, [`edit_file`, `write_file` unless `read_only`]. Errors: `CodeConfig`,
/// `CodeContradiction`.
///
/// An unreadable working directory is swallowed: the root falls back to `.` (only reachable when the
/// process has no working directory and no root was configured).
pub fn new_code_set(env: &ToolEnv, node: Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    let cfg: CodeConfig =
        yaml11::decode_mapping(node).map_err(|e| SetError::CodeConfig(e.to_string()))?;
    if cfg.read_only && cfg.auto_write {
        return Err(SetError::CodeContradiction);
    }
    let root = env.root().unwrap_or_else(|_| PathBuf::from("."));
    let cs = CodeSet::new(
        root,
        cfg.auto_write,
        env.interactor.is_some(),
        env.dirs.home.clone(),
        Arc::clone(&env.grants),
    )?;
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(Glob(Arc::clone(&cs))),
        Arc::new(Grep(Arc::clone(&cs))),
        Arc::new(ListDir(Arc::clone(&cs))),
        Arc::new(ReadFile(Arc::clone(&cs))),
    ];
    if !cfg.read_only {
        tools.push(Arc::new(EditFile(Arc::clone(&cs))));
        tools.push(Arc::new(WriteFile(cs)));
    }
    Ok(tools)
}

/// Whether the `tools.code` value `node` sets `read_only` — the writers left out. A value that does not decode
/// says no: the set itself refuses it when it is built.
pub(crate) fn read_only(node: Option<&RawNode>) -> bool {
    yaml11::decode_mapping::<CodeConfig>(node).is_ok_and(|c| c.read_only)
}

/// The extra roots `roots`, `, `-joined; `None` when there are none.
fn extra_list(roots: &[PathBuf]) -> Option<String> {
    let extra: Vec<String> = roots.iter().map(|p| p.display().to_string()).collect();
    (!extra.is_empty()).then(|| extra.join(", "))
}

/// Whether the real directory `dir` may be a root a keypress or a project stands on — the dialog's directory
/// grant, and the root a project's grants are keyed by (docs/design/grants.md §4.2): never `/` (no parent), and
/// never the home directory or an ancestor of it — one keypress on a write to `~/x.txt` would otherwise open
/// `~/.ssh` and every shell rc file, and a project at `~` would be every later run's under it. The home is
/// compared as configured and by its real location.
pub(crate) fn offers_root(home: Option<&Path>, dir: &Path) -> bool {
    if dir.parent().is_none() {
        return false;
    }
    let Some(home) = home else {
        return true;
    };
    let real_home = fs::physical(home);
    ![Some(home.to_path_buf()), real_home]
        .into_iter()
        .flatten()
        .any(|h| paths::within(dir, &h))
}

/// The directory a write to `real` would be granted under: its parent, or `real` itself when it has none
/// (`/`, which [`offers_root`] never offers).
pub(crate) fn grant_dir(real: &Path) -> &Path {
    real.parent().unwrap_or(real)
}

/// A protected write's refusal: `{arg} is protected: {reason} — ` and then what is left to do. The trust
/// record and the sessions tree have no approval path at all; another protected path needs the user's approval
/// of this one file, which a run with nobody to ask cannot give — the user edits it themselves.
fn protected_refusal(arg: &str, real: &Path, why: Why, attended: bool) -> String {
    let rest = match (why, attended) {
        (Why::Trust, _) => "no tool may write it; the user trusts a project config by running \
                            `iota config trust` themselves"
            .to_owned(),
        (Why::Grants, _) => "no tool may write it; iota alone writes the sessions and the \
                             grants they hold"
            .to_owned(),
        (_, true) => format!(
            "writing it needs the user's approval of this file, and this call has none for {}",
            real.display()
        ),
        (_, false) => "writing it needs the user's approval, which this \
                       non-interactive run cannot ask for; ask the user to make the \
                       change themselves"
            .to_owned(),
    };
    format!("{arg} is protected: {} — {rest}", why.reason())
}

/// What an outside write's header and approval prompt say: the real location, absolute, marked
/// [`OUTSIDE_ROOT_MARK`]. Display only: the call checks [`RunCtx::approved`] as a path.
pub(crate) fn outside_summary(real: &Path) -> String {
    format!("{} {OUTSIDE_ROOT_MARK}", real.display())
}

/// NUL within the first 8000 bytes.
pub(crate) fn looks_binary(data: &[u8]) -> bool {
    let n = data.len().min(CODE_BINARY_SNIFF_LEN);
    data[..n].contains(&0)
}

/// `"%.1f MB"` (≥ 1<<20) / `"%.1f KB"` (≥ 1<<10) / `"%d B"`.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn byte_count(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / f64::from(1_u32 << 20))
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / f64::from(1_u32 << 10))
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use crate::tool::ToolOutput;
    use std::path::{Path, PathBuf};

    use super::{CodeSet, byte_count, looks_binary};
    use crate::tool::context::RunCtx;

    /// What follows a refused outside path in a run with nobody to ask: how to allow it.
    pub(crate) const UNATTENDED: &str = " — writing there needs the user's approval, which this non-interactive run cannot ask for; pass --allow <dir>, or list its directory in the agent's `allow:`, to allow it";

    /// What a writer's call is told about `arg` with no approval: the cleaned path, or the refusal.
    pub(crate) fn jail(cs: &CodeSet, arg: &str) -> Result<PathBuf, ToolOutput> {
        cs.write_target(&RunCtx::default(), arg).map(|(p, _)| p)
    }

    #[test]
    fn byte_count_thresholds() {
        assert_eq!(byte_count(0), "0 B");
        assert_eq!(byte_count(5), "5 B");
        assert_eq!(byte_count(1023), "1023 B");
        assert_eq!(byte_count(1024), "1.0 KB");
        assert_eq!(byte_count(1229), "1.2 KB");
        assert_eq!(byte_count(1024 * 1024 - 1), "1024.0 KB");
        assert_eq!(byte_count(1024 * 1024), "1.0 MB");
        assert_eq!(byte_count(3_565_158), "3.4 MB");
    }

    #[test]
    fn binary_sniff_is_capped_at_8000_bytes() {
        assert!(!looks_binary(b""));
        assert!(!looks_binary(b"plain text"));
        assert!(looks_binary(b"ma\x00in"));
        let mut late = vec![b'x'; 9000];
        late.push(0);
        assert!(!looks_binary(&late), "a NUL past 8000 bytes is not sniffed");
        let mut early = vec![b'x'; 7999];
        early.push(0);
        assert!(looks_binary(&early));
    }

    #[test]
    fn paths_and_display_are_lexical() {
        let cs = CodeSet::new(
            PathBuf::from("/proj"),
            false,
            false,
            None,
            std::sync::Arc::default(),
        )
        .expect("set");
        // Reads are open: join, clean, nothing else — no root is consulted.
        for (arg, want) in [
            ("a.txt", "/proj/a.txt"),
            ("  a/b/../c  ", "/proj/a/c"),
            ("../outside.txt", "/outside.txt"),
            ("a/../../outside.txt", "/outside.txt"),
            ("/etc/passwd", "/etc/passwd"),
            ("..", "/"),
        ] {
            assert_eq!(cs.read_path(arg), Ok(PathBuf::from(want)), "{arg}");
        }
        assert_eq!(
            cs.read_path("   "),
            Err(ToolOutput::err("missing required argument: path"))
        );
        // Writes are jailed.
        assert_eq!(jail(&cs, "a.txt"), Ok(PathBuf::from("/proj/a.txt")));
        assert_eq!(jail(&cs, "  a/b/../c  "), Ok(PathBuf::from("/proj/a/c")));
        assert_eq!(jail(&cs, "/proj/x"), Ok(PathBuf::from("/proj/x")));
        assert_eq!(jail(&cs, "."), Ok(PathBuf::from("/proj")));
        assert_eq!(
            jail(&cs, "   "),
            Err(ToolOutput::err("missing required argument: path"))
        );
        for arg in ["../outside.txt", "a/../../outside.txt", "/etc/passwd", ".."] {
            assert_eq!(
                jail(&cs, arg),
                Err(ToolOutput::err(format!(
                    "path is outside the project root (/proj): {arg}{UNATTENDED}"
                ))),
                "{arg}"
            );
        }
        assert_eq!(cs.display(Path::new("/proj")), ".");
        assert_eq!(cs.display(Path::new("/proj/a/b.rs")), "a/b.rs");
    }

    /// A temp dir holding `root/` (the project, with `a.txt` and `dir/`) and `out/` (outside it,
    /// with `secret.txt` and `deep/`).
    #[cfg(unix)]
    pub(crate) fn jail_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        let root = t.path().join("root");
        let out = t.path().join("out");
        std::fs::create_dir_all(root.join("dir")).expect("root/dir");
        std::fs::create_dir_all(out.join("deep")).expect("out/deep");
        std::fs::write(root.join("a.txt"), "a").expect("a.txt");
        std::fs::write(out.join("secret.txt"), "s").expect("secret.txt");
        (t, root, out)
    }

    #[cfg(unix)]
    fn outside(root: &Path, arg: &str) -> Result<PathBuf, ToolOutput> {
        Err(ToolOutput::err(format!(
            "path is outside the project root ({}): {arg}{UNATTENDED}",
            root.display()
        )))
    }

    /// What `write_path` says about a path whose real location it cannot work out.
    #[cfg(unix)]
    fn unverified(root: &Path, arg: &str) -> Result<PathBuf, ToolOutput> {
        Err(ToolOutput::err(format!(
            "cannot verify {arg} inside the project root ({})",
            root.display()
        )))
    }

    /// The three outcomes: a root holds it (the lexical path), outside with its real location known (the
    /// lexical path and the real one, through a symlinked parent too), or unverifiable.
    #[cfg(unix)]
    #[test]
    fn write_path_classifies_inside_outside_and_unverifiable() {
        use super::Write;
        use std::os::unix::fs::symlink;
        let (_t, root, out) = jail_fixture();
        symlink(&out, root.join("outdir")).expect("link");
        symlink("lb", root.join("la")).expect("la");
        symlink("la", root.join("lb")).expect("lb");
        let real_out = crate::app::canonical(&out).expect("real out");
        let cs =
            CodeSet::new(root.clone(), false, true, None, std::sync::Arc::default()).expect("set");
        assert_eq!(
            cs.write_path("dir/x"),
            Ok(Write::Inside(root.join("dir/x")))
        );
        assert_eq!(
            cs.write_path("outdir/n/x.txt"),
            Ok(Write::Outside {
                path: root.join("outdir/n/x.txt"),
                real: real_out.join("n/x.txt"),
            })
        );
        assert_eq!(
            cs.write_path("../out/secret.txt"),
            Ok(Write::Outside {
                path: out.join("secret.txt"),
                real: real_out.join("secret.txt"),
            })
        );
        assert_eq!(cs.write_path("la"), Ok(Write::Unverifiable));
        assert_eq!(
            cs.write_path(" "),
            Err(ToolOutput::err("missing required argument: path"))
        );
    }

    /// An outside write in a run with a user is asked — the subject names its real location and offers its
    /// real directory — and lands only with "allow once" for exactly that location, refused without saying this
    /// call has none; in a run with nobody to ask it is never asked, and its refusal says how to allow it (a
    /// directory to pass or list), "allow once" or not. There is no third way: no run refuses without either.
    #[cfg(unix)]
    #[test]
    fn an_outside_write_asks_or_says_how_to_allow_it() {
        use crate::tool::context::Approved;
        let (_t, root, out) = jail_fixture();
        let real = crate::app::canonical(&out).expect("real out").join("x.txt");
        let arg = "../out/x.txt";
        let refusal = format!(
            "path is outside the project root ({}): {arg}",
            root.display()
        );
        let once = RunCtx {
            approved: Some(Approved::Once(real.clone())),
            ..RunCtx::default()
        };
        let set = |attended| {
            CodeSet::new(
                root.clone(),
                true,
                attended,
                None,
                std::sync::Arc::default(),
            )
            .expect("set")
        };

        let attended = set(true);
        let mut args = crate::provider::model::JsonObject::new();
        args.insert("path".to_owned(), arg.into());
        assert!(
            attended.write_asks(Some(&args)),
            "asked, `auto_write` or not"
        );
        assert_eq!(
            attended.approvable(arg),
            Some(crate::tool::ApprovalSubject {
                detail: format!("{} (outside the project root)", real.display()),
                target: real.clone(),
                scope: real.parent().map(Path::to_path_buf),
            })
        );
        assert_eq!(
            jail(&attended, arg),
            Err(ToolOutput::err(format!(
                "{refusal} — writing there needs the user's approval, and this call has none for {}",
                real.display()
            )))
        );
        assert_eq!(
            attended.write_target(&once, arg),
            Ok((out.join("x.txt"), real.clone()))
        );

        let unattended = set(false);
        assert!(!unattended.write_asks(Some(&args)), "nobody to ask");
        assert_eq!(unattended.approvable(arg), None);
        for cx in [RunCtx::default(), once] {
            assert_eq!(
                unattended.write_target(&cx, arg).map(|(p, _)| p),
                Err(ToolOutput::err(format!("{refusal}{UNATTENDED}")))
            );
        }
    }

    /// A static directory of the store (`allow:`, `--allow`) is a root: a path under it is accepted (relative
    /// arguments still hang off the project root), a path under no root is refused naming every root by its
    /// real location, a link inside a static directory that leaves every root is refused, and the display
    /// stays root-anchored.
    #[cfg(unix)]
    #[test]
    fn the_jail_admits_a_static_directory_from_the_store() {
        use std::os::unix::fs::symlink;
        let (t, root, out) = jail_fixture();
        let other = t.path().join("other");
        std::fs::create_dir(&other).expect("other");
        let missing = t.path().join("not/yet");
        symlink(out.join("secret.txt"), other.join("escape")).expect("link");
        symlink(root.join("a.txt"), other.join("home")).expect("link");
        let real = |p: &Path| crate::app::fs::physical(p).expect("real");
        let mut fixed = crate::tool::grants::GrantSet::default();
        fixed.add_dir(real(&other));
        fixed.add_dir(real(&missing));
        let cs = CodeSet::new(
            root.clone(),
            false,
            false,
            None,
            std::sync::Arc::new(crate::tool::grants::Grants::with_static(fixed)),
        )
        .expect("set");
        let abs = |p: &Path| p.to_str().expect("utf-8").to_owned();
        for p in [
            other.join("new.txt"),
            other.join("home"),
            other.clone(),
            missing.join("x.txt"),
        ] {
            assert_eq!(jail(&cs, &abs(&p)), Ok(p.clone()), "{p:?}");
        }
        assert_eq!(jail(&cs, "../other/x"), Ok(other.join("x")));
        assert_eq!(jail(&cs, "a.txt"), Ok(root.join("a.txt")));
        let roots = format!(
            "{}) and the extra write roots ({}, {}",
            root.display(),
            real(&other).display(),
            real(&missing).display()
        );
        for arg in [
            abs(&out.join("secret.txt")),
            abs(&other.join("escape")),
            "../out/x".to_owned(),
        ] {
            assert_eq!(
                jail(&cs, &arg),
                Err(ToolOutput::err(format!(
                    "path is outside the project root ({roots}): {arg}{UNATTENDED}"
                ))),
                "{arg}"
            );
        }
        symlink("lb", other.join("la")).expect("la");
        symlink("la", other.join("lb")).expect("lb");
        let arg = abs(&other.join("la"));
        assert_eq!(
            jail(&cs, &arg),
            Err(ToolOutput::err(format!(
                "cannot verify {arg} inside the project root ({roots})"
            )))
        );
        assert_eq!(
            cs.display(&other.join("new.txt")),
            abs(&other.join("new.txt"))
        );
        assert_eq!(cs.display(&root.join("dir/x")), "dir/x");
    }

    #[cfg(unix)]
    #[test]
    fn write_path_refuses_symlinks_that_leave_the_root() {
        use std::os::unix::fs::symlink;
        let (_t, root, out) = jail_fixture();
        symlink(out.join("secret.txt"), root.join("escape")).expect("link");
        symlink(&out, root.join("outdir")).expect("link");
        symlink(out.join("missing.txt"), root.join("dangling")).expect("link");
        symlink(out.join("nodir"), root.join("dangling_dir")).expect("link");
        // `..` in a link's text is taken physically: `s/..` is `out/`, not `root/`.
        symlink(out.join("deep"), root.join("s")).expect("link");
        symlink("s/../secret.txt", root.join("trick")).expect("link");
        // The same through the dangling-link branch: the link text is walked again, physically.
        symlink("s/../missing.txt", root.join("trick_dangling")).expect("link");
        let cs =
            CodeSet::new(root.clone(), false, false, None, std::sync::Arc::default()).expect("set");
        for arg in [
            "escape",
            "outdir",
            "outdir/new.txt",
            "outdir/secret.txt",
            "dangling",
            "dangling_dir/new.txt",
            "trick",
            "trick_dangling",
        ] {
            assert_eq!(jail(&cs, arg), outside(&root, arg), "{arg}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_path_allows_symlinks_that_stay_inside() {
        use std::os::unix::fs::symlink;
        let (_t, root, _out) = jail_fixture();
        symlink(root.join("a.txt"), root.join("alias")).expect("link");
        symlink("dir", root.join("subdir")).expect("link");
        symlink("dir/later.txt", root.join("dangling_inside")).expect("link");
        let cs =
            CodeSet::new(root.clone(), false, false, None, std::sync::Arc::default()).expect("set");
        // The returned path is the lexical one, links and all.
        for arg in [
            "alias",
            "subdir",
            "subdir/new.txt",
            "subdir/x/y.txt",
            "dangling_inside",
        ] {
            assert_eq!(jail(&cs, arg), Ok(root.join(arg)), "{arg}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_path_accepts_a_root_reached_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let (t, root, _out) = jail_fixture();
        let via = t.path().join("via");
        symlink(&root, &via).expect("link");
        let real = crate::app::canonical(&root).expect("canonical root");
        let cs =
            CodeSet::new(via.clone(), false, false, None, std::sync::Arc::default()).expect("set");
        assert_eq!(jail(&cs, "new.txt"), Ok(via.join("new.txt")));
        let abs = real.join("new.txt");
        let abs = abs.to_str().expect("utf-8");
        assert_eq!(jail(&cs, abs), Ok(PathBuf::from(abs)));
        // The failure text names the configured root, not its real location.
        assert_eq!(jail(&cs, "../out/x"), outside(&via, "../out/x"));

        // macOS: `/tmp` is a link to `/private/tmp`, and a root under one accepts the other.
        let tmp = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
        let cs = CodeSet::new(
            tmp.path().to_path_buf(),
            false,
            false,
            None,
            std::sync::Arc::default(),
        )
        .expect("set");
        let real = crate::app::canonical(tmp.path()).expect("canonical tmp");
        assert_eq!(jail(&cs, "new.txt"), Ok(tmp.path().join("new.txt")));
        let abs = real.join("new.txt");
        let abs = abs.to_str().expect("utf-8");
        assert_eq!(jail(&cs, abs), Ok(PathBuf::from(abs)));
    }

    /// A walk base is its real location, spelled under the configured root that really holds it: a
    /// root reached through a link keeps its spelling, a link to a project directory walks that
    /// directory, a link out of the project walks the real target.
    #[cfg(unix)]
    #[test]
    fn walk_base_is_the_real_location_under_its_root() {
        use std::os::unix::fs::symlink;
        let (t, root, out) = jail_fixture();
        let via = t.path().join("via");
        symlink(&root, &via).expect("via");
        symlink("dir", root.join("subdir")).expect("subdir");
        symlink(&out, root.join("outdir")).expect("outdir");
        let cs =
            CodeSet::new(via.clone(), false, false, None, std::sync::Arc::default()).expect("set");
        let real_out = crate::app::canonical(&out).expect("real out");
        for (base, want) in [
            (via.clone(), via.clone()),
            (via.join("dir"), via.join("dir")),
            (via.join("subdir"), via.join("dir")),
            (root.join("subdir"), via.join("dir")),
            (via.join("outdir"), real_out.clone()),
            (out.join("deep"), real_out.join("deep")),
            (t.path().join("missing"), t.path().join("missing")),
        ] {
            assert_eq!(cs.walk_base(base.clone()), want, "{base:?}");
        }
    }

    /// Plants the review's B1 links under `root`: `D/D/D/D/S -> out` (each `D` a 250-char name,
    /// so the resolved prefix passes macOS's 1024-byte `PATH_MAX`), `L -> "D/D/D/D"` and
    /// `L2 -> "D/D/D/D/S/../out/secret.txt"`. Built with relative `mkdir`/`ln` from a shell,
    /// because the absolute paths are themselves too long for the filesystem calls.
    #[cfg(unix)]
    pub(crate) fn plant_overlong(root: &Path, out: &Path) {
        let d = "D".repeat(250);
        let script = format!(
            "set -e; mkdir -p {d}/{d}/{d}; cd {d}/{d}/{d}; mkdir {d}; cd {d}; ln -s \"$1\" S"
        );
        let status = std::process::Command::new("sh")
            .args(["-c", &script, "sh"])
            .arg(out)
            .current_dir(root)
            .status()
            .expect("sh");
        assert!(status.success(), "planting the overlong tree");
        let deep = format!("{d}/{d}/{d}/{d}");
        std::os::unix::fs::symlink(&deep, root.join("L")).expect("L");
        std::os::unix::fs::symlink(format!("{deep}/S/../out/secret.txt"), root.join("L2"))
            .expect("L2");
    }

    // B1: a resolved prefix past PATH_MAX makes `canonicalize`/`lstat` fail with ENAMETOOLONG
    // while the kernel follows the same links fine. Unknown is refused, never judged lexically.
    #[cfg(unix)]
    #[test]
    fn write_path_refuses_what_it_cannot_resolve() {
        use super::Write;
        use crate::tool::context::Approved;
        use std::os::unix::fs::symlink;
        let (t, root, out) = jail_fixture();
        plant_overlong(&root, &out);
        // On macOS this really is the ENAMETOOLONG branch (Linux's PATH_MAX is 4096, so there
        // the links resolve and the result is an ordinary "outside").
        #[cfg(target_os = "macos")]
        assert!(crate::app::canonical(&root.join("L/S/secret.txt")).is_none());
        assert!(
            std::fs::read_to_string(root.join("L/S/secret.txt")).is_ok(),
            "the kernel follows it"
        );

        // A link loop is ELOOP, not "missing".
        symlink("lb", root.join("la")).expect("la");
        symlink("la", root.join("lb")).expect("lb");
        let cs =
            CodeSet::new(root.clone(), false, false, None, std::sync::Arc::default()).expect("set");
        for arg in ["L/S/secret.txt", "L/S/planted.txt", "L2"] {
            // Linux resolves the long prefix and finds it outside; macOS cannot resolve it at all.
            let want = if cfg!(target_os = "macos") {
                unverified(&root, arg)
            } else {
                outside(&root, arg)
            };
            assert_eq!(jail(&cs, arg), want, "{arg}");
        }
        for arg in ["la", "la/new.txt"] {
            assert_eq!(jail(&cs, arg), unverified(&root, arg), "{arg}");
        }

        // With a user to ask an unknown location is still refused: it is never approvable, never
        // asked about, and neither an approval the call could carry nor a directory grant over it admits it.
        let grants = std::sync::Arc::new(crate::tool::grants::Grants::default());
        let real_t = crate::app::canonical(t.path()).expect("real t");
        grants.update(crate::tool::grants::Level::Session, |s| {
            s.add_dir(real_t.clone())
        });
        let asking = CodeSet::new(root.clone(), true, true, None, grants).expect("set");
        let mut unknown = vec!["la", "la/new.txt"];
        if cfg!(target_os = "macos") {
            unknown.extend(["L/S/secret.txt", "L/S/planted.txt", "L2"]);
        }
        for arg in &unknown {
            assert_eq!(asking.write_path(arg), Ok(Write::Unverifiable), "{arg}");
            assert_eq!(asking.approvable(arg), None, "{arg}");
            let cx = RunCtx {
                approved: Some(Approved::Once(root.join(arg))),
                ..RunCtx::default()
            };
            assert_eq!(
                asking.write_target(&cx, arg).map(|(p, _)| p),
                Err(ToolOutput::err(format!(
                    "cannot verify {arg} inside the project root ({}) and the extra write roots ({})",
                    root.display(),
                    real_t.display()
                ))),
                "{arg}"
            );
        }

        // EACCES under an unreadable directory is not "missing" either (root bypasses modes,
        // so the case only bites when the lstat really fails).
        let locked = root.join("locked");
        std::fs::create_dir(&locked).expect("locked");
        set_mode(&locked, 0o000);
        if std::fs::symlink_metadata(locked.join("x"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            assert_eq!(jail(&cs, "locked/x"), unverified(&root, "locked/x"));
            assert_eq!(asking.write_path("locked/x"), Ok(Write::Unverifiable));
            assert_eq!(asking.approvable("locked/x"), None);
        }
        set_mode(&locked, 0o755);

        // A root whose own real location is unknown fails the set.
        let looped = t.path().join("looped");
        symlink(&looped, &looped).expect("self link");
        assert_eq!(
            CodeSet::new(
                looped.clone(),
                false,
                false,
                None,
                std::sync::Arc::default()
            )
            .err(),
            Some(crate::tool::sets::SetError::CodeRoot(looped))
        );
    }

    #[cfg(unix)]
    fn set_mode(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
}
