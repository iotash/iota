//! `finalize_interrupt` — the pure three-state interrupt table (chat/interrupt.go:44-61,
//! docs/design/interrupt.md; all 7 Go cases port in `tests/interrupt.rs`).

use crate::provider::model::{AssistantBody, Attachment, Body, Message};

/// What an interrupted turn leaves behind (see [`finalize_interrupt`]).
#[derive(Debug, PartialEq)]
pub struct InterruptDecision {
    /// The history after the table was applied.
    pub history: Vec<Message>,
    /// Whether the turn should be persisted.
    pub persist: bool,
    /// The dropped user message's attachments — non-empty only when the turn was
    /// discarded whole, so the caller can restore them to the pending set.
    pub dropped_attachments: Vec<Attachment>,
}

/// Decides what an interrupted turn leaves behind (chat/interrupt.go).
/// `history[watermark..]` is this turn's messages (the user message first, then any
/// completed tool rounds). The three-state table:
///
/// - partial text exists → append an assistant message carrying the partial
///   (`interrupted: true`, NO raw content — a partial provider blob may be invalid on
///   replay); persist.
/// - no text and only the user message since the watermark → truncate the whole turn back
///   to the watermark; nothing to persist; the dropped user message's attachments are
///   handed back so the caller can restore them to the pending set (ESC must not silently
///   strip an /edit canvas).
/// - no text but completed tool rounds → keep the turn as-is with NO trailing assistant
///   message (the tool side effects already happened); persist.
///
/// A reasoning-only partial (reasoning without content) counts as "no text".
pub fn finalize_interrupt(
    mut history: Vec<Message>,
    watermark: usize,
    partial: &str,
    partial_reasoning: &str,
) -> InterruptDecision {
    if !partial.is_empty() {
        history.push(Message {
            content: partial.to_owned(),
            body: Body::Assistant(AssistantBody {
                reasoning: partial_reasoning.to_owned(),
                interrupted: true,
                ..AssistantBody::default()
            }),
            ..Message::default()
        });
        return InterruptDecision {
            history,
            persist: true,
            dropped_attachments: Vec::new(),
        };
    }
    if history.len() <= watermark + 1 {
        let dropped = if watermark < history.len() {
            history[watermark].attachments.clone()
        } else {
            Vec::new()
        };
        history.truncate(watermark);
        return InterruptDecision {
            history,
            persist: false,
            dropped_attachments: dropped,
        };
    }
    InterruptDecision {
        history,
        persist: true,
        dropped_attachments: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! `finalize_interrupt` — the three-state persistence table (formerly `tests/repl/interrupt.rs`, reached
    //! through a `#[doc(hidden)]` re-export; moved in-file 2026-09-15).

    use super::{InterruptDecision, finalize_interrupt};
    use crate::provider::model::{
        AssistantBody, Attachment, Body, Message, Role, ToolBody, ToolCall,
    };
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

    // The three-state persistence table plus the reasoning-only case (reasoning without content counts as
    // "no text").
    #[test]
    fn partial_text_is_kept_as_an_interrupted_assistant() {
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
    fn no_text_and_no_tool_rounds_drops_the_turn() {
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
    fn tool_rounds_are_kept_as_is() {
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
    fn reasoning_only_counts_as_no_text() {
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
    fn partial_text_after_tool_rounds_is_appended() {
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

    // Cancelling a send must not strip the message's attachments: the discarded turn hands
    // them back so the next message still carries the /edit canvas (image turns ALWAYS take
    // this path — they produce no text).
    #[test]
    fn a_dropped_turn_hands_back_its_attachments() {
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
}
