//! The chat layer's error type (chat/chat.go, chat/turns.go). Every Display text is byte-equal to the Go
//! message it ports, except where a message named a feature that no longer exists (DIVERGENCES).

use crate::provider::error::ProviderError;

/// Why a headless run failed.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// The per-loop `--max-turns` cap was hit without a final response (chat.go:290).
    #[error("tool loop reached the --max-turns limit without a final response ({turns} turns)")]
    LocalCap {
        /// The local cap that was exhausted.
        turns: std::num::NonZeroU32,
    },
    /// The run-wide `TurnBudget` was exhausted (chat.go:293-294). Go's text named the child agents it was
    /// shared with; they went with the retired `delegate` toolset (DIVERGENCES).
    #[error(
        "tool loop reached the --max-turns limit without a final response ({turns} turns, the whole run's budget)"
    )]
    SharedCap {
        /// The shared cap that was exhausted.
        turns: u32,
    },
    /// The output ceiling cut the model off (DIVERGENCES X-69): an error, never an answer — the reply
    /// would be a fragment, and a fragment of THOUGHT promoted to a reply is what the reasoning-only rule
    /// did before. Not retried: the same request hits the same ceiling.
    #[error("output truncated — {0}")]
    Truncated(crate::provider::Truncation),
    /// The provider failed; Display is the provider's own text.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The provider failed AFTER the round's text had reached the user (DIVERGENCES X-82): shown once,
    /// never replayed — a retry would stream the same answer again. Display is the provider's own text.
    /// Only the interactive tool loop raises it; headless has no retry to keep it from.
    #[error(transparent)]
    AfterOutput(ProviderError),
    /// `--output-format` was neither `text` nor `json` (chat/output.go:51).
    #[error("unknown output format {0:?} (want text or json)")]
    BadFormat(String),
    /// The run was cancelled (Ctrl-C / SIGTERM) before it finished (DIVERGENCES I-03).
    #[error("interrupted")]
    Interrupted,
    /// Writing the report or the reply failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::ChatError;

    #[test]
    fn display_texts_match_go() {
        assert_eq!(
            ChatError::LocalCap {
                turns: std::num::NonZeroU32::new(7).expect("nonzero")
            }
            .to_string(),
            "tool loop reached the --max-turns limit without a final response (7 turns)"
        );
        assert_eq!(
            ChatError::SharedCap { turns: 5 }.to_string(),
            "tool loop reached the --max-turns limit without a final response (5 turns, the whole run's budget)"
        );
        assert_eq!(ChatError::Interrupted.to_string(), "interrupted");
        assert_eq!(
            ChatError::Truncated(crate::provider::Truncation {
                reason: "stop_reason: max_tokens".to_owned(),
                limit: Some(32_000),
                dropped_tools: vec!["bash".to_owned()],
            })
            .to_string(),
            "output truncated — stop_reason: max_tokens: the output hit its 32000-token limit; the tool call it was composing was dropped (bash)"
        );
    }
}
