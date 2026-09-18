//! `iota mcp add|list|get|remove` — the MCP servers a config declares, edited from the command line (brain page
//! `mcp-cli-and-oauth`).
//!
//! Two scopes and no third file: `--scope user` (the default) writes `~/.iota.yaml`, `--scope project` writes
//! `./.iota.yaml`, and `-c <file>` makes that file the only scope. Each command touches ONE file and only its
//! `mcp_servers:` block (`config::edit`); a project file may not carry a secret in clear, so a header or
//! environment value written there must be a `${…}` reference.

use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;

use crate::app::HostDirs;
use crate::app::env::Env;
use crate::cmd::args::{
    McpAction, McpAddCmd, McpAuthArg, McpCmd, McpListCmd, McpListScope, McpScope,
};
use crate::cmd::list::column_width;
use crate::cmd::{ArgsError, CliError, RunError, SetupError, io};
use crate::config::edit::{read_mcp_servers, write_mcp_servers};
use crate::config::{Config, DEFAULT_AGENT, McpServerConfig};
use crate::mcp::config::{AuthMode, ServerConfig};

/// `iota mcp <action>`. `explicit` is `-c/--config`: with it there is exactly one file and `--scope` is refused.
pub(crate) async fn run_mcp(
    cmd: &McpCmd,
    explicit: Option<&Path>,
    env: &Env,
    cancel: &CancellationToken,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    match &cmd.action {
        McpAction::Add(add) => run_add(add, explicit, env, io),
        McpAction::List(list) => run_list(list, explicit, env, cancel, io).await,
        McpAction::Get { name } => run_get(name, explicit, env, io),
        McpAction::Remove { name, scope } => run_remove(name, *scope, explicit, &env.dirs, io),
        McpAction::Login {
            name,
            no_browser,
            client_id,
        } => {
            run_login(
                name,
                *no_browser,
                client_id.clone(),
                explicit,
                env,
                cancel,
                io,
            )
            .await
        }
        McpAction::Logout { name } => run_logout(name, explicit, env, io).await,
    }
}

// ---------------------------------------------------------------- scopes and files

/// The file a scope writes: the existing `.iota.yaml|yml` of the tier, else `.iota.yaml` there.
fn scope_file(scope: McpScope, dirs: &HostDirs) -> Result<PathBuf, SetupError> {
    let dir = match scope {
        McpScope::User => dirs.home.as_deref().ok_or(SetupError::NoHome)?,
        McpScope::Project => dirs
            .cwd
            .as_deref()
            .ok_or_else(|| SetupError::Cwd(crate::cmd::cwd_err()))?,
    };
    Ok(Config::find_config_file(dir).unwrap_or_else(|| {
        dir.join(format!(
            "{}{}",
            crate::app::CONFIG_BASE,
            crate::app::CONFIG_EXTS[0]
        ))
    }))
}

/// The one file a writing command targets: `-c` alone, else the scope given (or `default`).
fn target_file(
    explicit: Option<&Path>,
    scope: Option<McpScope>,
    default: McpScope,
    dirs: &HostDirs,
) -> Result<PathBuf, CliError> {
    match (explicit, scope) {
        (Some(_), Some(_)) => Err(ArgsError::McpScopeWithConfig.into()),
        (Some(path), None) => Ok(path.to_path_buf()),
        (None, scope) => Ok(scope_file(scope.unwrap_or(default), dirs)?),
    }
}

/// Every file a reading command looks at, in merge order (the project file wins a name): `-c` alone, else the
/// user tier and the project tier — filtered to one of them by `scope`.
fn source_files(explicit: Option<&Path>, scope: Option<McpScope>, dirs: &HostDirs) -> Vec<PathBuf> {
    if let Some(path) = explicit {
        return vec![path.to_path_buf()];
    }
    let tiers = [
        (McpScope::User, dirs.home.as_deref()),
        (McpScope::Project, dirs.cwd.as_deref()),
    ];
    tiers
        .into_iter()
        .filter(|(tier, _)| scope.is_none_or(|s| s == *tier))
        .filter_map(|(_, dir)| dir.and_then(Config::find_config_file))
        .collect()
}

/// One declared server with the file that declares it.
struct Declared {
    name: String,
    file: PathBuf,
    entry: McpServerConfig,
}

/// The servers the files declare, later files replacing earlier entries of the same name (a load's merge).
fn declared_servers(files: &[PathBuf]) -> Result<Vec<Declared>, CliError> {
    let mut by_name: BTreeMap<String, Declared> = BTreeMap::new();
    for file in files {
        let read = read_mcp_servers(file).map_err(SetupError::McpEdit)?;
        for (name, entry) in read.servers {
            by_name.insert(
                name.clone(),
                Declared {
                    name,
                    file: file.clone(),
                    entry,
                },
            );
        }
    }
    Ok(by_name.into_values().collect())
}

// ---------------------------------------------------------------- add

/// `iota mcp add <name> [flags] -- <command> [args…]` / `iota mcp add <name> --url <url> [flags]`.
fn run_add(
    add: &McpAddCmd,
    explicit: Option<&Path>,
    env: &Env,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    check_name(&add.name)?;
    let entry = entry_of(add)?;
    let scope = add.scope.unwrap_or(McpScope::User);
    let file = target_file(explicit, add.scope, McpScope::User, &env.dirs)?;
    // A project file is shared: what it carries is read by everyone who clones it, so a value there names
    // the variable, never the secret.
    if explicit.is_none() && scope == McpScope::Project {
        for (at, value) in entry
            .headers
            .iter()
            .map(|(k, v)| (format!("headers.{k}"), v))
            .chain(entry.env.iter().map(|(k, v)| (format!("env.{k}"), v)))
        {
            if !value.contains("${") {
                return Err(ArgsError::McpProjectSecret(at).into());
            }
        }
    }

    let read = read_mcp_servers(&file).map_err(SetupError::McpEdit)?;
    if read.servers.contains_key(&add.name) {
        return Err(SetupError::McpExists {
            name: add.name.clone(),
            file: file.display().to_string(),
        }
        .into());
    }
    let mut servers = read.servers;
    servers.insert(add.name.clone(), entry.clone());
    write_mcp_servers(&file, &read.text, &servers).map_err(SetupError::McpEdit)?;
    writeln!(
        io.stdout,
        "Added {} ({}) to {}",
        add.name,
        describe(&entry),
        file.display()
    )?;
    if entry.auth == AuthMode::Oauth {
        writeln!(io.stdout, "Next: iota mcp login {}", add.name)?;
    }
    // The agent that a bare `iota` runs may select its servers by name; a new one it does not list will
    // never load, which is worth one line now rather than a silent absence later. Read from the merged
    // config a run would load — and said nothing when that config cannot be loaded, which is that run's
    // own error to report.
    if let Ok(cfg) = Config::load(explicit, env, &mut |_| {})
        && let Some(agent) = cfg.agents.get(DEFAULT_AGENT)
        && let Some(listed) = &agent.mcp_servers
        && !listed.iter().any(|n| n == &add.name)
    {
        writeln!(
            io.stdout,
            "agent {DEFAULT_AGENT} lists its servers explicitly; add {} there to use it",
            add.name
        )?;
    }
    Ok(())
}

/// A server name is a plain YAML key and a wire-name segment: letters, digits, `_`, `-` and `.`.
fn check_name(name: &str) -> Result<(), ArgsError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(ArgsError::McpName(name.to_owned()));
    }
    Ok(())
}

/// The entry `add`'s flags describe: exactly one of the two forms, each with the flags that apply to it.
fn entry_of(add: &McpAddCmd) -> Result<McpServerConfig, ArgsError> {
    let mut entry = McpServerConfig {
        defer: add.defer.clone(),
        ..McpServerConfig::default()
    };
    match (&add.url, add.command.first()) {
        (Some(_), Some(_)) => return Err(ArgsError::McpAddTarget("both")),
        (None, None) => return Err(ArgsError::McpAddTarget("neither")),
        (Some(url), None) => {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ArgsError::McpUrlScheme(url.clone()));
            }
            if !add.env.is_empty() {
                return Err(ArgsError::McpAddFlag {
                    flag: "-e/--env",
                    form: "command",
                });
            }
            entry.url.clone_from(url);
            for raw in &add.headers {
                let (name, value) = raw
                    .split_once(':')
                    .map(|(n, v)| (n.trim(), v.trim()))
                    .filter(|(n, _)| !n.is_empty())
                    .ok_or_else(|| ArgsError::McpBadHeader(raw.clone()))?;
                entry.headers.insert(name.to_owned(), value.to_owned());
            }
            entry.auth = match add.auth {
                Some(McpAuthArg::Oauth) => AuthMode::Oauth,
                Some(McpAuthArg::None) | None => AuthMode::None,
            };
            if entry.auth != AuthMode::Oauth
                && (add.client_id.is_some() || add.client_secret_env.is_some())
            {
                return Err(ArgsError::McpAddFlag {
                    flag: "--client-id",
                    form: "--auth oauth",
                });
            }
            if let Some(id) = &add.client_id {
                entry.client_id.clone_from(id);
            }
            if let Some(var) = &add.client_secret_env {
                // The variable's NAME goes into the file, as the reference a run expands; the secret itself
                // never does.
                let is_name =
                    !var.is_empty() && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if add.client_id.is_none() || !is_name {
                    return Err(ArgsError::McpClientSecretEnv(var.clone()));
                }
                entry.client_secret = format!("${{env:{var}}}");
            }
        }
        (None, Some(command)) => {
            if !add.headers.is_empty() {
                return Err(ArgsError::McpAddFlag {
                    flag: "--header",
                    form: "--url",
                });
            }
            if add.auth.is_some() {
                return Err(ArgsError::McpAddFlag {
                    flag: "--auth",
                    form: "--url",
                });
            }
            if add.client_id.is_some() || add.client_secret_env.is_some() {
                return Err(ArgsError::McpAddFlag {
                    flag: "--client-id",
                    form: "--url",
                });
            }
            entry.command.clone_from(command);
            entry.args = add.command[1..].to_vec();
            for raw in &add.env {
                let (name, value) = raw
                    .split_once('=')
                    .map(|(n, v)| (n.trim(), v))
                    .filter(|(n, _)| !n.is_empty())
                    .ok_or_else(|| ArgsError::McpBadEnv(raw.clone()))?;
                entry.env.insert(name.to_owned(), value.to_owned());
            }
        }
    }
    Ok(entry)
}

/// `stdio: <command line>` or `http: <url>` — the endpoint as the status views print it.
fn describe(entry: &McpServerConfig) -> String {
    if entry.url.is_empty() {
        let mut line = entry.command.clone();
        for arg in &entry.args {
            line.push(' ');
            line.push_str(arg);
        }
        format!("stdio: {line}")
    } else {
        format!("http: {}", entry.url)
    }
}

/// `stdio` / `http`: the transport column.
fn transport(entry: &McpServerConfig) -> &'static str {
    if entry.url.is_empty() {
        "stdio"
    } else {
        "http"
    }
}

/// The auth column: static headers, OAuth with where its tokens stand, or nothing.
fn auth_label(d: &Declared, env: &Env) -> String {
    match d.entry.auth {
        AuthMode::Oauth => format!("oauth: {}", login_state(d, env).as_str()),
        AuthMode::None if !d.entry.headers.is_empty() => "header".to_owned(),
        AuthMode::None => "none".to_owned(),
    }
}

/// The token store of an OAuth server, over the URL as a run would expand it.
fn token_store(d: &Declared, env: &Env) -> Option<crate::mcp::auth::TokenStore> {
    let expanded = crate::mcp::config::expand_server_config(&server_config(&d.name, &d.entry), env);
    crate::mcp::auth::TokenStore::for_server(&env.dirs, &d.name, &expanded.url)
}

/// Where an OAuth server's tokens stand (no home = nowhere to have stored them).
fn login_state(d: &Declared, env: &Env) -> crate::mcp::auth::LoginState {
    token_store(d, env).map_or(crate::mcp::auth::LoginState::NotLoggedIn, |s| s.status())
}

// ---------------------------------------------------------------- list

/// `iota mcp list [--scope user|project|all] [--json] [--probe]`.
async fn run_list(
    list: &McpListCmd,
    explicit: Option<&Path>,
    env: &Env,
    cancel: &CancellationToken,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    if explicit.is_some() && list.scope.is_some() {
        return Err(ArgsError::McpScopeWithConfig.into());
    }
    let scope = match list.scope {
        Some(McpListScope::User) => Some(McpScope::User),
        Some(McpListScope::Project) => Some(McpScope::Project),
        Some(McpListScope::All) | None => None,
    };
    let files = source_files(explicit, scope, &env.dirs);
    let declared = declared_servers(&files)?;
    let probes = if list.probe {
        Some(probe(&declared, env, cancel).await)
    } else {
        None
    };

    if list.json {
        let rows: Vec<serde_json::Value> = declared
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mut row = serde_json::json!({
                    "name": d.name,
                    "transport": transport(&d.entry),
                    "file": d.file.display().to_string(),
                    "command": d.entry.command,
                    "args": d.entry.args,
                    "url": d.entry.url,
                    "env": d.entry.env,
                    "headers": d.entry.headers,
                    "defer": d.entry.defer,
                    "client_id": d.entry.client_id,
                    "client_secret": d.entry.client_secret,
                    "auth": match d.entry.auth {
                        AuthMode::Oauth => "oauth",
                        AuthMode::None if !d.entry.headers.is_empty() => "header",
                        AuthMode::None => "none",
                    },
                    "login": (d.entry.auth == AuthMode::Oauth).then(|| login_state(d, env).as_str()),
                });
                if let Some(probes) = &probes
                    && let Some(status) = probes.get(i)
                {
                    row["probe"] = serde_json::json!({
                        "state": if status.connected() { "connected" } else { "failed" },
                        "tools": status.tools,
                        "error": status.error(),
                    });
                }
                row
            })
            .collect();
        let text = serde_json::to_string_pretty(&rows)
            .map_err(|e| RunError::Io(std::io::Error::other(e)))?;
        writeln!(io.stdout, "{text}")?;
        return Ok(());
    }

    if declared.is_empty() {
        writeln!(
            io.stdout,
            "No MCP servers configured. Add one with `iota mcp add <name> -- <command>` or `iota mcp add <name> --url <url>`."
        )?;
        return Ok(());
    }
    let width = column_width(declared.iter().map(|d| &d.name));
    let file_width = declared
        .iter()
        .map(|d| crate::text::width::str_width(&d.file.display().to_string()))
        .max()
        .unwrap_or(0);
    writeln!(io.stdout, "MCP servers:")?;
    for (i, d) in declared.iter().enumerate() {
        let mut line = format!(
            "  {:width$}  {:5}  {:file_width$}  [auth: {}]",
            d.name,
            transport(&d.entry),
            d.file.display(),
            auth_label(d, env)
        );
        if let Some(probes) = &probes
            && let Some(status) = probes.get(i)
        {
            line.push_str("  ");
            line.push_str(&probe_label(status));
        }
        writeln!(io.stdout, "{line}")?;
    }
    Ok(())
}

/// `--probe`: connect to every declared server the way a run would (the 30 s deadline, the config-order
/// merge) and report the resolved statuses, index-aligned with `declared`.
async fn probe(
    declared: &[Declared],
    env: &Env,
    cancel: &CancellationToken,
) -> Vec<crate::mcp::ServerStatus> {
    let configs: Vec<ServerConfig> = declared
        .iter()
        .map(|d| server_config(&d.name, &d.entry))
        .collect();
    let manager = crate::mcp::Manager::new(
        configs,
        crate::mcp::ManagerOptions::new(crate::llm::default_http_client(), env.clone()),
    );
    let statuses = manager.connect_all(cancel).await;
    manager.close().await;
    statuses
}

/// `connected (N tools)` or `failed: <first line of the error>`.
fn probe_label(status: &crate::mcp::ServerStatus) -> String {
    match status.error() {
        None => format!("connected ({} tools)", status.tool_count),
        Some(err) => format!("failed: {}", err.split('\n').next().unwrap_or_default()),
    }
}

/// The manager's config for one entry (what `assemble::build_mcp_configs` builds for a run).
pub(crate) fn server_config(name: &str, entry: &McpServerConfig) -> ServerConfig {
    ServerConfig {
        name: name.to_owned(),
        command: entry.command.clone(),
        args: entry.args.clone(),
        url: entry.url.clone(),
        env: entry.env.clone(),
        headers: entry.headers.clone(),
        auth: entry.auth,
        client_id: entry.client_id.clone(),
        client_secret: entry.client_secret.clone(),
    }
}

// ---------------------------------------------------------------- get

/// `iota mcp get <name>`: the entry as declared, the file it comes from, its auth.
fn run_get(
    name: &str,
    explicit: Option<&Path>,
    env: &Env,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let files = source_files(explicit, None, &env.dirs);
    let declared = declared_servers(&files)?;
    let Some(d) = declared.iter().find(|d| d.name == name) else {
        return Err(unknown(name, &declared).into());
    };
    writeln!(io.stdout, "{}:", d.name)?;
    writeln!(io.stdout, "  file: {}", d.file.display())?;
    writeln!(io.stdout, "  transport: {}", transport(&d.entry))?;
    if !d.entry.command.is_empty() {
        writeln!(io.stdout, "  command: {}", d.entry.command)?;
    }
    if !d.entry.args.is_empty() {
        writeln!(io.stdout, "  args:")?;
        for arg in &d.entry.args {
            writeln!(io.stdout, "    - {arg}")?;
        }
    }
    if !d.entry.url.is_empty() {
        writeln!(io.stdout, "  url: {}", d.entry.url)?;
    }
    for (label, map) in [("env", &d.entry.env), ("headers", &d.entry.headers)] {
        if map.is_empty() {
            continue;
        }
        writeln!(io.stdout, "  {label}:")?;
        for (k, v) in map {
            writeln!(io.stdout, "    {k}: {v}")?;
        }
    }
    if let Some(defer) = &d.entry.defer {
        writeln!(io.stdout, "  defer: {defer}")?;
    }
    if !d.entry.client_id.is_empty() {
        writeln!(io.stdout, "  client_id: {}", d.entry.client_id)?;
    }
    if !d.entry.client_secret.is_empty() {
        writeln!(io.stdout, "  client_secret: {}", d.entry.client_secret)?;
    }
    match d.entry.auth {
        AuthMode::Oauth => {
            let state = login_state(d, env);
            let path = token_store(d, env)
                .map(|s| format!("; {}", s.path().display()))
                .unwrap_or_default();
            writeln!(io.stdout, "  auth: oauth ({}{path})", state.as_str())?;
        }
        AuthMode::None => writeln!(io.stdout, "  auth: {}", auth_label(d, env))?,
    }
    Ok(())
}

// ---------------------------------------------------------------- login / logout

/// The declared OAuth server `name`, with its token store.
fn oauth_server(
    name: &str,
    explicit: Option<&Path>,
    env: &Env,
) -> Result<(Declared, crate::mcp::auth::TokenStore, ServerConfig), CliError> {
    let files = source_files(explicit, None, &env.dirs);
    let mut declared = declared_servers(&files)?;
    let Some(at) = declared.iter().position(|d| d.name == name) else {
        return Err(unknown(name, &declared).into());
    };
    let d = declared.remove(at);
    if d.entry.auth != AuthMode::Oauth {
        return Err(SetupError::McpNotOauth(name.to_owned()).into());
    }
    let expanded = crate::mcp::config::expand_server_config(&server_config(&d.name, &d.entry), env);
    let store = crate::mcp::auth::TokenStore::for_server(&env.dirs, &d.name, &expanded.url)
        .ok_or(SetupError::McpNoHomeForToken)?;
    Ok((d, store, expanded))
}

/// `iota mcp login <name> [--no-browser]`: the flow of `mcp::auth::login`, each step on stdout as it
/// happens (the URL must be on screen while the wait runs), a pasted redirect URL read from stdin as the
/// fallback to the loopback callback.
async fn run_login(
    name: &str,
    no_browser: bool,
    client_id: Option<String>,
    explicit: Option<&Path>,
    env: &Env,
    cancel: &CancellationToken,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    use crate::mcp::auth::{Browser, LoginRequest, LoginStep, login, step_line};

    let (_, store, expanded) = oauth_server(name, explicit, env)?;
    // `--client-id` beats the entry's; the entry's secret goes with the entry's id alone.
    let entry_secret = Some(expanded.client_secret.clone()).filter(|s| !s.is_empty());
    let (client_id, client_secret) = match client_id {
        Some(id) if id == expanded.client_id => (Some(id), entry_secret),
        Some(id) => (Some(id), None),
        None => (
            Some(expanded.client_id.clone()).filter(|s| !s.is_empty()),
            entry_secret,
        ),
    };
    let browser = if no_browser {
        Browser::Print
    } else {
        Browser::Open(env.clone())
    };
    // stdin as the paste source: one line, read on a DETACHED thread. At EOF (a pipe, `/dev/null`) it yields
    // nothing and the listener alone decides. Not `spawn_blocking`: a terminal's `read_line` cannot be
    // cancelled, and the runtime's shutdown waits for its blocking pool — the browser would have come back
    // and the command would still sit there until Enter. A plain thread is simply left behind at exit.
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::stdin()
            .read_line(&mut line)
            .ok()
            .filter(|&n| n > 0)
            .map(|_| line);
        let _ = tx.send(read);
    });
    let paste = Box::pin(async move { rx.await.ok().flatten() });
    let request = LoginRequest {
        name,
        url: &expanded.url,
        http: crate::llm::default_http_client(),
        store,
        browser,
        paste: Some(paste),
        cancel,
        client_id,
        client_secret,
    };
    let outcome = {
        let mut report = |step: LoginStep| {
            let line = match &step {
                LoginStep::Waiting { paste: true } => Some(
                    "Waiting for the browser to come back (5m), or paste the redirect URL here:"
                        .to_owned(),
                ),
                LoginStep::Waiting { paste: false } => {
                    Some("Waiting for the browser to come back (5m)…".to_owned())
                }
                other => step_line(other),
            };
            if let Some(line) = line {
                let _ = writeln!(io.stdout, "{line}");
                let _ = io.stdout.flush();
            }
        };
        Box::pin(login(request, &mut report)).await
    };
    let outcome = outcome.map_err(|source| RunError::McpLogin {
        name: name.to_owned(),
        source,
    })?;
    writeln!(
        io.stdout,
        "{}",
        crate::mcp::auth::logged_in_line(name, &outcome)
    )?;
    Ok(())
}

/// `iota mcp logout <name>`: the tokens are forgotten (revoked when the server publishes a revocation
/// endpoint) and the file removed.
async fn run_logout(
    name: &str,
    explicit: Option<&Path>,
    env: &Env,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    let (_, store, _) = oauth_server(name, explicit, env)?;
    let had = crate::mcp::auth::logout(&crate::llm::default_http_client(), &store)
        .await
        .map_err(|source| RunError::McpLogout {
            name: name.to_owned(),
            source,
        })?;
    if had {
        writeln!(
            io.stdout,
            "Logged out of {name} (forgot {})",
            store.path().display()
        )?;
    } else {
        writeln!(io.stdout, "Not logged in to {name} (nothing to forget)")?;
    }
    Ok(())
}

/// `SetupError::McpUnknown` with the names there are.
fn unknown(name: &str, declared: &[Declared]) -> SetupError {
    SetupError::McpUnknown {
        name: name.to_owned(),
        servers: declared.iter().map(|d| d.name.clone()).collect(),
    }
}

// ---------------------------------------------------------------- remove

/// `iota mcp remove <name> [--scope user|project]`: the entry leaves the ONE file that declares it; when both
/// tiers declare the name, `--scope` says which.
fn run_remove(
    name: &str,
    scope: Option<McpScope>,
    explicit: Option<&Path>,
    dirs: &HostDirs,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    if explicit.is_some() && scope.is_some() {
        return Err(ArgsError::McpScopeWithConfig.into());
    }
    let candidates = source_files(explicit, scope, dirs);
    let mut holding: Vec<(PathBuf, crate::config::edit::FileServers)> = Vec::new();
    for file in candidates {
        let read = read_mcp_servers(&file).map_err(SetupError::McpEdit)?;
        if read.servers.contains_key(name) {
            holding.push((file, read));
        }
    }
    let (file, read) = match holding.len() {
        1 => holding.remove(0),
        0 => {
            let declared = declared_servers(&source_files(explicit, scope, dirs))?;
            return Err(match scope {
                Some(scope) => SetupError::McpNotInScope {
                    name: name.to_owned(),
                    file: scope_file(scope, dirs)?.display().to_string(),
                },
                None => unknown(name, &declared),
            }
            .into());
        }
        _ => {
            return Err(SetupError::McpAmbiguous {
                name: name.to_owned(),
                files: holding
                    .iter()
                    .map(|(f, _)| f.display().to_string())
                    .collect(),
            }
            .into());
        }
    };
    let mut servers = read.servers;
    servers.remove(name);
    write_mcp_servers(&file, &read.text, &servers).map_err(SetupError::McpEdit)?;
    writeln!(io.stdout, "Removed {name} from {}", file.display())?;
    Ok(())
}
