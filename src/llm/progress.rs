//! Upload progress (chat/progress.go, T3 design D8): the run loop mints one [`TurnProgress`] per
//! turn and scopes every provider call under the [`TURN_PROGRESS`] task-local; `Client::send_payload`
//! reads [`current`] and, when a reporter is present, streams the request body through a
//! [`ProgressBody`] that reports `(sent, total)` and fires `sent()` once the round-trip returns.
//! Background calls (title, compaction, `list_models`) run outside the scope
//! and see `None` — Go's nil reporter.

use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;

/// The byte-progress handler: `(done, total)`.
pub(crate) type OnSend = Box<dyn Fn(u64, u64) + Send + Sync>;
/// The "request fully sent" handler.
pub(crate) type OnSent = Box<dyn Fn() + Send + Sync>;

/// The per-turn reporter (progress.go:20-60): handlers are installed by the phase watcher and
/// cleared when it drops; a reporter with no handlers is silent.
pub(crate) struct TurnProgress {
    handlers: Mutex<Option<(OnSend, OnSent)>>,
}

impl TurnProgress {
    /// A reporter with no handlers.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            handlers: Mutex::new(None),
        })
    }

    /// Installs (or clears) the handlers.
    pub(crate) fn set_handlers(&self, h: Option<(OnSend, OnSent)>) {
        *self.handlers.lock().unwrap_or_else(PoisonError::into_inner) = h;
    }

    /// Reports `done` of `total` request bytes sent.
    pub(crate) fn send(&self, done: u64, total: u64) {
        if let Some((on_send, _)) = self
            .handlers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            on_send(done, total);
        }
    }

    /// Reports that the round-trip returned (fires on EVERY arm — ok, error, timeout, cancel).
    pub(crate) fn sent(&self) {
        if let Some((_, on_sent)) = self
            .handlers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            on_sent();
        }
    }
}

tokio::task_local! {
    /// The turn's reporter, scoped around each provider call by `stream_round`/`stream_turn`.
    pub(crate) static TURN_PROGRESS: Arc<TurnProgress>;
}

/// The reporter of the enclosing turn, or `None` outside a scope (Go's nil slot).
pub(crate) fn current() -> Option<Arc<TurnProgress>> {
    TURN_PROGRESS.try_with(Arc::clone).ok()
}

/// Bytes per reported chunk (progress.go:93).
pub(crate) const PROGRESS_CHUNK: usize = 64 * 1024;
/// Minimum interval between reports after the first (progress.go:93).
pub(crate) const PROGRESS_THROTTLE: Duration = Duration::from_millis(150);

/// The counting request body: a `Stream` over the payload that reports progress to its reporter
/// (progress.go:78-96). It yields [`PROGRESS_CHUNK`]-sized zero-copy slices; a report goes out on
/// the FINAL byte and otherwise no more often than [`PROGRESS_THROTTLE`] — the first chunk always
/// reports (Go's `last` starts at the zero time).
pub(crate) struct ProgressBody {
    payload: Bytes,
    pos: usize,
    rep: Arc<TurnProgress>,
    last: Option<Instant>,
}

impl ProgressBody {
    /// A body over `payload` reporting to `rep`; the first chunk ALWAYS reports.
    pub(crate) fn new(payload: Bytes, rep: Arc<TurnProgress>) -> Self {
        Self {
            payload,
            pos: 0,
            rep,
            last: None,
        }
    }
}

impl futures::Stream for ProgressBody {
    type Item = std::io::Result<Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let total = self.payload.len();
        if self.pos >= total {
            return Poll::Ready(None);
        }
        let end = self.pos.saturating_add(PROGRESS_CHUNK).min(total);
        let chunk = self.payload.slice(self.pos..end);
        self.pos = end;
        // The final byte always reports, regardless of throttling (progress.go:90).
        let now = Instant::now();
        let due = end == total
            || self
                .last
                .is_none_or(|l| now.duration_since(l) >= PROGRESS_THROTTLE);
        if due {
            self.last = Some(now);
            let (done, total) = (
                u64::try_from(end).unwrap_or(u64::MAX),
                u64::try_from(total).unwrap_or(u64::MAX),
            );
            self.rep.send(done, total);
        }
        Poll::Ready(Some(Ok(chunk)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{PROGRESS_CHUNK, ProgressBody, TURN_PROGRESS, TurnProgress, current};
    use bytes::Bytes;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    /// Drains a body and returns `(the bytes it yielded, every `(done, total)` it reported)`.
    async fn drain(payload: Bytes, rep: &Arc<TurnProgress>) -> (Vec<u8>, Vec<(u64, u64)>) {
        let log: Arc<Mutex<Vec<(u64, u64)>>> = Arc::default();
        let sink = Arc::clone(&log);
        rep.set_handlers(Some((
            Box::new(move |d, t| sink.lock().unwrap().push((d, t))),
            Box::new(|| {}),
        )));
        let mut body = ProgressBody::new(payload, Arc::clone(rep));
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk.expect("body chunk"));
        }
        let reports = log.lock().unwrap().clone();
        (out, reports)
    }

    // Go: chat/progress_test.go:12 TestTurnProgressNilSafety — a plain context carries no slot,
    // and a nil reporter's methods must not panic. The Rust twin: `current()` is `None` outside a
    // scope, and a reporter with no handlers swallows every call.
    #[tokio::test]
    async fn test_turn_progress_nil_safety() {
        assert!(current().is_none(), "a bare task carries no progress slot");
        let bare = TurnProgress::new();
        bare.send(1, 2); // must not panic and must deliver nothing
        bare.sent();
        bare.set_handlers(None);

        let tp = TurnProgress::new();
        let seen = TURN_PROGRESS
            .scope(Arc::clone(&tp), async { current().is_some() })
            .await;
        assert!(seen, "the scoped reporter is not retrievable");
        assert!(current().is_none(), "the scope must not leak");
    }

    // Go: chat/progress_test.go:27 TestProgressBodyReports — the final byte always reports,
    // regardless of throttling; cleared handlers stop deliveries.
    #[tokio::test]
    async fn test_progress_body_reports() {
        let rep = TurnProgress::new();
        let (body, reports) = drain(Bytes::from_static(b"xxxxxxxxxx"), &rep).await;
        assert_eq!(body, b"xxxxxxxxxx");
        assert_eq!(
            reports.last().copied(),
            Some((10, 10)),
            "reports = {reports:?}, want a final done=10 total=10"
        );
        assert_eq!(reports.first().map(|r| r.1), Some(10));

        // Cleared handlers stop deliveries.
        rep.set_handlers(None);
        rep.send(99, 99);
        assert_eq!(reports.last().copied(), Some((10, 10)));
    }

    /// Chunking law: a payload of `2·CHUNK + 1` yields three slices, reassembles byte-for-byte,
    /// and the throttle never suppresses the final report.
    #[tokio::test]
    async fn a_large_body_is_chunked_and_reassembles() {
        let n = PROGRESS_CHUNK * 2 + 1;
        let payload = Bytes::from(vec![b'y'; n]);
        let rep = TurnProgress::new();
        let (body, reports) = drain(payload.clone(), &rep).await;
        assert_eq!(body.len(), n);
        assert_eq!(Bytes::from(body), payload);
        let total = u64::try_from(n).unwrap();
        assert_eq!(reports.first().copied(), Some((65536, total)));
        assert_eq!(reports.last().copied(), Some((total, total)));
    }
}

#[cfg(test)]
mod wire_tests {
    //! The transport half of the reporter (chat/reqlog.go:106-116): the body reaches the server
    //! intact through `ProgressBody`, the upload is counted, and `sent()` fires once per attempt.
    //!
    //! A streaming `reqwest::Body` has no size hint, so `Client::build_request` sets
    //! `Content-Length` explicitly — without it hyper would fall back to `Transfer-Encoding:
    //! chunked`, a wire change Go never made (T3 design §4.4 / spec §5.4 open question 4). This
    //! test is the standing proof that hyper honours the header.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{TURN_PROGRESS, TurnProgress};
    use crate::llm::Client;
    use bytes::Bytes;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    // Go: chat/progress_test.go:68 TestRecordingTransportProgress
    #[tokio::test]
    async fn test_recording_transport_progress() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .and(header("content-length", "2048"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;

        let uploaded: Arc<Mutex<u64>> = Arc::default();
        let sent: Arc<Mutex<usize>> = Arc::default();
        let (up, sn) = (Arc::clone(&uploaded), Arc::clone(&sent));
        let rep = TurnProgress::new();
        rep.set_handlers(Some((
            Box::new(move |done, _| *up.lock().unwrap() = done),
            Box::new(move || *sn.lock().unwrap() += 1),
        )));

        let body = Bytes::from(vec![b'y'; 2048]);
        let client = Client::new(&server.uri(), crate::llm::default_http_client());
        let cancel = CancellationToken::new();
        let answer = TURN_PROGRESS
            .scope(Arc::clone(&rep), async {
                client
                    .send(&cancel, reqwest::Method::POST, "/v1", Some(body.clone()))
                    .await
            })
            .await
            .expect("the request must succeed");
        assert_eq!(answer.status().as_u16(), 200);

        let seen: Vec<Request> = server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, body, "body corrupted through ProgressBody");
        assert!(
            seen[0].headers.get("transfer-encoding").is_none(),
            "an explicit Content-Length must keep the body out of chunked framing"
        );
        assert_eq!(*uploaded.lock().unwrap(), 2048);
        assert_eq!(
            *sent.lock().unwrap(),
            1,
            "sent() must fire once per attempt"
        );
    }
}
