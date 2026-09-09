//! `retry_round` + the `is_retryable` table (chat/run.go:1417-1447, chat/chat.go:126-158).

use std::time::Duration;

use crate::chat::ChatError;
use crate::llm::LlmError;
use crate::provider::error::ProviderError;
use crate::ui::facade::BusyGuard;
use tokio_util::sync::CancellationToken;

use crate::repl::errors::describe_error;

/// Max transient re-attempts of one model call (chat.go:126 `maxRetries`).
pub(crate) const MAX_RETRIES: u32 = 10;

/// One unit of the linear backoff — attempt N waits N of these (run.go:1417
/// `retryBackoff`). Tests compress it through tokio's paused clock instead of a knob.
const RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Linear backoff attempt×1s (test-compressible), max 10; cancel beats the timer; ONE
/// recovery notice; `allowed=false` (image providers) passes through. `round` is an async
/// closure (Rust 2024 `AsyncFnMut`): each attempt's future re-borrows the closure's
/// captures — the exact Go per-attempt closure shape (run.go:1426-1447).
///
/// Nothing lands in the transcript until the turn gives up: the current error's
/// classification rides the busy status row (`"%s — retrying (attempt %d/%d)"`), and a
/// recovery leaves exactly one dim `"⟳ %s — recovered after %d attempt(s)"` notice.
/// Interrupt partials travel OUTSIDE the `Result` (the closure's captured buffers);
/// `Err(ChatError::Interrupted)` only signals the class.
pub async fn retry_round<T>(
    cancel: &CancellationToken,
    allowed: bool,
    busy: impl Fn(&str) -> BusyGuard,
    notice: impl Fn(String),
    mut round: impl AsyncFnMut() -> Result<T, ChatError>,
) -> Result<T, ChatError> {
    let mut result = round().await;
    if !allowed {
        // Dedicated image providers bill per attempt: never auto-retry (run.go:1428).
        return result;
    }
    let mut attempt: u32 = 1;
    while attempt <= MAX_RETRIES {
        let Err(e) = &result else { break };
        if !is_retryable(e) {
            break;
        }
        let headline = describe_error(e).headline;
        let guard = busy(&format!(
            "{headline} — retrying (attempt {attempt}/{MAX_RETRIES})"
        ));
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                // The cancelled turn wins over the backoff timer (run.go:1434-1437);
                // the partials of the last attempt stay in the closure's captures.
                guard.stop();
                return Err(ChatError::Interrupted);
            }
            () = tokio::time::sleep(RETRY_BACKOFF * attempt) => {}
        }
        guard.stop();
        result = round().await;
        if result.is_ok() {
            notice(format!(
                "⟳ {headline} — recovered after {attempt} attempt(s)"
            ));
        }
        attempt += 1;
    }
    result
}

/// Whether the error is likely transient and worth retrying (chat.go:136-158).
///
/// Non-retryable: user interruption, the tool-loop caps, a cancelled or provider-declared
/// permanent failure, a non-streaming stream response (`ErrNoEvents`) and HTTP 4xx except
/// 429. Everything else — 429, 5xx, transport faults, malformed frames — retries.
pub(crate) fn is_retryable(err: &ChatError) -> bool {
    match err {
        ChatError::Interrupted
        | ChatError::LocalCap { .. }
        | ChatError::SharedCap { .. }
        | ChatError::Provider(ProviderError::Cancelled | ProviderError::Permanent(_)) => false,
        ChatError::Provider(ProviderError::Wire { source, .. }) => match source.as_ref() {
            LlmError::NoEvents | LlmError::Cancelled => false,
            LlmError::Status(se) => se.status == 429 || se.status >= 500,
            _ => true,
        },
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::chat::ChatError;
    use crate::llm::{LlmError, StatusError};
    use crate::provider::error::{PermanentError, ProviderError, WireOp};

    use super::is_retryable;

    fn status(code: u16) -> ChatError {
        ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::Status(StatusError {
                status: code,
                status_text: String::new(),
                method: "POST".to_owned(),
                url: "http://x".to_owned(),
                body: String::new(),
            }),
        ))
    }

    // Go: chat/retry_test.go:13 TestPermanentErrorNotRetryable — provider-declared
    // permanent failures (imagen safety filters) never retry; an untyped failure still does.
    #[test]
    fn test_permanent_error_not_retryable() {
        let bare = ChatError::Provider(ProviderError::Permanent(PermanentError::msg(
            "imagen: all candidates were safety-filtered: unsafe",
        )));
        assert!(!is_retryable(&bare), "PermanentError classified retryable");
        let transient = ChatError::Provider(ProviderError::other("connection reset by peer"));
        assert!(
            is_retryable(&transient),
            "transient error must stay retryable"
        );
    }

    // Go: chat/chat.go:136-158 — the structured-status table and its sentinels.
    #[test]
    fn test_is_retryable_table() {
        assert!(!is_retryable(&ChatError::Interrupted));
        assert!(!is_retryable(&ChatError::LocalCap {
            turns: std::num::NonZeroU32::new(3).expect("nonzero")
        }));
        assert!(!is_retryable(&ChatError::SharedCap { turns: 3 }));
        assert!(!is_retryable(&ChatError::Provider(
            ProviderError::Cancelled
        )));
        assert!(!is_retryable(&ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::NoEvents
        ))));
        // Status table: 429 and 5xx retry, other 4xx do not.
        assert!(is_retryable(&status(429)));
        assert!(is_retryable(&status(500)));
        assert!(is_retryable(&status(503)));
        assert!(!is_retryable(&status(400)));
        assert!(!is_retryable(&status(404)));
        // An in-band stream error carries no status: it retries.
        assert!(is_retryable(&ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::InBand("overloaded_error".to_owned())
        ))));
    }
}
