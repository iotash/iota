//! OpenAI-shaped images dialect (internal/llm/images.go): generations, the two `/images/edits`
//! forms (multipart and JSON), `/models`, and the shared response reader that takes either an SSE
//! stream or a plain JSON body depending on what the server actually answered.

use crate::llm::json::Raw;
use base64::Engine as _;
use bytes::Bytes;
use reqwest::{Method, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::client::Payload;
use super::multipart::MultipartForm;
use super::{client::Client, error::LlmError, google::b64_bytes, sse::Sse};

/// The generations endpoint path.
pub(crate) const PATH_GENERATIONS: &str = "/images/generations";
/// The edits endpoint path (both the multipart and the JSON form).
pub(crate) const PATH_EDITS: &str = "/images/edits";
/// The models endpoint path.
pub(crate) const PATH_MODELS: &str = "/models";

/// The progressive-frame observer of one call; `None` = a unary turn (`-m`, a quiet round).
///
/// Its PRESENCE is what asks the backend to stream — the provider sets `stream`/`partial_images`
/// from it (provider/images.go:104-109), so an unwatched turn posts the plain body.
pub(crate) type OnPartial<'a> = Option<&'a mut (dyn FnMut(&[u8]) + Send)>;

/// `POST /images/generations` body. Headless never sets `stream`.
///
/// `response_format` is deliberately absent: gpt-image models reject the parameter (always b64), relays default
/// to b64, and DALL·E's default url form is handled by the caller fetching the link (images.go:33-35).
#[derive(Serialize, Default)]
pub(crate) struct ImagesRequest {
    /// Model id.
    pub(crate) model: String,
    /// The prompt.
    pub(crate) prompt: String,
    /// Image count; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) n: Option<u32>,
    /// Image size; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<String>,
    /// Streaming flag; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stream: bool,
    /// Partial image frames; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) partial_images: Option<u32>,
}

/// One generated image.
#[derive(Deserialize, Default)]
pub(crate) struct ImageDatum {
    /// Inline bytes (base64 on the wire).
    #[serde(default, with = "b64_bytes")]
    pub(crate) b64_json: Vec<u8>,
    /// URL fallback.
    #[serde(default)]
    pub(crate) url: String,
    /// Declared media type.
    #[serde(default)]
    pub(crate) media_type: String,
    /// Declared MIME type (alternate key).
    #[serde(default)]
    pub(crate) mime_type: String,
}

impl ImageDatum {
    /// `media_type` if non-empty else `mime_type`.
    pub(crate) fn declared_type(&self) -> &str {
        if self.media_type.is_empty() {
            &self.mime_type
        } else {
            &self.media_type
        }
    }
}

/// Generations response body.
#[derive(Deserialize, Default)]
pub(crate) struct ImagesResponse {
    /// The images.
    #[serde(default)]
    pub(crate) data: Vec<ImageDatum>,
}

/// One listed model.
#[derive(Deserialize, Default)]
pub(crate) struct ImagesModel {
    /// Model id.
    #[serde(default)]
    pub(crate) id: String,
    /// Output modalities.
    #[serde(default)]
    pub(crate) output_modalities: Vec<String>,
}

/// The `GET /models` page.
#[derive(Deserialize, Default)]
struct ModelsPage {
    #[serde(default)]
    data: Vec<ImagesModel>,
}

/// One SSE frame of a streamed generation (defensive path only).
///
/// Event names are matched by SUFFIX: the Images API says `image_generation.partial_image` while the Responses
/// API wraps the same payload as `response.image_generation_call.partial_image` (images.go:74-77).
#[derive(Deserialize, Default)]
pub(crate) struct ImagesStreamEvent {
    /// Event type (`….partial_image` / `….completed`).
    #[serde(default)]
    pub(crate) r#type: String,
    /// Inline bytes (base64 on the wire).
    #[serde(default, with = "b64_bytes")]
    pub(crate) b64_json: Vec<u8>,
    /// Output format (`png`, `jpeg`, …).
    #[serde(default)]
    pub(crate) output_format: String,
    /// Declared media type.
    #[serde(default)]
    pub(crate) media_type: String,
    /// In-band error; `null` is ABSENT.
    #[serde(default)]
    pub(crate) error: Option<Raw>,
}

impl ImagesStreamEvent {
    /// `media_type` wins; else png|jpeg|webp|gif → "image/<fmt>", jpg → "image/jpeg", else "".
    pub(crate) fn media_type(&self) -> String {
        if !self.media_type.is_empty() {
            return self.media_type.clone();
        }
        match self.output_format.as_str() {
            "png" | "jpeg" | "webp" | "gif" => format!("image/{}", self.output_format),
            "jpg" => "image/jpeg".to_owned(),
            _ => String::new(),
        }
    }
}

/// One uploaded reference image of an edit call (images.go:153).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageFile {
    /// The file name the part declares.
    pub(crate) name: String,
    /// The part's `Content-Type` (empty is legal and still emits the header).
    pub(crate) mime: String,
    /// The raw bytes.
    pub(crate) data: Vec<u8>,
}

/// `POST /images/edits` in either form (images.go:159).
#[derive(Clone, Debug, Default)]
pub(crate) struct ImagesEditRequest {
    /// Model id.
    pub(crate) model: String,
    /// The edit instruction.
    pub(crate) prompt: String,
    /// Image count; 0 omits the field.
    pub(crate) n: Option<u32>,
    /// Image size; `""` omits the field.
    pub(crate) size: Option<String>,
    /// Ask for progressive frames.
    pub(crate) stream: bool,
    /// How many partial frames before the finished picture.
    pub(crate) partial_images: Option<u32>,
    /// The reference images.
    pub(crate) images: Vec<ImageFile>,
}

/// One reference image of a JSON edit request: the xAI shape, a typed object carrying a data URI
/// (images.go:238-241).
#[derive(Serialize)]
struct ImagesEditJsonImage {
    /// Always `"image_url"`.
    r#type: &'static str,
    /// `data:<mime>;base64,<std padded>`.
    url: String,
}

/// The JSON edit body (images.go:246-256).
#[derive(Serialize)]
struct ImagesEditJsonRequest {
    /// Model id.
    model: String,
    /// The edit instruction.
    prompt: String,
    /// Streaming flag; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
    /// Partial image frames; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_images: Option<u32>,
    /// ONE object for a single reference, an ARRAY for several — the documented shape is the
    /// object; the array is the natural generalization.
    image: serde_json::Value,
    /// Image size; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

/// The multipart form of an edit request, with an explicit boundary (unit-testable against the
/// bytes Go's `mime/multipart.Writer` produces).
///
/// TEXT FIELDS COME FIRST (images.go:180-186): relay gateways sniff `model` in the form's leading
/// bytes to route the request, and with a multi-MB image part first the field falls outside the
/// sniff window — zenmux then answers 404 "Requested model is not valid" (reproduced live).
fn build_edit_form(req: &ImagesEditRequest, form: MultipartForm) -> (String, Bytes) {
    let mut form = form;
    form.field("model", &req.model);
    form.field("prompt", &req.prompt);
    if let Some(n) = req.n {
        form.field("n", &n.to_string());
    }
    if let Some(size) = &req.size {
        form.field("size", size);
    }
    if req.stream {
        form.field("stream", "true");
        form.field(
            "partial_images",
            &req.partial_images.unwrap_or(0).to_string(),
        );
    }
    // A single reference uploads as `image`, several as `image[]` — what the official SDKs put on
    // the wire for the two cases (images.go:195-198).
    let field = if req.images.len() > 1 {
        "image[]"
    } else {
        "image"
    };
    for f in &req.images {
        form.file(field, &f.name, &f.mime, &f.data);
    }
    form.finish()
}

/// The images endpoint.
pub(crate) struct Images {
    /// The wire client.
    pub(crate) client: Client,
}

impl Images {
    /// POST /images/generations (JSON) then `consume_images` by response Content-Type.
    pub(crate) async fn generate(
        &self,
        cancel: &CancellationToken,
        req: &ImagesRequest,
        on_partial: OnPartial<'_>,
    ) -> Result<ImagesResponse, LlmError> {
        let payload = serde_json::to_vec(req)
            .map(Bytes::from)
            .map_err(LlmError::Encode)?;
        let resp = self
            .client
            .send(cancel, Method::POST, PATH_GENERATIONS, Some(payload))
            .await?;
        consume_images(resp, cancel, on_partial).await
    }

    /// POST /images/edits as `multipart/form-data` (images.go:180).
    ///
    /// The form is encoded to `Bytes` by our own writer rather than reqwest's streaming
    /// `multipart::Form`, so the `/debug` recorder can capture the body and the retry loop can
    /// clone it per attempt (T-41).
    pub(crate) async fn edit(
        &self,
        cancel: &CancellationToken,
        req: &ImagesEditRequest,
        on_partial: OnPartial<'_>,
    ) -> Result<ImagesResponse, LlmError> {
        let (content_type, body) = build_edit_form(req, MultipartForm::new());
        let resp = self
            .client
            .send_payload(
                cancel,
                Method::POST,
                PATH_EDITS,
                Some(Payload::Multipart { content_type, body }),
            )
            .await?;
        consume_images(resp, cancel, on_partial).await
    }

    /// POST /images/edits with a JSON body (images.go:241). Some backends (xAI) implement ONLY
    /// this form — their docs say the official SDK's `images.edit()` cannot be used because it
    /// posts multipart. The response shape is the shared one.
    pub(crate) async fn edit_json(
        &self,
        cancel: &CancellationToken,
        req: &ImagesEditRequest,
        on_partial: OnPartial<'_>,
    ) -> Result<ImagesResponse, LlmError> {
        let images: Vec<ImagesEditJsonImage> = req
            .images
            .iter()
            .map(|f| {
                let mime = if f.mime.is_empty() {
                    "image/png"
                } else {
                    &f.mime
                };
                ImagesEditJsonImage {
                    r#type: "image_url",
                    url: format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&f.data)
                    ),
                }
            })
            .collect();
        let image = if images.len() == 1 {
            serde_json::to_value(&images[0])
        } else {
            serde_json::to_value(&images)
        }
        .map_err(LlmError::Encode)?;
        let body = ImagesEditJsonRequest {
            model: req.model.clone(),
            prompt: req.prompt.clone(),
            stream: req.stream,
            partial_images: req.partial_images,
            image,
            size: req.size.clone(),
        };
        let payload = serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(LlmError::Encode)?;
        let resp = self
            .client
            .send(cancel, Method::POST, PATH_EDITS, Some(payload))
            .await?;
        consume_images(resp, cancel, on_partial).await
    }

    /// GET /models → sorted by id.
    pub(crate) async fn models(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<ImagesModel>, LlmError> {
        let page: ModelsPage = self.client.get_json(cancel, PATH_MODELS).await?;
        let mut models = page.data;
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }
}

/// Content-Type not starting "text/event-stream" → JSON decode (`MalformedImages`); else SSE: unparseable
/// payloads skipped; non-null `error` → `InBand`; type suffix `.partial_image` with bytes → `on_partial`;
/// `.completed` with b64 → final (last wins); EOF without final → `ImageStreamIncomplete`.
///
/// The RESPONSE Content-Type decides, not the request: a relay that ignores `stream:true` answers with the plain
/// JSON body, and asking again "properly" would bill a second generation (images.go:103-107) — so an observer on
/// a unary answer is simply never called.
pub(crate) async fn consume_images(
    resp: reqwest::Response,
    cancel: &CancellationToken,
    mut on_partial: OnPartial<'_>,
) -> Result<ImagesResponse, LlmError> {
    let streaming = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    if !streaming {
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(LlmError::Cancelled),
            r = resp.bytes() => r.map_err(LlmError::Transport)?,
        };
        return serde_json::from_slice(&bytes).map_err(LlmError::MalformedImages);
    }

    let mut sse = Sse::new(resp.bytes_stream(), cancel.clone());
    let mut last: Option<ImageDatum> = None;
    while let Some(evt) = sse.next().await? {
        // A keepalive or a frame shape we do not model.
        let Ok(event) = serde_json::from_slice::<ImagesStreamEvent>(&evt.data) else {
            continue;
        };
        if let Some(err) = &event.error {
            return Err(LlmError::InBand(err.get().to_owned()));
        }
        if event.r#type.ends_with(".partial_image") {
            // A frame reaches the widget only when this turn is watched and carries bytes.
            if let Some(f) = on_partial.as_deref_mut()
                && !event.b64_json.is_empty()
            {
                f(&event.b64_json);
            }
        } else if event.r#type.ends_with(".completed") && !event.b64_json.is_empty() {
            let media_type = event.media_type();
            last = Some(ImageDatum {
                b64_json: event.b64_json,
                url: String::new(),
                media_type,
                mime_type: String::new(),
            });
        }
    }
    // A half-rendered picture is never presented as final.
    last.map_or(Err(LlmError::ImageStreamIncomplete), |d| {
        Ok(ImagesResponse { data: vec![d] })
    })
}

#[cfg(test)]
mod tests {
    use super::{ImageDatum, ImageFile, ImagesEditRequest, ImagesStreamEvent, build_edit_form};
    use crate::llm::multipart::MultipartForm;

    /// The reference form (one image, no size, unstreamed) byte for byte against the bytes a real
    /// `mime/multipart.Writer` produced when driven exactly as images.go:180-208 drives it.
    #[test]
    fn edit_form_matches_the_go_writer_for_one_reference() {
        let req = ImagesEditRequest {
            model: "gpt-image-1".to_owned(),
            prompt: "add a robot".to_owned(),
            n: Some(1),
            images: vec![ImageFile {
                name: "image-1.png".to_owned(),
                mime: "image/png".to_owned(),
                data: vec![7],
            }],
            ..ImagesEditRequest::default()
        };
        let (ct, body) = build_edit_form(&req, MultipartForm::with_boundary("BOUND"));
        assert_eq!(ct, "multipart/form-data; boundary=BOUND");
        assert_eq!(
            String::from_utf8_lossy(&body),
            concat!(
                "--BOUND\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-1",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nadd a robot",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n1",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"image\"; filename=\"image-1.png\"\r\n",
                "Content-Type: image/png\r\n\r\n\u{7}",
                "\r\n--BOUND--\r\n",
            )
        );
    }

    /// Two references become `image[]`, and a streaming request carries `size`, `stream` and
    /// `partial_images` — all still AHEAD of the file parts (the relay sniff window).
    #[test]
    fn edit_form_matches_the_go_writer_for_two_streaming_references() {
        let req = ImagesEditRequest {
            model: "gpt-image-1".to_owned(),
            prompt: "merge them".to_owned(),
            n: Some(1),
            size: Some("1024x1024".to_owned()),
            stream: true,
            partial_images: Some(1),
            images: vec![
                ImageFile {
                    name: "image-1.png".to_owned(),
                    mime: "image/png".to_owned(),
                    data: vec![7],
                },
                ImageFile {
                    name: "b.jpg".to_owned(),
                    mime: "image/jpeg".to_owned(),
                    data: vec![8],
                },
            ],
        };
        let (_, body) = build_edit_form(&req, MultipartForm::with_boundary("BOUND"));
        assert_eq!(
            String::from_utf8_lossy(&body),
            concat!(
                "--BOUND\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-1",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nmerge them",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n1",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"size\"\r\n\r\n1024x1024",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"stream\"\r\n\r\ntrue",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"partial_images\"\r\n\r\n1",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"image-1.png\"\r\n",
                "Content-Type: image/png\r\n\r\n\u{7}",
                "\r\n--BOUND\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"b.jpg\"\r\n",
                "Content-Type: image/jpeg\r\n\r\n\u{8}",
                "\r\n--BOUND--\r\n",
            )
        );
    }

    /// The unstreamed form omits `stream`/`partial_images` entirely, and a zero `n` / empty
    /// `size` omit their fields (the Go `if` guards).
    #[test]
    fn edit_form_omits_the_unset_knobs() {
        let req = ImagesEditRequest {
            model: "m".to_owned(),
            prompt: "p".to_owned(),
            ..ImagesEditRequest::default()
        };
        let (_, body) = build_edit_form(&req, MultipartForm::with_boundary("B"));
        let s = String::from_utf8_lossy(&body).into_owned();
        for absent in [
            "name=\"n\"",
            "name=\"size\"",
            "name=\"stream\"",
            "partial_images",
        ] {
            assert!(!s.contains(absent), "{absent} must be absent: {s}");
        }
    }

    // Go: internal/llm/llm_test.go:276
    #[test]
    fn test_images_stream_event_media_type() {
        // Streaming events declare a bare output_format, relays reuse media_type; both fold to a mime type,
        // and an undeclared event leaves the decision to the caller's byte sniffing.
        let cases = [
            ("", "png", "image/png"),
            ("", "jpeg", "image/jpeg"),
            ("", "jpg", "image/jpeg"),
            ("image/webp", "", "image/webp"),
            ("image/webp", "png", "image/webp"),
            ("", "", ""),
            ("", "tiff", ""),
            ("", "webp", "image/webp"),
            ("", "gif", "image/gif"),
        ];
        for (media_type, output_format, want) in cases {
            let evt = ImagesStreamEvent {
                media_type: media_type.to_owned(),
                output_format: output_format.to_owned(),
                ..ImagesStreamEvent::default()
            };
            assert_eq!(
                evt.media_type(),
                want,
                "media_type={media_type:?} output_format={output_format:?}"
            );
        }
    }

    #[test]
    fn image_datum_declared_type_prefers_media_type() {
        let both = ImageDatum {
            media_type: "image/jpeg".to_owned(),
            mime_type: "image/png".to_owned(),
            ..ImageDatum::default()
        };
        assert_eq!(both.declared_type(), "image/jpeg");
        let only_mime = ImageDatum {
            mime_type: "image/png".to_owned(),
            ..ImageDatum::default()
        };
        assert_eq!(only_mime.declared_type(), "image/png");
        assert_eq!(ImageDatum::default().declared_type(), "");
    }

    #[test]
    fn images_request_omits_the_unset_knobs() {
        let req = super::ImagesRequest {
            model: "gpt-image-1".to_owned(),
            prompt: "a cat".to_owned(),
            n: Some(1),
            ..super::ImagesRequest::default()
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"gpt-image-1","prompt":"a cat","n":1}"#
        );
    }
}
