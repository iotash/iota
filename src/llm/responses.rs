//! Responses dialect (internal/llm/responses.go): request/response/event shapes, `Responses` and its stream.

use crate::llm::chatcomp::OpenAiUsage;
use crate::llm::json::{JsonObject, Raw, is_none_or_empty};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{client::Client, error::LlmError, error::RespFailure, models, sse::Sse};

/// The responses endpoint path.
pub(crate) const PATH_RESPONSES: &str = "/responses";

/// `POST /responses` body.
#[derive(Serialize)]
pub(crate) struct RespRequest {
    /// Model id.
    pub(crate) model: String,
    /// System instructions; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instructions: Option<String>,
    /// Input items; always present (`[]` when empty — DIVERGENCES D-14).
    pub(crate) input: Vec<InputItem>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// Nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f64>,
    /// Reasoning effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<RespReasoning>,
    /// Tools; empty omitted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolEntry>,
    /// Streaming flag; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stream: bool,
}

/// One input item.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum InputItem {
    /// A role message.
    Msg(RespMsg),
    /// A replayed function call.
    FunctionCall(RespFunctionCall),
    /// A function call output.
    FunctionCallOutput(RespFunctionCallOutput),
    /// A raw output item of a previous round, replayed verbatim.
    Raw(Raw),
}

/// A role message.
#[derive(Serialize)]
pub(crate) struct RespMsg {
    /// Role string.
    pub(crate) role: &'static str,
    /// Content.
    pub(crate) content: RespContent,
}

/// Message content: a plain string or a parts array.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum RespContent {
    /// Plain text.
    Text(String),
    /// Multi-part content.
    Parts(Vec<RespPart>),
}

/// One content part.
#[derive(Serialize)]
#[serde(tag = "type")]
pub(crate) enum RespPart {
    /// `input_text`.
    #[serde(rename = "input_text")]
    Text {
        /// The text.
        text: String,
    },
    /// `input_image`.
    #[serde(rename = "input_image")]
    Image {
        /// Data URL.
        image_url: String,
        /// Always `"auto"`.
        detail: &'static str,
    },
    /// `input_file`.
    #[serde(rename = "input_file")]
    File {
        /// Data URL.
        file_data: String,
        /// File name.
        filename: String,
    },
}

/// A replayed `function_call` item.
#[derive(Serialize)]
pub(crate) struct RespFunctionCall {
    /// Always `"function_call"`.
    pub(crate) r#type: &'static str,
    /// Call id.
    pub(crate) call_id: String,
    /// Function name.
    pub(crate) name: String,
    /// JSON-encoded arguments.
    pub(crate) arguments: String,
}

/// A `function_call_output` item.
#[derive(Serialize)]
pub(crate) struct RespFunctionCallOutput {
    /// Always `"function_call_output"`.
    pub(crate) r#type: &'static str,
    /// Call id.
    pub(crate) call_id: String,
    /// Tool output text.
    pub(crate) output: String,
}

/// A function tool.
#[derive(Serialize, Clone)]
pub(crate) struct RespTool {
    /// Always `"function"`.
    pub(crate) r#type: &'static str,
    /// Function name.
    pub(crate) name: String,
    /// Description; always emitted.
    pub(crate) description: String,
    /// JSON Schema; `None` and the empty map omitted (Go `omitempty`).
    #[serde(skip_serializing_if = "is_none_or_empty")]
    pub(crate) parameters: Option<JsonObject>,
    /// Always emitted, always `false`.
    pub(crate) strict: bool,
    /// Deferred loading; `false` omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) defer_loading: bool,
}

/// A built-in tool (e.g. `image_generation`).
#[derive(Serialize)]
pub(crate) struct RespBuiltinTool {
    /// Tool type.
    pub(crate) r#type: &'static str,
    /// Partial image frames; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) partial_images: Option<u32>,
}

/// `reasoning`.
#[derive(Serialize)]
pub(crate) struct RespReasoning {
    /// Effort string.
    pub(crate) effort: String,
}

/// The client-executed `tool_search` tool.
#[derive(Serialize)]
pub(crate) struct RespToolSearch {
    /// Always `"tool_search"`.
    pub(crate) r#type: &'static str,
    /// Always `"client"`.
    pub(crate) execution: &'static str,
    /// Description; `""` omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    /// Parameters schema; `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<JsonObject>,
}

/// A `tool_search_output` item.
#[derive(Serialize)]
pub(crate) struct RespToolSearchOutput {
    /// Always `"tool_search_output"`.
    pub(crate) r#type: &'static str,
    /// Call id.
    pub(crate) call_id: String,
    /// Always `"completed"`.
    pub(crate) status: &'static str,
    /// Always `"client"`.
    pub(crate) execution: &'static str,
    /// The found tools; NO skip: `[]` when empty (Go null; POLICY 5 `"tools":null` → emit `[]`).
    pub(crate) tools: Vec<RespTool>,
}

/// One entry of the `tools` array.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ToolEntry {
    /// A function tool.
    Function(RespTool),
    /// A built-in tool.
    Builtin(RespBuiltinTool),
    /// The tool-search tool.
    ToolSearch(RespToolSearch),
}

/// A response-level error object.
#[derive(Deserialize, Default, Clone)]
pub(crate) struct RespError {
    /// Error code.
    #[serde(default)]
    pub(crate) code: String,
    /// Error message.
    #[serde(default)]
    pub(crate) message: String,
}

/// A response object (unary body or the `response` of a terminal event).
#[derive(Deserialize, Default)]
pub(crate) struct RespResponse {
    /// Output items.
    #[serde(default)]
    pub(crate) output: Vec<RespOutput>,
    /// Usage, when reported.
    #[serde(default)]
    pub(crate) usage: Option<OpenAiUsage>,
    /// Error object.
    #[serde(default)]
    pub(crate) error: Option<RespError>,
    /// Incomplete details.
    #[serde(default)]
    pub(crate) incomplete_details: Option<RespIncomplete>,
}

/// `incomplete_details`.
#[derive(Deserialize, Default)]
pub(crate) struct RespIncomplete {
    /// Reason.
    #[serde(default)]
    pub(crate) reason: String,
}

/// One output item of a response object.
#[derive(Deserialize, Default)]
pub(crate) struct RespOutput {
    /// Item type.
    #[serde(default)]
    pub(crate) r#type: String,
    /// Image generation result (base64).
    #[serde(default)]
    pub(crate) result: String,
    /// Image output format.
    #[serde(default)]
    pub(crate) output_format: String,
    /// Message content parts.
    #[serde(default)]
    pub(crate) content: Vec<RespOutputContent>,
}

/// One content part of an output item.
#[derive(Deserialize, Default)]
pub(crate) struct RespOutputContent {
    /// Part type (`output_text`, `refusal`, …).
    #[serde(default)]
    pub(crate) r#type: String,
    /// Text.
    #[serde(default)]
    pub(crate) text: String,
}

impl RespOutput {
    /// The item's class.
    pub(crate) fn kind(&self) -> ItemKind {
        ItemKind::parse(&self.r#type)
    }
}

impl RespResponse {
    /// Concat of every content part with type `output_text` across items.
    ///
    /// SDK `Response.OutputText` parity (responses.go:165-175): refusal parts are skipped, item order is kept.
    pub(crate) fn output_text(&self) -> String {
        let mut out = String::new();
        for item in &self.output {
            for part in &item.content {
                if part.r#type == "output_text" {
                    out.push_str(&part.text);
                }
            }
        }
        out
    }
}

/// One stream event, classified once here so the dialect matches on shapes, never on strings
/// (responses.go:245-300). Terminal failures never reach this enum: `next()` turns them into
/// `LlmError::Failure` / `LlmError::InBand`.
pub(crate) enum RespEvent {
    /// `response.reasoning_summary_text.delta`.
    ReasoningDelta(String),
    /// `response.output_text.delta` — and a type-less frame that carries a `delta` (some relays).
    TextDelta(String),
    /// `response.image_generation_call.in_progress` / `.generating`: generation started.
    ImageGenerating,
    /// `response.image_generation_call.partial_image`: one refining frame, standard-padded base64
    /// (responses.go:185).
    ImagePartial(String),
    /// `response.output_item.added`: the item, verbatim.
    ItemAdded(Raw),
    /// `response.function_call_arguments.delta`.
    ArgumentsDelta {
        /// The item the fragment belongs to.
        item_id: String,
        /// The arguments fragment.
        delta: String,
    },
    /// `response.function_call_arguments.done`: the complete arguments.
    ArgumentsDone {
        /// The item the arguments belong to.
        item_id: String,
        /// The complete arguments JSON.
        arguments: String,
    },
    /// `response.output_item.done`: the item, verbatim.
    ItemDone(Raw),
    /// `response.completed`: the response object (usage lives there).
    Completed(Option<RespResponse>),
    /// Anything else — nothing to act on.
    Other,
}

/// An output item's class (`type`), shared by the streamed item and the unary response's items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    /// `message`.
    Message,
    /// `reasoning`.
    Reasoning,
    /// `function_call` — a client tool call.
    FunctionCall,
    /// `image_generation_call`.
    ImageGenerationCall,
    /// `tool_search_call` — a client-executed search leg.
    ToolSearchCall,
    /// Anything else, by name.
    Other(String),
}

impl ItemKind {
    /// The class for a `type` string.
    pub(crate) fn parse(kind: &str) -> Self {
        match kind {
            "message" => Self::Message,
            "reasoning" => Self::Reasoning,
            "function_call" => Self::FunctionCall,
            "image_generation_call" => Self::ImageGenerationCall,
            "tool_search_call" => Self::ToolSearchCall,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The class of a stored item, read off its own `"type"`; `None` when the blob does not parse.
    pub(crate) fn of_raw(raw: &Raw) -> Option<Self> {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(default)]
            r#type: String,
        }
        serde_json::from_str::<Probe>(raw.get())
            .ok()
            .map(|p| Self::parse(&p.r#type))
    }
}

/// The event as the wire carries it: `type` plus every field any event type has.
#[derive(Deserialize, Default)]
struct RawRespEvent {
    /// Event type.
    #[serde(default)]
    r#type: String,
    /// Text delta.
    #[serde(default)]
    delta: String,
    /// Item id.
    #[serde(default)]
    item_id: String,
    /// Complete arguments (`function_call_arguments.done`).
    #[serde(default)]
    arguments: String,
    /// The output item (`output_item.added` / `.done`), verbatim.
    #[serde(default)]
    item: Option<Raw>,
    /// The response object (terminal events).
    #[serde(default)]
    response: Option<RespResponse>,
    /// Error code (type `error`).
    #[serde(default)]
    code: String,
    /// Error message (type `error`).
    #[serde(default)]
    message: String,
    /// In-band error; `null` is ABSENT.
    #[serde(default)]
    error: Option<Raw>,
    /// One progressive image frame, standard-padded base64
    /// (`response.image_generation_call.partial_image`; responses.go:185).
    #[serde(default)]
    partial_image_b64: String,
}

impl RawRespEvent {
    /// The classified event.
    fn classify(self) -> RespEvent {
        match self.r#type.as_str() {
            "response.reasoning_summary_text.delta" => RespEvent::ReasoningDelta(self.delta),
            "response.output_text.delta" => RespEvent::TextDelta(self.delta),
            "response.image_generation_call.in_progress"
            | "response.image_generation_call.generating" => RespEvent::ImageGenerating,
            "response.image_generation_call.partial_image" => {
                RespEvent::ImagePartial(self.partial_image_b64)
            }
            "response.output_item.added" => {
                self.item.map_or(RespEvent::Other, RespEvent::ItemAdded)
            }
            "response.function_call_arguments.delta" => RespEvent::ArgumentsDelta {
                item_id: self.item_id,
                delta: self.delta,
            },
            "response.function_call_arguments.done" => RespEvent::ArgumentsDone {
                item_id: self.item_id,
                arguments: self.arguments,
            },
            "response.output_item.done" => self.item.map_or(RespEvent::Other, RespEvent::ItemDone),
            "response.completed" => RespEvent::Completed(self.response),
            // Some relays emit type-less delta frames; they are content.
            "" if !self.delta.is_empty() => RespEvent::TextDelta(self.delta),
            _ => RespEvent::Other,
        }
    }
}

/// An output item parsed from `RespEvent::item`.
#[derive(Deserialize, Default)]
pub(crate) struct RespOutputItem {
    /// Item id.
    #[serde(default, deserialize_with = "crate::llm::none_if_empty")]
    pub(crate) id: Option<String>,
    /// Item type.
    #[serde(default)]
    pub(crate) r#type: String,
    /// Call id.
    #[serde(default, deserialize_with = "crate::llm::none_if_empty")]
    pub(crate) call_id: Option<String>,
    /// Function name.
    #[serde(default, deserialize_with = "crate::llm::none_if_empty")]
    pub(crate) name: Option<String>,
    /// Arguments (a JSON string, or anything else).
    #[serde(default)]
    pub(crate) arguments: Option<serde_json::Value>,
    /// Image generation result (base64).
    #[serde(default)]
    pub(crate) result: String,
    /// Tool-search query.
    #[serde(default)]
    pub(crate) query: String,
    /// Image output format.
    #[serde(default)]
    pub(crate) output_format: String,
}

impl RespOutputItem {
    /// The item's class.
    pub(crate) fn kind(&self) -> ItemKind {
        ItemKind::parse(&self.r#type)
    }

    /// The string when `arguments` is a JSON string, else "".
    ///
    /// responses.go:215-221 (`OfString` parity): some gateways send an OBJECT instead, and callers then fall
    /// back to the delta-accumulated arguments.
    pub(crate) fn arguments_string(&self) -> String {
        match &self.arguments {
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => String::new(),
        }
    }
}

impl RawRespEvent {
    /// Terminal failure events → `RespFailure` (responses.go:301-319); everything else → `None`.
    fn failure(&self) -> Option<RespFailure> {
        match self.r#type.as_str() {
            "error" => Some(RespFailure {
                event: self.r#type.clone(),
                code: self.code.clone(),
                message: self.message.clone(),
            }),
            "response.failed" => {
                let err = self.response.as_ref().and_then(|r| r.error.as_ref());
                Some(RespFailure {
                    event: self.r#type.clone(),
                    code: err.map(|e| e.code.clone()).unwrap_or_default(),
                    message: err.map(|e| e.message.clone()).unwrap_or_default(),
                })
            }
            "response.incomplete" => Some(RespFailure {
                event: self.r#type.clone(),
                code: self
                    .response
                    .as_ref()
                    .and_then(|r| r.incomplete_details.as_ref())
                    .map(|d| d.reason.clone())
                    .unwrap_or_default(),
                message: String::new(),
            }),
            _ => None,
        }
    }
}

/// The responses endpoint.
///
/// The endpoint sends no `[DONE]` sentinel: a stream ends at EOF after a terminal event
/// (`response.completed` on success), and events dispatch on the `type` field INSIDE the data JSON — the SSE
/// `event:` line is redundant and ignored.
pub(crate) struct Responses {
    /// The wire client.
    pub(crate) client: Client,
}

impl Responses {
    /// POST /responses with stream=false.
    pub(crate) async fn create(
        &self,
        cancel: &CancellationToken,
        req: &mut RespRequest,
    ) -> Result<RespResponse, LlmError> {
        req.stream = false;
        self.client
            .do_json(cancel, Method::POST, PATH_RESPONSES, Some(&*req))
            .await
    }

    /// POST /responses with stream=true.
    pub(crate) async fn stream_response(
        &self,
        cancel: &CancellationToken,
        req: &mut RespRequest,
    ) -> Result<RespStream, LlmError> {
        req.stream = true;
        let sse = self
            .client
            .stream(cancel, Method::POST, PATH_RESPONSES, Some(&*req))
            .await?;
        Ok(RespStream { sse })
    }

    /// = `models::openai_model_ids` (§3.8.0).
    pub(crate) async fn models(&self, cancel: &CancellationToken) -> Result<Vec<String>, LlmError> {
        models::openai_model_ids(&self.client, cancel).await
    }
}

/// A responses SSE stream of `RespEvent`s.
pub(crate) struct RespStream {
    sse: Sse,
}

impl RespStream {
    /// Next event. Terminal mapping: data `error` non-null → `InBand`; type `"error"` →
    /// `Failure(RespFailure{event:"error", code, message})`; `"response.failed"` → `Failure{event, code:
    /// response.error.code, message: response.error.message}`; `"response.incomplete"` → `Failure{event, code:
    /// response.incomplete_details.reason, message: ""}`.
    pub(crate) async fn next(&mut self) -> Result<Option<RespEvent>, LlmError> {
        let Some(event) = self.sse.next().await? else {
            // A stream that ended without a single event never streamed at all (client.go:47).
            return if self.sse.saw_event() {
                Ok(None)
            } else {
                Err(LlmError::NoEvents)
            };
        };
        let parsed: RawRespEvent =
            serde_json::from_slice(&event.data).map_err(LlmError::MalformedEvent)?;
        // A literal `"error": null` is ABSENT (POLICY F-05): `Option<Raw>` decodes it to `None`.
        if let Some(err) = &parsed.error {
            return Err(LlmError::InBand(err.get().to_owned()));
        }
        if let Some(failure) = parsed.failure() {
            return Err(LlmError::Failure(failure));
        }
        Ok(Some(parsed.classify()))
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tokio_util::sync::CancellationToken;

    use super::{LlmError, RawRespEvent, RespEvent, RespOutputItem, RespResponse, RespStream, Sse};

    /// A `RespStream` over an in-memory body delivered as one chunk.
    fn stream_of(raw: &'static str) -> RespStream {
        RespStream {
            sse: Sse::new(
                futures::stream::iter([Ok(Bytes::from_static(raw.as_bytes()))]),
                CancellationToken::new(),
            ),
        }
    }

    /// The error `raw` yields (`RespEvent` is not `Debug`, so `unwrap_err` is unavailable here).
    async fn err_of(raw: &'static str) -> LlmError {
        match stream_of(raw).next().await {
            Err(e) => e,
            Ok(_) => panic!("{raw:?} did not fail"),
        }
    }

    #[tokio::test]
    async fn next_maps_terminal_events_and_treats_a_null_error_as_absent() {
        // POLICY F-05: a literal `"error": null` is ABSENT, not an in-band failure.
        let mut s = stream_of(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\",\"error\":null}\n\n",
        );
        assert!(matches!(s.next().await.unwrap().unwrap(), RespEvent::TextDelta(d) if d == "hi"));
        assert!(s.next().await.unwrap().is_none(), "clean end after EOF");

        // A non-null envelope IS an in-band failure, carrying its raw JSON.
        match err_of("data: {\"type\":\"x\",\"error\":{\"message\":\"boom\"}}\n\n").await {
            LlmError::InBand(raw) => assert_eq!(raw, r#"{"message":"boom"}"#),
            other => panic!("{other:?}"),
        }

        // Each terminal event becomes a structured failure with the Go Display text.
        for (raw, want) in [
            (
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"model exploded\"}}}\n\n",
                "response.failed: model exploded (server_error)",
            ),
            (
                "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
                "response.incomplete: max_output_tokens",
            ),
            (
                "data: {\"type\":\"error\",\"code\":\"ERR_UPSTREAM\",\"message\":\"bad stream\"}\n\n",
                "error: bad stream (ERR_UPSTREAM)",
            ),
        ] {
            match err_of(raw).await {
                LlmError::Failure(f) => assert_eq!(f.to_string(), want),
                other => panic!("{raw}: {other:?}"),
            }
        }

        // Malformed JSON and a body that never streamed.
        assert!(matches!(
            err_of("data: not json\n\n").await,
            LlmError::MalformedEvent(_)
        ));
        assert!(matches!(err_of("").await, LlmError::NoEvents));
        // The `event:` line is ignored: dispatch is on the data JSON's own `type`.
        let mut s = stream_of("event: ignored\ndata: {\"type\":\"response.completed\"}\n\n");
        assert!(matches!(
            s.next().await.unwrap().unwrap(),
            RespEvent::Completed(None)
        ));
    }

    #[test]
    fn output_text_skips_refusals_across_items() {
        let resp: RespResponse = serde_json::from_str(
            r#"{"status":"completed","output":[
                {"id":"rs_1","type":"reasoning","summary":[]},
                {"id":"msg_1","type":"message","content":[
                    {"type":"output_text","text":"Hello"},
                    {"type":"refusal","refusal":"no"},
                    {"type":"output_text","text":" world"}]}]}"#,
        )
        .unwrap();
        assert_eq!(resp.output_text(), "Hello world");
        assert_eq!(RespResponse::default().output_text(), "");
    }

    #[test]
    fn arguments_string_only_unwraps_json_strings() {
        let string: RespOutputItem =
            serde_json::from_str(r#"{"type":"function_call","arguments":"{\"q\":\"x\"}"}"#)
                .unwrap();
        assert_eq!(string.arguments_string(), r#"{"q":"x"}"#);
        // Object-form arguments (some gateways) fall back to delta-accumulated args.
        let object: RespOutputItem =
            serde_json::from_str(r#"{"type":"function_call","arguments":{"q":"x"}}"#).unwrap();
        assert_eq!(object.arguments_string(), "");
        let absent: RespOutputItem = serde_json::from_str(r#"{"type":"function_call"}"#).unwrap();
        assert_eq!(absent.arguments_string(), "");
    }

    #[test]
    fn failure_maps_the_three_terminal_events() {
        let failed: RawRespEvent = serde_json::from_str(
            r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"model exploded"}}}"#,
        )
        .unwrap();
        let f = failed.failure().unwrap();
        assert_eq!(
            (f.event.as_str(), f.code.as_str(), f.message.as_str()),
            ("response.failed", "server_error", "model exploded")
        );

        let incomplete: RawRespEvent = serde_json::from_str(
            r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
        )
        .unwrap();
        let f = incomplete.failure().unwrap();
        assert_eq!(f.code, "max_output_tokens");
        assert_eq!(f.message, "");

        let error: RawRespEvent =
            serde_json::from_str(r#"{"type":"error","code":"ERR","message":"bad","param":null}"#)
                .unwrap();
        let f = error.failure().unwrap();
        assert_eq!(
            (f.event.as_str(), f.code.as_str(), f.message.as_str()),
            ("error", "ERR", "bad")
        );

        // A bare response.failed with no response object carries no detail at all.
        let bare: RawRespEvent = serde_json::from_str(r#"{"type":"response.failed"}"#).unwrap();
        assert_eq!(
            bare.failure().unwrap().to_string(),
            "response.failed: no detail provided"
        );
        let bare: RawRespEvent = serde_json::from_str(r#"{"type":"response.incomplete"}"#).unwrap();
        assert_eq!(
            bare.failure().unwrap().to_string(),
            "response.incomplete: no detail provided"
        );

        for ok in [
            r#"{"type":"response.completed"}"#,
            r#"{"type":"response.output_text.delta","delta":"x"}"#,
            r#"{"type":""}"#,
        ] {
            let event: RawRespEvent = serde_json::from_str(ok).unwrap();
            assert!(event.failure().is_none(), "{ok}");
        }
    }
}
