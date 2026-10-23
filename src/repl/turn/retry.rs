//! `retry_round` + the `is_retryable` table (chat/run.go:1417-1447, chat/chat.go:126-158), with a 429's own
//! rungs (DIVERGENCES X-84).

use std::time::Duration;

use crate::headless::ChatError;
use crate::llm::LlmError;
use crate::llm::client::RETRY_AFTER_CAP;
use crate::provider::error::ProviderError;
use crate::ui::facade::BusyGuard;
use tokio_util::sync::CancellationToken;

use crate::repl::errors::describe_error;

/// Max transient re-attempts of one model call (chat.go:126 `maxRetries`).
pub(crate) const MAX_RETRIES: u32 = 10;

/// One unit of the linear backoff — attempt N waits N of these. Tests compress it through tokio's
/// paused clock instead of a knob.
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Max re-attempts of a call refused with 429 (X-84): a rate limit the next few seconds do not clear is a
/// quota, and the loop's notice gate — not this ladder — decides when to look again.
pub(crate) const RATE_RETRIES: u32 = 3;

/// The 429 backoff when the response named no wait: attempt N waits this × 2ⁿ⁻¹ (2, 4, 8 s).
pub(crate) const RATE_BACKOFF: Duration = Duration::from_secs(2);

/// Which failures one model call may retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retries {
    /// The whole table: 429 on its own rungs, everything else on the linear ladder.
    All,
    /// The loop already holds for a rate limit (X-84): a 429 fails at once — the wire layer's own two
    /// retries are all it gets — while every other transient fault keeps its ladder.
    NoRateLimit,
    /// A dedicated image provider bills per attempt: never auto-retried (run.go:1428).
    None,
}

/// A call the provider refused with 429, and the wait it asked for (X-84).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RateLimit {
    /// The response's `retry-after`, when it sent one.
    pub(crate) retry_after: Option<Duration>,
}

/// The 429 behind `err`, if that is what it is — the status itself, never the body's wording.
pub(crate) fn rate_limit(err: &ChatError) -> Option<RateLimit> {
    let (ChatError::Provider(ProviderError::Wire { source, .. })
    | ChatError::AfterOutput(ProviderError::Wire { source, .. })) = err
    else {
        return None;
    };
    match source.as_ref() {
        LlmError::Status(se) if se.status == 429 => Some(RateLimit {
            retry_after: se.retry_after,
        }),
        _ => None,
    }
}

/// Whole seconds, rounded up: what a busy row says a wait is.
fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// Linear backoff attempt×1s (test-compressible), max 10; a 429 on its own rungs (X-84): at most
/// [`RATE_RETRIES`], waiting its `retry-after` when that is at most [`RETRY_AFTER_CAP`] and 2/4/8 s when
/// there is none, and not at all when it asks for longer. Cancel beats the timer; ONE recovery notice;
/// [`Retries::None`] (image providers) passes through. `round` is an async closure (Rust 2024
/// `AsyncFnMut`): each attempt's future re-borrows the closure's captures — the exact Go per-attempt
/// closure shape (run.go:1426-1447).
///
/// Nothing lands in the transcript until the turn gives up: the current error's
/// classification rides the busy status row (`"%s — retrying (attempt %d/%d)"`, a 429 with a wait
/// `"%s — retrying in %ds (attempt %d/%d)"`), and a recovery leaves exactly one dim
/// `"⟳ %s — recovered after %d attempt(s)"` notice.
/// Interrupt partials travel OUTSIDE the `Result` (the closure's captured buffers);
/// `Err(ChatError::Interrupted)` only signals the class.
pub async fn retry_round<T>(
    cancel: &CancellationToken,
    retries: Retries,
    busy: impl Fn(&str) -> BusyGuard,
    notice: impl Fn(String),
    mut round: impl AsyncFnMut() -> Result<T, ChatError>,
) -> Result<T, ChatError> {
    let mut result = round().await;
    if retries == Retries::None {
        // Dedicated image providers bill per attempt: never auto-retry (run.go:1428).
        return result;
    }
    let mut attempt: u32 = 1;
    while let Err(e) = &result {
        if !is_retryable(e) {
            break;
        }
        let (max, wait, shown) = match rate_limit(e) {
            None => (MAX_RETRIES, RETRY_BACKOFF * attempt, None),
            Some(_) if retries == Retries::NoRateLimit => break,
            // Asked for longer than a minute: a limit with a time on it, not a fault to sleep through.
            Some(RateLimit {
                retry_after: Some(d),
            }) if d > RETRY_AFTER_CAP => break,
            Some(RateLimit {
                retry_after: Some(d),
            }) => (RATE_RETRIES, d, Some(d)),
            Some(RateLimit { retry_after: None }) => (
                RATE_RETRIES,
                RATE_BACKOFF * 2_u32.pow(attempt.saturating_sub(1).min(16)),
                None,
            ),
        };
        if attempt > max {
            break;
        }
        let headline = describe_error(e).headline;
        let label = match shown {
            Some(d) => format!(
                "{headline} — retrying in {}s (attempt {attempt}/{max})",
                ceil_secs(d)
            ),
            None => format!("{headline} — retrying (attempt {attempt}/{max})"),
        };
        let guard = busy(&label);
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                // The cancelled turn wins over the backoff timer (run.go:1434-1437);
                // the partials of the last attempt stay in the closure's captures.
                guard.stop();
                return Err(ChatError::Interrupted);
            }
            () = tokio::time::sleep(wait) => {}
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
/// Non-retryable: user interruption, an output the ceiling cut off (the same request hits it again), the
/// tool-loop caps, a cancelled or provider-declared
/// permanent failure, a failure after the round's text reached the user (`AfterOutput`), a
/// non-streaming stream response (`ErrNoEvents`), a stream that went silent
/// past its idle bound (`StreamIdle`) and HTTP 4xx except 429. Everything else — 429, 5xx,
/// transport faults, malformed frames — retries.
///
/// `StreamIdle` stays out on purpose: it fires after the response head, and the wire layer retries
/// only up to the head. A replay would not run the round's tool calls twice (they run only after
/// its stream ends), but the tool rounds the turn already completed cannot be replayed — the turn
/// keeps them and reports the stall — and every attempt would sit out the whole bound again. The
/// user sees the error (with the knob that widens the bound) and decides.
///
/// `AfterOutput` is the same reasoning for any other fault: the error alone cannot say whether the
/// round's text was already on screen (a cut body, a malformed or in-band tail frame, a missing
/// terminator all arrive after a whole answer as readily as before one), so `stream_round` wraps the
/// failure once text streamed. A replay would put the same answer on screen again — eleven times when
/// the tail fault is deterministic, the GLM loop (X-82). A failure before any text — a thought is
/// only a meter row — still retries.
pub(crate) fn is_retryable(err: &ChatError) -> bool {
    match err {
        ChatError::Interrupted
        | ChatError::Truncated(_)
        | ChatError::AfterOutput(_)
        | ChatError::LocalCap { .. }
        | ChatError::SharedCap { .. }
        | ChatError::Provider(ProviderError::Cancelled | ProviderError::Permanent(_)) => false,
        ChatError::Provider(ProviderError::Wire { source, .. }) => match source.as_ref() {
            LlmError::NoEvents | LlmError::Cancelled | LlmError::StreamIdle(_) => false,
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

    use crate::headless::ChatError;
    use crate::llm::{LlmError, StatusError};
    use crate::provider::error::{PermanentError, ProviderError, WireOp};
    use crate::ui::facade::BusyGuard;
    use tokio_util::sync::CancellationToken;

    use super::{Retries, is_retryable, retry_round};

    fn status(code: u16) -> ChatError {
        ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::Status(StatusError {
                status: code,
                status_text: String::new(),
                method: "POST".to_owned(),
                url: "http://x".to_owned(),
                body: String::new(),
                retry_after: None,
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
        // An output the ceiling cut off hits the same ceiling again (X-69).
        assert!(!is_retryable(&ChatError::Truncated(
            crate::provider::Truncation::default()
        )));
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
        // A failure after the round's text reached the user is reported, never replayed (X-82) —
        // whatever the fault, a transient-looking one included.
        assert!(!is_retryable(&ChatError::AfterOutput(
            ProviderError::other("connection reset by peer")
        )));
        // A stream that went silent after its head is reported, never replayed.
        assert!(!is_retryable(&ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::StreamIdle(std::time::Duration::from_secs(180))
        ))));
        // An in-band stream error carries no status: it retries.
        assert!(is_retryable(&ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::InBand("overloaded_error".to_owned())
        ))));
    }

    /// One Anthropic SSE event.
    fn sse(v: &serde_json::Value) -> String {
        format!(
            "event: {}\ndata: {v}\n\n",
            v["type"].as_str().expect("type")
        )
    }

    /// A WHOLE answer through `message_delta` (`stop_reason` included when `stop` is set) — what the
    /// user sees — and no `message_stop`: each case appends its own tail.
    fn whole_answer(stop: bool) -> String {
        use serde_json::json;
        let delta = if stop {
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}})
        } else {
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":9}})
        };
        [
            json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"the whole answer"}}),
            json!({"type":"content_block_stop","index":0}),
            delta,
        ]
        .iter()
        .map(sse)
        .collect()
    }

    /// Serves ONE request with `body` under a 200 head. `short` > 0 announces that many bytes MORE than
    /// it sends and then closes the connection: the body is cut, a transport fault after the answer.
    async fn serve_once(body: String, short: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut req = Vec::new();
            let mut buf = [0_u8; 4096];
            // The head, then as much body as it announces.
            let (head_end, want) = loop {
                let n = sock.read(&mut buf).await.expect("read");
                req.extend_from_slice(&buf[..n]);
                if let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&req[..i]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse().expect("length"));
                    break (i + 4, len);
                }
            };
            while req.len() < head_end + want {
                let n = sock.read(&mut buf).await.expect("read");
                req.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                body.len() + short
            );
            sock.write_all(head.as_bytes()).await.expect("head");
            sock.write_all(body.as_bytes()).await.expect("body");
            sock.flush().await.expect("flush");
        });
        format!("http://{addr}")
    }

    // Evidence for the replay bug: a round that streamed its WHOLE answer and then failed at the tail.
    // Every tail fault the anthropic dialect can meet after the text is classified `Request failed` (a
    // cut body: `Network error`) and RETRYABLE on its own — the error carries no trace of what already
    // reached the user. Whether it is replayed is therefore the call site's business (`stream_round`
    // marks it delivered, X-82), never this table's.
    #[tokio::test]
    async fn a_whole_answer_with_a_failing_tail_is_retryable_on_its_own() {
        use crate::provider::ToolProvider;
        use crate::repl::errors::describe_error;
        use crate::testing::RecordingSink;
        use serde_json::json;

        let error = sse(&json!({"type":"error","error":{"type":"overloaded_error","message":"x"}}));
        let cases: [(&str, String, usize, &str); 5] = [
            // The relay drops the connection after `stop_reason`, before `message_stop`.
            ("cut body", whole_answer(true), 64, "Network error"),
            // A trailing frame that is not JSON.
            (
                "malformed tail",
                format!(
                    "{}event: message_stop\ndata: {{not json\n\n",
                    whole_answer(true)
                ),
                0,
                "Request failed",
            ),
            // The body ends with no `message_stop` and no `stop_reason` (X-71).
            ("no terminator", whole_answer(false), 0, "Request failed"),
            // The usage figure of the last frame does not parse.
            (
                "bad usage",
                format!(
                    "{}{}",
                    whole_answer(true),
                    sse(
                        &json!({"type":"message_delta","delta":{},"usage":{"output_tokens":"many"}})
                    )
                ),
                0,
                "Request failed",
            ),
            // An in-band error after the answer (a post-hoc filter, an overload).
            (
                "in-band error",
                format!("{}{error}", whole_answer(true)),
                0,
                "Request failed",
            ),
        ];
        for (name, body, short, headline) in cases {
            let url = serve_once(body, short).await;
            let p = crate::provider::anthropic::AnthropicProvider::new(
                "k",
                &url,
                "m",
                None,
                reqwest::Client::new(),
            );
            let mut sink = RecordingSink::default();
            let err = p
                .stream_chat_with_tools(&CancellationToken::new(), &[], &[], &mut sink)
                .await
                .map(|_| ())
                .expect_err(name);
            assert_eq!(
                sink.content(),
                "the whole answer",
                "{name}: delivered first"
            );
            let err = ChatError::Provider(err);
            assert_eq!(describe_error(&err).headline, headline, "{name}: {err}");
            assert!(is_retryable(&err), "{name}: {err}");
        }
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
            Retries::All,
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
            Retries::All,
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
                        retry_after: None,
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

    // A dedicated image provider bills per attempt, so `Retries::None` passes the first result through
    // untouched even for an otherwise transient failure.
    #[tokio::test(start_paused = true)]
    async fn a_disallowed_retry_passes_the_first_result_through() {
        let busy = Busy::default();
        let calls = Mutex::new(0_u32);
        let cancel = CancellationToken::new();

        let got: Result<String, ChatError> = retry_round(
            &cancel,
            Retries::None,
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
            "Retries::None must not retry"
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
            Retries::All,
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
            Retries::All,
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

    // --- the 429 rungs (X-84) --------------------------------------------------------

    /// A 429, asking for `retry_after` when given.
    fn too_many(retry_after: Option<std::time::Duration>) -> ChatError {
        ChatError::Provider(ProviderError::wire(
            WireOp::Stream,
            LlmError::Status(StatusError {
                status: 429,
                status_text: "Too Many Requests".to_owned(),
                method: "POST".to_owned(),
                url: "http://x".to_owned(),
                body: String::new(),
                retry_after,
            }),
        ))
    }

    /// Runs `retry_round` over a call that always fails with `err`, recording when each call was made.
    async fn always(
        retries: Retries,
        err: impl Fn() -> ChatError,
    ) -> (Vec<std::time::Duration>, Vec<String>) {
        let busy = Busy::default();
        let at: Mutex<Vec<std::time::Duration>> = Mutex::default();
        let t0 = tokio::time::Instant::now();
        let cancel = CancellationToken::new();
        let got: Result<(), ChatError> = retry_round(
            &cancel,
            retries,
            |label| busy.guard(label),
            |_| panic!("a call that never recovers must not notice"),
            async || {
                at.lock().expect("at").push(t0.elapsed());
                Err(err())
            },
        )
        .await;
        assert!(got.is_err());
        (at.into_inner().expect("at"), busy.labels())
    }

    fn secs(s: &[u64]) -> Vec<std::time::Duration> {
        s.iter()
            .map(|&n| std::time::Duration::from_secs(n))
            .collect()
    }

    // No wait named: three rungs, 2/4/8 s apart, the busy row counting to 3.
    #[tokio::test(start_paused = true)]
    async fn a_rate_limit_climbs_three_rungs_with_doubling_backoff() {
        let (at, labels) = always(Retries::All, || too_many(None)).await;
        assert_eq!(
            at,
            secs(&[0, 2, 6, 14]),
            "1 call + 3 retries, 2/4/8 s apart"
        );
        assert_eq!(
            labels,
            [1, 2, 3].map(|n| format!("Rate limited (429) — retrying (attempt {n}/3)"))
        );
    }

    // A wait of a minute or less is slept, and the row says how long.
    #[tokio::test(start_paused = true)]
    async fn a_short_retry_after_is_slept_and_shown() {
        let wait = std::time::Duration::from_millis(36_200);
        let (at, labels) = always(Retries::All, || too_many(Some(wait))).await;
        assert_eq!(at.len(), 4);
        assert_eq!(at[1], wait, "the second call waits exactly the retry-after");
        assert_eq!(
            labels[0],
            "Rate limited (429) — retrying in 37s (attempt 1/3)"
        );
        let (at, _) = always(Retries::All, || {
            too_many(Some(std::time::Duration::from_secs(60)))
        })
        .await;
        assert_eq!(at, secs(&[0, 60, 120, 180]), "60 s is still slept");
    }

    // Longer than a minute: one call, no row, no sleep — the gate decides when to look again.
    #[tokio::test(start_paused = true)]
    async fn a_long_retry_after_is_not_slept() {
        let (at, labels) = always(Retries::All, || {
            too_many(Some(std::time::Duration::from_secs(61)))
        })
        .await;
        assert_eq!(at, secs(&[0]));
        assert!(labels.is_empty(), "{labels:?}");
    }

    // Under a hold a 429 fails on its first call; every other transient fault keeps its whole ladder.
    #[tokio::test(start_paused = true)]
    async fn under_a_hold_a_rate_limit_is_not_retried() {
        let (at, labels) = always(Retries::NoRateLimit, || too_many(None)).await;
        assert_eq!(at, secs(&[0]));
        assert!(labels.is_empty(), "{labels:?}");
        let (at, labels) = always(Retries::NoRateLimit, || status(503)).await;
        assert_eq!(at.len(), 11, "a 5xx still climbs 1 + 10");
        assert_eq!(
            labels[9],
            "Provider server error (503) — retrying (attempt 10/10)"
        );
        let (at, _) = always(Retries::None, || too_many(None)).await;
        assert_eq!(at, secs(&[0]));
    }

    // The 429 backoff yields to an interrupt like the linear one does.
    #[tokio::test(start_paused = true)]
    async fn an_interrupt_during_a_rate_limit_backoff_wins_over_the_timer() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let calls = Mutex::new(0_u32);
        let got: Result<(), ChatError> = retry_round(
            &cancel,
            Retries::All,
            |_| BusyGuard::new(|| {}),
            |_| panic!("an interrupted retry must not notice"),
            async || {
                *calls.lock().expect("calls") += 1;
                Err(too_many(Some(std::time::Duration::from_secs(30))))
            },
        )
        .await;
        assert!(matches!(got, Err(ChatError::Interrupted)));
        assert_eq!(*calls.lock().expect("calls"), 1);
    }

    // What counts as a rate limit is the status alone, wherever the error carries it.
    #[test]
    fn rate_limit_reads_the_status_and_its_wait() {
        use super::{RateLimit, rate_limit};
        let wait = Some(std::time::Duration::from_secs(9));
        assert_eq!(
            rate_limit(&too_many(wait)),
            Some(RateLimit { retry_after: wait })
        );
        let ChatError::Provider(inner) = too_many(None) else {
            unreachable!()
        };
        assert_eq!(
            rate_limit(&ChatError::AfterOutput(inner)),
            Some(RateLimit { retry_after: None })
        );
        assert_eq!(rate_limit(&status(503)), None);
        assert_eq!(rate_limit(&transient()), None);
        assert_eq!(rate_limit(&ChatError::Interrupted), None);
    }
}
