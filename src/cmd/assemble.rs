//! MCP config + dispatcher assembly (cmd/root.go:574-658). The MCP half arrives as a [`McpPart`] — the connected
//! manager as a dispatcher plus its wire-prefix oracle — or `None` when no server is configured.

use std::sync::Arc;

use crate::mcp::config::{ServerConfig, parse_mcp_flag};
use crate::tool::{DeferredGroup, Registry, merge};
use crate::tool::{Dispatcher, PrefixOf, ToolEnv};

use crate::cmd::CliError;

/// The MCP half of the dispatcher: the manager as a tool dispatcher plus its wire-prefix oracle.
pub(crate) struct McpPart {
    /// The manager, dispatching `mcp__…` calls.
    pub(crate) dispatch: Arc<dyn Dispatcher>,
    /// Resolves a server's wire-name prefix lazily (segments are assigned at connect time).
    pub(crate) prefix_of: PrefixOf,
}

impl McpPart {
    /// The part for a connected manager.
    pub(crate) fn of(manager: &Arc<crate::mcp::Manager>) -> Self {
        Self {
            dispatch: Arc::clone(manager) as Arc<dyn Dispatcher>,
            prefix_of: manager.prefix_of(),
        }
    }
}
use crate::config::{AgentConfig, Config, ModelConfig};

/// Uses `crate::mcp::config` only. Config servers (`BTreeMap` order = sorted by name) then `--mcp`
/// flags in order (`parse_mcp_flag` errors abort: `McpFlagError::EmptyFlag` → `CliError::McpFlag`). Deferred
/// groups sorted by name.
/// Blank defer → `Warning: mcp server {name}: defer needs a one-line summary of the server's tools (not deferred)`.
pub(crate) fn build_mcp_configs(
    cfg: &Config,
    agent_cfg: &AgentConfig,
    mcp_flags: &[String],
    warn: &mut dyn FnMut(String),
) -> Result<(Vec<ServerConfig>, Vec<DeferredGroup>), CliError> {
    // root.go:616-621: the agent's `mcp_servers:` selects the config-file subset; an unknown name aborts.
    let selected = cfg.mcp_servers_for(agent_cfg)?;
    let mut configs = Vec::with_capacity(selected.len() + mcp_flags.len());
    let mut defers = Vec::new();

    for (name, server_cfg) in &selected {
        configs.push(ServerConfig {
            name: name.clone(),
            command: server_cfg.command.clone(),
            args: server_cfg.args.clone(),
            url: server_cfg.url.clone(),
            env: server_cfg.env.clone(),
            headers: server_cfg.headers.clone(),
        });
        // `defer:` opts the server into deferred loading; its VALUE is the group summary the search manifest
        // shows. A blank value defeats the point (the summary IS the retrieval corpus) — warn loudly and
        // advertise the server fully instead (root.go:632-642).
        if let Some(raw) = &server_cfg.defer {
            let summary = raw.trim();
            if summary.is_empty() {
                warn(format!(
                    "Warning: mcp server {name}: defer needs a one-line summary of the server's tools (not deferred)"
                ));
            } else {
                defers.push(DeferredGroup {
                    name: name.clone(),
                    summary: summary.to_owned(),
                });
            }
        }
    }
    // root.go:646: deterministic manifest order. `selected` is a `BTreeMap`, so this is already the order the
    // groups were collected in — kept because the manifest's description truncation must not shuffle (F-07).
    defers.sort_by(|a, b| a.name.cmp(&b.name));

    // root.go:649-651: an explicit `--mcp` flag outranks the config and always loads, in flag order.
    for flag in mcp_flags {
        configs.push(parse_mcp_flag(flag)?);
    }

    Ok((configs, defers))
}

/// The MCP part arrives as a [`McpPart`] (`None` when no server is configured).
/// `Registry::build` → `enable_set("skills")` in agent mode → `enable_set("ask")` when `env.interactor` is set
/// (interactive runs only) → parts = [registry if non-empty] + [defer wrapper |
/// mcp dispatcher] → merge. Warn sink receives messages WITHOUT prefix; the caller prints `⚠ {msg}`.
/// `` defer_mode has no effect without a deferred mcp server (add `defer: "<summary>"` to one) ``.
pub(crate) fn build_dispatcher(
    agent_cfg: &AgentConfig,
    model_cfg: &ModelConfig,
    mcp: Option<McpPart>,
    defers: Vec<DeferredGroup>,
    agent_mode: bool,
    env: &ToolEnv,
    warn: &mut dyn FnMut(String),
) -> Arc<dyn Dispatcher> {
    // root.go:578-587. The built-ins are the first part, so they win any tool-name collision with MCP.
    let mut registry = Registry::build(env, &agent_cfg.tools, warn);
    if agent_mode {
        // Skills are activated through the `skills` set's `load_skill`; a config entry may still declare it.
        registry.enable_set(env, crate::tool::sets::SKILLS_SET, warn);
    }
    // root.go:588-592: the ask set is interactive-only (it needs `env.Interact`), so a headless run never
    // enables it and the model never sees a tool it cannot use. Enabled HERE so `choose`/`confirm` keep Go's
    // advertised position among the built-ins.
    if env.interactor.is_some() && !crate::tool::set_disabled(&agent_cfg.tools, "ask") {
        registry.enable_set(env, "ask", warn);
    }

    let mut parts: Vec<Arc<dyn Dispatcher>> = Vec::new();
    if !registry.is_empty() {
        parts.push(Arc::new(registry));
    }
    if let Some(McpPart {
        dispatch,
        prefix_of,
    }) = mcp
    {
        if defers.is_empty() {
            if !model_cfg.defer_mode.is_empty() {
                warn(
                    "defer_mode has no effect without a deferred mcp server (add `defer: \"<summary>\"` to one)"
                        .to_owned(),
                );
            }
            parts.push(dispatch);
        } else {
            // Wire-name prefixes resolve lazily: segments are assigned at connect time, so the deferring
            // wrapper re-asks per snapshot (root.go:600-609). Whether the mode APPLIES to this dialect was
            // settled in `Config::load` (an unusable protocol is a config error, not a silent downgrade), so
            // there is nothing left to decide here.
            parts.push(model_cfg.defer_mode().wrap(dispatch, defers, prefix_of));
        }
    }
    merge(parts)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use crate::tool::DeferredGroup;
    use crate::tool::{Dispatcher, PrefixOf, ToolEnv};

    use super::McpPart;

    use super::{build_dispatcher, build_mcp_configs};
    use crate::config::{AgentConfig, Config, McpServerConfig, ModelConfig};

    /// What `build_mcp_configs` returns.
    type Configs =
        Result<(Vec<crate::mcp::config::ServerConfig>, Vec<DeferredGroup>), crate::cmd::CliError>;

    fn collect(cfg: &Config, agent_cfg: &AgentConfig, flags: &[String]) -> (Configs, Vec<String>) {
        let mut warnings = Vec::new();
        let r = build_mcp_configs(cfg, agent_cfg, flags, &mut |w| warnings.push(w));
        (r, warnings)
    }

    fn config() -> Config {
        Config {
            mcp_servers: BTreeMap::from([
                (
                    "zeta".to_owned(),
                    McpServerConfig {
                        command: "zeta-bin".to_owned(),
                        defer: Some("  zeta tools  ".to_owned()),
                        ..McpServerConfig::default()
                    },
                ),
                (
                    "alpha".to_owned(),
                    McpServerConfig {
                        url: "https://alpha.example/mcp".to_owned(),
                        defer: Some("   ".to_owned()),
                        ..McpServerConfig::default()
                    },
                ),
            ]),
            ..Config::default()
        }
    }

    #[test]
    fn config_servers_are_sorted_then_flags_in_order() {
        let cfg = config();
        let (r, warnings) = collect(
            &cfg,
            &AgentConfig::default(),
            &["srv-bin --root /tmp".to_owned(), "https://x/mcp".to_owned()],
        );
        let (configs, defers) = r.expect("configs");
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta", "srv-bin", "https://x/mcp"]);
        assert_eq!(configs[3].url, "https://x/mcp");
        assert_eq!(configs[2].args, ["--root", "/tmp"]);
        // Only the non-blank `defer:` becomes a group, and its summary is trimmed.
        assert_eq!(
            defers,
            vec![DeferredGroup {
                name: "zeta".to_owned(),
                summary: "zeta tools".to_owned(),
            }]
        );
        assert_eq!(
            warnings,
            vec![
                "Warning: mcp server alpha: defer needs a one-line summary of the server's tools (not deferred)"
                    .to_owned()
            ]
        );
    }

    /// POLICY F-02 in the one place that is compiled in EVERY feature set — `assemble.rs` names no `iota_mcp`
    /// type, so `--mcp ""` fails identically in a build without the `mcp` feature (`cli_mcp_flag_empty` pins the
    /// end-to-end half).
    #[test]
    fn mcp_flag_empty_is_feature_independent() {
        let (r, _) = collect(
            &Config::default(),
            &AgentConfig::default(),
            &["  ".to_owned()],
        );
        assert_eq!(
            r.expect_err("empty flag must fail").to_string(),
            "--mcp: empty server specification"
        );
    }

    #[test]
    fn unknown_mcp_server_selection_aborts() {
        let agent_cfg = AgentConfig {
            mcp_servers: Some(vec!["nope".to_owned()]),
            ..AgentConfig::default()
        };
        let (r, _) = collect(&config(), &agent_cfg, &[]);
        assert_eq!(
            r.expect_err("unknown selection must fail").to_string(),
            "mcp_servers: \"nope\" is not defined under the top-level mcp_servers"
        );
    }

    /// A dispatcher part standing in for the manager (`lib.rs::run` passes the real one).
    #[derive(Default)]
    struct FakeMcpPart;

    impl Dispatcher for FakeMcpPart {
        fn tools(&self) -> Vec<crate::provider::model::ToolDef> {
            Vec::new()
        }

        fn call_tool<'a>(
            &'a self,
            _cx: &'a crate::tool::context::RunCtx,
            name: &'a str,
            _args: crate::provider::model::JsonObject,
        ) -> crate::BoxFuture<'a, crate::tool::ToolResult> {
            Box::pin(
                async move { Err(crate::tool::error::ToolError::UnknownTool(name.to_owned())) },
            )
        }
    }

    /// The part `run` hands over for a connected manager.
    fn mcp_part() -> McpPart {
        let prefix_of: PrefixOf = Arc::new(|_: &str| String::new());
        McpPart {
            dispatch: Arc::new(FakeMcpPart) as Arc<dyn Dispatcher>,
            prefix_of,
        }
    }

    #[test]
    fn defer_mode_without_a_deferred_server_warns_once() {
        let model_cfg = ModelConfig {
            defer_mode: "reference".to_owned(),
            ..ModelConfig::default()
        };
        let mut warnings = Vec::new();
        let d = build_dispatcher(
            &AgentConfig::default(),
            &model_cfg,
            Some(mcp_part()),
            Vec::new(),
            false,
            &ToolEnv::default(),
            &mut |w| warnings.push(w),
        );
        assert!(d.tools().is_empty());
        assert_eq!(
            warnings,
            vec![
                "defer_mode has no effect without a deferred mcp server (add `defer: \"<summary>\"` to one)"
                    .to_owned()
            ]
        );

        // With no MCP part at all the warning is not raised (Go's `mgr != nil` guard, root.go:592-611).
        let mut warnings = Vec::new();
        build_dispatcher(
            &AgentConfig::default(),
            &model_cfg,
            None,
            Vec::new(),
            false,
            &ToolEnv::default(),
            &mut |w| warnings.push(w),
        );
        assert!(warnings.is_empty());
    }

    /// The mode is taken as written, with no capability check and no downgrade: `Config::load` refused the
    /// mismatch already, so a dispatcher build has nothing left to warn about.
    #[test]
    fn defer_mode_wraps_without_a_second_capability_check() {
        let model_cfg = ModelConfig {
            defer_mode: "reference".to_owned(),
            ..ModelConfig::default()
        };
        let groups = vec![DeferredGroup {
            name: "srv".to_owned(),
            summary: "things".to_owned(),
        }];
        let mut warnings = Vec::new();
        build_dispatcher(
            &AgentConfig::default(),
            &model_cfg,
            Some(mcp_part()),
            groups,
            false,
            &ToolEnv::default(),
            &mut |w| warnings.push(w),
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    /// Unknown toolset keys are warnings, never aborts, and the ask set is never enabled headlessly.
    #[test]
    fn registry_warnings_reach_the_caution_sink() {
        let mut agent_cfg = AgentConfig::default();
        agent_cfg
            .tools
            .insert("nosuchset".to_owned(), serde_norway::Value::Null);
        let mut warnings = Vec::new();
        let d = build_dispatcher(
            &agent_cfg,
            &ModelConfig::default(),
            None,
            Vec::new(),
            false,
            &ToolEnv::default(),
            &mut |w| warnings.push(w),
        );
        assert_eq!(
            warnings,
            vec!["unknown toolset \"nosuchset\" (ignored)".to_owned()]
        );
        assert!(d.tools().is_empty());
    }
}
