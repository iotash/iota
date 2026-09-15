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
//! Every key is checked against the layer it was written in BEFORE the document is decoded (`strict`), so a key
//! of another layer — or one that is simply misspelled — is an error naming its coordinate, never a silently
//! ignored line. Bool fields take the YAML 1.1 spellings through `crate::tool::yaml11` (DIVERGENCES I-01).

pub mod agent;
pub mod model;
pub mod params;
pub mod provider;
mod strict;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::app::env::{Env, expand};
use crate::app::{CONFIG_BASE, CONFIG_EXTS, HostDirs};
use crate::provider::ProviderKind;
use crate::tool::DeferMode;

pub use agent::AgentConfig;
pub use model::{BadModelRef, ModelConfig, ModelEntry, ModelRef};
pub use params::{Declared, ParamLayers, WindowDecl};
pub use provider::ProviderConfig;

/// The `agents:` entry a run with no positional argument falls back to.
pub const DEFAULT_AGENT: &str = "default";

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
/// which is why the two shapes are separate types: a document is what ONE file said, a `Config` what the whole
/// stack means.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(default)]
struct ConfigFile {
    /// `providers:` — endpoints.
    providers: BTreeMap<String, ProviderConfig>,
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

    /// The `context_window:` this run declares — the agent's override, else the model's own — as it was
    /// WRITTEN; `None` when neither layer wrote one. Parsing it is the caller's (see [`WindowDecl`]).
    pub fn window_decl(&self) -> Option<WindowDecl<'_>> {
        Declared::window_decl(&self.agent, &self.model)
    }

    /// The four layered declarations this run resolved to, with [`window_decl`](Resolved::window_decl)
    /// already parsed. What a new session evaluates at startup, and what the `/model` switch re-evaluates
    /// against another model (brain page `model-param-layering`).
    pub fn declared(&self, window: Option<u64>) -> Declared {
        Declared::of(&self.agent, &self.model, window)
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
    /// Loads every file [`sources`](Config::sources) names, each merged over the previous, then validates the
    /// result once.
    ///
    /// A missing file is silent and an UNREADABLE one is a warning that drops it, because neither says
    /// anything about what the user meant. A file that parses but says something wrong — a key of another
    /// layer, an unknown toolset, a reference that points nowhere — FAILS the load: it is a mistake with an
    /// obvious fix, and a run that quietly ignored it would do the wrong thing silently.
    ///
    /// Warnings (full text, with prefix): `Warning: config {path}: {err} (ignored)` (read error other than
    /// not-found), `Warning: config {path}: {err} (file ignored)` (YAML syntax error).
    pub fn load(
        explicit: Option<&Path>,
        env: &Env,
        warn: &mut dyn FnMut(String),
    ) -> Result<Config, ConfigError> {
        let mut cfg = Config::default();
        for path in Self::sources(explicit, &env.dirs) {
            cfg.merge_file(&path, env, warn)?;
        }
        cfg.validate(warn)?;
        Ok(cfg)
    }

    /// The files this invocation reads, in merge order: `-c <file>` alone, else the global
    /// `~/.iota.yaml|yml` followed by the project-local `./.iota.yaml|yml` (config.go:122-134). A missing home
    /// or working directory silently skips that tier; an explicit path is returned whether or not it exists,
    /// which is what `iota config path` reports.
    pub fn sources(explicit: Option<&Path>, dirs: &HostDirs) -> Vec<PathBuf> {
        match explicit {
            Some(path) => vec![path.to_path_buf()],
            None => [dirs.home.as_deref(), dirs.cwd.as_deref()]
                .into_iter()
                .flatten()
                .filter_map(Self::find_config_file)
                .collect(),
        }
    }

    /// ONE document, decoded → expanded → validated. `load` without the file discovery, and the entry point
    /// every test that has its config as a string uses.
    pub fn parse(
        data: &[u8],
        env: &Env,
        warn: &mut dyn FnMut(String),
    ) -> Result<Config, ConfigError> {
        let mut cfg = Config::default();
        cfg.merge_document(decode(data)?, env)?;
        cfg.validate(warn)?;
        Ok(cfg)
    }

    /// One file, merged over what is already here (whole entry at a time, by name); expands `key`/`url`/
    /// `system_file` ONCE. See [`load`](Config::load) for which failures warn and which abort.
    pub fn merge_file(
        &mut self,
        path: &Path,
        env: &Env,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), ConfigError> {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                // config.go:172-176: a missing file is silent; any other read failure is loud.
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn(format!("Warning: config {}: {e} (ignored)", path.display()));
                }
                return Ok(());
            }
        };
        let merged = decode(&data).and_then(|file| self.merge_document(file, env));
        match merged {
            Ok(()) => {}
            // config.go:178-185: a file that is not YAML at all is dropped with a warning saying why — there
            // is no coordinate to report and nothing to act on but the parser's own message.
            Err(ConfigError::Parse(e)) => warn(format!(
                "Warning: config {}: {e} (file ignored)",
                path.display()
            )),
            // Everything else names a key or a reference the user wrote, so it fails the load with the file
            // it was written in.
            Err(e) => {
                return Err(ConfigError::File {
                    path: path.display().to_string(),
                    source: Box::new(e),
                });
            }
        }
        Ok(())
    }

    /// Merges one decoded document over what is already here.
    fn merge_document(&mut self, file: ConfigFile, env: &Env) -> Result<(), ConfigError> {
        for (name, mut entry) in file.providers {
            entry.key = expand_owned(entry.key, env);
            entry.url = expand_owned(entry.url, env);
            self.providers.insert(name, entry);
        }
        for (name, entry) in file.models {
            self.models.insert(name.clone(), entry.into_config(&name)?);
        }
        for (name, mut agent_cfg) in file.agents {
            agent_cfg.system_file = expand_owned(agent_cfg.system_file, env);
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

    /// The agent a bare `iota` runs: [`DEFAULT_AGENT`], when the config declares it.
    ///
    /// The fallback is deliberately `agents:` only: a `models.default` or a `providers.default` says which
    /// model or endpoint it is, never how to drive one, so there is one entry point and not three.
    pub fn default_agent(&self) -> Option<&str> {
        self.agents
            .contains_key(DEFAULT_AGENT)
            .then_some(DEFAULT_AGENT)
    }

    /// What `iota run <name>` resolves: the `agents:` entry alone, with the model it defaults to and the
    /// endpoint that model rides on. `None` = no such agent.
    ///
    /// It used to fall through to `models:`, `providers:` and finally a built-in provider type, so one name
    /// could mean four things and a collision silently changed what ran. An agent is now the ONLY thing a run
    /// can name (brain page `cli-surface-agent-first`); the other two layers are reached through it.
    pub fn resolve_agent(&self, name: &str) -> Option<Resolved> {
        let agent_cfg = self.agents.get(name)?;
        let model = agent_cfg
            .models
            .first()
            .and_then(|r| self.model_of(r))
            .unwrap_or_default();
        let mut resolved = self.finish(name, model, agent_cfg.clone());
        name.clone_into(&mut resolved.agent_name);
        Some(resolved)
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

    /// Every agent a run may name, in config order (`BTreeMap` = sorted): the `unknown agent` hint and
    /// `iota list agents`.
    pub fn agent_names(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
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

/// ONE document, decoded: YAML syntax first ([`ConfigError::Parse`]), then the key audit, which is what makes
/// a misplaced or misspelled key an error naming its coordinate rather than a line nothing reads.
///
/// The bytes are parsed twice — once as a [`serde_norway::Value`] for the audit, once into the typed shape —
/// because only the raw form knows which ENTRY a key was written in, and only the typed decode reports a
/// field's type error with the line it is on. Config files are small; the clarity is worth the second pass.
fn decode(data: &[u8]) -> Result<ConfigFile, ConfigError> {
    let doc: serde_norway::Value =
        serde_norway::from_slice(data).map_err(|e| ConfigError::Parse(e.to_string()))?;
    strict::audit(&doc)?;
    serde_norway::from_slice(data).map_err(|e| ConfigError::Parse(e.to_string()))
}

/// `expand` on an owned string, allocating only when a `${…}` was substituted.
fn expand_owned(s: String, env: &Env) -> String {
    match expand(&s, env) {
        std::borrow::Cow::Borrowed(_) => s,
        std::borrow::Cow::Owned(expanded) => expanded,
    }
}

/// Config-level failures that abort a run.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A failure that belongs to ONE file of the stack, prefixed with the file it was written in (the
    /// cross-layer validation at the end of the load has no single file to name and stays unwrapped).
    #[error("config {path}: {source}")]
    File {
        /// The file as it was found.
        path: String,
        /// What is wrong inside it.
        #[source]
        source: Box<ConfigError>,
    },
    /// A key that does not belong where it was written: another layer's, retired, or unknown. `at` is the
    /// config coordinate (`agents.coder.tools.delegate`).
    #[error("{at}: {message}")]
    Key {
        /// The coordinate the key was written at.
        at: String,
        /// Which layer owns it, or what replaced it.
        message: String,
    },
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
