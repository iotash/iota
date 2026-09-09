//! Google dialect (internal/llm/google.go): `generateContent` / `streamGenerateContent` / models / `:predict`
//! (Imagen) shapes, the `Google` endpoint and its stream.

use std::borrow::Cow;

use crate::llm::json::{JsonObject, Raw};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{client::Client, error::LlmError, models::query_escape, sse::Sse};

/// The Google endpoint (Gemini API or Vertex AI).
#[derive(Clone)]
pub struct Google {
    /// The wire client.
    pub client: Client,
    /// Vertex AI path rules when true.
    pub vertex: bool,
    /// API version segment (`v1beta`, `v1beta1`, `v1`).
    pub version: String,
}

impl Google {
    /// google.go:153-171. Rejects `?`, `&`, `..` → `InvalidModelName`. Vertex: `projects/`|`models/`|`publishers/` pass
    /// through; contains `/` → `publishers/<first>/models/<rest>`; else `publishers/google/models/<m>`. Gemini:
    /// `models/`|`tunedModels/` pass through; else `models/<m>`. Result `/{version}/{resolved}`.
    pub fn model_path(&self, model: &str) -> Result<String, LlmError> {
        if model.contains('?') || model.contains('&') || model.contains("..") {
            return Err(LlmError::InvalidModelName(model.to_owned()));
        }
        let resolved: Cow<'_, str> = if self.vertex {
            if model.starts_with("projects/")
                || model.starts_with("models/")
                || model.starts_with("publishers/")
            {
                Cow::Borrowed(model)
            } else if let Some((vendor, rest)) = model.split_once('/') {
                // The relay-station "vendor/model" convention (google.go:161-163).
                Cow::Owned(format!("publishers/{vendor}/models/{rest}"))
            } else {
                Cow::Owned(format!("publishers/google/models/{model}"))
            }
        } else if model.starts_with("models/") || model.starts_with("tunedModels/") {
            Cow::Borrowed(model)
        } else {
            Cow::Owned(format!("models/{model}"))
        };
        Ok(format!("/{}/{resolved}", self.version))
    }

    /// POST {path}:generateContent.
    pub async fn generate(
        &self,
        cancel: &CancellationToken,
        model: &str,
        req: &GenerateRequest,
    ) -> Result<GenerateResponse, LlmError> {
        let path = self.model_path(model)?;
        self.client
            .do_json(
                cancel,
                Method::POST,
                &format!("{path}:generateContent"),
                Some(req),
            )
            .await
    }

    /// POST {path}:streamGenerateContent?alt=sse.
    pub async fn stream_generate(
        &self,
        cancel: &CancellationToken,
        model: &str,
        req: &GenerateRequest,
    ) -> Result<GoogleStream, LlmError> {
        let path = self.model_path(model)?;
        let sse = self
            .client
            .stream(
                cancel,
                Method::POST,
                &format!("{path}:streamGenerateContent?alt=sse"),
                Some(req),
            )
            .await?;
        Ok(GoogleStream { sse })
    }

    /// Gemini: GET `/{version}/models`. Vertex: GET `/{version}/publishers/google/models`, on `is_list_fallback()` →
    /// GET literal `/v1beta/models` (fallback failure returns the ORIGINAL error). Pagination
    /// `?pageToken=<urlencoded>`. Entries under `models`|`publisherModels`|`data`; name = `name` else `id`. Sorted by name.
    pub async fn models(&self, cancel: &CancellationToken) -> Result<Vec<GModelInfo>, LlmError> {
        let base = if self.vertex {
            format!("/{}/publishers/google/models", self.version)
        } else {
            format!("/{}/models", self.version)
        };
        match self.list_models(cancel, &base).await {
            Ok(models) => Ok(models),
            Err(err) => {
                // Relay stations (zenmux-style) serve ONLY the Gemini-shaped listing; the literal path is
                // deliberate — it is NOT templated with `version` (google.go:225).
                if self.vertex
                    && err.is_list_fallback()
                    && let Ok(models) = self.list_models(cancel, PATH_MODELS_FALLBACK).await
                {
                    return Ok(models);
                }
                Err(err)
            }
        }
    }

    /// One listing path, following `nextPageToken` to the end and sorting by name (google.go:244-283).
    async fn list_models(
        &self,
        cancel: &CancellationToken,
        base: &str,
    ) -> Result<Vec<GModelInfo>, LlmError> {
        let mut models: Vec<GModelInfo> = Vec::new();
        let mut page_token = String::new();
        loop {
            let path = if page_token.is_empty() {
                base.to_owned()
            } else {
                format!("{base}?pageToken={}", query_escape(&page_token))
            };
            let page: ModelsPage = self.client.get_json(cancel, &path).await?;
            for list in [page.models, page.publisher_models, page.data] {
                for m in list {
                    let name = m.name.or(m.id).unwrap_or_default();
                    models.push(GModelInfo {
                        name,
                        methods: m.supported_generation_methods,
                        output: m.output_modalities,
                    });
                }
            }
            if page.next_page_token.is_empty() {
                break;
            }
            page_token = page.next_page_token;
        }
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(models)
    }

    /// POST {path}:predict.
    pub async fn predict(
        &self,
        cancel: &CancellationToken,
        model: &str,
        req: &PredictRequest,
    ) -> Result<PredictResponse, LlmError> {
        let path = self.model_path(model)?;
        self.client
            .do_json(cancel, Method::POST, &format!("{path}:predict"), Some(req))
            .await
    }
}

/// The Gemini-shaped listing path the Vertex listing falls back to (google.go:225, literal — never templated).
pub(crate) const PATH_MODELS_FALLBACK: &str = "/v1beta/models";

/// One page of a model listing; relays answer under any of the three keys (google.go:258-263).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ModelsPage {
    #[serde(default)]
    models: Vec<ModelsEntry>,
    #[serde(default)]
    publisher_models: Vec<ModelsEntry>,
    #[serde(default)]
    data: Vec<ModelsEntry>,
    #[serde(default)]
    next_page_token: String,
}

/// One listing entry (google.go:245-250).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ModelsEntry {
    #[serde(default, deserialize_with = "crate::llm::none_if_empty")]
    name: Option<String>,
    /// `openai`-style relays.
    #[serde(default, deserialize_with = "crate::llm::none_if_empty")]
    id: Option<String>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

/// One listed model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GModelInfo {
    /// Model name (`name` else `id`).
    pub name: String,
    /// Supported generation methods.
    pub methods: Vec<String>,
    /// Supported output modalities.
    pub output: Vec<String>,
}

/// A `Content` (role + parts).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GContent {
    /// Parts; empty omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<GPart>,
    /// Role; `None` omitted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
}

/// A `Part`. `PartialEq` is derivable ONLY because the opaque parts are `Raw`, never a bare boxed `RawValue`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GPart {
    /// Text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Whether the text is a thought.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thought: bool,
    /// Thought signature bytes (base64 on the wire).
    #[serde(default, with = "b64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub thought_signature: Vec<u8>,
    /// Inline blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GBlob>,
    /// File reference (opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<Raw>,
    /// Function call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GFunctionCall>,
    /// Function response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GFunctionResp>,
    /// Executable code (opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_code: Option<Raw>,
    /// Code execution result (opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<Raw>,
    /// Video metadata (opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_metadata: Option<Raw>,
}

impl GPart {
    /// google.go:55-63 (thought/thoughtSignature/videoMetadata do NOT count).
    ///
    /// Vertex rejects a request containing an empty `{}` part (`required oneof field 'data' must have one
    /// initialized field`) and a stream can trail one, so replayed content is filtered through this.
    pub fn has_data(&self) -> bool {
        !self.text.is_empty()
            || self.inline_data.is_some()
            || self.file_data.is_some()
            || self.function_call.is_some()
            || self.function_response.is_some()
            || self.executable_code.is_some()
            || self.code_execution_result.is_some()
    }
}

/// An inline blob.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GBlob {
    /// Raw bytes (base64 on the wire); empty omitted.
    #[serde(default, with = "b64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<u8>,
    /// MIME type; `None` omitted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime_type: String,
}

/// A function call part.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct GFunctionCall {
    /// Call id; `None` omitted.
    #[serde(
        default,
        deserialize_with = "crate::llm::none_if_empty",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    /// Arguments; `None` omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<JsonObject>,
    /// Function name; `None` omitted.
    #[serde(
        default,
        deserialize_with = "crate::llm::none_if_empty",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
}

/// A function response part.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct GFunctionResp {
    /// Call id; `None` omitted.
    #[serde(
        default,
        deserialize_with = "crate::llm::none_if_empty",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    /// Function name; `None` omitted.
    #[serde(
        default,
        deserialize_with = "crate::llm::none_if_empty",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
    /// Response object; `None` omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<JsonObject>,
}

/// `generationConfig`.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GGenerationConfig {
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Thinking configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<GThinkingConfig>,
    /// Response modalities (e.g. `TEXT`, `IMAGE`); empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<&'static str>,
}

/// `thinkingConfig`.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GThinkingConfig {
    /// Include thought parts; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub include_thoughts: bool,
    /// Thinking level; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

/// One `tools` entry.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GTool {
    /// Function declarations; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub function_declarations: Vec<GFunctionDeclaration>,
}

/// A function declaration.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GFunctionDeclaration {
    /// Name; `None` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Description; `None` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// JSON Schema; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters_json_schema: Option<JsonObject>,
}

/// `generateContent` body.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerateRequest {
    /// Contents; always present (`[]` when empty — DIVERGENCES D-14).
    pub contents: Vec<GContent>,
    /// System instruction; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GContent>,
    /// Generation config; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GGenerationConfig>,
    /// Tools; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<GTool>,
}

/// `generateContent` response (also one stream chunk).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerateResponse {
    /// Candidates.
    #[serde(default)]
    pub candidates: Vec<GCandidate>,
    /// Usage, when reported.
    #[serde(default)]
    pub usage_metadata: Option<GUsageMetadata>,
    /// In-band error; `null` is ABSENT.
    #[serde(default)]
    pub error: Option<Raw>,
}

/// One candidate.
#[derive(Deserialize, Default)]
pub struct GCandidate {
    /// The content.
    #[serde(default)]
    pub content: Option<GContent>,
}

/// `usageMetadata`.
#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GUsageMetadata {
    /// Prompt tokens.
    #[serde(default)]
    pub prompt_token_count: u64,
    /// Candidate tokens.
    #[serde(default)]
    pub candidates_token_count: u64,
    /// Thought tokens.
    #[serde(default)]
    pub thoughts_token_count: u64,
    /// Cached content tokens.
    #[serde(default)]
    pub cached_content_token_count: u64,
    /// Total tokens.
    #[serde(default)]
    pub total_token_count: u64,
}

impl GenerateResponse {
    /// Candidate 0 non-thought texts concatenated.
    pub fn text(&self) -> String {
        let Some(content) = self.candidates.first().and_then(|c| c.content.as_ref()) else {
            return String::new();
        };
        content
            .parts
            .iter()
            .filter(|p| !p.thought)
            .map(|p| p.text.as_str())
            .collect()
    }
}

/// serde `with` module: `Vec<u8>` <-> standard padded base64 string.
pub(crate) mod b64_bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Encodes `v` as a standard padded base64 string.
    pub(crate) fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }

    /// Decodes a standard padded base64 string; a JSON `null` decodes to no bytes (Go leaves the slice nil).
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let Some(s) = Option::<String>::deserialize(d)? else {
            return Ok(Vec::new());
        };
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

/// A `streamGenerateContent?alt=sse` stream of `GenerateResponse` chunks.
pub struct GoogleStream {
    sse: Sse,
}

impl GoogleStream {
    /// Next chunk: `Ok(None) && !saw_event()` → `NoEvents`; JSON failure → `MalformedChunk`; non-null `error` → `InBand`.
    pub async fn next(&mut self) -> Result<Option<GenerateResponse>, LlmError> {
        let Some(evt) = self.sse.next().await? else {
            return if self.sse.saw_event() {
                Ok(None)
            } else {
                Err(LlmError::NoEvents)
            };
        };
        let resp: GenerateResponse =
            serde_json::from_slice(&evt.data).map_err(LlmError::MalformedChunk)?;
        if let Some(e) = &resp.error {
            return Err(LlmError::InBand(e.get().to_owned()));
        }
        Ok(Some(resp))
    }
}

// Imagen :predict (google.go:294-342)

/// `:predict` body.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PredictRequest {
    /// Instances (one prompt).
    pub instances: Vec<GImageInstance>,
    /// Parameters; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<GImageParams>,
}

/// One `:predict` instance.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GImageInstance {
    /// The prompt.
    pub prompt: String,
    /// Reference images; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reference_images: Vec<GReferenceImage>,
}

/// A reference image.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GReferenceImage {
    /// Reference type; `None` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reference_type: String,
    /// Reference id; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<u32>,
    /// The image; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_image: Option<GImageBlob>,
}

/// An image blob (`:predict`).
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GImageBlob {
    /// Raw bytes (base64 on the wire); empty omitted.
    #[serde(default, with = "b64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub bytes_base64_encoded: Vec<u8>,
    /// MIME type; `None` omitted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime_type: String,
}

/// `:predict` parameters.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GImageParams {
    /// Sample count; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u32>,
    /// Aspect ratio; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    /// Negative prompt; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    /// Image size (`sampleImageSize`); `None` omitted.
    #[serde(rename = "sampleImageSize", skip_serializing_if = "Option::is_none")]
    pub image_size: Option<String>,
}

/// `:predict` response.
#[derive(Deserialize, Default)]
pub struct PredictResponse {
    /// Predictions.
    #[serde(default)]
    pub predictions: Vec<GPrediction>,
}

/// One prediction.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GPrediction {
    /// Inline bytes (base64 on the wire).
    #[serde(default, with = "b64_bytes")]
    pub bytes_base64_encoded: Vec<u8>,
    /// GCS URI of the result.
    #[serde(default)]
    pub gcs_uri: String,
    /// MIME type.
    #[serde(default)]
    pub mime_type: String,
    /// Safety-filter reason.
    #[serde(default)]
    pub rai_filtered_reason: String,
}

#[cfg(test)]
mod tests {
    use super::{GBlob, GContent, GPart};

    /// google.go:55-63: `thought`, `thoughtSignature` and `videoMetadata` never make a part carry data.
    #[test]
    fn has_data_table() {
        assert!(!GPart::default().has_data());
        assert!(
            !GPart {
                thought: true,
                thought_signature: vec![1],
                video_metadata: Some(
                    crate::provider::model::Raw::from_string("{}".to_owned()).unwrap()
                ),
                ..GPart::default()
            }
            .has_data()
        );
        assert!(
            GPart {
                text: "x".to_owned(),
                ..GPart::default()
            }
            .has_data()
        );
        assert!(
            GPart {
                inline_data: Some(GBlob::default()),
                ..GPart::default()
            }
            .has_data()
        );
        assert!(
            GPart {
                file_data: Some(crate::provider::model::Raw::from_string("1".to_owned()).unwrap()),
                ..GPart::default()
            }
            .has_data()
        );
    }

    /// `[]byte` fields are standard PADDED base64 both ways; a `null` decodes to no bytes.
    #[test]
    fn b64_bytes_round_trip() {
        let c: GContent =
            serde_json::from_str(r#"{"parts":[{"thoughtSignature":"AQID"},{"inlineData":{"data":null,"mimeType":"image/png"}}]}"#)
                .unwrap();
        assert_eq!(c.parts[0].thought_signature, [1, 2, 3]);
        assert!(c.parts[1].inline_data.as_ref().unwrap().data.is_empty());
        let part = GPart {
            thought_signature: vec![1, 2, 3],
            ..GPart::default()
        };
        assert_eq!(
            serde_json::to_string(&part).unwrap(),
            r#"{"thoughtSignature":"AQID"}"#
        );
        assert!(
            serde_json::from_str::<GContent>(r#"{"parts":[{"thoughtSignature":"!!"}]}"#).is_err()
        );
    }
}
