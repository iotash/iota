//! PROBE (evaluation `xprov-fable`, 2026-10-09): what each dialect puts on the wire when the history it
//! replays was produced by ANOTHER dialect — the steady state a session would be in after a provider
//! switch, and the state `iota resume -M other:model` already puts a bundle in today.
//!
//! Not product code. Every assertion here is the measured answer to "what is lost, and what is kept, when
//! a foreign `RawContent` rides an assistant message": the foreign payload is ignored, text and tool calls
//! are reconstructed from the neutral fields, and the reasoning TEXT is never sent by any dialect.

use crate::common::{body_json, mock_sse};
use iota::provider::ToolProvider;
use iota::provider::anthropic::AnthropicProvider;
use iota::provider::model::{
    AssistantBody, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef,
};
use iota::provider::openai::OpenAiProvider;
use iota::provider::openresponses::OpenResponsesProvider;
use iota::provider::sink::NullSink;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::MockServer;

const ANTHROPIC_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
const RESPONSES_DONE: &str = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n";
const CHATCOMP_DONE: &str = "data: [DONE]\n\n";

fn raw(s: &str) -> Raw {
    Raw::from_string(s.to_owned()).unwrap()
}

fn obj(v: Value) -> JsonObject {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

fn tool() -> ToolDef {
    ToolDef {
        name: "f".to_owned(),
        description: "does f".to_owned(),
        input_schema: Some(obj(
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )),
        deferred: false,
    }
}

/// One tool-use turn as the ANTHROPIC dialect records it: a signed thinking block in the raw payload of
/// the tool round AND of the final reply, the call in the neutral `tool_calls`, the reasoning text beside it.
fn anthropic_turn(n: u32) -> Vec<Message> {
    let call = ToolCall {
        id: format!("toolu_{n}"),
        name: "f".to_owned(),
        arguments: obj(json!({"q": "x"})),
    };
    vec![
        Message::user(format!("q{n}")),
        Message::assistant_with_calls(
            "",
            vec![call.clone()],
            Some(RawContent::Anthropic(vec![raw(&format!(
                r#"{{"type":"thinking","thinking":"weigh {n}","signature":"SIG_A{n}"}}"#
            ))])),
        )
        .with_reasoning(format!("weigh {n}")),
        Message::tool_result(&call, "result", false),
        Message::assistant_body(
            format!("answer {n}"),
            AssistantBody {
                reasoning: format!("settle {n}"),
                raw_content: Some(RawContent::Anthropic(vec![raw(&format!(
                    r#"{{"type":"thinking","thinking":"settle {n}","signature":"SIG_B{n}"}}"#
                ))])),
                ..AssistantBody::default()
            },
        ),
    ]
}

/// The same turn as the RESPONSES dialect records it: the encrypted reasoning item and the `function_call`
/// item in the tool round's raw payload; the final text reply carries no raw (the dialect records items
/// for tool rounds only).
fn responses_turn(n: u32) -> Vec<Message> {
    let call = ToolCall {
        id: format!("call_{n}"),
        name: "f".to_owned(),
        arguments: obj(json!({"q": "x"})),
    };
    vec![
        Message::user(format!("q{n}")),
        Message::assistant_with_calls(
            "",
            vec![call.clone()],
            Some(RawContent::OpenResponses(vec![
                raw(&format!(
                    r#"{{"id":"rs_{n}","type":"reasoning","summary":[{{"type":"summary_text","text":"think {n}"}}],"encrypted_content":"OPAQUE_{n}"}}"#
                )),
                raw(&format!(
                    r#"{{"id":"fc_{n}","type":"function_call","call_id":"call_{n}","name":"f","arguments":"{{\"q\":\"x\"}}"}}"#
                )),
            ])),
        )
        .with_reasoning(format!("think {n}")),
        Message::tool_result(&call, "result", false),
        Message::assistant(format!("answer {n}")).with_reasoning(format!("settle {n}")),
    ]
}

async fn send_anthropic(messages: &[Message]) -> Value {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/v1/messages", ANTHROPIC_STOP).await;
    let p = AnthropicProvider::new("k", &server.uri(), "claude-x", None, reqwest::Client::new());
    p.stream_chat_with_tools(
        &CancellationToken::new(),
        messages,
        &[tool()],
        &mut NullSink,
    )
    .await
    .expect("anthropic round");
    body_json(&server.received_requests().await.expect("recorded")[0])
}

async fn send_responses(messages: &[Message]) -> Value {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/responses", RESPONSES_DONE).await;
    let p = OpenResponsesProvider::new("k", &server.uri(), "gpt-x", None, reqwest::Client::new());
    p.stream_chat_with_tools(
        &CancellationToken::new(),
        messages,
        &[tool()],
        &mut NullSink,
    )
    .await
    .expect("responses round");
    body_json(&server.received_requests().await.expect("recorded")[0])
}

async fn send_chatcomp(messages: &[Message]) -> Value {
    let server = MockServer::start().await;
    mock_sse(&server, "POST", "/chat/completions", CHATCOMP_DONE).await;
    let p = OpenAiProvider::new("k", &server.uri(), "gpt-x", None, reqwest::Client::new());
    p.stream_chat_with_tools(
        &CancellationToken::new(),
        messages,
        &[tool()],
        &mut NullSink,
    )
    .await
    .expect("chatcomp round");
    body_json(&server.received_requests().await.expect("recorded")[0])
}

/// The shape of a Responses `input` array: each item's `type`, or its `role` for a plain message.
fn input_shape(body: &Value) -> Vec<String> {
    body["input"]
        .as_array()
        .expect("input array")
        .iter()
        .map(|i| {
            i["type"]
                .as_str()
                .or_else(|| i["role"].as_str())
                .expect("type or role")
                .to_owned()
        })
        .collect()
}

/// The block types of every Anthropic message, in order.
fn anthropic_shape(body: &Value) -> Vec<(String, Vec<String>)> {
    body["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .map(|m| {
            let role = m["role"].as_str().expect("role").to_owned();
            let blocks = m["content"]
                .as_array()
                .expect("content array")
                .iter()
                .map(|b| b["type"].as_str().expect("block type").to_owned())
                .collect();
            (role, blocks)
        })
        .collect()
}

fn history(turns: Vec<Vec<Message>>) -> Vec<Message> {
    let mut h = vec![Message::system("sys")];
    for t in turns {
        h.extend(t);
    }
    h.push(Message::user("next"));
    h
}

/// 【实测】An anthropic-born history continued on the responses dialect: the signed thinking blocks are
/// not sent (they are the foreign variant), the tool round is REBUILT from `tool_calls` as a bare
/// `function_call` + `function_call_output`, the text replies ride as messages — and the reasoning TEXT,
/// which the neutral message does carry, is sent by nobody.
#[tokio::test]
async fn anthropic_history_on_the_responses_dialect_loses_only_what_is_not_neutral() {
    let body = send_responses(&history(vec![anthropic_turn(1)])).await;
    let text = body.to_string();
    assert_eq!(
        input_shape(&body),
        [
            "user",
            "function_call",
            "function_call_output",
            "assistant",
            "user"
        ]
    );
    assert_eq!(body["instructions"], json!("sys"));
    assert_eq!(body["input"][1]["call_id"], json!("toolu_1"));
    assert_eq!(body["input"][1]["arguments"], json!(r#"{"q":"x"}"#));
    assert_eq!(body["input"][3]["content"], json!("answer 1"));
    assert!(
        !text.contains("SIG_A1") && !text.contains("SIG_B1"),
        "{text}"
    );
    assert!(!text.contains("thinking"), "{text}");
    assert!(
        !text.contains("weigh 1") && !text.contains("settle 1"),
        "the reasoning text is never sent: {text}"
    );
}

/// 【实测】A responses-born history continued on the anthropic dialect: the encrypted reasoning items are
/// not sent, the tool round is rebuilt as a `tool_use` block from `tool_calls` with the responses
/// `call_id` as its id, the results coalesce into user messages — and no `thinking` block exists anywhere,
/// which Anthropic accepts at a turn boundary ("outside tool use, omit prior turns' thinking").
#[tokio::test]
async fn responses_history_on_the_anthropic_dialect_is_rebuilt_without_thinking() {
    let body = send_anthropic(&history(vec![responses_turn(1)])).await;
    let text = body.to_string();
    assert_eq!(
        anthropic_shape(&body),
        [
            ("user".to_owned(), vec!["text".to_owned()]),
            ("assistant".to_owned(), vec!["tool_use".to_owned()]),
            ("user".to_owned(), vec!["tool_result".to_owned()]),
            ("assistant".to_owned(), vec!["text".to_owned()]),
            ("user".to_owned(), vec!["text".to_owned()]),
        ]
    );
    assert_eq!(body["messages"][1]["content"][0]["id"], json!("call_1"));
    assert_eq!(body["system"][0]["text"], json!("sys"));
    assert!(
        !text.contains("OPAQUE_1") && !text.contains("rs_1"),
        "{text}"
    );
    assert!(
        !text.contains("think 1") && !text.contains("settle 1"),
        "the reasoning text is never sent: {text}"
    );
}

/// 【实测】A MIXED history — turn 1 born on anthropic, turn 2 born on responses, i.e. the session after one
/// switch each way — replayed on all three dialects: each keeps its own turn's payload verbatim and
/// rebuilds the other's from the neutral fields. Nothing is rejected client-side, nothing leaks across.
#[tokio::test]
async fn a_mixed_history_replays_each_dialects_own_payload_and_rebuilds_the_rest() {
    let mixed = history(vec![anthropic_turn(1), responses_turn(2)]);

    // On anthropic: turn 1's thinking blocks lead their assistant messages, signatures intact; turn 2 is
    // bare tool_use + text.
    let body = send_anthropic(&mixed).await;
    let text = body.to_string();
    assert_eq!(
        anthropic_shape(&body),
        [
            ("user".to_owned(), vec!["text".to_owned()]),
            (
                "assistant".to_owned(),
                vec!["thinking".to_owned(), "tool_use".to_owned()]
            ),
            ("user".to_owned(), vec!["tool_result".to_owned()]),
            (
                "assistant".to_owned(),
                vec!["thinking".to_owned(), "text".to_owned()]
            ),
            ("user".to_owned(), vec!["text".to_owned()]),
            ("assistant".to_owned(), vec!["tool_use".to_owned()]),
            ("user".to_owned(), vec!["tool_result".to_owned()]),
            ("assistant".to_owned(), vec!["text".to_owned()]),
            ("user".to_owned(), vec!["text".to_owned()]),
        ]
    );
    assert!(text.contains("SIG_A1") && text.contains("SIG_B1"), "{text}");
    assert!(!text.contains("OPAQUE_2"), "{text}");

    // On responses: turn 1 is rebuilt; turn 2's reasoning item replays verbatim (encrypted content kept,
    // the function_call's `id` stripped, its `call_id` kept).
    let body = send_responses(&mixed).await;
    let text = body.to_string();
    assert_eq!(
        input_shape(&body),
        [
            "user",
            "function_call",
            "function_call_output",
            "assistant",
            "user",
            "reasoning",
            "function_call",
            "function_call_output",
            "assistant",
            "user"
        ]
    );
    assert!(
        !text.contains("SIG_A1") && !text.contains("SIG_B1"),
        "{text}"
    );
    assert_eq!(body["input"][5]["encrypted_content"], json!("OPAQUE_2"));
    assert_eq!(body["input"][6]["call_id"], json!("call_2"));
    assert_eq!(body["input"][6].get("id"), None);

    // On chat-completions (a third dialect neither turn was born on): everything rebuilt, nothing foreign.
    let body = send_chatcomp(&mixed).await;
    let text = body.to_string();
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["role"].as_str().expect("role"))
        .collect();
    assert_eq!(
        roles,
        [
            "system",
            "user",
            "assistant",
            "tool",
            "assistant",
            "user",
            "assistant",
            "tool",
            "assistant",
            "user"
        ]
    );
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], json!("toolu_1"));
    assert_eq!(body["messages"][6]["tool_calls"][0]["id"], json!("call_2"));
    assert!(
        !text.contains("SIG_") && !text.contains("OPAQUE_") && !text.contains("reasoning"),
        "{text}"
    );
}
