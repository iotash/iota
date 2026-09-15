//! The dialect raw-payload round trip: the provider tag, the array-shaped variants and every drop rule
//! (`chat/session_test.go:340`, and the session halves of the Go dialect tests).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::ProviderKind;
use iota::provider::model::{Message, Raw, RawContent, ToolCall};
use iota::session::{NewSession, SessionRaw, blob_to_raw, raw_to_blob};
use pretty_assertions::assert_eq;

use crate::common::{log_lines, temp_store};

fn raw(json: &str) -> Raw {
    Raw::from_string(json.to_owned()).unwrap()
}
#[test]
fn raw_content_dropped_on_provider_mismatch() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(NewSession::new(ProviderKind::OpenAi, "m1"))
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .append_messages(&[Message::assistant_with_calls(
            "",
            vec![ToolCall {
                id: "c1".to_owned(),
                name: "t".to_owned(),
                arguments: iota::provider::model::JsonObject::new(),
            }],
            Some(RawContent::OpenAi(raw(r#"{"sig":"abc"}"#))),
        )])
        .unwrap();
    drop(writer);

    // Loading under a different provider type drops the opaque blob and keeps the rest.
    let sess = store.load(&id, ProviderKind::Anthropic).unwrap();
    assert_eq!(sess.messages[0].raw_content(), None);
    assert_eq!(sess.messages[0].tool_calls().len(), 1);
    // The same bundle under the writing type restores it.
    let sess = store.load(&id, ProviderKind::OpenAi).unwrap();
    assert_eq!(
        sess.messages[0].raw_content().cloned(),
        Some(RawContent::OpenAi(raw(r#"{"sig":"abc"}"#)))
    );
}

/// `gemini` and `vertexai` are the same dialect but NOT the same tag: a blob tagged with one never
/// restores under the other (CONTRACTS S§2.4).
#[test]
fn gemini_blob_does_not_restore_under_vertexai() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(NewSession::new(ProviderKind::Gemini, "m1"))
        .unwrap();
    let id = writer.id().to_owned();
    let blob = raw(r#"{"parts":[{"text":"t"}],"role":"model"}"#);
    writer
        .append_messages(&[Message::assistant_with_calls(
            "",
            vec![],
            Some(RawContent::Google(blob.clone())),
        )])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    assert!(log_lines(&dir)[0].contains(r#""raw":{"provider":"gemini","#));
    assert_eq!(
        store.load(&id, ProviderKind::Gemini).unwrap().messages[0]
            .raw_content()
            .cloned(),
        Some(RawContent::Google(blob))
    );
    assert_eq!(
        store.load(&id, ProviderKind::VertexAi).unwrap().messages[0].raw_content(),
        None
    );
}

/// An anthropic message with an EMPTY block list omits the `raw` key entirely (Go's `(nil, nil)`, D-50);
/// a non-empty one becomes a JSON array that round-trips.
#[test]
fn anthropic_blocks_array_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(NewSession::new(ProviderKind::Anthropic, "m1"))
        .unwrap();
    let id = writer.id().to_owned();
    let blocks = vec![
        raw(r#"{"type":"server_tool_use","id":"srv_1"}"#),
        raw(r#"{"type":"tool_search_tool_result","content":[]}"#),
    ];
    writer
        .append_messages(&[
            Message::assistant_with_calls("empty", vec![], Some(RawContent::Anthropic(vec![]))),
            Message::assistant_with_calls(
                "full",
                vec![],
                Some(RawContent::Anthropic(blocks.clone())),
            ),
        ])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    let lines = log_lines(&dir);
    assert_eq!(
        lines[0], r#"{"role":"assistant","content":"empty"}"#,
        "an empty block list must omit the raw key"
    );
    assert_eq!(
        lines[1],
        r#"{"role":"assistant","content":"full","raw":{"provider":"anthropic","blob":[{"type":"server_tool_use","id":"srv_1"},{"type":"tool_search_tool_result","content":[]}]}}"#
    );
    let sess = store.load(&id, ProviderKind::Anthropic).unwrap();
    assert_eq!(sess.messages[0].raw_content(), None);
    assert_eq!(
        sess.messages[1].raw_content().cloned(),
        Some(RawContent::Anthropic(blocks))
    );
}

/// The responses dialect stores its output items the same way, and Go's `null` shape reads back as the
/// empty item list.
#[test]
fn openresponses_items_array_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(NewSession::new(ProviderKind::OpenResponses, "m1"))
        .unwrap();
    let id = writer.id().to_owned();
    let items = vec![raw(r#"{"type":"reasoning","id":"rs_1"}"#)];
    writer
        .append_messages(&[Message::assistant_with_calls(
            "",
            vec![],
            Some(RawContent::OpenResponses(items.clone())),
        )])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    assert_eq!(
        log_lines(&dir)[0],
        r#"{"role":"assistant","raw":{"provider":"openresponses","blob":[{"type":"reasoning","id":"rs_1"}]}}"#
    );
    assert_eq!(
        store
            .load(&id, ProviderKind::OpenResponses)
            .unwrap()
            .messages[0]
            .raw_content()
            .cloned(),
        Some(RawContent::OpenResponses(items))
    );
    // A Go-written `null` blob (a nil slice) reads as the empty item list — read-only, Rust writes `[]`.
    assert_eq!(
        blob_to_raw(
            ProviderKind::OpenResponses,
            &SessionRaw {
                provider: "openresponses".to_owned(),
                blob: raw("null"),
            }
        ),
        Some(RawContent::OpenResponses(vec![]))
    );
}

/// A blob whose SHAPE does not fit the variant is dropped, reproducing Go's swallowed unmarshal error.
#[test]
fn wrongly_shaped_blobs_are_dropped() {
    let array = SessionRaw {
        provider: "openai".to_owned(),
        blob: raw(r#"[{"a":1}]"#),
    };
    assert_eq!(
        blob_to_raw(ProviderKind::OpenAi, &array),
        None,
        "an array where the assistant message object is wanted"
    );
    let object = SessionRaw {
        provider: "anthropic".to_owned(),
        blob: raw(r#"{"a":1}"#),
    };
    assert_eq!(
        blob_to_raw(ProviderKind::Anthropic, &object),
        None,
        "an object where the block array is wanted"
    );
    // A pretty-printed array is still an array.
    let spaced = SessionRaw {
        provider: "anthropic".to_owned(),
        blob: raw("  [ ]"),
    };
    assert_eq!(
        blob_to_raw(ProviderKind::Anthropic, &spaced),
        Some(RawContent::Anthropic(vec![]))
    );
}

/// A `RawContent` variant that does not belong to the writing kind is never written.
#[test]
fn mismatched_pairings_never_write() {
    let cases = [
        (ProviderKind::Anthropic, RawContent::OpenAi(raw("{}"))),
        (ProviderKind::OpenAi, RawContent::Anthropic(vec![raw("{}")])),
        (
            ProviderKind::Gemini,
            RawContent::OpenResponses(vec![raw("{}")]),
        ),
        (ProviderKind::Imagen, RawContent::Google(raw("{}"))),
    ];
    for (kind, rc) in cases {
        assert_eq!(raw_to_blob(kind, &rc), None, "{kind} must not write {rc:?}");
    }
    assert_eq!(
        raw_to_blob(ProviderKind::OpenAi, &RawContent::OpenAi(raw(r#"{"a":1}"#))),
        Some(raw(r#"{"a":1}"#))
    );
}
