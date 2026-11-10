//! The paths iota's own machinery never writes: the one table both enforcement points read — the OS sandbox
//! around a `shell` command (`crate::shell::sandbox`) and the `code` writers' jail
//! (`crate::tool::builtins::code`).
//!
//! A sandboxed command runs without approval and the project root is writable, so without this a silent
//! command could plant `<root>/.git/hooks/pre-commit` (run by the user's own next `git commit`), point git at
//! another program from `<root>/.git/config`, rewrite the project's `.iota.yaml` for the next run, or trust a
//! file on the user's behalf through `~/.iota/trusted.json`. The list is the three items Claude Code, Codex and
//! Cursor all protect — git's hooks and config and the agent's own config — plus iota's trust record; nothing
//! wider (reports/iota/over-engineering.md (b)).

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
    /// `<root>/.git` itself.
    GitDir,
    /// `<root>/.git/hooks`.
    GitHooks,
    /// `<root>/.git/config`.
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

/// The table for a run rooted at `root`, with `app_home` (`~/.iota`) when there is a home. An entry under
/// another entry comes after it (`.git` before `.git/hooks`), the order a bind-mount backend must apply them in.
pub fn table(root: &Path, app_home: Option<&Path>) -> Vec<Protected> {
    let at = |path: PathBuf, reach, why| Protected { path, reach, why };
    let git = root.join(".git");
    let mut t: Vec<Protected> = CONFIG_EXTS
        .iter()
        .map(|ext| {
            let file = root.join(format!("{CONFIG_BASE}{ext}"));
            at(file, Reach::Tree, Why::ProjectConfig)
        })
        .collect();
    t.extend([
        at(git.clone(), Reach::Entry, Why::GitDir),
        at(git.join("hooks"), Reach::Tree, Why::GitHooks),
        at(git.join("config"), Reach::Tree, Why::GitConfig),
    ]);
    if let Some(home) = app_home {
        t.push(at(home.to_path_buf(), Reach::Entry, Why::Trust));
        t.push(at(home.join(super::TRUST_FILE), Reach::Tree, Why::Trust));
    }
    t
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Reach, Why, table};

    #[test]
    fn the_table_is_the_project_config_git_and_the_trust_record() {
        let t = table(Path::new("/r"), Some(Path::new("/h/.iota")));
        let got: Vec<(PathBuf, Reach, Why)> =
            t.into_iter().map(|p| (p.path, p.reach, p.why)).collect();
        let want = [
            ("/r/.iota.yaml", Reach::Tree, Why::ProjectConfig),
            ("/r/.iota.yml", Reach::Tree, Why::ProjectConfig),
            ("/r/.git", Reach::Entry, Why::GitDir),
            ("/r/.git/hooks", Reach::Tree, Why::GitHooks),
            ("/r/.git/config", Reach::Tree, Why::GitConfig),
            ("/h/.iota", Reach::Entry, Why::Trust),
            ("/h/.iota/trusted.json", Reach::Tree, Why::Trust),
        ]
        .map(|(p, r, w)| (PathBuf::from(p), r, w));
        assert_eq!(got, want);
        // No home: no trust record to protect.
        assert_eq!(table(Path::new("/r"), None).len(), 5);
    }
}
