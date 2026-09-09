//! The `gemini` / `vertexai` providers (provider/google.go) on the Google dialect.

use std::borrow::Cow;

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{
    Attachment, JsonObject, Message, Raw, RawContent, Role, ToolCall, ToolDef,
};
use crate::provider::sink::{ReasoningGate, StreamSink};
use crate::provider::{
    ChatResult, HttpTransport, ImageTunable, Provider, ProviderKind, RoundResult, ToolProvider,
    TopPTunable, Tunable,
};
use reqwest::header::HeaderName;
use tokio_util::sync::CancellationToken;

use crate::llm::LlmError;
use crate::llm::client::Client;
use crate::llm::google::{
    GBlob, GContent, GFunctionCall, GFunctionDeclaration, GFunctionResp, GGenerationConfig, GPart,
    GThinkingConfig, GTool, GenerateRequest, Google,
};
use crate::provider::common::{HasCore, ProviderCore, credential_header, make_client};
use crate::provider::think::{StreamThinkSplitter, split_inline_think};
use crate::provider::usage_conv::google_usage;

/// Default Gemini API base URL (`https://generativelanguage.googleapis.com`).
pub(crate) const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
/// Default Vertex AI base URL (`https://aiplatform.googleapis.com`).
pub(crate) const VERTEX_DEFAULT_BASE_URL: &str = "https://aiplatform.googleapis.com";

/// The express-mode auth header both backends take (provider/google.go:62). No `Authorization`/ADC path exists.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// `role` of the user side of a `contents` list (system instruction, user turns and tool results).
const ROLE_USER: &str = "user";
/// `role` of the assistant side of a `contents` list.
const ROLE_MODEL: &str = "model";

/// Google provider (tool, tunable, `top_p`, `image_tunable`; `RawContent::Google`, sanitised on replay).
pub struct GoogleProvider {
    core: ProviderCore,
    client: Google,
    tool_call_ids: bool,
    image_output: bool,
}

impl HasCore for GoogleProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut ProviderCore {
        &mut self.core
    }
}

/// `llm.New(baseURL, http)` plus the static `x-goog-api-key` header (provider/google.go:61-62).
fn api_key_client(api_key: &str, base_url: &str, default: &str, http: HttpTransport) -> Client {
    make_client(base_url, default, Some(http)).with_header(
        HeaderName::from_static(API_KEY_HEADER),
        credential_header(api_key),
    )
}

impl GoogleProvider {
    /// Gemini API: base "" → `GEMINI_DEFAULT_BASE_URL`; version "v1beta", vertex false, ids true.
    pub fn gemini(
        api_key: &str,
        base_url: &str,
        model: &str,
        temperature: Option<f64>,
        http: impl Into<HttpTransport>,
    ) -> Self {
        Self {
            core: ProviderCore {
                kind: ProviderKind::Gemini,
                model: model.to_owned(),
                temperature,
                top_p: None,
                effort: None,
            },
            client: Google {
                client: api_key_client(api_key, base_url, GEMINI_DEFAULT_BASE_URL, http.into()),
                vertex: false,
                version: "v1beta".to_owned(),
            },
            tool_call_ids: true,
            image_output: false,
        }
    }

    /// Vertex AI: base "" → `VERTEX_DEFAULT_BASE_URL` + "v1beta1"; else "v1"; vertex true, ids false.
    pub fn vertex_ai(
        api_key: &str,
        base_url: &str,
        model: &str,
        temperature: Option<f64>,
        http: impl Into<HttpTransport>,
    ) -> Self {
        // genai constructor parity: the default endpoint speaks v1beta1, a custom base URL historically v1.
        let version = if base_url.is_empty() { "v1beta1" } else { "v1" };
        Self {
            core: ProviderCore {
                kind: ProviderKind::VertexAi,
                model: model.to_owned(),
                temperature,
                top_p: None,
                effort: None,
            },
            client: Google {
                client: api_key_client(api_key, base_url, VERTEX_DEFAULT_BASE_URL, http.into()),
                vertex: true,
                version: version.to_owned(),
            },
            tool_call_ids: false,
            image_output: false,
        }
    }

    /// Test seam (`TestGoogleGoldenRequest` pins Version on a custom URL).
    pub fn set_version(&mut self, v: &str) {
        v.clone_into(&mut self.client.version);
    }

    /// provider/google.go:139-207 (pure; tested clientless): (contents, `systemInstruction`).
    pub fn build_contents(&self, messages: &[Message]) -> (Vec<GContent>, Option<GContent>) {
        let mut contents: Vec<GContent> = Vec::new();
        let mut system: Option<GContent> = None;
        for msg in messages {
            match msg.role() {
                Role::System => {
                    if msg.is_tools_mount() {
                        continue; // a system-tools mount (K3 wire shape): chatcomp-only, skip here
                    }
                    system = Some(text_content(ROLE_USER, &msg.content));
                }
                Role::User => {
                    let content = if msg.attachments.is_empty() {
                        text_content(ROLE_USER, &msg.content)
                    } else {
                        let mut parts: Vec<GPart> =
                            msg.attachments.iter().map(inline_data_part).collect();
                        parts.push(text_part(&msg.content));
                        GContent {
                            role: ROLE_USER.to_owned(),
                            parts,
                        }
                    };
                    append_user_content(&mut contents, content);
                }
                Role::Assistant => self.push_assistant(&mut contents, msg),
                Role::Tool => {
                    let mut response = JsonObject::new();
                    let key = if msg.is_error() { "error" } else { "output" };
                    response.insert(key.to_owned(), msg.content.clone().into());
                    let part = GPart {
                        function_response: Some(GFunctionResp {
                            id: self.call_id(msg.tool_call_id()),
                            name: Some(msg.tool_call_name().to_owned()),
                            response: Some(response),
                        }),
                        ..GPart::default()
                    };
                    append_user_content(
                        &mut contents,
                        GContent {
                            role: ROLE_USER.to_owned(),
                            parts: vec![part],
                        },
                    );
                }
            }
        }
        (contents, system)
    }

    /// One assistant message: verbatim raw content (sanitised) > reconstructed tool calls > generated images >
    /// plain text (provider/google.go:160-192).
    fn push_assistant(&self, contents: &mut Vec<GContent>, msg: &Message) {
        // Only this dialect's own replay payload is trusted (Go's failed type assertion falls through).
        let raw = match msg.raw_content() {
            Some(RawContent::Google(r)) => serde_json::from_str::<GContent>(r.get()).ok(),
            _ => None,
        };
        if let Some(raw) = raw {
            if let Some(c) = sanitize_content(&raw) {
                contents.push(c.into_owned());
            }
            return;
        }
        if !msg.tool_calls().is_empty() {
            let mut parts = Vec::new();
            if !msg.content.is_empty() {
                parts.push(text_part(&msg.content));
            }
            for tc in msg.tool_calls() {
                parts.push(GPart {
                    function_call: Some(GFunctionCall {
                        id: self.call_id(&tc.id),
                        // Go marshals a nil/empty map[string]any as absent (omitempty).
                        args: if tc.arguments.is_empty() {
                            None
                        } else {
                            Some(tc.arguments.clone())
                        },
                        name: Some(tc.name.clone()),
                    }),
                    ..GPart::default()
                });
            }
            contents.push(GContent {
                role: ROLE_MODEL.to_owned(),
                parts,
            });
            return;
        }
        if !msg.attachments.is_empty() {
            // A generated image round-trips as a model inlineData part so follow-ups edit it in place.
            let mut parts = Vec::new();
            if !msg.content.is_empty() {
                parts.push(text_part(&msg.content));
            }
            parts.extend(msg.attachments.iter().map(inline_data_part));
            contents.push(GContent {
                role: ROLE_MODEL.to_owned(),
                parts,
            });
            return;
        }
        contents.push(text_content(ROLE_MODEL, &msg.content));
    }

    /// The call id, or `""` on a backend that rejects `functionCall`/`functionResponse` ids (Vertex AI).
    fn call_id(&self, id: &str) -> Option<String> {
        self.tool_call_ids.then(|| id.to_owned())
    }

    /// provider/google.go:208-237: contents + `systemInstruction` + `generationConfig` (emitted only when at
    /// least one knob is set). Tools are attached by the streaming path alone.
    // Go narrows the provider's float64 knobs with `float32(*p.temperature)` (google.go:213,218).
    #[allow(clippy::cast_possible_truncation)]
    fn build_request(&self, messages: &[Message]) -> GenerateRequest {
        let (contents, system_instruction) = self.build_contents(messages);
        let mut req = GenerateRequest {
            contents,
            system_instruction,
            generation_config: None,
            tools: Vec::new(),
        };
        let mut cfg = GGenerationConfig {
            temperature: self.core.temperature.map(|t| t as f32),
            top_p: self.core.top_p.map(|p| p as f32),
            thinking_config: self.core.effort.map(|e| GThinkingConfig {
                include_thoughts: true,
                // No level mapping or validation at this layer: "xhigh"/"max" fail server-side by design.
                thinking_level: Some(e.as_str().to_uppercase()),
            }),
            response_modalities: Vec::new(),
        };
        if self.image_output {
            // The explicit opt-in for official-API models that need responseModalities to emit images.
            cfg.response_modalities = vec!["TEXT", "IMAGE"];
        }
        if cfg.temperature.is_some()
            || cfg.top_p.is_some()
            || cfg.thinking_config.is_some()
            || !cfg.response_modalities.is_empty()
        {
            req.generation_config = Some(cfg);
        }
        req
    }
}

/// `{role, parts:[{text}]}` (provider/google.go:135-137).
fn text_content(role: &str, text: &str) -> GContent {
    GContent {
        role: role.to_owned(),
        parts: vec![text_part(text)],
    }
}

/// A bare `{text}` part.
fn text_part(text: &str) -> GPart {
    GPart {
        text: text.to_owned(),
        ..GPart::default()
    }
}

/// An `{inlineData:{data, mimeType}}` part; the attachment's filename is never sent.
fn inline_data_part(att: &Attachment) -> GPart {
    GPart {
        inline_data: Some(GBlob {
            data: att.data.clone(),
            mime_type: att.mime_type.clone(),
        }),
        ..GPart::default()
    }
}

/// Folds a user-role content into a trailing user-role content (provider/google.go:127-133).
///
/// Gemini multiturn requests alternate user/model roles, and an interrupted turn can leave tool results
/// (user-role function responses) directly followed by the next user message.
fn append_user_content(contents: &mut Vec<GContent>, mut c: GContent) {
    if let Some(last) = contents.last_mut()
        && last.role == ROLE_USER
    {
        last.parts.append(&mut c.parts);
        return;
    }
    contents.push(c);
}

/// Drops parts without data (google.go:55-63): `None` when nothing survives; `Borrowed` when all survive.
///
/// Sessions persisted before this filter existed can still carry such parts, so history replay must clean
/// them too, not just fresh stream output. The input is never mutated.
pub fn sanitize_content(c: &GContent) -> Option<Cow<'_, GContent>> {
    let kept = c.parts.iter().filter(|p| p.has_data()).count();
    if kept == 0 {
        return None;
    }
    if kept == c.parts.len() {
        return Some(Cow::Borrowed(c));
    }
    Some(Cow::Owned(GContent {
        parts: c.parts.iter().filter(|p| p.has_data()).cloned().collect(),
        role: c.role.clone(),
    }))
}

/// An `inlineData` part as a generated-image attachment.
fn image_attachment(blob: &GBlob) -> Attachment {
    Attachment {
        filename: String::new(),
        mime_type: blob.mime_type.clone(),
        data: blob.data.clone(),
    }
}

impl Provider for GoogleProvider {
    fn kind(&self) -> ProviderKind {
        self.core.kind
    }

    fn model(&self) -> &str {
        &self.core.model
    }

    fn set_model(&mut self, model: String) {
        self.core.model = model;
    }

    /// Go `var _ UsageReporter = (*GoogleProvider)(nil)` (provider/google.go:16) — the
    /// dialect (gemini AND vertexai) records last-call usage (T-38).
    fn reports_usage(&self) -> bool {
        true
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
            Ok(models.into_iter().map(|m| m.name).collect())
        })
    }

    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            let req = self.build_request(messages);
            let resp = self
                .client
                .generate(cancel, &self.core.model, &req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Chat, e))?;
            let usage = resp.usage_metadata.as_ref().map(google_usage);
            // The unary path surfaces generated images too (-m single-shot runs).
            let images = resp
                .candidates
                .first()
                .and_then(|c| c.content.as_ref())
                .map(|c| {
                    c.parts
                        .iter()
                        .filter_map(|p| p.inline_data.as_ref().map(image_attachment))
                        .collect()
                })
                .unwrap_or_default();
            let (text, _) = split_inline_think(&resp.text());
            Ok(ChatResult {
                text,
                usage,
                images,
            })
        })
    }

    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        Some(self)
    }

    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        Some(self)
    }

    fn as_top_p_tunable(&mut self) -> Option<&mut dyn TopPTunable> {
        Some(self)
    }

    fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
        Some(self)
    }
}

impl ToolProvider for GoogleProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(async move {
            let mut req = self.build_request(messages);
            if !tools.is_empty() {
                // ToolDef.deferred is ignored: the dialect has no deferral protocol.
                req.tools = vec![GTool {
                    function_declarations: tools
                        .iter()
                        .map(|t| GFunctionDeclaration {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters_json_schema: t.input_schema.clone(),
                        })
                        .collect(),
                }];
            }

            let mut gate = ReasoningGate::new(sink);
            let mut split = StreamThinkSplitter::new();
            let mut think_full = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut images: Vec<Attachment> = Vec::new();
            let mut usage = None;
            // All parts, verbatim, so the next round replays the thought signatures.
            let mut raw_parts: Vec<GPart> = Vec::new();

            let mut stream = self
                .client
                .stream_generate(cancel, &self.core.model, &req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Stream, e))?;

            loop {
                let resp = match stream.next().await {
                    Ok(Some(resp)) => resp,
                    Ok(None) => break,
                    Err(e) => {
                        // Whatever was already split belongs on the sink before the round fails.
                        split.flush(&mut gate);
                        return Err(ProviderError::wire(WireOp::Stream, e));
                    }
                };
                if let Some(u) = &resp.usage_metadata {
                    usage = Some(google_usage(u)); // last chunk wins
                }
                let Some(content) = resp.candidates.first().and_then(|c| c.content.as_ref()) else {
                    continue;
                };
                for part in &content.parts {
                    if part.has_data() && part.inline_data.is_none() {
                        // Generated images ride `images` (and the session bundle) — duplicating their
                        // base64 into the raw replay blob would double-store megabytes per image.
                        raw_parts.push(part.clone());
                    }
                    if let Some(blob) = &part.inline_data {
                        gate.close();
                        images.push(image_attachment(blob));
                    } else if part.thought {
                        // Wire-level reasoning bypasses the think-tag splitter and does NOT close reasoning.
                        gate.reasoning(&part.text);
                        think_full.push_str(&part.text);
                    } else if let Some(fc) = &part.function_call {
                        gate.close();
                        let name = fc.name.clone().unwrap_or_default();
                        // Gemini may not return an ID; generate one.
                        let id = fc
                            .id
                            .clone()
                            .unwrap_or_else(|| format!("call_{name}_{}", tool_calls.len()));
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments: fc.args.clone().unwrap_or_default(),
                        });
                    } else if !part.text.is_empty() {
                        split.write(&part.text, &mut gate);
                    }
                }
            }
            split.flush(&mut gate);
            gate.close();
            think_full.push_str(&split.think);

            let raw_content = if tool_calls.is_empty() {
                None
            } else {
                let content = GContent {
                    role: ROLE_MODEL.to_owned(),
                    parts: raw_parts,
                };
                Some(RawContent::Google(Raw::from_value(&content).map_err(
                    |e| ProviderError::wire(WireOp::Stream, LlmError::Encode(e)),
                )?))
            };
            Ok(RoundResult {
                content: split.content,
                reasoning: think_full,
                tool_calls,
                usage,
                raw_content,
                images,
            })
        })
    }
}

impl ImageTunable for GoogleProvider {
    fn set_image_output(&mut self, on: bool) {
        self.image_output = on;
    }

    fn image_output(&self) -> bool {
        self.image_output
    }
}
