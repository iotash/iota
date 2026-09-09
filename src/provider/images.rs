//! The `images` provider (provider/images.go) on the OpenAI-shaped images dialect. The field is `params`, NOT
//! `gen` — `gen` is a reserved keyword in edition 2024.
//!
//! One turn is a stateless generation from the trailing user message, or an EDIT when that message
//! carries image references — `/images/edits` as multipart, or as JSON when `json_edits` is on
//! (the `/model` switch, the `json_edits` config key and the session meta all reach the same
//! flag). Progressive frames arrive through [`ImagePartialProvider::chat_observed`]: the
//! observer's presence is what puts `stream:true, partial_images:1` on the request, so a headless
//! `-m` run posts the plain unary body.

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{Attachment, Message};
use crate::provider::{
    ChatResult, HttpTransport, ImageEditJsonTunable, ImageGenOptions, ImageGenParams,
    ImageGenTunable, ImagePartialProvider, Provider, ProviderKind,
};
use reqwest::header::AUTHORIZATION;
use tokio_util::sync::CancellationToken;

use crate::llm::images::{ImageFile, Images, ImagesEditRequest, ImagesRequest, OnPartial};
use crate::provider::common::{OPENAI_DEFAULT_BASE_URL, credential_header, make_client};
use crate::provider::image_util::{ext_for_mime, fetch_image, image_mime, last_user_turn};

/// Accepted image sizes.
pub const IMAGES_IMAGE_SIZES: [&str; 6] = [
    "auto",
    "1024x1024",
    "1536x1024",
    "1024x1536",
    "1792x1024",
    "1024x1792",
];

/// Images provider (`image_gen_tunable`, `image_edit_json_tunable`; no core, so `as_tunable()` is `None`).
///
/// Same session shape as `ImagenProvider`: every turn is a stateless generation from the trailing user message
/// and the type is deliberately not tunable, so the chat layer's capability gates apply. The dialect has no
/// aspect-ratio or negative-prompt parameters — dimensions live in the single size knob.
pub struct ImagesProvider {
    model: String,
    client: Images,
    params: ImageGenParams,
    json_edits: bool,
}

impl ImagesProvider {
    /// base "" → `OPENAI_DEFAULT_BASE_URL`; header `Authorization: Bearer <key>`; retries 0.
    ///
    /// Billed, non-idempotent generations get no transport retries (see `ImagenProvider::new`).
    pub fn new(api_key: &str, base_url: &str, model: &str, http: impl Into<HttpTransport>) -> Self {
        let client = make_client(base_url, OPENAI_DEFAULT_BASE_URL, Some(http.into()))
            .with_header(
                AUTHORIZATION,
                credential_header(&format!("Bearer {api_key}")),
            )
            .with_retries(0);
        Self {
            model: model.to_owned(),
            client: Images { client },
            params: ImageGenParams::default(),
            json_edits: false,
        }
    }

    /// One turn (provider/images.go:97-154), with `on_partial` deciding both the streaming flags
    /// and where progressive frames go.
    ///
    /// Wire errors come back RAW — the chat layer prints what the backend said; only the relay
    /// image fetch is wrapped (`images: fetching result: …`).
    async fn run(
        &self,
        cancel: &CancellationToken,
        messages: &[Message],
        on_partial: OnPartial<'_>,
    ) -> Result<ChatResult, ProviderError> {
        let (prompt, refs) = last_user_turn(messages);
        if prompt.trim().is_empty() {
            return Err(ProviderError::permanent_msg("images: empty prompt"));
        }
        // One partial frame: enough to watch the composition settle without redrawing the block
        // on every tick (the openresponses preview budget). The observer IS the request for it.
        let stream = on_partial.is_some();
        let partial_images = stream.then_some(1);
        let resp = if refs.is_empty() {
            let req = ImagesRequest {
                model: self.model.clone(),
                prompt,
                n: Some(1),
                size: self.params.image_size.clone(),
                stream,
                partial_images,
            };
            self.client.generate(cancel, &req, on_partial).await
        } else {
            let req = ImagesEditRequest {
                model: self.model.clone(),
                prompt,
                n: Some(1),
                size: self.params.image_size.clone(),
                stream,
                partial_images,
                images: refs
                    .iter()
                    .map(|a| ImageFile {
                        name: a.filename.clone(),
                        mime: a.mime_type.clone(),
                        data: a.data.clone(),
                    })
                    .collect(),
            };
            // Some backends (xAI) implement ONLY the JSON form; OpenAI and its mirrors want
            // multipart, so the switch defaults off (provider/images.go:130-134).
            if self.json_edits {
                self.client.edit_json(cancel, &req, on_partial).await
            } else {
                self.client.edit(cancel, &req, on_partial).await
            }
        }
        .map_err(|e| ProviderError::wire(WireOp::Raw, e))?;

        let mut images: Vec<Attachment> = Vec::new();
        for (i, d) in resp.data.iter().enumerate() {
            let mut data = d.b64_json.clone();
            let mut mime = image_mime(d.declared_type(), &d.b64_json);
            if data.is_empty() && !d.url.is_empty() {
                // DALL·E's default response form: a short-lived link, fetched immediately
                // (unauthenticated — the URL itself is the token).
                let fetched = fetch_image(cancel, &self.client.client, &d.url)
                    .await
                    .map_err(|e| ProviderError::other(format!("images: fetching result: {e}")))?;
                (data, mime) = fetched;
            }
            if data.is_empty() {
                continue;
            }
            images.push(Attachment {
                filename: format!("image-{}{}", i + 1, ext_for_mime(&mime)),
                mime_type: mime,
                data,
            });
        }
        if images.is_empty() {
            return Err(ProviderError::permanent_msg(
                "images: response contained no images",
            ));
        }
        Ok(ChatResult {
            text: String::new(),
            usage: None,
            images,
        })
    }
}

impl Provider for ImagesProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Images
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Keeps models by METADATA only: relays attach `output_modalities` (keep `image`), entries without
    /// metadata are kept — the user judges, never name heuristics (provider/images.go:71-93).
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
                .filter(|m| {
                    m.output_modalities.is_empty()
                        || m.output_modalities
                            .iter()
                            .any(|out| out.eq_ignore_ascii_case("image"))
                })
                .map(|m| m.id)
                .collect())
        })
    }

    /// One generation (no references) or edit (references present); the returned text is always
    /// empty — pictures ride `ChatResult::images`. Unwatched: no `stream`, no `partial_images`.
    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(self.run(cancel, messages, None))
    }

    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        Some(self)
    }

    fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
        Some(self)
    }

    /// The dialect streams: an interactive turn is run through [`ImagePartialProvider`].
    fn as_image_partial_provider(&self) -> Option<&dyn ImagePartialProvider> {
        Some(self)
    }
}

impl ImagePartialProvider for ImagesProvider {
    fn chat_observed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        on_partial: &'a mut (dyn FnMut(&[u8]) + Send),
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(self.run(cancel, messages, Some(on_partial)))
    }
}

impl ImageGenTunable for ImagesProvider {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        self.params = p;
    }

    fn image_gen_params(&self) -> &ImageGenParams {
        &self.params
    }

    /// The dialect has sizes only: no aspect ratio, no negative prompt (provider/images.go:63-67).
    fn image_gen_options(&self) -> ImageGenOptions {
        ImageGenOptions {
            aspect_ratios: Vec::new(),
            image_sizes: IMAGES_IMAGE_SIZES.to_vec(),
            negative_prompt: false,
        }
    }
}

impl ImageEditJsonTunable for ImagesProvider {
    fn set_json_edits(&mut self, on: bool) {
        self.json_edits = on;
    }

    fn json_edits(&self) -> bool {
        self.json_edits
    }
}
