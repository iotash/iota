//! The wiremock openai transcript fixture (`openai_transcript(&MockServer)`: one chat-completions stream ending in
//! `[DONE]`, the `-m` end-to-end tests' backend). WP15 owns and fills this file; the scaffold ships it empty so
//! `mod.rs` can pre-register it.
//!
//! The mounted responder answers BOTH shapes the headless run can ask for on `/chat/completions`, chosen by the
//! request's own `"stream"` field: the tool loop streams (`stream: true` → SSE, ending in `[DONE]`), and a run
//! with no tools advertised sends the unary request instead (chat.go:117, POLICY §5). Both carry the same reply
//! and the same usage, so a test asserts one set of numbers whatever the toolset is.

use std::time::Duration;

use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

/// The reply both transcript shapes return.
pub const REPLY: &str = "Hello from iota.";
/// `prompt_tokens` of the transcript's usage block.
pub const INPUT_TOKENS: u64 = 11;
/// `completion_tokens` of the transcript's usage block.
pub const OUTPUT_TOKENS: u64 = 7;
/// `total_tokens` of the transcript's usage block.
pub const TOTAL_TOKENS: u64 = 18;

/// The streaming half: two content deltas, a `finish_reason` chunk carrying usage, then `[DONE]`.
const SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from \"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"iota.\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
    "data: [DONE]\n\n",
);

/// The unary half (the same reply and usage as [`SSE`]).
const UNARY: &str = concat!(
    "{\"choices\":[{\"message\":{\"content\":\"Hello from iota.\"}}],",
    "\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}",
);

/// Answers a chat-completions request with the SSE transcript or the unary body, after an optional delay.
struct Transcript {
    delay: Option<Duration>,
}

impl Respond for Transcript {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let streaming = serde_json::from_slice::<serde_json::Value>(&req.body)
            .ok()
            .and_then(|v| v.get("stream").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let template = if streaming {
            ResponseTemplate::new(200).set_body_raw(SSE, "text/event-stream")
        } else {
            ResponseTemplate::new(200).set_body_raw(UNARY, "application/json")
        };
        match self.delay {
            Some(d) => template.set_delay(d),
            None => template,
        }
    }
}

/// Mounts the chat-completions transcript on `server`: `POST /chat/completions` answers [`REPLY`] with the
/// [`INPUT_TOKENS`]/[`OUTPUT_TOKENS`]/[`TOTAL_TOKENS`] usage block, streaming or unary as the request asks.
pub async fn openai_transcript(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Transcript { delay: None })
        .mount(server)
        .await;
}

/// [`openai_transcript`] with the response head held back for `delay` — long enough for a signal to arrive while
/// the request is in flight (`cli_sigint_exits_130_with_interrupted_json`).
pub async fn openai_transcript_delayed(server: &MockServer, delay: Duration) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Transcript { delay: Some(delay) })
        .mount(server)
        .await;
}

/// Mounts a terminal `400 Bad Request` on `POST /chat/completions` (client.go:32-47 shapes the message).
pub async fn openai_bad_request(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(
            "{\"error\":{\"message\":\"bad model\",\"type\":\"invalid_request_error\"}}",
            "application/json",
        ))
        .mount(server)
        .await;
}

/// Mounts a `500` on `GET /models`, the failure `-l <provider>` reports (`x-should-retry: false` keeps it to one
/// attempt so the test never waits for a backoff).
pub async fn openai_models_fail(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("x-should-retry", "false")
                .set_body_raw(
                    "{\"error\":{\"message\":\"models are down\"}}",
                    "application/json",
                ),
        )
        .mount(server)
        .await;
}
