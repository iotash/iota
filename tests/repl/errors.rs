//! `describe_error` classification + the transcript `errorBlock` idiom
//! (`chat/errors_test.go` ports).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use iota::chat::ChatError;
use iota::llm::{LlmError, StatusError};
use iota::provider::error::{ProviderError, WireOp};
use iota::repl::{ErrorReport, Transcript, describe_error};
use iota::testing::{ScriptedUi, UiEvent};
use pretty_assertions::assert_eq;

fn status_err(status: u16, body: &str) -> LlmError {
    LlmError::Status(StatusError {
        status,
        status_text: "Status Text".to_owned(),
        method: "POST".to_owned(),
        url: "https://api.example.com/v1/x".to_owned(),
        body: body.to_owned(),
    })
}

/// A wire failure as the chat layer sees it.
fn wire(e: LlmError) -> ChatError {
    ChatError::Provider(ProviderError::wire(WireOp::Stream, e))
}

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

// Go: chat/errors_test.go:19 TestDescribeError — byte-exact headlines/details/hints;
// wire errors get a status-class headline plus the envelope's message, never the raw
// URL/JSON dump.
#[test]
fn test_describe_error() {
    struct Case {
        name: &'static str,
        err: ChatError,
        headline: &'static str,
        detail: String,
        hint: &'static str,
    }
    let cases = [
        Case {
            name: "rate limit with openai envelope",
            err: wire(status_err(
                429,
                r#"{"message":"Rate limit reached for gpt-4o","type":"tokens","code":"rate_limit_exceeded"}"#,
            )),
            headline: "Rate limited (429)",
            detail: "Rate limit reached for gpt-4o".to_owned(),
            hint: "",
        },
        Case {
            name: "auth failure hints at the key",
            err: wire(status_err(
                401,
                r#"{"message":"Incorrect API key provided"}"#,
            )),
            headline: "Authentication failed (401)",
            detail: "Incorrect API key provided".to_owned(),
            hint: "Check the API key for this provider",
        },
        Case {
            name: "context overflow reroutes to /compact",
            err: wire(status_err(
                400,
                r#"{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}"#,
            )),
            headline: "Context window exceeded (400)",
            detail: "This model's maximum context length is 8192 tokens".to_owned(),
            hint: "Try /compact to shrink the conversation",
        },
        Case {
            name: "anthropic prompt-too-long phrasing",
            err: wire(status_err(
                400,
                r#"{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}"#,
            )),
            headline: "Context window exceeded (400)",
            detail: "prompt is too long: 210000 tokens > 200000 maximum".to_owned(),
            hint: "Try /compact to shrink the conversation",
        },
        Case {
            name: "non-JSON body falls back verbatim",
            err: wire(status_err(502, "upstream connect error")),
            headline: "Provider server error (502)",
            detail: "upstream connect error".to_owned(),
            hint: "",
        },
        Case {
            name: "bare string error value",
            err: wire(status_err(400, r#""invalid request""#)),
            headline: "Request rejected (400 Status Text)",
            detail: "invalid request".to_owned(),
            hint: "",
        },
        Case {
            name: "nested envelope from a proxy",
            err: wire(status_err(
                404,
                r#"{"error":{"message":"model x does not exist"}}"#,
            )),
            headline: "Not found (404)",
            detail: "model x does not exist".to_owned(),
            hint: "Check the model name (/model) and base URL",
        },
        Case {
            name: "permanent failure keeps its text",
            err: ChatError::Provider(ProviderError::permanent_msg("quota")),
            headline: "Request failed",
            detail: "quota".to_owned(),
            hint: "",
        },
        Case {
            name: "no SSE events",
            err: wire(LlmError::NoEvents),
            headline: "Provider did not stream",
            detail: format!("stream error: {}", LlmError::NoEvents),
            hint: "",
        },
        Case {
            name: "plain error",
            err: ChatError::Io(std::io::Error::other("tool rounds exceeded")),
            headline: "Request failed",
            detail: "tool rounds exceeded".to_owned(),
            hint: "",
        },
    ];
    for c in cases {
        let r = describe_error(&c.err);
        assert_eq!(r.headline, c.headline, "{}: headline", c.name);
        assert_eq!(r.detail.join("\n"), c.detail, "{}: detail", c.name);
        assert_eq!(r.hint, c.hint, "{}: hint", c.name);
    }
}

// Go: chat/errors_test.go:105 TestErrorReportLines — lines() appends the hint after the
// detail rows.
#[test]
fn test_error_report_lines() {
    let mut r = ErrorReport {
        headline: String::new(),
        detail: vec!["a".to_owned(), "b".to_owned()],
        hint: "h".to_owned(),
    };
    assert_eq!(r.lines().join("|"), "a|b|h");
    r.hint = String::new();
    assert_eq!(r.lines().join("|"), "a|b");
}

/// Records the transcript's facade calls as the `kind:payload` lines the assertions compare
/// (a `ScriptedUi` at 80×30 plus the rendering of its event log).
#[derive(Clone)]
struct Rec(Arc<ScriptedUi>);

impl Default for Rec {
    fn default() -> Self {
        let ui = ScriptedUi::new(Vec::new());
        ui.set_size(80, 30);
        Self(ui)
    }
}

impl Rec {
    /// The facade the transcript under test writes to.
    fn ui(&self) -> Arc<ScriptedUi> {
        Arc::clone(&self.0)
    }

    fn lines(&self) -> Vec<String> {
        self.0
            .events()
            .into_iter()
            .filter_map(|e| {
                Some(match e {
                    UiEvent::Print(lines) => format!("print:{}", lines.join("|")),
                    UiEvent::UserBlock(s) => format!("user:{s}"),
                    UiEvent::CallPreview(l) => format!("call:{l}"),
                    UiEvent::CallDetail(d) => format!("detail:{d}"),
                    UiEvent::CallLine(l) => format!("line:{l}"),
                    UiEvent::ClosePreview => "settle".to_owned(),
                    UiEvent::PauseClock => "pause".to_owned(),
                    UiEvent::ResumeClock => "resume".to_owned(),
                    UiEvent::CallBody(rows) => format!("body:{}", rows.join("|")),
                    _ => return None,
                })
            })
            .collect()
    }

    fn joined(&self) -> String {
        self.lines().join("\n")
    }
}

// Go: chat/errors_test.go:117 TestTranscriptErrorBlock — errorBlock renders headline +
// tool-result-idiom detail rows in ONE block (one separator), and groups with adjacent
// error output like error().
#[test]
fn test_transcript_error_block() {
    let rec = Rec::default();
    let tr = Transcript::new(rec.ui(), None);

    tr.user("hi");
    tr.error_block(
        "Rate limited (429)",
        &["Rate limit reached".to_owned(), "second row".to_owned()],
    );
    tr.error_block("Provider server error (500)", &[]); // consecutive: same block

    let want = [
        "user:hi".to_owned(),
        "print:".to_owned(), // one separator opens the error block
        format!(
            "print:{}",
            [
                red("✗ Rate limited (429)"),
                dim("  ⎿ Rate limit reached"),
                dim("    second row"),
            ]
            .join("|")
        ),
        format!("print:{}", red("✗ Provider server error (500)")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/errors_test.go:146 TestTranscriptErrorBlockHangingWrap — overlong detail rows
// pre-wrap under the hanging indent (width 80 → wrap at 75), so region-level wrapping
// never restarts a continuation row at column zero.
#[test]
fn test_transcript_error_block_hanging_wrap() {
    let rec = Rec::default();
    let tr = Transcript::new(rec.ui(), None);
    tr.user("hi");
    let long = "x".repeat(100);
    tr.error_block("Rate limited (429)", std::slice::from_ref(&long));

    let want = [
        "user:hi".to_owned(),
        "print:".to_owned(),
        format!(
            "print:{}",
            [
                red("✗ Rate limited (429)"),
                dim(&format!("  ⎿ {}", &long[..75])),
                dim(&format!("    {}", &long[75..])),
            ]
            .join("|")
        ),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/errors_test.go:167 TestTranscriptErrorBlockMultiline — detail entries carrying
// embedded newlines are split and indented per row (blank rows dropped).
#[test]
fn test_transcript_error_block_multiline() {
    let rec = Rec::default();
    let tr = Transcript::new(rec.ui(), None);
    tr.user("hi");
    tr.error_block("Request failed", &["line one\nline two\n\n".to_owned()]);

    let want = [
        "user:hi".to_owned(),
        "print:".to_owned(),
        format!(
            "print:{}",
            [
                red("✗ Request failed"),
                dim("  ⎿ line one"),
                dim("    line two"),
            ]
            .join("|")
        ),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}
