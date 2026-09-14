//! Chat-completions dialect (internal/llm/chatcomp.go): request/response/chunk shapes, `ChatComp` and its stream.
//! Request field order = Go struct order; `skip_serializing_if` reproduces every `omitempty`.

use crate::llm::json::{JsonObject, Raw, is_none_or_empty};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{client::Client, error::LlmError, models, sse::Sse};

/// The chat-completions endpoint path.
pub(crate) const PATH_CHAT: &str = "/chat/completions";

/// `POST /chat/completions` body.
#[derive(Serialize)]
pub(crate) struct ChatCompRequest {
    /// Model id.
    pub(crate) model: String,
    /// Conversation; always present.
    pub(crate) messages: Vec<ChatMessage>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// Nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// Reasoning effort string; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<String>,
    /// Advertised tools; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ChatTool>,
    /// Streaming flag; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stream: bool,
    /// Streaming options (usage in the final chunk).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_options: Option<ChatStreamOptions>,
}

/// One serialised message: a built message, a system-tools mount, or a recorded assistant payload replayed verbatim.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ChatMessage {
    /// A built message.
    Msg(ChatMsg),
    /// The system-tools mount (`role` + `tools`, NO content key).
    ToolsMount(ChatToolsMsg),
    /// A recorded assistant message replayed verbatim (validated with `Raw::from_string` at record time).
    Raw(Raw),
}

/// A built chat message.
#[derive(Serialize)]
pub(crate) struct ChatMsg {
    /// Role string.
    pub(crate) role: &'static str,
    /// Content; never skipped.
    pub(crate) content: ChatContent,
    /// Requested tool calls; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ChatToolCall>,
    /// Tool-result correlation id; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
}

/// The system-tools mount message (NO content key).
#[derive(Serialize)]
pub(crate) struct ChatToolsMsg {
    /// Role string (`system`).
    pub(crate) role: &'static str,
    /// The mounted tools.
    pub(crate) tools: Vec<ChatTool>,
}

/// Message content: a plain string or a parts array.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ChatContent {
    /// Plain text.
    Text(String),
    /// Multi-part content (`image_url` → `file` → `text` LAST).
    Parts(Vec<ChatPart>),
}

/// One content part.
#[derive(Serialize)]
#[serde(tag = "type")]
pub(crate) enum ChatPart {
    /// A text part.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// An image part (data URL).
    #[serde(rename = "image_url")]
    ImageUrl {
        /// The image URL object.
        image_url: ChatImageUrl,
    },
    /// A file part (data URL + filename).
    #[serde(rename = "file")]
    File {
        /// The file object.
        file: ChatFileData,
    },
}

/// The `image_url` object.
#[derive(Serialize)]
pub(crate) struct ChatImageUrl {
    /// Data URL.
    pub(crate) url: String,
}

/// The `file` object.
#[derive(Serialize)]
pub(crate) struct ChatFileData {
    /// Data URL.
    pub(crate) file_data: String,
    /// File name.
    pub(crate) filename: String,
}

/// An assistant tool call being replayed.
#[derive(Serialize)]
pub(crate) struct ChatToolCall {
    /// Call id.
    pub(crate) id: String,
    /// Always `"function"`.
    pub(crate) r#type: &'static str,
    /// Function name + JSON-encoded arguments.
    pub(crate) function: ChatToolCallFunc,
}

/// The function half of a tool call.
#[derive(Serialize)]
pub(crate) struct ChatToolCallFunc {
    /// Function name.
    pub(crate) name: String,
    /// JSON-encoded arguments.
    pub(crate) arguments: String,
}

/// An advertised tool.
#[derive(Serialize, Clone)]
pub(crate) struct ChatTool {
    /// Always `"function"`.
    pub(crate) r#type: &'static str,
    /// The function definition.
    pub(crate) function: ChatToolFunction,
}

/// An advertised function.
#[derive(Serialize, Clone)]
pub(crate) struct ChatToolFunction {
    /// Function name.
    pub(crate) name: String,
    /// Description; `None` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    /// JSON Schema; `None` and the empty map omitted (Go `omitempty`).
    #[serde(skip_serializing_if = "is_none_or_empty")]
    pub(crate) parameters: Option<JsonObject>,
}

/// `stream_options`.
#[derive(Serialize)]
pub(crate) struct ChatStreamOptions {
    /// Ask for usage in the final chunk.
    pub(crate) include_usage: bool,
}

/// Token usage (`usage`). Chat-completions reports `prompt_tokens`/`completion_tokens`/`prompt_tokens_details`;
/// the responses dialect reports the same figures as `input_tokens`/`output_tokens`/`input_tokens_details` — one
/// struct decodes both.
#[derive(Deserialize, Default, Clone)]
pub struct OpenAiUsage {
    /// Prompt (input) tokens, cache hits INCLUDED.
    #[serde(default, alias = "prompt_tokens")]
    pub input_tokens: u64,
    /// Completion (output) tokens, reasoning tokens included.
    #[serde(default, alias = "completion_tokens")]
    pub output_tokens: u64,
    /// Total tokens (compat servers often omit it).
    #[serde(default)]
    pub total_tokens: u64,
    /// Cached-token details.
    #[serde(default, alias = "prompt_tokens_details")]
    pub input_tokens_details: Option<OpenAiTokenDetails>,
}

/// `prompt_tokens_details` / `input_tokens_details`.
#[derive(Deserialize, Default, Clone)]
pub struct OpenAiTokenDetails {
    /// Cached prompt tokens.
    #[serde(default)]
    pub cached_tokens: u64,
}

/// Unary response body.
#[derive(Deserialize, Default)]
pub(crate) struct ChatCompResponse {
    /// Choices.
    #[serde(default)]
    pub(crate) choices: Vec<ChatChoice>,
    /// Usage, when reported.
    #[serde(default)]
    pub(crate) usage: Option<OpenAiUsage>,
}

/// One unary choice.
#[derive(Deserialize, Default)]
pub(crate) struct ChatChoice {
    /// The message.
    #[serde(default)]
    pub(crate) message: ChatChoiceMessage,
}

/// The message of a unary choice.
#[derive(Deserialize, Default)]
pub(crate) struct ChatChoiceMessage {
    /// Content (nullable).
    #[serde(default)]
    pub(crate) content: Option<String>,
}

/// One stream chunk.
#[derive(Deserialize, Default)]
pub(crate) struct ChatChunk {
    /// Choices.
    #[serde(default)]
    pub(crate) choices: Vec<ChatChunkChoice>,
    /// Usage (final chunk).
    #[serde(default)]
    pub(crate) usage: Option<OpenAiUsage>,
    /// In-band error; `null` is ABSENT.
    #[serde(default)]
    pub(crate) error: Option<Raw>,
}

/// One stream choice.
#[derive(Deserialize, Default)]
pub(crate) struct ChatChunkChoice {
    /// The delta.
    #[serde(default)]
    pub(crate) delta: ChatDelta,
    /// Finish reason (nullable).
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

/// A stream delta.
#[derive(Deserialize, Default)]
pub(crate) struct ChatDelta {
    /// Content delta.
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// Reasoning delta (`reasoning`).
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    /// Reasoning delta (`reasoning_content`).
    #[serde(default)]
    pub(crate) reasoning_content: Option<String>,
    /// Tool-call deltas.
    #[serde(default)]
    pub(crate) tool_calls: Vec<ChatToolDelta>,
}

/// One tool-call delta.
#[derive(Deserialize, Default)]
pub(crate) struct ChatToolDelta {
    /// Call slot index (sparse indices are kept — DIVERGENCES F-04).
    #[serde(default)]
    pub(crate) index: i64,
    /// Call id (first delta only).
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// Function delta.
    #[serde(default)]
    pub(crate) function: ChatToolDeltaFunc,
}

/// The function half of a tool-call delta.
#[derive(Deserialize, Default)]
pub(crate) struct ChatToolDeltaFunc {
    /// Function name.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Arguments fragment.
    #[serde(default)]
    pub(crate) arguments: Option<String>,
}

/// The chat-completions endpoint.
pub(crate) struct ChatComp {
    /// The wire client.
    pub(crate) client: Client,
}

impl ChatComp {
    /// Forces `stream=false`, `stream_options=None`; POST `/chat/completions`.
    pub(crate) async fn complete(
        &self,
        cancel: &CancellationToken,
        req: &mut ChatCompRequest,
    ) -> Result<ChatCompResponse, LlmError> {
        req.stream = false;
        req.stream_options = None;
        self.client
            .do_json(cancel, Method::POST, PATH_CHAT, Some(&*req))
            .await
    }

    /// Forces `stream=true`, `stream_options={include_usage:true}`.
    pub(crate) async fn stream_completion(
        &self,
        cancel: &CancellationToken,
        req: &mut ChatCompRequest,
    ) -> Result<ChatCompStream, LlmError> {
        req.stream = true;
        req.stream_options = Some(ChatStreamOptions {
            include_usage: true,
        });
        let sse = self
            .client
            .stream(cancel, Method::POST, PATH_CHAT, Some(&*req))
            .await?;
        Ok(ChatCompStream { sse })
    }

    /// = `models::openai_model_ids(&self.client, cancel)`.
    pub(crate) async fn models(&self, cancel: &CancellationToken) -> Result<Vec<String>, LlmError> {
        models::openai_model_ids(&self.client, cancel).await
    }
}

/// A chat-completions SSE stream of `ChatChunk`s.
pub(crate) struct ChatCompStream {
    sse: Sse,
}

impl ChatCompStream {
    /// Next chunk: `Ok(None) && !saw_event()` → `NoEvents`; JSON failure → `MalformedChunk`; non-null `error` → `InBand`.
    pub(crate) async fn next(&mut self) -> Result<Option<ChatChunk>, LlmError> {
        let Some(evt) = self.sse.next().await? else {
            // A stream that ended without a single event answered a stream request with a plain body.
            return if self.sse.saw_event() {
                Ok(None)
            } else {
                Err(LlmError::NoEvents)
            };
        };
        let chunk: ChatChunk =
            serde_json::from_slice(&evt.data).map_err(LlmError::MalformedChunk)?;
        if let Some(e) = &chunk.error {
            return Err(LlmError::InBand(e.get().to_owned()));
        }
        Ok(Some(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `messages` is always present; every `omitempty` field disappears at its Go-zero value, and the key
    /// order is the Go struct order.
    #[test]
    fn request_omits_exactly_what_go_omits() {
        let mut req = ChatCompRequest {
            model: "m".to_owned(),
            messages: Vec::new(),
            temperature: None,
            top_p: None,
            reasoning_effort: None,
            tools: Vec::new(),
            stream: false,
            stream_options: None,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m","messages":[]}"#
        );

        // A pointer to 0.0 IS sent (Go omits only nil pointers).
        req.temperature = Some(0.0);
        req.top_p = Some(0.9);
        req.reasoning_effort = Some("high".to_owned());
        req.stream = true;
        req.stream_options = Some(ChatStreamOptions {
            include_usage: true,
        });
        req.messages.push(ChatMessage::Msg(ChatMsg {
            role: "user",
            content: ChatContent::Text(String::new()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }));
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m","messages":[{"role":"user","content":""}],"temperature":0.0,"top_p":0.9,"reasoning_effort":"high","stream":true,"stream_options":{"include_usage":true}}"#
        );
    }

    /// The tools mount carries NO content key; a recorded assistant payload rides through verbatim.
    #[test]
    fn tools_mount_has_no_content_and_raw_rides_verbatim() {
        let raw = r#"{"role":"assistant","content":"prev","reasoning":"think"}"#;
        let msgs = vec![
            ChatMessage::ToolsMount(ChatToolsMsg {
                role: "system",
                tools: vec![ChatTool {
                    r#type: "function",
                    function: ChatToolFunction {
                        name: "f".to_owned(),
                        description: String::new(),
                        parameters: None,
                    },
                }],
            }),
            ChatMessage::Raw(Raw::from_string(raw.to_owned()).unwrap()),
        ];
        assert_eq!(
            serde_json::to_string(&msgs).unwrap(),
            format!(
                r#"[{{"role":"system","tools":[{{"type":"function","function":{{"name":"f"}}}}]}},{raw}]"#
            )
        );
    }

    /// A JSON `null` in a nullable response string deserialises to `None` (normalised to `""` by the consumer),
    /// and an absent tool-call `index` is 0.
    #[test]
    fn chunk_nulls_and_defaults() {
        let chunk: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"content":null,"tool_calls":[{"id":"c1"}]},"finish_reason":null}],"usage":null,"error":null}"#,
        )
        .unwrap();
        assert!(chunk.usage.is_none() && chunk.error.is_none());
        let choice = &chunk.choices[0];
        assert!(choice.finish_reason.is_none() && choice.delta.content.is_none());
        assert_eq!(choice.delta.tool_calls[0].index, 0);
        assert!(choice.delta.tool_calls[0].function.name.is_none());
    }
}
