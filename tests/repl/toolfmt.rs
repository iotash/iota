//! `iota::tool::fmt` pins — the D-12 lift (`chat/toolcall_test.go` ports).

use iota::provider::model::{JsonObject, ToolCall};
use iota::testing::{HeaderDispatch, NoCapDispatch};
use iota::tool::fmt::{print_tool_result_lines, tool_call_header};
use pretty_assertions::assert_eq;

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

#[test]
fn the_tool_call_header_digests_sorted_args_and_truncates_long_values() {
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

// A tool that writes its own
// summary takes over the header completely: an empty one is a bare name, NOT a fallback
// to the argument digest (which for edit_file would paste a whole file into the header).
#[test]
fn a_tools_own_summary_takes_over_the_header() {
    let tc = call(
        "edit_file",
        serde_json::json!({
            "path": "src/ui/model.rs",
            "new_string": "code\n".repeat(500),
        }),
    );

    let custom = HeaderDispatch {
        summary: Some("src/ui/model.rs".to_owned()),
    };
    assert_eq!(
        tool_call_header(&custom, &tc),
        "[edit_file src/ui/model.rs]"
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

// The pure rows — the caller styles red on error.
#[test]
fn tool_result_rows_show_three_lines_then_a_tail_count() {
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
