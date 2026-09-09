//! `retry_round` — the per-model-call retry of a tool turn (`chat/retryround_test.go`).
//!
//! Go compressed the backoff by reassigning the `retryBackoff` package var; the Rust unit
//! is a const, so these tests compress TIME instead (tokio's paused clock advances a
//! pending `sleep` the moment the runtime goes idle). The interrupt case needs no clock at
//! all: a cancelled token beats the timer by construction (`biased` select).
//!
//! The upload phase's controller (`repl::phases`: `watch_phases`, `format_byte_size`, the
//! `Phases` clone semantics) is crate-private for the same reason and is unit-tested in
//! `src/repl/phases.rs`; the L3 half — that a turn raises `Busy("Waiting for the model")` — is
//! asserted in `tests/repl/host.rs`.
//!
//! `is_retryable`'s own table — including `TestPermanentErrorNotRetryable` and the
//! historical `\b4\d{2}\b` scan — lives as unit tests in `src/retry.rs`, which is where
//! the crate-private function is (the WP41/WP48 homing precedent). The `RenderSink` state
//! machine and the L3 tool-loop flows live in `tests/toolloop.rs`, which mounts the
//! crate-private modules they need.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use iota::chat::ChatError;
use iota::llm::{LlmError, StatusError};
use iota::provider::error::{ProviderError, WireOp};
use iota::repl::retry_round;
use iota::ui::facade::BusyGuard;
use tokio_util::sync::CancellationToken;

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

// Go: chat/retryround_test.go:28 TestRetryRoundRecoversAndNotices — a transient failure
// re-issues the SAME call and nothing else: 3 calls, 2 busy rows, and EXACTLY ONE dim
// recovery notice (not one per attempt).
#[tokio::test(start_paused = true)]
async fn test_retry_round_recovers_and_notices() {
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

// Go: chat/retryround_test.go:51 TestRetryRoundPermanentErrorNotRetried — a 4xx surfaces
// on the FIRST call; nothing lands in the transcript on the way.
#[tokio::test(start_paused = true)]
async fn test_retry_round_permanent_error_not_retried() {
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

// Go: chat/retryround_test.go:65 TestRetryRoundDisallowedPassesThrough — a dedicated image
// provider bills per attempt, so `allowed = false` passes the first result through
// untouched even for an otherwise transient failure.
#[tokio::test(start_paused = true)]
async fn test_retry_round_disallowed_passes_through() {
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

// Go: chat/retryround_test.go:79 TestRetryRoundInterruptedDuringBackoff — the cancelled
// turn beats the backoff timer (Go pinned it against an HOUR-long one): one call, then
// `Interrupted`, and the busy row is stopped on the way out.
#[tokio::test(start_paused = true)]
async fn test_retry_round_interrupted_during_backoff() {
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

// New (no Go equivalent — `maxRetries` was never exercised end to end): the ladder stops
// at 10 attempts and the last busy row says so.
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
