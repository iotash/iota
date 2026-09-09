//! Anthropic messages dialect tests (`provider/anthropic_wire_test.go`, the Anthropic cases of
//! `provider/defermode_wire_test.go` and the anthropic case of `provider/usage_wire_test.go`): the golden
//! request, index-keyed stream assembly, the in-band error event, paginated model listing, the reference
//! defer protocol (`defer_loading` + server-block capture/replay) and unary usage ownership.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
use iota::provider::{Effort, Provider, RoundResult, ToolProvider, TopPTunable, Tunable};
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

// Go: provider/anthropic_wire_test.go:19
#[tokio::test]
async fn test_anthropic_golden_request() {
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

    // The whole body: max_tokens is the hard 4096; the user blocks run image / document / inlined text file with
    // the message text LAST; the tool_result and the following user text are ONE user message (is_error always
    // present); the tools schema forwards only type/properties/required; stream is on.
    assert_eq!(
        body_json(request),
        json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 4096,
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

// Go: provider/anthropic_wire_test.go:153
#[tokio::test]
async fn test_anthropic_stream_transcript() {
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
}

// Go: provider/anthropic_wire_test.go:249
#[tokio::test]
async fn test_anthropic_stream_error_event() {
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

// Go: provider/anthropic_wire_test.go:279
#[tokio::test]
async fn test_anthropic_models_pagination() {
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

// Go: provider/defermode_wire_test.go:16
#[tokio::test]
async fn test_anthropic_defer_loading_wire() {
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

// Go: provider/defermode_wire_test.go:176
#[tokio::test]
async fn test_anthropic_server_block_capture() {
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

// Go: provider/defermode_wire_test.go:244
#[tokio::test]
async fn test_anthropic_server_block_replay() {
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

// Go: provider/usage_wire_test.go:15 (anthropic case)
#[tokio::test]
async fn test_unary_chat_owns_its_usage_anthropic() {
    const WITH_USAGE: &str = r#"{"content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":11,"output_tokens":7}}"#;
    const WITHOUT_USAGE: &str = r#"{"content":[{"type":"text","text":"hi"}]}"#;

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
            ResponseTemplate::new(200).set_body_raw(body, "application/json")
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

    // A second call whose response omits usage must not keep reporting the first call's figures.
    let second = p
        .chat(&cancel, &[Message::user("q")])
        .await
        .expect("unary chat without usage");
    assert_eq!(second.text, "hi");
    assert_eq!(second.usage, None);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    // A unary request never streams and never carries tools.
    let requests = server.received_requests().await.expect("recorded requests");
    let body = body_json(&requests[0]);
    assert_eq!(body.get("stream"), None);
    assert_eq!(body.get("tools"), None);
    assert_eq!(body["max_tokens"], json!(4096));
}

// Go: provider/defermode_wire_test.go:293 TestAnthropicThinkingBlockCaptureAndReplay — a thinking
// block is captured WITH its signature and replayed on the next request unconditionally, no
// deferred tool required (unlike the server blocks above). Endpoints that implement thinking mode
// reject an assistant turn whose thinking is missing ("the content[].thinking in the thinking mode
// must be passed back to the API"), and the block must lead the content array.
#[tokio::test]
async fn test_anthropic_thinking_block_capture_and_replay() {
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

// Go: provider/defermode_wire_test.go:378 TestAnthropicThinkingBlockWithoutSignature — an endpoint
// that implements thinking mode without sealing the block sends no `signature_delta`; the replayed
// block then carries no `"signature"` key at all rather than an empty one, so what goes back is
// exactly what came in.
#[tokio::test]
async fn test_anthropic_thinking_block_without_signature() {
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

// Go: provider/defermode_wire_test.go:408 TestAnthropicSignatureOnlyThinkingBlockReplays — a block
// can arrive sealed but empty (DeepSeek's Anthropic endpoint sends `signature_delta` with no
// `thinking_delta`). The signature alone makes it part of the turn, so it replays.
#[tokio::test]
async fn test_anthropic_signature_only_thinking_block_replays() {
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

// Go: provider/defermode_wire_test.go:442 TestAnthropicEmptyThinkingBlockDropped — neither body nor
// signature means the block never carried anything; it stays out of the replay.
#[tokio::test]
async fn test_anthropic_empty_thinking_block_dropped() {
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
