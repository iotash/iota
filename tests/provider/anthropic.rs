//! Anthropic messages dialect tests (`provider/anthropic_wire_test.go`, the Anthropic cases of
//! `provider/defermode_wire_test.go` and the anthropic case of `provider/usage_wire_test.go`): the golden
//! request, index-keyed stream assembly, the in-band error event, paginated model listing, the reference
//! defer protocol (`defer_loading` + server-block capture/replay) and unary usage ownership.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::common::{body_json, mock_sse};
use iota::provider::anthropic::AnthropicProvider;
use iota::provider::model::{
    AssistantBody, Attachment, Body, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef,
};
use iota::provider::sink::NullSink;
use iota::provider::usage::Usage;
use iota::provider::{
    Effort, MaxTokensTunable, Provider, RoundResult, ToolProvider, TopPTunable, Truncation, Tunable,
};
use iota::testing::RecordingSink;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// A provider pointed at `server` (no jitter needed: these tests never retry).
fn provider(server: &MockServer, model: &str, temperature: Option<f64>) -> AnthropicProvider {
    AnthropicProvider::new(
        "sk-test",
        &server.uri(),
        model,
        temperature,
        reqwest::Client::new(),
    )
}

/// `json!({..})` as the `JsonObject` the model layer uses for schemas and tool arguments.
fn obj(v: Value) -> JsonObject {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

/// One round through the tool path with a discarding sink.
async fn round(
    p: &AnthropicProvider,
    messages: &[Message],
    tools: &[ToolDef],
) -> Result<RoundResult, iota::provider::error::ProviderError> {
    let cancel = CancellationToken::new();
    let mut sink = NullSink;
    p.stream_chat_with_tools(&cancel, messages, tools, &mut sink)
        .await
}

/// The block `type`s of `messages[idx]` in the recorded request body.
fn block_types(body: &Value, idx: usize) -> Vec<String> {
    body["messages"][idx]["content"]
        .as_array()
        .expect("content array")
        .iter()
        .map(|b| b["type"].as_str().expect("block type").to_owned())
        .collect()
}
#[tokio::test]
async fn the_anthropic_request_body_is_byte_exact() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", STOP).await;

    let mut p = provider(&server, "claude-sonnet-4-6", Some(0.7));
    p.set_effort(Some(Effort::High));
    p.set_top_p(Some(0.9));

    let call = ToolCall {
        id: "t1".to_owned(),
        name: "f".to_owned(),
        arguments: obj(json!({"q": "x"})),
    };
    let messages = vec![
        Message::system("sys"),
        // A system-tools mount is skipped entirely: it must NOT become an empty `system` block (CONTRACTS §3.8).
        Message::system_tools(vec![ToolDef {
            name: "late".to_owned(),
            ..ToolDef::default()
        }]),
        Message {
            content: "look".to_owned(),
            attachments: vec![
                Attachment {
                    filename: "a.png".to_owned(),
                    mime_type: "image/png".to_owned(),
                    data: vec![1],
                },
                Attachment {
                    filename: "b.pdf".to_owned(),
                    mime_type: "application/pdf".to_owned(),
                    data: vec![2],
                },
                Attachment {
                    filename: "c.txt".to_owned(),
                    mime_type: "text/plain".to_owned(),
                    data: b"hello".to_vec(),
                },
            ],
            ..Message::default()
        },
        Message::assistant_with_calls("using", vec![call.clone()], None),
        Message::tool_result(&call, "result", false),
        // Interrupt state 3: the next user message merges into the pending tool results — one user message,
        // tool_result blocks first.
        Message::user("next"),
    ];
    let tools = vec![ToolDef {
        name: "f".to_owned(),
        description: "does f".to_owned(),
        input_schema: Some(obj(json!({
            "type": "object",
            "properties": {"q": {"type": "string"}},
            "required": ["q"],
        }))),
        deferred: false,
    }];
    round(&p, &messages, &tools).await.expect("round");

    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url.path(), "/v1/messages");
    assert_eq!(request.headers["x-api-key"], "sk-test");
    assert_eq!(request.headers["anthropic-version"], "2023-06-01");

    // The whole body: max_tokens is the 32000 default (X-68; Go's hard 4096 cut thinking models off); the user blocks run image / document / inlined text file with
    // the message text LAST; the tool_result and the following user text are ONE user message (is_error always
    // present); the tools schema forwards only type/properties/required; stream is on.
    assert_eq!(
        body_json(request),
        json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 32000,
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AQ=="}},
                    {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "Ag=="}},
                    {"type": "text", "text": "[File: c.txt]\nhello"},
                    {"type": "text", "text": "look"},
                ]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "using"},
                    {"type": "tool_use", "id": "t1", "input": {"q": "x"}, "name": "f"},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1",
                     "content": [{"type": "text", "text": "result"}], "is_error": false},
                    {"type": "text", "text": "next"},
                ]},
            ],
            "system": [{"type": "text", "text": "sys"}],
            "temperature": 0.7,
            "top_p": 0.9,
            "output_config": {"effort": "high"},
            "tools": [{
                "name": "f",
                "description": "does f",
                "input_schema": {
                    "type": "object",
                    "properties": {"q": {"type": "string"}},
                    "required": ["q"],
                },
            }],
            "stream": true,
        })
    );
}
/// A host notice followed by what the user typed next (a bot's memory-write notice lands at turn end, so the
/// next request carries assistant → notice → user) is ONE user message, both texts in order: the API rejects
/// two consecutive user-role messages. A tool result, a notice and a user message are one run too.
#[tokio::test]
async fn consecutive_user_role_messages_are_one_user_message() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", STOP).await;
    let p = provider(&server, "claude-sonnet-4-6", None);

    let call = ToolCall {
        id: "t1".to_owned(),
        name: "f".to_owned(),
        arguments: obj(json!({})),
    };
    let messages = vec![
        Message::user("hi"),
        Message::assistant("hello"),
        Message::notice("memory: MEMORY.md ## User +1 line: [user] x (2026-09-30)"),
        Message::user("again"),
        Message::assistant_with_calls("", vec![call.clone()], None),
        Message::tool_result(&call, "result", false),
        Message::notice("[background job b1 finished]"),
        Message::user("next"),
    ];
    round(&p, &messages, &[]).await.expect("round");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = body_json(&requests[0]);
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["role"].as_str().expect("role"))
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
    assert_eq!(
        body["messages"][2]["content"],
        json!([
            {"type": "text", "text": "memory: MEMORY.md ## User +1 line: [user] x (2026-09-30)"},
            {"type": "text", "text": "again"},
        ])
    );
    assert_eq!(block_types(&body, 4), ["tool_result", "text", "text"]);
}

#[tokio::test]
async fn an_anthropic_stream_assembles_text_thinking_tool_use_and_usage() {
    // Index 3's delta arrives between index 2's two fragments and the stops arrive out of index order: only
    // per-index accumulation assembles this correctly.
    const TRANSCRIPT: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":11}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"think"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Let me check."}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":1}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"lookup"}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"t2","name":"other"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"x\"}"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":3}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":2}"#,
        "\n\n",
        "event: ping\n",
        r#"data: {"type":"ping"}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;

    let p = provider(&server, "m", None);
    let cancel = CancellationToken::new();
    let mut sink = RecordingSink::default();
    let out = p
        .stream_chat_with_tools(
            &cancel,
            &[Message::user("q")],
            &[ToolDef {
                name: "lookup".to_owned(),
                ..ToolDef::default()
            }],
            &mut sink,
        )
        .await
        .expect("round");

    assert_eq!(out.reasoning, "think");
    assert_eq!(sink.reasoning(), "think");
    assert!(
        sink.closed_before_content(),
        "reasoning must close before the first content write: {:?}",
        sink.events
    );
    assert_eq!(out.content, "Let me check.");
    assert_eq!(sink.content(), out.content);
    assert_eq!(
        out.tool_calls,
        vec![
            ToolCall {
                id: "t1".to_owned(),
                name: "lookup".to_owned(),
                arguments: obj(json!({"q": "x"})),
            },
            ToolCall {
                id: "t2".to_owned(),
                name: "other".to_owned(),
                arguments: JsonObject::new(),
            },
        ],
        "interleaved tool calls must assemble in index order"
    );
    // Input (plus cache counts) lands at message_start, the cumulative output at message_delta.
    assert_eq!(
        out.usage,
        Some(Usage {
            input: 11,
            output: 7,
            ..Usage::default()
        })
    );
    assert_eq!(
        out.truncated, None,
        "a round that ended at tool_use was not cut off"
    );
}

/// GLM's Anthropic-compatible endpoint (`…/api/anthropic/v1/messages`) sends placeholder zeros at
/// `message_start` and the real counts only at `message_delta`, beside fields iota does not read
/// (`server_tool_use`, `service_tier`). `message_delta.usage` is the message's cumulative usage by
/// Anthropic's definition, so what it carries IS the figure — DIVERGENCES X-30.
#[tokio::test]
async fn a_message_delta_usage_is_laid_over_a_placeholder_start() {
    const TRANSCRIPT: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":18,"output_tokens":16,"cache_read_input_tokens":0,"server_tool_use":{"web_search_requests":0},"service_tier":"standard"}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;

    let p = provider(&server, "glm-5.3", None);
    let out = round(&p, &[Message::user("q")], &[]).await.expect("round");
    assert_eq!(out.content, "hi");
    assert_eq!(
        out.usage,
        Some(Usage {
            input: 18,
            output: 16,
            ..Usage::default()
        }),
        "the delta's input side must replace the start's placeholder zero"
    );
}

/// The control: the real API's older shape, where `message_delta.usage` carries `output_tokens` alone.
/// The overlay replaces only what the delta carries, so the start's input side and cache counts survive —
/// the rule never wipes a real figure to 0.
#[tokio::test]
async fn a_message_delta_without_input_keeps_the_starts_figures() {
    const TRANSCRIPT: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":11,"cache_read_input_tokens":5,"cache_creation_input_tokens":4}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;

    let p = provider(&server, "m", None);
    let out = round(&p, &[Message::user("q")], &[]).await.expect("round");
    assert_eq!(
        out.usage,
        Some(Usage {
            input: 11,
            output: 7,
            cache_read: 5,
            cache_write: 4,
            total: 0,
        }),
        "a delta without the input side must keep the start's"
    );
}
#[tokio::test]
async fn an_error_event_ends_the_stream_with_its_message() {
    // overloaded_error arrives on a 200 stream, not as HTTP 529.
    const TRANSCRIPT: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
        "\n\n",
        "event: error\n",
        r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;

    let p = provider(&server, "m", None);
    let err = round(&p, &[Message::user("q")], &[])
        .await
        .expect_err("expected stream error");
    let text = err.to_string();
    assert!(text.contains("received error while streaming"), "{text}");
    assert!(text.contains("overloaded_error"), "{text}");
    assert!(text.contains("Overloaded"), "{text}");
    assert_eq!(
        text,
        r#"stream error: received error while streaming: {"type":"overloaded_error","message":"Overloaded"}"#
    );
}
#[tokio::test]
async fn the_model_listing_follows_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param_is_missing("after_id"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"claude-b"},{"id":"claude-a"}],"has_more":true,"first_id":"claude-b","last_id":"claude-a"}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("after_id", "claude-a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"claude-c"}],"has_more":false,"first_id":"claude-c","last_id":"claude-c"}"#,
        ))
        .mount(&server)
        .await;

    let p = provider(&server, "m", None);
    let cancel = CancellationToken::new();
    let models = p.list_models(&cancel).await.expect("list models");

    let requests = server.received_requests().await.expect("recorded requests");
    let uris: Vec<String> = requests
        .iter()
        .map(|r| match r.url.query() {
            Some(q) => format!("{}?{q}", r.url.path()),
            None => r.url.path().to_owned(),
        })
        .collect();
    assert_eq!(uris, ["/v1/models", "/v1/models?after_id=claude-a"]);
    for request in &requests {
        assert_eq!(request.headers["x-api-key"], "sk-test");
        assert_eq!(request.headers["anthropic-version"], "2023-06-01");
    }
    // Merged across pages, then sorted byte-wise ascending.
    assert_eq!(models, ["claude-a", "claude-b", "claude-c"]);
}
#[tokio::test]
async fn deferred_tools_ride_the_wire_as_the_tool_reference_form() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", STOP).await;

    let p = provider(&server, "claude-sonnet-5", None);
    let tools = vec![
        ToolDef {
            name: "plain".to_owned(),
            description: "d".to_owned(),
            input_schema: Some(obj(json!({"properties": {}}))),
            deferred: false,
        },
        ToolDef {
            name: "mcp__gh__pr".to_owned(),
            description: "d".to_owned(),
            input_schema: Some(obj(json!({"properties": {}}))),
            deferred: true,
        },
    ];
    round(&p, &[Message::user("go")], &tools)
        .await
        .expect("round");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = body_json(&requests[0]);
    // plain + deferred + the server search tool, in that order.
    assert_eq!(
        body["tools"],
        json!([
            {"name": "plain", "description": "d", "input_schema": {"type": "object", "properties": {}}},
            {"name": "mcp__gh__pr", "description": "d", "input_schema": {"type": "object", "properties": {}},
             "defer_loading": true},
            {"type": "tool_search_tool_regex_20251119", "name": "tool_search_tool_regex"},
        ]),
        "plain must carry no defer_loading and the search tool no input_schema"
    );
}
#[tokio::test]
async fn server_tool_blocks_are_captured_into_the_raw_payload() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv_1","name":"tool_search_tool_regex","input":{}}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"weather\"}"}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_search_tool_result","tool_use_id":"srv_1","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"atmos_query"}]}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"found it"}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;

    let p = provider(&server, "claude-sonnet-5", None);
    // Capture is gated on the reference protocol being active: the request must carry a deferred tool.
    let out = round(
        &p,
        &[Message::user("go")],
        &[ToolDef {
            name: "atmos_query".to_owned(),
            description: "d".to_owned(),
            input_schema: Some(obj(json!({"type": "object"}))),
            deferred: true,
        }],
    )
    .await
    .expect("round");

    assert_eq!(out.content, "found it");
    // Go also asserted that server search deltas never reach the tool-call observer; the observer is
    // interactive-only and not ported (DIVERGENCES D-20), so the observable rule here is that a
    // `server_tool_use` block never becomes a client tool call.
    assert!(out.tool_calls.is_empty(), "{:?}", out.tool_calls);

    let Some(RawContent::Anthropic(blocks)) = out.raw_content else {
        panic!("raw content = {:?}, want 2 server blocks", out.raw_content);
    };
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    // Block 0 is recomposed from the start event plus the streamed input_json_delta.
    assert_eq!(
        blocks[0].get(),
        r#"{"type":"server_tool_use","id":"srv_1","name":"tool_search_tool_regex","input":{"pattern":"weather"}}"#
    );
    // Block 1 arrives complete in content_block_start and is kept verbatim.
    assert_eq!(
        blocks[1].get(),
        r#"{"type":"tool_search_tool_result","tool_use_id":"srv_1","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"atmos_query"}]}}"#
    );

    // Persistence round-trip: the blob is a JSON array of the raw blocks, byte-identical on the way back.
    let blob = serde_json::to_string(&blocks).expect("marshal");
    let back: Vec<Raw> = serde_json::from_str(&blob).expect("unmarshal");
    assert_eq!(back, blocks);
}
#[tokio::test]
async fn captured_server_tool_blocks_replay_verbatim() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    const SERVER_TOOL_USE: &str = r#"{"type":"server_tool_use","id":"srv_1","name":"tool_search_tool_regex","input":{"pattern":"weather"}}"#;
    const SEARCH_RESULT: &str = r#"{"type":"tool_search_tool_result","tool_use_id":"srv_1","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"atmos_query"}]}}"#;

    let blocks = vec![
        Raw::from_string(SERVER_TOOL_USE.to_owned()).expect("valid block"),
        Raw::from_string(SEARCH_RESULT.to_owned()).expect("valid block"),
    ];
    let messages = vec![
        Message::user("search"),
        Message {
            content: "found atmos_query".to_owned(),
            body: Body::Assistant(AssistantBody {
                raw_content: Some(RawContent::Anthropic(blocks)),
                ..AssistantBody::default()
            }),
            ..Message::default()
        },
        Message::user("use it"),
    ];
    let tool = |deferred| ToolDef {
        name: "atmos_query".to_owned(),
        description: "d".to_owned(),
        input_schema: Some(obj(json!({"type": "object"}))),
        deferred,
    };

    for (name, tools, want) in [
        (
            "deferred-tool-replays",
            vec![tool(true)],
            vec!["server_tool_use", "tool_search_tool_result", "text"],
        ),
        // Without a deferred tool in THIS request the history degrades to its text — no endpoint guessing.
        ("no-deferred-strips", vec![tool(false)], vec!["text"]),
    ] {
        let server = MockServer::start().await;
        mock_sse(&server, "POST", "/v1/messages", STOP).await;
        let p = provider(&server, "claude-sonnet-5", None);
        round(&p, &messages, &tools).await.expect("round");
        let requests = server.received_requests().await.expect("recorded requests");
        assert_eq!(block_types(&body_json(&requests[0]), 1), want, "{name}");
    }
}

/// Anthropic's 529 "overloaded" is not special-cased anywhere: the shared client's `>= 500` rule retries it
/// (llm-anthropic.md Behaviors, retry policy). `Retry-After-Ms: 1` keeps the test instant.
#[tokio::test]
async fn overloaded_529_is_retried_by_the_status_rule() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_: &Request| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(529)
                    .insert_header("Retry-After-Ms", "1")
                    .set_body_string(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
            } else {
                ResponseTemplate::new(200).set_body_raw(STOP, "text/event-stream")
            }
        })
        .mount(&server)
        .await;

    let p = provider(&server, "m", None);
    let out = round(&p, &[Message::user("q")], &[]).await.expect("round");
    assert_eq!(out.content, "");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "529 must be retried");
}
/// The unary call rides the stream (X-71): the client's 120 s bound is on the response HEADERS, and a
/// server that delays a non-streaming response's headers until a long generation is done would trip it;
/// a stream sends its head first. It still reports only its own usage, sends no tools, and carries the
/// default ceiling.
#[tokio::test]
async fn an_anthropic_unary_call_streams_and_reports_only_its_own_usage() {
    const WITH_USAGE: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":11}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        "\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    const WITHOUT_USAGE: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        "\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    let server = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_: &Request| {
            let body = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                WITH_USAGE
            } else {
                WITHOUT_USAGE
            };
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        })
        .mount(&server)
        .await;

    let p = provider(&server, "m", None);
    let cancel = CancellationToken::new();
    let first = p
        .chat(&cancel, &[Message::user("q")])
        .await
        .expect("unary chat");
    assert_eq!(first.text, "hi");
    assert_eq!(
        first.usage,
        Some(Usage {
            input: 11,
            output: 7,
            ..Usage::default()
        })
    );
    assert_eq!(first.truncated, None);

    // A second call whose response omits usage must not keep reporting the first call's figures.
    let second = p
        .chat(&cancel, &[Message::user("q")])
        .await
        .expect("unary chat without usage");
    assert_eq!(second.text, "hi");
    assert_eq!(second.usage, None);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // A unary request streams and never carries tools.
    let requests = server.received_requests().await.expect("recorded requests");
    let body = body_json(&requests[0]);
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body.get("tools"), None);
    assert_eq!(body["max_tokens"], json!(32000));
}

/// M4 (X-71): a body that ends before the message did is an error on BOTH entry points — the tool
/// round and the unary call — never a finished answer. Complete is `message_stop`, or (for an endpoint
/// that closes without it) a body ending after a `message_delta` that carried a `stop_reason`.
#[tokio::test]
async fn a_stream_that_ends_before_the_message_did_is_incomplete() {
    const TEXT: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"unfinished"}}"#,
        "\n\n",
    );
    const PING: &str = "event: ping\ndata: {\"type\":\"ping\"}\n\n";
    const DELTA_NO_REASON: &str = concat!(
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{},"usage":{"output_tokens":3}}"#,
        "\n\n",
    );
    let cut = [
        ("ping only", PING.to_owned()),
        ("text, then EOF", TEXT.to_owned()),
        (
            "a message_delta without a stop_reason",
            format!("{TEXT}{DELTA_NO_REASON}"),
        ),
    ];
    for (name, body) in cut {
        let server = MockServer::start().await;
        mock_sse(&server, "POST", "/v1/messages", body.leak()).await;
        let p = provider(&server, "m", None);
        let e = round(&p, &[Message::user("q")], &[]).await.expect_err(name);
        assert!(
            e.to_string()
                .contains("stream ended before the message did"),
            "{name}: {e}"
        );
        let e = p
            .chat(&CancellationToken::new(), &[Message::user("q")])
            .await
            .expect_err(name);
        assert!(
            e.to_string()
                .contains("stream ended before the message did"),
            "{name} (unary): {e}"
        );
    }

    // The compatible ending: a stop_reason, then EOF with no message_stop — complete, cut or not.
    for (reason, cut) in [("end_turn", false), ("max_tokens", true)] {
        let body = format!(
            "{TEXT}event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{reason}\"}},\"usage\":{{\"output_tokens\":3}}}}\n\n"
        );
        let server = MockServer::start().await;
        mock_sse(&server, "POST", "/v1/messages", body.leak()).await;
        let p = provider(&server, "m", None);
        let out = p
            .chat(&CancellationToken::new(), &[Message::user("q")])
            .await
            .expect(reason);
        assert_eq!(out.text, "unfinished");
        assert_eq!(out.truncated.is_some(), cut, "{reason}");
    }
}

/// One `message_delta` with `delta` and `usage`, and (optionally) the `message_stop` after it.
fn tail(delta: &str, usage: &str) -> String {
    format!(
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{delta},\"usage\":{usage}}}\n\n"
    )
}

/// The body of a round: `blocks` (complete SSE), then `tails`, then `message_stop` when `stop`.
fn round_body(blocks: &str, tails: &[String], stop: bool) -> &'static str {
    let mut s = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":0,\"output_tokens\":0}}}}}}\n\n{blocks}"
    );
    for t in tails {
        s.push_str(t);
    }
    if stop {
        s.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    }
    s.leak()
}

const THINKING_BLOCK: &str = concat!(
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"half a thought"}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":0}"#,
    "\n\n",
);
const TEXT_BLOCK: &str = concat!(
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"half an answer"}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":1}"#,
    "\n\n",
);
const TOOL_BLOCK: &str = concat!(
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"write_file"}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":2}"#,
    "\n\n",
);

/// R1 (X-69): `message_delta`'s `stop_reason` is optional, and a later usage-only delta is an UPDATE of the
/// usage, not a retraction of the reason. A `max_tokens` once reported stays the round's fact — with and
/// without `message_stop`, after a thought only, after text, after a fully assembled tool call (still dropped
/// and named) — while the later usage still lands. Both entry points.
#[tokio::test]
async fn a_later_usage_only_delta_never_erases_a_reported_truncation() {
    let cut = tail(
        r#"{"stop_reason":"max_tokens"}"#,
        r#"{"output_tokens":4096}"#,
    );
    let usage_only = tail("{}", r#"{"input_tokens":23,"output_tokens":4096}"#);
    let shapes: [(&str, String, &str, Vec<String>); 3] = [
        ("thinking only", THINKING_BLOCK.to_owned(), "", Vec::new()),
        (
            "thinking and text",
            format!("{THINKING_BLOCK}{TEXT_BLOCK}"),
            "half an answer",
            Vec::new(),
        ),
        (
            "an assembled tool call",
            format!("{THINKING_BLOCK}{TOOL_BLOCK}"),
            "",
            vec!["write_file".to_owned()],
        ),
    ];
    for (name, blocks, content, dropped) in shapes {
        for stop in [true, false] {
            let body = round_body(&blocks, &[cut.clone(), usage_only.clone()], stop);
            let server = MockServer::start().await;
            mock_sse(&server, "POST", "/v1/messages", body).await;
            let mut p = provider(&server, "m", None);
            p.set_max_tokens(Some(4096));
            let tools = [ToolDef {
                name: "write_file".to_owned(),
                ..ToolDef::default()
            }];
            let out = round(&p, &[Message::user("q")], &tools)
                .await
                .unwrap_or_else(|e| panic!("{name} stop={stop}: {e}"));
            let cut = out
                .truncated
                .unwrap_or_else(|| panic!("{name} stop={stop}: the truncation was erased"));
            assert_eq!(cut.limit, Some(4096));
            assert_eq!(cut.dropped_tools, dropped, "{name}");
            assert!(out.tool_calls.is_empty(), "{name}: a cut call never runs");
            assert_eq!(out.content, content, "{name}");
            assert_eq!(
                out.usage,
                Some(Usage {
                    input: 23,
                    output: 4096,
                    ..Usage::default()
                }),
                "{name} stop={stop}: the later usage still lands"
            );
            let unary = p
                .chat(&CancellationToken::new(), &[Message::user("q")])
                .await
                .unwrap_or_else(|e| panic!("{name} unary: {e}"));
            assert!(unary.truncated.is_some(), "{name} stop={stop} (unary)");
            assert_eq!(
                server.received_requests().await.expect("requests").len(),
                2,
                "{name}: one request per call, no retry"
            );
        }
    }
}

/// R1, the other half: a usage-only tail after a NORMAL end changes nothing — `end_turn` stays a finished
/// answer, `tool_use` still hands its call back to run (once: one request). A `max_tokens` arriving after
/// another reason still wins; two different non-max reasons contradict each other and are an error.
#[tokio::test]
async fn a_usage_only_tail_after_a_normal_end_is_still_a_normal_end() {
    let usage_only = tail("{}", r#"{"input_tokens":23,"output_tokens":9}"#);
    let reason = |r: &str| {
        tail(
            &format!(r#"{{"stop_reason":"{r}"}}"#),
            r#"{"output_tokens":9}"#,
        )
    };
    for stop in [true, false] {
        let server = MockServer::start().await;
        mock_sse(
            &server,
            "POST",
            "/v1/messages",
            round_body(TEXT_BLOCK, &[reason("end_turn"), usage_only.clone()], stop),
        )
        .await;
        let out = round(&provider(&server, "m", None), &[Message::user("q")], &[])
            .await
            .expect("end_turn");
        assert_eq!(out.truncated, None);
        assert_eq!(out.content, "half an answer");
        assert_eq!(out.usage.map(|u| u.input), Some(23));

        let server = MockServer::start().await;
        mock_sse(
            &server,
            "POST",
            "/v1/messages",
            round_body(TOOL_BLOCK, &[reason("tool_use"), usage_only.clone()], stop),
        )
        .await;
        let tools = [ToolDef {
            name: "write_file".to_owned(),
            ..ToolDef::default()
        }];
        let out = round(&provider(&server, "m", None), &[Message::user("q")], &tools)
            .await
            .expect("tool_use");
        assert_eq!(out.truncated, None);
        assert_eq!(out.tool_calls.len(), 1, "the call is handed back to run");
        assert_eq!(server.received_requests().await.expect("r").len(), 1);
    }

    // A later max_tokens over an earlier end_turn: the cut is the fact.
    let server = MockServer::start().await;
    mock_sse(
        &server,
        "POST",
        "/v1/messages",
        round_body(
            TEXT_BLOCK,
            &[reason("end_turn"), reason("max_tokens")],
            true,
        ),
    )
    .await;
    let out = round(&provider(&server, "m", None), &[Message::user("q")], &[])
        .await
        .expect("round");
    assert!(out.truncated.is_some());

    // end_turn, then tool_use: contradictory, trusted neither way.
    let server = MockServer::start().await;
    mock_sse(
        &server,
        "POST",
        "/v1/messages",
        round_body(TOOL_BLOCK, &[reason("end_turn"), reason("tool_use")], true),
    )
    .await;
    let e = round(&provider(&server, "m", None), &[Message::user("q")], &[])
        .await
        .expect_err("conflicting reasons");
    assert!(
        e.to_string()
            .contains("conflicting stop reasons (end_turn, then tool_use)"),
        "{e}"
    );
}

/// `max_tokens:` from the config (X-68) is what the request carries, on both entry points.
#[tokio::test]
async fn a_configured_max_tokens_rides_the_wire() {
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", STOP).await;
    let mut p = provider(&server, "glm-5.3", None);
    p.set_max_tokens(Some(131_072));
    assert_eq!(p.max_tokens(), Some(131_072));
    round(&p, &[Message::user("q")], &[]).await.expect("round");
    p.chat(&CancellationToken::new(), &[Message::user("q")])
        .await
        .expect("unary");
    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(requests.len(), 2);
    for r in &requests {
        assert_eq!(body_json(r)["max_tokens"], json!(131_072));
    }
}

/// What GLM-5.3 sends when the ceiling hits mid-thought (`/tmp/iota-glm-probe/r1_4096_max.sse`, shortened):
/// a sealed thinking block, no text block, `stop_reason: max_tokens`.
const CUT_MID_THOUGHT: &str = concat!(
    "event: message_start\n",
    r#"data: {"type":"message_start","message":{"usage":{"input_tokens":0,"output_tokens":0}}}"#,
    "\n\n",
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"The user is leaning toward option 1. This needs flagging as a"}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"2e2c9b8a9d6c47e3a7e00bc7"}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":0}"#,
    "\n\n",
    "event: message_delta\n",
    r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null},"usage":{"input_tokens":733,"output_tokens":32000}}"#,
    "\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// `stop_reason: max_tokens` is no longer dropped (X-69): the round says it was cut off, at which ceiling,
/// and still hands back the sealed thinking block (the replay contract does not change).
#[tokio::test]
async fn a_round_cut_off_at_max_tokens_reports_the_truncation() {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", CUT_MID_THOUGHT).await;
    let p = provider(&server, "glm-5.3", None);
    let out = round(&p, &[Message::user("q")], &[]).await.expect("round");
    assert_eq!(
        out.truncated,
        Some(Truncation {
            reason: "stop_reason: max_tokens".to_owned(),
            limit: Some(32_000),
            dropped_tools: Vec::new(),
        })
    );
    assert_eq!(out.content, "", "the endpoint opened no text block");
    assert!(
        out.reasoning.ends_with("flagging as a"),
        "{}",
        out.reasoning
    );
    assert!(out.tool_calls.is_empty());
    let Some(RawContent::Anthropic(blocks)) = out.raw_content else {
        panic!("the sealed thinking block must still be captured");
    };
    assert!(blocks[0].get().contains("2e2c9b8a9d6c47e3a7e00bc7"));

    // The unary path reports the same cut.
    let unary = p
        .chat(&CancellationToken::new(), &[Message::user("q")])
        .await
        .expect("unary");
    assert_eq!(unary.text, "");
    assert_eq!(
        unary.truncated.map(|t| t.limit),
        Some(Some(32_000)),
        "the unary call must not pass a cut-off reply off as finished"
    );
}

/// A ceiling that hits while a tool call's arguments are still streaming: the call is dropped — its JSON
/// may be cut mid-value — and the truncation NAMES it, where Go dropped it without a word (X-69).
#[tokio::test]
async fn a_tool_call_cut_off_mid_arguments_is_dropped_by_name() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Writing the file."}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"write_file"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\",\"content\":\"fn ma"}}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":32000}}"#,
        "\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = provider(&server, "m", None);
    let tools = [ToolDef {
        name: "write_file".to_owned(),
        ..ToolDef::default()
    }];
    let out = round(&p, &[Message::user("q")], &tools)
        .await
        .expect("round");
    assert!(out.tool_calls.is_empty(), "a cut-off call never runs");
    assert_eq!(out.content, "Writing the file.");
    let cut = out.truncated.expect("the cut is reported");
    assert_eq!(cut.dropped_tools, vec!["write_file".to_owned()]);
    assert_eq!(
        cut.to_string(),
        "stop_reason: max_tokens: the output hit its 32000-token limit; the tool call it was composing was dropped (write_file)"
    );
}

// A thinking
// block is captured WITH its signature and replayed on the next request unconditionally, no
// deferred tool required (unlike the server blocks above). Endpoints that implement thinking mode
// reject an assistant turn whose thinking is missing ("the content[].thinking in the thinking mode
// must be passed back to the API"), and the block must lead the content array.
#[tokio::test]
async fn a_signed_thinking_block_is_captured_and_replayed() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"weigh it\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-abc\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    const STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let plain = ToolDef {
        name: "t".to_owned(),
        description: "d".to_owned(),
        input_schema: Some(obj(json!({"type": "object"}))),
        deferred: false,
    };

    // Round 1: the stream carries thinking + signature; capture must keep both.
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = provider(&server, "claude-sonnet-5", None);
    let out = round(&p, &[Message::user("hi")], std::slice::from_ref(&plain))
        .await
        .expect("round 1");
    let Some(RawContent::Anthropic(blocks)) = out.raw_content else {
        panic!("thinking block not captured: {:?}", out.raw_content);
    };
    assert_eq!(blocks.len(), 1, "one replayable block, got {blocks:?}");
    let captured: serde_json::Value =
        serde_json::from_str(blocks[0].get()).expect("captured block is JSON");
    assert_eq!(captured["type"], "thinking");
    assert_eq!(captured["thinking"], "weigh it");
    assert_eq!(captured["signature"], "sig-abc", "signature dropped");

    // Round 2: replaying that turn sends the block back, FIRST, signature intact — with a plain
    // (non-deferred) tool set, where a server block would have been stripped.
    let messages = vec![
        Message::user("hi"),
        Message {
            content: "answer".to_owned(),
            body: Body::Assistant(AssistantBody {
                raw_content: Some(RawContent::Anthropic(blocks)),
                ..AssistantBody::default()
            }),
            ..Message::default()
        },
        Message::user("again"),
    ];
    let server2 = MockServer::start().await;
    mock_sse(&server2, "POST", "/v1/messages", STOP).await;
    let p2 = provider(&server2, "claude-sonnet-5", None);
    round(&p2, &messages, std::slice::from_ref(&plain))
        .await
        .expect("round 2");
    let requests = server2
        .received_requests()
        .await
        .expect("recorded requests");
    let body = body_json(&requests[0]);
    let first = &body["messages"][1]["content"][0];
    assert_eq!(
        first["type"], "thinking",
        "thinking must lead the assistant content: {body}"
    );
    assert_eq!(first["signature"], "sig-abc", "replay lost the signature");
    assert_eq!(first["thinking"], "weigh it", "replay lost the body");
}

// An endpoint
// that implements thinking mode without sealing the block sends no `signature_delta`; the replayed
// block then carries no `"signature"` key at all rather than an empty one, so what goes back is
// exactly what came in.
#[tokio::test]
async fn a_thinking_block_without_a_signature_is_not_replayed() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"unsealed\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = provider(&server, "claude-sonnet-5", None);
    let out = round(&p, &[Message::user("hi")], &[]).await.expect("round");
    let Some(RawContent::Anthropic(blocks)) = out.raw_content else {
        panic!("thinking block not captured");
    };
    assert_eq!(
        blocks[0].get(),
        r#"{"type":"thinking","thinking":"unsealed"}"#,
        "an unsealed thinking block must carry no signature key"
    );
}

// A block
// can arrive sealed but empty (DeepSeek's Anthropic endpoint sends `signature_delta` with no
// `thinking_delta`). The signature alone makes it part of the turn, so it replays.
#[tokio::test]
async fn a_signature_only_thinking_block_still_replays() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SEAL\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = provider(&server, "claude-sonnet-5", None);
    let out = round(&p, &[Message::user("hi")], &[]).await.expect("round");
    let Some(RawContent::Anthropic(blocks)) = out.raw_content else {
        panic!("signature-only thinking block was dropped");
    };
    assert_eq!(
        blocks[0].get(),
        r#"{"type":"thinking","thinking":"","signature":"SEAL"}"#,
    );
}

// Neither body nor
// signature means the block never carried anything; it stays out of the replay.
#[tokio::test]
async fn an_empty_thinking_block_is_dropped() {
    const TRANSCRIPT: &str = concat!(
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", TRANSCRIPT).await;
    let p = provider(&server, "claude-sonnet-5", None);
    let out = round(&p, &[Message::user("hi")], &[]).await.expect("round");
    assert!(
        out.raw_content.is_none(),
        "an empty thinking block must not replay"
    );
}
