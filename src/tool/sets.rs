//! The built-in toolset table (tool/tool.go:329-341): the four set names, their factories and the config
//! types they decode, plus `SetError`, the byte-equal Go factory refusals. Every set is always compiled in —
//! one binary, exactly like Go.

use std::{collections::BTreeMap, sync::Arc};

use crate::tool::{Tool, ToolEnv};

/// A raw YAML node as parsed from the config file; each set's factory decodes its own `tools.<name>` value.
pub type RawNode = serde_norway::Value;
/// `tools:` map — key presence enables a set; the raw value is decoded by the set's factory.
pub type ToolsConfig = BTreeMap<String, RawNode>;
/// Must succeed on `None`/`Null` (defaults). May return `Ok(vec![])` ("contributes no tools").
pub(crate) type SetFactory = fn(&ToolEnv, Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError>;
/// The four built-in set names (tool/tool.go:329-341).
pub(crate) const SET_NAMES: [&str; 4] = ["shell", SKILLS_SET, "code", "ask"];

/// The skills set — `load_skill` alone. It was called `agent` until the three-layer split, where the word
/// `agent` became the name of a config layer and could no longer also mean a toolset (brain page
/// `config-three-layers`). The old spelling is still accepted by `crate::config::migrate`.
pub const SKILLS_SET: &str = "skills";

/// The factory of a built-in set; `None` for unknown names.
pub fn set_factory(name: &str) -> Option<SetFactory> {
    match name {
        "shell" => Some(super::shell::new_shell_set),
        SKILLS_SET => Some(super::agent::new_skills_set),
        "code" => Some(super::code::new_code_set),
        "ask" => Some(super::ask::new_ask_set),
        _ => None,
    }
}

/// Why a toolset factory refused its configuration. Every text is byte-equal to the Go set factories,
/// except [`SetError::NoShell`], which reports a machine Go never had to refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    /// `tools.shell` is not a mapping (or fails to decode).
    #[error("config must be a mapping (sandbox, network, auto_run, write): {0}")]
    ShellConfig(String),
    /// `tools.shell` was asked for on a machine with no interpreter to run it (the
    /// `crate::shell::interp::NoShell` text).
    #[error("{0}")]
    NoShell(String),
    /// `tools.shell.sandbox` is neither `auto` nor `off`.
    #[error("sandbox must be \"auto\" or \"off\", got {0:?}")]
    BadSandbox(String),
    /// `tools.code` is not a mapping (or fails to decode).
    #[error("config must be a mapping (auto_write, read_only): {0}")]
    CodeConfig(String),
    /// `tools.code` sets both `read_only` and `auto_write`.
    #[error(
        "read_only and auto_write contradict each other: auto_write approves writes the set does not offer"
    )]
    CodeContradiction,
}

#[cfg(test)]
mod tests {
    use super::{SET_NAMES, set_factory};

    /// Every built-in set name resolves to its factory (the config surface is exactly these names), and an
    /// unknown name resolves to nothing. Formerly `tests/tool/framework.rs`.
    #[test]
    fn every_built_in_set_has_a_factory() {
        for name in SET_NAMES {
            assert!(set_factory(name).is_some(), "{name} must have a factory");
        }
        assert!(set_factory("nope").is_none());
    }
}
