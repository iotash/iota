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
