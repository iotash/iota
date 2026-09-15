//! `iota list [agents|models|providers|sessions]` — what this installation can run, read off the config and
//! the session store.
//!
//! It replaced `-l`, which meant "list configured providers, or the models of one" and asked the PROVIDER for
//! the second half over the network. With three config layers that question no longer has one answer, so the
//! listing says what the config declares and nothing else: the network listing lives where it is actually
//! used, in the interactive `/model` picker. No key is needed to read your own config.

use std::io::Write;

use clap::ValueEnum as _;

use crate::app::HostDirs;
use crate::app::env::Env;

use crate::cmd::cli::{ListCmd, ListWhat};
use crate::cmd::resolve::resolve_agent;
use crate::cmd::{CliError, io};
use crate::config::{ApiKey, Config, ModelRef, ProviderConfig};

/// `iota list [what] [<agent>]`. Nothing here touches the network or needs a key.
pub fn run_list(
    cmd: &ListCmd,
    cfg: &Config,
    env: &Env,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let what = cmd.what.unwrap_or(ListWhat::Agents);
    if cmd.agent.is_some() && what != ListWhat::Models {
        // The word the user typed, taken from clap's own value table so there is one spelling of it.
        let word = what
            .to_possible_value()
            .map_or_else(String::new, |v| v.get_name().to_owned());
        return Err(CliError::ListTakesNoName(word));
    }
    match what {
        ListWhat::Agents => list_agents(cfg, io),
        ListWhat::Models => match &cmd.agent {
            Some(name) => list_agent_models(cfg, name, io),
            None => list_models(cfg, io),
        },
        ListWhat::Providers => list_providers(cfg, env, io),
        ListWhat::Sessions => list_sessions(&env.dirs, io),
    }
}

/// `iota list agents`: what a run may name, with the description the entry documents itself with and the size
/// of its candidate set (the number `-M` and the picker choose from).
fn list_agents(cfg: &Config, io: &mut io::Streams) -> Result<(), CliError> {
    if cfg.agents.is_empty() {
        writeln!(
            io.stdout,
            "No agents configured. Run `iota config init` to write a starter config."
        )?;
        return Ok(());
    }
    let width = column_width(cfg.agents.keys());
    writeln!(io.stdout, "Agents:")?;
    for (name, agent_cfg) in &cfg.agents {
        let models = match agent_cfg.models.len() {
            1 => "1 model".to_owned(),
            n => format!("{n} models"),
        };
        let mut line = format!("  {name:width$}  {models}");
        if !agent_cfg.description.is_empty() {
            line.push_str("  ");
            line.push_str(&agent_cfg.description);
        }
        writeln!(io.stdout, "{line}")?;
    }
    Ok(())
}

/// `iota list models`: the `models:` entries and the endpoint each one rides on.
fn list_models(cfg: &Config, io: &mut io::Streams) -> Result<(), CliError> {
    if cfg.models.is_empty() {
        writeln!(
            io.stdout,
            "No models configured. Add a `models:` entry (see `iota config path`)."
        )?;
        return Ok(());
    }
    let width = column_width(cfg.models.keys());
    writeln!(io.stdout, "Models:")?;
    for (name, model) in &cfg.models {
        writeln!(
            io.stdout,
            "  {name:width$}  {}:{}",
            model.provider_or(name),
            model.id
        )?;
    }
    Ok(())
}

/// `iota list models <agent>`: that agent's candidate set, best first — the first line is the model a run
/// starts on, and a `provider:*` line is a whole endpoint's catalogue (resolved in the picker at startup).
fn list_agent_models(cfg: &Config, name: &str, io: &mut io::Streams) -> Result<(), CliError> {
    let resolved = resolve_agent(cfg, name)?;
    writeln!(io.stdout, "Models for agent {name}:")?;
    for r in &resolved.agent.models {
        let line = match r {
            ModelRef::Entry(entry) => match cfg.models.get(entry) {
                Some(m) => format!("{entry} ({}:{})", m.provider_or(entry), m.id),
                None => entry.clone(),
            },
            ModelRef::Inline { .. } => r.to_string(),
            ModelRef::All { provider } => format!("{provider}:* (every model {provider} lists)"),
        };
        writeln!(io.stdout, "  {line}")?;
    }
    Ok(())
}

/// `iota list providers`: the endpoints, with where each one's key comes from — the question a failing run
/// actually asks.
fn list_providers(cfg: &Config, env: &Env, io: &mut io::Streams) -> Result<(), CliError> {
    if cfg.providers.is_empty() {
        writeln!(
            io.stdout,
            "No providers configured. Run `iota config init` to write a starter config."
        )?;
        return Ok(());
    }
    writeln!(io.stdout, "Providers:")?;
    for name in cfg.providers.keys() {
        let endpoint = cfg.provider(name);
        writeln!(
            io.stdout,
            "  {}  [{}]",
            provider_line(name, endpoint.kind, endpoint.config),
            key_source(&endpoint.api_key(env))
        )?;
    }
    Ok(())
}

/// Where a provider's key comes from, as the listing says it — the precedence a run applies
/// (`Endpoint::api_key`), so `[key: OPENAI_API_KEY]` means the variable is what the run would use even when
/// `key:` is set too.
fn key_source(key: &ApiKey) -> String {
    match key {
        ApiKey::Env { var, .. } => format!("key: {var}"),
        ApiKey::Config(_) => "key: config".to_owned(),
        ApiKey::Missing { var } => format!("no key: set {var}"),
    }
}

/// One `providers:` line: `{name}` alone when the entry's own name IS the type, else `{name} (type: {t})`,
/// plus the base URL when the entry overrides it (root.go:455-467, minus the `model:` a one-layer block used
/// to write here).
pub fn provider_line(name: &str, raw_type: &str, provider_cfg: &ProviderConfig) -> String {
    let mut info = name.to_owned();
    let parts = [
        (raw_type != name).then(|| format!("type: {raw_type}")),
        (!provider_cfg.url.is_empty()).then(|| format!("url: {}", provider_cfg.url)),
    ];
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    if !parts.is_empty() {
        info.push_str(" (");
        info.push_str(&parts.join(", "));
        info.push(')');
    }
    info
}

/// `iota list sessions`: every saved bundle, newest first — the RESOLUTION view (the flat root plus every
/// project bucket), because that is the set `iota resume <id>` can reach.
fn list_sessions(dirs: &HostDirs, io: &mut io::Streams) -> Result<(), CliError> {
    let store = crate::session::SessionStore::from_dirs(dirs)?;
    let sessions = store.list_all().map_err(CliError::ListSessions)?;
    if sessions.is_empty() {
        writeln!(io.stdout, "No saved sessions.")?;
        return Ok(());
    }
    let width = column_width(sessions.iter().map(|s| &s.id));
    writeln!(io.stdout, "Sessions:")?;
    for info in &sessions {
        writeln!(
            io.stdout,
            "  {:width$}  {}",
            info.id,
            crate::repl::session_label(info, None)
        )?;
    }
    Ok(())
}

/// The width of a name column: the longest entry, so the second column lines up.
fn column_width<'a>(names: impl Iterator<Item = &'a String>) -> usize {
    names
        .map(|n| crate::text::width::str_width(n))
        .max()
        .unwrap_or(0)
}
