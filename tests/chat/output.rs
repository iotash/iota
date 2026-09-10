//! The run report and the two output formats (`chat/output_test.go`), cancellation in JSON mode, and the image
//! file naming/modes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{path::Path, sync::Arc};

use iota::chat::images::{HOME_NOT_DEFINED, save_image, save_images_for_turn};
use iota::chat::turns::RunCtx;
use iota::chat::{
    AgentOptions, ChatError, OnceOptions, OnceOutcome, OutputFormat, RunReport, TokenUsage, once,
    parse_output_format, write_report,
};
use iota::provider::Provider;
use iota::provider::model::Attachment;
use iota::provider::usage::Usage;
use iota::testing::{FakeToolProvider, StaticDispatcher};
use iota::tool::Dispatcher;
use tokio_util::sync::CancellationToken;

// Go: chat/output_test.go:17
#[test]
fn test_parse_output_format() {
    for (input, want) in [
        ("", OutputFormat::Text),
        ("text", OutputFormat::Text),
        ("json", OutputFormat::Json),
        (" json ", OutputFormat::Json),
    ] {
        assert_eq!(parse_output_format(input).unwrap(), want, "{input:?}");
    }
    // An unknown format must not degrade to text: a caller that asked for JSON and received prose would parse
    // the prose as data.
    for input in ["stream-json", "yaml"] {
        let err = parse_output_format(input).expect_err(input);
        assert!(
            matches!(err, ChatError::BadFormat(ref s) if s == input),
            "{err}"
        );
        assert_eq!(
            err.to_string(),
            format!("unknown output format {input:?} (want text or json)")
        );
    }
}

// Go: chat/output_test.go:46
#[test]
fn test_token_usage_sum_preserves_the_absent_total() {
    // anthropic reports cache counts BESIDE input and no total, so a summed total of zero still has to mean
    // "add the parts up yourself" rather than "this run was free".
    let mut anthropic = TokenUsage::default();
    anthropic.add(Usage {
        input: 100,
        output: 20,
        cache_read: 900,
        ..Usage::default()
    });
    anthropic.add(Usage {
        input: 150,
        output: 30,
        cache_read: 1000,
        ..Usage::default()
    });
    assert_eq!(anthropic.total_tokens, 0, "the no-total dialect tell");
    assert_eq!(anthropic.cache_read_tokens, 1900);
    assert_eq!(anthropic.input_tokens, 250);
    assert_eq!(anthropic.to_usage().context_tokens(), 250 + 50 + 1900);

    let mut openai = TokenUsage::default();
    openai.add(Usage {
        input: 1000,
        output: 50,
        cache_read: 800,
        total: 1050,
        ..Usage::default()
    });
    openai.add(Usage {
        input: 1200,
        output: 60,
        cache_read: 900,
        total: 1260,
        ..Usage::default()
    });
    assert_eq!(openai.total_tokens, 2310);
    // The wire form and the core form round-trip.
    assert_eq!(TokenUsage::from(openai.to_usage()), openai);
}

/// Runs `once` in JSON mode; returns the parsed report and the run result.
async fn run_json(
    p: &mut dyn Provider,
    dispatch: Arc<dyn Dispatcher>,
    cancel: CancellationToken,
) -> (serde_json::Value, Result<OnceOutcome, ChatError>) {
    let mut buf: Vec<u8> = Vec::new();
    let res = once(
        cancel,
        p,
        dispatch,
        OnceOptions {
            message: "go".to_owned(),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
            format: OutputFormat::Json,
            images_dir: None,
            history: Vec::new(),
            jobs: None,
        },
        &mut buf,
    )
    .await;
    let text = String::from_utf8(buf).expect("utf-8");
    assert!(
        text.ends_with("}\n"),
        "one pretty object + newline: {text:?}"
    );
    let rep = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("output is not valid JSON: {e}\n{text}"));
    (rep, res)
}

// Go: chat/output_test.go:117
#[tokio::test]
async fn test_once_json_reports_every_round() {
    let mut p = FakeToolProvider::reporting(2, None);
    let (rep, res) = run_json(
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        CancellationToken::new(),
    )
    .await;
    res.expect("run failed");
    assert_eq!(rep["type"], "result");
    assert_eq!(rep["provider"], "openai");
    assert_eq!(rep["model"], "gpt-test");
    assert_eq!(rep["reply"], "final answer");
    assert!(rep.get("error").is_none(), "{rep}");
    // Two tool rounds plus the answering round.
    assert_eq!(rep["rounds"], 3);
    let rounds = rep["round_usage"].as_array().expect("round_usage");
    assert_eq!(rounds.len(), 3);
    // Usage is attributed to the round that incurred it, not smeared.
    for (i, r) in rounds.iter().enumerate() {
        let n = u64::try_from(i + 1).unwrap();
        assert_eq!(r["round"], n);
        assert_eq!(r["usage"]["input_tokens"], 100 * n, "round {n} input");
    }
    // The two tool rounds name their tool; the answering round names none.
    assert_eq!(rounds[0]["tools"], serde_json::json!(["noop"]));
    assert_eq!(rounds[1]["tools"], serde_json::json!(["noop"]));
    assert!(
        rounds[2].get("tools").is_none(),
        "final round tools = {}",
        rounds[2]
    );
    // Totals are the sum of the parts.
    assert_eq!(rep["usage"]["input_tokens"], 600);
    assert_eq!(rep["usage"]["output_tokens"], 60);
    assert_eq!(rep["usage"]["total_tokens"], 660);
    assert_eq!(rep["usage"]["cache_read_tokens"], 6);
    assert_eq!(rep["usage"]["cache_write_tokens"], 0);
    assert!(rep["duration_ms"].is_u64());
}

// Go: chat/output_test.go:158
#[tokio::test]
async fn test_once_json_reports_a_failed_run() {
    // A failed run still reports: the rounds before the failure were billed. The error travels out too.
    let mut p = FakeToolProvider::reporting(5, Some(3));
    let (rep, res) = run_json(
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        CancellationToken::new(),
    )
    .await;
    let err = res.expect_err("Once must still return the error in JSON mode");
    assert_eq!(err.to_string(), "boom");
    let text = rep["error"].as_str().expect("error");
    assert!(
        text.contains("boom"),
        "Error = {text:?}, want the upstream failure"
    );
    assert_eq!(rep["error"], "boom");
    assert_eq!(rep["reply"], "", "want empty on failure");
    // Rounds 1 and 2 completed and were paid for; round 3 never returned.
    assert_eq!(rep["rounds"], 2);
    assert_eq!(rep["usage"]["input_tokens"], 300);
    assert_eq!(rep["round_usage"].as_array().unwrap().len(), 2);
}

// Go: chat/output_test.go:180
#[tokio::test]
async fn test_once_text_mode_stays_bare() {
    // Text mode is unchanged: the reply, alone, with no report anywhere near it.
    let mut p = FakeToolProvider::reporting(1, None);
    let mut buf: Vec<u8> = Vec::new();
    once(
        CancellationToken::new(),
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        OnceOptions {
            message: "go".to_owned(),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
            format: OutputFormat::Text,
            images_dir: None,
            history: Vec::new(),
            jobs: None,
        },
        &mut buf,
    )
    .await
    .expect("run failed");
    assert_eq!(String::from_utf8(buf).unwrap(), "final answer\n");

    // On failure text mode writes NOTHING.
    let mut p = FakeToolProvider::reporting(5, Some(2));
    let mut buf: Vec<u8> = Vec::new();
    let err = once(
        CancellationToken::new(),
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        OnceOptions {
            message: "go".to_owned(),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
            format: OutputFormat::Text,
            images_dir: None,
            history: Vec::new(),
            jobs: None,
        },
        &mut buf,
    )
    .await
    .expect_err("call 2 fails");
    assert_eq!(err.to_string(), "boom");
    assert!(
        buf.is_empty(),
        "text mode wrote {:?} on failure",
        String::from_utf8_lossy(&buf)
    );

    // The unary path (no tools advertised): one round, the reply alone.
    let mut p = FakeToolProvider::reporting(0, None);
    let mut buf: Vec<u8> = Vec::new();
    once(
        CancellationToken::new(),
        &mut p,
        Arc::new(StaticDispatcher::new(&[])),
        OnceOptions {
            message: "go".to_owned(),
            system: "be brief".to_owned(),
            agent: AgentOptions::default(),
            max_turns: None,
            format: OutputFormat::Text,
            images_dir: None,
            history: Vec::new(),
            jobs: None,
        },
        &mut buf,
    )
    .await
    .expect("run failed");
    assert_eq!(String::from_utf8(buf).unwrap(), "final answer\n");
    assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

// Go: chat/output_test.go:193
#[test]
fn test_run_report_wire_names() {
    // The wire names are the contract a consumer parses against; a rename would silently break every caller.
    let rep = RunReport {
        kind: "result",
        rounds: 1,
        usage: TokenUsage {
            cache_write_tokens: 7,
            ..TokenUsage::default()
        },
        ..RunReport::default()
    };
    let mut buf: Vec<u8> = Vec::new();
    write_report(&mut buf, &rep).unwrap();
    let text = String::from_utf8(buf).unwrap();
    for key in [
        "\"type\"",
        "\"provider\"",
        "\"model\"",
        "\"reply\"",
        "\"rounds\"",
        "\"duration_ms\"",
        "\"input_tokens\"",
        "\"output_tokens\"",
        "\"cache_read_tokens\"",
        "\"cache_write_tokens\"",
        "\"total_tokens\"",
    ] {
        assert!(text.contains(key), "report is missing {key}:\n{text}");
    }
    // Omitted when absent rather than reported as empty noise.
    for key in [
        "\"error\"",
        "\"images\"",
        "\"image_errors\"",
        "\"round_usage\"",
    ] {
        assert!(
            !text.contains(key),
            "report should omit {key} when empty:\n{text}"
        );
    }
    // Go's Encoder: 2-space indent and a trailing newline.
    assert_eq!(
        text,
        "{\n  \"type\": \"result\",\n  \"provider\": \"\",\n  \"model\": \"\",\n  \"reply\": \"\",\n  \"rounds\": 1,\n  \"duration_ms\": 0,\n  \"usage\": {\n    \"input_tokens\": 0,\n    \"output_tokens\": 0,\n    \"cache_read_tokens\": 0,\n    \"cache_write_tokens\": 7,\n    \"total_tokens\": 0\n  }\n}\n"
    );
}

// New (DIVERGENCES I-03): a cancelled run fails with `interrupted`; JSON still carries the report.
#[tokio::test]
async fn cancelled_run_reports_interrupted_in_json() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    // A runaway provider: without the cancellation check the loop would never end.
    let mut p = FakeToolProvider::looping(1, 0);
    let (rep, res) = run_json(
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        cancel.clone(),
    )
    .await;
    let err = res.expect_err("a cancelled run must fail");
    assert!(matches!(err, ChatError::Interrupted), "{err}");
    assert_eq!(rep["type"], "result");
    assert_eq!(rep["error"], "interrupted");
    assert_eq!(rep["reply"], "");
    assert_eq!(rep["rounds"], 0);
    assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Any failure while the token is cancelled becomes `interrupted` (the provider's own error is replaced).
    let mut p = FakeToolProvider::reporting(5, Some(2));
    let cx = RunCtx::new(cancel.clone());
    let _ = cx;
    let (rep, res) = run_json(
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        cancel.clone(),
    )
    .await;
    assert!(matches!(res, Err(ChatError::Interrupted)));
    assert_eq!(rep["error"], "interrupted");

    // Text mode prints nothing.
    let mut p = FakeToolProvider::looping(1, 0);
    let mut buf: Vec<u8> = Vec::new();
    let err = once(
        cancel,
        &mut p,
        Arc::new(StaticDispatcher::new(&["noop"])),
        OnceOptions {
            message: "go".to_owned(),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
            format: OutputFormat::Text,
            images_dir: None,
            history: Vec::new(),
            jobs: None,
        },
        &mut buf,
    )
    .await
    .expect_err("cancelled");
    assert!(matches!(err, ChatError::Interrupted));
    assert!(buf.is_empty());
}

// New (chat/images.go:21-58,156-170): file naming, extension table, modes and the missing-home error.
#[test]
fn save_image_names_and_modes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let png = Attachment {
        filename: String::new(),
        mime_type: "image/png".to_owned(),
        data: b"\x89PNG".to_vec(),
    };
    // A nested directory is created on demand (MkdirAll).
    let nested = dir.path().join("a").join("b");
    let path = save_image(&png, Some(&nested), 3).expect("save");
    assert!(path.is_absolute());
    assert_eq!(path.parent().unwrap(), nested.as_path());
    let name = path.file_name().unwrap().to_str().unwrap();
    // <YYYYMMDD-HHMMSS>-<seq>.<ext>
    let (stamp, rest) = name.split_at(15);
    assert_eq!(rest, "-3.png", "{name}");
    assert_eq!(stamp.len(), 15);
    assert_eq!(&stamp[8..9], "-");
    assert!(stamp[..8].bytes().all(|b| b.is_ascii_digit()), "{name}");
    assert!(stamp[9..].bytes().all(|b| b.is_ascii_digit()), "{name}");
    assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o600, 0o600, "owner rw: {mode:o}");
        assert_eq!(mode & 0o133, 0, "0644 minus umask: {mode:o}");
        let dmode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode & 0o700, 0o700, "dir 0755 minus umask: {dmode:o}");
    }

    // The extension follows the MIME type; unknown types get `.bin`; seq is the index in the batch.
    let att = |mime: &str| Attachment {
        filename: String::new(),
        mime_type: mime.to_owned(),
        data: vec![1],
    };
    let saved = save_images_for_turn(
        &[
            att("image/jpeg"),
            att("image/webp"),
            att("image/gif"),
            att("application/octet-stream"),
        ],
        Some(dir.path()),
    );
    assert!(saved.failures.is_empty(), "{:?}", saved.failures);
    let names: Vec<String> = saved
        .paths
        .iter()
        .map(|p| {
            let n = Path::new(p).file_name().unwrap().to_str().unwrap();
            n[15..].to_owned()
        })
        .collect();
    assert_eq!(names, ["-0.jpg", "-1.webp", "-2.gif", "-3.bin"]);
    // The saved subset carries the BASENAME of each saved file and the original mime/data (images.go:131).
    assert_eq!(saved.attachments.len(), 4);
    for (att, path) in saved.attachments.iter().zip(&saved.paths) {
        assert_eq!(
            att.filename,
            Path::new(path).file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(att.data, vec![1]);
    }
    assert_eq!(saved.attachments[0].mime_type, "image/jpeg");
    assert_eq!(saved.attachments[3].mime_type, "application/octet-stream");

    // No home directory: every image fails with Go's os.UserHomeDir text, and nothing is written.
    assert_eq!(save_image(&png, None, 0).unwrap_err(), HOME_NOT_DEFINED);
    let saved = save_images_for_turn(&[png.clone(), png], None);
    assert!(saved.paths.is_empty());
    // A failed save is reported and NOT attached (S§3): nothing to persist.
    assert!(saved.attachments.is_empty());
    assert_eq!(
        saved.failures,
        [
            "saving image failed: $HOME is not defined",
            "saving image failed: $HOME is not defined"
        ]
    );

    // Text mode prints the saved paths as `🖼 saved: <path>` lines after the reply.
    let img = Attachment {
        filename: String::new(),
        mime_type: "image/png".to_owned(),
        data: b"x".to_vec(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut buf: Vec<u8> = Vec::new();
    rt.block_on(async {
        let mut p = FakeToolProvider::scripted(
            vec![iota::provider::RoundResult {
                content: String::new(),
                images: vec![img],
                ..iota::provider::RoundResult::default()
            }],
            "",
        );
        once(
            CancellationToken::new(),
            &mut p,
            Arc::new(StaticDispatcher::new(&[])),
            OnceOptions {
                message: "draw".to_owned(),
                system: String::new(),
                agent: AgentOptions::default(),
                max_turns: None,
                format: OutputFormat::Text,
                images_dir: Some(dir.path().to_path_buf()),
                history: Vec::new(),
                jobs: None,
            },
            &mut buf,
        )
        .await
        .expect("run failed");
    });
    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("🖼 saved: "), "{text:?}");
    assert!(!text.starts_with('\n'), "no blank reply line: {text:?}");
    let saved = text.trim_end_matches('\n').trim_start_matches("🖼 saved: ");
    assert_eq!(std::fs::read(saved).unwrap(), b"x");
    assert_eq!(text.as_bytes()[..4], [0xF0, 0x9F, 0x96, 0xBC]);
}
