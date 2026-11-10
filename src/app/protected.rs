//! The paths iota's own machinery never writes: the one table both enforcement points read — the OS sandbox
//! around a `shell` command (`crate::shell::sandbox`) and the `code` writers' jail
//! (`crate::tool::builtins::code`).
//!
//! A sandboxed command runs without approval and the project root is writable, so without this a silent
//! command could plant `<root>/.git/hooks/pre-commit` (run by the user's own next `git commit`), point git at
//! another program from `<root>/.git/config`, rewrite the project's `.iota.yaml` for the next run, or trust a
//! file on the user's behalf through `~/.iota/trusted.json`. The list is the three items Claude Code, Codex and
//! Cursor all protect — git's hooks and config and the agent's own config (the project's and the user's own
//! `~/.iota.yaml`, which is trusted by definition and decides which code the next run starts) — plus iota's
//! trust record; nothing wider (reports/iota/over-engineering.md (b)). In a linked worktree, where `<root>/.git`
//! is a `gitdir:` file, git's hooks and config are those of the repository's common dir, so those are on it too.

use std::path::{Path, PathBuf};

use super::{CONFIG_BASE, CONFIG_EXTS};

/// How much of a protected path is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The path and everything under it.
    Tree,
    /// The name alone: it cannot be renamed, removed or replaced (by a link, or `.git` by a `gitdir:` file), so
    /// a [`Reach::Tree`] entry under it cannot be moved out from under its rule. Its contents stay writable.
    Entry,
}

/// Why a path is protected — what a write there would do, and so what a refusal says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// `<root>/.iota.yaml|yml`.
    ProjectConfig,
    /// `~/.iota.yaml|yml`.
    HomeConfig,
    /// `<root>/.git` itself (and a linked worktree's common dir itself).
    GitDir,
    /// `<root>/.git/hooks` (a linked worktree's: `<common>/hooks`).
    GitHooks,
    /// `<root>/.git/config` and `config.worktree` (a linked worktree's also `<common>/config` and
    /// `<gitdir>/config.worktree`).
    GitConfig,
    /// `~/.iota` itself and `~/.iota/trusted.json`.
    Trust,
}

impl Why {
    /// The clause a refusal gives as the reason.
    pub fn reason(self) -> &'static str {
        match self {
            Self::ProjectConfig => {
                "it is the project config iota loads, and a change takes effect on the next run"
            }
            Self::HomeConfig => {
                "it is the user's own config, loaded without asking, and a change takes effect on the next run"
            }
            Self::GitDir => "replacing it points git at another repository and its hooks",
            Self::GitHooks => "git executes the files in .git/hooks on the user's next git command",
            Self::GitConfig => {
                "it can point git at another program (core.fsmonitor, core.hooksPath)"
            }
            Self::Trust => "it records which project configs the user trusts",
        }
    }
}

/// One protected path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Protected {
    /// Absolute, as built from the root or home it hangs off (not resolved).
    pub path: PathBuf,
    /// What of it is protected.
    pub reach: Reach,
    /// Why.
    pub why: Why,
}

/// The table for a run rooted at `root`, with the extra write roots (`write_roots:`) and the user's `home` when
/// there is one. An entry under another entry comes after it (`.git` before `.git/hooks`), the order a
/// bind-mount backend must apply them in.
///
/// An extra root that is itself a repository gets the git rows the project root gets — writable is not the
/// same as "run later in the user's terminal". A root with no `.git` adds nothing (one `lstat`); the project
/// root's rows are there even then, so a `.git` cannot be created under them.
///
/// When `<root>/.git` is a `gitdir:` file (a linked worktree, `git worktree add`), the file is read now: the
/// common dir it leads to (the gitdir's `commondir`, else the gitdir itself) is protected as `<root>/.git` is —
/// the entry itself, its `hooks` and `config` — and so is the gitdir's own `config.worktree`. Nothing else of
/// either is, so `git commit` keeps writing objects and refs there.
pub fn table(root: &Path, extra: &[PathBuf], home: Option<&Path>) -> Vec<Protected> {
    let at = |path: PathBuf, reach, why| Protected { path, reach, why };
    let configs = |dir: &Path, why| {
        CONFIG_EXTS
            .iter()
            .map(move |ext| at(dir.join(format!("{CONFIG_BASE}{ext}")), Reach::Tree, why))
            .collect::<Vec<_>>()
    };
    let git_dir = |git: PathBuf| {
        [
            at(git.clone(), Reach::Entry, Why::GitDir),
            at(git.join("hooks"), Reach::Tree, Why::GitHooks),
            at(git.join("config"), Reach::Tree, Why::GitConfig),
            // Read too when `extensions.worktreeConfig` is on (the main worktree's own config).
            at(git.join("config.worktree"), Reach::Tree, Why::GitConfig),
        ]
    };
    // `.git` itself, its hooks and config, and for a linked worktree its common dir's and `config.worktree`.
    let repo = |git: PathBuf| {
        let linked = linked_gitdir(&git);
        let mut rows: Vec<Protected> = git_dir(git).into();
        if let Some((gitdir, common)) = linked {
            rows.extend(git_dir(common));
            let worktree = gitdir.join("config.worktree");
            rows.push(at(worktree, Reach::Tree, Why::GitConfig));
        }
        rows
    };
    let mut t = configs(root, Why::ProjectConfig);
    t.extend(repo(root.join(".git")));
    for w in extra {
        let git = w.join(".git");
        if std::fs::symlink_metadata(&git).is_ok() {
            t.extend(repo(git));
        }
    }
    if let Some(home) = home {
        t.extend(configs(home, Why::HomeConfig));
        let app_home = home.join(super::DOT_DIR);
        let record = app_home.join(super::TRUST_FILE);
        t.push(at(app_home, Reach::Entry, Why::Trust));
        t.push(at(record, Reach::Tree, Why::Trust));
    }
    t
}

/// A linked worktree's `(gitdir, common dir)`, when `git` is a `gitdir: <path>` file (git's gitfile format: a
/// relative path is relative to the file's directory); the common dir is the gitdir's `commondir` file (relative
/// to the gitdir), else the gitdir itself. `None` for a `.git` directory, a missing one, or anything else.
fn linked_gitdir(git: &Path) -> Option<(PathBuf, PathBuf)> {
    let text = std::fs::read_to_string(git).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let gitdir = super::paths::clean(&git.parent()?.join(target));
    let common = std::fs::read_to_string(gitdir.join("commondir")).map_or_else(
        |_| gitdir.clone(),
        |c| super::paths::clean(&gitdir.join(c.trim())),
    );
    Some((gitdir, common))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Reach, Why, table};

    #[test]
    fn the_table_is_the_configs_git_and_the_trust_record() {
        let t = table(Path::new("/r"), &[], Some(Path::new("/h")));
        let got: Vec<(PathBuf, Reach, Why)> =
            t.into_iter().map(|p| (p.path, p.reach, p.why)).collect();
        let want = [
            ("/r/.iota.yaml", Reach::Tree, Why::ProjectConfig),
            ("/r/.iota.yml", Reach::Tree, Why::ProjectConfig),
            ("/r/.git", Reach::Entry, Why::GitDir),
            ("/r/.git/hooks", Reach::Tree, Why::GitHooks),
            ("/r/.git/config", Reach::Tree, Why::GitConfig),
            ("/r/.git/config.worktree", Reach::Tree, Why::GitConfig),
            ("/h/.iota.yaml", Reach::Tree, Why::HomeConfig),
            ("/h/.iota.yml", Reach::Tree, Why::HomeConfig),
            ("/h/.iota", Reach::Entry, Why::Trust),
            ("/h/.iota/trusted.json", Reach::Tree, Why::Trust),
        ]
        .map(|(p, r, w)| (PathBuf::from(p), r, w));
        assert_eq!(got, want);
        // No home: no home config or trust record to protect.
        assert_eq!(table(Path::new("/r"), &[], None).len(), 6);
    }

    /// A linked worktree: `<root>/.git` is a `gitdir:` file (relative, as `git worktree add --relative-paths`
    /// writes it, or absolute), and the gitdir's `commondir` leads to the main repository's `.git`, whose
    /// entry, hooks and config join the table, with the gitdir's own `config.worktree`.
    #[test]
    fn a_linked_worktree_protects_its_common_dir_hooks_and_config() {
        let t = tempfile::tempdir().expect("tempdir");
        let common = t.path().join("main/.git");
        let gitdir = common.join("worktrees/w");
        let root = t.path().join("w");
        std::fs::create_dir_all(&gitdir).expect("gitdir");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(gitdir.join("commondir"), "../..\n").expect("commondir");
        for gitfile in [
            format!("gitdir: {}\n", gitdir.display()),
            "gitdir: ../main/.git/worktrees/w\n".to_owned(),
        ] {
            std::fs::write(root.join(".git"), &gitfile).expect("gitfile");
            let got: Vec<(PathBuf, Reach, Why)> = table(&root, &[], None)
                .into_iter()
                .skip(6)
                .map(|p| (p.path, p.reach, p.why))
                .collect();
            assert_eq!(
                got,
                [
                    (common.clone(), Reach::Entry, Why::GitDir),
                    (common.join("hooks"), Reach::Tree, Why::GitHooks),
                    (common.join("config"), Reach::Tree, Why::GitConfig),
                    (common.join("config.worktree"), Reach::Tree, Why::GitConfig),
                    (gitdir.join("config.worktree"), Reach::Tree, Why::GitConfig),
                ],
                "{gitfile}"
            );
        }
    }

    /// An extra write root adds rows only when it is a repository: none for a plain directory (or a missing
    /// one), and a repository's four — `.git` itself, `hooks`, `config`, `config.worktree` — after the project
    /// root's, before the home's.
    #[test]
    fn an_extra_root_that_is_a_repository_gets_the_git_rows() {
        let t = tempfile::tempdir().expect("tempdir");
        let plain = t.path().join("plain");
        let repo = t.path().join("repo");
        std::fs::create_dir_all(&plain).expect("plain");
        std::fs::create_dir_all(repo.join(".git")).expect("repo");
        let base = table(Path::new("/r"), &[], None);
        let with_plain = table(Path::new("/r"), &[plain, t.path().join("missing")], None);
        assert_eq!(with_plain, base, "a root with no .git adds nothing");
        let got: Vec<(PathBuf, Reach, Why)> =
            table(Path::new("/r"), std::slice::from_ref(&repo), None)
                .into_iter()
                .skip(base.len())
                .map(|p| (p.path, p.reach, p.why))
                .collect();
        let git = repo.join(".git");
        assert_eq!(
            got,
            [
                (git.clone(), Reach::Entry, Why::GitDir),
                (git.join("hooks"), Reach::Tree, Why::GitHooks),
                (git.join("config"), Reach::Tree, Why::GitConfig),
                (git.join("config.worktree"), Reach::Tree, Why::GitConfig),
            ]
        );
    }
}
