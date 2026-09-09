//! The approval gate of the quiet loop (`chat/approval_test.go`): refusal with nobody to ask, forwarding to an
//! injected approver, and the shape of `QuietHost::ask_approval`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use iota::chat::run::refusal_text;
use iota::chat::turns::RunCtx;
use iota::chat::{QuietHost, execute_with_tools};
use iota::provider::model::{Message, Role, ToolCall};
use iota::tool::Dispatcher;

use crate::common::{GatedDispatch, WritingProvider, lock};

/// Runs the gated round trip: `write_file` requested once, then the model echoes the last history entry.
async fn run_gated(host: &mut QuietHost) -> (String, Arc<GatedDispatch>, Vec<Message>) {
    let d = Arc::new(GatedDispatch::new());
    let mut history = vec![Message::user("go")];
    let outcome = execute_with_tools(
        &RunCtx::default(),
        &WritingProvider::default(),
        d.clone(),
        &mut history,
        d.tools(),
        "",
        None,
        host,
    )
    .await
    .expect("loop failed");
    (outcome.content, d, history)
}

// Go: chat/approval_test.go:53
#[tokio::test]
async fn test_quiet_loop_refuses_when_there_is_nobody_to_ask() {
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

// Go: chat/approval_test.go:68
#[tokio::test]
async fn test_quiet_loop_forwards_approval() {
    // A delegated child has no user of its own but runs inside a parent that does, so its question travels up
    // and the answer decides the call.
    let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let mut host = QuietHost {
        approve: Some(Box::new(move |tc: &ToolCall, detail: &str| {
            lock(&seen).push((tc.name.clone(), detail.to_owned()));
            (true, String::new())
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

// Go: chat/approval_test.go:92
#[tokio::test]
async fn test_quiet_loop_denial_continues_the_run() {
    // A denial is a result the model reads, not an aborted turn: the child carries on and reports back.
    let mut host = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            (false, "The user declined this call.".to_owned())
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

// Go: chat/approval_test.go:111
#[test]
fn test_quiet_host_ask_approval_shapes() {
    // The refusal text is the call's result either way, so a failing prompt must not be mistaken for consent.
    let h = QuietHost::new();
    let tc = ToolCall {
        name: "edit_file".to_owned(),
        ..ToolCall::default()
    };
    let (ok, why) = h.ask_approval(&tc, "path:x");
    assert!(!ok);
    assert!(why.contains("edit_file"), "nil approver = ({ok}, {why:?})");
    assert_eq!(why, refusal_text("edit_file"));
    assert_eq!(
        why,
        "edit_file was not executed: it requires interactive approval, which is unavailable in this non-interactive run. Set the toolset's auto-approve option (tools.code.auto_write / tools.shell.auto_run) to permit it here."
    );

    let h = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            (false, "prompt broke".to_owned())
        })),
        ..QuietHost::new()
    };
    let (ok, why) = h.ask_approval(&tc, "path:x");
    assert!(!ok);
    assert_eq!(
        why, "prompt broke",
        "failed prompt must be a refusal carrying the reason"
    );

    // An approver's answer travels verbatim, detail included.
    let h = QuietHost {
        approve: Some(Box::new(|tc: &ToolCall, detail: &str| {
            (true, format!("{}:{detail}", tc.name))
        })),
        ..QuietHost::new()
    };
    assert_eq!(
        h.ask_approval(&tc, "path:x"),
        (true, "edit_file:path:x".to_owned())
    );
}

// Go: chat/approval_test.go:138 TestForwardedApprovalCarriesTheCallDetail — the prompt has to say
// what the call is ABOUT. For a delegated call nothing else on screen does: the widget above
// describes the delegation, not the operation the child is asking to perform, so a gate naming only
// the tool asks the user to authorize "write_file" without saying which file. (Portable since T-30
// gave the built-in code/shell sets real `header_summary` capabilities — D-12 is closed.)
#[tokio::test]
async fn test_forwarded_approval_carries_the_call_detail() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut host = QuietHost {
        approve: Some(Box::new(move |_tc: &ToolCall, detail: &str| {
            lock(&sink).push(detail.to_owned());
            (false, "The user declined this call.".to_owned())
        })),
        ..QuietHost::new()
    };
    let d = Arc::new(GatedDispatch::with_header());
    let mut history = vec![Message::user("go")];
    execute_with_tools(
        &RunCtx::default(),
        &WritingProvider::with_path("internal/ui/model.go"),
        d.clone(),
        &mut history,
        d.tools(),
        "",
        None,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(lock(&seen).as_slice(), ["internal/ui/model.go"]);
    assert_eq!(d.ran(), 0);
}

/// The header half of the detail split (`chat/approval_test.go:171`): one implementation, two
/// readers — the transcript's `[name detail]` header and the gate's bare detail.
mod header_split {
    use iota::tool::Dispatcher;
    use iota::tool::fmt::tool_call_header;
    use pretty_assertions::assert_eq;

    use crate::common::{GatedDispatch, call_with};

    /// `provider.ToolCall{Name: n, Arguments: {k: v}…}` — the id is irrelevant to a header.
    fn call(name: &str, args: &[(&str, &str)]) -> iota::provider::model::ToolCall {
        call_with("c1", name, args)
    }

    // Go: chat/approval_test.go:171 TestToolCallHeaderUnchangedBySplit — the header keeps its shape
    // after the detail was split out of it.
    #[test]
    fn test_tool_call_header_unchanged_by_split() {
        let detail = GatedDispatch::with_header();
        let tc = call("write_file", &[("path", "a/b.go")]);
        assert_eq!(
            tool_call_header(&detail as &dyn Dispatcher, &tc),
            "[write_file a/b.go]"
        );

        // A tool with no summary of its own falls back to the argument digest, and an empty summary
        // renders as a bare name rather than the digest.
        let plain = GatedDispatch::new();
        assert_eq!(
            tool_call_header(&plain as &dyn Dispatcher, &tc),
            "[write_file path:a/b.go]"
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
