//! Argument readers shared by every built-in tool.

use std::{
    io::{ErrorKind, Read},
    path::Path,
};

use crate::tool::ToolOutput;

use crate::provider::model::JsonObject;
use serde_json::Value;

/// `Value::String` → `&str`; anything else (missing, non-string) → `""`.
pub(crate) fn str_arg<'a>(args: &'a JsonObject, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `Value::Number` → f64 truncated toward zero; anything else → 0 (strings are NOT parsed).
pub(crate) fn int_arg(args: &JsonObject, key: &str) -> i64 {
    let Some(Value::Number(n)) = args.get(key) else {
        return 0;
    };
    if let Some(i) = n.as_i64() {
        return i;
    }
    if let Some(u) = n.as_u64() {
        return i64::try_from(u).unwrap_or(i64::MAX);
    }
    n.as_f64().map_or(0, float_to_int)
}

/// A float argument as an integer: truncation toward zero, saturating at the i64 range.
#[allow(clippy::cast_possible_truncation)]
fn float_to_int(f: f64) -> i64 {
    f.trunc() as i64
}

/// `Value::Bool` → it; `Value::String` matching `strconv.ParseBool` ("1","t","T","TRUE","true","True" /
/// "0","f","F","FALSE","false","False"); else `default`.
pub(crate) fn bool_arg(args: &JsonObject, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.as_str() {
            "1" | "t" | "T" | "TRUE" | "true" | "True" => true,
            "0" | "f" | "F" | "FALSE" | "false" | "False" => false,
            _ => default,
        },
        _ => default,
    }
}

/// Err texts embed the ABSOLUTE path: `file does not exist: {p}`, `cannot access {p}: {e}`,
/// `{p} is a directory, not a file`, `{p} is not a regular file`, `cannot open {p}: {e}`, `cannot read {p}: {e}`.
/// Ok = (bytes ≤ max, true size).
pub(crate) fn read_file_limited(path: &Path, max: u64) -> Result<(Vec<u8>, u64), ToolOutput> {
    let p = path.display();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(ToolOutput::err(format!("file does not exist: {p}")));
        }
        Err(e) => return Err(ToolOutput::err(format!("cannot access {p}: {e}"))),
    };
    if meta.is_dir() {
        return Err(ToolOutput::err(format!("{p} is a directory, not a file")));
    }
    if !meta.is_file() {
        return Err(ToolOutput::err(format!("{p} is not a regular file")));
    }
    let file =
        std::fs::File::open(path).map_err(|e| ToolOutput::err(format!("cannot open {p}: {e}")))?;
    let mut data = Vec::new();
    file.take(max)
        .read_to_end(&mut data)
        .map_err(|e| ToolOutput::err(format!("cannot read {p}: {e}")))?;
    Ok((data, meta.len()))
}

#[cfg(test)]
mod tests {
    use crate::provider::model::JsonObject;
    use serde_json::json;

    use super::{bool_arg, int_arg, read_file_limited, str_arg};

    fn args(v: serde_json::Value) -> JsonObject {
        match v {
            serde_json::Value::Object(m) => m,
            _ => panic!("object literal expected"),
        }
    }

    // `bool_arg` accepts the twelve spellings `1 t T TRUE true True` / `0 f F FALSE false False` and
    // nothing else.
    #[test]
    fn bool_arg_parse_bool_spellings() {
        for s in ["1", "t", "T", "TRUE", "true", "True"] {
            assert!(bool_arg(&args(json!({ "k": s })), "k", false), "{s}");
        }
        for s in ["0", "f", "F", "FALSE", "false", "False"] {
            assert!(!bool_arg(&args(json!({ "k": s })), "k", true), "{s}");
        }
        // Real booleans pass through; anything else keeps the default.
        assert!(bool_arg(&args(json!({ "k": true })), "k", false));
        assert!(!bool_arg(&args(json!({ "k": false })), "k", true));
        for v in [
            json!("yes"),
            json!("tRuE"),
            json!(""),
            json!(1),
            json!(null),
        ] {
            assert!(bool_arg(&args(json!({ "k": v })), "k", true), "{v}");
            assert!(!bool_arg(&args(json!({ "k": v })), "k", false), "{v}");
        }
        assert!(bool_arg(&JsonObject::new(), "missing", true));
        assert!(!bool_arg(&JsonObject::new(), "missing", false));
    }

    // `int_arg` reads numbers only — a numeric string is 0.
    #[test]
    fn int_arg_ignores_strings() {
        assert_eq!(int_arg(&args(json!({ "n": 7 })), "n"), 7);
        assert_eq!(int_arg(&args(json!({ "n": 7.9 })), "n"), 7);
        assert_eq!(int_arg(&args(json!({ "n": -2.5 })), "n"), -2);
        assert_eq!(int_arg(&args(json!({ "n": "12" })), "n"), 0);
        assert_eq!(int_arg(&args(json!({ "n": true })), "n"), 0);
        assert_eq!(int_arg(&args(json!({ "n": null })), "n"), 0);
        assert_eq!(int_arg(&JsonObject::new(), "n"), 0);
        assert_eq!(int_arg(&args(json!({ "n": u64::MAX })), "n"), i64::MAX);
    }

    #[test]
    fn str_arg_strings_only() {
        assert_eq!(str_arg(&args(json!({ "s": "x" })), "s"), "x");
        assert_eq!(str_arg(&args(json!({ "s": 1 })), "s"), "");
        assert_eq!(str_arg(&args(json!({ "s": null })), "s"), "");
        assert_eq!(str_arg(&JsonObject::new(), "s"), "");
    }

    #[test]
    fn read_file_limited_error_texts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"hello world").expect("write");
        let (data, size) = read_file_limited(&file, 5).expect("read");
        assert_eq!(data, b"hello");
        assert_eq!(size, 11);
        let (data, size) = read_file_limited(&file, 100).expect("read");
        assert_eq!(data, b"hello world");
        assert_eq!(size, 11);

        let missing = dir.path().join("nope");
        assert_eq!(
            read_file_limited(&missing, 10).expect_err("missing").text,
            format!("file does not exist: {}", missing.display())
        );
        assert_eq!(
            read_file_limited(dir.path(), 10).expect_err("dir").text,
            format!("{} is a directory, not a file", dir.path().display())
        );
    }
}
