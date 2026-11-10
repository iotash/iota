//! Gitignore-aware file walk on `walkdir` + the `ignore` crate's gitignore matcher
//! (real git semantics — DIVERGENCES D-17).

use std::{ffi::OsStr, path::Path};

use crate::app::paths;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use super::CODE_WALK_MAX_ENTRIES;

/// How a [`walk_files`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Walked {
    /// The tree was exhausted.
    Done,
    /// The visitor asked to stop.
    Stopped,
    /// [`CODE_WALK_MAX_ENTRIES`] entries were visited.
    Capped,
    /// The run's token was cancelled.
    Cancelled,
}

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
/// returning false stops the walk. `root` is the walk's anchor (`CodeSet::anchor`): `rel_slash` is relative to it and
/// its `.gitignore` is the one applied.
///
/// Reads are open, so `base` may be `/`: the walk is bounded by entries VISITED, not only by what the
/// visitor keeps — it stops after [`CODE_WALK_MAX_ENTRIES`] entries of any kind (so a pattern that never
/// matches cannot walk the whole disk), and as soon as `cancel` fires (ESC ends the call's await, and this
/// runs on the blocking pool, which nothing else would stop) — ripgrep's and fd's shape.
pub(crate) fn walk_files(
    root: &Path,
    base: &Path,
    cancel: &CancellationToken,
    visit: &mut dyn FnMut(&Path, &str, &walkdir::DirEntry) -> bool,
) -> Walked {
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
    let mut visited = 0usize;
    while let Some(next) = it.next() {
        if cancel.is_cancelled() {
            return Walked::Cancelled;
        }
        if visited == CODE_WALK_MAX_ENTRIES {
            return Walked::Capped;
        }
        visited += 1;
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
            return Walked::Stopped;
        }
    }
    Walked::Done
}

#[cfg(test)]
mod tests {
    use super::{Walked, walk_files};
    use tokio_util::sync::CancellationToken;

    /// A cancelled token stops the walk at the next entry: already cancelled, nothing is visited;
    /// cancelled mid-walk (as ESC does while the walk runs on the blocking pool), nothing more is.
    #[test]
    fn a_cancelled_token_stops_the_walk() {
        let t = tempfile::tempdir().expect("tempdir");
        for i in 0..20 {
            std::fs::write(t.path().join(format!("f{i:02}")), "x").expect("file");
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut seen = 0;
        let walked = walk_files(t.path(), t.path(), &cancel, &mut |_, _, _| {
            seen += 1;
            true
        });
        assert_eq!((walked, seen), (Walked::Cancelled, 0));

        let cancel = CancellationToken::new();
        let mut seen = 0;
        let walked = walk_files(t.path(), t.path(), &cancel, &mut |_, _, _| {
            seen += 1;
            cancel.cancel();
            true
        });
        assert_eq!((walked, seen), (Walked::Cancelled, 1));

        let mut seen = 0;
        let walked = walk_files(
            t.path(),
            t.path(),
            &CancellationToken::new(),
            &mut |_, _, _| {
                seen += 1;
                true
            },
        );
        assert_eq!((walked, seen), (Walked::Done, 20));
    }
}
