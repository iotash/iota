//! The soft-migration layer: a one-layer `providers.<name>` block — every key under the provider, the shape
//! iota shipped with — is still accepted and split into the three entries it means.
//!
//! The split is mechanical and total, so a pre-split config behaves EXACTLY as it did: the endpoint keys
//! stay, the model-level keys become an implicit `models.<name>`, and the usage keys become an implicit
//! `agents.<name>` whose candidate set is that model (or `<name>:*` when the block never named one, which is
//! how a config without `model:` still opens the picker). Each split block prints ONE deprecation line.
//!
//! This layer is scheduled for removal after 1.0 (`docs/MIGRATION-ROADMAP.md` Phase 1b, step 8).

use crate::config::agent::AgentConfig;
use crate::config::model::{ModelConfig, ModelRef};
use crate::config::provider::ProviderConfig;
use crate::tool::sets::{SKILLS_SET, ToolsConfig};

/// The toolset that used to be called `agent` and is now [`SKILLS_SET`] (the word `agent` named three
/// different things; brain page `config-three-layers`).
pub(crate) const OLD_SKILLS_SET: &str = "agent";

/// One `providers.<name>` entry as a one-layer config writes it: the endpoint keys plus every model- and
/// agent-level key that used to live here. A three-layer config simply leaves the latter empty.
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub(crate) struct LegacyProviderEntry {
    // ---- endpoint ------------------------------------------------------------------------------------
    /// `type:`.
    #[serde(rename = "type")]
    pub(crate) kind: String,
    /// `key:`.
    pub(crate) key: String,
    /// `url:`.
    pub(crate) url: String,

    // ---- model level ---------------------------------------------------------------------------------
    /// `model:` — the default model id; becomes `models.<name>.id`.
    pub(crate) model: String,
    /// `context_window:`.
    pub(crate) context_window: String,
    /// `defer_mode:`.
    pub(crate) defer_mode: String,
    /// `effort:`.
    pub(crate) effort: String,
    /// `temperature:`.
    pub(crate) temperature: Option<f64>,
    /// `top_p:`.
    pub(crate) top_p: Option<f64>,
    /// `image:`.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub(crate) image: bool,
    /// `aspect_ratio:`.
    pub(crate) aspect_ratio: String,
    /// `image_size:`.
    pub(crate) image_size: String,
    /// `negative_prompt:`.
    pub(crate) negative_prompt: String,
    /// `json_edits:`.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub(crate) json_edits: bool,

    // ---- agent level ---------------------------------------------------------------------------------
    /// `system:`.
    pub(crate) system: String,
    /// `system_file:`.
    pub(crate) system_file: String,
    /// `tools:`.
    pub(crate) tools: ToolsConfig,
    /// `mcp_servers:`.
    pub(crate) mcp_servers: Option<Vec<String>>,
    /// `agent:` — now spelled `workspace:` on an agent.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub(crate) agent: bool,
    /// `no_save:`.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub(crate) no_save: bool,
    /// `notify:`.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_opt_bool")]
    pub(crate) notify: Option<bool>,
}

/// What one `providers.<name>` block splits into.
pub(crate) struct Split {
    /// The endpoint entry (always present).
    pub(crate) provider: ProviderConfig,
    /// The implicit `models.<name>`, when the block carried model-level keys.
    pub(crate) model: Option<ModelConfig>,
    /// The implicit `agents.<name>`, when the block carried agent-level keys.
    pub(crate) agent: Option<AgentConfig>,
}

impl LegacyProviderEntry {
    /// Splits the block, naming every migrated key in ONE warning line (`""` when nothing migrated).
    pub(crate) fn split(self, name: &str, warn: &mut dyn FnMut(String)) -> Split {
        let mut moved: Vec<&'static str> = Vec::new();
        // A key "is present" when it carries a value that DOES something: a `false` switch and an empty
        // string change nothing, so they neither migrate nor deserve a deprecation line.
        let mut mark = |present: bool, key: &'static str| {
            if present {
                moved.push(key);
            }
            present
        };
        let model_keys = [
            mark(!self.model.is_empty(), "model"),
            mark(!self.context_window.is_empty(), "context_window"),
            mark(!self.defer_mode.is_empty(), "defer_mode"),
            mark(!self.effort.is_empty(), "effort"),
            mark(self.temperature.is_some(), "temperature"),
            mark(self.top_p.is_some(), "top_p"),
            mark(self.image, "image"),
            mark(!self.aspect_ratio.is_empty(), "aspect_ratio"),
            mark(!self.image_size.is_empty(), "image_size"),
            mark(!self.negative_prompt.is_empty(), "negative_prompt"),
            mark(self.json_edits, "json_edits"),
        ]
        .into_iter()
        .any(|p| p);
        let agent_keys = [
            mark(!self.system.is_empty(), "system"),
            mark(!self.system_file.is_empty(), "system_file"),
            mark(!self.tools.is_empty(), "tools"),
            mark(self.mcp_servers.is_some(), "mcp_servers"),
            mark(self.agent, "agent"),
            mark(self.no_save, "no_save"),
            mark(self.notify.is_some(), "notify"),
        ]
        .into_iter()
        .any(|p| p);

        if !moved.is_empty() {
            warn(format!(
                "Warning: config providers.{name}: {} now belong under `models:` / `agents:` (still accepted; see README)",
                moved.join(", ")
            ));
        }

        let provider = ProviderConfig {
            kind: self.kind,
            key: self.key,
            url: self.url,
        };
        let model = model_keys.then(|| ModelConfig {
            provider: name.to_owned(),
            id: self.model,
            context_window: self.context_window,
            defer_mode: self.defer_mode,
            effort: self.effort,
            temperature: self.temperature,
            top_p: self.top_p,
            image: self.image,
            aspect_ratio: self.aspect_ratio,
            image_size: self.image_size,
            negative_prompt: self.negative_prompt,
            json_edits: self.json_edits,
            migrated: true,
        });
        let agent = agent_keys.then(|| AgentConfig {
            // The candidate set is the implicit model when there is one, and "whatever this provider lists"
            // when the block never named a model — which is exactly the run that opened the picker before.
            models: vec![if model_keys {
                ModelRef::Entry(name.to_owned())
            } else {
                ModelRef::All {
                    provider: name.to_owned(),
                }
            }],
            system: self.system,
            system_file: self.system_file,
            tools: self.tools,
            mcp_servers: self.mcp_servers,
            workspace: self.agent,
            no_save: self.no_save,
            notify: self.notify,
            description: String::new(),
            effort: String::new(),
            temperature: None,
            top_p: None,
            migrated: true,
        });
        Split {
            provider,
            model,
            agent,
        }
    }
}

/// Renames the `agent` toolset key to `skills` in place, warning once and naming `where_` (the config
/// coordinate, e.g. `agents.coder.tools`). An explicit `skills` key already present wins and the old key is
/// dropped.
pub(crate) fn rename_skills_set(
    tools: &mut ToolsConfig,
    where_: &str,
    warn: &mut dyn FnMut(String),
) {
    let Some(node) = tools.remove(OLD_SKILLS_SET) else {
        return;
    };
    warn(format!(
        "Warning: config {where_}: toolset `{OLD_SKILLS_SET}` is now `{SKILLS_SET}` (still accepted; see README)"
    ));
    tools.entry(SKILLS_SET.to_owned()).or_insert(node);
}
