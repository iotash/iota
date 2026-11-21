//! Paths and the strings they show up in, as the PRODUCT spells them — the one place a test asks how a path
//! will read, so no test resolves it a second way (`fs::canonicalize` is a lint, `clippy.toml`). Each binary
//! that mounts this uses a subset, hence the `dead_code` allowance.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// `p` as the product resolves and displays it: `iota::app::fs::physical` — symlinks resolved (macOS's `/var`
/// is `/private/var`) and, on Windows, no `\\?\` prefix. A multi-component tail is joined a component at a
/// time by the caller (a literal `"a/b"` keeps its `/` on Windows).
pub fn shown(p: &Path) -> PathBuf {
    iota::app::fs::physical(p).expect("the path resolves")
}

/// The working directory a child started in `dir` reports — its `getcwd`. On Unix that is the resolved path
/// ([`shown`]); on Windows `GetCurrentDirectory` returns the string the child was started with, verbatim
/// (no 8.3 name expanded: `RUNNER~1` stays), so there the path handed to the child IS the expectation.
pub fn child_cwd(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.to_path_buf()
    } else {
        shown(dir)
    }
}

/// `s` as a JSON body spells it: a string in it is escaped — a Windows path's every `\` is `\\` there, and the
/// path as displayed is nowhere in the raw bytes.
pub fn in_json(s: &str) -> String {
    let quoted = serde_json::to_string(s).expect("a JSON string");
    quoted[1..quoted.len() - 1].to_owned()
}
