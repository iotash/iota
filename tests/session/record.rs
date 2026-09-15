//! The `messages.jsonl` line DTOs: the `arguments` null/`{}`/absent rule, the usage key set and the
//! unknown-key tolerance a Go-written line depends on.

use iota::session::{SessionRecord, SessionToolCall, SessionUsage};
use pretty_assertions::assert_eq;

/// `null`, `{}` and an absent key all deserialise to the empty map, and Rust ALWAYS writes `{}` (D-49).
#[test]
fn arguments_null_is_accepted_and_empty_object_is_written() {
    for line in [
        r#"{"id":"c1","name":"f","arguments":null}"#,
        r#"{"id":"c1","name":"f","arguments":{}}"#,
        r#"{"id":"c1","name":"f"}"#,
    ] {
        let call: SessionToolCall = serde_json::from_str(line).unwrap();
        assert!(call.arguments.is_empty(), "from {line}");
        assert_eq!(
            serde_json::to_string(&call).unwrap(),
            r#"{"id":"c1","name":"f","arguments":{}}"#,
            "rewritten from {line}"
        );
    }
    // A populated map survives verbatim, with serde_json's sorted keys (Go marshal order).
    let call: SessionToolCall =
        serde_json::from_str(r#"{"id":"c1","name":"f","arguments":{"z":1,"a":2}}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&call).unwrap(),
        r#"{"id":"c1","name":"f","arguments":{"a":2,"z":1}}"#
    );
}

/// `usage.in` / `usage.out` are emitted even when zero; the other three are omitted (chat/session.go:90-99).
#[test]
fn usage_key_set() {
    assert_eq!(
        serde_json::to_string(&SessionUsage::default()).unwrap(),
        r#"{"in":0,"out":0}"#
    );
    assert_eq!(
        serde_json::to_string(&SessionUsage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total: 5,
        })
        .unwrap(),
        r#"{"in":1,"out":2,"cache_read":3,"cache_write":4,"total":5}"#
    );
    // Reading tolerates a partial object (old bundles).
    let usage: SessionUsage = serde_json::from_str(r#"{"in":7}"#).unwrap();
    assert_eq!(usage.input, 7);
    assert_eq!(usage.output, 0);
}

/// A record with only a role is the minimum Go can have written; every other field defaults.
#[test]
fn minimal_record_round_trips() {
    let rec: SessionRecord = serde_json::from_str(r#"{"role":"user"}"#).unwrap();
    assert_eq!(
        rec,
        SessionRecord {
            role: "user".to_owned(),
            ..SessionRecord::default()
        }
    );
    assert_eq!(
        serde_json::to_string(&rec).unwrap(),
        r#"{"role":"user"}"#,
        "every empty field is omitted"
    );
    // Even an empty object parses (Go's zero value), and a `null` raw is simply absent.
    let rec: SessionRecord = serde_json::from_str(r#"{"raw":null}"#).unwrap();
    assert_eq!(rec, SessionRecord::default());
    assert_eq!(serde_json::to_string(&rec).unwrap(), r#"{"role":""}"#);
}

/// A Go-written line with a `raw` blob keeps the blob's bytes verbatim through a Rust round trip.
#[test]
fn raw_blob_is_carried_verbatim() {
    let line = r#"{"role":"assistant","raw":{"provider":"gemini","blob":{"parts":[{"text":"t","thought":true,"thoughtSignature":"AQID"}],"role":"model"}}}"#;
    let rec: SessionRecord = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&rec).unwrap(), line);
}
