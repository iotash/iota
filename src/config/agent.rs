//! `agents.<name>` — the USAGE layer: which models this agent may drive, the prompt it drives them with,
//! the tools and MCP servers it loads, and the session-shaped switches (`workspace`, `no_save`, `notify`).
//!
//! An agent may override the tunables a model sets as defaults (`effort`/`temperature`/`top_p`) — ONE level
//! of inheritance, deliberately not a chain (brain page `config-three-layers`).

use serde::de::{SeqAccess, Visitor};

use crate::config::ConfigError;
use crate::config::model::ModelRef;
use crate::tool::sets::ToolsConfig;

/// One `agents.<name>` entry.
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    /// `models:` — the candidate set, best first. The FIRST entry is the default model; when it is a
    /// `provider:*` wildcard the run starts in the model picker instead.
    #[serde(deserialize_with = "one_or_many")]
    pub models: Vec<ModelRef>,
    /// `system:` — an inline system prompt (wins over `system_file`).
    pub system: String,
    /// `system_file:` — a file holding the system prompt (`${var}` expanded once at merge time).
    pub system_file: String,
    /// `tools:` — RAW set values; key presence enables the set, the set's factory decodes its value.
    pub tools: ToolsConfig,
    /// `mcp_servers:` — which top-level servers this agent loads: `None` (key absent) = all, `Some([])` =
    /// none, names = that subset (an unknown name is [`ConfigError::UnknownMcpServer`]).
    pub mcp_servers: Option<Vec<String>>,
    /// `workspace:` — the project overlay and the workspace tools (what `--agent` switches on; the key was
    /// spelled `agent:` in the one-layer config).
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub workspace: bool,
    /// `no_save:` — start ephemeral, as `--no-save` does.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub no_save: bool,
    /// `notify:` — the attention ping sent while the terminal is UNFOCUSED. A THREE-state switch: absent
    /// (`None`) means ON, so only an explicit `notify: false` silences it.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_opt_bool")]
    pub notify: Option<bool>,
    /// `description:` — what this agent is FOR. Documentation of the entry: nothing in the binary reads it
    /// since the toolset that put it in front of the model was retired.
    pub description: String,
    /// `effort:` — overrides the model's default.
    pub effort: String,
    /// `temperature:` — overrides the model's default.
    pub temperature: Option<f64>,
    /// `top_p:` — overrides the model's default.
    pub top_p: Option<f64>,
    /// Set when the entry was synthesised from a one-layer `providers.<name>` block, never from YAML.
    #[serde(skip)]
    pub migrated: bool,
}

impl AgentConfig {
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

/// `models: sonnet` and `models: [sonnet, "openai:*"]` both decode; a single reference is the common case
/// and writing it as a one-item list is noise.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ModelRef>, D::Error> {
    d.deserialize_any(ModelListVisitor)
}

/// The visitor behind [`one_or_many`].
struct ModelListVisitor;

impl<'de> Visitor<'de> for ModelListVisitor {
    type Value = Vec<ModelRef>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a model reference or a list of them")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Vec<ModelRef>, E> {
        Ok(Vec::new())
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Vec<ModelRef>, E> {
        v.parse().map(|r| vec![r]).map_err(E::custom)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<ModelRef>, A::Error> {
        let mut out = Vec::new();
        while let Some(r) = seq.next_element::<ModelRef>()? {
            out.push(r);
        }
        Ok(out)
    }
}
