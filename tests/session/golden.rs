//! Byte-level goldens against the REAL Go writer's output (design §8.1 / PROBE-RESULT.md): `meta.json`'s
//! key order, 2-space indentation and missing trailing newline, and the whole `messages.jsonl` omitempty
//! matrix.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::ProviderKind;
use iota::provider::model::{
    AssistantBody, Attachment, Body, JsonObject, Message, Raw, RawContent, ToolCall,
};
use iota::provider::usage::Usage;
use iota::session::{META_FILE, SESSION_SCHEMA_VERSION, SessionMeta};
use pretty_assertions::assert_eq;

use crate::common::{log_lines, temp_store};

/// `meta.json` as the Go writer produced it in the probe run (`json.MarshalIndent(m, "", "  ")`,
/// struct-order keys, NO trailing newline).
const GO_META: &str = r#"{
  "v": 1,
  "id": "ws0b07cp764v",
  "created_at": "2026-08-31T17:28:23+08:00",
  "updated_at": "2026-08-31T17:28:23+08:00",
  "provider": "openai",
  "model": "gpt-probe",
  "temperature": 0.7,
  "base_url": "http://127.0.0.1:1/v1",
  "cwd": "/tmp/probe-cwd",
  "title": "probe session",
  "message_count": 3
}"#;

/// The probe bundle's `messages.jsonl`, compact, one record per line.
const GO_LOG: [&str; 3] = [
    r#"{"role":"system","content":"you are a probe"}"#,
    r#"{"role":"user","content":"hello"}"#,
    r#"{"role":"assistant","content":"hi there","usage":{"in":11,"out":7,"total":18}}"#,
];

/// `to_string_pretty` on a `SessionMeta` reproduces the Go writer byte for byte.
#[test]
fn meta_serialises_exactly_like_go() {
    let meta = SessionMeta {
        version: SESSION_SCHEMA_VERSION,
        id: "ws0b07cp764v".to_owned(),
        created_at: "2026-08-31T17:28:23+08:00".to_owned(),
        updated_at: "2026-08-31T17:28:23+08:00".to_owned(),
        provider: "openai".to_owned(),
        model: "gpt-probe".to_owned(),
        temperature: Some(0.7),
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        cwd: "/tmp/probe-cwd".to_owned(),
        title: "probe session".to_owned(),
        message_count: 3,
        ..SessionMeta::default()
    };
    assert_eq!(serde_json::to_string_pretty(&meta).unwrap(), GO_META);
    // ...and back, losslessly.
    assert_eq!(serde_json::from_str::<SessionMeta>(GO_META).unwrap(), meta);
}

/// The file on disk has 2-space indentation, Go's key order and NO trailing newline.
#[test]
fn meta_file_has_go_key_order_and_no_trailing_newline() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(
            ProviderKind::OpenAi,
            "gpt-probe",
            Some(0.7),
            "http://127.0.0.1:1/v1",
            "/tmp/probe-cwd",
            false,
        )
        .unwrap();
    writer
        .update_meta(|meta| meta.title = "probe session".to_owned())
        .unwrap();
    writer
        .append_messages(&[
            Message::system("you are a probe"),
            Message::user("hello"),
            Message::assistant("hi there").with_usage(Some(Usage {
                input: 11,
                output: 7,
                total: 18,
                ..Usage::default()
            })),
        ])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    let text = std::fs::read_to_string(dir.join(META_FILE)).unwrap();
    assert!(
        !text.ends_with('\n'),
        "meta.json must have no trailing newline"
    );
    assert!(text.starts_with("{\n  \"v\": 1,\n  \"id\": "), "got {text}");
    assert!(text.ends_with("\n  \"message_count\": 3\n}"), "got {text}");
    // The key ORDER is Go's struct order, not alphabetical.
    let keys: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split('"').next())
        .collect();
    assert_eq!(
        keys,
        [
            "v",
            "id",
            "created_at",
            "updated_at",
            "provider",
            "model",
            "temperature",
            "base_url",
            "cwd",
            "title",
            "message_count",
        ]
    );

    // The log matches the Go writer's bytes (only the timestamps and the id differ between runs).
    assert_eq!(log_lines(&dir), GO_LOG);
    let raw = std::fs::read(dir.join("messages.jsonl")).unwrap();
    assert_eq!(*raw.last().unwrap(), b'\n', "each record ends in a newline");
}

/// The whole omitempty matrix of `messages.jsonl` (CONTRACTS S§2.3), driven through the real writer.
#[test]
fn jsonl_omitempty_matrix() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(ProviderKind::Gemini, "gemini-probe", None, "", "", false)
        .unwrap();
    let dir = writer.dir().to_path_buf();

    let mut arguments = JsonObject::new();
    arguments.insert("q".to_owned(), serde_json::Value::from(1));
    let calls = vec![ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments,
    }];
    let blob = r#"{"parts":[{"text":"thinking...","thought":true}],"role":"model"}"#;

    writer
        .append_messages(&[
            Message::system("sys"),
            Message {
                attachments: vec![Attachment {
                    filename: "a.txt".to_owned(),
                    mime_type: "text/plain".to_owned(),
                    data: b"hello".to_vec(),
                }],
                ..Message::user("hi")
            },
            Message::assistant_with_calls(
                "",
                calls.clone(),
                Some(RawContent::Google(
                    Raw::from_string(blob.to_owned()).unwrap(),
                )),
            )
            .with_usage(Some(Usage {
                input: 1000,
                output: 200,
                total: 1200,
                ..Usage::default()
            })),
            Message::tool_result(&calls[0], "ok", false),
            Message::assistant("done")
                .with_reasoning("th".to_owned())
                .with_usage(Some(Usage {
                    input: 10,
                    output: 5,
                    ..Usage::default()
                })),
            Message {
                content: "cut".to_owned(),
                body: Body::Assistant(AssistantBody {
                    interrupted: true,
                    ..AssistantBody::default()
                }),
                ..Message::default()
            },
        ])
        .unwrap();
    writer
        .append_compaction(
            "SUMMARY",
            3,
            Some(Usage {
                input: 50,
                output: 20,
                ..Usage::default()
            }),
        )
        .unwrap();
    drop(writer);

    assert_eq!(
        log_lines(&dir),
        [
            r#"{"role":"system","content":"sys"}"#,
            r#"{"role":"user","content":"hi","attachments":[{"filename":"a.txt","mime":"text/plain","data_ref":"sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"}]}"#,
            &format!(
                r#"{{"role":"assistant","tool_calls":[{{"id":"c1","name":"f","arguments":{{"q":1}}}}],"raw":{{"provider":"gemini","blob":{blob}}},"usage":{{"in":1000,"out":200,"total":1200}}}}"#
            ),
            r#"{"role":"tool","content":"ok","tool_call_id":"c1","tool_call_name":"f"}"#,
            r#"{"role":"assistant","content":"done","reasoning":"th","usage":{"in":10,"out":5}}"#,
            r#"{"role":"assistant","content":"cut","interrupted":true}"#,
            r#"{"role":"compaction","content":"SUMMARY","compacted_through":2,"usage":{"in":50,"out":20}}"#,
        ]
    );
}

/// A tool call with no arguments still emits `"arguments":{}` (D-49), and an error tool result keeps
/// `is_error`.
#[test]
fn always_present_keys() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(ProviderKind::OpenAi, "m1", None, "", "", false)
        .unwrap();
    let dir = writer.dir().to_path_buf();
    let call = ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    };
    writer
        .append_messages(&[
            Message::assistant_with_calls("", vec![call.clone()], None),
            Message::tool_result(&call, "boom", true),
            Message::assistant("zero usage").with_usage(Some(Usage::default())),
        ])
        .unwrap();
    drop(writer);

    assert_eq!(
        log_lines(&dir),
        [
            r#"{"role":"assistant","tool_calls":[{"id":"c1","name":"f","arguments":{}}]}"#,
            r#"{"role":"tool","content":"boom","tool_call_id":"c1","tool_call_name":"f","is_error":true}"#,
            r#"{"role":"assistant","content":"zero usage","usage":{"in":0,"out":0}}"#,
        ]
    );
}
