//! `iota config [check|path|init]` — the three questions a config file raises: does it say what I think it
//! says, which file am I actually editing, and how do I get a first one.
//!
//! `init` is what makes the surface honest about the zero-config start it gave up: a run names an `agents:`
//! entry now, so the first thing a new install needs is a file that HAS one (brain page
//! `cli-surface-agent-first`).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::app::{CONFIG_BASE, CONFIG_EXTS, HostDirs};
use crate::cmd::cli::{ConfigAction, ConfigCmd};
use crate::cmd::{CliError, io};
use crate::config::{Config, DEFAULT_AGENT};
use crate::vars::VarResolver;

/// The starter config `iota config init` writes. Every layer gets one entry and one sentence saying what it
/// is for; the model id is deliberately a current one rather than a placeholder, so the file runs as written
/// once the key is in the environment.
const STARTER: &str = "\
# iota config. Three layers, each answering one question.
#
#   providers:  how do I reach the API?      (type, key, url)
#   models:     which model, and how does its protocol work?
#   agents:     how is it driven?            (prompt, tools, MCP servers, sessions)
#
# `iota` runs the agent named `default`; `iota run <agent>` runs any other.
# See `iota list agents` and https://iota.sh for the full reference.

providers:
  openai:
    # The key is read from $OPENAI_API_KEY when this is absent — keep it out of the file if you can.
    # key: sk-...
    type: openai

models:
  gpt:
    provider: openai
    id: gpt-5.2
    # context_window: 400k    # what /compact accounts against

agents:
  default:
    models: [gpt]             # the candidate set, best first; -M and /model pick from it
    system: \"You are a helpful assistant.\"
    tools:
      code:                   # read/write/edit/grep over the project
      shell:                  # shell commands, with a sandbox by default
    # workspace: true         # AGENTS.md overlay, skills, project-scoped sessions
";

/// `iota config <action>`; the default action is `check`. `explicit` is `-c/--config`, which is global — so
/// `iota -c f.yaml config check` and `iota config check -c f.yaml` are the same invocation.
pub fn run_config(
    cmd: &ConfigCmd,
    explicit: Option<&Path>,
    dirs: &HostDirs,
    resolver: &dyn VarResolver,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    match cmd.action.unwrap_or(ConfigAction::Check) {
        ConfigAction::Check => check(explicit, dirs, resolver, io),
        ConfigAction::Path => path(explicit, dirs, io),
        ConfigAction::Init => init(explicit, dirs, io),
    }
}

/// `config check`: load exactly what a run would load — every warning on stderr, the first hard error as the
/// run's own — then report what the three layers ended up holding.
fn check(
    explicit: Option<&Path>,
    dirs: &HostDirs,
    resolver: &dyn VarResolver,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let files = Config::sources(explicit, dirs);
    let cfg = Config::load(explicit, dirs, resolver, &mut |w| io.warning(&w))?;
    if files.is_empty() {
        writeln!(
            io.stdout,
            "No config file found. Run `iota config init` to write one."
        )?;
        return Ok(());
    }
    for file in &files {
        writeln!(io.stdout, "{}", file.display())?;
    }
    writeln!(
        io.stdout,
        "OK: {} provider(s), {} model(s), {} agent(s), {} mcp server(s)",
        cfg.providers.len(),
        cfg.models.len(),
        cfg.agents.len(),
        cfg.mcp_servers.len()
    )?;
    // Not an error: naming an agent every time is a legitimate way to work. It is worth ONE line, because a
    // bare `iota` is what most people type first.
    if cfg.default_agent().is_none() {
        io.warning(&format!(
            "Warning: no `agents.{DEFAULT_AGENT}` entry: a bare `iota` has nothing to run"
        ));
    }
    Ok(())
}

/// `config path`: the files this invocation reads, in merge order, each marked as present or missing — the
/// answer to "why is my change not taking effect".
fn path(explicit: Option<&Path>, dirs: &HostDirs, io: &mut io::Streams) -> Result<(), CliError> {
    let files = Config::sources(explicit, dirs);
    if files.is_empty() {
        writeln!(
            io.stdout,
            "No config file found. Run `iota config init` to write one."
        )?;
        return Ok(());
    }
    for file in &files {
        let state = if file.is_file() { "" } else { "  (missing)" };
        writeln!(io.stdout, "{}{state}", file.display())?;
    }
    Ok(())
}

/// `config init`: write the starter file, refusing to touch one that exists — a config is hand-edited state,
/// and a command that silently replaced it would be a data-loss bug.
fn init(explicit: Option<&Path>, dirs: &HostDirs, io: &mut io::Streams) -> Result<(), CliError> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => default_config_path(dirs)?,
    };
    if path.exists() {
        return Err(CliError::ConfigExists(path.display().to_string()));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, STARTER)?;
    writeln!(io.stdout, "Wrote {}", path.display())?;
    writeln!(
        io.stdout,
        "Next: export OPENAI_API_KEY=... (or put `key:` in the file), then run `iota`."
    )?;
    Ok(())
}

/// `~/.iota.yaml` — the global config, in the first of the two extensions.
fn default_config_path(dirs: &HostDirs) -> Result<PathBuf, CliError> {
    dirs.home
        .as_ref()
        .map(|home| home.join(format!("{CONFIG_BASE}{}", CONFIG_EXTS[0])))
        .ok_or(CliError::NoHome)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::STARTER;

    /// A resolver with nothing in it: the starter config must not depend on the environment to be valid.
    struct NoVars;

    impl crate::vars::VarResolver for NoVars {
        fn env_var(&self, _name: &str) -> Option<String> {
            None
        }

        fn cwd(&self) -> Option<PathBuf> {
            None
        }

        fn home(&self) -> Option<PathBuf> {
            None
        }
    }

    /// The starter file must be a config iota itself accepts — every key checked, every reference resolved —
    /// or `iota config init` would hand a new user a file that fails on the next command.
    #[test]
    fn the_starter_config_loads() {
        let mut warnings = Vec::new();
        let cfg = crate::config::Config::parse(STARTER.as_bytes(), &NoVars, &mut |w| {
            warnings.push(w);
        })
        .expect("the starter config parses and validates");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(cfg.default_agent().is_some(), "it declares agents.default");
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.models.len(), 1);
    }
}
