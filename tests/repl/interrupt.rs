//! `finalize_interrupt` — the three-state persistence table (`chat/interrupt_test.go`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::model::{AssistantBody, Attachment, Body, Message, Role, ToolBody, ToolCall};
use iota::repl::{InterruptDecision, finalize_interrupt};
use pretty_assertions::assert_eq;

fn system() -> Message {
    Message {
        content: "sys".to_owned(),
        body: Body::System,
        ..Message::default()
    }
}

fn user() -> Message {
    Message {
        content: "question".to_owned(),
        ..Message::default()
    }
}

fn tool_use() -> Message {
    Message {
        body: Body::Assistant(AssistantBody {
            tool_calls: vec![ToolCall {
                id: "c1".to_owned(),
                name: "t".to_owned(),
                ..ToolCall::default()
            }],
            ..AssistantBody::default()
        }),
        ..Message::default()
    }
}

fn tool_result() -> Message {
    Message {
        content: "out".to_owned(),
        body: Body::Tool(ToolBody {
            call_id: "c1".to_owned(),
            call_name: "t".to_owned(),
            ..ToolBody::default()
        }),
        ..Message::default()
    }
}

// Go: chat/interrupt_test.go:12 TestFinalizeInterrupt — the three-state persistence
// table plus the reasoning-only case (reasoning without content counts as "no text").
#[test]
fn test_finalize_interrupt_partial_text_is_kept_as_interrupted_assistant() {
    let history = vec![system(), user()];
    let InterruptDecision {
        history: got,
        persist,
        ..
    } = finalize_interrupt(history, 1, "partial answer", "partial thinking");
    assert!(persist, "expected persist=true when partial text exists");
    assert_eq!(got.len(), 3);
    let last = &got[2];
    assert_eq!(last.role(), Role::Assistant);
    assert_eq!(last.content, "partial answer");
    assert_eq!(last.reasoning(), "partial thinking");
    assert!(
        last.interrupted(),
        "assistant message not marked interrupted"
    );
    assert!(
        last.raw_content().is_none(),
        "interrupted assistant message must carry no raw content"
    );
}

#[test]
fn test_finalize_interrupt_no_text_no_tool_rounds_drops_the_turn() {
    let history = vec![system(), user()];
    let InterruptDecision {
        history: got,
        persist,
        ..
    } = finalize_interrupt(history, 1, "", "");
    assert!(
        !persist,
        "expected persist=false when the turn yielded nothing"
    );
    assert_eq!(got.len(), 1, "expected turn rolled back to watermark");
    assert_eq!(got[0].role(), Role::System);
}

#[test]
fn test_finalize_interrupt_tool_rounds_are_kept_as_is() {
    let history = vec![system(), user(), tool_use(), tool_result()];
    let InterruptDecision {
        history: got,
        persist,
        ..
    } = finalize_interrupt(history, 1, "", "");
    assert!(persist, "expected persist=true when tool rounds completed");
    assert_eq!(got.len(), 4);
    assert_eq!(
        got.last().unwrap().role(),
        Role::Tool,
        "expected no trailing assistant message"
    );
}

#[test]
fn test_finalize_interrupt_reasoning_only_counts_as_no_text() {
    let history = vec![system(), user()];
    let InterruptDecision {
        history: got,
        persist,
        ..
    } = finalize_interrupt(history, 1, "", "only thinking so far");
    assert!(
        !persist,
        "expected persist=false for a reasoning-only partial"
    );
    assert_eq!(got.len(), 1, "expected turn rolled back");
}

#[test]
fn test_finalize_interrupt_partial_after_tool_rounds_appends() {
    let history = vec![user(), tool_use(), tool_result()];
    let InterruptDecision {
        history: got,
        persist,
        ..
    } = finalize_interrupt(history, 0, "final partial", "");
    assert!(persist);
    assert_eq!(got.len(), 4);
    let last = &got[3];
    assert!(last.interrupted());
    assert_eq!(last.content, "final partial");
}

// Go: chat/interrupt_test.go:95 TestFinalizeInterruptReturnsDroppedAttachments —
// cancelling a send must not strip the message's attachments: the discarded turn hands
// them back so the next message still carries the /edit canvas (image turns ALWAYS take
// this path — they produce no text).
#[test]
fn test_finalize_interrupt_returns_dropped_attachments() {
    let canvas = Attachment {
        filename: "gen-1.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: vec![1],
    };
    let history = vec![
        Message {
            content: "a cat".to_owned(),
            ..Message::default()
        },
        Message {
            attachments: vec![canvas.clone()],
            body: Body::Assistant(AssistantBody {
                ..AssistantBody::default()
            }),
            ..Message::default()
        },
        Message {
            content: "add a hat".to_owned(),
            attachments: vec![canvas],
            ..Message::default()
        },
    ];

    let InterruptDecision {
        history: got,
        persist,
        dropped_attachments: dropped,
    } = finalize_interrupt(history.clone(), 2, "", "");
    assert_eq!(got.len(), 2, "discard path broken");
    assert!(!persist, "discard path broken");
    assert_eq!(dropped.len(), 1, "want the canvas back");
    assert_eq!(dropped[0].filename, "gen-1.png");

    // A turn that survives keeps its attachments in history — nothing to hand back
    // (they already reached the model).
    let d = finalize_interrupt(history.clone(), 2, "partial text", "").dropped_attachments;
    assert!(d.is_empty(), "kept turn must not return attachments: {d:?}");
    // Completed tool rounds keep the turn too.
    let mut with_tool = history.clone();
    with_tool.push(Message {
        content: "out".to_owned(),
        body: Body::Tool(ToolBody {
            ..ToolBody::default()
        }),
        ..Message::default()
    });
    let d = finalize_interrupt(with_tool, 2, "", "").dropped_attachments;
    assert!(d.is_empty(), "tool-round turn must not return attachments");
    // A watermark at the end (nothing composed yet) is not a crash.
    let watermark = history.len();
    let d = finalize_interrupt(history, watermark, "", "").dropped_attachments;
    assert!(d.is_empty(), "empty turn = {d:?}");
}
