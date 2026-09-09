//! Wiring for the `delegate` toolset (cmd/delegate.go): `tools.delegate` decodes to agents that ARE provider
//! entries, every agent is validated at startup in name order (POLICY F-08), and the returned `ChildFactory`
//! builds a FRESH provider and a FRESH dispatcher on every delegation.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::BoxError;
use crate::app::HostDirs;
use crate::chat::{AgentOptions, ChatDelegator, Child, ChildFactory};
use crate::provider::ProviderParams;
use crate::provider::{HttpTransport, ProviderKind, provider_env_key};
use crate::text::go_float;
use crate::tool::sets::{RawNode, ToolsConfig};
use crate::tool::{AgentInfo, Dispatcher, Env};
use crate::tool::{Registry, yaml11};
use crate::vars::EnvSource;

use crate::cmd::CliError;
use crate::cmd::resolve::check_provider_name;
use crate::config::{Config, ProviderConfig};

/// `tools: delegate:` (delegate.go `delegateConfig`).
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DelegateConfig {
    /// `agents:` — agent name → provider reference.
    pub agents: BTreeMap<String, AgentRef>,
    /// `max_turns:` — the optional per-child cap; `<= 0` means none (delegate.go:74-77 clamped negatives to 0)
    /// once it becomes `Child.max_turns`.
    pub max_turns: i64,
}

/// A provider name, optionally with the one thing the provider entry cannot carry: what the agent is FOR
/// (delegate.go `agentRef`). The bare-string form is the common case.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum AgentRef {
    /// `reviewer: openai` — the provider name alone.
    Name(String),
    /// `reviewer: {provider: openai, description: …}`. `provider` defaults so `review: {description: x}` decodes
    /// (provider `""`) and fails as `agent "review": no provider named`, not as a mapping error.
    Full {
        /// The provider name (`""` when absent).
        #[serde(default)]
        provider: String,
        /// What this agent is for (shown in the `delegate` tool description).
        #[serde(default)]
        description: String,
    },
}

impl AgentRef {
    /// The provider name (`Name(s)` → `s`; `Full` → `provider`).
    pub fn provider(&self) -> &str {
        match self {
            Self::Name(s) => s,
            Self::Full { provider, .. } => provider,
        }
    }

    /// The description (`Name` → `""`; `Full` → `description`).
    pub fn description(&self) -> &str {
        match self {
            Self::Name(_) => "",
            Self::Full { description, .. } => description,
        }
    }
}

/// One validated agent: everything the `ChildFactory` needs to rebuild the child from scratch.
#[derive(Clone)]
struct Resolved {
    /// The resolved provider TYPE string (`Config::get`).
    provider_type: String,
    /// The provider entry the agent IS.
    provider_cfg: ProviderConfig,
    /// The child's toolset (the parent's minus `delegate`).
    tools: ToolsConfig,
}

/// Validates every agent in name order (texts below). The startup build of each child's tools serves ONLY
/// validation (`Toolset` error) and `read_only_registry`; the returned `ChildFactory` calls `build_child_tools`
/// AGAIN on every delegation, exactly as it constructs a fresh provider (delegate.go:137-186) — see
/// `crate::chat::ChildFactory`.
pub fn build_delegator(
    cfg: &Config,
    node: Option<&RawNode>,
    http: impl Into<HttpTransport>,
    root: PathBuf,
    dirs: HostDirs,
    env: Arc<dyn EnvSource>,
) -> Result<Arc<ChatDelegator>, DelegateError> {
    build_delegator_with_factory(cfg, node, http, root, dirs, env).map(|(d, _)| d)
}

/// [`build_delegator`] plus the [`ChildFactory`] it installed. `http` is the run's transport
/// (`reqwest::Client` alone = no `/debug` recorder). The factory is otherwise sealed inside
/// `ChatDelegator`, and `child_dispatcher_is_fresh_per_delegation` has to call it twice to prove that each
/// delegation gets its own `Registry` (and therefore its own read-before-edit ledger).
pub fn build_delegator_with_factory(
    cfg: &Config,
    node: Option<&RawNode>,
    http: impl Into<HttpTransport>,
    root: PathBuf,
    dirs: HostDirs,
    env: Arc<dyn EnvSource>,
) -> Result<(Arc<ChatDelegator>, ChildFactory), DelegateError> {
    // The transport (client + the run's `/debug` recorder) rides into every child: Go's children
    // share the parent's recording transport (delegate.go:148).
    let http: HttpTransport = http.into();
    // delegate.go:65-70: a zero node keeps the zero config, which then fails as `no agents configured`.
    let server_cfg: DelegateConfig = yaml11::decode_mapping(node).map_err(DelegateError::Config)?;
    if server_cfg.agents.is_empty() {
        return Err(DelegateError::NoAgents);
    }
    // delegate.go:74-77.
    let max_turns = crate::chat::turns::turn_cap(server_cfg.max_turns);

    let mut agents: BTreeMap<String, AgentInfo> = BTreeMap::new();
    let mut by_name: BTreeMap<String, Resolved> = BTreeMap::new();

    // A `BTreeMap` visits the agents in NAME order, so the first failing agent is the same on every run
    // (DIVERGENCES F-08; Go's map iteration picked one at random).
    for (name, agent_ref) in &server_cfg.agents {
        let provider_name = agent_ref.provider();
        if provider_name.is_empty() {
            return Err(DelegateError::NoProvider(name.clone()));
        }
        let (provider_type, provider_cfg) = cfg.get(provider_name);
        check_provider_name(cfg, provider_name, &provider_type)
            .map_err(|e| DelegateError::Provider(name.clone(), e))?;
        // A child has no way to be asked which model to use — the interactive picker and `-M` are both out of
        // reach down here, so an empty name would reach the provider as a 400 mid-conversation.
        if provider_cfg.model.is_empty() {
            return Err(DelegateError::NoModel(
                name.clone(),
                provider_name.to_owned(),
            ));
        }
        // The same reasoning covers every value only the API could reject (delegate.go:109-117).
        if provider_cfg.effort().is_err() {
            return Err(DelegateError::Effort(
                name.clone(),
                provider_name.to_owned(),
                provider_cfg.effort.clone(),
            ));
        }
        if let Some(t) = provider_cfg.temperature
            && !(0.0..=2.0).contains(&t)
        {
            return Err(DelegateError::Temperature(
                name.clone(),
                provider_name.to_owned(),
                t,
            ));
        }
        if let Some(p) = provider_cfg.top_p
            && !(0.0..=1.0).contains(&p)
        {
            return Err(DelegateError::TopP(
                name.clone(),
                provider_name.to_owned(),
                p,
            ));
        }

        let tools = child_tools(&provider_cfg.tools);
        // Built here so the parallel decision rests on what the user configured, and so a complaint IS the
        // startup error this function promises (delegate.go:118-131).
        let (registry, warnings) = build_child_tools(&root, &dirs, &tools, provider_cfg.agent);
        if !warnings.is_empty() {
            return Err(DelegateError::Toolset(name.clone(), warnings.join("; ")));
        }
        agents.insert(
            name.clone(),
            AgentInfo {
                description: agent_ref.description().to_owned(),
                read_only: read_only_registry(registry.as_ref()),
            },
        );
        by_name.insert(
            name.clone(),
            Resolved {
                provider_type,
                provider_cfg,
                tools,
            },
        );
    }

    let build: ChildFactory = Arc::new(move |name: &str| -> Result<Child, BoxError> {
        let Some(r) = by_name.get(name) else {
            return Err(Box::new(DelegateError::UnknownAgent(name.to_owned())));
        };
        // delegate.go:141-147: the environment outranks `key:`, and a child with neither cannot run.
        let env_key = provider_env_key(&r.provider_type);
        let key = env
            .var(env_key)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| r.provider_cfg.key.clone());
        if key.is_empty() {
            return Err(Box::new(DelegateError::ApiKey(name.to_owned(), env_key)));
        }
        let named = |e: CliError| DelegateError::Provider(name.to_owned(), e);
        let kind: ProviderKind = r
            .provider_type
            .parse()
            .map_err(|e| named(CliError::UnknownType(e)))?;
        let mut provider = crate::provider::new_provider(
            kind,
            ProviderParams {
                api_key: &key,
                base_url: &r.provider_cfg.url,
                model: &r.provider_cfg.model,
                temperature: r.provider_cfg.temperature,
            },
            Some(http.clone()),
        )
        .map_err(|e| named(CliError::Provider(e)))?;
        if let Ok(effort @ Some(_)) = r.provider_cfg.effort()
            && let Some(tunable) = provider.as_tunable()
        {
            tunable.set_effort(effort);
        }
        if let Some(top_p) = r.provider_cfg.top_p
            && let Some(tunable) = provider.as_top_p_tunable()
        {
            tunable.set_top_p(Some(top_p));
        }
        // Image settings are deliberately NOT applied (delegate.go:162-168): a delegation returns text, and a
        // child that generated an image would write it where the parent never learns of it.
        let system = r
            .provider_cfg
            .resolve_system()
            .map_err(|e| named(CliError::Config(e)))?;
        // The child's OWN toolset, rebuilt exactly as it was validated — its complaints are dropped, because
        // startup already refused anything that would produce one.
        let (dispatch, _) = build_child_tools(&root, &dirs, &r.tools, r.provider_cfg.agent);
        Ok(Child {
            provider,
            dispatch,
            system,
            agent: AgentOptions {
                enabled: r.provider_cfg.agent,
                root: root.clone(),
                cwd: dirs.cwd.clone(),
                home: dirs.home.clone(),
            },
            max_turns,
        })
    });

    Ok((
        Arc::new(ChatDelegator::new(agents, Arc::clone(&build))),
        build,
    ))
}

/// `Env { project_root: Some(root), dirs, delegate: None }`; `Registry::build` collecting warnings; `agent_mode` →
/// `enable_set("agent")`. Returns (dispatcher, warnings).
pub fn build_child_tools(
    root: &Path,
    dirs: &HostDirs,
    tools: &ToolsConfig,
    agent_mode: bool,
) -> (Arc<dyn Dispatcher>, Vec<String>) {
    let mut warnings = Vec::new();
    // No `delegate` seam: a child that could delegate would delegate recursively. No `interact` either — a child
    // is not the conversation the user is in (delegate.go:203).
    let env = Env {
        project_root: Some(root.to_path_buf()),
        dirs: dirs.clone(),
        ..Env::default()
    };
    let mut registry = Registry::build(&env, tools, &mut |w| warnings.push(w));
    if agent_mode {
        // `AgentMode` only injects the AGENTS.md/skills text; `load_skill` comes from the agent SET, so an agent
        // configured `agent: true` needs it enabled here too (delegate.go:205-207).
        registry.enable_set(&env, "agent", &mut |w| warnings.push(w));
    }
    (Arc::new(registry), warnings)
}

/// Copy minus the `"delegate"` key (a child that could delegate would delegate recursively).
pub fn child_tools(raw: &ToolsConfig) -> ToolsConfig {
    raw.iter()
        .filter(|(k, _)| k.as_str() != "delegate")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// true iff every advertised tool answers `supports_parallel(name, None) == true` (empty → true).
pub(crate) fn read_only_registry(d: &dyn Dispatcher) -> bool {
    // The parallel opt-in already means "does not write, needs no approval, opens no surface" — reusing it keeps
    // one definition of harmless instead of two that could disagree (delegate.go:235-246).
    d.tools()
        .iter()
        .all(|def| d.supports_parallel(&def.name, None))
}

/// Why `tools.delegate` refused its configuration at startup (cmd/delegate.go texts); `run` wraps it as
/// `CliError::Delegate` (`tools.delegate: {e}`).
#[derive(Debug, thiserror::Error)]
pub enum DelegateError {
    /// `tools.delegate` is not a mapping (or fails to decode).
    #[error("config must be a mapping (agents, max_turns): {0}")]
    Config(String),
    /// `agents:` is absent or empty.
    #[error("no agents configured (add `agents:` mapping agent names to provider names)")]
    NoAgents,
    /// The agent's reference names no provider.
    #[error("agent {0:?}: no provider named")]
    NoProvider(String),
    /// The agent's provider entry failed (wraps `UnknownProvider` / `new_provider` / `system_file` errors).
    #[error("agent {0:?}: {1}")]
    Provider(String, #[source] CliError),
    /// The agent's provider entry has no `model:`.
    #[error(
        "agent {0:?}: provider {1:?} has no `model:` (a delegated agent cannot be asked to pick one)"
    )]
    NoModel(String, String),
    /// The agent's provider entry has an invalid `effort:`.
    #[error("agent {0:?}: provider {1:?} has effort {2:?}: want low|medium|high|xhigh|max")]
    Effort(String, String, String),
    /// The agent's provider entry has a `temperature:` outside 0.0-2.0.
    #[error("agent {0:?}: provider {1:?} has temperature {t}: want 0.0-2.0", t = go_float(*.2))]
    Temperature(String, String, f64),
    /// The agent's provider entry has a `top_p:` outside 0.0-1.0.
    #[error("agent {0:?}: provider {1:?} has top_p {t}: want 0.0-1.0", t = go_float(*.2))]
    TopP(String, String, f64),
    /// The child's toolset build complained (warnings joined `"; "`).
    #[error("agent {0:?}: {1}")]
    Toolset(String, String),
    /// A delegation named an agent that is not configured.
    #[error("unknown agent {0:?}")]
    UnknownAgent(String),
    /// The child's key is neither in the environment nor in `key:` (raised lazily by the `ChildFactory`).
    #[error("agent {0:?}: API key is required (set {1} or `key:`)")]
    ApiKey(String, &'static str),
}
