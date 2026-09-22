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

use crate::cmd::args::{ListCmd, ListWhat};
use crate::cmd::resolve::resolve_agent;
use crate::cmd::{ArgsError, CliError, SetupError, io};
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
        return Err(ArgsError::ListTakesNoName(word).into());
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

/// `iota list agents`: what a run may name, with the model each one starts on (`model:` as written, or
/// `(picker)` when the run opens the picker instead), the size of its choices (the number `-M` and the
/// picker choose from — every `models:` entry when the agent lists none) and the description the entry
/// documents itself with.
fn list_agents(cfg: &Config, io: &mut io::Streams) -> Result<(), CliError> {
    if cfg.agents.is_empty() {
        writeln!(
            io.stdout,
            "No agents configured. Run `iota config init` to write a starter config."
        )?;
        return Ok(());
    }
    let starts: Vec<String> = cfg
        .agents
        .values()
        .map(|a| {
            a.model
                .as_ref()
                .map_or_else(|| PICKER.to_owned(), ModelRef::to_string)
        })
        .collect();
    let width = column_width(cfg.agents.keys());
    let start_width = column_width(starts.iter());
    writeln!(io.stdout, "Agents:")?;
    for ((name, agent_cfg), start) in cfg.agents.iter().zip(&starts) {
        let choices = match cfg.choices_of(agent_cfg).len() {
            1 => "1 choice".to_owned(),
            n => format!("{n} choices"),
        };
        let mut line = format!("  {name:width$}  {start:start_width$}  {choices}");
        if !agent_cfg.description.is_empty() {
            line.push_str("  ");
            line.push_str(&agent_cfg.description);
        }
        writeln!(io.stdout, "{line}")?;
    }
    Ok(())
}

/// The model column of `iota list agents` for an agent without `model:` — the run starts in the picker.
const PICKER: &str = "(picker)";

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

/// `iota list models <agent>`: that agent's choices in declaration order — every `models:` entry when it
/// lists none — with `*` on the model a run starts on. The default is marked where a row names its
/// `provider:id`; a default the choices do not name (or name only through a wildcard) is a `*` row of its
/// own on top, and an agent without `model:` marks nothing: its run starts in the picker. A `provider:*`
/// line is a whole endpoint's catalogue (resolved in the picker).
fn list_agent_models(cfg: &Config, name: &str, io: &mut io::Streams) -> Result<(), CliError> {
    let resolved = resolve_agent(cfg, name)?;
    let start = resolved.agent.model.as_ref();
    let is_start = |r: &ModelRef| match r {
        ModelRef::Entry(entry) => cfg.models.get(entry).is_some_and(|m| {
            m.provider_or(entry) == resolved.provider_name && m.id == resolved.model.id
        }),
        ModelRef::Inline { provider, id } => {
            *provider == resolved.provider_name && *id == resolved.model.id
        }
        ModelRef::All { .. } => false,
    };
    writeln!(io.stdout, "Models for agent {name}:")?;
    if let Some(start) = start
        && !resolved.agent.choices.iter().any(&is_start)
    {
        writeln!(io.stdout, "* {}", choice_line(cfg, start))?;
    }
    for r in &resolved.agent.choices {
        let mark = if start.is_some() && is_start(r) {
            '*'
        } else {
            ' '
        };
        writeln!(io.stdout, "{mark} {}", choice_line(cfg, r))?;
    }
    Ok(())
}

/// One row of `iota list models <agent>`: an entry with the `provider:id` it stands for, an inline pair as
/// written, a wildcard with what it means.
fn choice_line(cfg: &Config, r: &ModelRef) -> String {
    match r {
        ModelRef::Entry(entry) => match cfg.models.get(entry) {
            Some(m) => format!("{entry} ({}:{})", m.provider_or(entry), m.id),
            None => entry.clone(),
        },
        ModelRef::Inline { .. } => r.to_string(),
        ModelRef::All { provider } => format!("{provider}:* (every model {provider} lists)"),
    }
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
    let sessions = store.list_all().map_err(SetupError::ListSessions)?;
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
pub(crate) fn column_width<'a>(names: impl Iterator<Item = &'a String>) -> usize {
    names
        .map(|n| crate::text::width::str_width(n))
        .max()
        .unwrap_or(0)
}
