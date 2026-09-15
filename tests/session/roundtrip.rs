//! The store's round trip: create → append → compact → resume → load, over every record shape a real
//! session produces (`iota::testing::every_record_shape`, the list `examples/mkbundle.rs` writes as the
//! format's smoke sample). What is pinned is that the bundle hands back exactly what went in — the
//! attachment's bytes, the raw payload under its own tag, usage, reasoning, the interrupted flag — and
//! that a compaction marker changes the VIEW without touching the full history.

use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::session::{NewSession, summary_preamble};
use iota::testing::every_record_shape;
use pretty_assertions::assert_eq;

use crate::common::{log_lines, temp_store};

const KIND: ProviderKind = ProviderKind::OpenAi;

#[test]
fn every_record_shape_survives_the_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store
        .create(NewSession {
            temperature: Some(0.7),
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            cwd: "/tmp/roundtrip-cwd".to_owned(),
            ..NewSession::new(KIND, "gpt-probe")
        })
        .unwrap();
    let id = writer.id().to_owned();
    let shapes = every_record_shape();
    writer.append_messages(&shapes).unwrap();

    // Load: the view IS the appended list, shape for shape.
    let loaded = store.load(&id, KIND).unwrap();
    assert_eq!(loaded.messages, shapes);
    assert_eq!(loaded.meta.model, "gpt-probe");
    assert_eq!(loaded.meta.temperature, Some(0.7));
    assert_eq!(loaded.meta.base_url, "http://127.0.0.1:1/v1");
    assert_eq!(loaded.meta.cwd, "/tmp/roundtrip-cwd");
    assert_eq!(loaded.meta.message_count, 6);
    assert_eq!(
        (loaded.usage.input, loaded.usage.output),
        (1010, 205),
        "the log's usage is the sum of every assistant record's"
    );

    // Compact after one more round, keeping that round: the view is the system message, then the summary
    // woven into the retained round; the full history still has everything; the marker is one more line
    // on disk.
    let again = [Message::user("again"), Message::assistant("again answer")];
    writer.append_messages(&again).unwrap();
    writer.append_compaction("SUMMARY", 2, None).unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);
    assert_eq!(log_lines(&dir).len(), 9, "6 shapes + 2 + the marker");

    let (mut resumed, session) = store.resume(&id, KIND).unwrap();
    assert_eq!(
        session
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        [
            "sys",
            format!("{}again", summary_preamble("SUMMARY")).as_str(),
            "again answer",
        ]
    );
    let mut full = shapes.clone();
    full.extend(again.iter().cloned());
    assert_eq!(store.load_full(&id, KIND).unwrap(), full);

    // The resumed writer continues the same log: one more record lands after the marker and the full
    // history grows by exactly it.
    resumed
        .append_messages(&[Message::user("after resume")])
        .unwrap();
    full.push(Message::user("after resume"));
    assert_eq!(store.load_full(&id, KIND).unwrap(), full);
    assert_eq!(log_lines(&dir).len(), 10);
}
