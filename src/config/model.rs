//! `models.<name>` — the MODEL layer: which provider serves the model, its wire id, the properties that
//! belong to the model itself (`context_window`) and the protocol its DIALECT decides (`defer_mode`, the
//! image-generation knobs), plus the tunable defaults an agent may override.
//!
//! `defer_mode` lives here and not on an agent because [`DeferMode::supports`] is a statement about a
//! dialect, and the dialect is fixed by the provider a model points at (brain page `config-three-layers`).
//! Needing two protocols for one model means two `models:` entries, not an override further down.

use serde::Deserialize as _;
use serde::de::{MapAccess, Visitor};

use crate::config::ConfigError;
use crate::provider::error::InvalidEffort;
use crate::provider::{Effort, ImageGenParams};
use crate::tool::DeferMode;

/// How a model is named where one is referenced: in `agents.<name>.model` and `agents.<name>.choices`, in a
/// `models:` shorthand and behind `-M`.
///
/// The separator is `:` with the PROVIDER first, and the id is everything after the first colon — so a
/// relay's own `vendor/model` shape survives verbatim (`openrouter:anthropic/claude-3.5-sonnet`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelRef {
    /// `sonnet` — a `models:` entry by name.
    Entry(String),
    /// `anthropic:claude-sonnet-4` — a provider plus an inline model id.
    Inline {
        /// The `providers:` entry (or built-in type) that serves it.
        provider: String,
        /// The wire model id, verbatim.
        id: String,
    },
    /// `anthropic:*` — every model the provider lists (resolved at runtime through `list_models`).
    All {
        /// The `providers:` entry (or built-in type) to enumerate.
        provider: String,
    },
}

impl ModelRef {
    /// The provider this reference names; `None` for a bare entry name (the entry decides).
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Entry(_) => None,
            Self::Inline { provider, .. } | Self::All { provider } => Some(provider),
        }
    }

    /// Whether this reference is the `provider:*` wildcard.
    pub fn is_wildcard(&self) -> bool {
        matches!(self, Self::All { .. })
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entry(name) => f.write_str(name),
            Self::Inline { provider, id } => write!(f, "{provider}:{id}"),
            Self::All { provider } => write!(f, "{provider}:*"),
        }
    }
}

impl std::str::FromStr for ModelRef {
    type Err = BadModelRef;

    /// `split_once(':')`: no colon → an entry name; otherwise the left side is the provider and the WHOLE
    /// right side the model id (`*` alone being the wildcard).
    fn from_str(s: &str) -> Result<Self, BadModelRef> {
        let s = s.trim();
        if s.is_empty() {
            return Err(BadModelRef::Empty);
        }
        let Some((provider, id)) = s.split_once(':') else {
            return Ok(Self::Entry(s.to_owned()));
        };
        let (provider, id) = (provider.trim(), id.trim());
        if provider.is_empty() {
            return Err(BadModelRef::NoProvider(s.to_owned()));
        }
        if id.is_empty() {
            return Err(BadModelRef::NoModel(s.to_owned()));
        }
        if id == "*" {
            return Ok(Self::All {
                provider: provider.to_owned(),
            });
        }
        Ok(Self::Inline {
            provider: provider.to_owned(),
            id: id.to_owned(),
        })
    }
}

/// Why a model reference could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BadModelRef {
    /// The reference is empty (or all whitespace).
    #[error("a model reference must not be empty")]
    Empty,
    /// Nothing before the colon (`:gpt-5`).
    #[error("model reference {0:?}: nothing before the ':' (want \"provider:model\")")]
    NoProvider(String),
    /// Nothing after the colon (`openai:`).
    #[error(
        "model reference {0:?}: nothing after the ':' (want \"provider:model\" or \"provider:*\")"
    )]
    NoModel(String),
    /// A space after the colon made YAML read the item as a mapping — the one mistake this syntax invites.
    #[error(
        "a model reference must be a string like \"provider:model\", but `{0}: {1}` parses as a YAML mapping — remove the space after the colon (`{0}:{1}`)"
    )]
    YamlMapping(String, String),
}

impl<'de> serde::Deserialize<'de> for ModelRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ModelRefVisitor)
    }
}

/// Accepts the string form and turns the `- provider: model` mistake into [`BadModelRef::YamlMapping`]
/// instead of serde's "invalid type: map".
struct ModelRefVisitor;

impl<'de> Visitor<'de> for ModelRefVisitor {
    type Value = ModelRef;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a model reference: \"name\", \"provider:model\" or \"provider:*\"")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ModelRef, E> {
        v.parse().map_err(E::custom)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ModelRef, A::Error> {
        let entry: Option<(String, serde_norway::Value)> = map.next_entry()?;
        let (key, value) = entry.unwrap_or_default();
        // Drain the rest so the deserializer stays consistent even though we are about to fail.
        while map
            .next_entry::<serde_norway::Value, serde_norway::Value>()?
            .is_some()
        {}
        Err(serde::de::Error::custom(BadModelRef::YamlMapping(
            key,
            scalar_text(&value),
        )))
    }
}

/// A YAML scalar as the user typed it (best effort); anything else falls back to its debug shape, which is
/// only ever used inside an error message.
fn scalar_text(v: &serde_norway::Value) -> String {
    match v {
        serde_norway::Value::String(s) => s.clone(),
        serde_norway::Value::Bool(b) => b.to_string(),
        serde_norway::Value::Number(n) => n.to_string(),
        serde_norway::Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

/// One `models.<name>` entry as written: the shorthand `sonnet: anthropic:claude-x` or the full mapping.
#[derive(Debug, Clone)]
pub enum ModelEntry {
    /// `sonnet: anthropic:claude-sonnet-4` — provider and id in one string.
    Shorthand(String),
    /// The full mapping form.
    Full(Box<ModelConfig>),
}

impl ModelEntry {
    /// The entry as a [`ModelConfig`]. The shorthand must name a provider AND a model: a bare name would
    /// have no endpoint, and `provider:*` is a candidate SET, which only an agent can hold.
    pub fn into_config(self, name: &str) -> Result<ModelConfig, ConfigError> {
        match self {
            Self::Full(cfg) => Ok(*cfg),
            Self::Shorthand(s) => match s.parse::<ModelRef>() {
                Err(e) => Err(ConfigError::Model(name.to_owned(), e.to_string())),
                Ok(ModelRef::Inline { provider, id }) => Ok(ModelConfig {
                    provider,
                    id,
                    ..ModelConfig::default()
                }),
                Ok(ModelRef::Entry(_)) => Err(ConfigError::Model(
                    name.to_owned(),
                    format!("{s:?} names no provider (want \"provider:model\")"),
                )),
                Ok(ModelRef::All { .. }) => Err(ConfigError::Model(
                    name.to_owned(),
                    format!(
                        "{s:?} is a candidate set, not a model (use it in `agents.<name>.choices`)"
                    ),
                )),
            },
        }
    }
}

impl<'de> serde::Deserialize<'de> for ModelEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ModelEntryVisitor)
    }
}

/// String → shorthand, mapping → the full form. Written by hand so a bad mapping reports the FIELD that is
/// wrong rather than serde's untagged "data did not match any variant".
struct ModelEntryVisitor;

impl<'de> Visitor<'de> for ModelEntryVisitor {
    type Value = ModelEntry;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"provider:model\" or a mapping (provider, id, …)")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ModelEntry, E> {
        Ok(ModelEntry::Shorthand(v.to_owned()))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ModelEntry, A::Error> {
        ModelConfig::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            .map(|c| ModelEntry::Full(Box::new(c)))
    }
}

/// One configured model: the provider that serves it, its wire id, and everything that is a property of the
/// MODEL rather than of the endpoint or of a particular use.
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ModelConfig {
    /// `provider:` — the `providers:` entry (or built-in type) that serves it; `""` = the entry's own name.
    pub provider: String,
    /// `id:` — the wire model id, verbatim. `""` leaves the model unchosen (the picker opens).
    pub id: String,
    /// `context_window:` — the budget compaction accounts against (`window::parse_window_size`).
    pub context_window: String,
    /// `defer_mode:` — `normal` (default) | `reference` | `tool-search` | `system-tools`. Validated against
    /// the provider's dialect by [`crate::config::Config::load`]; a mismatch is a config ERROR.
    pub defer_mode: String,
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
    /// `json_edits:` — JSON-body image edits (`images` type only).
    #[serde(deserialize_with = "crate::tool::yaml11::deserialize_bool")]
    pub json_edits: bool,
}

impl ModelConfig {
    /// The provider this model names: `provider:`, or `name` when the key is absent (so a `models:` entry
    /// named after its provider needs no `provider:` line).
    pub fn provider_or<'a>(&'a self, name: &'a str) -> &'a str {
        if self.provider.is_empty() {
            name
        } else {
            &self.provider
        }
    }

    /// Pins `provider:` to the entry's own name when the key was absent, so the entry keeps naming its
    /// endpoint once it is copied out of the map.
    pub fn anchor_provider(&mut self, name: &str) {
        if self.provider.is_empty() {
            self.provider.push_str(name);
        }
    }

    /// `effort:` as a level; `Ok(None)` when the key is unset.
    pub fn effort(&self) -> Result<Option<Effort>, InvalidEffort> {
        Effort::optional(&self.effort)
    }

    /// `aspect_ratio:` / `image_size:` / `negative_prompt:`; an empty value is unset.
    pub fn image_gen_params(&self) -> ImageGenParams {
        ImageGenParams::from_raw(&self.aspect_ratio, &self.image_size, &self.negative_prompt)
    }

    /// The defer mode this model mounts deferred groups with. `""` and — after `Config::load` has had its
    /// say — an unknown spelling both mean [`DeferMode::DEFAULT`].
    pub fn defer_mode(&self) -> DeferMode {
        DeferMode::from_name(&self.defer_mode).unwrap_or(DeferMode::DEFAULT)
    }
}
