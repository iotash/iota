//! `iota::tool::fmt` pins — the D-12 lift (`chat/toolcall_test.go` ports).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::BoxFuture;
use iota::chat::turns::RunCtx;
use iota::provider::model::{JsonObject, ToolCall};
use iota::tool::fmt::{print_tool_result_lines, tool_call_header};
use iota::tool::{Dispatcher, ToolOutput, ToolResult};
use pretty_assertions::assert_eq;

/// A dispatcher with NO header capability (Go's nil dispatcher — the digest applies).
struct NoCapDispatch;

impl Dispatcher for NoCapDispatch {
    fn tools(&self) -> Vec<iota::provider::model::ToolDef> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async { Ok(ToolOutput::ok("")) })
    }
}

/// A dispatcher carrying the header capability (Go `headerDispatch`).
struct HeaderDispatch {
    summary: Option<String>,
}

impl Dispatcher for HeaderDispatch {
    fn tools(&self) -> Vec<iota::provider::model::ToolDef> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        _name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async { Ok(ToolOutput::ok("")) })
    }

    fn header_summary(&self, _name: &str, _args: &JsonObject) -> Option<String> {
        self.summary.clone()
    }
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    let arguments = match args {
        serde_json::Value::Object(m) => m,
        _ => JsonObject::new(),
    };
    ToolCall {
        id: String::new(),
        name: name.to_owned(),
        arguments,
    }
}

// Go: chat/toolcall_test.go:15 TestToolCallHeader
#[test]
fn test_tool_call_header() {
    let tests = [
        (
            "single arg",
            call("shell", serde_json::json!({"command": "git status"})),
            "[shell command:git status]",
        ),
        (
            "keys sorted",
            call(
                "shell",
                serde_json::json!({"command": "git", "cwd": "/tmp", "stdin": "hi"}),
            ),
            "[shell command:git cwd:/tmp stdin:hi]",
        ),
        ("no args", call("ping", serde_json::json!({})), "[ping]"),
        (
            "newline collapsed",
            call("x", serde_json::json!({"a": "l1\nl2"})),
            "[x a:l1 l2]",
        ),
    ];
    for (name, tc, want) in tests {
        assert_eq!(tool_call_header(&NoCapDispatch, &tc), want, "{name}");
    }

    // Long values are truncated to TOOL_HEADER_MAX_VALUE (15) runes + ellipsis.
    let long = call("x", serde_json::json!({"a": "z".repeat(100)}));
    let got = tool_call_header(&NoCapDispatch, &long);
    assert!(
        got.contains(&format!("{}…", "z".repeat(15))),
        "long value not truncated to 15 runes + ellipsis: {got}"
    );
    assert!(
        !got.contains(&"z".repeat(16)),
        "over-long value survived: {got}"
    );
}

// Go: chat/toolcall_test.go:95 TestToolCallHeaderCapability — a tool that writes its own
// summary takes over the header completely: an empty one is a bare name, NOT a fallback
// to the argument digest (which for edit_file would paste a whole file into the header).
#[test]
fn test_tool_call_header_capability() {
    let tc = call(
        "edit_file",
        serde_json::json!({
            "path": "internal/ui/model.go",
            "new_string": "code\n".repeat(500),
        }),
    );

    let custom = HeaderDispatch {
        summary: Some("internal/ui/model.go".to_owned()),
    };
    assert_eq!(
        tool_call_header(&custom, &tc),
        "[edit_file internal/ui/model.go]"
    );

    let empty = HeaderDispatch {
        summary: Some(String::new()),
    };
    assert_eq!(
        tool_call_header(&empty, &tc),
        "[edit_file]",
        "empty summary must be a bare name"
    );

    // No capability declared: the generic digest applies, unchanged.
    let fallback = tool_call_header(&NoCapDispatch, &tc);
    assert!(
        fallback.contains("path:"),
        "digest not used when the tool declares no summary: {fallback}"
    );
}

// Go: chat/toolcall_test.go:41 TestPrintToolResult — the pure rows (the caller styles
// red on error; the Go test ran under NoColor for the same unstyled shape).
#[test]
fn test_print_tool_result() {
    let tests: [(&str, &str, &[&str]); 5] = [
        (
            "two lines shown fully",
            "line1\nline2",
            &["  ⎿ line1", "    line2"],
        ),
        (
            "exactly three shown fully",
            "a\nb\nc",
            &["  ⎿ a", "    b", "    c"],
        ),
        (
            "over three truncates with tail",
            "a\nb\nc\nd\ne",
            &["  ⎿ a", "    b", "    … +3 lines"],
        ),
        ("trailing blank lines trimmed", "only\n\n\n", &["  ⎿ only"]),
        ("empty becomes no output", "   \n  ", &["  ⎿ (no output)"]),
    ];
    for (name, result, want) in tests {
        assert_eq!(print_tool_result_lines(result, false), want, "{name}");
    }
    // Rows are identical on error — styling is the caller's (transcript) job.
    assert_eq!(
        print_tool_result_lines("boom", true),
        vec!["  ⎿ boom".to_owned()]
    );
}
