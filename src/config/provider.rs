//! `providers.<name>` — the ENDPOINT layer: how to reach an API and how to authenticate, and nothing else.
//!
//! Everything that describes how to USE a model moved out in the three-layer split (brain page
//! `config-three-layers`): a model's identity and its protocol bindings live under `models:`, the way an
//! agent drives it under `agents:`. A one-layer config that still writes those keys here is accepted by
//! [`crate::config::migrate`], which splits it into the three entries it means.

/// One `providers.<name>` entry. Every field defaults; the three together are the whole endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    /// `type:` — the built-in provider type behind an alias (`""` = the entry's own name is the type).
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
