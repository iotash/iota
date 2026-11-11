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

use std::{fmt::Write as _, path::PathBuf, sync::Arc};

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
    /// The top-level `write_roots:` as the tools hold them (`ToolEnv::write_roots`, resolved once at
    /// assembly): the roots beyond the project root the code tools reach and a sandboxed shell writes. Empty
    /// prints no line, so a config without the key keeps the block's bytes.
    pub write_roots: Vec<PathBuf>,
    /// What the detected hosts tell the model (`host::Presenter::environment`): `(key, value)` pairs, one
    /// line each after the run's own facts — `host: herdr`, `herdr pane: w1:p3`. Empty in a plain terminal.
    pub host: Vec<(String, String)>,
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
        /// Whether `project` was loaded: `false` = skipped as not trusted (`config::trust`), so the model is
        /// not told it is in force.
        project_trusted: bool,
    },
    /// The one file `-c` named — the only scope there is.
    Explicit(PathBuf),
}

impl Default for ConfigFiles {
    fn default() -> Self {
        Self::Tiers {
            user: None,
            project: None,
            project_trusted: true,
        }
    }
}

/// The harness prompt's inputs, held for the whole run: the environment read at assembly, the enabled
/// toolsets, and the clock that says which day it is. The text is composed from them once the hosts are known
/// — their facts join `<environment>` — and again whenever the day changes, because `date:` is part of it
/// (docs/design/bot-mode.md §2.5).
#[derive(Clone)]
pub struct HarnessInputs {
    /// The run's own facts. `date` and `host` are overwritten by every [`Self::compose`].
    pub env: Environment,
    /// The enabled toolsets (`cmd::assemble::enabled_toolsets`).
    pub toolsets: Vec<String>,
    /// Today's local date, `YYYY-MM-DD`: [`today`] in a real run, anything a test wants in a test.
    pub clock: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Default for HarnessInputs {
    /// No toolsets — the harness is `""` — on the real clock.
    fn default() -> Self {
        Self {
            env: Environment::default(),
            toolsets: Vec::new(),
            clock: Arc::new(today),
        }
    }
}

impl std::fmt::Debug for HarnessInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessInputs")
            .field("env", &self.env)
            .field("toolsets", &self.toolsets)
            .finish_non_exhaustive()
    }
}

impl HarnessInputs {
    /// The date the clock reads now.
    pub fn today(&self) -> String {
        (self.clock)()
    }

    /// The harness text for `day`, with the hosts' `host` facts: `""` for an agent without tools.
    pub fn compose(&self, day: &str, host: Vec<(String, String)>) -> String {
        let env = Environment {
            date: day.to_owned(),
            host,
            ..self.env.clone()
        };
        compose(&env, &self.toolsets)
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
A tool call the user declines is not retried: say what it was for and ask. A shell command failing with `Operation not permitted` or `Read-only file system`, or with no network, was stopped by the sandbox, not wrong in itself — say so instead of rewriting it.";

/// The `<iota_cli>` block: iota's own command line, driven from the `shell` set.
const IOTA_CLI: &str = "<iota_cli>\n\
iota itself runs OUTSIDE the shell sandbox when it is the command's first word (plain `iota …`, no pipe or chain):\n\
- iota mcp list [--probe] | get | login (background: true) | logout <name>\n\
- iota config check | path\n\
- iota list agents | models | providers\n\
- iota run <agent> -m \"<task>\" — a child agent under its own agent config\n\
Its config writers stay IN the sandbox and fail; ask the user to run them:\n\
- iota mcp add <name> --url <url> [--header 'K: V'] [--scope user|project], or -- <command> [args]\n\
- iota mcp remove <name>; iota config init | trust | untrust\n\
Providers, models and agents: edit the config file (the user is asked), then `iota config check`. Changes apply from the next session, not this one. Flags: `iota <verb> --help`.\n\
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
        ConfigFiles::Tiers {
            user,
            project,
            project_trusted,
        } => {
            let _ = writeln!(out, "user config: {}", absent(user));
            let skipped = if *project_trusted {
                ""
            } else {
                " (not trusted: not loaded)"
            };
            let _ = writeln!(out, "project config: {}{skipped}", absent(project));
        }
        ConfigFiles::Explicit(path) => {
            let _ = writeln!(
                out,
                "config: {} (given with -c; the only scope)",
                path.display()
            );
        }
    }
    if !env.write_roots.is_empty() {
        let roots: Vec<String> = env
            .write_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let _ = writeln!(out, "write roots: {}", roots.join(", "));
    }
    // The hosts' facts last: a terminal multiplexer names itself and the pane the chat runs in, so the
    // model can drive that host's own CLI (brain page `host-integration`, 2026-09-21).
    for (key, value) in &env.host {
        let _ = writeln!(out, "{key}: {value}");
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
                project_trusted: true,
            },
            host: Vec::new(),
            write_roots: Vec::new(),
        }
    }

    /// The four id lines a herdr pane adds. Its note line comes on top; the herdr host's own test bounds
    /// everything it says (`host::herdr::tests::the_lines_fit_the_hosts_share_of_the_harness`).
    fn herdr_lines() -> Vec<(String, String)> {
        [
            ("host", "herdr"),
            ("herdr pane", "w12:p34"),
            ("herdr workspace", "w12"),
            ("herdr tab", "w12:t56"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
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
            "iota mcp list [--probe] | get | login (background: true) | logout <name>",
            "iota config check | path\n",
            "iota list agents | models | providers",
            "iota run <agent> -m \"<task>\"",
            "iota <verb> --help",
        ] {
            assert!(text.contains(verb), "missing {verb:?} in {text}");
        }
        // The two rules of the preamble.
        assert!(text.contains("is not retried"), "{text}");
        assert!(text.contains("OUTSIDE the shell sandbox"), "{text}");
        // iota's own writers of a config or the trust record are not self-calls (`shell::selfcall`, X-95):
        // the block lists them as the user's to run, never under the OUTSIDE line.
        let (outside, writers) = text[cli_at..]
            .split_once(
                "Its config writers stay IN the sandbox and fail; ask the user to run them:\n",
            )
            .expect("the writers' paragraph");
        for writer in ["mcp add", "mcp remove", "config init", "trust", "untrust"] {
            assert!(
                !outside.contains(writer),
                "{writer:?} listed as leaving the sandbox: {text}"
            );
        }
        assert!(
            writers.contains("- iota mcp add <name> --url <url> [--header 'K: V'] [--scope user|project], or -- <command> [args]\n- iota mcp remove <name>; iota config init | trust | untrust\n"),
            "{text}"
        );
        assert!(text.contains("stopped by the sandbox"), "{text}");
        // A shell command's denial in both sandboxes' words (Seatbelt, bwrap). No claim that a write
        // outside the project is stopped: a listed write root is writable, and a `code` write outside every
        // root is asked, not stopped; where the shell may write is its own description's to say.
        assert!(
            text.contains(
                "A shell command failing with `Operation not permitted` or `Read-only file system`"
            ),
            "{text}"
        );
        assert!(!text.contains("a write outside the project"), "{text}");
    }

    /// The whole paragraph, environment included, stays under the ceiling the decision set. The lines a
    /// host adds come on top of it — only inside a multiplexer, and bounded to a tenth of the cap: here for the
    /// four herdr id lines, and by each host's own test for all it says.
    #[test]
    fn the_harness_fits_the_cap() {
        let text = compose(&env(), &sets(&["code", "shell"]));
        assert!(
            text.len() <= HARNESS_CAP,
            "harness is {} bytes, cap {HARNESS_CAP}",
            text.len()
        );
        let hosted = Environment {
            host: herdr_lines(),
            ..env()
        };
        let added = compose(&hosted, &sets(&["code", "shell"])).len() - text.len();
        assert!(
            added <= HARNESS_CAP / 10,
            "the host lines add {added} bytes, more than a tenth of the cap"
        );
    }

    /// A host's facts close the block, one `key: value` line each in the order given; no host, no line.
    #[test]
    fn host_lines_close_the_environment() {
        let hosted = Environment {
            host: herdr_lines(),
            ..env()
        };
        let text = compose(&hosted, &sets(&["code"]));
        assert!(
            text.contains(
                "\nproject config: /Users/someone/Work/project/.iota.yaml\nhost: herdr\nherdr pane: w12:p34\nherdr workspace: w12\nherdr tab: w12:t56\n</environment>"
            ),
            "{text}"
        );
        let text = compose(&env(), &sets(&["code"]));
        assert!(
            text.contains(
                "\nproject config: /Users/someone/Work/project/.iota.yaml\n</environment>"
            ),
            "{text}"
        );
        assert!(!text.contains("host:"), "{text}");
    }

    /// `write_roots:` is one line after the config files, before any host line, its roots in the order the
    /// tools hold them; with none there is no line at all.
    #[test]
    fn write_roots_line_only_when_listed() {
        let widened = Environment {
            write_roots: vec![
                PathBuf::from("/Users/someone/scratch"),
                PathBuf::from("/Users/someone/Work/shared-lib"),
            ],
            host: herdr_lines(),
            ..env()
        };
        let text = compose(&widened, &sets(&["code"]));
        assert!(
            text.contains(
                "\nproject config: /Users/someone/Work/project/.iota.yaml\nwrite roots: /Users/someone/scratch, /Users/someone/Work/shared-lib\nhost: herdr\n"
            ),
            "{text}"
        );
        let text = compose(&env(), &sets(&["code"]));
        assert!(!text.contains("write roots"), "{text}");
    }

    /// Every environment fact is one line; a missing config file is spelled `(absent)`, a skipped project
    /// file `(not trusted: not loaded)`, and `-c` collapses the two tiers into one line.
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
        // A project file the load skipped as not trusted is not presented as in force.
        let skipped = Environment {
            configs: ConfigFiles::Tiers {
                user: None,
                project: Some(PathBuf::from("/Users/someone/Work/project/.iota.yaml")),
                project_trusted: false,
            },
            ..env()
        };
        let text = compose(&skipped, &sets(&["code"]));
        assert!(
            text.contains(
                "\nproject config: /Users/someone/Work/project/.iota.yaml (not trusted: not loaded)\n"
            ),
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

    /// [`today`] is local only if the zone `tz-system` maps the Windows setting to can be looked up,
    /// and Windows has no tz database on disk: jiff's zoneinfo search list is empty off unix, so the
    /// copy `tzdb-bundle-platform` links in is the only one. Without it this lookup fails and every
    /// `Zoned::now()` is UTC. `Asia/Shanghai` is the UTC+8 zone the wrong `date:` was seen in, and with
    /// no DST since 1991 its offset at a fixed instant is a constant only real data can produce; a
    /// `UTC` offset of zero is also what the fallback answers, so it could not tell the two apart.
    /// Windows-only because a unix result depends on the machine's tzdata, not on our feature set.
    #[cfg(windows)]
    #[test]
    fn named_zone_resolves_on_windows() {
        let tz = jiff::tz::TimeZone::get("Asia/Shanghai")
            .expect("no tz database: is jiff's `tzdb-bundle-platform` feature on?");
        let offset = tz.to_offset(jiff::Timestamp::from_second(1_790_000_000).expect("timestamp"));
        assert_eq!(offset, jiff::tz::offset(8));
    }
}
