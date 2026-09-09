//! The YAML config model (config/config.go): `~/.iota.yaml|yml` merged under `./.iota.yaml|yml` (or `-c <file>`
//! alone), provider aliases, top-level MCP servers, `${var}` expansion of `key`/`url`/`system_file` at merge time.
//! Decoded with `serde_norway`; every key Go accepts is accepted here (keys with no headless effect are parsed and
//! ignored), unknown keys are ignored, and bool fields take the YAML 1.1 spellings through `crate::tool::yaml11`
//! (DIVERGENCES I-01).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::app::{CONFIG_BASE, CONFIG_EXTS, HostDirs};
use crate::provider::error::InvalidEffort;
use crate::provider::{Effort, ImageGenParams};
use crate::tool::sets::ToolsConfig;
use crate::vars;
use crate::vars::VarResolver;

/// One `providers.<name>` entry (config.go `ProviderConfig`). Every field defaults; unknown keys are ignored.
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ProviderConfig {
    /// `type:` — the built-in provider type behind an alias (`""` = the entry's own name is the type).
    #[serde(rename = "type")]
    pub kind: String,
    /// `key:` — the API key (`${var}` expanded once at merge time).
    pub key: String,
    /// `url:` — the base URL (`${var}` expanded once at merge time).
    pub url: String,
    /// `model:` — the default model.
    pub model: String,
    /// `system:` — an inline system prompt (wins over `system_file`).
    pub system: String,
    /// `system_file:` — a file holding the system prompt (`${var}` expanded once at merge time).
    pub system_file: String,
    /// `effort:` — the default reasoning effort (`low|medium|high|xhigh|max`; `""` = provider default).
    pub effort: String,
    /// `temperature:` — the default sampling temperature (0.0-2.0; range-checked at resolution).
    pub temperature: Option<f64>,
    /// `top_p:` — nucleus sampling (0.0-1.0; range-checked by tuning).
    pub top_p: Option<f64>,
    /// `image:` — image-generation opt-in for providers that need one.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub image: bool,
    /// `aspect_ratio:` — image-provider generation default (verbatim; `""` omits it).
    pub aspect_ratio: String,
    /// `image_size:` — image-provider generation default (verbatim; `""` omits it).
    pub image_size: String,
    /// `negative_prompt:` — image-provider generation default (verbatim; `""` omits it).
    pub negative_prompt: String,
    /// `json_edits:` — JSON-body image edits (`images` type only); parsed, no headless effect beyond its warning.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub json_edits: bool,
    /// `no_save:` — parsed, no headless effect.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub no_save: bool,
    /// `notify:` — the attention ping sent while the terminal is UNFOCUSED (config.go:58-63).
    /// A THREE-state switch: absent (`None`) means ON, so only an explicit `notify: false`
    /// silences it. `cmd` threads `notify.unwrap_or(true)` into `host::Presenter::new`
    /// (root.go:414); the headless run has no presenter and ignores it.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_opt_bool")]
    pub notify: Option<bool>,
    /// `mcp_servers:` — which top-level servers this provider loads: `None` (key absent) = all, `Some([])` = none,
    /// names = that subset (an unknown name is `ConfigError::UnknownMcpServer`).
    pub mcp_servers: Option<Vec<String>>,
    /// `defer_mode:` — `normal` (default) | `reference` | `tool-search` | `system-tools`; resolved by
    /// `crate::tool::resolve_defer_mode`.
    pub defer_mode: String,
    /// `agent:` — agent mode for this provider (`--agent` equivalent).
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub agent: bool,
    /// `context_window:` — parsed (`window::parse_window_size`), no headless effect.
    pub context_window: String,
    /// `tools:` — RAW set values; key presence enables the set, the set's factory decodes its value.
    pub tools: ToolsConfig,
}

impl ProviderConfig {
    /// `effort:` as a level; `Ok(None)` when the key is unset.
    pub fn effort(&self) -> Result<Option<Effort>, InvalidEffort> {
        Effort::optional(&self.effort)
    }

    /// `aspect_ratio:` / `image_size:` / `negative_prompt:`; an empty value is unset.
    pub fn image_gen_params(&self) -> ImageGenParams {
        ImageGenParams::from_raw(&self.aspect_ratio, &self.image_size, &self.negative_prompt)
    }
}

/// One top-level `mcp_servers.<name>` entry (config.go `MCPServerConfig`).
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct McpServerConfig {
    /// stdio transport: the command to spawn.
    pub command: String,
    /// stdio transport: command arguments.
    pub args: Vec<String>,
    /// streamable-HTTP transport: the endpoint URL.
    pub url: String,
    /// Extra environment for the child process.
    pub env: BTreeMap<String, String>,
    /// Extra HTTP headers.
    pub headers: BTreeMap<String, String>,
    /// `defer:` — the server's one-line tool summary; `Some` opts into deferred loading (blank = loud warning,
    /// not deferred), `None` = advertise fully.
    pub defer: Option<String>,
}

/// The top-level config file (config.go `Config`).
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    /// `providers:` — aliases and per-type defaults, by name.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// `mcp_servers:` — the MCP servers a provider may select, by name.
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

impl Config {
    /// Never fails. `explicit` Some → that file only. Else `find_config_file(home)` then `find_config_file(cwd)`,
    /// each merged over the previous.
    /// Warnings (full text, with prefix): `Warning: config {path}: {err} (ignored)` (read error other than
    /// not-found), `Warning: config {path}: {err} (file ignored)` (parse error).
    pub fn load(
        explicit: Option<&Path>,
        dirs: &HostDirs,
        resolver: &dyn VarResolver,
        warn: &mut dyn FnMut(String),
    ) -> Config {
        let mut cfg = Config::default();
        if let Some(path) = explicit {
            cfg.merge_file(path, resolver, warn);
            return cfg;
        }
        // Global: ~/.iota.yaml / .yml, then local: ./.iota.yaml / .yml (config.go:122-134). A missing home or
        // working directory silently skips that tier.
        for dir in [dirs.home.as_deref(), dirs.cwd.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(path) = Self::find_config_file(dir) {
                cfg.merge_file(&path, resolver, warn);
            }
        }
        cfg
    }

    /// Whole-entry replace by name; expands `key`/`url`/`system_file` ONCE.
    pub fn merge_file(
        &mut self,
        path: &Path,
        resolver: &dyn VarResolver,
        warn: &mut dyn FnMut(String),
    ) {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                // config.go:172-176: a missing file is silent; any other read failure is loud.
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn(format!("Warning: config {}: {e} (ignored)", path.display()));
                }
                return;
            }
        };
        // config.go:178-185: LOUD, never silent — a parse error drops the whole file, with a warning saying why.
        let file: Config = match serde_norway::from_slice(&data) {
            Ok(file) => file,
            Err(e) => {
                warn(format!(
                    "Warning: config {}: {e} (file ignored)",
                    path.display()
                ));
                return;
            }
        };
        for (name, mut provider_cfg) in file.providers {
            provider_cfg.key = expand_owned(provider_cfg.key, resolver);
            provider_cfg.url = expand_owned(provider_cfg.url, resolver);
            provider_cfg.system_file = expand_owned(provider_cfg.system_file, resolver);
            self.providers.insert(name, provider_cfg);
        }
        for (name, server_cfg) in file.mcp_servers {
            self.mcp_servers.insert(name, server_cfg);
        }
    }

    /// `<dir>/.iota.yaml` then `<dir>/.iota.yml`; first that exists (metadata Ok).
    pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
        CONFIG_EXTS
            .iter()
            .map(|ext| dir.join(format!("{CONFIG_BASE}{ext}")))
            .find(|p| std::fs::metadata(p).is_ok())
    }

    /// Unconfigured → (name, default); configured → (kind, or name when kind is `""`; the provider's config).
    pub fn get(&self, name: &str) -> (String, ProviderConfig) {
        match self.providers.get(name) {
            None => (name.to_owned(), ProviderConfig::default()),
            Some(provider_cfg) => {
                let kind = if provider_cfg.kind.is_empty() {
                    name.to_owned()
                } else {
                    provider_cfg.kind.clone()
                };
                (kind, provider_cfg.clone())
            }
        }
    }

    /// `None` → all; `Some([])` → empty; names → subset; unknown → `Err(UnknownMcpServer)`.
    pub fn mcp_servers_for(
        &self,
        provider_cfg: &ProviderConfig,
    ) -> Result<BTreeMap<String, McpServerConfig>, ConfigError> {
        let Some(names) = &provider_cfg.mcp_servers else {
            return Ok(self.mcp_servers.clone());
        };
        names
            .iter()
            .map(|name| {
                self.mcp_servers
                    .get(name)
                    .map(|server_cfg| (name.clone(), server_cfg.clone()))
                    .ok_or_else(|| ConfigError::UnknownMcpServer(name.clone()))
            })
            .collect()
    }
}

impl ProviderConfig {
    /// Inline `system` wins; else `system_file` verbatim; neither → `""`.
    pub fn resolve_system(&self) -> Result<String, ConfigError> {
        if !self.system.is_empty() || self.system_file.is_empty() {
            return Ok(self.system.clone());
        }
        // Go's `os.ReadFile` error names the file (`open <path>: …`); keep that shape so the user learns WHICH
        // file could not be read. The text after the path is the OS's own (DIVERGENCES D-16 spirit).
        std::fs::read(&self.system_file)
            .map(|data| String::from_utf8_lossy(&data).into_owned())
            .map_err(|e| {
                ConfigError::SystemFile(std::io::Error::new(
                    e.kind(),
                    format!("open {}: {e}", self.system_file),
                ))
            })
    }
}

/// `vars::expand` on an owned string, allocating only when a `${…}` was substituted.
fn expand_owned(s: String, resolver: &dyn VarResolver) -> String {
    match vars::expand(&s, resolver) {
        std::borrow::Cow::Borrowed(_) => s,
        std::borrow::Cow::Owned(expanded) => expanded,
    }
}

/// Config-level failures that abort a run (config.go texts).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `system_file:` could not be read (a silently empty system prompt is worse than failing loudly).
    #[error("system_file: {0}")]
    SystemFile(#[source] std::io::Error),
    /// A provider's `mcp_servers:` names a server the top-level map does not define.
    #[error("mcp_servers: {0:?} is not defined under the top-level mcp_servers")]
    UnknownMcpServer(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::Config;
    use crate::app::HostDirs;
    use crate::testing::map_resolver;

    // Go: config/config_test.go:222 TestNotifyField — `notify` parses as a switch and defaults
    // to nil: absent means ON, so only an explicit `false` silences the attention channels.
    // (The `unwrap_or(true)` default itself is pinned where `cmd` threads it into the presenter.)
    #[test]
    fn test_notify_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.yaml");
        std::fs::write(
            &path,
            "providers:\n  quiet:\n    type: openai\n    notify: false\n  norm:\n    type: openai\n",
        )
        .expect("write config");
        let mut warnings = Vec::new();
        let cfg = Config::load(
            Some(&path),
            &HostDirs::default(),
            &map_resolver(&[]),
            &mut |w| warnings.push(w),
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let (_, provider_cfg) = cfg.get("quiet");
        assert_eq!(provider_cfg.notify, Some(false), "notify: false not parsed");
        assert!(
            !provider_cfg.notify.unwrap_or(true),
            "an explicit false must silence"
        );

        let (_, provider_cfg) = cfg.get("norm");
        assert_eq!(provider_cfg.notify, None, "notify must default nil (on)");
        assert!(
            provider_cfg.notify.unwrap_or(true),
            "an absent notify means ON"
        );
    }
}
