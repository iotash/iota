//! Reading a bundle back: the compaction weave, the LOSSLESS `/export` history, log-wide usage, and
//! every tolerance `bufio.Scanner` gives Go (`chat/session.go:813-941`, `chat/session_test.go:410`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use iota::provider::ProviderKind;
use iota::provider::model::{Message, Role};
use iota::provider::usage::Usage;
use iota::session::{
    ATTACHMENTS_DIR, LOG_FILE, MAX_LOG_LINE, SessionError, SessionRecord, load_full_history,
    load_log, record_to_message, scan_records, summary_preamble,
};
use pretty_assertions::assert_eq;

use crate::common::{bucket_dir, temp_store, write_bundle};

const KIND: ProviderKind = ProviderKind::OpenAi;

/// Writes `lines` as a bundle log under a fresh temp directory.
fn log_dir(lines: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(ATTACHMENTS_DIR)).unwrap();
    std::fs::write(dir.path().join(LOG_FILE), lines).unwrap();
    dir
}

/// Every record `scan_records` hands over, in order.
fn records(dir: &Path) -> Vec<SessionRecord> {
    let mut out = Vec::new();
    scan_records(dir, &mut |rec| out.push(rec)).unwrap();
    out
}

// Go: chat/session_test.go:410 (adapted — `LoadFullHistory` is /export-only, so the weave half is what
// this pins)
#[test]
fn load_log_weaves_the_last_compaction() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    writer
        .append_messages(&[
            Message::user("first question"),
            Message::assistant("first answer"),
        ])
        .unwrap();
    writer.append_compaction("SUMMARY", 0, None).unwrap();
    writer
        .append_messages(&[
            Message::user("second question"),
            Message::assistant("second answer"),
        ])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    // Four conversation records are on disk; the view keeps only the retained tail...
    let log = load_log(&dir, KIND).unwrap();
    assert_eq!(
        log.conv_count, 4,
        "conv_count is the FULL log, not the view"
    );
    let contents: Vec<&str> = log.view.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        [
            &format!("{}second question", summary_preamble("SUMMARY")) as &str,
            "second answer",
        ]
    );
    // ...and the pre-compaction round is untouched on disk (lines are never rewritten).
    assert_eq!(records(&dir).len(), 5);
    assert_eq!(store.load(&id, KIND).unwrap().messages.len(), 2);
}

/// The LAST marker wins, `through` is clamped, and a marker that supersedes everything appends the
/// synthetic user message instead of prepending to nothing.
#[test]
fn compaction_edges() {
    // Nothing retained → the synthetic {role: user, content: preamble}.
    let dir = log_dir(concat!(
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "{\"role\":\"compaction\",\"content\":\"S\",\"compacted_through\":1}\n"
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    assert_eq!(log.view.len(), 1);
    assert_eq!(log.view[0].role(), Role::User);
    assert_eq!(log.view[0].content, summary_preamble("S"));
    assert_eq!(log.conv_count, 1);

    // `through` beyond the conversation length is clamped, not a panic.
    let dir = log_dir(concat!(
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "{\"role\":\"compaction\",\"content\":\"S\",\"compacted_through\":99}\n"
    ));
    assert_eq!(load_log(dir.path(), KIND).unwrap().view.len(), 1);

    // Two markers: the LAST one wins.
    let dir = log_dir(concat!(
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "{\"role\":\"compaction\",\"content\":\"OLD\",\"compacted_through\":1}\n",
        "{\"role\":\"user\",\"content\":\"u2\"}\n",
        "{\"role\":\"compaction\",\"content\":\"NEW\",\"compacted_through\":1}\n"
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    assert_eq!(
        log.view[0].content,
        format!("{}u2", summary_preamble("NEW"))
    );
}

/// The LAST system record wins and is placed FIRST, whatever its position in the log.
#[test]
fn last_system_record_wins_and_leads_the_view() {
    let dir = log_dir(concat!(
        "{\"role\":\"system\",\"content\":\"old system\"}\n",
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "{\"role\":\"system\",\"content\":\"new system\"}\n",
        "{\"role\":\"assistant\",\"content\":\"a1\"}\n"
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    let view: Vec<(Role, &str)> = log
        .view
        .iter()
        .map(|m| (m.role(), m.content.as_str()))
        .collect();
    assert_eq!(
        view,
        [
            (Role::System, "new system"),
            (Role::User, "u1"),
            (Role::Assistant, "a1"),
        ]
    );
    assert_eq!(log.conv_count, 2, "system records are not conversation");
}

/// Usage is summed over EVERY record, markers and compacted-away rounds included.
#[test]
fn usage_sums_the_whole_log_including_markers() {
    let dir = log_dir(concat!(
        "{\"role\":\"assistant\",\"content\":\"a1\",\"usage\":{\"in\":1000,\"out\":200,\"cache_read\":5,\"total\":1200}}\n",
        "{\"role\":\"compaction\",\"content\":\"S\",\"compacted_through\":1,\"usage\":{\"in\":50,\"out\":20}}\n",
        "{\"role\":\"assistant\",\"content\":\"a2\",\"usage\":{\"in\":10,\"out\":5}}\n"
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    assert_eq!(
        log.usage,
        Usage {
            input: 1060,
            output: 225,
            cache_read: 5,
            cache_write: 0,
            total: 1200,
        }
    );
    // The first round is compacted away but its cost is still counted.
    assert_eq!(log.view.len(), 1);
}

/// Blank lines, a corrupt line and a crash-truncated tail are all tolerated (chat/session.go:822-831).
#[test]
fn blank_corrupt_and_truncated_lines_are_tolerated() {
    let dir = log_dir(concat!(
        "\n",
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "   \n",
        "not json at all\n",
        "{\"role\":\"assistant\",\"content\":\"a1\"}\n",
        "{\"role\":\"user\",\"content\":\"trunc" // no closing brace, no newline
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    let contents: Vec<&str> = log.view.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, ["u1", "a1"]);
    assert_eq!(log.conv_count, 2);

    // A complete final line WITHOUT its newline is still a record.
    let dir = log_dir("{\"role\":\"user\",\"content\":\"last\"}");
    assert_eq!(load_log(dir.path(), KIND).unwrap().view.len(), 1);
}

/// A record whose role is neither a known role nor `compaction` is SKIPPED (D-47).
#[test]
fn unknown_roles_are_skipped() {
    let dir = log_dir(concat!(
        "{\"role\":\"user\",\"content\":\"u1\"}\n",
        "{\"role\":\"weird\",\"content\":\"from the future\"}\n",
        "{\"role\":\"\",\"content\":\"no role at all\"}\n"
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    assert_eq!(log.view.len(), 1);
    assert_eq!(log.conv_count, 1);
    // ...but the record is still SEEN by the scanner, so its usage would still count.
    assert_eq!(records(dir.path()).len(), 3);
}

/// `record_to_message` refuses a compaction marker — unreachable from `load_log`, which consumes markers
/// before conversion, and pinned so no direct caller can leak one into a prompt (CONTRACTS S§2.7).
#[test]
fn record_to_message_rejects_markers_and_unknown_roles() {
    let dir = tempfile::tempdir().unwrap();
    let marker = SessionRecord {
        role: "compaction".to_owned(),
        content: "SUMMARY".to_owned(),
        compacted_through: 2,
        ..SessionRecord::default()
    };
    assert_eq!(record_to_message(&marker, dir.path(), KIND), None);
    let weird = SessionRecord {
        role: "weird".to_owned(),
        ..SessionRecord::default()
    };
    assert_eq!(record_to_message(&weird, dir.path(), KIND), None);
    // Each known role converts.
    for role in ["system", "user", "assistant", "tool"] {
        let rec = SessionRecord {
            role: role.to_owned(),
            ..SessionRecord::default()
        };
        assert!(
            record_to_message(&rec, dir.path(), KIND).is_some(),
            "{role}"
        );
    }
}

/// A missing attachment file, and a `data_ref` without the `sha256:` prefix, are skipped — the MESSAGE is
/// kept (chat/session.go:784-792).
#[test]
fn broken_attachment_refs_are_skipped_and_the_message_kept() {
    let dir = log_dir(concat!(
        r#"{"role":"user","content":"missing file","attachments":[{"filename":"a.txt","mime":"text/plain","data_ref":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}]}"#,
        "\n",
        r#"{"role":"user","content":"bad prefix","attachments":[{"filename":"b.txt","mime":"text/plain","data_ref":"deadbeef"}]}"#,
        "\n",
        r#"{"role":"user","content":"empty digest","attachments":[{"filename":"c.txt","mime":"text/plain","data_ref":"sha256:"}]}"#,
        "\n",
        r#"{"role":"user","content":"traversal","attachments":[{"filename":"d.txt","mime":"text/plain","data_ref":"sha256:../../etc/hosts"}]}"#,
        "\n",
    ));
    let log = load_log(dir.path(), KIND).unwrap();
    assert_eq!(log.view.len(), 4, "every message survives");
    for msg in &log.view {
        assert!(
            msg.attachments.is_empty(),
            "{:?} kept an unreadable attachment",
            msg.content
        );
    }
}

/// A line that REACHES the 32 MiB cap aborts the scan with Go's `read session log: …` frame, and the cap
/// fires while reading so nothing near that size is ever allocated (D-56).
#[test]
fn oversized_line_aborts_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(LOG_FILE);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"{\"role\":\"user\",\"content\":\"ok\"}\n")
            .unwrap();
        // One hostile line of exactly MAX_LOG_LINE bytes, written in chunks.
        f.write_all(b"{\"role\":\"user\",\"content\":\"").unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut written = 26;
        while written < MAX_LOG_LINE {
            let take = chunk.len().min(MAX_LOG_LINE - written);
            f.write_all(&chunk[..take]).unwrap();
            written += take;
        }
        f.write_all(b"\n").unwrap();
    }
    let err = load_log(dir.path(), KIND).unwrap_err();
    assert!(matches!(err, SessionError::ReadLog(_)));
    assert!(
        err.to_string().starts_with("read session log: "),
        "got {err}"
    );

    // One byte under the cap is still a valid (if unparsable) line and the scan completes.
    std::fs::write(&path, format!("{}\n", "x".repeat(MAX_LOG_LINE - 1))).unwrap();
    assert_eq!(load_log(dir.path(), KIND).unwrap().view.len(), 0);
}

/// A bundle with no `messages.jsonl` at all surfaces the open error, exactly like Go's `os.Open`.
#[test]
fn missing_log_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_log(dir.path(), KIND).unwrap_err();
    assert!(matches!(err, SessionError::Io(_)), "got {err:?}");
}

// Go: chat/session_test.go:410 TestLoadFullHistoryIgnoresCompaction — the FULL log is every
// conversation record on disk, including the rounds a compaction marker hides from the
// `load_log` view, with the marker itself skipped. (`SessionStore::load_full` is the id-taking
// twin of Go's `LoadFullHistory`, which resolves the directory the same way.)
#[test]
fn load_full_history_ignores_compaction() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();

    writer
        .append_messages(&[
            Message::user("first question"),
            Message::assistant("first answer"),
        ])
        .unwrap();
    writer.append_compaction("SUMMARY", 0, None).unwrap();
    writer
        .append_messages(&[
            Message::user("second question"),
            Message::assistant("second answer"),
        ])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    let view = store.load(&id, KIND).unwrap();
    let full = store.load_full(&id, KIND).unwrap();
    // The bare-directory half of the pair — Go's `LoadFullHistory` body after `sessionDir` — is
    // the same history; `SessionStore::load_full` only adds the locator.
    assert_eq!(load_full_history(&dir, KIND).unwrap(), full);
    assert_eq!(full.len(), 4, "full history: {full:#?}");
    assert!(
        full.len() > view.messages.len(),
        "full history ({}) should exceed the compacted view ({})",
        full.len(),
        view.messages.len()
    );
    // The pre-compaction round is intact and unweaved (no summary preamble).
    assert_eq!(full[0].content, "first question");
    assert_eq!(full[1].content, "first answer");
    // Every round is there, in append order, and the marker itself is skipped.
    assert_eq!(
        full.iter()
            .map(|m| (m.role(), m.content.as_str()))
            .collect::<Vec<_>>(),
        [
            (Role::User, "first question"),
            (Role::Assistant, "first answer"),
            (Role::User, "second question"),
            (Role::Assistant, "second answer"),
        ]
    );
    for m in &full {
        assert!(
            !m.content.contains("SUMMARY"),
            "compaction marker leaked into full history: {m:#?}"
        );
    }
}

// Go: chat/session_project_test.go:140 — `LoadFullHistory` resolves a BUCKETED id through the
// same locator `load`/`resume` use, so an agent-mode session exports like any other.
#[test]
fn load_full_history_finds_a_bucketed_id() {
    let (_home, store) = temp_store();
    let root = "/work/proj";
    let flat_id = "aaaa00000000";
    let bucket_id = "aaab00000000";
    write_bundle(&store.root().join(flat_id), flat_id, "");
    write_bundle(
        &bucket_dir(store.root(), root).join(bucket_id),
        bucket_id,
        root,
    );

    for id in [flat_id, bucket_id] {
        let full = store.load_full(id, KIND).unwrap();
        assert_eq!(full.len(), 1, "{id}: {full:#?}");
        assert_eq!(full[0].content, "hi");
    }
    // An unknown id errors through the locator rather than yielding an empty history.
    assert!(matches!(
        store.load_full("zzzz00000000", KIND).unwrap_err(),
        SessionError::NotFound(_)
    ));
}

// New: the full history shares `record_to_message` with the view, so attachments and raw content
// are restored identically, an unknown role is SKIPPED like a corrupt line (D-47), and system
// records keep their append position (there is no weaving here).
#[test]
fn load_full_history_shares_the_record_decoder() {
    let (_home, store) = temp_store();
    let mut writer = store.create(KIND, "m1", None, "", "", false, "").unwrap();
    let id = writer.id().to_owned();
    writer.append_messages(&[Message::user("u1")]).unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    // Append a corrupt role, a marker and a system record by hand — the writer refuses to.
    let mut log = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
    log.push_str(concat!(
        "{\"role\":\"martian\",\"content\":\"?\"}\n",
        "{\"role\":\"compaction\",\"content\":\"S\",\"compacted_through\":1}\n",
        "{\"role\":\"system\",\"content\":\"sys\"}\n",
        "{\"role\":\"assistant\",\"content\":\"a1\"}\n"
    ));
    std::fs::write(dir.join(LOG_FILE), log).unwrap();

    assert_eq!(
        store
            .load_full(&id, KIND)
            .unwrap()
            .iter()
            .map(|m| (m.role(), m.content.as_str()))
            .collect::<Vec<_>>(),
        [
            (Role::User, "u1"),
            (Role::System, "sys"),
            (Role::Assistant, "a1"),
        ]
    );
}

// New: a bundle whose log has gone missing surfaces the open error, exactly like `load_log` —
// `/export` prints it as `Error: …` instead of writing an empty document.
#[test]
fn load_full_history_reports_a_missing_log() {
    let (_home, store) = temp_store();
    let id = "aaaa00000000";
    write_bundle(&store.root().join(id), id, "");
    std::fs::remove_file(store.root().join(id).join(LOG_FILE)).unwrap();
    let err = store.load_full(id, KIND).unwrap_err();
    assert!(matches!(err, SessionError::Io(_)), "got {err:?}");
}
