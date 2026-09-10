//! The append lifecycle (`chat/session_test.go`): lazy bundle creation, the round trip through
//! `messages.jsonl`, the counters, the attachment store and compaction markers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::ProviderKind;
use iota::provider::model::{Attachment, JsonObject, Message, Raw, RawContent, Role, ToolCall};
use iota::provider::usage::Usage;
use iota::session::ATTACHMENTS_DIR;
use pretty_assertions::assert_eq;

use crate::common::{log_lines, temp_store};

const KIND: ProviderKind = ProviderKind::OpenAi;

fn call(id: &str, name: &str, args: &[(&str, serde_json::Value)]) -> ToolCall {
    let mut arguments = JsonObject::new();
    for (k, v) in args {
        arguments.insert((*k).to_owned(), v.clone());
    }
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

// Go: chat/session_test.go:38
#[test]
fn session_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();

    let raw = RawContent::OpenAi(Raw::from_string(r#"{"sig":"abc"}"#.to_owned()).unwrap());
    let calls = vec![call("c1", "tool", &[("x", serde_json::Value::from(1))])];
    let msgs = vec![
        Message::system("sys"),
        Message {
            attachments: vec![Attachment {
                filename: "a.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                data: b"hello".to_vec(),
            }],
            ..Message::user("hi")
        },
        Message::assistant_with_calls("", calls.clone(), Some(raw)),
        Message::tool_result(&calls[0], "result", false),
        Message::assistant("done"),
    ];
    writer.append_messages(&msgs).unwrap();
    drop(writer);

    let sess = store.load(&id, KIND).unwrap();
    assert_eq!(sess.messages.len(), msgs.len());

    // Attachment bytes restored.
    let user = &sess.messages[1];
    assert_eq!(user.attachments.len(), 1);
    assert_eq!(user.attachments[0].data, b"hello");
    assert_eq!(user.attachments[0].filename, "a.txt");
    assert_eq!(user.attachments[0].mime_type, "text/plain");

    // Tool call preserved, arguments included.
    let assistant = &sess.messages[2];
    assert_eq!(assistant.tool_calls().len(), 1);
    assert_eq!(assistant.tool_calls()[0].id, "c1");
    assert_eq!(assistant.tool_calls()[0].name, "tool");
    assert_eq!(
        assistant.tool_calls()[0].arguments.get("x"),
        Some(&serde_json::Value::from(1))
    );
    // RawContent restored (same provider type).
    assert_eq!(
        assistant.raw_content().cloned(),
        Some(RawContent::OpenAi(
            Raw::from_string(r#"{"sig":"abc"}"#.to_owned()).unwrap()
        ))
    );

    // Tool result fields preserved.
    let tool = &sess.messages[3];
    assert_eq!(tool.role(), Role::Tool);
    assert_eq!(tool.tool_call_id(), "c1");
    assert_eq!(tool.content, "result");

    // Listing reports the session with the right message count.
    let infos = store.list(None).unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, id);
    assert_eq!(infos[0].message_count, i64::try_from(msgs.len()).unwrap());
}

// Go: chat/session_test.go:111
#[test]
fn session_usage_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();

    writer
        .append_messages(&[
            Message::user("u1"),
            Message::assistant("a1").with_usage(Some(Usage {
                input: 1000,
                output: 200,
                ..Usage::default()
            })),
        ])
        .unwrap();
    // The summary pass is a billed call of its own; the marker carries it.
    writer
        .append_compaction(
            "SUMMARY",
            0,
            Some(Usage {
                input: 500,
                output: 50,
                ..Usage::default()
            }),
        )
        .unwrap();
    writer
        .append_messages(&[
            Message::user("u2"),
            Message::assistant("a2").with_usage(Some(Usage {
                input: 1500,
                output: 300,
                ..Usage::default()
            })),
        ])
        .unwrap();

    let want = Usage {
        input: 3000,
        output: 550,
        ..Usage::default()
    };
    assert_eq!(writer.usage(), want);
    drop(writer);

    let (resumed, sess) = store.resume(&id, KIND).unwrap();
    assert_eq!(
        sess.usage, want,
        "compacted-away rounds and the marker count too"
    );
    assert_eq!(resumed.usage(), want);
    let last = sess.messages.last().unwrap();
    assert_eq!(
        last.usage(),
        Some(Usage {
            input: 1500,
            output: 300,
            ..Usage::default()
        }),
        "per-message usage lost in the round trip"
    );
    // Records written before usage existed contribute nothing rather than breaking the load.
    assert_eq!(sess.messages[0].usage(), None);
}

// Go: chat/session_test.go:167
#[test]
fn interrupted_flag_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    writer
        .append_messages(&[
            Message::user("hi"),
            Message::assistant("partial reply")
                .with_reasoning("partial thinking".to_owned())
                .with_interrupted(true),
        ])
        .unwrap();
    drop(writer);

    let sess = store.load(&id, KIND).unwrap();
    assert_eq!(sess.messages.len(), 2);
    let assistant = &sess.messages[1];
    assert!(assistant.interrupted(), "interrupted flag not restored");
    assert_eq!(assistant.content, "partial reply");
    assert_eq!(assistant.reasoning(), "partial thinking");
    // The flag stays off for ordinary messages (omitempty both ways).
    assert!(!sess.messages[0].interrupted());
}

// Go: chat/session_test.go:368
#[test]
fn lazy_session_creation() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    let dir = writer.dir().to_path_buf();

    // In-memory updates (Go's /model before any turn) must not create the bundle.
    writer
        .update_meta(|meta| {
            meta.model = "m2".to_owned();
            meta.title = "draft".to_owned();
        })
        .unwrap();
    assert!(!writer.on_disk());
    assert!(store.list(None).unwrap().is_empty());
    assert!(!dir.exists(), "session dir exists before the first append");

    // The first real append materialises the bundle with the pending model/title.
    writer.append_messages(&[Message::user("hi")]).unwrap();
    assert!(writer.on_disk());
    drop(writer);

    let infos = store.list(None).unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, id);
    assert_eq!(infos[0].model, "m2");
    assert_eq!(infos[0].title, "draft");
    // `attachments/` is created UNCONDITIONALLY, even with no attachment in sight (Go parity).
    assert!(dir.join(ATTACHMENTS_DIR).is_dir());
}

/// An empty batch writes nothing at all — it must not even materialise the bundle.
#[test]
fn empty_batch_is_a_no_op() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let dir = writer.dir().to_path_buf();
    writer.append_messages(&[]).unwrap();
    assert!(!writer.on_disk());
    assert!(!dir.exists());
}

/// Identical attachment bytes are stored once (content-addressed dedup) and both records point at it.
#[test]
fn attachments_are_deduplicated() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let dir = writer.dir().to_path_buf();
    let att = |name: &str| Attachment {
        filename: name.to_owned(),
        mime_type: "text/plain".to_owned(),
        data: b"hello".to_vec(),
    };
    writer
        .append_messages(&[
            Message {
                attachments: vec![att("a.txt")],
                ..Message::user("one")
            },
            Message {
                attachments: vec![att("b.txt")],
                ..Message::user("two")
            },
        ])
        .unwrap();
    drop(writer);

    let stored: Vec<_> = std::fs::read_dir(dir.join(ATTACHMENTS_DIR))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        stored,
        vec!["2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"],
        "sha256(\"hello\") stored once"
    );
    // Both lines keep their own filename but share the one data_ref.
    let lines = log_lines(&dir);
    assert!(lines[0].contains(r#""filename":"a.txt""#));
    assert!(lines[1].contains(r#""filename":"b.txt""#));
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains(
                r#""data_ref":"sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824""#
            ))
            .count(),
        2
    );
}

/// `message_count` counts every record including system messages; `conv_count` (visible through the
/// marker index) excludes them, and a compaction marker bumps NEITHER counter.
#[test]
fn compaction_marker_bumps_no_counter() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    writer
        .append_messages(&[
            Message::system("sys"),
            Message::user("u1"),
            Message::assistant("a1"),
        ])
        .unwrap();
    assert_eq!(writer.meta().message_count, 3);

    // conv_count is 2 (system excluded), so retain_tail 0 supersedes both.
    writer.append_compaction("SUMMARY", 0, None).unwrap();
    assert_eq!(
        writer.meta().message_count,
        3,
        "the marker must not count as a message"
    );
    let dir = writer.dir().to_path_buf();
    drop(writer);

    let lines = log_lines(&dir);
    assert_eq!(lines.len(), 4);
    assert_eq!(
        lines[3],
        r#"{"role":"compaction","content":"SUMMARY","compacted_through":2}"#
    );

    // A second marker after one more round indexes from the FULL conversation length, not the view.
    let (mut writer, _) = store.resume(&id, KIND).unwrap();
    writer.append_messages(&[Message::user("u2")]).unwrap();
    writer.append_compaction("AGAIN", 1, None).unwrap();
    drop(writer);
    let lines = log_lines(&dir);
    assert_eq!(
        lines[5], r#"{"role":"compaction","content":"AGAIN","compacted_through":2}"#,
        "conv_count 3 minus retain_tail 1"
    );
}

/// `compacted_through` is clamped at zero: retaining more than exists is not negative.
#[test]
fn compaction_through_never_goes_negative() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let dir = writer.dir().to_path_buf();
    writer.append_messages(&[Message::user("u1")]).unwrap();
    writer.append_compaction("SUMMARY", 10, None).unwrap();
    drop(writer);
    // `compacted_through: 0` is omitted by omitempty, exactly like Go.
    assert_eq!(
        log_lines(&dir)[1],
        r#"{"role":"compaction","content":"SUMMARY"}"#
    );
}

/// `update_meta` on a resumed (already created) writer writes through immediately.
#[test]
fn update_meta_writes_through_once_created() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    writer.append_messages(&[Message::user("hi")]).unwrap();
    drop(writer);

    let (mut writer, _) = store.resume(&id, KIND).unwrap();
    writer
        .update_meta(|meta| meta.title = "renamed".to_owned())
        .unwrap();
    drop(writer);
    assert_eq!(store.load(&id, KIND).unwrap().meta.title, "renamed");
}

/// `images_path` has no side effects; `images_dir` creates the bundle and the directory.
#[test]
fn images_dir_is_created_on_demand() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let path = writer.images_path();
    assert_eq!(path, writer.dir().join("images"));
    assert!(!path.exists(), "images_path must not touch disk");
    assert!(!writer.on_disk());

    let made = writer.images_dir().expect("images dir");
    assert_eq!(made, path);
    assert!(made.is_dir());
    assert!(writer.on_disk(), "images_dir materialises the bundle");
}

// Go: chat/session_test.go:518 TestDeferredSaveBacklog — the store-level half of the `/save` flow
// for an ephemeral chat: the writer is minted only when the user saves, the WHOLE accumulated
// backlog lands in ONE append (the watermark never moved while `writer` was `None`), the title and
// the window the command re-stamps ride along, and the session resumes losslessly.
//
// The run-loop half — that `/save` mints late and flushes exactly this backlog — is
// `tests/repl/commands.rs::save_mints_late_and_flushes_the_backlog`.
#[test]
fn test_deferred_save_backlog() {
    let (_home, store) = temp_store();

    // Several turns accumulate in memory with NO writer: every persist was a no-op.
    let backlog = vec![
        Message::user("explore this"),
        Message::assistant("sure"),
        Message::user("turned out valuable"),
        Message::assistant("saving then"),
    ];

    // /save: mint + append everything since watermark 0 + the custom title.
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let dir = writer.dir().to_path_buf();
    assert!(!writer.on_disk(), "creating a writer must not touch disk");
    writer.append_messages(&backlog).unwrap();
    writer
        .update_meta(|m| m.title = "keeper".to_owned())
        .unwrap();
    writer.update_meta(|m| m.context_window = 128_000).unwrap();
    let id = writer.id().to_owned();
    drop(writer);

    // One append: four records, in order, no gaps.
    let lines = log_lines(&dir);
    assert_eq!(lines.len(), backlog.len());

    let (_writer, sess) = store.resume(&id, KIND).unwrap();
    assert_eq!(
        sess.messages.len(),
        backlog.len(),
        "resumed {} messages, want {}",
        sess.messages.len(),
        backlog.len()
    );
    let contents: Vec<&str> = sess.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        ["explore this", "sure", "turned out valuable", "saving then"],
        "backlog order lost"
    );
    assert_eq!(sess.meta.title, "keeper");
    assert_eq!(sess.meta.context_window, 128_000);
}
