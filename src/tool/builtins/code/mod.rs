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

use crate::app::protected::{self, Reach, Why};
use crate::app::{fs, paths};
use crate::tool::context::{Approved, RunCtx};
use crate::tool::{ApprovalSubject, OutsideWrites, Tool, ToolEnv, ToolOutput};
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

/// What the writers do with a path outside every root whose real location is known: the top-level
/// `outside_writes:` met with whether the run has a user to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outside {
    /// `ask` with a user (an interactor is bound): the write is put to the user, every time.
    Ask,
    /// `ask` with nobody to ask (`-m`, the SDK path): refused, saying how to allow it (`write_roots:`).
    Unattended,
    /// `refuse`: refused with the jail's own text, no prompt — the behaviour before writes could be approved.
    Refuse,
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
    /// user's approval of this one file, whatever `auto_write`, `write_roots:` or a session grant says, and the
    /// trust record never at all. `path` and `real` as for [`Write::Outside`].
    Protected {
        path: PathBuf,
        real: PathBuf,
        why: Why,
    },
    /// The real location cannot be worked out: refused, never approvable.
    Unverifiable,
}

/// State shared by the six tools: the project root, the extra write roots, the approval policy and
/// the read ledger.
pub(crate) struct CodeSet {
    root: Root,
    /// `ToolEnv::write_roots`: further roots the write jail accepts. Relative arguments and every display
    /// stay anchored on `root`.
    extra: Vec<Root>,
    /// The process working directory — the display anchor of the header-path ladder
    /// (captured at set construction), not an execution dir.
    cwd: PathBuf,
    auto_write: bool,
    outside: Outside,
    /// The user's home, whose config and trust record the protected table names; None without one.
    home: Option<PathBuf>,
    reads: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl CodeSet {
    /// A set rooted at `root`, also admitting `extra` (the header-path anchor is the process cwd;
    /// when it cannot be read it stays empty and the ladder skips its rungs). A root whose real
    /// location cannot be worked out (see [`fs::physical`]) fails the set — `CodeRoot` for the
    /// project root, there being nothing to jail paths to, `WriteRoot` for an extra one, which is
    /// never silently dropped. `home` is the user's home, for the protected table.
    pub(crate) fn new(
        root: PathBuf,
        extra: Vec<PathBuf>,
        auto_write: bool,
        outside: Outside,
        home: Option<PathBuf>,
    ) -> Result<Arc<Self>, SetError> {
        let root = Root::new(root.clone()).ok_or(SetError::CodeRoot(root))?;
        let extra = extra
            .into_iter()
            .map(|p| Root::new(p.clone()).ok_or(SetError::WriteRoot(p)))
            .collect::<Result<_, _>>()?;
        Ok(Arc::new(Self {
            root,
            extra,
            cwd: std::env::current_dir().unwrap_or_default(),
            auto_write,
            outside,
            home,
            reads: Mutex::new(HashMap::new()),
        }))
    }

    /// The project root as configured — the anchor of relative arguments, walks and displays.
    pub(crate) fn root(&self) -> &Path {
        &self.root.path
    }

    /// The root a walk from `base` is anchored on — the project root or the extra root that
    /// lexically contains it, the project root first: its `.gitignore` filters the walk and its
    /// relative paths are what patterns match. `base` itself when no root contains it lexically
    /// (reads are open, so a walk may start anywhere). `base` is a [`Self::walk_base`], so this
    /// lexical test decides by the real location.
    pub(crate) fn anchor<'a>(&'a self, base: &'a Path) -> &'a Path {
        std::iter::once(&self.root)
            .chain(&self.extra)
            .map(|r| r.path.as_path())
            .find(|root| within(root, base))
            .unwrap_or(base)
    }

    /// Where a walk from the existing directory `base` really starts: its real location, expressed
    /// under the configured path of the root whose real location contains it (the project root
    /// first), else the real location itself. So every spelling of one directory — a link to it, a
    /// path through a link, `..`, the absolute path — walks the same tree under the same
    /// `.gitignore` ([`Self::anchor`]) and reports the same paths, and a root reached through a
    /// link keeps its own spelling. `base` unchanged when it cannot be canonicalized.
    pub(crate) fn walk_base(&self, base: PathBuf) -> PathBuf {
        let Ok(real) = std::fs::canonicalize(&base) else {
            return base;
        };
        std::iter::once(&self.root)
            .chain(&self.extra)
            .find_map(|r| {
                within(&r.real, &real)
                    .then(|| paths::rel(&r.real, &real))
                    .flatten()
                    .map(|rel| paths::clean(&r.path.join(rel)))
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
    /// [`fs::physical`]) inside the real location of the project root or of an extra root is
    /// [`Write::Inside`]; a known location outside them all — a symlink under a root that points out of
    /// every root, dangling or not, included — is [`Write::Outside`]; a location that cannot be worked out
    /// is [`Write::Unverifiable`]. Before any of that, a real location on the protected table is
    /// [`Write::Protected`] — a root, `write_roots:` included, never admits one. The target need not exist,
    /// so `write_file` can create one.
    ///
    /// A `..` in the argument is folded before any link is resolved (`link/../x` is `x`, not
    /// the shell's "`x` next to where `link` points"); check and open use that same path.
    pub(crate) fn write_path(&self, arg: &str) -> Result<Write, ToolOutput> {
        let p = self.read_path(arg)?;
        Ok(match fs::physical(&p) {
            Some(real) => match self.protected_at(&real) {
                Some(why) => Write::Protected { path: p, real, why },
                None if self.admits_real(&real) => Write::Inside(p),
                None => Write::Outside { path: p, real },
            },
            None => Write::Unverifiable,
        })
    }

    /// Why `real` is protected, if it is: compared with each table entry's real location, worked out now (a
    /// `.git/hooks` linked elsewhere since the set was built is matched where it points now). A
    /// [`Reach::Tree`] entry holds everything under it, a [`Reach::Entry`] only itself.
    fn protected_at(&self, real: &Path) -> Option<Why> {
        protected::table(&self.root.path, self.home.as_deref())
            .into_iter()
            .find(|e| {
                let at = fs::physical(&e.path).unwrap_or_else(|| e.path.clone());
                match e.reach {
                    Reach::Tree => within(&at, real),
                    Reach::Entry => at == real,
                }
            })
            .map(|e| e.why)
    }

    /// Where a writer's call may write, decided at call time — after any prompt, so a link swapped since is
    /// caught: `(ledger path, target)`. Inside a root both are the lexical path. Outside every root the call
    /// is admitted only under `outside_writes: ask` and only when [`RunCtx::approved`] covers the real
    /// location resolved NOW ([`approves`]: "allow once" names exactly it, a session grant its directory);
    /// then the target is that real location. Err texts: `missing required argument: path`,
    /// `path is outside the project root ({root}): {arg}` (alone under `refuse`; followed under `ask` by why
    /// it was not approved), `cannot verify {arg} inside the project root ({root})` — `{root}` followed,
    /// when `write_roots:` lists any, by `) and the extra write roots ({e1}, {e2}`.
    ///
    /// A protected path ([`Write::Protected`]) is admitted only when "allow once" named exactly its real
    /// location — never by a session grant — and the trust record never: `{arg} is protected: {reason} — …`,
    /// the rest saying what the user can do instead ([`protected_refusal`]).
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
                let why = match self.outside {
                    Outside::Ask if approves(cx.approved.as_ref(), &real) => {
                        return Ok((path, real));
                    }
                    Outside::Ask => format!(
                        " — writing there needs the user's approval, and this call has none for {}",
                        real.display()
                    ),
                    Outside::Unattended => {
                        " — writing there needs the user's approval, which this \
                        non-interactive run cannot ask for; list its directory in the top-level \
                        `write_roots:` to allow it"
                            .to_owned()
                    }
                    Outside::Refuse => String::new(),
                };
                Err(ToolOutput::err(format!("{refusal}{why}")))
            }
            Write::Protected { path, real, why } => {
                if why != Why::Trust
                    && self.outside == Outside::Ask
                    && cx.approved.as_ref() == Some(&Approved::Once(real.clone()))
                {
                    return Ok((path, real));
                }
                Err(ToolOutput::err(protected_refusal(
                    arg,
                    &real,
                    why,
                    self.outside,
                )))
            }
            Write::Unverifiable => Err(ToolOutput::err(format!(
                "cannot verify {arg} inside the project root ({roots})"
            ))),
        }
    }

    /// What a write the user would be asked about is bound to, from one resolution of its real location:
    /// `Some` only under `outside_writes: ask` with a user to ask, for a path outside every root whose location
    /// is known (named [`OUTSIDE_ROOT_MARK`], grantable for its directory) or a protected path other than the
    /// trust record (named [`PROTECTED_MARK`] and why, "allow once" only).
    pub(crate) fn approvable(&self, arg: &str) -> Option<ApprovalSubject> {
        if self.outside != Outside::Ask {
            return None;
        }
        match self.write_path(arg) {
            Ok(Write::Outside { real, .. }) => Some(ApprovalSubject {
                detail: outside_summary(&real),
                scope: Some(grant_dir(&real).to_path_buf()),
                target: real,
            }),
            Ok(Write::Protected { real, why, .. }) if why != Why::Trust => Some(ApprovalSubject {
                detail: format!("{} {PROTECTED_MARK}: {}", real.display(), why.reason()),
                scope: None,
                target: real,
            }),
            _ => None,
        }
    }

    /// Whether the writers' calls are put to the user: inside the roots unless `auto_write`, outside
    /// them always under `outside_writes: ask` with a user to ask; an unverifiable or empty path never
    /// (it is refused at the call). `None` (the argument-less probe) answers whether the tool can ask.
    pub(crate) fn write_asks(&self, args: Option<&crate::provider::model::JsonObject>) -> bool {
        let Some(args) = args else {
            return !self.auto_write || self.outside == Outside::Ask;
        };
        let arg = crate::tool::args::str_arg(args, "path");
        match self.write_path(arg) {
            Ok(Write::Inside(_)) => !self.auto_write,
            Ok(Write::Outside { .. }) => self.outside == Outside::Ask,
            Ok(Write::Protected { why, .. }) => why != Why::Trust && self.outside == Outside::Ask,
            Ok(Write::Unverifiable) | Err(_) => false,
        }
    }

    /// `auto_write`: what the writers waive is the approval of writes INSIDE the roots. A write outside
    /// them, or to a protected path, is still asked (`outside_writes: ask`) or refused, never waived.
    pub(crate) fn waives(&self) -> bool {
        self.auto_write
    }

    /// The writers' description sentence on writes outside the roots: under `outside_writes: ask` with a
    /// user, that such a write asks first (so a model does not take a refusal as final); else nothing.
    pub(crate) fn outside_clause(&self) -> &'static str {
        match self.outside {
            Outside::Ask => " Outside that, the user is asked to approve a write there first.",
            Outside::Unattended | Outside::Refuse => "",
        }
    }

    /// `the project root`, or `the project root or the extra write roots ({e1}, {e2})` — where the
    /// writers' descriptions say a write may land.
    pub(crate) fn write_scope(&self) -> String {
        match self.extra_list() {
            None => "the project root".to_owned(),
            Some(extra) => format!("the project root or the extra write roots ({extra})"),
        }
    }

    /// The extra roots as configured, `, `-joined; `None` when `write_roots:` lists none.
    fn extra_list(&self) -> Option<String> {
        if self.extra.is_empty() {
            return None;
        }
        let extra: Vec<String> = self
            .extra
            .iter()
            .map(|r| r.path.display().to_string())
            .collect();
        Some(extra.join(", "))
    }

    /// `{root}`, or `{root}) and the extra write roots ({e1}, {e2}` — the inside of the error
    /// texts' parentheses.
    fn roots_clause(&self) -> String {
        let root = self.root.path.display();
        match self.extra_list() {
            None => root.to_string(),
            Some(extra) => format!("{root}) and the extra write roots ({extra}"),
        }
    }

    /// Whether a real location lies inside the real location of any root.
    fn admits_real(&self, real: &Path) -> bool {
        std::iter::once(&self.root)
            .chain(&self.extra)
            .any(|r| within(&r.real, real))
    }

    /// Root-relative slash path; `"."` for the root; absolute outside the project root (a read
    /// anywhere, a write under an extra root) or when `rel` fails.
    pub(crate) fn display(&self, abs: &Path) -> String {
        if !within(&self.root.path, abs) {
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
    let outside = match (env.outside_writes, env.interactor.is_some()) {
        (OutsideWrites::Refuse, _) => Outside::Refuse,
        (OutsideWrites::Ask, true) => Outside::Ask,
        (OutsideWrites::Ask, false) => Outside::Unattended,
    };
    let cs = CodeSet::new(
        root,
        env.write_roots.clone(),
        cfg.auto_write,
        outside,
        env.dirs.home.clone(),
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

/// Whether what the gate allowed covers a write to `real`, compared as paths: "allow once" must name
/// exactly it, a session grant its directory ([`grant_dir`]) — the same key the gate stored. Nothing
/// allowed covers nothing.
fn approves(approved: Option<&Approved>, real: &Path) -> bool {
    match approved {
        Some(Approved::Once(target)) => target == real,
        Some(Approved::Dir(dir)) => grant_dir(real) == dir,
        None => false,
    }
}

/// The directory "allow for this session" grants for a write to `real`: its parent, or `real` itself when
/// it has none (`/`) — never nothing, which would be the tool-wide key an inside grant uses.
pub(crate) fn grant_dir(real: &Path) -> &Path {
    real.parent().unwrap_or(real)
}

/// A protected write's refusal: `{arg} is protected: {reason} — ` and then what is left to do. The trust
/// record has no approval path at all; another protected path needs the user's approval of this one file,
/// which `refuse` and a run with nobody to ask cannot give — the user edits it themselves.
fn protected_refusal(arg: &str, real: &Path, why: Why, outside: Outside) -> String {
    let rest = match (why, outside) {
        (Why::Trust, _) => "no tool may write it; the user trusts a project config by running \
                            `iota config trust` themselves"
            .to_owned(),
        (_, Outside::Ask) => format!(
            "writing it needs the user's approval of this file, and this call has none for {}",
            real.display()
        ),
        (_, Outside::Unattended) => "writing it needs the user's approval, which this \
                                     non-interactive run cannot ask for; ask the user to make the \
                                     change themselves"
            .to_owned(),
        (_, Outside::Refuse) => {
            "`outside_writes: refuse` refuses every write that needs approval; \
                                 ask the user to make the change themselves"
                .to_owned()
        }
    };
    format!("{arg} is protected: {} — {rest}", why.reason())
}

/// What an outside write's header and approval prompt say: the real location, absolute, marked
/// [`OUTSIDE_ROOT_MARK`]. Display only: the call checks [`RunCtx::approved`] as paths ([`approves`]).
pub(crate) fn outside_summary(real: &Path) -> String {
    format!("{} {OUTSIDE_ROOT_MARK}", real.display())
}

/// NUL within the first 8000 bytes.
pub(crate) fn looks_binary(data: &[u8]) -> bool {
    let n = data.len().min(CODE_BINARY_SNIFF_LEN);
    data[..n].contains(&0)
}

/// Whether `p` is `root` or lies under it (both compared as given, lexically).
fn within(root: &Path, p: &Path) -> bool {
    paths::rel(root, p).is_some_and(|rel| {
        let rel = paths::to_slash(&rel);
        rel != ".." && !rel.starts_with("../")
    })
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

    use super::{CodeSet, Outside, Write, byte_count, looks_binary};
    use crate::tool::context::{Approved, RunCtx};

    /// What a writer's call is told about `arg` with no approval: under `Outside::Refuse`, exactly the jail
    /// as it was before writes could be approved — the cleaned path, or the refusal.
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
            Vec::new(),
            false,
            Outside::Refuse,
            None,
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
                    "path is outside the project root (/proj): {arg}"
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
            "path is outside the project root ({}): {arg}",
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
        use std::os::unix::fs::symlink;
        let (_t, root, out) = jail_fixture();
        symlink(&out, root.join("outdir")).expect("link");
        symlink("lb", root.join("la")).expect("la");
        symlink("la", root.join("lb")).expect("lb");
        let real_out = std::fs::canonicalize(&out).expect("real out");
        let cs = CodeSet::new(root.clone(), Vec::new(), false, Outside::Ask, None).expect("set");
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

    /// `write_roots:`: a path under an extra root is accepted (relative arguments still hang off
    /// the project root), a path under no root is refused naming every root, a link inside an
    /// extra root that leaves every root is refused, and the display stays root-anchored.
    #[cfg(unix)]
    #[test]
    fn write_path_admits_the_extra_write_roots() {
        use std::os::unix::fs::symlink;
        let (t, root, out) = jail_fixture();
        let other = t.path().join("other");
        std::fs::create_dir(&other).expect("other");
        let missing = t.path().join("not/yet");
        symlink(out.join("secret.txt"), other.join("escape")).expect("link");
        symlink(root.join("a.txt"), other.join("home")).expect("link");
        let cs = CodeSet::new(
            root.clone(),
            vec![other.clone(), missing.clone()],
            false,
            Outside::Refuse,
            None,
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
            other.display(),
            missing.display()
        );
        for arg in [
            abs(&out.join("secret.txt")),
            abs(&other.join("escape")),
            "../out/x".to_owned(),
        ] {
            assert_eq!(
                jail(&cs, &arg),
                Err(ToolOutput::err(format!(
                    "path is outside the project root ({roots}): {arg}"
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

    /// An extra root whose real location is unknown refuses the set, never silently dropped.
    #[cfg(unix)]
    #[test]
    fn an_unresolvable_write_root_refuses_the_set() {
        let (t, root, _out) = jail_fixture();
        let looped = t.path().join("looped");
        std::os::unix::fs::symlink(&looped, &looped).expect("self link");
        assert_eq!(
            CodeSet::new(root, vec![looped.clone()], false, Outside::Refuse, None).err(),
            Some(crate::tool::sets::SetError::WriteRoot(looped))
        );
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
        let cs = CodeSet::new(root.clone(), Vec::new(), false, Outside::Refuse, None).expect("set");
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
        let cs = CodeSet::new(root.clone(), Vec::new(), false, Outside::Refuse, None).expect("set");
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
        let real = std::fs::canonicalize(&root).expect("canonical root");
        let cs = CodeSet::new(via.clone(), Vec::new(), false, Outside::Refuse, None).expect("set");
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
            Vec::new(),
            false,
            Outside::Refuse,
            None,
        )
        .expect("set");
        let real = std::fs::canonicalize(tmp.path()).expect("canonical tmp");
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
        let cs = CodeSet::new(via.clone(), Vec::new(), false, Outside::Refuse, None).expect("set");
        let real_out = std::fs::canonicalize(&out).expect("real out");
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
        use std::os::unix::fs::symlink;
        let (t, root, out) = jail_fixture();
        plant_overlong(&root, &out);
        // On macOS this really is the ENAMETOOLONG branch (Linux's PATH_MAX is 4096, so there
        // the links resolve and the result is an ordinary "outside").
        #[cfg(target_os = "macos")]
        assert!(std::fs::canonicalize(root.join("L/S/secret.txt")).is_err());
        assert!(
            std::fs::read_to_string(root.join("L/S/secret.txt")).is_ok(),
            "the kernel follows it"
        );

        // A link loop is ELOOP, not "missing".
        symlink("lb", root.join("la")).expect("la");
        symlink("la", root.join("lb")).expect("lb");
        let cs = CodeSet::new(root.clone(), Vec::new(), false, Outside::Refuse, None).expect("set");
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

        // Under `outside_writes: ask` an unknown location is still refused: it is never approvable, never
        // asked about, and no approval the call could carry admits it.
        let asking = CodeSet::new(root.clone(), Vec::new(), true, Outside::Ask, None).expect("set");
        let mut unknown = vec!["la", "la/new.txt"];
        if cfg!(target_os = "macos") {
            unknown.extend(["L/S/secret.txt", "L/S/planted.txt", "L2"]);
        }
        for arg in &unknown {
            assert_eq!(asking.write_path(arg), Ok(Write::Unverifiable), "{arg}");
            assert_eq!(asking.approvable(arg), None, "{arg}");
            for approved in [
                Approved::Once(root.join(arg)),
                Approved::Dir(root.clone()),
                Approved::Dir(PathBuf::from("/")),
            ] {
                let cx = RunCtx {
                    approved: Some(approved),
                    ..RunCtx::default()
                };
                assert_eq!(
                    asking.write_target(&cx, arg).map(|(p, _)| p),
                    unverified(&root, arg),
                    "{arg}"
                );
            }
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
            CodeSet::new(looped.clone(), Vec::new(), false, Outside::Refuse, None).err(),
            Some(crate::tool::sets::SetError::CodeRoot(looped))
        );
    }

    #[cfg(unix)]
    fn set_mode(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
}
