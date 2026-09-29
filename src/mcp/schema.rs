//! Normalizes an MCP server's `inputSchema` before it reaches a provider. JSON Schema regexes are ECMA-262 with the
//! `u` flag (2020-12 §6.4), where an identity escape is legal only for a syntax character, `/`, and `-` inside a
//! class. Servers written against looser engines emit escapes like `\_` that strict providers reject as "not a
//! regex", failing the whole request (`DeepSeek` validates `pattern` formats whenever the schema declares a
//! `$schema`). Dropping such a backslash matches the same strings everywhere (X-57).

use serde_json::Value;

use crate::provider::model::JsonObject;

/// Keywords whose values are instance data, not subschemas: a `pattern` key in them is not a regex.
const DATA_KEYWORDS: [&str; 4] = ["const", "default", "enum", "examples"];

/// Rewrites every `pattern` and `patternProperties` key in `schema` (recursively) with [`strict_pattern`].
pub(crate) fn normalize(schema: &mut JsonObject) {
    if let Some(Value::String(p)) = schema.get_mut("pattern") {
        *p = strict_pattern(p);
    }
    if let Some(Value::Object(props)) = schema.get_mut("patternProperties") {
        *props = std::mem::take(props)
            .into_iter()
            .map(|(k, v)| (strict_pattern(&k), v))
            .collect();
    }
    for (key, value) in schema.iter_mut() {
        if !DATA_KEYWORDS.contains(&key.as_str()) {
            normalize_value(value);
        }
    }
}

fn normalize_value(value: &mut Value) {
    match value {
        Value::Object(o) => normalize(o),
        Value::Array(a) => a.iter_mut().for_each(normalize_value),
        _ => {}
    }
}

/// `pattern` with each backslash dropped whose escape the `u` flag forbids: one before an ASCII punctuation
/// character that is not a syntax character or `/` (nor `-` inside a class), or before a non-ASCII character.
/// Letter and digit escapes (`\d`, `\u{..}`, `\1`) are kept as written.
fn strict_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e) if e.is_ascii_alphanumeric() || is_legal_identity(e, in_class) => {
                    out.push('\\');
                    out.push(e);
                }
                Some(e) => out.push(e),
                None => out.push('\\'),
            },
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// ECMA-262 `IdentityEscape[+UnicodeMode]`: `SyntaxCharacter` or `/`; `ClassEscape` adds `-`.
fn is_legal_identity(c: char, in_class: bool) -> bool {
    matches!(
        c,
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/'
    ) || (in_class && c == '-')
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn needless_escapes_are_dropped() {
        assert_eq!(strict_pattern(r"^agent\_run\_"), "^agent_run_");
        assert_eq!(strict_pattern(r"a\:b\@c\#\é"), "a:b@c#é");
        assert_eq!(strict_pattern(r"\-x"), "-x");
    }

    #[test]
    fn legal_escapes_are_kept() {
        for p in [
            r"^\d+\.\d+$",
            r"\(\)\[\]\{\}\|\*\+\?\^\$\\\/",
            r"[a\-z]",
            r"\u{1F600}\p{L}\1\b",
            r"[\]\-]x",
            "trailing\\",
        ] {
            assert_eq!(strict_pattern(p), p, "{p}");
        }
    }

    #[test]
    fn class_ends_before_a_later_escaped_dash() {
        assert_eq!(strict_pattern(r"[a\-z]\-"), r"[a\-z]-");
    }

    #[test]
    fn normalize_walks_subschemas_and_skips_instance_data() {
        let mut schema = json!({
            "properties": {
                "runId": { "type": "string", "pattern": r"^agent\_run\_" },
                "pattern": { "type": "string", "default": { "pattern": r"\_" } },
                "tags": { "type": "array", "items": { "pattern": r"\_" } },
                "any": { "anyOf": [{ "pattern": r"\_" }] }
            },
            "patternProperties": { r"^x\_": { "pattern": r"\_" } },
            "enum": [{ "pattern": r"\_" }]
        });
        normalize(schema.as_object_mut().unwrap());
        assert_eq!(
            schema,
            json!({
                "properties": {
                    "runId": { "type": "string", "pattern": "^agent_run_" },
                    "pattern": { "type": "string", "default": { "pattern": r"\_" } },
                    "tags": { "type": "array", "items": { "pattern": "_" } },
                    "any": { "anyOf": [{ "pattern": "_" }] }
                },
                "patternProperties": { "^x_": { "pattern": "_" } },
                "enum": [{ "pattern": r"\_" }]
            })
        );
    }
}
