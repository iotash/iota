//! `openai` provider tests (`provider/openai_wire_test.go`, `provider/defermode_wire_test.go:137`,
//! `provider/usage_wire_test.go`): the golden request, the streaming consumption contract, the inline
//! `<think>` splitting, the system-tools mount and the per-call usage ownership.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::mock_sse;
use iota::provider::model::{Attachment, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef};
use iota::provider::openai::OpenAiProvider;
use iota::provider::usage::Usage;
use iota::provider::{Effort, Provider, RoundResult, ToolProvider, TopPTunable, Tunable};
use iota::testing::RecordingSink;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

/// A provider aimed at `server` (Go: `NewOpenAI(key, srv.URL, model, temp, srv.Client())`).
fn provider(
    server: &MockServer,
    key: &str,
    model: &str,
    temperature: Option<f64>,
) -> OpenAiProvider {
    OpenAiProvider::new(
        key,
        &server.uri(),
        model,
        temperature,
        reqwest::Client::new(),
    )
}

/// One streaming round with a recording sink.
async fn round(
    p: &OpenAiProvider,
    messages: &[Message],
    tools: &[ToolDef],
) -> (RoundResult, RecordingSink) {
    let mut sink = RecordingSink::default();
    let cancel = CancellationToken::new();
    let res = p
        .stream_chat_with_tools(&cancel, messages, tools, &mut sink)
        .await
        .expect("round failed");
    (res, sink)
}

/// The single request the server recorded, as JSON.
async fn recorded(server: &MockServer) -> (Request, Value) {
    let mut reqs = server
        .received_requests()
        .await
        .expect("no request recorded");
    assert_eq!(reqs.len(), 1, "expected exactly one request");
    let req = reqs.remove(0);
    let body = serde_json::from_slice(&req.body).expect("request body is not JSON");
    (req, body)
}

/// Answers each request with the next body, the last repeating (two responses in one test).
struct Sequence {
    bodies: Vec<&'static str>,
    seen: AtomicUsize,
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self
            .seen
            .fetch_add(1, Ordering::SeqCst)
            .min(self.bodies.len() - 1);
        ResponseTemplate::new(200).set_body_raw(self.bodies[i], "application/json")
    }
}

/// A JSON-Schema object `{"type":"object"}`.
fn object_schema() -> JsonObject {
    let mut m = JsonObject::new();
    m.insert("type".to_owned(), Value::String("object".to_owned()));
    m
}

/// The recorded assistant payload the golden request replays verbatim (kimi `reasoning` preserved).
const RAW_ASSISTANT: &str = r#"{"role":"assistant","content":"prev","reasoning":"think","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]}"#;

// Go: provider/openai_wire_test.go:21
/// The exact request JSON the openai provider emits — the wire contract OpenAI-compatible servers see:
/// message shapes, attachment parts (image data-URL / bare-b64 file / text LAST), tool definitions,
/// verbatim raw-JSON assistant replay, stream options.
#[tokio::test]
async fn test_openai_golden_request() {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", "data: [DONE]\n\n").await;

    let mut p = provider(&server, "sk-test", "gpt-4o", Some(0.7));
    p.set_effort(Some(Effort::High));
    p.set_top_p(Some(0.9));

    let call = ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    };
    let msgs = vec![
        Message::system("sys"),
        Message {
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
            ],
            ..Message::user("look")
        },
        Message::assistant_with_calls(
            "prev",
            vec![call.clone()],
            Some(RawContent::OpenAi(
                Raw::from_string(RAW_ASSISTANT.to_owned()).unwrap(),
            )),
        ),
        Message::tool_result(&call, "result", false),
    ];
    let tools = vec![ToolDef {
        name: "f".to_owned(),
        description: "does f".to_owned(),
        input_schema: Some(object_schema()),
        deferred: false,
    }];
    round(&p, &msgs, &tools).await;

    let (req, got) = recorded(&server).await;
    assert_eq!(
        req.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer sk-test"
    );
    assert_eq!(req.url.path(), "/chat/completions");

    for (key, want) in [
        ("model", json!("gpt-4o")),
        ("temperature", json!(0.7)),
        ("top_p", json!(0.9)),
        ("reasoning_effort", json!("high")),
        ("stream", json!(true)),
        ("stream_options", json!({"include_usage": true})),
    ] {
        assert_eq!(got[key], want, "{key}");
    }

    let messages = got["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "messages");
    assert_eq!(messages[0], json!({"role": "system", "content": "sys"}));
    // user parts: image data-URL, file bare-b64 + filename, text LAST
    assert_eq!(
        messages[1],
        json!({"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AQ=="}},
            {"type": "file", "file": {"file_data": "Ag==", "filename": "b.pdf"}},
            {"type": "text", "text": "look"},
        ]})
    );
    // assistant raw replay is verbatim (kimi reasoning preserved)
    assert_eq!(messages[2]["reasoning"], json!("think"));
    assert_eq!(
        messages[2],
        serde_json::from_str::<Value>(RAW_ASSISTANT).unwrap()
    );
    assert_eq!(
        messages[3],
        json!({"role": "tool", "content": "result", "tool_call_id": "c1"})
    );
    assert_eq!(
        got["tools"],
        json!([{"type": "function", "function": {
            "name": "f", "description": "does f", "parameters": {"type": "object"},
        }}])
    );
}

// Go: provider/openai_wire_test.go:114
/// A recorded-style SSE transcript pins the consumption contract: reasoning deltas (both field spellings)
/// reach the sink, which closes before the first content write; interleaved index-keyed tool-call deltas
/// assemble in order; usage lands from the final chunk; `finish_reason=tool_calls` yields tool calls plus a
/// verbatim-replayable raw assistant JSON.
#[tokio::test]
async fn test_openai_stream_transcript() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning\":\"th\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"ink\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\"}},{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"other\",\"arguments\":\"{}\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"x\\\"}\"}}]},\"finish_reason\":null}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
        "\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let tools = [ToolDef {
        name: "lookup".to_owned(),
        ..ToolDef::default()
    }];
    let (res, sink) = round(&p, &[Message::user("q")], &tools).await;

    assert_eq!(res.reasoning, "think");
    assert_eq!(sink.reasoning(), "think");
    assert!(sink.closed_before_content(), "reasoning never closed");

    assert_eq!(res.content, "Let me check.");
    assert_eq!(sink.content(), res.content);

    assert_eq!(res.tool_calls.len(), 2);
    assert_eq!(res.tool_calls[0].id, "c1");
    assert_eq!(res.tool_calls[0].name, "lookup");
    assert_eq!(res.tool_calls[0].arguments["q"], json!("x"));
    assert_eq!(res.tool_calls[1].id, "c2");
    assert_eq!(res.tool_calls[1].name, "other");
    assert!(res.tool_calls[1].arguments.is_empty());

    assert_eq!(
        res.usage,
        Some(Usage {
            input: 10,
            output: 5,
            total: 15,
            ..Usage::default()
        })
    );

    let Some(RawContent::OpenAi(raw)) = &res.raw_content else {
        panic!("raw assistant JSON = {:?}", res.raw_content);
    };
    assert!(raw.get().contains(r#""tool_calls""#), "{}", raw.get());
    assert!(
        raw.get().contains(r#""reasoning":"think""#),
        "{}",
        raw.get()
    );
    assert_eq!(
        serde_json::from_str::<Value>(raw.get()).unwrap(),
        json!({
            "role": "assistant",
            "content": "Let me check.",
            "reasoning": "think",
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "other", "arguments": "{}"}},
            ],
        })
    );
}

// Go: provider/openai_wire_test.go:179
/// A content stream opening with `<think>` (tags split across deltas) is a leaked reasoning block: it reaches
/// the reasoning channel, which closes before the first visible write, and the returned content and reasoning
/// are clean. Go drives this through `StreamChat`; the port folds that into the tools variant with no tools.
#[tokio::test]
async fn test_openai_stream_inline_think() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"<th\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"ink>pond\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"ering</think>\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\\n\\nhello\"}}]}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let (res, sink) = round(&p, &[Message::user("q")], &[]).await;

    assert_eq!(res.reasoning, "pondering");
    assert_eq!(sink.reasoning(), "pondering");
    assert!(
        sink.closed_before_content(),
        "reasoning must close before the first content write: {:?}",
        sink.events
    );
    assert_eq!(res.content, "hello");
    assert_eq!(sink.content(), "hello");
    assert!(res.tool_calls.is_empty() && res.raw_content.is_none());

    // No tools advertised ⇒ no `tools` key at all (Go `omitempty`).
    let (_, body) = recorded(&server).await;
    assert!(body.get("tools").is_none(), "{body}");
}

// Go: provider/openai_wire_test.go:228
/// The interleaved-thinking shape: the round ends in tool calls with the think block never closed — the whole
/// text is reasoning, content stays empty, and the raw assistant replay keeps the verbatim unclosed tag with
/// NO duplicate reasoning field.
#[tokio::test]
async fn test_openai_stream_inline_think_tool_round() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"<think>need\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" a tool\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let tools = [ToolDef {
        name: "f".to_owned(),
        ..ToolDef::default()
    }];
    let (res, sink) = round(&p, &[Message::user("q")], &tools).await;

    assert_eq!(res.reasoning, "need a tool");
    assert_eq!(sink.reasoning(), "need a tool");
    assert!(sink.closed_before_content());
    assert_eq!(res.content, "");
    assert_eq!(sink.content(), "");
    assert_eq!(res.tool_calls.len(), 1);
    assert_eq!(res.tool_calls[0].id, "c1");
    assert_eq!(res.tool_calls[0].name, "f");

    let Some(RawContent::OpenAi(raw)) = &res.raw_content else {
        panic!("raw assistant JSON = {:?}", res.raw_content);
    };
    let raw_msg: Value = serde_json::from_str(raw.get()).unwrap();
    assert_eq!(
        raw_msg["content"],
        json!("<think>need a tool"),
        "raw content must keep the verbatim unclosed tag"
    );
    assert!(
        raw_msg.get("reasoning").is_none(),
        "tag-extracted think must not duplicate into the reasoning field: {raw_msg}"
    );
}

// Go: provider/defermode_wire_test.go:137
/// The system-tools mode wire (chatcomp): a system message carrying Tools serializes as role system + tools
/// and NO content key (the K3 constraint), keeping its place in the message order.
#[tokio::test]
async fn test_openai_system_tools_message_wire() {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", "data: [DONE]\n\n").await;

    let p = provider(&server, "sk-test", "kimi-k3", None);
    let msgs = vec![
        Message::user("go"),
        Message::system_tools(vec![ToolDef {
            name: "loaded_tool".to_owned(),
            description: "d".to_owned(),
            input_schema: Some(object_schema()),
            deferred: false,
        }]),
    ];
    round(&p, &msgs, &[]).await;

    let (_, got) = recorded(&server).await;
    let msgs = got["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "messages");
    assert_eq!(msgs[0], json!({"role": "user", "content": "go"}));
    assert_eq!(msgs[1]["role"], json!("system"));
    assert!(
        msgs[1].get("content").is_none(),
        "the tools mount must carry NO content key (K3 400s otherwise)"
    );
    assert_eq!(
        msgs[1]["tools"],
        json!([{"type": "function", "function": {
            "name": "loaded_tool", "description": "d", "parameters": {"type": "object"},
        }}])
    );
}

// Go: provider/usage_wire_test.go:15 (openai case)
/// The UNARY `chat` path owns its usage figures exactly like the streaming path: it reports what the response
/// carried, and a response without a usage block reads as "unknown" instead of leaving the previous call's
/// numbers standing.
#[tokio::test]
async fn test_unary_chat_owns_its_usage_openai() {
    const WITH_USAGE: &str = r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#;
    const WITHOUT_USAGE: &str = r#"{"choices":[{"message":{"content":"hi"}}]}"#;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Sequence {
            bodies: vec![WITH_USAGE, WITHOUT_USAGE],
            seen: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let p = provider(&server, "k", "m", None);
    let cancel = CancellationToken::new();
    let msgs = [Message::user("q")];

    let first = p.chat(&cancel, &msgs).await.expect("Chat");
    assert_eq!(first.text, "hi");
    assert_eq!(
        first.usage,
        // No total on the wire ⇒ the converter falls back to input+output.
        Some(Usage {
            input: 11,
            output: 7,
            total: 18,
            ..Usage::default()
        })
    );

    // A second call whose response omits usage must not keep reporting the first call's figures.
    let second = p.chat(&cancel, &msgs).await.expect("Chat (no usage)");
    assert_eq!(second.text, "hi");
    assert_eq!(second.usage, None, "stale usage reported");

    // Neither request carries `stream` or `stream_options` (Complete forces them off).
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    for req in &reqs {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body.get("stream").is_none() && body.get("stream_options").is_none());
    }
}

/// DIVERGENCES F-04: a server that emits sparse tool-call indices (a gap in the key set) keeps every call.
/// Go iterated `0..len(map)` and silently dropped everything after the gap.
#[tokio::test]
async fn sparse_tool_call_indices_are_kept() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":2,\"id\":\"c2\",\"function\":{\"name\":\"second\",\"arguments\":\"{}\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c0\",\"function\":{\"name\":\"first\",\"arguments\":\"{\\\"a\\\":1}\"}},{\"index\":-3,\"function\":{\"arguments\":\"!\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let (res, _) = round(&p, &[Message::user("q")], &[]).await;

    // Index 2 survives the gap, and the negative index clamps onto slot 0 (Go's `if idx < 0 { idx = 0 }`),
    // whose arguments then fail to parse and fall back to an empty map.
    assert_eq!(res.tool_calls.len(), 2);
    assert_eq!(res.tool_calls[0].id, "c0");
    assert_eq!(res.tool_calls[0].name, "first");
    assert!(res.tool_calls[0].arguments.is_empty());
    assert_eq!(res.tool_calls[1].id, "c2");
    assert_eq!(res.tool_calls[1].name, "second");
    assert!(res.tool_calls[1].arguments.is_empty());

    let Some(RawContent::OpenAi(raw)) = &res.raw_content else {
        panic!("raw assistant JSON = {:?}", res.raw_content);
    };
    assert_eq!(
        serde_json::from_str::<Value>(raw.get()).unwrap(),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {"id": "c0", "type": "function", "function": {"name": "first", "arguments": "{\"a\":1}!"}},
                {"id": "c2", "type": "function", "function": {"name": "second", "arguments": "{}"}},
            ],
        })
    );
}

/// Tool-call fragments are discarded unless the round finishes with `tool_calls`, and the replay payload is
/// cleared with them (openai.go:273,310).
#[tokio::test]
async fn tool_calls_without_a_tool_calls_finish_are_dropped() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\",\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let (res, _) = round(&p, &[Message::user("q")], &[]).await;
    assert_eq!(res.content, "hi");
    assert!(res.tool_calls.is_empty());
    assert!(res.raw_content.is_none());
    assert_eq!(res.usage, None, "no usage on the wire");
}

/// A streaming usage block without `total_tokens` is ignored (the unary path accepts one) — openai.go:217.
#[tokio::test]
async fn streaming_usage_without_a_total_is_ignored() {
    const TRANSCRIPT: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3}}\n",
        "\n",
        "data: [DONE]\n",
    );
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", TRANSCRIPT).await;

    let p = provider(&server, "k", "m", None);
    let (res, _) = round(&p, &[Message::user("q")], &[]).await;
    assert_eq!(res.content, "hi");
    assert_eq!(res.usage, None);
}

/// A stream that never emits an event (a compat server answering a stream request with a plain body) is
/// `ErrNoEvents` under Go's `stream error:` prefix; an in-band chunk error keeps its message.
#[tokio::test]
async fn stream_failures_keep_the_go_prefixes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"{"choices":[]}"#, "application/json"),
        )
        .mount(&server)
        .await;
    let p = provider(&server, "k", "m", None);
    let mut sink = RecordingSink::default();
    let err = p
        .stream_chat_with_tools(
            &CancellationToken::new(),
            &[Message::user("q")],
            &[],
            &mut sink,
        )
        .await
        .expect_err("a plain body must not look like a stream");
    assert_eq!(
        err.to_string(),
        "stream error: stream ended without any SSE events (server did not stream?)"
    );
    // Even on the error path the reasoning channel is closed exactly once.
    assert_eq!(sink.events.len(), 1);
    assert!(sink.closed_before_content());

    let server = MockServer::start().await;
    mock_sse(
        &server,
        "POST",
        "/chat/completions",
        "data: {\"error\":{\"message\":\"boom\"}}\n\n",
    )
    .await;
    let p = provider(&server, "k", "m", None);
    let mut sink = RecordingSink::default();
    let err = p
        .stream_chat_with_tools(
            &CancellationToken::new(),
            &[Message::user("q")],
            &[],
            &mut sink,
        )
        .await
        .expect_err("in-band error");
    assert_eq!(
        err.to_string(),
        r#"stream error: received error while streaming: {"message":"boom"}"#
    );
}

/// `list_models` sorts the ids and wraps failures in Go's `failed to list models:` prefix; a unary response
/// with no choices is `no response choices`.
#[tokio::test]
async fn list_models_and_no_choices() {
    let server = MockServer::start().await;
    crate::common::mock_json(
        &server,
        "GET",
        "/models",
        json!({"data": [{"id": "gpt-4o"}, {"id": "gpt-3.5"}]}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(r"{}", "application/json"))
        .mount(&server)
        .await;

    let p = provider(&server, "k", "m", None);
    let cancel = CancellationToken::new();
    assert_eq!(p.list_models(&cancel).await.unwrap(), ["gpt-3.5", "gpt-4o"]);
    assert_eq!(
        p.chat(&cancel, &[Message::user("q")])
            .await
            .expect_err("no choices")
            .to_string(),
        "no response choices"
    );

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"error":{"message":"nope","type":"invalid_request_error"}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let p = provider(&server, "k", "m", None);
    let text = p
        .list_models(&CancellationToken::new())
        .await
        .expect_err("401")
        .to_string();
    assert!(text.starts_with("failed to list models: GET \""), "{text}");
    assert!(
        text.ends_with(
            "/models\": 401 Unauthorized {\"message\":\"nope\",\"type\":\"invalid_request_error\"}"
        ),
        "{text}"
    );
}
