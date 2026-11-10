//! The `code` toolset: configuration, the shared `CodeSet` (physical path jail, read ledger)
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

use crate::app::paths;
use crate::tool::{Tool, ToolEnv, ToolOutput};
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
/// `grep` clips matched lines to this many bytes.
pub(crate) const CODE_GREP_MAX_LINE_LEN: usize = 500;
/// `grep` context lines are clamped to this.
pub(crate) const CODE_MAX_GREP_CONTEXT: i64 = 10;
/// `list_dir` lists at most this many entries.
pub(crate) const CODE_MAX_DIR_ENTRIES: usize = 500;
/// Bytes sniffed for a NUL by `looks_binary`.
pub(crate) const CODE_BINARY_SNIFF_LEN: usize = 8000;

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

/// State shared by the six tools: the project root, the approval policy and the read ledger.
pub(crate) struct CodeSet {
    root: PathBuf,
    /// `root` with its symlinks resolved (see [`physical`]) — what the jail compares the
    /// targets' real locations against.
    real_root: PathBuf,
    /// The process working directory — the display anchor of the header-path ladder
    /// (captured at set construction), not an execution dir.
    cwd: PathBuf,
    auto_write: bool,
    reads: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl CodeSet {
    /// A set rooted at `root` (the header-path anchor is the process cwd; when it cannot be
    /// read it stays empty and the ladder skips its rungs). A root whose real location cannot
    /// be worked out (see [`physical`]) fails the set: there is nothing to jail paths to.
    pub(crate) fn new(root: PathBuf, auto_write: bool) -> Result<Arc<Self>, SetError> {
        let real_root = physical(&root).ok_or_else(|| SetError::CodeRoot(root.clone()))?;
        Ok(Arc::new(Self {
            root,
            real_root,
            cwd: std::env::current_dir().unwrap_or_default(),
            auto_write,
            reads: Mutex::new(HashMap::new()),
        }))
    }

    /// The `"path"` argument rendered for a call header. A missing path (a malformed call)
    /// yields `""` — a bare
    /// `"[read_file]"` — rather than falling back to a digest of whatever else the model
    /// sent.
    pub(crate) fn header_arg(&self, args: &crate::provider::model::JsonObject) -> String {
        crate::tool::fmt::header_path(
            crate::tool::args::str_arg(args, "path"),
            &self.cwd,
            &self.root,
        )
    }

    /// The path jail. Err texts: `missing required argument: path`, `path is outside the project
    /// root ({root}): {original arg}`.
    ///
    /// The argument is joined to the root and cleaned lexically; that cleaned path is what is
    /// returned. The decision is physical: the path's real location (symlinks resolved, see
    /// [`physical`]) must lie inside the root's real location, so a symlink under the root that
    /// points outside it — dangling or not — is refused. The target need not exist, so
    /// `write_file` can create one. A path whose real location cannot be worked out is refused.
    ///
    /// A `..` in the argument is folded before any link is resolved (`link/../x` is `x`, not
    /// the shell's "`x` next to where `link` points"); check and open use that same path.
    pub(crate) fn resolve(&self, arg: &str) -> Result<PathBuf, ToolOutput> {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return Err(ToolOutput::err("missing required argument: path"));
        }
        let mut p = PathBuf::from(trimmed);
        if !p.is_absolute() {
            p = self.root.join(p);
        }
        let p = paths::clean(&p);
        if self.contains(&p) {
            Ok(p)
        } else {
            Err(ToolOutput::err(format!(
                "path is outside the project root ({}): {arg}",
                self.root.display()
            )))
        }
    }

    /// Whether the real location of the absolute, cleaned `p` lies inside the root's — the
    /// jail's decision, for `resolve` and for the symlinks a walk meets.
    pub(crate) fn contains(&self, p: &Path) -> bool {
        physical(p).is_some_and(|real| within(&self.real_root, &real))
    }

    /// Whether a walk (`glob`, `grep`) may report `entry` at `abs`: anything but a symlink, or
    /// a symlink whose real location the jail accepts — a link out of the root is invisible.
    pub(crate) fn admits(&self, abs: &Path, entry: &walkdir::DirEntry) -> bool {
        !entry.path_is_symlink() || self.contains(abs)
    }

    /// Root-relative slash path; `"."` for the root; absolute when `rel` fails.
    pub(crate) fn display(&self, abs: &Path) -> String {
        paths::rel(&self.root, abs)
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
    let cs = CodeSet::new(root, cfg.auto_write)?;
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

/// Symlink hops [`physical`] follows before giving up (Linux's own `MAXSYMLINKS`).
const MAX_SYMLINK_HOPS: usize = 40;

/// The real location of `p`, which need not exist: its nearest existing ancestor
/// canonicalized, with the missing tail re-appended lexically. An ancestor that is a dangling
/// symlink cannot be canonicalized, so its link text is followed instead (relative to the
/// link's own directory) — a dangling link still reports where a write through it would land.
///
/// Only `NotFound` counts as "missing". Every other failure — `ENAMETOOLONG` (a resolved
/// prefix past `PATH_MAX`, which the kernel itself follows happily), `EACCES`, `ELOOP`,
/// `ENOTDIR`, too many hops — is `None`: the location is unknown and the caller refuses it.
fn physical(p: &Path) -> Option<PathBuf> {
    use std::io::ErrorKind::NotFound;
    let mut path = p.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        // Walk up to the nearest ancestor that exists (a dangling link exists for lstat).
        let mut tail = Vec::new();
        let mut at = path.as_path();
        let meta = loop {
            match std::fs::symlink_metadata(at) {
                Ok(meta) => break meta,
                Err(e) if e.kind() == NotFound => {
                    tail.push(at.components().next_back()?.as_os_str().to_owned());
                    at = at.parent()?;
                }
                Err(_) => return None,
            }
        };
        let (base, done) = match std::fs::canonicalize(at) {
            Ok(c) => (c, true),
            Err(e) if e.kind() == NotFound && meta.file_type().is_symlink() => {
                let target = std::fs::read_link(at).ok()?;
                (
                    std::fs::canonicalize(at.parent()?).ok()?.join(target),
                    false,
                )
            }
            Err(_) => return None,
        };
        let next = tail.iter().rev().fold(base, |acc, c| acc.join(c));
        if done {
            // The tail does not exist, so its `..` (if any) can only be read lexically.
            return Some(paths::clean(&next));
        }
        // The link text is resolved physically on the next pass, `..` and all.
        path = next;
    }
    None
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
    fn resolve_and_display_are_lexical() {
        let cs = CodeSet::new(PathBuf::from("/proj"), false).expect("set");
        assert_eq!(cs.resolve("a.txt"), Ok(PathBuf::from("/proj/a.txt")));
        assert_eq!(cs.resolve("  a/b/../c  "), Ok(PathBuf::from("/proj/a/c")));
        assert_eq!(cs.resolve("/proj/x"), Ok(PathBuf::from("/proj/x")));
        assert_eq!(cs.resolve("."), Ok(PathBuf::from("/proj")));
        assert_eq!(
            cs.resolve("   "),
            Err(ToolOutput::err("missing required argument: path"))
        );
        for arg in ["../outside.txt", "a/../../outside.txt", "/etc/passwd", ".."] {
            assert_eq!(
                cs.resolve(arg),
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

    #[cfg(unix)]
    #[test]
    fn resolve_refuses_symlinks_that_leave_the_root() {
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
        let cs = CodeSet::new(root.clone(), false).expect("set");
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
            assert_eq!(cs.resolve(arg), outside(&root, arg), "{arg}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_allows_symlinks_that_stay_inside() {
        use std::os::unix::fs::symlink;
        let (_t, root, _out) = jail_fixture();
        symlink(root.join("a.txt"), root.join("alias")).expect("link");
        symlink("dir", root.join("subdir")).expect("link");
        symlink("dir/later.txt", root.join("dangling_inside")).expect("link");
        let cs = CodeSet::new(root.clone(), false).expect("set");
        // The returned path is the lexical one, links and all.
        for arg in [
            "alias",
            "subdir",
            "subdir/new.txt",
            "subdir/x/y.txt",
            "dangling_inside",
        ] {
            assert_eq!(cs.resolve(arg), Ok(root.join(arg)), "{arg}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_accepts_a_root_reached_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let (t, root, _out) = jail_fixture();
        let via = t.path().join("via");
        symlink(&root, &via).expect("link");
        let real = std::fs::canonicalize(&root).expect("canonical root");
        let cs = CodeSet::new(via.clone(), false).expect("set");
        assert_eq!(cs.resolve("new.txt"), Ok(via.join("new.txt")));
        let abs = real.join("new.txt");
        let abs = abs.to_str().expect("utf-8");
        assert_eq!(cs.resolve(abs), Ok(PathBuf::from(abs)));
        // The failure text names the configured root, not its real location.
        assert_eq!(cs.resolve("../out/x"), outside(&via, "../out/x"));

        // macOS: `/tmp` is a link to `/private/tmp`, and a root under one accepts the other.
        let tmp = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
        let cs = CodeSet::new(tmp.path().to_path_buf(), false).expect("set");
        let real = std::fs::canonicalize(tmp.path()).expect("canonical tmp");
        assert_eq!(cs.resolve("new.txt"), Ok(tmp.path().join("new.txt")));
        let abs = real.join("new.txt");
        let abs = abs.to_str().expect("utf-8");
        assert_eq!(cs.resolve(abs), Ok(PathBuf::from(abs)));
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
    fn resolve_refuses_what_it_cannot_resolve() {
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
        let cs = CodeSet::new(root.clone(), false).expect("set");
        for arg in [
            "L/S/secret.txt",
            "L/S/planted.txt",
            "L2",
            "la",
            "la/new.txt",
        ] {
            assert_eq!(cs.resolve(arg), outside(&root, arg), "{arg}");
        }

        // EACCES under an unreadable directory is not "missing" either (root bypasses modes,
        // so the case only bites when the lstat really fails).
        let locked = root.join("locked");
        std::fs::create_dir(&locked).expect("locked");
        set_mode(&locked, 0o000);
        if std::fs::symlink_metadata(locked.join("x"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            assert_eq!(cs.resolve("locked/x"), outside(&root, "locked/x"));
        }
        set_mode(&locked, 0o755);

        // A root whose own real location is unknown fails the set.
        let looped = t.path().join("looped");
        symlink(&looped, &looped).expect("self link");
        assert_eq!(
            CodeSet::new(looped.clone(), false).err(),
            Some(crate::tool::sets::SetError::CodeRoot(looped))
        );
    }

    #[cfg(unix)]
    fn set_mode(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
}
