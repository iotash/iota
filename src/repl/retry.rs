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

/// One unit of the linear backoff — attempt N waits N of these, here and in the run loop's turn-level
/// retry. Tests compress it through tokio's paused clock instead of a knob.
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_secs(1);

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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! `is_retryable`'s table, and `retry_round` itself (formerly `tests/repl/turn.rs`, reached through a
    //! hidden `pub use` re-export; moved in-file 2026-09-15). The backoff is a const, so the retry tests
    //! compress TIME instead: tokio's paused clock advances a pending `sleep` the moment the runtime goes
    //! idle. The interrupt case needs no clock at all — a cancelled token beats the timer by construction
    //! (`biased` select).

    use std::sync::{Arc, Mutex};

    use crate::chat::ChatError;
    use crate::llm::{LlmError, StatusError};
    use crate::provider::error::{PermanentError, ProviderError, WireOp};
    use crate::ui::facade::BusyGuard;
    use tokio_util::sync::CancellationToken;

    use super::{is_retryable, retry_round};

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

    // Provider-declared
    // permanent failures (imagen safety filters) never retry; an untyped failure still does.
    #[test]
    fn a_provider_declared_permanent_failure_never_retries() {
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

    // The structured-status table and its sentinels.
    #[test]
    fn is_retryable_follows_the_status_table() {
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

    // --- retry_round -----------------------------------------------------------------

    /// Records every busy label the retry raised.
    #[derive(Clone, Default)]
    struct Busy(Arc<Mutex<Vec<String>>>);

    impl Busy {
        fn guard(&self, label: &str) -> BusyGuard {
            self.0.lock().expect("busy log").push(label.to_owned());
            BusyGuard::new(|| {})
        }

        fn labels(&self) -> Vec<String> {
            self.0.lock().expect("busy log").clone()
        }
    }

    /// A transient in-band stream failure (no 4xx token → retryable).
    fn transient() -> ChatError {
        ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::InBand("overloaded_error".to_owned()),
        ))
    }

    // A transient failure re-issues the SAME call and nothing else: 3 calls, 2 busy rows, and EXACTLY ONE
    // dim recovery notice (not one per attempt).
    #[tokio::test(start_paused = true)]
    async fn a_transient_failure_retries_the_same_call_and_notices_once() {
        let busy = Busy::default();
        let notices: Arc<Mutex<Vec<String>>> = Arc::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();

        let notices_in = Arc::clone(&notices);
        let got = retry_round(
            &cancel,
            true,
            |label| busy.guard(label),
            move |line| notices_in.lock().expect("notices").push(line),
            async || {
                let mut n = calls.lock().expect("calls");
                *n += 1;
                if *n < 3 {
                    Err(transient())
                } else {
                    Ok("done".to_owned())
                }
            },
        )
        .await;

        assert_eq!(got.expect("recovery"), "done");
        assert_eq!(*calls.lock().expect("calls"), 3, "want 3 calls");
        let labels = busy.labels();
        assert_eq!(labels.len(), 2, "want 2 busy rows: {labels:?}");
        assert_eq!(
            labels[0], "Request failed — retrying (attempt 1/10)",
            "the busy row carries the classification headline and the attempt counter"
        );
        assert_eq!(labels[1], "Request failed — retrying (attempt 2/10)");
        let notices = notices.lock().expect("notices").clone();
        assert_eq!(
            notices,
            vec!["⟳ Request failed — recovered after 2 attempt(s)".to_owned()],
            "exactly one recovery notice"
        );
    }

    // A 4xx surfaces on the FIRST call; nothing lands in the transcript on the way.
    #[tokio::test(start_paused = true)]
    async fn a_permanent_error_is_not_retried() {
        let busy = Busy::default();
        let notices: Arc<Mutex<Vec<String>>> = Arc::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();

        let notices_in = Arc::clone(&notices);
        let got: Result<String, ChatError> = retry_round(
            &cancel,
            true,
            |label| busy.guard(label),
            move |line| notices_in.lock().expect("notices").push(line),
            async || {
                *calls.lock().expect("calls") += 1;
                Err(ChatError::Provider(ProviderError::wire(
                    WireOp::Stream,
                    LlmError::Status(StatusError {
                        status: 400,
                        status_text: "Bad Request".to_owned(),
                        method: "POST".to_owned(),
                        url: "http://x".to_owned(),
                        body: "bad request".to_owned(),
                    }),
                )))
            },
        )
        .await;

        assert!(got.is_err(), "a 4xx must surface");
        assert_eq!(*calls.lock().expect("calls"), 1);
        assert!(busy.labels().is_empty(), "no retry row for a client error");
        assert!(notices.lock().expect("notices").is_empty());
    }

    // A dedicated image provider bills per attempt, so `allowed = false` passes the first result through
    // untouched even for an otherwise transient failure.
    #[tokio::test(start_paused = true)]
    async fn a_disallowed_retry_passes_the_first_result_through() {
        let busy = Busy::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();

        let got: Result<String, ChatError> = retry_round(
            &cancel,
            false,
            |label| busy.guard(label),
            |_| panic!("a disallowed retry must not notice"),
            async || {
                *calls.lock().expect("calls") += 1;
                Err(transient())
            },
        )
        .await;

        assert!(got.is_err());
        assert_eq!(
            *calls.lock().expect("calls"),
            1,
            "allowed=false must not retry"
        );
        assert!(busy.labels().is_empty());
    }

    // The cancelled turn beats the backoff timer: one call, then `Interrupted`, and the busy row is
    // stopped on the way out.
    #[tokio::test(start_paused = true)]
    async fn an_interrupt_during_the_backoff_wins_over_the_timer() {
        let busy = Busy::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let got: Result<String, ChatError> = retry_round(
            &cancel,
            true,
            |label| busy.guard(label),
            |_| panic!("an interrupted retry must not notice"),
            async || {
                *calls.lock().expect("calls") += 1;
                Err(transient())
            },
        )
        .await;

        assert!(
            matches!(got, Err(ChatError::Interrupted)),
            "want Interrupted, got {:?}",
            got.map(|_| ())
        );
        assert_eq!(*calls.lock().expect("calls"), 1);
        assert_eq!(busy.labels().len(), 1, "the attempt-1 row was raised");
    }

    // The ladder stops at 10 attempts and the last busy row says so.
    #[tokio::test(start_paused = true)]
    async fn retry_round_stops_at_max_retries() {
        let busy = Busy::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();

        let got: Result<String, ChatError> = retry_round(
            &cancel,
            true,
            |label| busy.guard(label),
            |_| panic!("a turn that never recovered must not notice"),
            async || {
                *calls.lock().expect("calls") += 1;
                Err(transient())
            },
        )
        .await;

        assert!(got.is_err());
        assert_eq!(*calls.lock().expect("calls"), 11, "1 call + 10 retries");
        let labels = busy.labels();
        assert_eq!(labels.len(), 10);
        assert_eq!(labels[9], "Request failed — retrying (attempt 10/10)");
    }
}
