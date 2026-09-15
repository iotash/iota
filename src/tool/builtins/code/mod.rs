//! The `code` toolset (tool/code.go:45-220): configuration, the shared `CodeSet` (lexical path jail, read ledger)
//! and the size/binary helpers. The six tools live in `tools`, the gitignore-aware walk in `walk`.

pub(crate) mod tools;
pub mod udiff;
pub(crate) mod walk;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::SystemTime,
};

use crate::app::paths;
use crate::tool::{Tool, ToolEnv};
use serde::Deserialize;

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
    /// The process working directory — the display anchor of the header-path ladder
    /// (tool/code.go `cs.cwd`, captured at set construction), not an execution dir.
    cwd: PathBuf,
    auto_write: bool,
    reads: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl CodeSet {
    /// A set rooted at `root` (the header-path anchor is the process cwd, Go
    /// `os.Getwd()` parity — a failure leaves it empty and the ladder skips its rungs).
    pub(crate) fn new(root: PathBuf, auto_write: bool) -> Arc<Self> {
        Arc::new(Self {
            root,
            cwd: std::env::current_dir().unwrap_or_default(),
            auto_write,
            reads: Mutex::new(HashMap::new()),
        })
    }

    /// The `"path"` argument rendered for a call header (tool/code.go:647-653
    /// `headerArg`). A missing path (a malformed call) yields `""` — a bare
    /// `"[read_file]"` — rather than falling back to a digest of whatever else the model
    /// sent.
    pub(crate) fn header_arg(&self, args: &crate::provider::model::JsonObject) -> String {
        crate::tool::fmt::header_path(
            crate::tool::args::str_arg(args, "path"),
            &self.cwd,
            &self.root,
        )
    }

    /// code.go:103-118 (lexical jail). Err texts: `missing required argument: path`, `path is outside the project
    /// root ({root}): {original arg}`.
    ///
    /// Lexical only: symlinks are never resolved and the target need not exist, so `write_file` can create one.
    pub(crate) fn resolve(&self, arg: &str) -> Result<PathBuf, String> {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return Err("missing required argument: path".to_owned());
        }
        let mut p = PathBuf::from(trimmed);
        if !p.is_absolute() {
            p = self.root.join(p);
        }
        let p = paths::clean(&p);
        let outside = || {
            Err(format!(
                "path is outside the project root ({}): {arg}",
                self.root.display()
            ))
        };
        match paths::rel(&self.root, &p) {
            None => outside(),
            Some(rel) => {
                let rel = paths::to_slash(&rel);
                if rel == ".." || rel.starts_with("../") {
                    outside()
                } else {
                    Ok(p)
                }
            }
        }
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
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(abs.to_path_buf(), mtime);
    }

    /// `{d} has not been read in this session — read it with read_file before modifying it` / `cannot access {d}:
    /// {e}` / `{d} changed on disk after it was read — read it again before modifying it`.
    pub(crate) fn require_fresh_read(&self, abs: &Path) -> Result<(), String> {
        let stamp = self
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(abs)
            .copied();
        let Some(stamp) = stamp else {
            return Err(format!(
                "{} has not been read in this session — read it with read_file before modifying it",
                self.display(abs)
            ));
        };
        let mtime = std::fs::metadata(abs)
            .and_then(|m| m.modified())
            .map_err(|e| format!("cannot access {}: {e}", self.display(abs)))?;
        if mtime == stamp {
            Ok(())
        } else {
            Err(format!(
                "{} changed on disk after it was read — read it again before modifying it",
                self.display(abs)
            ))
        }
    }
}

/// Order `glob`, `grep`, `list_dir`, `read_file`, [`edit_file`, `write_file` unless `read_only`]. Errors: `CodeConfig`,
/// `CodeContradiction`.
///
/// Go's `Env.Root()` swallows a failing `os.Getwd`; so does this, falling back to `.` (only reachable when the
/// process has no working directory and no root was configured).
pub fn new_code_set(env: &ToolEnv, node: Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    let cfg: CodeConfig = yaml11::decode_mapping(node).map_err(SetError::CodeConfig)?;
    if cfg.read_only && cfg.auto_write {
        return Err(SetError::CodeContradiction);
    }
    let root = env.root().unwrap_or_else(|_| PathBuf::from("."));
    let cs = CodeSet::new(root, cfg.auto_write);
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
        let cs = CodeSet::new(PathBuf::from("/proj"), false);
        assert_eq!(cs.resolve("a.txt"), Ok(PathBuf::from("/proj/a.txt")));
        assert_eq!(cs.resolve("  a/b/../c  "), Ok(PathBuf::from("/proj/a/c")));
        assert_eq!(cs.resolve("/proj/x"), Ok(PathBuf::from("/proj/x")));
        assert_eq!(cs.resolve("."), Ok(PathBuf::from("/proj")));
        assert_eq!(
            cs.resolve("   "),
            Err("missing required argument: path".to_owned())
        );
        for arg in ["../outside.txt", "a/../../outside.txt", "/etc/passwd", ".."] {
            assert_eq!(
                cs.resolve(arg),
                Err(format!("path is outside the project root (/proj): {arg}")),
                "{arg}"
            );
        }
        assert_eq!(cs.display(Path::new("/proj")), ".");
        assert_eq!(cs.display(Path::new("/proj/a/b.go")), "a/b.go");
    }
}
