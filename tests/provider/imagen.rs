//! Imagen provider tests (`provider/imagen_test.go`): the `:predict` golden request, the safety-filter and
//! no-images `PermanentError`s, the metadata-only model filter, the capability surface and the signed-URL
//! result fetch.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common::body_json;
use iota::provider::error::ProviderError;
use iota::provider::imagen::{IMAGEN_ASPECT_RATIOS, IMAGEN_IMAGE_SIZES, ImagenProvider};
use iota::provider::model::{AssistantBody, Attachment, Body, Message};
use iota::provider::{ImageGenOptions, ImageGenParams, Provider};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A provider over `uri` (non-empty ⇒ the relay/vertex form).
fn imagen(uri: &str, model: &str) -> ImagenProvider {
    ImagenProvider::new("k", uri, model, reqwest::Client::new())
}

/// One 200 JSON response for every POST.
async fn mock_predict(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), "application/json"))
        .mount(server)
        .await;
}

/// A user message carrying `attachments`.
fn user_with(content: &str, attachments: Vec<Attachment>) -> Message {
    Message {
        content: content.to_owned(),
        attachments,
        ..Message::default()
    }
}

/// `Attachment { filename, mime_type, data }`.
fn att(filename: &str, mime_type: &str, data: &[u8]) -> Attachment {
    Attachment {
        filename: filename.to_owned(),
        mime_type: mime_type.to_owned(),
        data: data.to_vec(),
    }
}

/// Whether the failure is the `PermanentError` the chat retry loop must not re-bill.
///
/// Go's `errors.As(err, &*PermanentError)` becomes a variant match: `ProviderError::Permanent` holds the
/// `PermanentError` directly and `#[error(transparent)]` forwards `source()` past it to the inner message.
fn is_permanent(err: &ProviderError) -> bool {
    matches!(err, ProviderError::Permanent(_))
}

/// The chat failure of a provider over `messages`.
async fn chat_err(p: &ImagenProvider, messages: &[Message]) -> ProviderError {
    match p.chat(&CancellationToken::new(), messages).await {
        Err(e) => e,
        Ok(ok) => panic!("expected a failure, got {ok:?}"),
    }
}

// Go: provider/imagen_test.go:22
#[tokio::test]
async fn test_imagen_golden_request() {
    // The :predict wire for the relay (vertex-form) backend: publishers/{vendor}/models path on v1,
    // x-goog-api-key auth, the instances/parameters envelope, and the stateless-per-message reference
    // derivation — ONLY the final user message's image attachments become referenceImages.
    let server = MockServer::start().await;
    mock_predict(
        &server,
        r#"{"predictions":[{"bytesBase64Encoded":"CQ==","mimeType":"image/jpeg"},{"bytesBase64Encoded":"Cg=="}]}"#,
    )
    .await;

    let mut p = imagen(&server.uri(), "bytedance/doubao-seedream-5.0-pro");
    p.as_image_gen_tunable()
        .unwrap()
        .set_image_gen_params(ImageGenParams {
            aspect_ratio: Some("3:2".to_owned()),
            image_size: Some("2K".to_owned()),
            negative_prompt: Some("blurry".to_owned()),
        });

    let messages = vec![
        Message::user("a cat"),
        Message {
            attachments: vec![att("image-1.png", "image/png", &[1])],
            body: Body::Assistant(AssistantBody {
                ..AssistantBody::default()
            }),
            ..Message::default()
        },
        user_with(
            "add a robot",
            vec![
                att("ref.png", "image/png", &[2]),
                att("notes.txt", "text/plain", b"x"), // non-image: dropped
            ],
        ),
    ];
    let out = p.chat(&CancellationToken::new(), &messages).await.unwrap();
    assert_eq!(out.text, "", "image providers never speak");
    assert_eq!(out.usage, None, "imagen reports no tokens");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].url.path(),
        "/v1/publishers/bytedance/models/doubao-seedream-5.0-pro:predict"
    );
    assert_eq!(reqs[0].headers.get("x-goog-api-key").unwrap(), "k");

    let got = body_json(&reqs[0]);
    let inst = &got["instances"][0];
    assert_eq!(inst["prompt"], "add a robot");
    let images = inst["referenceImages"].as_array().unwrap();
    assert_eq!(
        images.len(),
        1,
        "referenceImages must be ONLY this message's image attachment (history is not mined)"
    );
    assert_eq!(images[0]["referenceType"], "REFERENCE_TYPE_RAW");
    assert_eq!(images[0]["referenceId"], 1);
    // The predict envelope's image key is bytesBase64Encoded (NOT inlineData's "data") — the live API
    // silently ignores unknown keys.
    assert_eq!(images[0]["referenceImage"]["bytesBase64Encoded"], "Ag==");
    assert_eq!(images[0]["referenceImage"]["mimeType"], "image/png");

    // sampleCount pinned to 1 (official Imagen would default to 4); the size key is sampleImageSize on the
    // wire (genai converter parity), NOT the SDK-level config name imageSize.
    let params = &got["parameters"];
    assert_eq!(params["sampleCount"], 1);
    assert_eq!(params["aspectRatio"], "3:2");
    assert_eq!(params["sampleImageSize"], "2K");
    assert_eq!(params["negativePrompt"], "blurry");
    assert!(
        params.get("imageSize").is_none(),
        "the SDK-level name must not reach the wire: {params}"
    );

    // Outputs: mime respected, default png, extensions.
    assert_eq!(out.images.len(), 2);
    assert_eq!(out.images[0].mime_type, "image/jpeg");
    assert_eq!(out.images[0].filename, "image-1.jpg");
    assert_eq!(out.images[0].data, vec![9]);
    assert_eq!(out.images[1].mime_type, "image/png");
    assert_eq!(out.images[1].filename, "image-2.png");
    assert_eq!(out.images[1].data, vec![10]);
}

// Go: provider/imagen_test.go:96 — `test_imagen_official_path_form` lives in `src/imagen.rs`: the official
// form needs `vertex = false` with a test base URL, which only the private struct literal can build (Go's
// test is in-package for the same reason).

// Go: provider/imagen_test.go:113
#[tokio::test]
async fn test_imagen_safety_filtered() {
    let server = MockServer::start().await;
    mock_predict(
        &server,
        r#"{"predictions":[{"raiFilteredReason":"unsafe prompt"}]}"#,
    )
    .await;

    let p = imagen(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert_eq!(
        err.to_string(),
        "imagen: all candidates were safety-filtered: unsafe prompt"
    );
    // Deterministic failures are PermanentError: the chat retry loop must not re-bill a call that will fail
    // identically.
    assert!(is_permanent(&err), "{err:?}");

    let err = chat_err(&p, &[Message::assistant("no user turn")]).await;
    assert_eq!(err.to_string(), "imagen: empty prompt");
    assert!(is_permanent(&err), "{err:?}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "an empty prompt must fail BEFORE any HTTP call"
    );
}

// Go: provider/imagen_test.go:138
#[tokio::test]
async fn test_imagen_list_models_filter() {
    // Keep by METADATA only: outputModalities containing image (relay form) or supportedGenerationMethods
    // containing predict (official form); entries with no metadata at all are kept — never name heuristics.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"models":[
                {"name":"chat-model","outputModalities":["text"]},
                {"name":"image-model","outputModalities":["image"]},
                {"name":"imagen-x","supportedGenerationMethods":["predict"]},
                {"name":"gen-model","supportedGenerationMethods":["generateContent"]},
                {"name":"bare-model"}
            ]}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    // Every other path 404s, which is what drives the Vertex → /v1beta/models fallback.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let p = imagen(&server.uri(), "m");
    let names = p.list_models(&CancellationToken::new()).await.unwrap();
    assert_eq!(names, ["bare-model", "image-model", "imagen-x"]);
}

// Go: provider/imagen_test.go:195
#[test]
fn test_imagen_capability_surface() {
    // The type stays deliberately un-Tunable and token-less: the chat layer's capability gates key off these
    // assertions.
    let mut p = ImagenProvider::new("k", "", "m", reqwest::Client::new());
    assert!(p.as_tool_provider().is_none(), "imagen has no tool calling");
    assert!(p.as_tunable().is_none(), "imagen must not be Tunable");
    assert!(p.as_top_p_tunable().is_none());
    assert!(p.as_image_tunable().is_none(), "imagen always generates");
    assert!(
        p.as_image_edit_json_tunable().is_none(),
        "the JSON-edit switch is the images dialect's"
    );
    assert!(p.as_tool_search_host().is_none());

    let tun = p
        .as_image_gen_tunable()
        .expect("imagen must expose its generation params");
    assert_eq!(tun.image_gen_params(), &ImageGenParams::default());
    let opts = tun.image_gen_options();
    assert_eq!(
        opts,
        ImageGenOptions {
            aspect_ratios: IMAGEN_ASPECT_RATIOS.to_vec(),
            image_sizes: IMAGEN_IMAGE_SIZES.to_vec(),
            negative_prompt: true,
        }
    );
    assert!(
        !opts.aspect_ratios.is_empty() && !opts.image_sizes.is_empty(),
        "imagen must offer choice lists for the /model tabs"
    );
}

// Go: provider/imagen_test.go:217
#[tokio::test]
async fn test_imagen_gcs_uri_fetched() {
    // Relay-hosted models answer with a signed URL instead of inline bytes (predictions[].gcsUri). It is
    // fetched immediately — the link expires — with no API key attached (the signature IS the credential),
    // and the response header decides the mime.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob.png"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![7u8, 7, 7], "image/jpeg; charset=binary"),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    mock_predict(
        &server,
        &format!(r#"{{"predictions":[{{"gcsUri":"{uri}/blob.png?Expires=1&Signature=x"}}]}}"#),
    )
    .await;

    let p = imagen(&server.uri(), "klingai/kling-v2");
    let out = p
        .chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].data, vec![7, 7, 7]);
    // Mime params are stripped, so the extension maps still match exactly.
    assert_eq!(out.images[0].mime_type, "image/jpeg");
    assert_eq!(out.images[0].filename, "image-1.jpg");

    let reqs = server.received_requests().await.unwrap();
    let blob = reqs
        .iter()
        .find(|r| r.url.path() == "/blob.png")
        .expect("the signed URL was never fetched");
    assert_eq!(blob.url.query(), Some("Expires=1&Signature=x"));
    assert!(
        blob.headers.get("x-goog-api-key").is_none() && blob.headers.get("authorization").is_none(),
        "the signed-URL fetch must not carry credentials: {:?}",
        blob.headers
    );
}

// Go: provider/imagen_test.go:248
#[tokio::test]
async fn test_imagen_gcs_uri_unfetchable() {
    // A gs:// URI needs Google credentials iota never holds: fail loudly instead of reporting "no images".
    let server = MockServer::start().await;
    mock_predict(
        &server,
        r#"{"predictions":[{"gcsUri":"gs://bucket/out/image-1.png"}]}"#,
    )
    .await;

    let p = imagen(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert_eq!(
        err.to_string(),
        "imagen: fetching result image: unsupported image url \"gs://bucket/out/image-1.png\""
    );
    assert!(
        !is_permanent(&err),
        "a fetch failure is a plain error, not PermanentError"
    );
}

// Go: provider/imagen_test.go:262
#[tokio::test]
async fn test_imagen_no_images_error_is_diagnostic() {
    // An empty prediction set still says how many came back — the diagnosis that exposed the gcsUri gap
    // needed /debug to make.
    let server = MockServer::start().await;
    mock_predict(&server, r#"{"predictions":[{},{}]}"#).await;

    let p = imagen(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    assert_eq!(
        err.to_string(),
        "imagen: response contained no images (2 prediction(s), none carried inline bytes or a URI)"
    );
    assert!(is_permanent(&err), "{err:?}");
}

/// A wire failure reaches the caller RAW (CONTRACTS §3.3: image providers do not wrap), and `Retries = 0`
/// means a billed, non-idempotent `:predict` is attempted exactly once.
#[tokio::test]
async fn predict_failures_are_raw_and_never_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_raw(r#"{"error":{"message":"boom"}}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let p = imagen(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user("x")]).await;
    let text = err.to_string();
    assert!(
        text.starts_with("POST \"")
            && text.ends_with(": 500 Internal Server Error {\"message\":\"boom\"}"),
        "the StatusError must arrive unwrapped: {text}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a billed generation is never retried"
    );
}

/// The empty-prompt guard trims like Go's `strings.TrimSpace` and fires before the request is built.
#[tokio::test]
async fn whitespace_only_prompt_is_an_empty_prompt() {
    let server = MockServer::start().await;
    mock_predict(&server, r#"{"predictions":[]}"#).await;
    let p = imagen(&server.uri(), "m");
    let err = chat_err(&p, &[Message::user(" \t\n ")]).await;
    assert_eq!(err.to_string(), "imagen: empty prompt");
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Without generation params the envelope carries `sampleCount` alone (every other key is `omitempty`), and a
/// turn without attachments carries no `referenceImages`.
#[tokio::test]
async fn bare_request_omits_every_unset_parameter() {
    let server = MockServer::start().await;
    mock_predict(
        &server,
        r#"{"predictions":[{"bytesBase64Encoded":"CQ=="}]}"#,
    )
    .await;

    let p = imagen(&server.uri(), "m");
    p.chat(&CancellationToken::new(), &[Message::user("a cat")])
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let got: Value = body_json(&reqs[0]);
    assert_eq!(
        got,
        serde_json::json!({"instances": [{"prompt": "a cat"}], "parameters": {"sampleCount": 1}})
    );
}
