//! `providers.<name>` — the ENDPOINT layer: how to reach an API and how to authenticate, and nothing else.
//!
//! Everything that describes how to USE a model moved out in the three-layer split (brain page
//! `config-three-layers`): a model's identity and its protocol bindings live under `models:`, the way an
//! agent drives it under `agents:`. A key of either layer written here is refused by `crate::config::strict`,
//! which names the layer that owns it.

use crate::app::env::Env;
use crate::provider::provider_env_key;

/// One `providers.<name>` entry. Every field defaults; the three together are the whole endpoint.
#[derive(serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ProviderConfig {
    /// `type:` — the built-in provider type behind an alias (`""` = the entry's own name is the type).
    #[serde(rename = "type")]
    pub kind: String,
    /// `key:` — the API key (`${var}` expanded once at merge time).
    pub key: String,
    /// `url:` — the base URL (`${var}` expanded once at merge time).
    pub url: String,
}

impl ProviderConfig {
    /// The provider TYPE this entry names: `type:`, or `name` when the key is absent.
    pub fn kind_or<'a>(&'a self, name: &'a str) -> &'a str {
        if self.kind.is_empty() {
            name
        } else {
            &self.kind
        }
    }
}

/// A provider name resolved to its endpoint: the `providers:` entry (a default one for a name no entry
/// declares) and the TYPE string behind it (`type:`, else the name itself). Borrowed from the config;
/// [`Config::provider`](crate::config::Config::provider) is the one place that resolves a name, and
/// [`Resolved::endpoint`](crate::config::Resolved::endpoint) hands a run's own back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint<'a> {
    /// The provider TYPE (what `ProviderKind::from_str` reads).
    pub kind: &'a str,
    /// The entry: `key:` and `url:`.
    pub config: &'a ProviderConfig,
}

impl Endpoint<'_> {
    /// The environment variable this endpoint's key comes from (`OPENAI_API_KEY`, …; `API_KEY` for a type
    /// the binary does not know).
    pub fn env_key(&self) -> &'static str {
        provider_env_key(self.kind)
    }

    /// Where this endpoint's API key comes from under `env` — see [`ApiKey`].
    pub fn api_key(&self, env: &Env) -> ApiKey {
        let var = self.env_key();
        match env.var(var) {
            Some(key) => ApiKey::Env { var, key },
            None if !self.config.key.is_empty() => ApiKey::Config(self.config.key.clone()),
            None => ApiKey::Missing { var },
        }
    }
}

/// Where a run's API key comes from — the ONE definition of the precedence (root.go:62-92): the TYPE's
/// environment variable when it is set, else the entry's `key:`, else nothing. `resolve_run` takes the value,
/// `iota list providers` names the source, and `/model`'s catalog builds the other endpoints with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiKey {
    /// `$<var>` is set: the run uses it, whatever `key:` says.
    Env {
        /// The variable.
        var: &'static str,
        /// Its value.
        key: String,
    },
    /// `key:` in the config (the variable is unset).
    Config(String),
    /// Neither: `var` is what the user would set.
    Missing {
        /// The variable a `no key` message names.
        var: &'static str,
    },
}

impl ApiKey {
    /// The key itself; `None` when there is none.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Env { key, .. } | Self::Config(key) => Some(key),
            Self::Missing { .. } => None,
        }
    }
}
