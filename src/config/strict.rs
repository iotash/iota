//! The key audit: every key of a config document is checked against the layer it was written in, BEFORE the
//! document is decoded (brain page `config-three-layers`).
//!
//! iota used to accept a one-layer config — every key under `providers.<name>` — and split it into the three
//! entries it meant. That layer is gone (there is no released binary whose configs need carrying), and with it
//! the tolerance that made it possible: a key the layer does not own is a config MISTAKE, and the error says
//! which layer owns it rather than silently doing nothing.
//!
//! The audit works on the raw [`Value`] so an error can name the exact coordinate (`agents.coder.tools`), which
//! a `deny_unknown_fields` decode cannot: serde only knows the innermost struct.

use serde_norway::Value;

use crate::config::ConfigError;
use crate::tool::sets::SET_NAMES;

/// The top-level maps.
const TOP_KEYS: [&str; 4] = ["providers", "models", "agents", "mcp_servers"];
/// Every key a `providers.<name>` entry accepts — the ENDPOINT.
const PROVIDER_KEYS: [&str; 3] = ["type", "key", "url"];
/// Every key a `models.<name>` entry accepts.
const MODEL_KEYS: [&str; 12] = [
    "provider",
    "id",
    "context_window",
    "defer_mode",
    "effort",
    "temperature",
    "top_p",
    "image",
    "aspect_ratio",
    "image_size",
    "negative_prompt",
    "json_edits",
];
/// Every key an `agents.<name>` entry accepts.
const AGENT_KEYS: [&str; 13] = [
    "models",
    "system",
    "system_file",
    "tools",
    "mcp_servers",
    "workspace",
    "no_save",
    "notify",
    "description",
    "context_window",
    "effort",
    "temperature",
    "top_p",
];
/// Every key an `mcp_servers.<name>` entry accepts.
const MCP_KEYS: [&str; 6] = ["command", "args", "url", "env", "headers", "defer"];

/// Keys the three-layer split retired, and the sentence that tells the user what to write instead. They are
/// checked before the "belongs under" table, so the message names the replacement rather than a layer.
const RETIRED_KEYS: [(&str, &str); 2] = [
    (
        "model",
        "`model` is now a `models:` entry — write `models.<name>: <provider>:<id>` and list it in `agents.<name>.models`",
    ),
    ("agent", "`agent` is now `workspace:` on an `agents:` entry"),
];

/// Toolsets that were removed or renamed; anything else unknown gets the generic refusal.
const RETIRED_SETS: [(&str, &str); 2] = [
    (
        "agent",
        "the `agent` toolset is now called `skills` (the word `agent` names a config layer)",
    ),
    (
        "delegate",
        "the `delegate` toolset was removed — run child agents from bash instead (see README)",
    ),
];

/// Checks every key of one decoded document. `Ok(())` means the typed decode that follows will see nothing it
/// would have to ignore.
pub(crate) fn audit(doc: &Value) -> Result<(), ConfigError> {
    let Value::Mapping(top) = doc else {
        // An empty document (`Value::Null`) is a valid empty config; a scalar one fails the typed decode.
        return Ok(());
    };
    for (key, section) in top {
        let Some(name) = key.as_str() else { continue };
        if !TOP_KEYS.contains(&name) {
            return Err(ConfigError::Key {
                at: name.to_owned(),
                message: format!(
                    "unknown top-level key (want {})",
                    TOP_KEYS.map(|k| format!("{k}:")).join(", ")
                ),
            });
        }
        audit_section(name, section)?;
    }
    Ok(())
}

/// One top-level map: every entry that is a mapping has its keys checked against `section`'s own set.
fn audit_section(section: &str, entries: &Value) -> Result<(), ConfigError> {
    let Value::Mapping(entries) = entries else {
        return Ok(());
    };
    for (key, entry) in entries {
        // A `models:` shorthand (`sonnet: anthropic:claude-x`) is a string, not a mapping; so is a `tools:`
        // value the audit does not own. Only mappings carry keys to check.
        let (Some(name), Value::Mapping(fields)) = (key.as_str(), entry) else {
            continue;
        };
        for (key, value) in fields {
            let Some(key) = key.as_str() else { continue };
            check_key(section, name, key)?;
            if section == "agents" && key == "tools" {
                check_tools(name, value)?;
            }
        }
    }
    Ok(())
}

/// One `<section>.<entry>.<key>`: accepted, retired (with its replacement), owned by another layer, or unknown.
fn check_key(section: &str, entry: &str, key: &str) -> Result<(), ConfigError> {
    let (own, elsewhere): (&[&str], &[(&str, &[&str])]) = match section {
        "providers" => (
            &PROVIDER_KEYS,
            &[("models", &MODEL_KEYS), ("agents", &AGENT_KEYS)],
        ),
        "models" => (
            &MODEL_KEYS,
            &[("agents", &AGENT_KEYS), ("providers", &PROVIDER_KEYS)],
        ),
        "agents" => (
            &AGENT_KEYS,
            &[("models", &MODEL_KEYS), ("providers", &PROVIDER_KEYS)],
        ),
        _ => (&MCP_KEYS, &[]),
    };
    if own.contains(&key) {
        return Ok(());
    }
    let at = format!("{section}.{entry}.{key}");
    if let Some((_, hint)) = RETIRED_KEYS.iter().find(|(k, _)| *k == key) {
        return Err(ConfigError::Key {
            at,
            message: (*hint).to_owned(),
        });
    }
    // The two overlapping layers (`models:` and `agents:` share the four layered parameters) can never
    // collide here: a key valid in the section it was written in already returned above.
    if let Some((layer, _)) = elsewhere.iter().find(|(_, keys)| keys.contains(&key)) {
        return Err(ConfigError::Key {
            at,
            message: format!("`{key}` belongs under `{layer}:` (see README, \"The three layers\")"),
        });
    }
    Err(ConfigError::Key {
        at,
        message: format!("unknown key (want {})", own.join(", ")),
    })
}

/// `agents.<name>.tools`: every key names a built-in toolset.
fn check_tools(agent: &str, tools: &Value) -> Result<(), ConfigError> {
    let Value::Mapping(sets) = tools else {
        return Ok(());
    };
    for (key, _) in sets {
        let Some(name) = key.as_str() else { continue };
        if SET_NAMES.contains(&name) {
            continue;
        }
        let at = format!("agents.{agent}.tools.{name}");
        return Err(match RETIRED_SETS.iter().find(|(k, _)| *k == name) {
            Some((_, hint)) => ConfigError::Key {
                at,
                message: (*hint).to_owned(),
            },
            None => ConfigError::Key {
                at,
                message: format!("unknown toolset (want {})", SET_NAMES.join(", ")),
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::audit;

    /// Audits one document, returning the error text (`""` when it passes).
    fn check(yaml: &str) -> String {
        let doc: serde_norway::Value = serde_norway::from_str(yaml).expect("valid yaml");
        audit(&doc).err().map(|e| e.to_string()).unwrap_or_default()
    }

    #[test]
    fn a_three_layer_document_passes() {
        assert_eq!(
            check(
                "providers:\n  openai: {key: k}\nmodels:\n  gpt: openai:gpt-5\nagents:\n  default:\n    models: [gpt]\n    tools: {code: {}, shell: {}}\n"
            ),
            ""
        );
        // An empty document and an empty section are both fine.
        assert_eq!(check(""), "");
        assert_eq!(check("providers:\nmodels: {}\n"), "");
    }

    #[test]
    fn a_key_of_another_layer_names_that_layer() {
        assert_eq!(
            check("providers:\n  openai: {key: k, system: hi}\n"),
            "providers.openai.system: `system` belongs under `agents:` (see README, \"The three layers\")"
        );
        assert_eq!(
            check("providers:\n  openai: {key: k, effort: high}\n"),
            "providers.openai.effort: `effort` belongs under `models:` (see README, \"The three layers\")"
        );
        assert_eq!(
            check("agents:\n  a: {models: [x], url: https://x}\n"),
            "agents.a.url: `url` belongs under `providers:` (see README, \"The three layers\")"
        );
        // The four layered parameters live in BOTH `models:` and `agents:`, so neither reports the other.
        assert_eq!(
            check("models:\n  m: {provider: p, id: i, top_p: 0.5}\n"),
            ""
        );
        assert_eq!(check("agents:\n  a: {models: [m], top_p: 0.5}\n"), "");
        assert_eq!(
            check("models:\n  m: {provider: p, id: i, context_window: 400k}\n"),
            ""
        );
        assert_eq!(
            check("agents:\n  a: {models: [m], context_window: 400k}\n"),
            ""
        );
    }

    #[test]
    fn retired_keys_name_their_replacement() {
        assert!(
            check("providers:\n  openai: {key: k, model: gpt-4o}\n")
                .starts_with("providers.openai.model: `model` is now a `models:` entry"),
        );
        assert_eq!(
            check("providers:\n  openai: {key: k, agent: true}\n"),
            "providers.openai.agent: `agent` is now `workspace:` on an `agents:` entry"
        );
    }

    #[test]
    fn unknown_keys_are_refused_wherever_they_are() {
        assert_eq!(
            check("providers:\n  openai: {kye: k}\n"),
            "providers.openai.kye: unknown key (want type, key, url)"
        );
        assert_eq!(
            check("agnets:\n  a: {}\n"),
            "agnets: unknown top-level key (want providers:, models:, agents:, mcp_servers:)"
        );
        assert_eq!(
            check("mcp_servers:\n  fs: {commadn: npx}\n"),
            "mcp_servers.fs.commadn: unknown key (want command, args, url, env, headers, defer)"
        );
    }

    #[test]
    fn toolset_names_are_checked_where_they_are_written() {
        assert_eq!(
            check("agents:\n  a: {models: [m], tools: {nosuchset: {}}}\n"),
            "agents.a.tools.nosuchset: unknown toolset (want shell, skills, code, ask)"
        );
        assert_eq!(
            check("agents:\n  a: {models: [m], tools: {agent: {}}}\n"),
            "agents.a.tools.agent: the `agent` toolset is now called `skills` (the word `agent` names a config layer)"
        );
        assert_eq!(
            check("agents:\n  a: {models: [m], tools: {delegate: [r]}}\n"),
            "agents.a.tools.delegate: the `delegate` toolset was removed — run child agents from bash instead (see README)"
        );
    }
}
