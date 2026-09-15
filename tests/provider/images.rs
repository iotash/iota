//! Images provider tests (`provider/images_test.go`): the `/images/generations` golden request, the URL-form
//! result fetch, the metadata-only model filter, the capability surface, the mime-resolution table and the
//! defensive SSE paths (a backend that streams unasked, and one that never completes).

use crate::common::body_json;
use iota::provider::error::ProviderError;
use iota::provider::image_util::image_mime;
use iota::provider::images::{IMAGES_IMAGE_SIZES, ImagesProvider};
use iota::provider::model::{Attachment, Message};
use iota::provider::{ImageGenOptions, ImageGenParams, Provider};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A JPEG header (`\xff\xd8\xff\xe0` + padding), the bytes `"/9j/4AAQ"` decodes to.
const JPEG: [u8; 12] = [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0];
/// A PNG header.
const PNG: [u8; 12] = [
    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 0,
];

/// A provider over `uri`.
fn images(uri: &str, model: &str) -> ImagesProvider {
    ImagesProvider::new("k", uri, model, reqwest::Client::new())
}

/// One 200 response for `POST /images/generations` with the given body and Content-Type.
async fn mock_generate(server: &MockServer, body: &str, content_type: &str) {
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), content_type))
        .mount(server)
        .await;
}

/// Whether the failure is the `PermanentError` the chat retry loop must not re-bill (see `tests/imagen.rs`).
fn is_permanent(err: &ProviderError) -> bool {
    matches!(err, ProviderError::Permanent(_))
}

/// The chat failure of a provider over `messages`.
async fn chat_err(p: &ImagesProvider, messages: &[Message]) -> ProviderError {
    match p.chat(&CancellationToken::new(), messages).await {
        Err(e) => e,
        Ok(ok) => panic!("expected a failure, got {ok:?}"),
    }
}
#[tokio::test]
async fn the_images_generate_request_is_byte_exact() {
    // The /images/generations wire: bearer auth, n pinned to 1, size passthrough, b64 into the images.
    let server = MockServer::start().await;
    mock_generate(
        &server,
        r#"{"data":[{"b64_json":"CQ=="}]}"#,
        "application/json",
    )
    .await;

    let mut p = images(&server.uri(), "gpt-image-1");
    p.as_image_gen_tunable()
        .unwrap()
        .set_image_gen_params(ImageGenParams {
            image_size: Some("1024x1024".to_owned()),
            ..ImageGenParams::default()
        });
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.text, "");
    assert_eq!(out.usage, None);

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), "/images/generations");
    assert_eq!(reqs[0].headers.get("authorization").unwrap(), "Bearer k");

    let got = body_json(&reqs[0]);
    assert_eq!(got["model"], "gpt-image-1");
    assert_eq!(got["prompt"], "a cat");
    assert_eq!(got["n"], 1);
    assert_eq!(got["size"], "1024x1024");
    assert!(
        got.get("response_format").is_none(),
        "response_format must be omitted (gpt-image rejects it): {got}"
    );

    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].mime_type, "image/png");
    assert_eq!(out.images[0].filename, "image-1.png");
    assert_eq!(out.images[0].data, vec![9]);
}
#[tokio::test]
async fn a_url_result_is_fetched_when_no_b64_is_returned() {
    // DALL·E's default response form is a short-lived URL — fetched immediately, mime taken from the
    // response header (parameters stripped, so the extension maps still exact-match).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![9u8, 9], "image/jpeg; charset=binary"),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    mock_generate(
        &server,
        &format!(r#"{{"data":[{{"url":"{uri}/blob"}}]}}"#),
        "application/json",
    )
    .await;

    let p = images(&server.uri(), "dall-e-3");
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].mime_type, "image/jpeg");
    assert_eq!(out.images[0].filename, "image-1.jpg");
    assert_eq!(out.images[0].data, vec![9, 9]);

    let reqs = server.received_requests().await.unwrap();
    let blob = reqs
        .iter()
        .find(|r| r.url.path() == "/blob")
        .expect("the result URL was never fetched");
    assert!(
        blob.headers.get("authorization").is_none(),
        "the blob fetch must not carry the API key: {:?}",
        blob.headers
    );
}
#[tokio::test]
async fn images_lists_only_image_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"data":[
                {"id":"chat-model","output_modalities":["text"]},
                {"id":"image-model","output_modalities":["image"]},
                {"id":"bare-model"}
            ]}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let p = images(&server.uri(), "m");
    let names = p.list_models(&CancellationToken::new()).await.unwrap();
    assert_eq!(names, ["bare-model", "image-model"]);
}
#[tokio::test]
async fn images_advertises_exactly_the_image_gen_and_json_edit_capabilities() {
    let mut p = ImagesProvider::new("k", "", "m", reqwest::Client::new());
    assert!(p.as_tool_provider().is_none(), "images has no tool calling");
    assert!(p.as_tunable().is_none(), "images must not be Tunable");
    assert!(p.as_top_p_tunable().is_none());
    assert!(p.as_image_tunable().is_none(), "images always generates");
    assert!(p.as_tool_search_host().is_none());

    let opts = p
        .as_image_gen_tunable()
        .expect("images must expose generation params")
        .image_gen_options();
    assert_eq!(
        opts,
        ImageGenOptions {
            aspect_ratios: Vec::new(),
            image_sizes: IMAGES_IMAGE_SIZES.to_vec(),
            negative_prompt: false,
        },
        "the dialect has sizes only"
    );

    // The JSON-edit switch exists and defaults to off: OpenAI and its mirrors want multipart.
    let edits = p
        .as_image_edit_json_tunable()
        .expect("images must expose the JSON-edit switch");
    assert!(!edits.json_edits(), "json edits must default to off");
    edits.set_json_edits(true);
    assert!(p.as_image_edit_json_tunable().unwrap().json_edits());

    let server = MockServer::start().await;
    mock_generate(&server, r#"{"data":[]}"#, "application/json").await;
    let pe = images(&server.uri(), "m");
    let err = chat_err(&pe, &[Message::user("x")]).await;
    assert_eq!(err.to_string(), "images: response contained no images");
    assert!(is_permanent(&err), "empty data must be a PermanentError");

    let err = chat_err(&pe, &[]).await;
    assert_eq!(err.to_string(), "images: empty prompt");
    assert!(is_permanent(&err), "empty prompt must be a PermanentError");
}
#[test]
fn the_attachment_mime_follows_the_declared_type_then_the_bytes() {
    // A declared media_type wins (parameters stripped), otherwise the bytes are sniffed — OpenAI declares
    // nothing and defaults to png, but relays answer JPEG and output_format can ask for webp, so a hardcoded
    // png would file the wrong extension.
    let cases = [
        ("declared wins", "image/jpeg", &PNG[..], "image/jpeg"),
        (
            "declared params strip",
            "image/jpeg; charset=binary",
            &PNG[..],
            "image/jpeg",
        ),
        ("sniffed when undeclared", "", &JPEG[..], "image/jpeg"),
        ("sniffed png", "", &PNG[..], "image/png"),
        (
            "non-image declaration",
            "application/octet-stream",
            &JPEG[..],
            "image/jpeg",
        ),
        ("unknown bytes", "", &[0u8, 1, 2, 3][..], "image/png"),
    ];
    for (name, declared, data, want) in cases {
        assert_eq!(
            image_mime(declared, data),
            want,
            "{name}: image_mime({declared:?}, …)"
        );
    }
}
#[tokio::test]
async fn a_declared_media_type_is_honoured_over_sniffing() {
    // The relay shape end to end: JPEG bytes announced by media_type must reach the attachment as image/jpeg
    // with a .jpg name (OpenRouter's answer).
    let server = MockServer::start().await;
    mock_generate(
        &server,
        r#"{"data":[{"b64_json":"/9j/4AAQ","media_type":"image/jpeg"}]}"#,
        "application/json",
    )
    .await;

    let p = images(&server.uri(), "google/gemini-3.1-flash-lite-image");
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].mime_type, "image/jpeg");
    assert_eq!(out.images[0].filename, "image-1.jpg");
}
#[tokio::test]
async fn the_unary_images_request_carries_no_stream_key() {
    // An unwatched turn (-m, a quiet round) posts the plain unary request: no `stream`, no `partial_images`.
    // The progressive-frame observer is interactive-only and is not ported (DIVERGENCES D-20).
    let server = MockServer::start().await;
    mock_generate(
        &server,
        r#"{"data":[{"b64_json":"CQ=="}]}"#,
        "application/json",
    )
    .await;

    let p = images(&server.uri(), "gpt-image-2");
    p.chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let got = body_json(&reqs[0]);
    assert!(
        got.get("stream").is_none() && got.get("partial_images").is_none(),
        "an unwatched turn must not ask for streaming: {got}"
    );
    assert_eq!(
        got,
        serde_json::json!({"model": "gpt-image-2", "prompt": "a cat", "n": 1})
    );
}
#[tokio::test]
async fn a_backend_answering_plain_json_to_a_stream_request_still_yields_the_picture() {
    // The RESPONSE Content-Type decides how the body is read, because asking again would bill a second
    // generation. A relay that answers plain JSON must work without a second request.
    let server = MockServer::start().await;
    mock_generate(
        &server,
        r#"{"data":[{"b64_json":"CQ==","media_type":"image/png"}]}"#,
        "application/json",
    )
    .await;

    let p = images(&server.uri(), "relay-model");
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1, "unary fallback lost the image");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a second, billed request must never be issued"
    );
}
#[tokio::test]
async fn a_stream_that_never_completes_is_an_error() {
    // A stream that ends with partials but never completes is an error, not a half-rendered picture
    // presented as final. Headless never asks to stream, so this is the defensive path (DIVERGENCES D-29).
    let server = MockServer::start().await;
    mock_generate(
        &server,
        "data: {\"type\":\"image_generation.partial_image\",\"b64_json\":\"AQ==\"}\n\n",
        "text/event-stream",
    )
    .await;

    let p = images(&server.uri(), "gpt-image-2");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert_eq!(
        err.to_string(),
        "llm: image stream ended without a completed image"
    );
}
#[tokio::test]
async fn every_type_declaration_variant_decodes() {
    // Backends spell the payload's type three ways — media_type (OpenRouter), mime_type (xAI), output_format
    // on streaming events — and some declare nothing. All must land on the same attachment mime.
    for (name, body) in [
        (
            "media_type",
            r#"{"data":[{"b64_json":"/9j/4AAQ","media_type":"image/jpeg"}]}"#,
        ),
        (
            "mime_type",
            r#"{"data":[{"b64_json":"/9j/4AAQ","mime_type":"image/jpeg"}]}"#,
        ),
        ("undeclared", r#"{"data":[{"b64_json":"/9j/4AAQ"}]}"#), // sniffed
    ] {
        let server = MockServer::start().await;
        mock_generate(&server, body, "application/json").await;
        let p = images(&server.uri(), "m");
        let out = p
            .chat(&CancellationToken::new(), &[Message::user("x")])
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(out.images.len(), 1, "{name}");
        assert_eq!(out.images[0].mime_type, "image/jpeg", "{name}");
        assert_eq!(out.images[0].filename, "image-1.jpg", "{name}");
    }
}

/// The positive half of the defensive SSE path (DIVERGENCES D-29): a backend that streams unasked still
/// yields the `.completed` frame as the result, with its declared media type.
#[tokio::test]
async fn stream_completed_frame_is_the_result() {
    let server = MockServer::start().await;
    mock_generate(
        &server,
        concat!(
            "data: {\"type\":\"image_generation.partial_image\",\"b64_json\":\"AQ==\",\"partial_image_index\":0}\n\n",
            "data: {\"type\":\"response.image_generation_call.completed\",\"b64_json\":\"/9j/4AAQ\",\"media_type\":\"image/jpeg\"}\n\n",
        ),
        "text/event-stream",
    )
    .await;

    let p = images(&server.uri(), "gpt-image-2");
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1, "partial frames are not results");
    assert_eq!(out.images[0].mime_type, "image/jpeg");
    assert_eq!(out.images[0].filename, "image-1.jpg");
    assert_eq!(out.images[0].data, [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
}

/// An in-band `error` frame aborts the defensive stream with the raw JSON (`"error": null` stays ABSENT,
/// POLICY F-05).
#[tokio::test]
async fn stream_in_band_error_aborts() {
    let server = MockServer::start().await;
    mock_generate(
        &server,
        concat!(
            "data: {\"type\":\"image_generation.partial_image\",\"error\":null}\n\n",
            "data: {\"error\":{\"message\":\"boom\"}}\n\n",
        ),
        "text/event-stream",
    )
    .await;

    let p = images(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert_eq!(
        err.to_string(),
        r#"received error while streaming: {"message":"boom"}"#
    );
}

/// A non-JSON 2xx body is the images decode error, and `Retries = 0` keeps a billed generation to one
/// attempt.
#[tokio::test]
async fn malformed_body_and_no_retries() {
    let server = MockServer::start().await;
    mock_generate(&server, "not json", "application/json").await;
    let p = images(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert!(
        err.to_string()
            .starts_with("llm: malformed images response: "),
        "{err}"
    );

    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_raw("boom", "text/plain"))
        .mount(&failing)
        .await;
    let p = images(&failing.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert!(
        err.to_string()
            .ends_with(": 500 Internal Server Error boom"),
        "the StatusError must arrive unwrapped: {err}"
    );
    assert_eq!(failing.received_requests().await.unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// /images/edits — multipart and JSON (D-13)
// ---------------------------------------------------------------------------

/// One parsed part of a `multipart/form-data` body.
#[derive(Debug, PartialEq, Eq)]
struct Part {
    /// The `name=` parameter of the disposition.
    field: String,
    /// The `filename=` parameter, `""` for a text field.
    filename: String,
    /// The part's declared `Content-Type` (`None` when the header is absent).
    mime: Option<String>,
    /// The part's raw body.
    data: Vec<u8>,
}

/// Splits a multipart body on `boundary`, honouring the CRLF framing exactly.
fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Part> {
    let sep = format!("\r\n--{boundary}");
    // The first delimiter carries no leading CRLF; prefixing one lets the split be uniform.
    let mut buf = b"\r\n".to_vec();
    buf.extend_from_slice(body);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut out = Vec::new();
    for chunk in text.split(&sep).skip(1) {
        let Some(rest) = chunk.strip_prefix("\r\n") else {
            continue; // the closing "--\r\n"
        };
        let Some((head, data)) = rest.split_once("\r\n\r\n") else {
            continue;
        };
        let mut field = String::new();
        let mut filename = String::new();
        let mut mime = None;
        for line in head.split("\r\n") {
            if let Some(v) = line.strip_prefix("Content-Disposition: ") {
                for p in v.split("; ") {
                    if let Some(n) = p.strip_prefix("name=") {
                        n.trim_matches('"').clone_into(&mut field);
                    } else if let Some(n) = p.strip_prefix("filename=") {
                        n.trim_matches('"').clone_into(&mut filename);
                    }
                }
            } else if let Some(v) = line.strip_prefix("Content-Type:") {
                mime = Some(v.strip_prefix(' ').unwrap_or(v).to_owned());
            }
        }
        out.push(Part {
            field,
            filename,
            mime,
            data: data.as_bytes().to_vec(),
        });
    }
    out
}

/// The `boundary=` parameter of a `multipart/form-data` content type.
fn boundary_of(ct: &str) -> String {
    ct.split("boundary=")
        .nth(1)
        .expect("boundary")
        .trim_matches('"')
        .to_owned()
}

/// One 200 JSON response for `POST /images/edits`.
async fn mock_edits(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/images/edits"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), "application/json"))
        .mount(server)
        .await;
}

fn ref_att(name: &str, mime: &str, data: &[u8]) -> Attachment {
    Attachment {
        filename: name.to_owned(),
        mime_type: mime.to_owned(),
        data: data.to_vec(),
    }
}

fn edit_turn(content: &str, atts: Vec<Attachment>) -> Message {
    Message {
        content: content.to_owned(),
        attachments: atts,
        ..Message::default()
    }
}
#[tokio::test]
async fn the_images_edit_request_is_byte_exact_multipart() {
    // References switch the call to multipart /images/edits: one ref uploads as `image`, several
    // as `image[]`, with per-part content types and the same form fields.
    let server = MockServer::start().await;
    mock_edits(&server, r#"{"data":[{"b64_json":"CQ=="}]}"#).await;

    let p = images(&server.uri(), "gpt-image-1");
    let canvas = ref_att("image-1.png", "image/png", &[7]);
    p.chat(
        &CancellationToken::new(),
        &[edit_turn("add a robot", vec![canvas.clone()])],
    )
    .await
    .unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs[0].url.path(), "/images/edits");
    let ct = reqs[0]
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");

    // Text fields must precede the file parts: relay gateways sniff `model` in the form's
    // leading bytes (a multi-MB image first pushed it out of the window → 404 invalid_model,
    // reproduced live).
    let raw = &reqs[0].body;
    let model_at = raw
        .windows(12)
        .position(|w| w == br#"name="model""#)
        .expect("model field");
    let image_at = raw
        .windows(11)
        .position(|w| w == br#"name="image"#)
        .expect("image part");
    assert!(model_at < image_at, "model@{model_at} image@{image_at}");

    let parts = parse_multipart(raw, &boundary_of(ct));
    let text: Vec<(&str, String)> = parts
        .iter()
        .filter(|p| p.filename.is_empty())
        .map(|p| {
            (
                p.field.as_str(),
                String::from_utf8_lossy(&p.data).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        text,
        vec![
            ("model", "gpt-image-1".to_owned()),
            ("prompt", "add a robot".to_owned()),
            ("n", "1".to_owned()),
        ],
        "size is omitted when unset, and no streaming flags without an observer"
    );
    let files: Vec<&Part> = parts.iter().filter(|p| !p.filename.is_empty()).collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].field, "image", "a single ref uploads as `image`");
    assert_eq!(files[0].filename, "image-1.png");
    assert_eq!(files[0].mime.as_deref(), Some("image/png"));
    assert_eq!(files[0].data, [7]);

    // Two references: the field name becomes image[].
    p.chat(
        &CancellationToken::new(),
        &[edit_turn(
            "merge them",
            vec![canvas, ref_att("b.jpg", "image/jpeg", &[8])],
        )],
    )
    .await
    .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let ct = reqs[1]
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    let parts = parse_multipart(&reqs[1].body, &boundary_of(ct));
    let files: Vec<&Part> = parts.iter().filter(|p| !p.filename.is_empty()).collect();
    assert_eq!(files.len(), 2);
    assert!(
        files.iter().all(|f| f.field == "image[]"),
        "multiple refs must upload as image[]: {files:?}"
    );
    assert_eq!(files[1].mime.as_deref(), Some("image/jpeg"));
    assert_eq!(files[1].data, [8]);
}
#[tokio::test]
async fn json_edits_send_the_json_body_form() {
    // JSON edits: some backends (xAI) accept ONLY a JSON body on /images/edits and reject
    // multipart. The switch routes there; a single reference is the documented object form,
    // several become an array; results parse the same.
    let server = MockServer::start().await;
    mock_edits(&server, r#"{"data":[{"b64_json":"CQ=="}]}"#).await;

    let mut p = images(&server.uri(), "grok-imagine-image-quality");
    p.as_image_edit_json_tunable().unwrap().set_json_edits(true);
    let one = ref_att("a.png", "image/png", &[1]);
    let out = p
        .chat(
            &CancellationToken::new(),
            &[edit_turn("add a hat", vec![one.clone()])],
        )
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1, "response parsing broke");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs[0].url.path(), "/images/edits");
    let ct = reqs[0]
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("application/json"), "content-type = {ct:?}");
    assert_eq!(
        body_json(&reqs[0]),
        serde_json::json!({
            "model": "grok-imagine-image-quality",
            "prompt": "add a hat",
            "image": {"type": "image_url", "url": "data:image/png;base64,AQ=="},
        }),
        "a single reference must be an object"
    );

    // Several references generalize to an array.
    p.chat(
        &CancellationToken::new(),
        &[edit_turn(
            "merge",
            vec![one, ref_att("b.jpg", "image/jpeg", &[2])],
        )],
    )
    .await
    .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let got = body_json(&reqs[1]);
    let arr = got["image"].as_array().expect("array for several refs");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[1]["url"], "data:image/jpeg;base64,Ag==");

    // The switch is off by default: OpenAI and its mirrors want multipart.
    assert!(
        !images(&server.uri(), "gpt-image-1")
            .as_image_edit_json_tunable()
            .unwrap()
            .json_edits(),
        "json edits must default to off"
    );
}

/// A JSON edit with no declared mime falls back to `image/png` in the data URI
/// (llm/images.go:255-258).
#[tokio::test]
async fn json_edit_defaults_the_data_uri_mime() {
    let server = MockServer::start().await;
    mock_edits(&server, r#"{"data":[{"b64_json":"CQ=="}]}"#).await;
    let mut p = images(&server.uri(), "m");
    p.as_image_edit_json_tunable().unwrap().set_json_edits(true);
    p.chat(
        &CancellationToken::new(),
        &[edit_turn("x", vec![ref_att("a.png", "image/png", &[1])])],
    )
    .await
    .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        body_json(&reqs[0])["image"]["url"],
        "data:image/png;base64,AQ=="
    );
}

// ---------------------------------------------------------------------------
// progressive frames (D-20 image half)
// ---------------------------------------------------------------------------

/// Runs one observed turn, returning the frames the observer saw.
async fn observed(p: &ImagesProvider, messages: &[Message]) -> (Vec<Vec<u8>>, Vec<Attachment>) {
    let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&frames);
    let mut on_partial = move |b: &[u8]| sink.lock().unwrap().push(b.to_vec());
    let out = p
        .as_image_partial_provider()
        .expect("the images dialect streams")
        .chat_observed(&CancellationToken::new(), messages, &mut on_partial)
        .await
        .unwrap();
    let seen = frames.lock().unwrap().clone();
    (seen, out.images)
}
#[tokio::test]
async fn progressive_frames_reach_the_observer_before_the_result() {
    // An installed observer asks the backend for progressive frames, partials reach the
    // observer, and the completed event is the result.
    let server = MockServer::start().await;
    mock_generate(
        &server,
        concat!(
            "data: {\"type\":\"image_generation.partial_image\",\"b64_json\":\"AQ==\",\"partial_image_index\":0}\n\n",
            "data: {\"type\":\"image_generation.completed\",\"b64_json\":\"/9j/4AAQ\",\"media_type\":\"image/jpeg\"}\n\n",
        ),
        "text/event-stream",
    )
    .await;

    let p = images(&server.uri(), "gpt-image-2");
    let (frames, images) = observed(&p, &[Message::user("a cat")]).await;

    let reqs = server.received_requests().await.unwrap();
    let got = body_json(&reqs[0]);
    assert_eq!(
        got,
        serde_json::json!({
            "model": "gpt-image-2", "prompt": "a cat", "n": 1,
            "stream": true, "partial_images": 1,
        }),
        "the observer IS the request to stream"
    );
    assert_eq!(frames.len(), 1, "partial frames = {frames:?}");
    assert_eq!(frames[0][0], 1);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].mime_type, "image/jpeg");
}

/// The same flags reach the EDIT endpoints, and the frames still arrive (images.go:104-134).
#[tokio::test]
async fn observed_edit_streams_too() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/images/edits"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"type\":\"image_generation.partial_image\",\"b64_json\":\"AQ==\"}\n\n\
             data: {\"type\":\"image_generation.completed\",\"b64_json\":\"CQ==\"}\n\n"
                    .to_owned(),
                "text/event-stream",
            ),
        )
        .mount(&server)
        .await;

    let p = images(&server.uri(), "gpt-image-2");
    let (frames, images) = observed(
        &p,
        &[edit_turn(
            "add a hat",
            vec![ref_att("a.png", "image/png", &[1])],
        )],
    )
    .await;
    assert_eq!(frames.len(), 1);
    assert_eq!(images.len(), 1);

    let reqs = server.received_requests().await.unwrap();
    let ct = reqs[0]
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    let parts = parse_multipart(&reqs[0].body, &boundary_of(ct));
    let text: Vec<(&str, String)> = parts
        .iter()
        .filter(|p| p.filename.is_empty())
        .map(|p| {
            (
                p.field.as_str(),
                String::from_utf8_lossy(&p.data).into_owned(),
            )
        })
        .collect();
    assert!(
        text.contains(&("stream", "true".to_owned()))
            && text.contains(&("partial_images", "1".to_owned())),
        "{text:?}"
    );
}
#[tokio::test]
async fn a_plain_json_answer_calls_the_observer_never() {
    // A backend that ignores stream:true answers with the plain JSON body — the observer simply
    // never fires; asking again "properly" would bill a second generation.
    let server = MockServer::start().await;
    mock_generate(
        &server,
        r#"{"data":[{"b64_json":"CQ==","media_type":"image/png"}]}"#,
        "application/json",
    )
    .await;

    let p = images(&server.uri(), "relay-model");
    let (frames, images) = observed(&p, &[Message::user("a cat")]).await;
    assert!(
        frames.is_empty(),
        "no frames exist in a unary answer, observer called {} times",
        frames.len()
    );
    assert_eq!(images.len(), 1, "unary fallback lost the image");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a second, billed request must never be issued"
    );
}
