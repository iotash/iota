//! `-l` (cmd/root.go:439-529): without a provider, the configured providers that have a key (sorted lines); with a
//! provider, its model list fetched through `new_provider` (`Fetching available models...` on stderr).

use std::io::Write;

use crate::provider::ProviderParams;
use crate::provider::{HttpTransport, ProviderKind, provider_env_key};
use crate::vars::EnvSource;
use tokio_util::sync::CancellationToken;

use crate::cmd::cli::Cli;
use crate::cmd::resolve::{resolve_key_from_env_or_config, resolve_target};
use crate::cmd::{CliError, io};
use crate::config::{Config, ProviderConfig};

/// Whether a provider has a usable key: config `key:` non-empty, else the env var of `raw_type` set (root.go:439).
pub fn has_api_key(raw_type: &str, provider_cfg: &ProviderConfig, env: &dyn EnvSource) -> bool {
    !provider_cfg.key.is_empty()
        || env
            .var(provider_env_key(raw_type))
            .is_some_and(|v| !v.is_empty())
}

/// What `-l <provider>` resolved before it constructs a provider (root.go:485-508): the pure half of `run_list`,
/// so the precedence is testable without a network (`list_key_flag_wins`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListTarget {
    /// The provider argument as typed (the `Models for {name}:` heading).
    pub name: String,
    /// The resolved provider type string (`Config::get`).
    pub raw_type: String,
    /// The API key (`-k` verbatim, even `""` > env of the resolved type > config key); never empty.
    pub api_key: String,
    /// The base URL (`-u` if given > config; `""` = dialect default).
    pub base_url: String,
}

/// root.go:485-508, pure: name check, key = `-k` if given (verbatim, even `""`) > env of the resolved type >
/// config (`ApiKeyRequiredForList`), url = `-u` if given > config.
pub fn resolve_list(cli: &Cli, cfg: &Config, env: &dyn EnvSource) -> Result<ListTarget, CliError> {
    let name = cli.provider.as_deref().unwrap_or_default();
    // The argument goes through the same four-level resolution the run does, so `-l <agent>` lists the models
    // of the provider that agent's default model rides on.
    let resolved = resolve_target(cfg, name)?;
    let raw_type = resolved.provider_type.clone();
    let env_key = provider_env_key(&raw_type);
    let api_key = match &cli.key {
        Some(flag) => flag.clone(),
        None => resolve_key_from_env_or_config(env_key, &resolved.provider, env),
    };
    let base_url = cli
        .url
        .clone()
        .unwrap_or_else(|| resolved.provider.url.clone());
    if api_key.is_empty() {
        return Err(CliError::ApiKeyRequiredForList(env_key));
    }
    Ok(ListTarget {
        name: name.to_owned(),
        raw_type,
        api_key,
        base_url,
    })
}

/// `-l` without provider: configured providers with a key, lines sorted; `No providers configured. Set API keys
/// via environment variables or ~/.iota.yaml` / `Available providers:` + `  {line}`.
/// `-l <provider>`: check name; key = `-k` if given (verbatim, even `""`) > env of the resolved type > config
/// (`API key is required to list models: …`); url = `-u` if given > config (root.go:490-500); `new_provider`
/// (model `""`); stderr `Fetching available models...`; `failed to list models: {e}`; `No models available.` /
/// `Models for {name}:` + `  {model}` in provider order. (`list_key_flag_wins` pins the precedence.)
pub async fn run_list(
    cli: &Cli,
    cfg: &Config,
    env: &dyn EnvSource,
    cancel: &CancellationToken,
    http: Option<HttpTransport>,
    io: &mut io::Streams,
) -> Result<(), CliError> {
    if cli.provider.is_none() {
        return list_providers(cfg, env, io);
    }

    // List models for a specific provider (root.go:484-528).
    let target = resolve_list(cli, cfg, env)?;
    let kind: ProviderKind = target.raw_type.parse()?;
    let provider = crate::provider::new_provider(
        kind,
        ProviderParams {
            api_key: &target.api_key,
            base_url: &target.base_url,
            model: "",
            temperature: None,
        },
        http,
    )?;
    // chat/chat.go:21-24 (FetchModels): the notice precedes the request.
    writeln!(io.stderr, "Fetching available models...")?;
    let models = provider
        .list_models(cancel)
        .await
        .map_err(CliError::ListModels)?;
    if models.is_empty() {
        writeln!(io.stdout, "No models available.")?;
        return Ok(());
    }
    writeln!(io.stdout, "Models for {}:", target.name)?;
    for model in &models {
        writeln!(io.stdout, "  {model}")?;
    }
    Ok(())
}

/// root.go:448-482: the configured providers that have a key, one sorted line each.
fn list_providers(cfg: &Config, env: &dyn EnvSource, io: &mut io::Streams) -> Result<(), CliError> {
    let mut available: Vec<String> = cfg
        .providers
        .keys()
        .filter_map(|name| {
            let (raw_type, provider_cfg) = cfg.get(name);
            has_api_key(&raw_type, &provider_cfg, env)
                .then(|| provider_line(name, &raw_type, &provider_cfg, cfg.default_model_id(name)))
        })
        .collect();
    available.sort();

    if available.is_empty() {
        writeln!(
            io.stdout,
            "No providers configured. Set API keys via environment variables or ~/.iota.yaml"
        )?;
        return Ok(());
    }
    writeln!(io.stdout, "Available providers:")?;
    for line in &available {
        writeln!(io.stdout, "  {line}")?;
    }
    Ok(())
}

/// One `-l` line: `{name} (type: {t}[, url: {u}][, model: {m}])` when the type differs from the name, else
/// `{name}` / `{name} (default model: {m})`. `model` is the id of the same-named `models:` entry — which is
/// exactly the `model:` a one-layer block used to write here.
pub fn provider_line(
    name: &str,
    raw_type: &str,
    provider_cfg: &ProviderConfig,
    model: &str,
) -> String {
    // root.go:455-467, piece by piece: ` (type: %s`, `, url: %s`, `, model: %s`, `)` / ` (default model: %s)`.
    let mut info = name.to_owned();
    if raw_type != name {
        info.push_str(" (type: ");
        info.push_str(raw_type);
        if !provider_cfg.url.is_empty() {
            info.push_str(", url: ");
            info.push_str(&provider_cfg.url);
        }
        if !model.is_empty() {
            info.push_str(", model: ");
            info.push_str(model);
        }
        info.push(')');
    } else if !model.is_empty() {
        info.push_str(" (default model: ");
        info.push_str(model);
        info.push(')');
    }
    info
}
