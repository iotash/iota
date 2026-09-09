//! Per-dialect `StreamSink::tool_delta` emission (WP55; `provider/observer_test.go`
//! `TestToolCallObserver`, `provider/{openai.go:255,anthropic.go:392,openresponses.go:
//! 332,349,354}`).
//!
//! The composing observer is what raises the interactive tool-call widget mid-stream and
//! cuts the content render pipe (`TUI_DIVERGENCES` T-11). Three laws are pinned here, one
//! per dialect plus one shared:
//!
//! * deltas arrive in wire order, each carrying the name accumulated SO FAR;
//! * a fragment whose name is not known yet is ANONYMOUS (`None`) — the zombie-spinner
//!   rule: the consumer cuts the pipe but raises no widget it could never settle;
//! * anthropic's `server_tool_use` blocks never report at all — no `CallTool` settles a
//!   server-side search, so a widget raised for one would dangle.
//!
//! Google is deliberately absent: it is an atomic backend (`google.go:353` notifies with
//! an EMPTY delta, which the observer discards), so the Rust dialect emits nothing and the
//! widget rises at the tool walk instead.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common::mock_sse;
use iota::provider::ToolProvider;
use iota::provider::model::{Message, ToolDef};
use iota::provider::sink::StreamSink;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;
use wiremock::MockServer;

/// Records the composing observer's traffic as `"<name>:<delta>"`, with an anonymous
/// delta written as `":<delta>"` — the Go observer test's own join shape
/// (`observer_test.go:22`).
#[derive(Default)]
struct DeltaSink {
    deltas: Vec<String>,
}

impl StreamSink for DeltaSink {
    fn content(&mut self, _delta: &str) {}

    fn reasoning(&mut self, _delta: &str) {}

    fn reasoning_done(&mut self) {}

    fn tool_delta(&mut self, name: Option<&str>, delta: &str) {
        self.deltas
            .push(format!("{}:{delta}", name.unwrap_or_default()));
    }
}

/// Streams `transcript` through `p` and returns the recorded composing deltas.
async fn deltas<P: ToolProvider + ?Sized>(p: &P, tools: &[ToolDef]) -> Vec<String> {
    let mut sink = DeltaSink::default();
    p.stream_chat_with_tools(
        &CancellationToken::new(),
        &[Message::user("q")],
        tools,
        &mut sink,
    )
    .await
    .expect("round failed");
    sink.deltas
}

/// The one tool the transcripts advertise.
fn lookup() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "lookup".to_owned(),
        ..ToolDef::default()
    }]
}

// ---------------------------------------------------------------------------
// openai (chat completions)
// ---------------------------------------------------------------------------

// Go: provider/openai.go:255 — every non-empty `function.arguments` fragment reports with
// the name accumulated so far, in wire order, across interleaved indices.
#[tokio::test]
async fn openai_reports_named_argument_fragments_in_order() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\"}},{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"other\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"x\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;
    let p = iota::provider::openai::OpenAiProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(
        deltas(&p, &lookup()).await,
        vec![
            "lookup:{\"q\":".to_owned(),
            "other:{}".to_owned(),
            "lookup:\"x\"}".to_owned(),
        ]
    );
}

// New (the zombie-spinner rule, per dialect): an argument fragment that lands before its
// name is anonymous, and the name that follows starts reporting from that fragment on.
#[tokio::test]
async fn openai_fragment_before_the_name_is_anonymous() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"late\",\"arguments\":\"1}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;
    let p = iota::provider::openai::OpenAiProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(
        deltas(&p, &lookup()).await,
        vec![":{\"a\":".to_owned(), "late:1}".to_owned()],
        "an unnamed fragment must not carry a name it does not have yet"
    );
}

// ---------------------------------------------------------------------------
// anthropic (messages)
// ---------------------------------------------------------------------------

// Go: provider/anthropic.go:392 — `input_json_delta` fragments report under the block's
// `content_block_start` name, interleaved across parallel tool_use blocks.
#[tokio::test]
async fn anthropic_reports_input_json_deltas_per_block() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"lookup"}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t2","name":"other"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"x\"}"}}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = iota::provider::anthropic::AnthropicProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(
        deltas(&p, &lookup()).await,
        vec![
            "lookup:{\"q\":".to_owned(),
            "other:{}".to_owned(),
            "lookup:\"x\"}".to_owned(),
        ]
    );
}

// Go: provider/anthropic.go:390-393 — a `server_tool_use` block's args are NOT a client
// tool call: no `CallTool` ever settles one, so the widget must never be raised for it.
#[tokio::test]
async fn anthropic_never_reports_server_tool_use() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv_1","name":"tool_search_tool_regex","input":{}}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"weather\"}"}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = iota::provider::anthropic::AnthropicProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert!(
        deltas(&p, &lookup()).await.is_empty(),
        "a server-side search must not reach the composing observer"
    );
}

// ---------------------------------------------------------------------------
// openresponses
// ---------------------------------------------------------------------------

// Go: provider/openresponses.go:341-354 — `output_item.added` announces the name (and
// raises the widget on the announcement itself, with the "…" stand-in delta), then every
// `function_call_arguments.delta` reports under that name.
#[tokio::test]
async fn openresponses_names_deltas_from_the_added_item() {
    const TRANSCRIPT: &str = concat!(
        "event: response.output_item.added\n",
        r#"data: {"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup"}}"#,
        "\n\n",
        "event: response.function_call_arguments.delta\n",
        r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"q\":"}"#,
        "\n\n",
        "event: response.function_call_arguments.delta\n",
        r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"x\"}"}"#,
        "\n\n",
        "event: response.output_item.done\n",
        r#"data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}"}}"#,
        "\n\n",
        "event: response.completed\n",
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/responses", TRANSCRIPT).await;
    let p = iota::provider::openresponses::OpenResponsesProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(
        deltas(&p, &lookup()).await,
        vec![
            "lookup:…".to_owned(),
            "lookup:{\"q\":".to_owned(),
            "lookup:\"x\"}".to_owned(),
        ],
        "the announcement raises first, then the argument fragments"
    );
}

// New (the zombie-spinner rule, per dialect): argument deltas for an item nothing named
// stay anonymous — they still end the content stream, they raise nothing. The stock
// transcript in `openresponses.rs` has exactly this shape (no `output_item.added`).
#[tokio::test]
async fn openresponses_unannounced_items_stay_anonymous() {
    const TRANSCRIPT: &str = concat!(
        "event: response.function_call_arguments.delta\n",
        r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{}"}"#,
        "\n\n",
        "event: response.output_item.done\n",
        r#"data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":"{}"}}"#,
        "\n\n",
        "event: response.completed\n",
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/responses", TRANSCRIPT).await;
    let p = iota::provider::openresponses::OpenResponsesProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(deltas(&p, &lookup()).await, vec![":{}".to_owned()]);
}

// Go: provider/openresponses.go:330-332 — the server-side image built-in has no argument
// stream of its own, so generation start reports through the same channel under the
// `image_generation` name.
#[tokio::test]
async fn openresponses_image_generation_raises_through_the_observer() {
    const TRANSCRIPT: &str = concat!(
        "event: response.image_generation_call.in_progress\n",
        r#"data: {"type":"response.image_generation_call.in_progress","item_id":"ig_1"}"#,
        "\n\n",
        "event: response.completed\n",
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/responses", TRANSCRIPT).await;
    let p = iota::provider::openresponses::OpenResponsesProvider::new(
        "k",
        &server.uri(),
        "m",
        None,
        reqwest::Client::new(),
    );

    assert_eq!(
        deltas(&p, &[]).await,
        vec!["image_generation:…".to_owned()],
        "the delta must be non-empty: an empty one is the atomic-backend signal"
    );
}
