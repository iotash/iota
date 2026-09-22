//! `agents.<name>` — the USAGE layer: the model this agent starts on and the ones it may switch to, the
//! prompt it drives them with, the tools and MCP servers it loads, and the session-shaped switches
//! (`workspace`, `no_save`, `notify`).
//!
//! An agent may override the four parameters a model sets as defaults (`context_window`/`effort`/
//! `temperature`/`top_p`) — ONE level of inheritance, deliberately not a chain (brain page
//! `config-three-layers`), and the layered evaluation that re-derives them on a `/model` switch lives in
//! [`crate::config::params`].

use serde::de::{SeqAccess, Visitor};

use crate::config::ConfigError;
use crate::config::model::ModelRef;
use crate::tool::sets::ToolsConfig;

/// One `agents.<name>` entry.
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    /// `model:` — the model a run starts on: a `models:` entry by name, or an inline `provider:id`. Never a
    /// `provider:*` wildcard ([`Config::validate`](crate::config::Config) refuses one: a wildcard is a set
    /// to pick from, not a model). Unset = the run starts in the picker, over `choices`.
    pub model: Option<ModelRef>,
    /// `choices:` — what `/model` and `-M` pick from, in declaration order: entry names, inline
    /// `provider:id`s and `provider:*` wildcards. Unset = every top-level `models:` entry, in declaration
    /// order — what the config declares, never an implied wildcard
    /// ([`Config::choices_of`](crate::config::Config::choices_of)); the `Resolved` a run carries has that
    /// default applied. The set is advice, not a whitelist: a `model:` or a `-M` outside it is a warning.
    #[serde(deserialize_with = "one_or_many")]
    pub choices: Vec<ModelRef>,
    /// `system:` — an inline system prompt (wins over `system_file`).
    pub system: String,
    /// `system_file:` — a file holding the system prompt (`${var}` expanded once at merge time).
    pub system_file: String,
    /// `tools:` — RAW set values; key presence enables the set, the set's factory decodes its value.
    pub tools: ToolsConfig,
    /// `mcp_servers:` — which top-level servers this agent loads: `None` (key absent) = all, `Some([])` =
    /// none, names = that subset (an unknown name is [`ConfigError::UnknownMcpServer`]).
    pub mcp_servers: Option<Vec<String>>,
    /// `workspace:` — the project overlay (layered `AGENTS.md`) and the skills toolset. It is the ONLY way
    /// in: the `--agent` flag that used to switch it on per run was a second name for a config decision
    /// (brain page `cli-surface-agent-first`).
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub workspace: bool,
    /// `no_save:` — start ephemeral, as `--no-save` does.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub no_save: bool,
    /// `notify:` — the attention ping sent while the terminal is UNFOCUSED. A THREE-state switch: absent
    /// (`None`) means ON, so only an explicit `notify: false` silences it.
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_opt_bool")]
    pub notify: Option<bool>,
    /// `description:` — what this agent is FOR. Documentation of the entry, printed beside its name by
    /// `iota list agents`; nothing puts it in front of the model since the delegate toolset was retired.
    pub description: String,
    /// `context_window:` — overrides the model's own (same spelling, same
    /// [`parse_window_size`](crate::config::window::parse_window_size)). The window is a property of the
    /// MODEL, so this key is the exception the rule allows: an agent that knows it keeps a long
    /// conversation — or one that must not — says so once instead of forking a `models:` entry per usage
    /// (brain page `model-param-layering`).
    pub context_window: String,
    /// `effort:` — overrides the model's default.
    pub effort: String,
    /// `temperature:` — overrides the model's default.
    pub temperature: Option<f64>,
    /// `top_p:` — overrides the model's default.
    pub top_p: Option<f64>,
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

/// `choices: sonnet` and `choices: [sonnet, "openai:*"]` both decode; a single reference is the common case
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
