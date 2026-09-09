//! YAML 1.1 boolean leniency (DIVERGENCES I-01): one place that reads `true/yes/on` / `false/no/off` in any
//! case, quoted or plain, for every bool field of the config and for the toolset-disable rule.

use serde::{Deserialize, de::Error as _};

use crate::tool::sets::RawNode;

/// The error text every bool field reports for a value that is not a YAML-1.1 boolean.
const BAD_BOOL: &str = "invalid value: expected a boolean (true/yes/on/false/no/off)";

/// Bool(b) → Some(b); String s with `s.eq_ignore_ascii_case` ∈ {true,yes,on} → Some(true), {false,no,off} →
/// Some(false); else None.
pub(crate) fn yaml11_bool(v: &RawNode) -> Option<bool> {
    match v {
        RawNode::Bool(b) => Some(*b),
        RawNode::String(s) => {
            if ["true", "yes", "on"]
                .iter()
                .any(|w| s.eq_ignore_ascii_case(w))
            {
                Some(true)
            } else if ["false", "no", "off"]
                .iter()
                .any(|w| s.eq_ignore_ascii_case(w))
            {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// POLICY divergence: any YAML-1.1 false spelling disables (`false`, `False`, `no`, `off`, "false" quoted…).
pub(crate) fn is_false_scalar(v: &RawNode) -> bool {
    yaml11_bool(v) == Some(false)
}

/// serde `deserialize_with` for bool fields: Null → false; Bool; YAML-1.1 strings; anything else → error
/// `invalid value: expected a boolean (true/yes/on/false/no/off)`.
pub(crate) fn deserialize_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(deserialize_opt_bool(d)?.unwrap_or(false))
}

/// Null → None; otherwise as [`deserialize_bool`].
pub(crate) fn deserialize_opt_bool<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<bool>, D::Error> {
    let v = RawNode::deserialize(d)?;
    if v.is_null() {
        return Ok(None);
    }
    yaml11_bool(&v)
        .map(Some)
        .ok_or_else(|| D::Error::custom(BAD_BOOL))
}

/// None/Null → `T::default()`; else `serde_norway::from_value(v.clone())`, error rendered with Display (the text
/// after the colon differs from yaml.v3 — DIVERGENCES D-15).
pub(crate) fn decode_mapping<T: serde::de::DeserializeOwned + Default>(
    node: Option<&RawNode>,
) -> Result<T, String> {
    match node {
        None | Some(RawNode::Null) => Ok(T::default()),
        Some(v) => serde_norway::from_value(v.clone()).map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{
        decode_mapping, deserialize_bool, deserialize_opt_bool, is_false_scalar, yaml11_bool,
    };
    use crate::tool::sets::RawNode;

    fn node(yaml: &str) -> RawNode {
        serde_norway::from_str(yaml).expect("yaml")
    }

    // New (DIVERGENCES I-01): every YAML-1.1 spelling, plain or quoted, any case.
    #[test]
    fn yaml11_bool_table() {
        for (yaml, want) in [
            ("true", Some(true)),
            ("True", Some(true)),
            ("TRUE", Some(true)),
            ("yes", Some(true)),
            ("Yes", Some(true)),
            ("on", Some(true)),
            ("ON", Some(true)),
            ("\"true\"", Some(true)),
            ("'yes'", Some(true)),
            ("false", Some(false)),
            ("False", Some(false)),
            ("FALSE", Some(false)),
            ("no", Some(false)),
            ("No", Some(false)),
            ("off", Some(false)),
            ("OFF", Some(false)),
            ("\"false\"", Some(false)),
            ("'off'", Some(false)),
            ("0", None),
            ("1", None),
            ("~", None),
            ("null", None),
            ("maybe", None),
            ("[]", None),
            ("{}", None),
            ("\"\"", None),
        ] {
            let n = node(yaml);
            assert_eq!(yaml11_bool(&n), want, "yaml11_bool({yaml:?})");
            assert_eq!(
                is_false_scalar(&n),
                want == Some(false),
                "is_false_scalar({yaml:?})"
            );
        }
    }

    #[derive(Deserialize, Default, Debug, PartialEq, Eq)]
    #[serde(default)]
    struct Flags {
        #[serde(deserialize_with = "deserialize_bool")]
        a: bool,
        #[serde(deserialize_with = "deserialize_opt_bool")]
        b: Option<bool>,
    }

    #[test]
    fn deserialize_bool_fields() {
        let f: Flags = decode_mapping(Some(&node("a: yes\nb: Off\n"))).expect("decode");
        assert_eq!(
            f,
            Flags {
                a: true,
                b: Some(false)
            }
        );
        let f: Flags = decode_mapping(Some(&node("a: ~\nb: ~\n"))).expect("decode");
        assert_eq!(f, Flags { a: false, b: None });
        let f: Flags = decode_mapping(Some(&node("a: \"TRUE\"\n"))).expect("decode");
        assert_eq!(f, Flags { a: true, b: None });
        let err = decode_mapping::<Flags>(Some(&node("a: 1\n"))).expect_err("1 is not a bool");
        assert!(
            err.contains("invalid value: expected a boolean (true/yes/on/false/no/off)"),
            "{err}"
        );
        let err = decode_mapping::<Flags>(Some(&node("b: maybe\n"))).expect_err("maybe");
        assert!(
            err.contains("invalid value: expected a boolean (true/yes/on/false/no/off)"),
            "{err}"
        );
    }

    #[test]
    fn decode_mapping_null_is_default() {
        assert_eq!(decode_mapping::<Flags>(None), Ok(Flags::default()));
        assert_eq!(
            decode_mapping::<Flags>(Some(&RawNode::Null)),
            Ok(Flags::default())
        );
        assert_eq!(
            decode_mapping::<Flags>(Some(&node("{}"))),
            Ok(Flags::default())
        );
        // A non-mapping fails with the library's text (D-15); the caller prefixes it.
        assert!(decode_mapping::<Flags>(Some(&node("[not, a, mapping]"))).is_err());
    }
}
