//! The `imagen` provider (provider/imagen.go) on the Google `:predict` endpoint. The field is `params`, NOT
//! `gen` — `gen` is a reserved keyword in edition 2024.

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{Attachment, Message};
use crate::provider::{
    ChatResult, HttpTransport, ImageGenOptions, ImageGenParams, ImageGenTunable, Provider,
    ProviderKind,
};
use reqwest::header::HeaderName;
use tokio_util::sync::CancellationToken;

use crate::llm::google::{
    GImageBlob, GImageInstance, GImageParams, GModelInfo, GReferenceImage, Google, PredictRequest,
};
use crate::provider::common::{credential_header, make_client};
use crate::provider::google::GEMINI_DEFAULT_BASE_URL;
use crate::provider::image_util::{ext_for_mime, fetch_image, image_mime, last_user_turn};

/// Accepted aspect ratios.
pub const IMAGEN_ASPECT_RATIOS: [&str; 8] =
    ["1:1", "2:3", "3:2", "3:4", "4:3", "9:16", "16:9", "21:9"];
/// Accepted image sizes.
pub const IMAGEN_IMAGE_SIZES: [&str; 3] = ["1K", "2K", "4K"];
/// Reference type of the `referenceImages` entries (DIVERGENCES D-13 exception).
pub(crate) const REFERENCE_TYPE_RAW: &str = "REFERENCE_TYPE_RAW";

/// The express-mode auth header (provider/imagen.go:44).
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Imagen provider (`image_gen_tunable` only; no core, so `as_tunable()` is `None`).
///
/// Every turn is a text→image (or references+text→image) generation: no text output, no tool calling, no token
/// accounting. Requests are stateless-per-message — the prompt is the final user message and the reference
/// images are THAT MESSAGE's image attachments; nothing is mined from earlier history.
pub struct ImagenProvider {
    model: String,
    client: Google,
    params: ImageGenParams,
}

impl ImagenProvider {
    /// base "" → gemini base/v1beta/vertex=false; else vertex=true/"v1"; retries 0.
    ///
    /// No transport-level retries: a `:predict` is a billed, non-idempotent generation whose response arrives
    /// only when the work is done, so a relay 5xx after upstream completion would silently re-bill.
    pub fn new(api_key: &str, base_url: &str, model: &str, http: impl Into<HttpTransport>) -> Self {
        let vertex = !base_url.is_empty();
        let version = if vertex { "v1" } else { "v1beta" };
        let client = make_client(base_url, GEMINI_DEFAULT_BASE_URL, Some(http.into()))
            .with_header(
                HeaderName::from_static(API_KEY_HEADER),
                credential_header(api_key),
            )
            .with_retries(0);
        Self {
            model: model.to_owned(),
            client: Google {
                client,
                vertex,
                version: version.to_owned(),
            },
            params: ImageGenParams::default(),
        }
    }

    /// The predict call derived from the trailing user message alone, reference images numbered 1..n
    /// (provider/imagen.go:128-153).
    fn build_request(&self, messages: &[Message]) -> Result<PredictRequest, ProviderError> {
        let (prompt, refs) = last_user_turn(messages);
        if prompt.trim().is_empty() {
            return Err(ProviderError::permanent_msg("imagen: empty prompt"));
        }
        let mut inst = GImageInstance {
            prompt,
            reference_images: Vec::with_capacity(refs.len()),
        };
        for (i, a) in refs.iter().enumerate() {
            inst.reference_images.push(GReferenceImage {
                reference_type: REFERENCE_TYPE_RAW.to_owned(),
                reference_id: Some(u32::try_from(i + 1).unwrap_or(u32::MAX)),
                reference_image: Some(GImageBlob {
                    bytes_base64_encoded: a.data.clone(),
                    mime_type: a.mime_type.clone(),
                }),
            });
        }
        // sampleCount is pinned to 1: official Imagen defaults to FOUR images per call otherwise — 4x the
        // cost, and every follow-up turn would then drag four canvases along as reference images.
        Ok(PredictRequest {
            instances: vec![inst],
            parameters: Some(GImageParams {
                sample_count: Some(1),
                aspect_ratio: self.params.aspect_ratio.clone(),
                negative_prompt: self.params.negative_prompt.clone(),
                image_size: self.params.image_size.clone(),
            }),
        })
    }
}

/// Keep a model whose METADATA says it generates images: `supportedGenerationMethods` containing `predict`
/// (official form) or `outputModalities` containing `image` (relay form). An entry with no metadata at all is
/// kept — the user judges, never name heuristics (provider/imagen.go:96-111).
fn image_capable(m: &GModelInfo) -> bool {
    if m.methods.is_empty() && m.output.is_empty() {
        return true;
    }
    m.methods.iter().any(|meth| meth == "predict")
        || m.output.iter().any(|out| out.eq_ignore_ascii_case("image"))
}

impl Provider for ImagenProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Imagen
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn list_models<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async move {
            let models = self
                .client
                .models(cancel)
                .await
                .map_err(|e| ProviderError::wire(WireOp::ListModels, e))?;
            Ok(models
                .into_iter()
                .filter(image_capable)
                .map(|m| m.name)
                .collect())
        })
    }

    /// One generation. The returned text is always empty — the images travel in `ChatResult::images`.
    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            let req = self.build_request(messages)?;
            let resp = self
                .client
                .predict(cancel, &self.model, &req)
                .await
                // Image providers return wire errors RAW (CONTRACTS §3.3); only cancellation is reshaped.
                .map_err(|e| ProviderError::wire(WireOp::Raw, e))?;
            let mut images: Vec<Attachment> = Vec::new();
            let mut filtered = String::new();
            for (i, pred) in resp.predictions.iter().enumerate() {
                let mut data = pred.bytes_base64_encoded.clone();
                let mut mime = pred.mime_type.clone();
                if data.is_empty() {
                    if !pred.rai_filtered_reason.is_empty() {
                        filtered.clone_from(&pred.rai_filtered_reason);
                        continue;
                    }
                    if pred.gcs_uri.is_empty() {
                        continue;
                    }
                    // Referenced payload: relays answer with a signed https link that EXPIRES, so it is
                    // fetched now rather than remembered. (Official Vertex would answer gs://, which needs
                    // Google credentials we never hold — fetch_image rejects the scheme with a clear error.)
                    let fetched = fetch_image(cancel, &self.client.client, &pred.gcs_uri)
                        .await
                        .map_err(|e| {
                            ProviderError::other(format!("imagen: fetching result image: {e}"))
                        })?;
                    (data, mime) = fetched;
                }
                let mime = image_mime(&mime, &data);
                images.push(Attachment {
                    filename: format!("image-{}{}", i + 1, ext_for_mime(&mime)),
                    mime_type: mime,
                    data,
                });
            }
            if images.is_empty() {
                // Deterministic outcomes: retrying would bill again and fail again.
                if !filtered.is_empty() {
                    return Err(ProviderError::permanent_msg(format!(
                        "imagen: all candidates were safety-filtered: {filtered}"
                    )));
                }
                return Err(ProviderError::permanent_msg(format!(
                    "imagen: response contained no images ({} prediction(s), none carried inline bytes or a URI)",
                    resp.predictions.len()
                )));
            }
            Ok(ChatResult {
                text: String::new(),
                usage: None,
                images,
            })
        })
    }

    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        Some(self)
    }
}

impl ImageGenTunable for ImagenProvider {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        self.params = p;
    }

    fn image_gen_params(&self) -> &ImageGenParams {
        &self.params
    }

    /// What the Imagen `:predict` dialect understands across backends (seedream's advertised ratios ∪ official
    /// Imagen's); a size tier a given backend lacks is ignored or rejected server-side.
    fn image_gen_options(&self) -> ImageGenOptions {
        ImageGenOptions {
            aspect_ratios: IMAGEN_ASPECT_RATIOS.to_vec(),
            image_sizes: IMAGEN_IMAGE_SIZES.to_vec(),
            negative_prompt: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Google, ImagenProvider};
    use crate::provider::common::make_client;
    use crate::provider::google::GEMINI_DEFAULT_BASE_URL;
    use crate::provider::model::Message;
    use crate::provider::{ImageGenParams, Provider};
    use tokio_util::sync::CancellationToken;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// The official Gemini Developer API form needs `vertex = false` with a TEST base URL, which `new` (whose
    /// empty-base branch is what selects the official form) cannot express — Go's test builds the private
    /// struct literal for the same reason (`provider/imagen_test.go:104`).
    fn official(uri: &str, model: &str) -> ImagenProvider {
        ImagenProvider {
            model: model.to_owned(),
            client: Google {
                client: make_client(
                    uri,
                    GEMINI_DEFAULT_BASE_URL,
                    Some(reqwest::Client::new().into()),
                ),
                vertex: false,
                version: "v1beta".to_owned(),
            },
            params: ImageGenParams::default(),
        }
    }

    // Go: provider/imagen_test.go:96
    #[tokio::test]
    async fn test_imagen_official_path_form() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/imagen-4.0-generate-001:predict"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"predictions":[{"bytesBase64Encoded":"CQ=="}]}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let p = official(&server.uri(), "imagen-4.0-generate-001");
        let out = p
            .chat(&CancellationToken::new(), &[Message::user("a cat")])
            .await
            .unwrap();
        assert_eq!(out.images.len(), 1);
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].url.path(),
            "/v1beta/models/imagen-4.0-generate-001:predict"
        );
    }
}
