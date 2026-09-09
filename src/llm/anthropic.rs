//! Anthropic messages dialect (internal/llm/anthropic.go): request/response/event shapes, `Anthropic` and its stream.

use crate::llm::json::{JsonObject, Raw};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::models::query_escape;
use super::{client::Client, error::LlmError, sse::Sse};

/// The messages endpoint path.
pub(crate) const PATH_MESSAGES: &str = "/v1/messages";
/// The models endpoint path.
pub(crate) const PATH_MODELS: &str = "/v1/models";
/// The `anthropic-version` header value.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Always sent as `max_tokens` (POLICY 5 quirk).
pub(crate) const MAX_TOKENS: u32 = 4096;
/// The server-side tool-search tool type.
pub(crate) const SERVER_SEARCH_TOOL_TYPE: &str = "tool_search_tool_regex_20251119";
/// The server-side tool-search tool name.
pub(crate) const SERVER_SEARCH_TOOL_NAME: &str = "tool_search_tool_regex";

/// `POST /v1/messages` body.
#[derive(Serialize)]
pub(crate) struct AnthropicRequest {
    /// Model id.
    pub(crate) model: String,
    /// Always `MAX_TOKENS`.
    pub(crate) max_tokens: u32,
    /// Conversation.
    pub(crate) messages: Vec<AnthropicMsg>,
    /// System text blocks; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) system: Vec<TextBlock>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// Nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// Effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_config: Option<OutputConfig>,
    /// Tools; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<AnthropicToolEntry>,
    /// Streaming flag; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stream: bool,
}

/// One message; `content` is ALWAYS an array.
#[derive(Serialize)]
pub(crate) struct AnthropicMsg {
    /// Role string.
    pub(crate) role: &'static str,
    /// Content blocks.
    pub(crate) content: Vec<Block>,
}

/// A text block.
#[derive(Serialize)]
pub(crate) struct TextBlock {
    /// Always `"text"`.
    pub(crate) r#type: &'static str,
    /// The text.
    pub(crate) text: String,
}

/// One content block: typed, or a captured server block replayed verbatim.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Block {
    /// A typed block.
    Typed(TypedBlock),
    /// A captured `server_tool_use` / `tool_search_tool_result` block, verbatim.
    Raw(Raw),
}

/// A typed content block.
#[derive(Serialize)]
#[serde(tag = "type")]
pub(crate) enum TypedBlock {
    /// `text`.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// `image`.
    #[serde(rename = "image")]
    Image {
        /// Base64 source.
        source: Source,
    },
    /// `document`.
    #[serde(rename = "document")]
    Document {
        /// Base64 source.
        source: Source,
    },
    /// `tool_use` (field order type,id,input,name).
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Call id.
        id: String,
        /// Arguments.
        input: JsonObject,
        /// Tool name.
        name: String,
    },
    /// `tool_result`.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// The answered call id.
        tool_use_id: String,
        /// Result text blocks.
        content: Vec<TextBlock>,
        /// ALWAYS emitted.
        is_error: bool,
    },
}

/// A base64 source.
#[derive(Serialize)]
pub(crate) struct Source {
    /// Always `"base64"`.
    pub(crate) r#type: &'static str,
    /// MIME type.
    pub(crate) media_type: String,
    /// Base64 payload.
    pub(crate) data: String,
}

/// `output_config`.
#[derive(Serialize)]
pub(crate) struct OutputConfig {
    /// Effort string.
    pub(crate) effort: String,
}

/// One entry of the `tools` array.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicToolEntry {
    /// A client tool.
    Tool(AnthropicTool),
    /// The server-side tool-search tool.
    Server(ServerTool),
}

/// A client tool.
#[derive(Serialize)]
pub(crate) struct AnthropicTool {
    /// Tool name.
    pub(crate) name: String,
    /// Description; `""` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    /// Input schema.
    pub(crate) input_schema: ToolSchema,
    /// Deferred loading; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) defer_loading: bool,
}

/// A tool input schema.
#[derive(Serialize)]
pub(crate) struct ToolSchema {
    /// Always `"object"`.
    pub(crate) r#type: &'static str,
    /// Properties; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) properties: Option<serde_json::Value>,
    /// Required names; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) required: Vec<String>,
}

/// The server-side tool-search tool.
#[derive(Serialize)]
pub(crate) struct ServerTool {
    /// `SERVER_SEARCH_TOOL_TYPE`.
    pub(crate) r#type: &'static str,
    /// `SERVER_SEARCH_TOOL_NAME`.
    pub(crate) name: &'static str,
}

/// Token usage.
#[derive(Deserialize, Default, Clone)]
pub struct AnthropicUsage {
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cache read tokens.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Cache creation tokens.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Unary response body.
#[derive(Deserialize, Default)]
pub(crate) struct AnthropicResponse {
    /// Content blocks.
    #[serde(default)]
    pub(crate) content: Vec<RespBlock>,
    /// Usage, when reported.
    #[serde(default)]
    pub(crate) usage: Option<AnthropicUsage>,
}

/// One unary content block; only text is read (a unary response never carries tool calls).
#[derive(Deserialize)]
#[serde(tag = "type")]
pub(crate) enum RespBlock {
    /// A text block.
    #[serde(rename = "text")]
    Text {
        /// The text.
        #[serde(default)]
        text: String,
    },
    /// Any other block type.
    #[serde(other)]
    Other,
}

/// A content block's class, as `content_block_start` names it — or as a stored replay block names itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    /// `text`.
    Text,
    /// `thinking` — replays with its signature.
    Thinking,
    /// `redacted_thinking` — opaque, replays verbatim.
    RedactedThinking,
    /// `tool_use` — a client tool call.
    ToolUse,
    /// `server_tool_use` — a server-side search call.
    ServerToolUse,
    /// `tool_search_tool_result` — the search result, complete at start.
    ToolSearchToolResult,
    /// Anything else, by name.
    Other(String),
}

impl Default for BlockKind {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl BlockKind {
    /// The class for a `type` string.
    pub(crate) fn parse(kind: &str) -> Self {
        match kind {
            "text" => Self::Text,
            "thinking" => Self::Thinking,
            "redacted_thinking" => Self::RedactedThinking,
            "tool_use" => Self::ToolUse,
            "server_tool_use" => Self::ServerToolUse,
            "tool_search_tool_result" => Self::ToolSearchToolResult,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The class of a stored block, read off its own `"type"`; an unparseable blob is `Other("")`.
    pub(crate) fn of_raw(raw: &Raw) -> Self {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(default)]
            r#type: String,
        }
        serde_json::from_str::<Probe>(raw.get())
            .map_or_else(|_| Self::default(), |p| Self::parse(&p.r#type))
    }

    /// Whether the block replays unconditionally: the two thinking shapes do.
    pub(crate) fn is_thinking(&self) -> bool {
        matches!(self, Self::Thinking | Self::RedactedThinking)
    }
}

/// One `content_block_delta` payload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeltaKind {
    /// `text_delta`.
    Text(String),
    /// `thinking_delta`.
    Thinking(String),
    /// `input_json_delta` — a tool-input JSON fragment.
    InputJson(String),
    /// `signature_delta` — the seal that makes a thinking block replayable; dropping it makes the
    /// next request fail with "the content[].thinking in the thinking mode must be passed back to
    /// the API" on every endpoint that enforces the rule.
    Signature(String),
    /// An unknown delta type; ignored.
    Other,
}

/// Why the message stopped (`message_delta`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StopReason {
    /// `end_turn`.
    EndTurn,
    /// `max_tokens`.
    MaxTokens,
    /// `stop_sequence`.
    StopSequence,
    /// `tool_use` — the assembled tool calls are the round's result.
    ToolUse,
    /// Anything else, by name.
    Other(String),
}

impl StopReason {
    /// The reason for a `stop_reason` string; `None` when it is empty.
    fn parse(reason: &str) -> Option<Self> {
        Some(match reason {
            "" => return None,
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "stop_sequence" => Self::StopSequence,
            "tool_use" => Self::ToolUse,
            other => Self::Other(other.to_owned()),
        })
    }
}

/// One stream event, classified once here so the dialect matches on shapes, never on strings.
pub(crate) enum AnthropicEvent {
    /// `message_start`: the input-side usage.
    MessageStart {
        /// Input tokens plus the cache counts.
        usage: Option<AnthropicUsage>,
    },
    /// `content_block_start`: the block's class, its tool id/name when it has them, and the start
    /// JSON verbatim (a server result block replays with it).
    BlockStart {
        /// Content block index.
        index: u32,
        /// The block's class.
        kind: BlockKind,
        /// Tool-use id (`tool_use` / `server_tool_use`).
        id: String,
        /// Tool name (`tool_use` / `server_tool_use`).
        name: String,
        /// The whole start-event block JSON.
        raw: Raw,
    },
    /// `content_block_delta`.
    Delta {
        /// Content block index.
        index: u32,
        /// The payload.
        delta: DeltaKind,
    },
    /// `message_delta`: the stop reason and the cumulative output tokens.
    MessageDelta {
        /// Why the message stopped.
        stop_reason: Option<StopReason>,
        /// Cumulative output tokens, when reported.
        output_tokens: Option<u64>,
    },
    /// `content_block_stop`, `message_stop`, and anything unknown — nothing to act on.
    Other,
}

/// The event as the wire carries it: `type` plus every payload any event type has.
#[derive(Deserialize, Default)]
struct RawEvent {
    /// Event type (JSON `type`, falling back to the SSE `event:` field).
    #[serde(default)]
    r#type: String,
    /// Content block index.
    #[serde(default)]
    index: u32,
    /// `message_start` payload.
    #[serde(default)]
    message: Option<EventMessage>,
    /// Full start-event block JSON (kept verbatim for replay).
    #[serde(default)]
    content_block: Option<Raw>,
    /// Delta payload.
    #[serde(default)]
    delta: Option<RawDelta>,
    /// Usage (`message_delta`).
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    /// Error envelope (type `error`).
    #[serde(default)]
    error: Option<Raw>,
}

impl RawEvent {
    /// The classified event (anthropic.go:243-281 — the one place the event grammar is spelled out).
    fn classify(self) -> AnthropicEvent {
        match self.r#type.as_str() {
            "message_start" => AnthropicEvent::MessageStart {
                usage: self.message.and_then(|m| m.usage),
            },
            "content_block_start" => match self.content_block {
                Some(raw) => {
                    // The parsed fields are the common subset; the raw JSON is what a server result block
                    // replays with.
                    let start: ContentBlockStart =
                        serde_json::from_str(raw.get()).unwrap_or_default();
                    AnthropicEvent::BlockStart {
                        index: self.index,
                        kind: BlockKind::parse(&start.r#type),
                        id: start.id,
                        name: start.name,
                        raw,
                    }
                }
                None => AnthropicEvent::Other,
            },
            "content_block_delta" => match self.delta {
                Some(delta) => AnthropicEvent::Delta {
                    index: self.index,
                    delta: delta.classify(),
                },
                None => AnthropicEvent::Other,
            },
            "message_delta" => AnthropicEvent::MessageDelta {
                stop_reason: self.delta.and_then(|d| StopReason::parse(&d.stop_reason)),
                output_tokens: self.usage.map(|u| u.output_tokens),
            },
            _ => AnthropicEvent::Other,
        }
    }
}

/// The `message` of a `message_start` event.
#[derive(Deserialize, Default)]
struct EventMessage {
    /// Usage.
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

/// Parsed FROM the start-event block JSON.
#[derive(Deserialize, Default)]
struct ContentBlockStart {
    /// Block type.
    #[serde(default)]
    r#type: String,
    /// Tool-use id.
    #[serde(default)]
    id: String,
    /// Tool name.
    #[serde(default)]
    name: String,
}

/// A delta payload as the wire carries it.
#[derive(Deserialize, Default)]
struct RawDelta {
    /// Delta type.
    #[serde(default)]
    r#type: String,
    /// Text delta.
    #[serde(default)]
    text: String,
    /// Thinking delta.
    #[serde(default)]
    thinking: String,
    /// `signature_delta` payload.
    #[serde(default)]
    signature: String,
    /// Tool-input JSON fragment.
    #[serde(default)]
    partial_json: String,
    /// Stop reason (`message_delta`).
    #[serde(default)]
    stop_reason: String,
}

impl RawDelta {
    /// The classified delta.
    fn classify(self) -> DeltaKind {
        match self.r#type.as_str() {
            "text_delta" => DeltaKind::Text(self.text),
            "thinking_delta" => DeltaKind::Thinking(self.thinking),
            "input_json_delta" => DeltaKind::InputJson(self.partial_json),
            "signature_delta" => DeltaKind::Signature(self.signature),
            _ => DeltaKind::Other,
        }
    }
}

/// One page of `GET /v1/models`.
#[derive(Deserialize)]
pub(crate) struct ModelsPage {
    /// The listed models.
    #[serde(default)]
    pub(crate) data: Vec<ModelEntry>,
    /// Whether another page follows.
    #[serde(default)]
    pub(crate) has_more: bool,
    /// Cursor for the next page.
    #[serde(default)]
    pub(crate) last_id: String,
}

/// One listed model (anthropic.go modelEntry).
#[derive(Deserialize, Default)]
pub(crate) struct ModelEntry {
    /// Model id.
    #[serde(default)]
    pub(crate) id: String,
}

/// The messages endpoint.
pub(crate) struct Anthropic {
    /// The wire client.
    pub(crate) client: Client,
}

impl Anthropic {
    /// POST /v1/messages with stream=false.
    pub(crate) async fn message(
        &self,
        cancel: &CancellationToken,
        req: &mut AnthropicRequest,
    ) -> Result<AnthropicResponse, LlmError> {
        req.stream = false;
        self.client
            .do_json(cancel, Method::POST, PATH_MESSAGES, Some(&*req))
            .await
    }

    /// POST /v1/messages with stream=true.
    pub(crate) async fn stream_message(
        &self,
        cancel: &CancellationToken,
        req: &mut AnthropicRequest,
    ) -> Result<AnthropicStream, LlmError> {
        req.stream = true;
        let sse = self
            .client
            .stream(cancel, Method::POST, PATH_MESSAGES, Some(&*req))
            .await?;
        Ok(AnthropicStream { sse })
    }

    /// GET `/v1/models`, then `/v1/models?after_id=<urlencoded last_id>` while `has_more && last_id != ""`; sorted ids.
    pub(crate) async fn models(&self, cancel: &CancellationToken) -> Result<Vec<String>, LlmError> {
        let mut models: Vec<String> = Vec::new();
        let mut after_id = String::new();
        loop {
            let path = if after_id.is_empty() {
                PATH_MODELS.to_owned()
            } else {
                format!("{PATH_MODELS}?after_id={}", query_escape(&after_id))
            };
            let page: ModelsPage = self.client.get_json(cancel, &path).await?;
            models.extend(page.data.into_iter().map(|m| m.id));
            if !page.has_more || page.last_id.is_empty() {
                break;
            }
            after_id = page.last_id;
        }
        // Go `sort.Strings`: byte-wise ascending.
        models.sort_unstable();
        Ok(models)
    }
}

/// An anthropic SSE stream, classified into `AnthropicEvent`s.
pub(crate) struct AnthropicStream {
    sse: Sse,
}

impl AnthropicStream {
    /// Skips "ping"; event type = JSON `type`, falling back to the SSE `event:` field when empty; type "error" →
    /// `InBand` (error envelope raw JSON if present, else the whole data).
    pub(crate) async fn next(&mut self) -> Result<Option<AnthropicEvent>, LlmError> {
        loop {
            let Some(evt) = self.sse.next().await? else {
                // A body that carried no event at all never streamed (anthropic.go:231-233).
                return if self.sse.saw_event() {
                    Ok(None)
                } else {
                    Err(LlmError::NoEvents)
                };
            };
            let mut out: RawEvent =
                serde_json::from_slice(&evt.data).map_err(LlmError::MalformedEvent)?;
            if out.r#type.is_empty() {
                out.r#type = evt.kind; // fall back to the event: field
            }
            match out.r#type.as_str() {
                "ping" => continue,
                // In-band stream error (e.g. overloaded_error arrives on a 200 stream, not as HTTP 529).
                "error" => {
                    let detail = out.error.as_ref().map_or_else(
                        || String::from_utf8_lossy(&evt.data).into_owned(),
                        |e| e.get().to_owned(),
                    );
                    return Err(LlmError::InBand(detail));
                }
                _ => {}
            }
            return Ok(Some(out.classify()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_error` is emitted even when false; the untagged `Block::Raw` replays verbatim; `stream`/`defer_loading`
    /// disappear when false (anthropic.go:59-98).
    #[test]
    fn request_omitempty_shape() {
        let req = AnthropicRequest {
            model: "m".to_owned(),
            max_tokens: MAX_TOKENS,
            messages: vec![AnthropicMsg {
                role: "user",
                content: vec![
                    Block::Typed(TypedBlock::ToolResult {
                        tool_use_id: "t1".to_owned(),
                        content: vec![TextBlock {
                            r#type: "text",
                            text: "r".to_owned(),
                        }],
                        is_error: false,
                    }),
                    Block::Raw(
                        Raw::from_string(r#"{"type":"server_tool_use"}"#.to_owned()).unwrap(),
                    ),
                ],
            }],
            system: Vec::new(),
            temperature: None,
            top_p: None,
            output_config: None,
            tools: vec![AnthropicToolEntry::Server(ServerTool {
                r#type: SERVER_SEARCH_TOOL_TYPE,
                name: SERVER_SEARCH_TOOL_NAME,
            })],
            stream: false,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m","max_tokens":4096,"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"r"}],"is_error":false},{"type":"server_tool_use"}]}],"tools":[{"type":"tool_search_tool_regex_20251119","name":"tool_search_tool_regex"}]}"#
        );
    }
}
