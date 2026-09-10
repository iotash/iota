//! The YAML config model: `~/.iota.yaml|yml` merged under `./.iota.yaml|yml` (or `-c <file>` alone), decoded
//! with `serde_norway`, `${var}`-expanded at merge time and validated once at the end of [`Config::load`].
//!
//! THREE layers, each with one job (brain page `config-three-layers`):
//!
//! - `providers` — the endpoint: `type`/`key`/`url`, and nothing else;
//! - `models` — a provider reference plus the wire id, the model's own properties and the protocol
//!   its dialect decides (`defer_mode`, the image knobs), plus tunable defaults;
//! - `agents` — how a model is driven: the candidate set, the prompt, the tools, the MCP subset and
//!   the session switches.
//!
//! A one-layer config (every key under `providers.<name>`) is still accepted: the `migrate` module splits it
//! into the three entries it means and prints one deprecation line per block. Unknown keys are ignored everywhere and
//! bool fields take the YAML 1.1 spellings through `crate::tool::yaml11` (DIVERGENCES I-01).

pub mod agent;
pub mod migrate;
pub mod model;
pub mod provider;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::app::{CONFIG_BASE, CONFIG_EXTS, HostDirs};
use crate::provider::ProviderKind;
use crate::tool::DeferMode;
use crate::vars;
use crate::vars::VarResolver;

pub use agent::AgentConfig;
pub use model::{BadModelRef, ModelConfig, ModelEntry, ModelRef};
pub use provider::ProviderConfig;

use migrate::LegacyProviderEntry;

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

/// ONE config document, exactly as it is written on disk. [`Config`] is what a stack of these merges into,
/// which is why the two shapes are separate types: `providers` here still accepts the one-layer form.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(default)]
struct ConfigFile {
    /// `providers:` — endpoints (one-layer blocks accepted, see [`migrate`]).
    providers: BTreeMap<String, LegacyProviderEntry>,
    /// `models:` — configured models.
    models: BTreeMap<String, ModelEntry>,
    /// `agents:` — configured usages.
    agents: BTreeMap<String, AgentConfig>,
    /// `mcp_servers:` — the servers an agent may select.
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

/// The merged, expanded and validated config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// `providers:` — endpoints by name.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// `models:` — configured models by name.
    pub models: BTreeMap<String, ModelConfig>,
    /// `agents:` — configured usages by name.
    pub agents: BTreeMap<String, AgentConfig>,
    /// `mcp_servers:` — the MCP servers an agent may select, by name.
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

/// What a positional name resolved to: one entry from each layer, defaults where the name reached no further.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    /// The name as typed.
    pub name: String,
    /// The `providers:` entry (or built-in type) the run talks to.
    pub provider_name: String,
    /// Its resolved TYPE string (`type:`, or the entry's own name).
    pub provider_type: String,
    /// The endpoint entry (default when the name is an unconfigured built-in type).
    pub provider: ProviderConfig,
    /// The default model; `id` is `""` when the candidate set starts with a wildcard (the picker opens).
    pub model: ModelConfig,
    /// The agent driving it (default when the name reached no agent).
    pub agent: AgentConfig,
    /// The `agents:` entry name, or `""` when the positional name reached no agent. It is what a session
    /// bundle records, so a resume can say whether the agent it ran under still exists.
    pub agent_name: String,
}

impl Resolved {
    /// The agent's override, else the model's default.
    pub fn effort(&self) -> &str {
        if self.agent.effort.is_empty() {
            &self.model.effort
        } else {
            &self.agent.effort
        }
    }

    /// The agent's override, else the model's default.
    pub fn temperature(&self) -> Option<f64> {
        self.agent.temperature.or(self.model.temperature)
    }

    /// The agent's override, else the model's default.
    pub fn top_p(&self) -> Option<f64> {
        self.agent.top_p.or(self.model.top_p)
    }

    /// Points the run at another provider, keeping the model entry it already carries.
    pub fn move_to_provider(&mut self, cfg: &Config, name: &str) {
        let (provider_type, provider) = cfg.get(name);
        name.clone_into(&mut self.provider_name);
        self.provider_type = provider_type;
        self.provider = provider;
        name.clone_into(&mut self.model.provider);
    }

    /// Whether `provider_name`/`id` is inside the agent's candidate set. A `provider:*` entry covers every id
    /// that provider serves, and a bare entry name covers the model it points at.
    pub fn covers(&self, cfg: &Config, provider_name: &str, id: &str) -> bool {
        self.agent.models.iter().any(|r| match r {
            ModelRef::Entry(name) => cfg
                .models
                .get(name)
                .is_some_and(|m| m.provider_or(name) == provider_name && m.id == id),
            ModelRef::Inline { provider, id: want } => provider == provider_name && want == id,
            ModelRef::All { provider } => provider == provider_name,
        })
    }
}

impl Config {
    /// `explicit` Some → that file only. Else `find_config_file(home)` then `find_config_file(cwd)`, each
    /// merged over the previous. A read or parse failure is a WARNING that drops the file; only the
    /// cross-layer validation at the end can fail the load.
    ///
    /// Warnings (full text, with prefix): `Warning: config {path}: {err} (ignored)` (read error other than
    /// not-found), `Warning: config {path}: {err} (file ignored)` (parse error), plus the soft-migration lines.
    pub fn load(
        explicit: Option<&Path>,
        dirs: &HostDirs,
        resolver: &dyn VarResolver,
        warn: &mut dyn FnMut(String),
    ) -> Result<Config, ConfigError> {
        let mut cfg = Config::default();
        if let Some(path) = explicit {
            cfg.merge_file(path, resolver, warn);
        } else {
            // Global: ~/.iota.yaml / .yml, then local: ./.iota.yaml / .yml (config.go:122-134). A missing home
            // or working directory silently skips that tier.
            for dir in [dirs.home.as_deref(), dirs.cwd.as_deref()]
                .into_iter()
                .flatten()
            {
                if let Some(path) = Self::find_config_file(dir) {
                    cfg.merge_file(&path, resolver, warn);
                }
            }
        }
        cfg.validate(warn)?;
        Ok(cfg)
    }

    /// ONE document, decoded → expanded → migrated → validated. `load` without the file discovery, and the
    /// entry point every test that has its config as a string uses.
    pub fn parse(
        data: &[u8],
        resolver: &dyn VarResolver,
        warn: &mut dyn FnMut(String),
    ) -> Result<Config, ConfigError> {
        let file: ConfigFile =
            serde_norway::from_slice(data).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let mut cfg = Config::default();
        cfg.merge_document(file, resolver, warn)?;
        cfg.validate(warn)?;
        Ok(cfg)
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
        let file: ConfigFile = match serde_norway::from_slice(&data) {
            Ok(file) => file,
            Err(e) => {
                warn(format!(
                    "Warning: config {}: {e} (file ignored)",
                    path.display()
                ));
                return;
            }
        };
        // A structural refusal inside ONE file (a `models:` shorthand that names no provider) is the same
        // class of failure as a parse error, and Go's rule for those is: drop the file, say why, carry on.
        if let Err(e) = self.merge_document(file, resolver, warn) {
            warn(format!(
                "Warning: config {}: {e} (file ignored)",
                path.display()
            ));
        }
    }

    /// Merges one decoded document over what is already here.
    fn merge_document(
        &mut self,
        file: ConfigFile,
        resolver: &dyn VarResolver,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), ConfigError> {
        for (name, mut entry) in file.providers {
            entry.key = expand_owned(entry.key, resolver);
            entry.url = expand_owned(entry.url, resolver);
            entry.system_file = expand_owned(entry.system_file, resolver);
            let mut split = entry.split(&name, warn);
            if let Some(a) = &mut split.agent {
                migrate::rename_skills_set(&mut a.tools, &format!("providers.{name}.tools"), warn);
            }
            self.providers.insert(name.clone(), split.provider);
            // Go replaced the WHOLE provider entry by name; the implicit entries follow it, so a later file
            // that redefines a provider drops the model and the agent the earlier one implied.
            replace_migrated(&mut self.models, &name, split.model);
            replace_migrated(&mut self.agents, &name, split.agent);
        }
        for (name, entry) in file.models {
            self.models.insert(name.clone(), entry.into_config(&name)?);
        }
        for (name, mut agent_cfg) in file.agents {
            agent_cfg.system_file = expand_owned(agent_cfg.system_file, resolver);
            migrate::rename_skills_set(&mut agent_cfg.tools, &format!("agents.{name}.tools"), warn);
            self.agents.insert(name, agent_cfg);
        }
        for (name, server_cfg) in file.mcp_servers {
            self.mcp_servers.insert(name, server_cfg);
        }
        Ok(())
    }

    /// The cross-layer rules, checked once: every reference resolves, and every `defer_mode` applies to the
    /// dialect of the provider its model points at (that mismatch used to be a runtime warning that silently
    /// downgraded to `normal`).
    fn validate(&self, warn: &mut dyn FnMut(String)) -> Result<(), ConfigError> {
        for (name, m) in &self.models {
            let provider_name = m.provider_or(name);
            if !self.knows_provider(provider_name) {
                return Err(ConfigError::Model(
                    name.clone(),
                    format!("unknown provider {provider_name:?}"),
                ));
            }
            if m.defer_mode.is_empty() {
                continue;
            }
            let Some(mode) = DeferMode::from_name(&m.defer_mode) else {
                warn(format!(
                    "Warning: config models.{name}: unknown defer_mode {:?} (using {})",
                    m.defer_mode,
                    DeferMode::DEFAULT.name()
                ));
                continue;
            };
            // An unknown `type:` has its own error at construction; there is no dialect to judge against here.
            let Ok(kind) = self.provider_type(provider_name).parse::<ProviderKind>() else {
                continue;
            };
            if !mode.supports(kind) {
                return Err(ConfigError::DeferMode {
                    model: name.clone(),
                    mode: m.defer_mode.clone(),
                    kind,
                });
            }
        }
        for (name, a) in &self.agents {
            if a.models.is_empty() {
                return Err(ConfigError::Agent(
                    name.clone(),
                    "models: at least one model is required".to_owned(),
                ));
            }
            for r in &a.models {
                match r {
                    ModelRef::Entry(entry) if !self.models.contains_key(entry) => {
                        return Err(ConfigError::Agent(
                            name.clone(),
                            format!("models: unknown model {entry:?}"),
                        ));
                    }
                    ModelRef::Inline { provider, .. } | ModelRef::All { provider }
                        if !self.knows_provider(provider) =>
                    {
                        return Err(ConfigError::Agent(
                            name.clone(),
                            format!("models: unknown provider {provider:?}"),
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Whether `name` is a configured provider or a built-in type.
    pub fn knows_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name) || ProviderKind::is_known(name)
    }

    /// The TYPE string behind a provider name (`type:`, else the name itself).
    fn provider_type<'a>(&'a self, name: &'a str) -> &'a str {
        self.providers.get(name).map_or(name, |p| p.kind_or(name))
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
            Some(provider_cfg) => (provider_cfg.kind_or(name).to_owned(), provider_cfg.clone()),
        }
    }

    /// The four-level positional resolution: `agents:` → `models:` → `providers:` → a built-in type. A name
    /// defined in more than one layer is taken from the FIRST that has it, so an agent shadows a model of the
    /// same name and a model shadows a provider. `None` = the name is nowhere.
    pub fn resolve(&self, name: &str) -> Option<Resolved> {
        if let Some(agent_cfg) = self.agents.get(name) {
            let model = agent_cfg
                .models
                .first()
                .and_then(|r| self.model_of(r))
                .unwrap_or_default();
            let mut resolved = self.finish(name, model, agent_cfg.clone());
            name.clone_into(&mut resolved.agent_name);
            return Some(resolved);
        }
        if let Some(model) = self.models.get(name) {
            let mut model = model.clone();
            model.anchor_provider(name);
            return Some(self.finish(name, model, AgentConfig::default()));
        }
        if self.providers.contains_key(name) || ProviderKind::is_known(name) {
            let model = ModelConfig {
                provider: name.to_owned(),
                ..ModelConfig::default()
            };
            return Some(self.finish(name, model, AgentConfig::default()));
        }
        None
    }

    /// The model a reference names: a `models:` entry, an inline `provider:id`, or the provider alone for a
    /// `provider:*` wildcard (which leaves `id` empty — that IS "ask the picker").
    pub fn model_of(&self, r: &ModelRef) -> Option<ModelConfig> {
        match r {
            ModelRef::Entry(name) => self.models.get(name).map(|m| {
                let mut m = m.clone();
                m.anchor_provider(name);
                m
            }),
            ModelRef::Inline { provider, id } => Some(ModelConfig {
                provider: provider.clone(),
                id: id.clone(),
                ..ModelConfig::default()
            }),
            ModelRef::All { provider } => Some(ModelConfig {
                provider: provider.clone(),
                ..ModelConfig::default()
            }),
        }
    }

    /// Fills the endpoint half of a [`Resolved`] from whatever provider the model landed on.
    fn finish(&self, name: &str, model: ModelConfig, agent: AgentConfig) -> Resolved {
        let provider_name = if model.provider.is_empty() {
            name.to_owned()
        } else {
            model.provider.clone()
        };
        let (provider_type, provider) = self.get(&provider_name);
        Resolved {
            name: name.to_owned(),
            provider_name,
            provider_type,
            provider,
            model,
            agent,
            agent_name: String::new(),
        }
    }

    /// The default model id shown for a provider in the `-l` listing: the `models:` entry that carries the
    /// provider's own name (which is where a migrated one-layer `model:` lands). `""` when there is none.
    pub fn default_model_id(&self, provider_name: &str) -> &str {
        self.models
            .get(provider_name)
            .filter(|m| m.provider_or(provider_name) == provider_name)
            .map_or("", |m| m.id.as_str())
    }

    /// Every name a positional argument may carry, sorted and deduplicated (the `unknown provider` hint).
    pub fn configured_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .agents
            .keys()
            .chain(self.models.keys())
            .chain(self.providers.keys())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// `None` → all; `Some([])` → empty; names → subset; unknown → `Err(UnknownMcpServer)`.
    pub fn mcp_servers_for(
        &self,
        agent_cfg: &AgentConfig,
    ) -> Result<BTreeMap<String, McpServerConfig>, ConfigError> {
        let Some(names) = &agent_cfg.mcp_servers else {
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

/// Installs the implicit entry a migrated provider block produced — or, when the block had none, clears the
/// implicit entry an earlier file left behind. An entry the user wrote explicitly is never touched.
fn replace_migrated<T>(map: &mut BTreeMap<String, T>, name: &str, entry: Option<T>)
where
    T: Migrated,
{
    match entry {
        Some(e) => {
            map.insert(name.to_owned(), e);
        }
        None => {
            if map.get(name).is_some_and(Migrated::is_migrated) {
                map.remove(name);
            }
        }
    }
}

/// Whether an entry was synthesised by [`migrate`] rather than written by the user.
trait Migrated {
    /// True for a synthesised entry.
    fn is_migrated(&self) -> bool;
}

impl Migrated for ModelConfig {
    fn is_migrated(&self) -> bool {
        self.migrated
    }
}

impl Migrated for AgentConfig {
    fn is_migrated(&self) -> bool {
        self.migrated
    }
}

/// `vars::expand` on an owned string, allocating only when a `${…}` was substituted.
fn expand_owned(s: String, resolver: &dyn VarResolver) -> String {
    match vars::expand(&s, resolver) {
        std::borrow::Cow::Borrowed(_) => s,
        std::borrow::Cow::Owned(expanded) => expanded,
    }
}

/// Config-level failures that abort a run.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The document could not be decoded (only [`Config::parse`] surfaces this; `load` warns and drops the
    /// file instead).
    #[error("{0}")]
    Parse(String),
    /// `system_file:` could not be read (a silently empty system prompt is worse than failing loudly).
    #[error("system_file: {0}")]
    SystemFile(#[source] std::io::Error),
    /// An agent's `mcp_servers:` names a server the top-level map does not define.
    #[error("mcp_servers: {0:?} is not defined under the top-level mcp_servers")]
    UnknownMcpServer(String),
    /// A `models:` entry is malformed or points nowhere.
    #[error("models.{0}: {1}")]
    Model(String, String),
    /// An `agents:` entry is malformed or points nowhere.
    #[error("agents.{0}: {1}")]
    Agent(String, String),
    /// A `defer_mode:` that the dialect of its model's provider cannot speak. It used to warn at runtime and
    /// silently fall back to `normal`; a protocol the provider does not implement is a configuration
    /// mistake, so it is refused where it is written.
    #[error(
        "models.{model}: defer_mode {mode:?} does not apply to provider type {kind} (see docs/design/tool-defer.md)"
    )]
    DeferMode {
        /// The `models:` entry.
        model: String,
        /// The mode as written.
        mode: String,
        /// The dialect it landed on.
        kind: ProviderKind,
    },
}
