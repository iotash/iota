//! `openresponses` provider tests (`provider/openresponses_wire_test.go`, `defermode_wire_test.go:69-133`,
//! `usage_wire_test.go`): the golden request, the stream transcript, inline think splitting, the three
//! terminal failure events, the unary surface, image outputs and the 4-leg client-executed tool-search
//! protocol.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crate::common::{body_json, mock_json, mock_sse};
use iota::llm::LlmError;
use iota::provider::error::{ProviderError, WireOp};
use iota::provider::model::{Attachment, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef};
use iota::provider::openai::OpenAiProvider;
use iota::provider::openresponses::OpenResponsesProvider;
use iota::provider::sink::StreamSink;
use iota::provider::{
    ImageTunable, Provider, ProviderKind, RoundResult, ToolProvider, ToolSearchHost, ToolSearcher,
    TopPTunable, Tunable,
};
use iota::testing::{RecordingSink, SinkEvent};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// A minimal clean Responses stream: no `[DONE]` sentinel, the terminal `response.completed` then EOF.
const RESP_COMPLETED_SSE: &str = r#"event: response.completed
data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}

"#;

/// A provider over `server` with no configured tuning.
fn provider(server: &MockServer, model: &str) -> OpenResponsesProvider {
    OpenResponsesProvider::new("k", &server.uri(), model, None, reqwest::Client::new())
}

/// One streaming round with a recording sink.
async fn round(
    p: &OpenResponsesProvider,
    messages: &[Message],
    tools: &[ToolDef],
) -> (RoundResult, RecordingSink) {
    let cancel = CancellationToken::new();
    let mut sink = RecordingSink::default();
    let out = p
        .stream_chat_with_tools(&cancel, messages, tools, &mut sink)
        .await
        .expect("streaming round failed");
    (out, sink)
}

/// The raw replay items of a round, parsed.
fn raw_items(out: &RoundResult) -> Vec<Value> {
    match out.raw_content.as_ref() {
        Some(RawContent::OpenResponses(items)) => items
            .iter()
            .map(|i| serde_json::from_str(i.get()).unwrap())
            .collect(),
        other => panic!("raw content = {other:?}, want RawContent::OpenResponses"),
    }
}

/// A persisted reasoning item with provider-specific fields that must survive replay byte-for-byte.
const REASONING_ITEM: &str = r#"{"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"hidden"}],"encrypted_content":"OPAQUE"}"#;

/// A `{"type":"object"}` schema.
fn object_schema() -> JsonObject {
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), "object".into());
    schema
}

/// Mounts an SSE responder that answers each successive POST /responses with the next transcript.
async fn mock_sse_sequence(server: &MockServer, legs: &'static [&'static str]) {
    let hits = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |_: &Request| {
            let n = hits.fetch_add(1, Ordering::SeqCst);
            let body = legs.get(n).copied().unwrap_or("");
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        })
        .mount(server)
        .await;
}

// Go: provider/openresponses_wire_test.go:32
#[tokio::test]
async fn test_open_responses_golden_request() {
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", RESP_COMPLETED_SSE).await;

    let mut p = OpenResponsesProvider::new(
        "sk-test",
        &srv.uri(),
        "gpt-5",
        Some(0.7),
        reqwest::Client::new(),
    );
    p.set_effort(Some(iota::provider::Effort::High));
    p.set_top_p(Some(0.9));

    // Raw output items as persisted by a previous round: a reasoning item with provider-specific fields, a
    // message item (must be SKIPPED on replay), and the function call itself.
    let raw = vec![
        Raw::from_string(REASONING_ITEM.to_owned()).unwrap(),
        Raw::from_string(
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"prev"}]}"#
                .to_owned(),
        )
        .unwrap(),
        Raw::from_string(
            r#"{"id":"call_1","type":"function_call","call_id":"call_1","name":"f","arguments":"{}"}"#
                .to_owned(),
        )
        .unwrap(),
    ];

    let call = ToolCall {
        id: "call_1".to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    };
    let messages = vec![
        Message::system("sys"),
        // A system-tools mount is chatcomp-only: it must not become an input item NOR clear `instructions`.
        Message::system_tools(vec![ToolDef {
            name: "mounted".to_owned(),
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
            ],
            ..Message::default()
        },
        Message::assistant_with_calls(
            "prev",
            vec![call.clone()],
            Some(RawContent::OpenResponses(raw)),
        ),
        Message::tool_result(&call, "result", false),
    ];
    let tools = vec![ToolDef {
        name: "f".to_owned(),
        description: "does f".to_owned(),
        input_schema: Some(object_schema()),
        deferred: false,
    }];
    round(&p, &messages, &tools).await;

    let reqs = srv.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].headers.get("authorization").unwrap(),
        "Bearer sk-test"
    );
    assert_eq!(reqs[0].url.path(), "/responses");
    let got = body_json(&reqs[0]);

    for (key, want) in [
        ("model", json!("gpt-5")),
        ("temperature", json!(0.7)),
        ("top_p", json!(0.9)),
        ("instructions", json!("sys")),
        ("reasoning", json!({"effort": "high"})),
        ("stream", json!(true)),
    ] {
        assert_eq!(got[key], want, "{key}");
    }

    // input: user message, replayed reasoning + function_call (message item skipped), function_call_output.
    // System text is NOT an input item and neither is the tools mount.
    let input = got["input"].as_array().unwrap();
    assert_eq!(input.len(), 4, "input = {input:#?}");

    // user parts: input_image (plain data-URL string, detail auto), input_file (bare b64 + filename),
    // input_text LAST.
    assert_eq!(input[0]["role"], json!("user"));
    let parts = input[0]["content"].as_array().unwrap();
    assert_eq!(
        parts[0],
        json!({"type":"input_image","image_url":"data:image/png;base64,AQ==","detail":"auto"})
    );
    assert_eq!(
        parts[1],
        json!({"type":"input_file","file_data":"Ag==","filename":"b.pdf"}),
        "file_data must be bare base64, never a data URL"
    );
    assert_eq!(parts[2], json!({"type":"input_text","text":"look"}));
    assert_eq!(parts.len(), 3);

    // The reasoning item replays verbatim, provider-specific fields intact.
    assert_eq!(
        input[1],
        serde_json::from_str::<Value>(REASONING_ITEM).unwrap()
    );
    // The function_call replays with its `id` STRIPPED (OpenAI requires fc_-prefixed ids; Bedrock gateways
    // reuse ids across parallel calls); the message item is skipped entirely.
    assert_eq!(
        input[2],
        json!({"type":"function_call","call_id":"call_1","name":"f","arguments":"{}"})
    );
    assert!(
        input[2].get("id").is_none(),
        "id must be stripped: {}",
        input[2]
    );
    assert_eq!(
        input[3],
        json!({"type":"function_call_output","call_id":"call_1","output":"result"})
    );

    // tools are FLAT (no nested "function" object) and strict:false is explicit.
    let tool = &got["tools"].as_array().unwrap()[0];
    assert_eq!(
        *tool,
        json!({"type":"function","name":"f","description":"does f","parameters":{"type":"object"},"strict":false})
    );
    assert!(
        tool.get("function").is_none(),
        "chat-completions shape leaked"
    );
}

/// A tool whose `input_schema` is the EMPTY object — what an MCP server that declares no arguments sends —
/// advertises with NO `parameters` on both dialects (Go's `omitempty` on a map; `"parameters":{}` is a schema
/// with no `type`, and not every server accepts it). Until 2026-09-15 only the chat-completions path filtered
/// it, at its call site; the predicate now sits on both wire structs, and this pins the two outputs together.
#[tokio::test]
async fn empty_input_schema_is_omitted_on_both_dialects() {
    let tool = ToolDef {
        name: "f".to_owned(),
        description: "does f".to_owned(),
        input_schema: Some(JsonObject::new()),
        deferred: false,
    };
    let messages = vec![Message::user("hi")];

    let responses = MockServer::start().await;
    mock_sse(&responses, "POST", "/responses", RESP_COMPLETED_SSE).await;
    round(
        &provider(&responses, "gpt-5"),
        &messages,
        std::slice::from_ref(&tool),
    )
    .await;
    let got = body_json(&responses.received_requests().await.unwrap()[0]);
    assert_eq!(
        got["tools"],
        json!([{"type":"function","name":"f","description":"does f","strict":false}])
    );

    let chat = MockServer::start().await;
    mock_sse(&chat, "POST", "/chat/completions", "data: [DONE]\n\n").await;
    let p = OpenAiProvider::new("k", &chat.uri(), "gpt-4o", None, reqwest::Client::new());
    p.stream_chat_with_tools(
        &CancellationToken::new(),
        &messages,
        std::slice::from_ref(&tool),
        &mut RecordingSink::default(),
    )
    .await
    .expect("chat-completions round failed");
    let got = body_json(&chat.received_requests().await.unwrap()[0]);
    assert_eq!(
        got["tools"],
        json!([{"type":"function","function":{"name":"f","description":"does f"}}])
    );
}

/// The recorded transcript of `TestOpenResponsesStreamTranscript`.
// Go: provider/openresponses_wire_test.go:156
const TRANSCRIPT: &str = r#"event: response.created
data: {"type":"response.created","response":{"status":"in_progress"}}

event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"th"}

event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"ink"}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"think"}],"encrypted_content":"OPAQUE"}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"Let me "}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"check."}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Let me check."}]}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"q\":"}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"x\"}"}

event: response.function_call_arguments.done
data: {"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"q\":\"x\"}"}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"x\"}"}}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_2","name":"other","arguments":"{}"}}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18}}}
"#;

// Go: provider/openresponses_wire_test.go:155
#[tokio::test]
async fn test_open_responses_stream_transcript() {
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", TRANSCRIPT).await;
    let p = provider(&srv, "m");

    let tools = vec![ToolDef {
        name: "lookup".to_owned(),
        ..ToolDef::default()
    }];
    let (out, sink) = round(&p, &[Message::user("q")], &tools).await;

    assert_eq!(out.reasoning, "think");
    assert_eq!(sink.reasoning(), "think");
    assert!(
        sink.events.contains(&SinkEvent::ReasoningDone),
        "reasoning was never closed: {:?}",
        sink.events
    );
    assert!(
        sink.closed_before_content(),
        "reasoning must close before the first content write: {:?}",
        sink.events
    );
    assert_eq!(out.content, "Let me check.");
    assert_eq!(sink.content(), "Let me check.");

    // Two function_call items sharing item id fc_1 but with distinct call_ids (the Bedrock/zenmux parallel-call
    // collapse) yield two ToolCalls, in arrival order.
    let mut want_args = JsonObject::new();
    want_args.insert("q".to_owned(), "x".into());
    assert_eq!(
        out.tool_calls,
        vec![
            ToolCall {
                id: "call_1".to_owned(),
                name: "lookup".to_owned(),
                arguments: want_args,
            },
            ToolCall {
                id: "call_2".to_owned(),
                name: "other".to_owned(),
                arguments: JsonObject::new(),
            },
        ]
    );

    let usage = out.usage.expect("usage");
    assert_eq!((usage.input, usage.output, usage.total), (11, 7, 18));

    // Raw record: every completed item in order, VERBATIM — id hygiene happens at replay time.
    let items = raw_items(&out);
    assert_eq!(items.len(), 4, "{items:#?}");
    assert_eq!(items[0]["type"], json!("reasoning"));
    assert_eq!(items[0]["encrypted_content"], json!("OPAQUE"));
    assert_eq!(items[1]["type"], json!("message"));
    assert_eq!(
        (&items[2]["id"], &items[2]["call_id"], &items[2]["name"]),
        (&json!("fc_1"), &json!("call_1"), &json!("lookup"))
    );
    assert_eq!(
        (&items[3]["id"], &items[3]["call_id"]),
        (&json!("fc_1"), &json!("call_2")),
        "upstream id reuse must be preserved at record time"
    );
}

// Go: provider/openresponses_wire_test.go:266
#[tokio::test]
async fn test_open_responses_stream_inline_think() {
    // Relays that don't parse reasoning leak <think> into output_text deltas, split across frames.
    const SSE: &str = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"<think>pond"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"ering</think>\n\nhi"}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}
"#;
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", SSE).await;
    let p = provider(&srv, "m");

    let (out, sink) = round(&p, &[Message::user("q")], &[]).await;
    assert_eq!(out.reasoning, "pondering");
    assert_eq!(sink.reasoning(), "pondering");
    assert!(sink.events.contains(&SinkEvent::ReasoningDone));
    assert!(sink.closed_before_content(), "{:?}", sink.events);
    assert_eq!(out.content, "hi");
    assert_eq!(sink.content(), "hi");
    assert!(out.tool_calls.is_empty());
    assert!(out.raw_content.is_none());
}

// Go: provider/openresponses_wire_test.go:314
#[tokio::test]
async fn test_open_responses_terminal_events() {
    let cases: [(&str, &str, &[&str], &str); 3] = [
        (
            "response.failed",
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"model exploded\"}}}\n\n",
            &["response.failed", "model exploded", "server_error"],
            "server_error",
        ),
        (
            "response.incomplete",
            "event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
            &["response.incomplete", "max_output_tokens"],
            "max_output_tokens",
        ),
        (
            "error event",
            "event: error\ndata: {\"type\":\"error\",\"code\":\"ERR_UPSTREAM\",\"message\":\"bad stream\",\"param\":null,\"sequence_number\":1}\n\n",
            &["error", "bad stream", "ERR_UPSTREAM"],
            "ERR_UPSTREAM",
        ),
    ];
    for (name, terminal, subs, want_code) in cases {
        let srv = MockServer::start().await;
        let body = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"status\":\"in_progress\"}}}}\n\n{terminal}"
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&srv)
            .await;
        let p = provider(&srv, "m");

        let cancel = CancellationToken::new();
        let mut sink = RecordingSink::default();
        let err = p
            .stream_chat_with_tools(&cancel, &[Message::user("q")], &[], &mut sink)
            .await
            .expect_err("a terminal failure event surfaced no error (the old silent-EOF bug)");

        assert!(
            matches!(
                err,
                ProviderError::Wire {
                    op: WireOp::Stream,
                    ..
                }
            ),
            "{name}: {err:?}"
        );
        let text = err.to_string();
        assert!(text.starts_with("stream error: "), "{name}: {text}");
        for sub in subs {
            assert!(
                text.contains(sub),
                "{name}: {text:?} does not mention {sub:?}"
            );
        }
        // The structured failure is reachable, typed, behind the `stream error:` prefix.
        let llm = err.llm().expect("a wire failure");
        assert!(
            !matches!(llm, LlmError::NoEvents),
            "{name}: want a structured failure, not no-events"
        );
        match llm {
            LlmError::Failure(f) => assert_eq!(f.code, want_code, "{name}"),
            other => panic!("{name}: err = {other:?}, want LlmError::Failure"),
        }
    }
}

// Go: provider/openresponses_wire_test.go:374
#[tokio::test]
async fn test_open_responses_chat_and_models() {
    let srv = MockServer::start().await;
    mock_json(
        &srv,
        "POST",
        "/responses",
        json!({"id":"resp_1","status":"completed","output":[
            {"id":"rs_1","type":"reasoning","summary":[]},
            {"id":"msg_1","type":"message","content":[
                {"type":"output_text","text":"Hello"},
                {"type":"refusal","refusal":"no"},
                {"type":"output_text","text":" world"}]}]}),
    )
    .await;
    mock_json(
        &srv,
        "GET",
        "/models",
        json!({"object":"list","data":[{"id":"b-model"},{"id":"a-model"}]}),
    )
    .await;

    let p = provider(&srv, "m");
    let cancel = CancellationToken::new();

    // Unary Chat concatenates output_text parts across items, skipping refusals (SDK OutputText parity).
    let out = p.chat(&cancel, &[Message::user("hi")]).await.unwrap();
    assert_eq!(out.text, "Hello world");
    assert!(out.images.is_empty());
    assert!(out.usage.is_none(), "a usage-less response reports nothing");

    let reqs = srv.received_requests().await.unwrap();
    let got = body_json(&reqs[0]);
    assert!(
        got.get("stream").is_none(),
        "a non-streaming request carries a stream flag: {got}"
    );

    let models = p.list_models(&cancel).await.unwrap();
    assert_eq!(models, vec!["a-model", "b-model"]);
    assert_eq!(p.kind(), ProviderKind::OpenResponses);
    assert_eq!(p.model(), "m");
}

// Go: provider/openresponses_wire_test.go:409
#[tokio::test]
async fn test_open_responses_image_generation() {
    // base64 of [9, 8, 7].
    const SSE: &str = r#"data: {"type":"response.output_item.done","item":{"id":"ig_1","type":"image_generation_call","status":"completed","output_format":"png","result":"CQgH"}}

data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}

"#;
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", SSE).await;
    let mut p = provider(&srv, "gpt-5");
    p.set_image_output(true);
    assert!(p.image_output());

    let tools = vec![ToolDef {
        name: "f".to_owned(),
        input_schema: Some(object_schema()),
        ..ToolDef::default()
    }];
    let (out, _) = round(&p, &[Message::user("draw")], &tools).await;

    // The built-in leads the tools array, with partial_images declared.
    let got = body_json(&srv.received_requests().await.unwrap()[0]);
    let declared = got["tools"].as_array().unwrap();
    assert_eq!(declared.len(), 2, "{declared:#?}");
    assert_eq!(
        declared[0],
        json!({"type":"image_generation","partial_images":1})
    );
    assert_eq!(declared[1]["name"], json!("f"));

    assert_eq!(
        out.images,
        vec![Attachment {
            filename: String::new(),
            mime_type: "image/png".to_owned(),
            data: vec![9, 8, 7],
        }]
    );

    // The raw replay keeps the call item id (server-side multiturn context) but never the b64 payload.
    let items = raw_items(&out);
    assert_eq!(
        items,
        vec![json!({
            "id":"ig_1","type":"image_generation_call","status":"completed","output_format":"png"
        })]
    );
}

/// Records everything the image path emits: the composing observer's NAMES (which raise the
/// generation widget) beside every decoded progressive frame.
#[derive(Default)]
struct ImageSink {
    raised: Vec<String>,
    partials: Vec<Vec<u8>>,
}

impl StreamSink for ImageSink {
    fn content(&mut self, _delta: &str) {}

    fn reasoning(&mut self, _delta: &str) {}

    fn reasoning_done(&mut self) {}

    fn tool_delta(&mut self, name: Option<&str>, _delta: &str) {
        self.raised.push(name.unwrap_or_default().to_owned());
    }

    fn image_partial(&mut self, frame: &[u8]) {
        self.partials.push(frame.to_vec());
    }
}

// Go: provider/openresponses_wire_test.go:461 TestOpenResponsesImagePartials — progressive
// frames: `partial_image` events reach the sink DECODED, the `generating` event raises the
// composing widget under the name `image_generation`, and the declaration carries
// `partial_images`.
#[tokio::test]
async fn test_open_responses_image_partials() {
    // partial_image_b64 = base64([1, 2]); result = base64([3, 4, 5]).
    const SSE: &str = r#"data: {"type":"response.image_generation_call.generating"}

data: {"type":"response.image_generation_call.partial_image","partial_image_b64":"AQI="}

data: {"type":"response.output_item.done","item":{"id":"ig_1","type":"image_generation_call","result":"AwQF"}}

"#;
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", SSE).await;
    let mut p = provider(&srv, "gpt-5");
    p.set_image_output(true);

    let cancel = CancellationToken::new();
    let mut sink = ImageSink::default();
    let out = p
        .stream_chat_with_tools(&cancel, &[Message::user("draw")], &[], &mut sink)
        .await
        .expect("streaming round failed");

    let got = body_json(&srv.received_requests().await.unwrap()[0]);
    assert_eq!(got["tools"][0]["partial_images"], json!(1), "{got:#?}");
    assert_eq!(sink.partials, vec![vec![1_u8, 2]], "one decoded frame");
    assert_eq!(
        sink.raised.first().map(String::as_str),
        Some("image_generation"),
        "the widget was not raised: {:?}",
        sink.raised
    );
    assert_eq!(out.images.len(), 1, "the final image is missing");
    assert_eq!(out.images[0].data, vec![3_u8, 4, 5]);
}

/// New (the frame guards, no Go twin): an EMPTY `partial_image_b64`, undecodable base64 and a
/// frame that decodes to zero bytes are all dropped — a preview is never worth an error, and a
/// zero-byte frame would blank the widget body.
#[tokio::test]
async fn image_partial_frames_that_carry_nothing_are_dropped() {
    const SSE: &str = r#"data: {"type":"response.image_generation_call.partial_image"}

data: {"type":"response.image_generation_call.partial_image","partial_image_b64":"!!!!"}

data: {"type":"response.image_generation_call.partial_image","partial_image_b64":""}

data: {"type":"response.completed","response":{"status":"completed"}}

"#;
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", SSE).await;
    let mut p = provider(&srv, "gpt-5");
    p.set_image_output(true);

    let cancel = CancellationToken::new();
    let mut sink = ImageSink::default();
    p.stream_chat_with_tools(&cancel, &[Message::user("draw")], &[], &mut sink)
        .await
        .expect("streaming round failed");
    assert!(sink.partials.is_empty(), "{:?}", sink.partials);
}

// Go: provider/openresponses_wire_test.go:516 TestOpenResponsesComposingObserverLearnsName —
// the composing observer must learn the function's NAME, which in this dialect appears exactly
// once (in `response.output_item.added`) and never on the argument deltas themselves. The
// widget goes up on the announcement, before the first argument byte: a call that has been
// announced is already work in progress.
#[tokio::test]
async fn test_open_responses_composing_observer_learns_name() {
    const SSE: &str = r#"data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"Writing."}

data: {"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"c1","name":"write_file","arguments":""}}

data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}

data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"a.txt\"}"}

data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"c1","name":"write_file","arguments":"{\"path\":\"a.txt\"}"}}

data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}

"#;
    let srv = MockServer::start().await;
    mock_sse(&srv, "POST", "/responses", SSE).await;
    let p = provider(&srv, "m");

    let tools = vec![ToolDef {
        name: "write_file".to_owned(),
        input_schema: Some(object_schema()),
        ..ToolDef::default()
    }];
    let cancel = CancellationToken::new();
    let mut sink = ImageSink::default();
    p.stream_chat_with_tools(&cancel, &[Message::user("q")], &tools, &mut sink)
        .await
        .expect("streaming round failed");

    assert_eq!(
        sink.raised,
        vec![
            "write_file".to_owned(),
            "write_file".to_owned(),
            "write_file".to_owned(),
        ],
        "the announcement notifies, then the two argument deltas — an unnamed delta raises nothing"
    );
}

/// Leg 1 ends on a `tool_search_call`; leg 2 streams the answer.
// Go: provider/defermode_wire_test.go:71
const TOOL_SEARCH_LEGS: [&str; 2] = [
    r#"data: {"type":"response.output_item.done","item":{"id":"rs_1","type":"reasoning","summary":[]}}

data: {"type":"response.output_item.done","item":{"id":"ts_1","type":"tool_search_call","call_id":"tsc_1","status":"completed","execution":"client","arguments":{"query":"pull request"}}}

data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}

"#,
    r#"data: {"type":"response.output_text.delta","delta":"done"}

data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}

"#,
];

/// The deferred tool the searcher mounts.
fn deferred_tool() -> ToolDef {
    ToolDef {
        name: "mcp__gh__pr".to_owned(),
        description: "PRs".to_owned(),
        input_schema: Some(object_schema()),
        deferred: true,
    }
}

/// A searcher recording every query it is asked, answering with `hits`.
fn recording_searcher(hits: Vec<ToolDef>) -> (ToolSearcher, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let searcher: ToolSearcher = Arc::new(move |query: &str| {
        log.lock().unwrap().push(query.to_owned());
        hits.clone()
    });
    (searcher, seen)
}

// Go: provider/defermode_wire_test.go:69
#[tokio::test]
async fn test_open_responses_tool_search_loop() {
    let srv = MockServer::start().await;
    mock_sse_sequence(&srv, &TOOL_SEARCH_LEGS).await;

    let mut p = provider(&srv, "gpt-5.5");
    let mut hit = deferred_tool();
    hit.deferred = false; // the searcher answers with plain specs; defer_loading is added on the wire
    let (searcher, queries) = recording_searcher(vec![hit]);
    p.set_tool_searcher(Some(searcher));

    let (out, _) = round(&p, &[Message::user("go")], &[deferred_tool()]).await;
    assert_eq!(out.content, "done");
    assert!(out.tool_calls.is_empty());
    assert_eq!(queries.lock().unwrap().as_slice(), ["pull request"]);

    let reqs = srv.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2, "legs");

    // Leg 1: the deferred function carries defer_loading behind a client-executed tool_search entry.
    let leg1 = body_json(&reqs[0]);
    assert_eq!(
        leg1["tools"],
        json!([
            {"type":"function","name":"mcp__gh__pr","description":"PRs","parameters":{"type":"object"},"strict":false,"defer_loading":true},
            {"type":"tool_search","execution":"client",
             "description":"Search and load additional deferred tools by capability keywords before first use.",
             "parameters":{"type":"object","properties":{"query":{"type":"string","description":"Capability keywords"}},"required":["query"]}}
        ])
    );

    // Leg 2 input: the user turn, then EVERY raw item leg 1 produced replayed verbatim (the reasoning item MUST
    // accompany its tool_search_call — gpt-5.x rejects the call without it), then our tool_search_output.
    let leg2 = body_json(&reqs[1]);
    assert_eq!(
        leg2["input"],
        json!([
            {"role":"user","content":"go"},
            {"id":"rs_1","type":"reasoning","summary":[]},
            {"id":"ts_1","type":"tool_search_call","call_id":"tsc_1","status":"completed","execution":"client","arguments":{"query":"pull request"}},
            {"type":"tool_search_output","call_id":"tsc_1","status":"completed","execution":"client","tools":[
                {"type":"function","name":"mcp__gh__pr","description":"PRs","parameters":{"type":"object"},"strict":false,"defer_loading":true}]}
        ])
    );
    // The same tools array is re-sent on every leg.
    assert_eq!(leg2["tools"], leg1["tools"]);

    // Search legs are invisible upstream: a round that ends with neither tool calls nor images keeps no raw
    // replay blob at all (openresponses.go:512).
    assert!(out.raw_content.is_none(), "{:?}", out.raw_content);
}

/// A searcher with no hits still answers the call — with an empty ARRAY, never Go's `"tools":null`
/// (POLICY parity item 5 / CONTRACTS §3.8.2).
#[tokio::test]
async fn tool_search_output_with_no_hits_emits_empty_array() {
    let srv = MockServer::start().await;
    mock_sse_sequence(&srv, &TOOL_SEARCH_LEGS).await;

    let mut p = provider(&srv, "gpt-5.5");
    let (searcher, queries) = recording_searcher(Vec::new());
    p.set_tool_searcher(Some(searcher));

    let (out, _) = round(&p, &[Message::user("go")], &[deferred_tool()]).await;
    assert_eq!(out.content, "done");
    assert_eq!(queries.lock().unwrap().len(), 1);

    let reqs = srv.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let input = body_json(&reqs[1]);
    let answer = input["input"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        answer,
        json!({
            "type":"tool_search_output","call_id":"tsc_1","status":"completed","execution":"client","tools":[]
        })
    );
    // Byte-level: an empty array, not a null.
    let body = String::from_utf8(reqs[1].body.clone()).unwrap();
    assert!(body.contains(r#""tools":[]"#), "{body}");
    assert!(!body.contains(r#""tools":null"#), "{body}");
}

// Go: provider/usage_wire_test.go:47 (openresponses case)
#[tokio::test]
async fn test_unary_chat_owns_its_usage_openresponses() {
    const WITH_USAGE: &str = r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hi"}]}],"usage":{"input_tokens":11,"output_tokens":7}}"#;
    const WITHOUT_USAGE: &str = r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hi"}]}]}"#;

    let srv = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |_: &Request| {
            let body = if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                WITH_USAGE
            } else {
                WITHOUT_USAGE
            };
            ResponseTemplate::new(200).set_body_raw(body, "application/json")
        })
        .mount(&srv)
        .await;

    let p = provider(&srv, "m");
    let cancel = CancellationToken::new();

    let first = p.chat(&cancel, &[Message::user("q")]).await.unwrap();
    assert_eq!(first.text, "hi");
    let usage = first.usage.expect("usage");
    assert_eq!((usage.input, usage.output), (11, 7));
    // total is absent on the wire, so it is derived as input+output.
    assert_eq!(usage.total, 18);

    // A second call whose response omits usage must not keep reporting the first call's figures.
    let second = p.chat(&cancel, &[Message::user("q")]).await.unwrap();
    assert_eq!(second.text, "hi");
    assert!(
        second.usage.is_none(),
        "stale usage reported after a usage-less call: {:?}",
        second.usage
    );
}
