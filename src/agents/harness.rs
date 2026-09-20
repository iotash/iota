//! The built-in harness prompt (brain page `harness-prompt`): the fixed paragraph every agent WITH tools
//! sends ahead of the user's `system:` — who the model is running inside, what the environment is, and
//! (when the `shell` set is on) how iota's own command line is driven from it.
//!
//! It is composed at SEND time, like the AGENTS.md overlay, and never written into the session history:
//! a resumed session and an upgraded binary both get the current version, and `/model`'s System tab
//! shows it because that tab renders the same [`compose_send_history`](super::compose_send_history) the
//! wire uses. An agent with no `tools:` sends nothing — a chat-only or JSON-pipeline agent keeps today's
//! bytes exactly. There is no configuration key for it (decided 2026-09-20).
//!
//! The text lives here as data; every fact in `<environment>` arrives through [`Environment`], which the
//! command layer fills once at the binary edge, so the composition stays pure and byte-pinnable.

use std::{fmt::Write as _, path::PathBuf};

/// The `tools:` key that brings `<iota_cli>` with it — the config name of the shell set
/// (`tool::builtins::shell::SHELL_TOOL_NAME`, spelled here because `agents` sits beside `shell` in the
/// layer order and may name neither it nor `tool`).
const SHELL_SET: &str = "shell";

/// The size the whole paragraph must stay under, environment included: a harness is overhead on every
/// request, and this is the ceiling the decision set (1.5 KB).
pub const HARNESS_CAP: usize = 1536;

/// What the `<environment>` block reports. Plain data, resolved by the caller: the composition never
/// probes the machine itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    /// The project root (`agents::project_root`).
    pub project_root: PathBuf,
    /// `std::env::consts::OS` and `ARCH`, as one phrase (`macos (aarch64)`).
    pub platform: String,
    /// The interpreter the `shell` set runs (`bash`, `pwsh`, `cmd`); `(none)` when none resolved.
    pub shell: String,
    /// Today's local date, `YYYY-MM-DD` ([`today`]).
    pub date: String,
    /// The running binary, canonicalised (`HostDirs::exe`); `None` when the OS could not say.
    pub exe: Option<PathBuf>,
    /// The config files this run reads.
    pub configs: ConfigFiles,
}

/// The config files a run reads, as `iota mcp add`'s scopes see them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigFiles {
    /// The two tiers of a run without `-c`: the user file (`~/.iota.yaml`) and the project file
    /// (`./.iota.yaml`), each `None` when absent.
    Tiers {
        /// `~/.iota.yaml|yml`, when it exists.
        user: Option<PathBuf>,
        /// `./.iota.yaml|yml`, when it exists.
        project: Option<PathBuf>,
    },
    /// The one file `-c` named — the only scope there is.
    Explicit(PathBuf),
}

impl Default for ConfigFiles {
    fn default() -> Self {
        Self::Tiers {
            user: None,
            project: None,
        }
    }
}

/// `std::env::consts::OS` and `ARCH` as the environment block phrases them.
pub fn platform() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}

/// Today's local date, `YYYY-MM-DD`.
pub fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

/// The identity and the two behaviour rules, ahead of every tagged block.
const PREAMBLE: &str = "You run inside iota, a coding agent in the user's terminal, acting on their project through the tools you are given. Report what you did and found.\n\
A tool call the user declines is not retried: say what it was for and ask. A command failing with `Operation not permitted`, a write outside the project or no network was stopped by the sandbox, not wrong in itself — say so instead of rewriting it.";

/// The `<iota_cli>` block: iota's own command line, driven from the `shell` set.
const IOTA_CLI: &str = "<iota_cli>\n\
iota itself runs OUTSIDE the shell sandbox when it is the command's first word (plain `iota …`, no pipe or chain). It manages its own configuration:\n\
- iota mcp add <name> --url <url> [--header 'K: V'] [--scope user|project] — writes the entry and logs in through the browser if needed; run with background: true\n\
- iota mcp add <name> -- <command> [args]\n\
- iota mcp list [--probe] | get | remove | login (background: true) | logout <name>\n\
- iota config check | path | init\n\
- iota list agents | models | providers\n\
- iota run <agent> -m \"<task>\" — a child agent under its own agent config\n\
Change MCP servers with `iota mcp`; providers, models and agents by editing the config file, then `iota config check`. Changes apply from the next session, not this one. Flags: `iota <verb> --help`.\n\
</iota_cli>";

/// The harness for an agent enabling `toolsets` (its `tools:` keys that are not `false`): `""` when the
/// list is empty; the preamble and `<environment>` otherwise; `<iota_cli>` appended when the `shell` set
/// is among them.
pub fn compose(env: &Environment, toolsets: &[String]) -> String {
    if toolsets.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(HARNESS_CAP);
    out.push_str(PREAMBLE);
    out.push_str("\n\n");
    environment_block(env, &mut out);
    if toolsets.iter().any(|s| s == SHELL_SET) {
        out.push_str("\n\n");
        out.push_str(IOTA_CLI);
    }
    out
}

/// One `key: value` line per fact, absence spelled out rather than left blank.
fn environment_block(env: &Environment, out: &mut String) {
    let absent = |p: &Option<PathBuf>| {
        p.as_ref()
            .map_or_else(|| "(absent)".to_owned(), |p| p.display().to_string())
    };
    out.push_str("<environment>\n");
    let _ = writeln!(out, "project root: {}", env.project_root.display());
    let _ = writeln!(out, "platform: {}", env.platform);
    let _ = writeln!(out, "shell: {}", env.shell);
    let _ = writeln!(out, "date: {}", env.date);
    let _ = writeln!(
        out,
        "iota binary: {}",
        env.exe
            .as_ref()
            .map_or_else(|| "(unknown)".to_owned(), |p| p.display().to_string())
    );
    match &env.configs {
        ConfigFiles::Tiers { user, project } => {
            let _ = writeln!(out, "user config: {}", absent(user));
            let _ = writeln!(out, "project config: {}", absent(project));
        }
        ConfigFiles::Explicit(path) => {
            let _ = writeln!(
                out,
                "config: {} (given with -c; the only scope)",
                path.display()
            );
        }
    }
    out.push_str("</environment>");
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ConfigFiles, Environment, HARNESS_CAP, compose, platform, today};

    /// A Homebrew install on a Mac: the paths are as long as they get for most users, which is what makes
    /// the cap assertion below worth having.
    fn env() -> Environment {
        Environment {
            project_root: PathBuf::from("/Users/someone/Work/project"),
            platform: "macos (aarch64)".to_owned(),
            shell: "bash".to_owned(),
            date: "2026-09-20".to_owned(),
            exe: Some(PathBuf::from("/opt/homebrew/Cellar/iota/0.3.2/bin/iota")),
            configs: ConfigFiles::Tiers {
                user: Some(PathBuf::from("/Users/someone/.iota.yaml")),
                project: Some(PathBuf::from("/Users/someone/Work/project/.iota.yaml")),
            },
        }
    }

    fn sets(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    /// No toolset, no harness: a chat-only agent sends today's bytes exactly.
    #[test]
    fn no_tools_is_empty() {
        assert_eq!(compose(&env(), &[]), "");
    }

    /// Tools without the shell set: identity and environment, no `<iota_cli>`.
    #[test]
    fn code_without_shell_has_no_cli_block() {
        let text = compose(&env(), &sets(&["code"]));
        assert!(text.starts_with("You run inside iota"), "{text}");
        assert!(text.contains("<environment>\n"), "{text}");
        assert!(text.ends_with("</environment>"), "{text}");
        assert!(!text.contains("<iota_cli>"), "{text}");
    }

    /// The shell set brings the CLI block, after the environment.
    #[test]
    fn shell_brings_the_cli_block() {
        let text = compose(&env(), &sets(&["code", "shell"]));
        let env_at = text.find("<environment>").expect("environment");
        let cli_at = text.find("<iota_cli>").expect("iota_cli");
        assert!(env_at < cli_at, "the environment precedes the CLI block");
        assert!(text.ends_with("</iota_cli>"), "{text}");
        for verb in [
            "iota mcp add <name> --url <url>",
            "iota mcp add <name> -- <command> [args]",
            "iota mcp list [--probe]",
            "iota config check | path | init",
            "iota list agents | models | providers",
            "iota run <agent> -m \"<task>\"",
            "iota <verb> --help",
        ] {
            assert!(text.contains(verb), "missing {verb:?} in {text}");
        }
        // The two rules of the preamble.
        assert!(text.contains("is not retried"), "{text}");
        assert!(text.contains("OUTSIDE the shell sandbox"), "{text}");
        assert!(text.contains("stopped by the sandbox"), "{text}");
    }

    /// The whole paragraph, environment included, stays under the ceiling the decision set.
    #[test]
    fn the_harness_fits_the_cap() {
        let text = compose(&env(), &sets(&["code", "shell"]));
        assert!(
            text.len() <= HARNESS_CAP,
            "harness is {} bytes, cap {HARNESS_CAP}",
            text.len()
        );
    }

    /// Every environment fact is one line; a missing config file is spelled `(absent)`, and `-c` collapses
    /// the two tiers into one line.
    #[test]
    fn environment_lines_name_absence() {
        let text = compose(&env(), &sets(&["code"]));
        for line in [
            "project root: /Users/someone/Work/project",
            "platform: macos (aarch64)",
            "shell: bash",
            "date: 2026-09-20",
            "iota binary: /opt/homebrew/Cellar/iota/0.3.2/bin/iota",
            "user config: /Users/someone/.iota.yaml",
            "project config: /Users/someone/Work/project/.iota.yaml",
        ] {
            assert!(
                text.contains(&format!("\n{line}\n")),
                "missing {line:?} in {text}"
            );
        }
        let absent = Environment {
            configs: ConfigFiles::default(),
            ..env()
        };
        let text = compose(&absent, &sets(&["code"]));
        assert!(
            text.contains("\nuser config: (absent)\nproject config: (absent)\n"),
            "{text}"
        );
        let explicit = Environment {
            configs: ConfigFiles::Explicit(PathBuf::from("/tmp/f.yaml")),
            exe: None,
            ..env()
        };
        let text = compose(&explicit, &sets(&["code"]));
        assert!(
            text.contains("\nconfig: /tmp/f.yaml (given with -c; the only scope)\n"),
            "{text}"
        );
        assert!(!text.contains("user config:"), "{text}");
        assert!(text.contains("\niota binary: (unknown)\n"), "{text}");
    }

    /// The two probes the caller fills the environment from.
    #[test]
    fn probes_have_the_documented_shape() {
        assert_eq!(
            platform(),
            format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
        );
        let date = today();
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(date.as_bytes()[4], b'-');
        assert_eq!(date.as_bytes()[7], b'-');
    }
}
