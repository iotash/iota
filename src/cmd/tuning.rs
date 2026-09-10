//! Provider tuning from the config (cmd/root.go:133-199): image → effort → `top_p` → temperature → `json_edits` →
//! generation params, each warning byte-equal to Go, then the `tools`/`mcp_servers` warning for providers without tool
//! calling.
//!
//! The values come from the resolved MODEL (an image knob or a reasoning effort is a property of the model,
//! not of the endpoint), with the agent's overrides already folded in by [`Resolved::effort`] and friends.
//!
//! Warning texts (root.go):
//! `` Warning: `image: true` is redundant for provider type {k} (it always generates images) `` ·
//! `` Warning: `effort` does not apply to provider type {k} (ignored) `` ·
//! `` Warning: `top_p` does not apply to provider type {k} (ignored) `` ·
//! `Warning: temperature does not apply to provider type {k} (ignored)` ·
//! `` Warning: `json_edits` applies only to the images provider type (ignored for {k}) `` ·
//! `Warning: aspect_ratio/image_size/negative_prompt apply only to image providers (ignored for type {k})`.

use crate::provider::Provider;

use crate::cmd::CliError;
use crate::config::Resolved;

/// Fixed order: image → effort → `top_p` → temperature → `json_edits` → gen-params. Warnings carry the `Warning: `
/// prefix. Errors: `ConfigEffort`, `ConfigTopP`.
///
/// Every step is Go's `p.(provider.XTunable)` type assertion spelled as the capability accessor: the tuning lands
/// when the provider has the capability and warns (naming `p.kind()`, Go's `p.Type()`) when it does not. The
/// temperature was already given to `new_provider`, so this step only warns about a provider that cannot use it.
pub(crate) fn apply(
    p: &mut dyn Provider,
    resolved: &Resolved,
    temperature: Option<f64>,
    warn: &mut dyn FnMut(String),
) -> Result<(), CliError> {
    let kind = p.kind();
    let model_cfg = &resolved.model;

    // root.go:133-139
    if model_cfg.image {
        if let Some(tunable) = p.as_image_tunable() {
            tunable.set_image_output(true);
        } else if p.as_image_gen_tunable().is_some() {
            warn(format!(
                "Warning: `image: true` is redundant for provider type {kind} (it always generates images)"
            ));
        }
    }

    // root.go:140-149
    if let effort @ Some(_) = crate::provider::Effort::optional(resolved.effort())
        .map_err(|_| CliError::ConfigEffort(resolved.effort().to_owned()))?
    {
        if let Some(tunable) = p.as_tunable() {
            tunable.set_effort(effort);
        } else {
            warn(format!(
                "Warning: `effort` does not apply to provider type {kind} (ignored)"
            ));
        }
    }

    // root.go:150-159
    if let Some(top_p) = resolved.top_p() {
        if !(0.0..=1.0).contains(&top_p) {
            return Err(CliError::ConfigTopP(top_p));
        }
        if let Some(tunable) = p.as_top_p_tunable() {
            tunable.set_top_p(Some(top_p));
        } else {
            warn(format!(
                "Warning: `top_p` does not apply to provider type {kind} (ignored)"
            ));
        }
    }

    // root.go:160-164: the value already reached the constructor; only the warning is left.
    if temperature.is_some() && p.as_tunable().is_none() {
        warn(format!(
            "Warning: temperature does not apply to provider type {kind} (ignored)"
        ));
    }

    // root.go:165-171
    if model_cfg.json_edits {
        if let Some(tunable) = p.as_image_edit_json_tunable() {
            tunable.set_json_edits(true);
        } else {
            warn(format!(
                "Warning: `json_edits` applies only to the images provider type (ignored for {kind})"
            ));
        }
    }

    // root.go:172-181
    let gen_params = model_cfg.image_gen_params();
    if !gen_params.is_empty() {
        if let Some(tunable) = p.as_image_gen_tunable() {
            tunable.set_image_gen_params(gen_params);
        } else {
            warn(format!(
                "Warning: aspect_ratio/image_size/negative_prompt apply only to image providers (ignored for type {kind})"
            ));
        }
    }

    Ok(())
}

/// `Warning: tools/mcp_servers do not apply to provider type {kind} (no tool calling)` when !`as_tool_provider` &&
/// (tools non-empty || mcp configs non-empty).
pub(crate) fn warn_tools_without_calling(
    p: &dyn Provider,
    agent_cfg: &crate::config::AgentConfig,
    mcp_count: usize,
    warn: &mut dyn FnMut(String),
) {
    // root.go:196-199: explicitly configured tools that can never be called are worth one line, not a dead list.
    if p.as_tool_provider().is_none() && (!agent_cfg.tools.is_empty() || mcp_count > 0) {
        warn(format!(
            "Warning: tools/mcp_servers do not apply to provider type {} (no tool calling)",
            p.kind()
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::BoxFuture;
    use crate::provider::error::ProviderError;
    use crate::provider::model::Message;
    use crate::provider::{ChatResult, Provider, ProviderKind};
    use crate::testing::FakeToolProvider;
    use tokio_util::sync::CancellationToken;

    use super::{apply, warn_tools_without_calling};
    use crate::cmd::CliError;
    use crate::config::{AgentConfig, ModelConfig, Resolved};

    /// A provider with NO optional capability at all (Go: a type that satisfies none of the tuning interfaces).
    struct PlainProvider(ProviderKind);

    impl Provider for PlainProvider {
        fn kind(&self) -> ProviderKind {
            self.0
        }

        fn model(&self) -> &'static str {
            "m"
        }

        fn set_model(&mut self, _model: String) {}

        fn list_models<'a>(
            &'a self,
            _cancel: &'a CancellationToken,
        ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn chat<'a>(
            &'a self,
            _cancel: &'a CancellationToken,
            _messages: &'a [Message],
        ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
            Box::pin(async { Ok(ChatResult::default()) })
        }
    }

    /// A resolution carrying just this model.
    fn resolved(model: ModelConfig) -> Resolved {
        Resolved {
            model,
            ..Resolved::default()
        }
    }

    /// Collects the warnings `apply` emitted, in order.
    fn run(model: &ModelConfig, temperature: Option<f64>) -> (Result<(), CliError>, Vec<String>) {
        let mut p = PlainProvider(ProviderKind::Imagen);
        let mut warnings = Vec::new();
        let r = apply(&mut p, &resolved(model.clone()), temperature, &mut |w| {
            warnings.push(w);
        });
        (r, warnings)
    }

    #[test]
    fn config_effort_and_top_p_ranges_are_errors() {
        let bad_effort = ModelConfig {
            effort: "turbo".to_owned(),
            ..ModelConfig::default()
        };
        let (r, warnings) = run(&bad_effort, None);
        assert_eq!(
            r.expect_err("effort must be rejected").to_string(),
            "config effort \"turbo\": want low|medium|high|xhigh|max"
        );
        assert!(warnings.is_empty(), "the error precedes every warning");

        let bad_top_p = ModelConfig {
            top_p: Some(2.0),
            ..ModelConfig::default()
        };
        let (r, _) = run(&bad_top_p, None);
        assert_eq!(
            r.expect_err("top_p must be rejected").to_string(),
            "config top_p 2: want 0.0-1.0"
        );
    }

    /// A provider with no tuning capability warns about every knob, in root.go's order. (`image: true` is silent
    /// here: root.go:135-138 warns only for an `ImageGenTunable` provider — `cli_tuning_warnings_order` pins that
    /// half end-to-end against the real imagen provider.)
    #[test]
    fn warnings_follow_the_go_order() {
        let model_cfg = ModelConfig {
            image: true,
            effort: "high".to_owned(),
            top_p: Some(0.5),
            json_edits: true,
            aspect_ratio: "1:1".to_owned(),
            ..ModelConfig::default()
        };
        let (r, warnings) = run(&model_cfg, Some(0.7));
        assert!(r.is_ok());
        assert_eq!(
            warnings,
            vec![
                "Warning: `effort` does not apply to provider type imagen (ignored)".to_owned(),
                "Warning: `top_p` does not apply to provider type imagen (ignored)".to_owned(),
                "Warning: temperature does not apply to provider type imagen (ignored)".to_owned(),
                "Warning: `json_edits` applies only to the images provider type (ignored for imagen)"
                    .to_owned(),
                "Warning: aspect_ratio/image_size/negative_prompt apply only to image providers (ignored for type imagen)"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn tools_warning_needs_a_provider_without_tool_calling() {
        let mut agent_cfg = AgentConfig::default();
        agent_cfg
            .tools
            .insert("code".to_owned(), serde_norway::Value::Null);

        // A tool-calling provider says nothing.
        let tool_provider = FakeToolProvider::scripted(Vec::new(), "done");
        let mut warnings = Vec::new();
        warn_tools_without_calling(&tool_provider, &agent_cfg, 0, &mut |w| warnings.push(w));
        assert!(warnings.is_empty());

        // One without tool calling warns once, whether the tools came from `tools:` or from MCP.
        let plain = PlainProvider(ProviderKind::Imagen);
        let mut warnings = Vec::new();
        warn_tools_without_calling(&plain, &agent_cfg, 0, &mut |w| warnings.push(w));
        assert_eq!(
            warnings,
            vec![
                "Warning: tools/mcp_servers do not apply to provider type imagen (no tool calling)"
                    .to_owned()
            ]
        );

        let mut warnings = Vec::new();
        warn_tools_without_calling(&plain, &AgentConfig::default(), 1, &mut |w| {
            warnings.push(w);
        });
        assert_eq!(warnings.len(), 1, "an MCP server alone is enough");

        let mut warnings = Vec::new();
        warn_tools_without_calling(&plain, &AgentConfig::default(), 0, &mut |w| {
            warnings.push(w);
        });
        assert!(warnings.is_empty(), "nothing configured, nothing to say");
    }
}
