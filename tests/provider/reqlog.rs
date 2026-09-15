//! The `/debug` request log over wiremock (WP66; `T3_TEST_PLAN` §4): capture before the round-trip,
//! recording off, the progressive SSE tee, the retry pair, `fetch_bare`.
//!
//! Go drives these through `RequestLog.HTTPClient()` (an `http.RoundTripper`); the Rust seam sits at
//! the ONE execute point inside `llm::Client`, so every test here goes through the public client (or
//! a provider built on it) rather than through a transport.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use iota::llm::Client;
use iota::llm::client::NoJitter;
use iota::llm::reqlog::{REQ_LOG_MAX_BODY, RequestLog};
use iota::provider::images::ImagesProvider;
use iota::provider::model::Message;
use iota::provider::{HttpTransport, Provider};
use reqwest::Method;
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// A recording client over `server` with deterministic backoff.
fn recording(server: &MockServer, log: &Arc<RequestLog>) -> Client {
    Client::new(&server.uri(), reqwest::Client::new())
        .with_jitter(Arc::new(NoJitter))
        .with_recorder(Arc::clone(log))
}

/// A log with recording already ON.
fn verbose_log() -> Arc<RequestLog> {
    let log = Arc::new(RequestLog::new());
    log.set_verbose(true);
    log
}

/// A real round-trip through the recording
/// client captures the method, URL, request body and status, and the response body only as the
/// caller reads it (the capture is a TEE, not a buffer).
#[tokio::test]
async fn the_request_log_captures_a_real_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/thing"))
        .respond_with(|req: &Request| {
            assert_eq!(req.body, b"ping", "the server must see the original body");
            ResponseTemplate::new(201).set_body_string("pong")
        })
        .mount(&server)
        .await;

    let log = verbose_log();
    let client = recording(&server, &log);
    let resp = client
        .send(
            &CancellationToken::new(),
            Method::POST,
            "/v1/thing",
            Some(Bytes::from_static(b"ping")),
        )
        .await
        .expect("post");

    // The entry exists BEFORE the body is read: Go adds it before the round-trip so a pending row
    // shows up in the inspector immediately.
    let entries = log.entries();
    assert_eq!(
        entries.len(),
        1,
        "captured {} entries, want 1",
        entries.len()
    );
    let e = &entries[0];
    assert_eq!(e.method, "POST");
    assert!(e.url.ends_with("/v1/thing"), "entry url = {}", e.url);
    assert_eq!(e.req_body, b"ping");
    let pending = e.response();
    assert!(
        pending.status.starts_with("201"),
        "status = {:?}, want 201…",
        pending.status
    );
    assert!(
        pending.resp_body.is_empty(),
        "the tee must not pre-read the body"
    );
    assert!(pending.duration.is_zero(), "duration lands with the body");

    let body = resp.bytes().await.expect("read body");
    assert_eq!(&body[..], b"pong", "the caller still gets every byte");
    let done = e.response();
    assert_eq!(
        done.resp_body, b"pong",
        "the tee captured the streamed body"
    );
    assert!(
        !done.duration.is_zero(),
        "the duration lands at end of body"
    );
    assert!(done.err.is_none());
}

/// With recording off (the default) the
/// client passes straight through and captures nothing. The rebuilt response is skipped entirely,
/// so `url()` is still reqwest's own.
#[tokio::test]
async fn with_recording_off_nothing_is_captured() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let log = Arc::new(RequestLog::new()); // verbose off by default
    let resp = recording(&server, &log)
        .send(&CancellationToken::new(), Method::GET, "/v1/models", None)
        .await
        .expect("get");
    assert!(
        resp.url().as_str().ends_with("/v1/models"),
        "an unrecorded response keeps reqwest's own url: {}",
        resp.url()
    );
    let body = resp.bytes().await.expect("read body");
    assert_eq!(&body[..], b"ok");
    assert!(
        log.entries().is_empty(),
        "captured {} entries with recording off, want 0",
        log.entries().len()
    );
}

/// A client with NO recorder at all — every existing provider and wire test — records nothing and,
/// like the switched-off log, never has its response rebuilt.
#[tokio::test]
async fn a_client_without_a_recorder_is_inert() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let resp = Client::new(&server.uri(), reqwest::Client::new())
        .send(&CancellationToken::new(), Method::GET, "/v1/models", None)
        .await
        .expect("get");
    assert!(resp.url().as_str().ends_with("/v1/models"));
}

/// The retry loop sits ABOVE the capture, so EVERY attempt is its own entry — newest first, both
/// carrying the same URL and each its own status (Go's transport records per `RoundTrip` the same
/// way, client.go:105-113).
#[tokio::test]
async fn every_attempt_is_its_own_entry() {
    let server = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |_: &Request| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
                    .insert_header("Retry-After-Ms", "1")
                    .set_body_string(r#"{"error":{"message":"busy"}}"#)
            } else {
                ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#)
            }
        })
        .mount(&server)
        .await;

    let log = verbose_log();
    let resp = recording(&server, &log)
        .send(
            &CancellationToken::new(),
            Method::POST,
            "/v1/chat/completions",
            Some(Bytes::from_static(
                br#"{"messages":[{"role":"user","content":"hi"}]}"#,
            )),
        )
        .await
        .expect("the retry recovered");
    resp.bytes().await.expect("read body");

    assert_eq!(hits.load(Ordering::SeqCst), 2, "two attempts");
    let entries = log.entries();
    assert_eq!(entries.len(), 2, "two attempts = two entries");
    assert_eq!(entries[0].response().status, "200 OK", "newest first");
    assert_eq!(entries[1].response().status, "503 Service Unavailable");
    assert!(
        entries
            .iter()
            .all(|e| e.url.ends_with("/v1/chat/completions"))
    );
    assert_eq!(
        entries[0].summary, "hi",
        "the summary is the last user text"
    );
    // The failed attempt's error body rides through the same tee (the retry loop reads it to build
    // the `StatusError`).
    assert_eq!(
        entries[1].response().resp_body,
        br#"{"error":{"message":"busy"}}"#
    );
}

/// A failed round-trip records the error text and a duration instead of a status
/// (reqlog.go:117-125). The header timeout is the deterministic, network-free way to reach that
/// arm — a connection refusal would need a port this test cannot own.
#[tokio::test]
async fn a_failed_round_trip_is_recorded_as_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let log = verbose_log();
    let client = recording(&server, &log)
        .with_retries(0)
        .with_header_timeout(Duration::from_millis(50));
    let err = client
        .send(&CancellationToken::new(), Method::GET, "/v1/models", None)
        .await
        .expect_err("the header timeout fired");
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    let resp = entries[0].response();
    assert!(resp.status.is_empty(), "no status ever arrived: {resp:?}");
    assert_eq!(
        resp.err.as_deref(),
        Some(err.to_string().as_str()),
        "the failure text is recorded"
    );
    assert_eq!(
        resp.err.as_deref(),
        Some("response headers not received within 50ms")
    );
    assert!(!resp.duration.is_zero());
}

/// The streaming tee: an SSE body fills the entry AS the reader consumes it, and `Drop` is the
/// close backstop for a consumer that stops at the terminal event without draining to EOF (Go's
/// `recordingBody.Close`, reqlog.go:165-175).
#[tokio::test]
async fn an_sse_body_is_captured_and_the_drop_backstops_the_duration() {
    const TRANSCRIPT: &str = "data: one\n\ndata: two\n\ndata: [DONE]\n\n";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TRANSCRIPT, "text/event-stream"))
        .mount(&server)
        .await;

    let log = verbose_log();
    let client = recording(&server, &log);
    let mut sse = client
        .stream(
            &CancellationToken::new(),
            Method::POST,
            "/v1/messages",
            Some(&serde_json::json!({"messages": [{"role": "user", "content": "hi"}]})),
        )
        .await
        .expect("stream");

    let entry = log.entries().into_iter().next().expect("one entry");
    assert_eq!(entry.summary, "hi");
    assert!(
        entry.response().resp_body.is_empty(),
        "nothing is captured before the first read"
    );

    let first = sse.next().await.expect("first event").expect("some");
    assert_eq!(first.data, b"one");
    let mid = entry.response();
    assert!(
        !mid.resp_body.is_empty(),
        "the tee fills in while the stream is read"
    );
    assert!(
        mid.duration.is_zero(),
        "the duration is not stamped mid-stream"
    );

    // Stop at the terminal event without draining — exactly what the SDK does.
    drop(sse);
    assert!(
        !entry.response().duration.is_zero(),
        "Drop is the Close backstop"
    );
}

/// `fetch_bare` — the relay image GET both image dialects use — records like any other request:
/// no auth header on the wire, and the entry carries the GET and its absolute URL (whose last path
/// segment is what the `/debug` row shows as the action).
#[tokio::test]
async fn the_relay_image_fetch_is_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![9u8, 9], "image/png"))
        .mount(&server)
        .await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(r#"{{"data":[{{"url":"{uri}/blob.png"}}]}}"#),
            "application/json",
        ))
        .mount(&server)
        .await;

    let log = verbose_log();
    let transport = HttpTransport {
        client: reqwest::Client::new(),
        recorder: Some(Arc::clone(&log)),
    };
    let provider = ImagesProvider::new("k", &server.uri(), "gpt-image-1", transport);
    let out = provider
        .chat(&CancellationToken::new(), &[Message::user("a red fox")])
        .await
        .expect("generate");
    assert_eq!(out.images.len(), 1);

    let entries = log.entries();
    assert_eq!(entries.len(), 2, "the generation and the relay GET");
    // Newest first: the relay GET, then the generation that produced its URL.
    assert_eq!(entries[0].method, "GET");
    assert!(entries[0].url.ends_with("/blob.png"), "{}", entries[0].url);
    assert_eq!(entries[0].summary, "", "a blob GET carries no user text");
    assert_eq!(entries[1].method, "POST");
    assert!(entries[1].url.ends_with("/images/generations"));
    assert_eq!(entries[1].summary, "a red fox", "the images `prompt` field");
    assert_eq!(entries[0].response().status, "200 OK");
    assert_eq!(entries[0].response().resp_body, vec![9u8, 9]);
}

/// Both bodies are cut at 256 KiB, request and response alike (reqlog.go:158-166).
#[tokio::test]
async fn oversized_bodies_are_capped_on_both_halves() {
    let big = "x".repeat(REQ_LOG_MAX_BODY + 4096);
    let server = MockServer::start().await;
    let reply = big.clone();
    Mock::given(method("POST"))
        .and(path("/v1/thing"))
        .respond_with(move |_: &Request| ResponseTemplate::new(200).set_body_string(reply.clone()))
        .mount(&server)
        .await;

    let log = verbose_log();
    let resp = recording(&server, &log)
        .send(
            &CancellationToken::new(),
            Method::POST,
            "/v1/thing",
            Some(Bytes::from(big.clone())),
        )
        .await
        .expect("post");
    let got = resp.bytes().await.expect("read body");
    assert_eq!(got.len(), big.len(), "the caller still gets every byte");

    let e = log.entries().into_iter().next().expect("one entry");
    assert_eq!(e.req_body.len(), REQ_LOG_MAX_BODY);
    assert_eq!(e.response().resp_body.len(), REQ_LOG_MAX_BODY);
}

/// Recording never changes what the caller receives: the rebuilt response carries the same status,
/// headers and bytes (`T3_DESIGN` §10 risk 2). What it does NOT carry is `url()` — see the in-file
/// `rebuilt_response_keeps_everything_but_the_url` unit for that half of the seam.
#[tokio::test]
async fn the_rebuilt_response_is_transparent_to_the_caller() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-request-id", "abc123")
                .set_body_raw(r#"{"data":[]}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let log = verbose_log();
    let resp = recording(&server, &log)
        .send(&CancellationToken::new(), Method::GET, "/v1/models", None)
        .await
        .expect("get");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok()),
        Some("abc123"),
        "headers survive the rebuild"
    );
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body = resp.bytes().await.expect("read body");
    assert_eq!(&body[..], br#"{"data":[]}"#);
}

/// Cancelling mid-flight leaves an ERROR row, not a row pending forever: Go's `RoundTrip` returns
/// the context error and the transport records it.
#[tokio::test]
async fn a_cancelled_attempt_is_recorded_as_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let log = verbose_log();
    let cancel = CancellationToken::new();
    let client = recording(&server, &log);
    let fetch = async {
        client
            .send(&cancel, Method::GET, "/v1/models", None)
            .await
            .expect_err("cancelled")
    };
    let fire = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    };
    let (err, ()) = tokio::join!(fetch, fire);
    assert_eq!(err.to_string(), "interrupted");
    let e = log.entries().into_iter().next().expect("one entry");
    assert_eq!(e.response().err.as_deref(), Some("interrupted"));
    assert!(!e.response().duration.is_zero());
}
