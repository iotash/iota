//! Gitignore-aware file walk on `walkdir` + the `ignore` crate's gitignore matcher
//! (real git semantics — DIVERGENCES D-17).

use std::{ffi::OsStr, path::Path};

use crate::app::paths;
use walkdir::WalkDir;

/// `root/.gitignore` via `GitignoreBuilder::new(root).add(..).build()`; None when absent/unreadable.
///
/// Recompiled on every `glob`/`grep` call, so an edit to the file applies immediately.
pub(crate) fn ignore_matcher(root: &Path) -> Option<ignore::gitignore::Gitignore> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    if builder.add(root.join(".gitignore")).is_some() {
        return None;
    }
    builder.build().ok()
}

/// `WalkDir::new(base).follow_links(false).sort_by_file_name()`, pruning `.git` at any depth and gitignore-matched
/// dirs (other than `base`), skipping gitignore-matched files and erroring entries; `visit(abs, rel_slash, entry)`
/// returning false stops the walk.
pub(crate) fn walk_files(
    root: &Path,
    base: &Path,
    visit: &mut dyn FnMut(&Path, &str, &walkdir::DirEntry) -> bool,
) {
    let gitignore = ignore_matcher(root);
    let ignored = |rel: &str, is_dir: bool| {
        gitignore
            .as_ref()
            .is_some_and(|g| g.matched(rel, is_dir).is_ignore())
    };
    let mut it = WalkDir::new(base)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(next) = it.next() {
        // Unreadable entries are skipped, never fatal.
        let Ok(entry) = next else { continue };
        let path = entry.path();
        let Some(rel) = paths::rel(root, path) else {
            continue;
        };
        let rel = paths::to_slash(&rel);
        if entry.file_type().is_dir() {
            if entry.file_name() == OsStr::new(".git") || (path != base && ignored(&rel, true)) {
                it.skip_current_dir();
            }
            continue;
        }
        if ignored(&rel, false) {
            continue;
        }
        if !visit(path, &rel, &entry) {
            return;
        }
    }
}
