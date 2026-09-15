//! Shared iota-llm test fixtures (`TEST_PLAN.md` principle 10). Created by the scaffold with real bodies and
//! WP00-owned afterwards: later packages only `mod common;` this file (or add a NEW submodule they own) and may
//! ADD helpers, never change the frozen ones (`sse_stream`, `mock_json`, `mock_sse`, `body_json`).

use bytes::Bytes;
use futures::Stream;
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers};

/// An in-memory SSE body delivered as one chunk (Go `strings.NewReader`).
pub fn sse_stream(
    transcript: &'static str,
) -> impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static {
    futures::stream::iter([Ok(Bytes::from_static(transcript.as_bytes()))])
}

/// Mounts a 200 JSON response for `method` + `path`.
pub async fn mock_json(server: &MockServer, method: &str, path: &str, body: serde_json::Value) {
    Mock::given(matchers::method(method))
        .and(matchers::path(path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Mounts a 200 `text/event-stream` response carrying `transcript` verbatim for `method` + `path`.
pub async fn mock_sse(server: &MockServer, method: &str, path: &str, transcript: &'static str) {
    Mock::given(matchers::method(method))
        .and(matchers::path(path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(transcript, "text/event-stream"))
        .mount(server)
        .await;
}

/// The recorded request body as JSON (golden-request comparisons).
pub fn body_json(req: &Request) -> serde_json::Value {
    req.body_json()
        .expect("recorded request body is not valid JSON")
}
