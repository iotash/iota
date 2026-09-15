//! Google dialect tests (`internal/llm/llm_test.go` google cases, `provider/google_test.go`,
//! `provider/google_wire_test.go`, `provider/usage_wire_test.go` google case): the model-path table, the Vertex
//! listing fallback, the `contents` builder, the genai `RawContent` blob compatibility, the golden request and
//! the streaming transcript (thought parts, synthesised call ids, image outputs, usage).

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::common::body_json;
use iota::llm::Client;
use iota::llm::google::{GContent, GPart, Google};
use iota::provider::google::{GoogleProvider, sanitize_content};
use iota::provider::model::{
    AssistantBody, Attachment, Body, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef,
};
use iota::provider::usage::Usage;
use iota::provider::{Effort, Provider, ProviderKind};
use iota::testing::{RecordingSink, SinkEvent};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// The exact JSON `google.golang.org/genai@v1.63.0` produced for a persisted `RawContent` blob (captured via
/// `json.Marshal` on a `*genai.Content`) — `provider/google_wire_test.go:19`.
const GENAI_CONTENT_FIXTURE: &str = r#"{"parts":[{"text":"thinking...","thought":true,"thoughtSignature":"AQID"},{"text":"answer"},{"functionCall":{"id":"c1","args":{"q":1},"name":"f"}},{"inlineData":{"data":"CQ==","mimeType":"image/png"}},{"functionResponse":{"name":"f","response":{"output":"ok"}}}],"role":"model"}"#;

/// A clientless endpoint: `model_path` never touches the client (llm_test.go:190-191).
fn endpoint(vertex: bool, version: &str) -> Google {
    Google {
        client: Client::new("http://127.0.0.1:1", reqwest::Client::new()),
        vertex,
        version: version.to_owned(),
    }
}

/// A provider over `server` (or, with an empty `uri`, over a URL nothing ever calls).
fn gemini(uri: &str, model: &str, temperature: Option<f64>) -> GoogleProvider {
    GoogleProvider::gemini("k", uri, model, temperature, reqwest::Client::new())
}

/// The Vertex twin of [`gemini`] (`tool_call_ids` off).
fn vertex(uri: &str, model: &str) -> GoogleProvider {
    GoogleProvider::vertex_ai("k", uri, model, None, reqwest::Client::new())
}

/// One SSE mock answering every POST with `transcript`.
async fn mock_stream(server: &MockServer, transcript: &'static str) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(transcript, "text/event-stream"))
        .mount(server)
        .await;
}

/// `{name, args}` as a `ToolDef` input schema / call arguments.
fn obj(v: Value) -> JsonObject {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// One streaming round with no tools advertised.
async fn round(
    p: &GoogleProvider,
    messages: &[Message],
    tools: &[ToolDef],
    sink: &mut RecordingSink,
) -> iota::provider::RoundResult {
    p.as_tool_provider()
        .unwrap()
        .stream_chat_with_tools(&CancellationToken::new(), messages, tools, sink)
        .await
        .unwrap()
}
#[test]
fn the_google_model_path_is_built_per_dialect() {
    let vertex = endpoint(true, "v1");
    let gemini = endpoint(false, "v1beta");
    let cases: &[(&Google, &str, &str)] = &[
        (
            &vertex,
            "gemini-2.5-pro",
            "/v1/publishers/google/models/gemini-2.5-pro",
        ),
        (
            &vertex,
            "bytedance/doubao-seedream-5.0-pro",
            "/v1/publishers/bytedance/models/doubao-seedream-5.0-pro",
        ),
        (
            &vertex,
            "publishers/google/models/x",
            "/v1/publishers/google/models/x",
        ),
        (&vertex, "models/x", "/v1/models/x"),
        (&vertex, "projects/p/x", "/v1/projects/p/x"),
        (&gemini, "gemini-2.5-pro", "/v1beta/models/gemini-2.5-pro"),
        (&gemini, "vendor/model", "/v1beta/models/vendor/model"),
        (&gemini, "models/x", "/v1beta/models/x"),
        // Gemini pass-through prefixes and the 3-segment vendor split (google.go:161-169).
        (&gemini, "tunedModels/x", "/v1beta/tunedModels/x"),
        (&vertex, "a/b/c", "/v1/publishers/a/models/b/c"),
    ];
    for &(g, model, want) in cases {
        assert_eq!(g.model_path(model).unwrap(), want, "model_path({model:?})");
    }
    // Path metacharacters are rejected before any path is built.
    for bad in ["bad/../escape", "bad?x=1", "a&b", ".."] {
        let err = endpoint(true, "v1")
            .model_path(bad)
            .expect_err("accepted a path metacharacter");
        assert_eq!(err.to_string(), format!("llm: invalid model name {bad:?}"));
    }
}
#[tokio::test]
async fn vertex_lists_a_built_in_model_set_when_the_endpoint_cannot() {
    // The official publisher path 404s, or redirects to an HTML landing page (a decode error after the
    // redirect is followed); either way the Gemini-shaped listing answers.
    for official in [
        ResponseTemplate::new(404),
        ResponseTemplate::new(200).set_body_raw("<!DOCTYPE html><html>landing</html>", "text/html"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publishers/google/models"))
            .respond_with(official)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"models":[{"name":"google/gemini-3-pro"},{"name":"bytedance/doubao"}]}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let g = Google {
            client: Client::new(&server.uri(), reqwest::Client::new()),
            vertex: true,
            version: "v1".to_owned(),
        };
        let names = g.models(&CancellationToken::new()).await.unwrap();
        assert_eq!(names.len(), 2, "fallback failed: {names:?}");
        assert_eq!(names[0].name, "bytedance/doubao", "sorted by name");
        assert_eq!(names[1].name, "google/gemini-3-pro");
    }

    // The official shape, when present, wins (no fallback issued).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publishers/google/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"publisherModels":[{"name":"publishers/google/models/gemini-3-pro","supportedGenerationMethods":["generateContent"],"outputModalities":["text"]}]}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let g = Google {
        client: Client::new(&server.uri(), reqwest::Client::new()),
        vertex: true,
        version: "v1".to_owned(),
    };
    let names = g.models(&CancellationToken::new()).await.unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].name, "publishers/google/models/gemini-3-pro");
    assert_eq!(names[0].methods, ["generateContent"]);
    assert_eq!(names[0].output, ["text"]);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "no fallback request may be issued when the official path answers"
    );
}
#[test]
fn build_contents_sanitizes_a_raw_payload_before_replay() {
    // A streamed response can trail a zero-value part; replaying it makes Vertex AI reject the request with
    // 400 "required oneof field 'data' must have one initialized field".
    let raw_json = r#"{"parts":[{"thoughtSignature":"c2ln","functionCall":{"args":{"url":"https://example.com"},"name":"get_news"}},{}],"role":"model"}"#;
    let messages = vec![
        Message::user("hi"),
        Message::assistant_with_calls(
            "",
            vec![],
            Some(RawContent::Google(
                Raw::from_string(raw_json.to_owned()).unwrap(),
            )),
        ),
    ];

    let p = vertex("http://127.0.0.1:1", "test-model");
    let (contents, _) = p.build_contents(&messages);

    assert_eq!(contents.len(), 2);
    let model = &contents[1];
    assert_eq!(model.parts.len(), 1, "the empty part must be dropped");
    assert!(model.parts[0].function_call.is_some());
    assert_eq!(model.parts[0].thought_signature, b"sig");
    // The original raw content must stay intact for session persistence.
    assert_eq!(
        messages[1].raw_content().cloned(),
        Some(RawContent::Google(
            Raw::from_string(raw_json.to_owned()).unwrap()
        ))
    );
}
#[test]
fn a_content_whose_parts_are_all_empty_is_dropped() {
    let all_empty = GContent {
        role: "model".to_owned(),
        parts: vec![GPart::default(), GPart::default()],
    };
    assert!(sanitize_content(&all_empty).is_none());

    // All parts survive → the very same content is borrowed back, never copied.
    let survivors = GContent {
        role: "model".to_owned(),
        parts: vec![
            GPart {
                text: "a".to_owned(),
                ..GPart::default()
            },
            GPart {
                text: "b".to_owned(),
                ..GPart::default()
            },
        ],
    };
    let kept = sanitize_content(&survivors).unwrap();
    assert!(matches!(kept, std::borrow::Cow::Borrowed(_)));
    assert_eq!(kept.parts.len(), 2);
}
#[test]
fn tool_call_ids_travel_only_where_the_backend_accepts_them() {
    // The Gemini Developer API accepts FunctionCall/FunctionResponse IDs; Vertex AI does not.
    let call = ToolCall {
        id: "call_1".to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    };
    let messages = vec![
        Message::user("hi"),
        Message::assistant_with_calls("", vec![call.clone()], None),
        Message::tool_result(&call, "ok", false),
    ];

    for (p, want_id) in [
        (gemini("http://127.0.0.1:1", "m", None), "call_1"),
        (vertex("http://127.0.0.1:1", "m"), ""),
    ] {
        let (contents, _) = p.build_contents(&messages);
        assert_eq!(contents.len(), 3, "{:?}", p.kind());
        let fc = contents[1].parts[0]
            .function_call
            .as_ref()
            .expect("missing FunctionCall part");
        assert_eq!(
            fc.id.as_deref().unwrap_or_default(),
            want_id,
            "{:?} FunctionCall id",
            p.kind()
        );
        assert_eq!(fc.name.as_deref(), Some("f"));
        assert!(fc.args.is_none(), "an empty argument map is omitted");
        let fr = contents[2].parts[0]
            .function_response
            .as_ref()
            .expect("missing FunctionResponse part");
        assert_eq!(
            fr.id.as_deref().unwrap_or_default(),
            want_id,
            "{:?} FunctionResponse id",
            p.kind()
        );
        assert_eq!(fr.name.as_deref(), Some("f"));
        assert_eq!(fr.response.as_ref().unwrap()["output"], "ok");
    }

    // A failed tool result files its text under "error" instead (provider/google.go:194-196).
    let p = gemini("http://127.0.0.1:1", "m", None);
    let (contents, _) = p.build_contents(&[Message::tool_result(&call, "boom", true)]);
    assert_eq!(
        contents[0].parts[0]
            .function_response
            .as_ref()
            .unwrap()
            .response
            .as_ref()
            .unwrap()["error"],
        "boom"
    );
    assert_eq!(contents[0].role, "user");
}
#[test]
fn a_google_raw_payload_blob_stays_compatible() {
    let c: GContent = serde_json::from_str(GENAI_CONTENT_FIXTURE).unwrap();
    assert_eq!(c.parts.len(), 5, "fixture decode lost data: {c:?}");
    assert!(c.parts[0].thought);
    assert_eq!(c.parts[0].thought_signature, b"\x01\x02\x03");
    assert_eq!(
        c.parts[2].function_call.as_ref().unwrap().id.as_deref(),
        Some("c1")
    );
    assert_eq!(
        c.parts[3].inline_data.as_ref().unwrap().mime_type,
        "image/png"
    );
    assert_eq!(c.parts[3].inline_data.as_ref().unwrap().data, b"\x09");
    assert_eq!(c.role, "model");

    // Round-trip: the re-marshalled blob must be semantically identical.
    let out = serde_json::to_string(&c).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        serde_json::from_str::<Value>(GENAI_CONTENT_FIXTURE).unwrap(),
        "round-trip drift:\n got {out}\nwant {GENAI_CONTENT_FIXTURE}"
    );
}

/// `effort: max` reaches the wire as `thinkingLevel: HIGH`: Gemini knows LOW/MEDIUM/HIGH, and the two efforts
/// above `high` clamp to it instead of travelling as `MAX` into a 400 (DIVERGENCES X-33).
#[tokio::test]
async fn an_effort_above_high_is_sent_as_thinking_level_high() {
    let server = MockServer::start().await;
    mock_stream(&server, "data: {\"candidates\":[]}\n\n").await;
    let mut p = gemini(&server.uri(), "gemini-3-pro", None);
    p.as_tunable().unwrap().set_effort(Some(Effort::Max));
    let mut sink = RecordingSink::default();
    round(&p, &[Message::user("hi")], &[], &mut sink).await;

    let reqs = server.received_requests().await.unwrap();
    let gc = &body_json(&reqs[0])["generationConfig"];
    assert_eq!(gc["thinkingConfig"]["thinkingLevel"], "HIGH");
    assert_eq!(gc["thinkingConfig"]["includeThoughts"], json!(true));
}
#[tokio::test]
async fn the_google_request_body_is_byte_exact() {
    let server = MockServer::start().await;
    mock_stream(&server, "data: {\"candidates\":[]}\n\n").await;

    let mut p = gemini(&server.uri(), "gemini-2.5-pro", Some(0.5));
    p.as_tunable().unwrap().set_effort(Some(Effort::High));
    p.as_top_p_tunable().unwrap().set_top_p(Some(0.9));
    let messages = vec![
        Message::system("sys"),
        // A system-tools mount must never become a systemInstruction (google.go:145).
        Message::system_tools(vec![ToolDef {
            name: "late".to_owned(),
            ..ToolDef::default()
        }]),
        Message {
            content: "look".to_owned(),
            attachments: vec![Attachment {
                filename: "a.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: vec![9],
            }],
            ..Message::default()
        },
    ];
    let tools = vec![ToolDef {
        name: "f".to_owned(),
        description: "d".to_owned(),
        input_schema: Some(obj(json!({"type": "object"}))),
        deferred: false,
    }];
    let mut sink = RecordingSink::default();
    let out = round(&p, &messages, &tools, &mut sink).await;
    assert_eq!(out.content, "", "an empty candidate list is not an error");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        format!("{}?{}", reqs[0].url.path(), reqs[0].url.query().unwrap()),
        "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(reqs[0].headers["x-goog-api-key"], "k");
    assert_eq!(reqs[0].headers["content-type"], "application/json");

    let got = body_json(&reqs[0]);
    assert_eq!(got["systemInstruction"]["parts"][0]["text"], "sys");
    assert_eq!(got["systemInstruction"]["role"], "user");
    let gc = &got["generationConfig"];
    assert_eq!(gc["temperature"], json!(0.5));
    assert_eq!(gc["topP"], json!(0.9));
    assert_eq!(gc["thinkingConfig"]["includeThoughts"], json!(true));
    assert_eq!(gc["thinkingConfig"]["thinkingLevel"], "HIGH");
    assert!(
        gc.get("responseModalities").is_none(),
        "image output is off by default"
    );
    let parts = &got["contents"][0]["parts"];
    assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
    assert_eq!(parts[0]["inlineData"]["data"], "CQ==");
    assert_eq!(parts[1]["text"], "look", "the text part comes LAST");
    assert_eq!(parts.as_array().unwrap().len(), 2);
    let decl = &got["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["name"], "f");
    assert_eq!(decl["description"], "d");
    assert_eq!(decl["parametersJsonSchema"]["type"], "object");

    // Vertex express: publisher path + v1beta1 on the default endpoint form (a custom baseURL historically
    // flips to v1; pin the default form here).
    let mut pv = vertex(&server.uri(), "gemini-2.5-pro");
    pv.set_version("v1beta1");
    let mount_only = vec![
        Message::system_tools(vec![ToolDef {
            name: "late".to_owned(),
            ..ToolDef::default()
        }]),
        Message::user("q"),
    ];
    round(&pv, &mount_only, &[], &mut sink).await;

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        format!("{}?{}", reqs[1].url.path(), reqs[1].url.query().unwrap()),
        "/v1beta1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    let got = body_json(&reqs[1]);
    assert!(
        got.get("systemInstruction").is_none(),
        "a tools-mount message must not produce a systemInstruction: {got}"
    );
    assert!(got.get("tools").is_none(), "no tools advertised: {got}");
    assert!(
        got.get("generationConfig").is_none(),
        "no knob is set: {got}"
    );
    assert_eq!(got["contents"][0]["parts"][0]["text"], "q");

    // Without the pin, ANY non-empty base URL flips the version segment to v1 (provider/google.go:47-57).
    let pv = vertex(&server.uri(), "gemini-2.5-pro");
    round(&pv, &[Message::user("q")], &[], &mut sink).await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs[2].url.path(),
        "/v1/publishers/google/models/gemini-2.5-pro:streamGenerateContent"
    );
    assert_eq!(reqs[2].headers["x-goog-api-key"], "k");
}

/// The `image: true` opt-in (`ImageTunable`) adds `responseModalities` verbatim; it is off by default because
/// relays generate without it and text models reject the IMAGE modality (provider/google.go:226-232).
#[tokio::test]
async fn image_output_opt_in_declares_response_modalities() {
    let server = MockServer::start().await;
    mock_stream(&server, "data: {\"candidates\":[]}\n\n").await;

    let mut p = gemini(&server.uri(), "m", None);
    assert!(!p.as_image_tunable().unwrap().image_output());
    p.as_image_tunable().unwrap().set_image_output(true);
    assert!(p.as_image_tunable().unwrap().image_output());

    let mut sink = RecordingSink::default();
    round(&p, &[Message::user("draw")], &[], &mut sink).await;

    let reqs = server.received_requests().await.unwrap();
    let got = body_json(&reqs[0]);
    assert_eq!(
        got["generationConfig"]["responseModalities"],
        json!(["TEXT", "IMAGE"])
    );
    assert!(
        got["generationConfig"].get("temperature").is_none(),
        "the modalities alone are enough to emit generationConfig"
    );
}

/// `GoogleStream::next` maps the shared SSE outcomes (google.go:360-376) and treats `"error": null` as ABSENT
/// (DIVERGENCES F-05, where Go's `json.RawMessage` kept the literal null and reported an in-band error).
#[tokio::test]
async fn google_stream_maps_terminal_errors() {
    let cases: [(&'static str, &str); 3] = [
        (
            "",
            "stream error: stream ended without any SSE events (server did not stream?)",
        ),
        (
            "data: {oops\n\n",
            "stream error: llm: malformed stream chunk: key must be a string at line 1 column 2",
        ),
        (
            "data: {\"error\":{\"code\":429,\"message\":\"boom\"}}\n\n",
            "stream error: received error while streaming: {\"code\":429,\"message\":\"boom\"}",
        ),
    ];
    for (transcript, want) in cases {
        let server = MockServer::start().await;
        mock_stream(&server, transcript).await;
        let p = gemini(&server.uri(), "m", None);
        let mut sink = RecordingSink::default();
        let err = p
            .as_tool_provider()
            .unwrap()
            .stream_chat_with_tools(
                &CancellationToken::new(),
                &[Message::user("q")],
                &[],
                &mut sink,
            )
            .await
            .expect_err("expected a stream failure");
        assert_eq!(err.to_string(), want);
        assert!(
            sink.events.contains(&SinkEvent::ReasoningDone),
            "reasoning closes on every exit path"
        );
    }

    // A literal `"error": null` is not an in-band error.
    let server = MockServer::start().await;
    mock_stream(
        &server,
        "data: {\"error\":null,\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}\n\n",
    )
    .await;
    let p = gemini(&server.uri(), "m", None);
    let mut sink = RecordingSink::default();
    let out = round(&p, &[Message::user("q")], &[], &mut sink).await;
    assert_eq!(out.content, "ok");
}
#[tokio::test]
async fn a_google_stream_assembles_text_thoughts_and_function_calls() {
    const TRANSCRIPT: &str = concat!(
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"hm","thought":true,"thoughtSignature":"AQID"}]}}]}"#,
        "\n\n",
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Answer."},{}]}}]}"#,
        "\n\n",
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"q":"x"}}}]}}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_stream(&server, TRANSCRIPT).await;

    let p = gemini(&server.uri(), "m", None);
    let mut sink = RecordingSink::default();
    let tools = vec![ToolDef {
        name: "lookup".to_owned(),
        ..ToolDef::default()
    }];
    let out = round(&p, &[Message::user("q")], &tools, &mut sink).await;

    assert_eq!(out.reasoning, "hm");
    assert_eq!(sink.reasoning(), "hm");
    assert!(sink.events.contains(&SinkEvent::ReasoningDone));
    assert!(
        sink.closed_before_content(),
        "reasoning must close before the first content write: {:?}",
        sink.events
    );
    assert_eq!(out.content, "Answer.");
    assert_eq!(sink.content(), "Answer.");

    assert_eq!(out.tool_calls.len(), 1, "calls = {:?}", out.tool_calls);
    assert_eq!(out.tool_calls[0].name, "lookup");
    assert_eq!(
        out.tool_calls[0].id, "call_lookup_0",
        "an id-less functionCall gets a synthesised one"
    );
    assert_eq!(out.tool_calls[0].arguments["q"], "x");

    assert_eq!(
        out.usage,
        Some(Usage {
            input: 7,
            output: 3,
            total: 10,
            ..Usage::default()
        })
    );

    let Some(RawContent::Google(raw)) = out.raw_content else {
        panic!("expected a Google raw content: {:?}", out.raw_content);
    };
    let content: GContent = serde_json::from_str(raw.get()).unwrap();
    assert_eq!(content.role, "model");
    assert_eq!(
        content.parts.len(),
        3,
        "thought+sig, text, functionCall — the empty part is dropped: {content:?}"
    );
    assert_eq!(
        content.parts[0].thought_signature, b"\x01\x02\x03",
        "thought signature lost from raw content"
    );
    assert!(content.parts[0].thought);
    assert_eq!(content.parts[1].text, "Answer.");
    assert!(content.parts[2].function_call.is_some());
    assert!(out.images.is_empty());
}
#[tokio::test]
async fn image_output_opts_into_response_modalities_and_decodes_inline_data() {
    const TRANSCRIPT: &str = concat!(
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Here you go."},{"inlineData":{"mimeType":"image/png","data":"iVBO"}}]}}]}"#,
        "\n\n",
    );
    let server = MockServer::start().await;
    mock_stream(&server, TRANSCRIPT).await;

    let p = gemini(&server.uri(), "m", None);
    let mut sink = RecordingSink::default();
    let out = round(&p, &[Message::user("draw a cat")], &[], &mut sink).await;
    assert!(out.tool_calls.is_empty());
    assert_eq!(out.content, "Here you go.");
    assert_eq!(out.images.len(), 1, "images = {:?}", out.images);
    assert_eq!(out.images[0].mime_type, "image/png");
    assert_eq!(out.images[0].data, b"\x89PN");
    assert!(
        out.raw_content.is_none(),
        "no tool calls → no raw replay content"
    );

    // Round trip: the assistant message with the attachment replays as a model inlineData part plus its text.
    let messages = vec![
        Message::user("draw a cat"),
        Message {
            content: "Here you go.".to_owned(),
            attachments: vec![Attachment {
                filename: String::new(),
                mime_type: "image/png".to_owned(),
                data: vec![1, 2],
            }],
            body: Body::Assistant(AssistantBody::default()),
        },
        Message::user("make it blue"),
    ];
    let out = round(&p, &messages, &[], &mut sink).await;

    let reqs = server.received_requests().await.unwrap();
    let got = body_json(&reqs[1]);
    let contents = got["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3);
    let parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(parts.len(), 2, "model parts = {parts:?}");
    assert_eq!(parts[0]["text"], "Here you go.");
    assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
    assert_eq!(parts[1]["inlineData"]["data"], "AQI=");

    // Per-stream reset: the second stream reports exactly ITS image, not an accumulation.
    assert_eq!(
        out.images.len(),
        1,
        "images must reset per stream: {:?}",
        out.images
    );
}
#[tokio::test]
async fn a_google_unary_call_reports_only_its_own_usage() {
    const WITH_USAGE: &str = r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":7}}"#;
    const WITHOUT: &str = r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#;

    let server = MockServer::start().await;
    let seen = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&seen);
    Mock::given(method("POST"))
        .respond_with(move |_: &Request| {
            let body = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                WITH_USAGE
            } else {
                WITHOUT
            };
            ResponseTemplate::new(200).set_body_raw(body, "application/json")
        })
        .mount(&server)
        .await;

    let p = gemini(&server.uri(), "m", None);
    let cancel = CancellationToken::new();
    let out = p.chat(&cancel, &[Message::user("q")]).await.unwrap();
    assert_eq!(out.text, "hi");
    assert_eq!(
        out.usage,
        Some(Usage {
            input: 11,
            output: 7,
            total: 18,
            ..Usage::default()
        }),
        "total falls back to input+output"
    );

    // A second call whose response omits usage must not keep reporting the first call's figures.
    let out = p.chat(&cancel, &[Message::user("q")]).await.unwrap();
    assert_eq!(out.usage, None, "stale usage after a usage-less call");
    assert_eq!(seen.load(Ordering::SeqCst), 2);

    // The unary path is the `:generateContent` endpoint (no `alt=sse`).
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs[0].url.path(), "/v1beta/models/m:generateContent");
    assert_eq!(reqs[0].url.query(), None);
    assert_eq!(p.kind(), ProviderKind::Gemini);
    assert_eq!(p.model(), "m");
}

/// Gemini multiturn requests alternate user/model roles, so consecutive tool results — and a user message that
/// directly follows them — fold into ONE user-role content (provider/google.go:127-133).
#[test]
fn consecutive_user_contents_fold_into_one() {
    let first = ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    };
    let second = ToolCall {
        id: "c2".to_owned(),
        name: "g".to_owned(),
        arguments: JsonObject::new(),
    };
    let p = gemini("http://127.0.0.1:1", "m", None);
    let (contents, system) = p.build_contents(&[
        Message::user("hi"),
        Message::assistant_with_calls("", vec![first.clone(), second.clone()], None),
        Message::tool_result(&first, "one", false),
        Message::tool_result(&second, "two", false),
        Message::user("and now?"),
    ]);

    assert!(system.is_none());
    assert_eq!(contents.len(), 3, "{contents:?}");
    assert_eq!(contents[0].role, "user");
    assert_eq!(contents[1].role, "model");
    assert_eq!(
        contents[1].parts.len(),
        2,
        "one part per call, no text part"
    );
    assert_eq!(contents[2].role, "user");
    assert_eq!(
        contents[2].parts.len(),
        3,
        "two tool results plus the next user turn fold together: {:?}",
        contents[2]
    );
    assert_eq!(
        contents[2].parts[0]
            .function_response
            .as_ref()
            .unwrap()
            .id
            .as_deref(),
        Some("c1")
    );
    assert_eq!(
        contents[2].parts[1]
            .function_response
            .as_ref()
            .unwrap()
            .id
            .as_deref(),
        Some("c2")
    );
    assert_eq!(contents[2].parts[2].text, "and now?");
}

/// The unary path returns the visible half of `split_inline_think` and surfaces generated images too, so `-m`
/// single-shot runs still save what they produced (provider/google.go:249-261).
#[tokio::test]
async fn unary_chat_splits_inline_think_and_surfaces_images() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"candidates":[{"content":{"parts":[{"text":"skipped","thought":true},{"text":"<think>plan</think>Done."},{"inlineData":{"mimeType":"image/png","data":"AQI="}}]}}]}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let p = gemini(&server.uri(), "m", None);
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("q")])
        .await
        .unwrap();
    assert_eq!(
        out.text, "Done.",
        "thought parts and <think> are both stripped"
    );
    assert_eq!(out.usage, None);
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].mime_type, "image/png");
    assert_eq!(out.images[0].data, [1, 2]);
    assert!(out.images[0].filename.is_empty());
}
