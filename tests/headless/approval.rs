//! The approval gate of the quiet loop (`chat/approval_test.go`): refusal with nobody to ask, forwarding to an
//! injected approver, and the shape of `QuietHost::ask_approval`.

use std::sync::{Arc, Mutex};

use iota::headless::run::refusal_text;
use iota::headless::{QuietHost, TurnParams, execute_with_tools};
use iota::provider::model::{JsonObject, Message, Role, ToolCall};
use iota::testing::{FakeProvider, GatedDispatch, Round, lock};
use iota::tool::context::RunCtx;
use iota::tool::{Approval, Dispatcher};

/// Asks for `write_file` (with `args`) once, then answers `saw: ` + the last history entry's content — so a
/// test can assert on what the model was actually told.
fn writing(args: JsonObject) -> FakeProvider {
    FakeProvider::new()
        .with_tools()
        .answering(move |n, messages| {
            if n == 1 {
                return Round::calls(vec![ToolCall {
                    id: "c1".to_owned(),
                    name: "write_file".to_owned(),
                    arguments: args.clone(),
                }]);
            }
            let last = messages
                .last()
                .map(|m| m.content.as_str())
                .unwrap_or_default();
            Round::reply(&format!("saw: {last}"))
        })
}

/// The `write_file` call names the file it would write.
fn writing_path(path: &str) -> FakeProvider {
    let mut args = JsonObject::new();
    args.insert("path".to_owned(), serde_json::Value::from(path));
    writing(args)
}

/// Runs the gated round trip: `write_file` requested once, then the model echoes the last history entry.
async fn run_gated(host: &mut QuietHost) -> (String, Arc<GatedDispatch>, Vec<Message>) {
    let d = Arc::new(GatedDispatch::new());
    let mut history = vec![Message::user("go")];
    let outcome = execute_with_tools(
        TurnParams {
            cx: &RunCtx::default(),
            tp: &writing(JsonObject::new()),
            dispatch: d.clone(),
            tools: d.tools(),
            harness: "",
            overlay: "",
            max_turns: None,
        },
        &mut history,
        host,
    )
    .await
    .expect("loop failed");
    (outcome.content, d, history)
}

#[tokio::test]
async fn the_quiet_loop_refuses_when_there_is_nobody_to_ask() {
    // With nobody to ask, the loop refuses and says how to enable the call. That is the -m contract.
    let mut host = QuietHost::new();
    let (reply, d, history) = run_gated(&mut host).await;
    assert_eq!(d.ran(), 0, "a gated tool ran with no approval");
    assert!(
        reply.contains("auto_write"),
        "the refusal must name the way to enable it, got {reply:?}"
    );
    assert_eq!(reply, format!("saw: {}", refusal_text("write_file")));
    // The refusal is fed back as an IsError tool result answering the call.
    let result = &history[2];
    assert_eq!(result.role(), Role::Tool);
    assert!(result.is_error());
    assert_eq!(result.tool_call_id(), "c1");
    assert_eq!(result.tool_call_name(), "write_file");
    assert_eq!(result.content, refusal_text("write_file"));
    assert_eq!(host.rec.round_count(), 2);
}

#[tokio::test]
async fn the_quiet_loop_forwards_approval_to_the_injected_approver() {
    // A headless loop has no user of its own, so an injected approver is what decides a gated call: the
    // question travels up and the answer decides it.
    let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let mut host = QuietHost {
        approve: Some(Box::new(move |tc: &ToolCall, detail: &str| {
            lock(&seen).push((tc.name.clone(), detail.to_owned()));
            Approval::Allow
        })),
        ..QuietHost::new()
    };
    let (reply, d, history) = run_gated(&mut host).await;
    // A dispatcher with no `header_summary` of its own hands the gate an empty detail; the
    // capability-bearing shape is `test_forwarded_approval_carries_the_call_detail` below.
    assert_eq!(
        lock(&asked).as_slice(),
        &[("write_file".to_owned(), String::new())]
    );
    assert_eq!(d.ran(), 1, "tool ran {} times, want 1", d.ran());
    assert!(
        reply.contains("written"),
        "the model should have seen the result, got {reply:?}"
    );
    assert_eq!(history[2].content, "written");
    assert!(!history[2].is_error());
}

#[tokio::test]
async fn a_denied_call_does_not_end_the_run() {
    // A denial is a result the model reads, not an aborted turn: the child carries on and reports back.
    let mut host = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            Approval::Deny("The user declined this call.".to_owned())
        })),
        ..QuietHost::new()
    };
    let (reply, d, history) = run_gated(&mut host).await;
    assert_eq!(d.ran(), 0, "a denied tool ran anyway");
    assert!(
        reply.contains("declined"),
        "the model must be told it was declined, got {reply:?}"
    );
    assert_eq!(history[2].content, "The user declined this call.");
    assert!(history[2].is_error());
}

#[test]
fn the_quiet_host_answers_approval_from_its_approver_or_refuses() {
    // The refusal text is the call's result either way, so a failing prompt must not be mistaken for consent.
    let h = QuietHost::new();
    let tc = ToolCall {
        name: "edit_file".to_owned(),
        ..ToolCall::default()
    };
    let Approval::Deny(why) = h.ask_approval(&tc, "path:x") else {
        panic!("a refusal was expected")
    };
    assert!(why.contains("edit_file"), "nil approver = {why:?}");
    assert_eq!(why, refusal_text("edit_file"));
    assert_eq!(
        why,
        "edit_file was not executed: it requires interactive approval, which is unavailable in this non-interactive run. Set the toolset's auto-approve option (tools.code.auto_write / tools.shell.auto_run) to permit it here."
    );

    let h = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            Approval::Deny("prompt broke".to_owned())
        })),
        ..QuietHost::new()
    };
    let Approval::Deny(why) = h.ask_approval(&tc, "path:x") else {
        panic!("a refusal was expected")
    };
    assert_eq!(
        why, "prompt broke",
        "failed prompt must be a refusal carrying the reason"
    );

    // An approver's answer travels verbatim, detail included.
    let h = QuietHost {
        approve: Some(Box::new(|tc: &ToolCall, detail: &str| {
            Approval::Deny(format!("{}:{detail}", tc.name))
        })),
        ..QuietHost::new()
    };
    assert_eq!(
        h.ask_approval(&tc, "path:x"),
        Approval::Deny("edit_file:path:x".to_owned())
    );
    let h = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| Approval::Allow)),
        ..QuietHost::new()
    };
    assert_eq!(h.ask_approval(&tc, "path:x"), Approval::Allow);
}

// The forwarded prompt has to say what the call is ABOUT: a gate naming only the tool asks the user to authorize "write_file"
// without saying which file. (Portable since T-30 gave the built-in code/shell sets real
// `header_summary` capabilities — D-12 is closed.)
#[tokio::test]
async fn a_forwarded_approval_carries_the_call_detail() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut host = QuietHost {
        approve: Some(Box::new(move |_tc: &ToolCall, detail: &str| {
            lock(&sink).push(detail.to_owned());
            Approval::Deny("The user declined this call.".to_owned())
        })),
        ..QuietHost::new()
    };
    let d = Arc::new(GatedDispatch::with_header());
    let mut history = vec![Message::user("go")];
    execute_with_tools(
        TurnParams {
            cx: &RunCtx::default(),
            tp: &writing_path("src/ui/model.rs"),
            dispatch: d.clone(),
            tools: d.tools(),
            harness: "",
            overlay: "",
            max_turns: None,
        },
        &mut history,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(lock(&seen).as_slice(), ["src/ui/model.rs"]);
    assert_eq!(d.ran(), 0);
}

/// The header half of the detail split (`chat/approval_test.go:171`): one implementation, two
/// readers — the transcript's `[name detail]` header and the gate's bare detail.
mod header_split {
    use iota::tool::Dispatcher;
    use iota::tool::fmt::tool_call_header;
    use pretty_assertions::assert_eq;

    use iota::testing::{GatedDispatch, tool_call_with};

    /// `provider.ToolCall{Name: n, Arguments: {k: v}…}` — the id is irrelevant to a header.
    fn call(name: &str, args: &[(&str, &str)]) -> iota::provider::model::ToolCall {
        tool_call_with("c1", name, args)
    }

    // The header keeps its shape after the detail was split out of it.
    #[test]
    fn the_tool_call_header_is_unchanged_by_the_detail_split() {
        let detail = GatedDispatch::with_header();
        let tc = call("write_file", &[("path", "a/b.rs")]);
        assert_eq!(
            tool_call_header(&detail as &dyn Dispatcher, &tc),
            "[write_file a/b.rs]"
        );

        // A tool with no summary of its own falls back to the argument digest, and an empty summary
        // renders as a bare name rather than the digest.
        let plain = GatedDispatch::new();
        assert_eq!(
            tool_call_header(&plain as &dyn Dispatcher, &tc),
            "[write_file path:a/b.rs]"
        );
        assert_eq!(
            tool_call_header(&plain as &dyn Dispatcher, &call("x", &[])),
            "[x]"
        );
        // The capability's OWN empty answer is a bare name too — `edit_file`'s `new_string` must
        // never reach a header through the digest fallback (tool/codepath_test.go:96).
        assert_eq!(
            tool_call_header(
                &detail as &dyn Dispatcher,
                &call("write_file", &[("new_string", "x")])
            ),
            "[write_file]"
        );
    }
}
