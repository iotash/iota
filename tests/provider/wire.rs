//! Wire client tests (`internal/llm/llm_test.go` plus the WP02 additions): the SSE grammar, the retry policy,
//! cancellation, the no-events / in-band stream errors, `StatusError` shaping, the retry-delay precedence, the
//! header timeout and the shared `GET /models` listing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use crate::common::{mock_json, mock_sse, sse_stream};
use bytes::Bytes;
use futures::StreamExt;
use iota::llm::client::{
    BACKOFF_BASE, BACKOFF_CAP, ERROR_DETAIL_CAP, Jitter, NoJitter, StatusError, retry_delay,
    should_retry,
};
use iota::llm::{Client, Event, LlmError, Sse};
use iota::provider::model::Raw;
use reqwest::{
    Method,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// A client over `server` with no jitter (deterministic backoff).
fn client(server: &MockServer) -> Client {
    Client::new(&server.uri(), reqwest::Client::new()).with_jitter(Arc::new(NoJitter))
}

/// The chat-completions mapping over a raw `Sse` (chatcomp.go:193-209), so the wire tests stay independent of
/// the dialect packages: `Ok(None) && !saw_event()` → `NoEvents`; a non-null `error` → `InBand(raw)`.
async fn dialect_next(sse: &mut Sse) -> Result<Option<Vec<u8>>, LlmError> {
    #[derive(Deserialize)]
    struct Chunk {
        #[serde(default)]
        error: Option<Raw>,
    }
    let Some(evt) = sse.next().await? else {
        return if sse.saw_event() {
            Ok(None)
        } else {
            Err(LlmError::NoEvents)
        };
    };
    let chunk: Chunk = serde_json::from_slice(&evt.data).map_err(LlmError::MalformedChunk)?;
    if let Some(e) = chunk.error {
        return Err(LlmError::InBand(e.get().to_owned()));
    }
    Ok(Some(evt.data))
}

// Go: internal/llm/llm_test.go:18
#[tokio::test]
async fn test_sse_parsing() {
    const RAW: &str = ": comment\n\
        event: ping\ndata: {\"a\":1}\n\n\
        data: line1\ndata:line2\n\n\
        data: [DONE]\n\n\
        ignored: field\n";
    let mut s = Sse::new(sse_stream(RAW), CancellationToken::new());

    let evt = s.next().await.unwrap().expect("event 1");
    assert_eq!(
        evt,
        Event {
            kind: "ping".into(),
            data: br#"{"a":1}"#.to_vec()
        }
    );
    let evt = s.next().await.unwrap().expect("event 2");
    assert_eq!(evt.kind, "", "event type does not persist across events");
    assert_eq!(evt.data, b"line1\nline2", "multi-line data join");
    assert!(
        s.next().await.unwrap().is_none(),
        "expected EOF after [DONE] drain"
    );
    assert!(s.done() && s.saw_event(), "Done/SawEvent not set");
    assert!(
        s.next().await.unwrap().is_none(),
        "EOF is sticky once the body is exhausted"
    );

    // No trailing blank line: the last event still dispatches at EOF.
    let mut s2 = Sse::new(sse_stream("data: tail"), CancellationToken::new());
    let evt = s2.next().await.unwrap().expect("unterminated final event");
    assert_eq!(evt.data, b"tail");
    assert!(s2.saw_event() && !s2.done());
    assert!(s2.next().await.unwrap().is_none());

    // An unterminated `data: [DONE]` does NOT set done (sse.go:89-93) — the stream reads as event-less.
    let mut s3 = Sse::new(sse_stream("data: [DONE]"), CancellationToken::new());
    assert!(s3.next().await.unwrap().is_none());
    assert!(!s3.done() && !s3.saw_event());

    // CRLF line endings, an `event:` line without data (dropped), a comment as the unterminated last line,
    // and a field with no colon are all handled per sse.go.
    let mut s4 = Sse::new(
        sse_stream(
            "event: lonely\r\n\r\ndata: a\r\ndata\r\nretry: 5\r\n\r\ndata: b\n: trailing comment",
        ),
        CancellationToken::new(),
    );
    let evt = s4.next().await.unwrap().unwrap();
    assert_eq!(evt.kind, "", "an event line with no data is dropped");
    assert_eq!(
        evt.data, b"a\n",
        "a bare `data` line contributes an empty value"
    );
    let evt = s4.next().await.unwrap().unwrap();
    assert_eq!(evt.data, b"b");
    assert!(s4.next().await.unwrap().is_none());
}

// Go: internal/llm/llm_test.go:51
#[tokio::test]
async fn test_retry_policy() {
    let cancel = CancellationToken::new();
    let body = serde_json::json!({});

    // 429 + Retry-After-Ms recovers on the second attempt.
    let srv = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/x"))
        .respond_with(move |_: &Request| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429)
                    .insert_header("Retry-After-Ms", "1")
                    .set_body_string(r#"{"error":{"message":"slow down"}}"#)
            } else {
                ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#)
            }
        })
        .mount(&srv)
        .await;
    let out: serde_json::Value = client(&srv)
        .do_json(&cancel, Method::POST, "/x", Some(&body))
        .await
        .expect("retry did not recover");
    assert_eq!(out, serde_json::json!({"ok": true}));
    assert_eq!(hits.load(Ordering::SeqCst), 2, "hits");

    // 400 is terminal, error is structured, status code lands in Display.
    let srv2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/x"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":{"message":"bad","type":"invalid_request_error"}}"#),
        )
        .mount(&srv2)
        .await;
    let err = client(&srv2)
        .do_json::<_, serde_json::Value>(&cancel, Method::POST, "/x", Some(&body))
        .await
        .unwrap_err();
    let se = err.status().expect("StatusError");
    assert_eq!(se.status, 400);
    assert_eq!(srv2.received_requests().await.unwrap().len(), 1);
    let text = err.to_string();
    assert!(text.contains("400 Bad Request"), "{text}");
    assert!(text.contains("invalid_request_error"), "{text}");
    assert_eq!(
        text,
        format!(
            "POST {:?}: 400 Bad Request {{\"message\":\"bad\",\"type\":\"invalid_request_error\"}}",
            format!("{}/x", srv2.uri())
        )
    );
    assert_eq!(
        *se,
        StatusError {
            status: 400,
            status_text: "Bad Request".into(),
            method: "POST".into(),
            url: format!("{}/x", srv2.uri()),
            body: r#"{"message":"bad","type":"invalid_request_error"}"#.into(),
        }
    );

    // x-should-retry: false overrides a retryable status.
    let srv3 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(500).insert_header("x-should-retry", "false"))
        .mount(&srv3)
        .await;
    let err = client(&srv3)
        .do_json::<_, serde_json::Value>(&cancel, Method::POST, "/x", Some(&body))
        .await
        .unwrap_err();
    assert_eq!(err.status().map(|s| s.status), Some(500));
    assert_eq!(
        srv3.received_requests().await.unwrap().len(),
        1,
        "x-should-retry:false ignored"
    );

    // Retries are exhausted after `retries` extra attempts (3 hits for the default of 2).
    let srv4 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After-Ms", "1"))
        .mount(&srv4)
        .await;
    let err = client(&srv4)
        .do_json::<_, serde_json::Value>(&cancel, Method::POST, "/x", Some(&body))
        .await
        .unwrap_err();
    assert_eq!(err.status().map(|s| s.status), Some(503));
    assert_eq!(srv4.received_requests().await.unwrap().len(), 3);
}

// Go: internal/llm/llm_test.go:108
#[tokio::test]
async fn test_stream_cancellation() {
    const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";

    // Cancelling while the response head is pending aborts the request promptly.
    let srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(SSE, "text/event-stream")
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&srv)
        .await;
    let c = client(&srv);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let pending = tokio::spawn(async move {
        c.stream(
            &token,
            Method::POST,
            "/stream",
            Some(&serde_json::json!({"model": "m"})),
        )
        .await
        .map(|_| ())
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("stream() did not return after cancel — interrupt would freeze")
        .unwrap();
    assert!(matches!(res, Err(LlmError::Cancelled)), "{res:?}");

    // Cancelling mid-stream aborts the pending body read (the interrupt path).
    let held_open = futures::stream::iter([Ok(Bytes::from_static(SSE.as_bytes()))])
        .chain(futures::stream::pending());
    let cancel = CancellationToken::new();
    let mut sse = Sse::new(held_open, cancel.clone());
    let first = sse.next().await.unwrap().expect("first chunk");
    assert!(
        std::str::from_utf8(&first.data).unwrap().contains("\"hi\""),
        "first chunk: {first:?}"
    );
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    let res = tokio::time::timeout(Duration::from_secs(2), sse.next())
        .await
        .expect("sse.next did not return after cancel — interrupt would freeze");
    assert!(
        matches!(res, Err(LlmError::Cancelled)),
        "cancel surfaced as {res:?}, want Cancelled"
    );
}

// Go: internal/llm/llm_test.go:146
#[tokio::test]
async fn test_no_events_stream() {
    let srv = MockServer::start().await;
    mock_json(
        &srv,
        "POST",
        "/chat/completions",
        serde_json::json!({"choices":[{"message":{"content":"not a stream"}}]}),
    )
    .await;
    let cancel = CancellationToken::new();
    let mut sse = client(&srv)
        .stream(
            &cancel,
            Method::POST,
            "/chat/completions",
            Some(&serde_json::json!({"model": "m"})),
        )
        .await
        .unwrap();
    let err = loop {
        match dialect_next(&mut sse).await {
            Ok(_) => {}
            Err(e) => break e,
        }
    };
    assert!(matches!(err, LlmError::NoEvents), "{err}");
    assert_eq!(
        err.to_string(),
        "stream ended without any SSE events (server did not stream?)"
    );
    assert!(!sse.saw_event());
}

// Go: internal/llm/llm_test.go:169
#[tokio::test]
async fn test_in_band_stream_error() {
    let srv = MockServer::start().await;
    mock_sse(
        &srv,
        "POST",
        "/chat/completions",
        "data: {\"error\":{\"message\":\"boom\"}}\n\n",
    )
    .await;
    let cancel = CancellationToken::new();
    let mut sse = client(&srv)
        .stream(
            &cancel,
            Method::POST,
            "/chat/completions",
            Some(&serde_json::json!({"model": "m"})),
        )
        .await
        .unwrap();
    let err = dialect_next(&mut sse).await.unwrap_err();
    assert!(
        err.to_string().contains("boom"),
        "in-band error not surfaced: {err}"
    );
    assert_eq!(
        err.to_string(),
        r#"received error while streaming: {"message":"boom"}"#
    );
    assert!(matches!(err, LlmError::InBand(_)));
}

/// F-05: a literal `"error": null` in an error body is ABSENT — the detail is the trimmed body, not `null`.
#[tokio::test]
async fn status_error_null_envelope_is_absent() {
    let srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/null"))
        .respond_with(ResponseTemplate::new(400).set_body_string("\n {\"error\":null} \n"))
        .mount(&srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/text"))
        .respond_with(ResponseTemplate::new(418).set_body_string("  I'm a teapot  "))
        .mount(&srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/string"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"nope","x":1}"#))
        .mount(&srv)
        .await;
    let long = "x".repeat(ERROR_DETAIL_CAP + 100);
    Mock::given(method("POST"))
        .and(path("/long"))
        .respond_with(ResponseTemplate::new(400).set_body_string(long))
        .mount(&srv)
        .await;
    let c = client(&srv);
    let cancel = CancellationToken::new();
    let body = serde_json::json!({});
    let status = |p: &'static str| {
        let c = c.clone();
        let cancel = cancel.clone();
        let body = body.clone();
        async move {
            let err = c
                .do_json::<_, serde_json::Value>(&cancel, Method::POST, p, Some(&body))
                .await
                .unwrap_err();
            err.status().expect("StatusError").clone()
        }
    };

    let se = status("/null").await;
    assert_eq!(se.body, r#"{"error":null}"#);
    assert_eq!(se.status, 400);
    assert_eq!(se.status_text, "Bad Request");
    assert_eq!(se.method, "POST");
    assert_eq!(se.url, format!("{}/null", srv.uri()));

    let se = status("/text").await;
    assert_eq!(se.body, "I'm a teapot");
    assert_eq!(se.status_text, "I'm a teapot");

    let se = status("/string").await;
    assert_eq!(
        se.body, r#""nope""#,
        "any JSON value under `error` is taken verbatim"
    );

    let se = status("/long").await;
    assert_eq!(se.body.len(), ERROR_DETAIL_CAP + "…".len());
    assert!(se.body.ends_with('…'));
    assert_eq!(
        srv.received_requests().await.unwrap().len(),
        4,
        "4xx is never retried"
    );
}

/// A jitter that always takes the whole allowance (the `0.75d` floor).
struct MaxJitter;

impl Jitter for MaxJitter {
    fn jitter(&self, max: Duration) -> Duration {
        max
    }
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

fn status(code: u16) -> LlmError {
    LlmError::Status(StatusError {
        status: code,
        status_text: String::new(),
        method: "POST".into(),
        url: "http://h/x".into(),
        body: String::new(),
    })
}

/// client.go:188-212 and 171-186 as a pure table (NoJitter): `retry-after-ms` > `retry-after` seconds >
/// `retry-after` HTTP-date > backoff; the retry classification.
#[test]
fn retry_delay_precedence_table() {
    let ms = Duration::from_millis;
    let secs = Duration::from_secs;
    let rd = |h: Option<&HeaderMap>, attempt: u32| retry_delay(h, attempt, &NoJitter);

    // retry-after-ms wins over everything.
    assert_eq!(
        rd(
            Some(&headers(&[("retry-after-ms", "250"), ("retry-after", "3")])),
            0
        ),
        ms(250)
    );
    // A non-positive or unparseable retry-after-ms falls through to retry-after seconds (float).
    assert_eq!(
        rd(
            Some(&headers(&[("retry-after-ms", "0"), ("retry-after", "1.5")])),
            0
        ),
        ms(1500)
    );
    assert_eq!(
        rd(
            Some(&headers(&[("retry-after-ms", "abc"), ("retry-after", "2")])),
            0
        ),
        secs(2)
    );
    // Non-positive seconds fall through to the HTTP-date parse, then to backoff.
    assert_eq!(rd(Some(&headers(&[("retry-after", "-1")])), 0), ms(500));
    assert_eq!(rd(Some(&headers(&[("retry-after", "0")])), 1), secs(1));
    // An HTTP-date in the future yields the remaining time; one in the past falls through to backoff.
    let future = httpdate::fmt_http_date(SystemTime::now() + secs(60));
    let d = rd(Some(&headers(&[("retry-after", &future)])), 0);
    assert!(d > secs(55) && d <= secs(60), "{d:?}");
    let past = httpdate::fmt_http_date(SystemTime::now() - secs(60));
    assert_eq!(rd(Some(&headers(&[("retry-after", &past)])), 2), secs(2));
    assert_eq!(rd(Some(&headers(&[("retry-after", "later")])), 0), ms(500));

    // Backoff: 500ms << attempt capped at 8s; transport failures (no headers) always back off.
    assert_eq!(rd(None, 0), BACKOFF_BASE);
    assert_eq!(rd(None, 1), secs(1));
    assert_eq!(rd(None, 2), secs(2));
    assert_eq!(rd(None, 3), secs(4));
    assert_eq!(rd(None, 4), BACKOFF_CAP);
    assert_eq!(rd(None, 5), BACKOFF_CAP);
    assert_eq!(rd(None, 40), BACKOFF_CAP);
    assert_eq!(rd(Some(&HeaderMap::new()), 0), ms(500));
    // Jitter subtracts up to d/4: the sleep lies in [0.75d, d].
    assert_eq!(retry_delay(None, 1, &MaxJitter), ms(750));
    assert_eq!(retry_delay(None, 4, &MaxJitter), secs(6));
    let r = retry_delay(None, 0, &iota::llm::RandJitter);
    assert!(r >= ms(375) && r <= ms(500), "{r:?}");

    // Classification: 408/409/429/5xx retry, other 4xx do not, x-should-retry overrides either way.
    let none: Option<&HeaderMap> = None;
    for code in [408, 409, 429, 500, 502, 503, 599] {
        assert!(should_retry(&status(code), none), "{code}");
    }
    for code in [400, 401, 403, 404, 422] {
        assert!(!should_retry(&status(code), none), "{code}");
    }
    assert!(!should_retry(
        &status(500),
        Some(&headers(&[("x-should-retry", "false")]))
    ));
    assert!(should_retry(
        &status(400),
        Some(&headers(&[("x-should-retry", "true")]))
    ));
    assert!(should_retry(
        &status(500),
        Some(&headers(&[("x-should-retry", "maybe")]))
    ));
    assert!(should_retry(&LlmError::HeaderTimeout(ms(200)), none));
    assert!(!should_retry(&LlmError::NoEvents, none));
    assert!(!should_retry(&LlmError::Cancelled, none));
}

/// POLICY I-02: the response-header timeout is its own variant, classified like a transport error (retried,
/// not a list fallback) and printed with Go's duration text.
#[tokio::test]
async fn header_timeout_is_transport_error() {
    let srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_secs(1)),
        )
        .mount(&srv)
        .await;
    let c = client(&srv)
        .with_header_timeout(Duration::from_millis(200))
        .with_retries(1);
    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let err = c
        .get_json::<serde_json::Value>(&cancel, "/slow")
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::HeaderTimeout(d) if d == Duration::from_millis(200)),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "response headers not received within 200ms"
    );
    assert!(!err.is_list_fallback());
    assert!(err.status().is_none());
    assert_eq!(
        srv.received_requests().await.unwrap().len(),
        2,
        "retried retries+1 times"
    );
    // Two 200 ms waits plus one 500 ms backoff (NoJitter), never the 1 s the server takes.
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );
}

/// `wire::models::openai_model_ids`: ids sorted bytewise; a GET carries no body and no Content-Type.
#[tokio::test]
async fn openai_model_ids_sorted() {
    use iota::llm::models::openai_model_ids;

    let srv = MockServer::start().await;
    mock_json(
        &srv,
        "GET",
        "/models",
        serde_json::json!({"data":[{"id":"gpt-b"},{"id":"gpt-a"},{"id":"Zeta"},{"object":"model"}]}),
    )
    .await;
    let c = client(&srv);
    let cancel = CancellationToken::new();
    let ids = openai_model_ids(&c, &cancel).await.unwrap();
    assert_eq!(ids, vec!["", "Zeta", "gpt-a", "gpt-b"]);
    let reqs = srv.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].headers.get("content-type").is_none());
    assert!(reqs[0].body.is_empty());

    let empty = MockServer::start().await;
    mock_json(&empty, "GET", "/models", serde_json::json!({})).await;
    assert!(
        openai_model_ids(&client(&empty), &cancel)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Base URL trimming, static headers appended per attempt (`Header.Add`), `Content-Type` only with a body,
/// the per-attempt auth hook and its non-retried failure, and a non-JSON 2xx as `Decode`.
#[tokio::test]
async fn client_request_shape() {
    let srv = MockServer::start().await;
    mock_json(&srv, "POST", "/v1/x", serde_json::json!({"ok": 1})).await;
    Mock::given(method("GET"))
        .and(path("/v1/html"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<!DOCTYPE html><html>landing</html>"),
        )
        .mount(&srv)
        .await;
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hook_calls);
    let c = Client::new(&format!("{}/v1///", srv.uri()), reqwest::Client::new())
        .with_header(
            HeaderName::from_static("x-static"),
            HeaderValue::from_static("one"),
        )
        .with_header(
            HeaderName::from_static("x-static"),
            HeaderValue::from_static("two"),
        )
        .with_auth(Arc::new(move |req| {
            counter.fetch_add(1, Ordering::SeqCst);
            req.headers_mut()
                .insert("authorization", HeaderValue::from_static("Bearer tok"));
            Ok(())
        }));
    assert_eq!(c.base_url(), format!("{}/v1", srv.uri()));
    let cancel = CancellationToken::new();

    let out: serde_json::Value = c
        .do_json(
            &cancel,
            Method::POST,
            "/x",
            Some(&serde_json::json!({"q": "<b>&"})),
        )
        .await
        .unwrap();
    assert_eq!(out, serde_json::json!({"ok": 1}));
    let reqs = srv.received_requests().await.unwrap();
    let req = &reqs[0];
    assert_eq!(req.url.path(), "/v1/x");
    assert_eq!(req.headers.get("content-type").unwrap(), "application/json");
    let statics: Vec<_> = req.headers.get_all("x-static").iter().collect();
    assert_eq!(statics, ["one", "two"]);
    assert_eq!(req.headers.get("authorization").unwrap(), "Bearer tok");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&req.body).unwrap(),
        serde_json::json!({"q": "<b>&"})
    );
    assert_eq!(hook_calls.load(Ordering::SeqCst), 1);

    // A non-JSON 2xx body is a Decode error and a list-fallback trigger (google.go:235-242).
    let err = c
        .get_json::<serde_json::Value>(&cancel, "/html")
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Decode(_)), "{err:?}");
    assert!(err.is_list_fallback());
    let reqs = srv.received_requests().await.unwrap();
    assert!(
        reqs[1].headers.get("content-type").is_none(),
        "GET sends no Content-Type"
    );

    // A failing auth hook is `Authorize` and never reaches the server.
    let failing =
        Client::new(&srv.uri(), reqwest::Client::new()).with_auth(Arc::new(|_| Err("nope".into())));
    let err = failing
        .get_json::<serde_json::Value>(&cancel, "/v1/x")
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Authorize(_)), "{err:?}");
    assert_eq!(err.to_string(), "llm: authorize request: nope");
    assert_eq!(srv.received_requests().await.unwrap().len(), 2);

    // A cancelled token short-circuits the retry sleep.
    let srv5 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/busy"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "30"))
        .mount(&srv5)
        .await;
    let c5 = client(&srv5);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });
    let err = tokio::time::timeout(
        Duration::from_secs(2),
        c5.get_json::<serde_json::Value>(&cancel, "/busy"),
    )
    .await
    .expect("retry sleep ignored the cancel")
    .unwrap_err();
    assert!(matches!(err, LlmError::Cancelled), "{err:?}");
    assert_eq!(srv5.received_requests().await.unwrap().len(), 1);
}
