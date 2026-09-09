//! The `openresponses` provider (provider/openresponses.go) on the responses dialect, including the 4-leg
//! tool-search protocol and image outputs.

use std::collections::HashMap;

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{
    Attachment, JsonObject, Message, Raw, RawContent, Role, ToolCall, ToolDef,
};
use crate::provider::sink::{ReasoningGate, StreamSink};
use crate::provider::usage::Usage;
use crate::provider::{
    ChatResult, HttpTransport, ImageTunable, Provider, ProviderKind, RoundResult, ToolProvider,
    ToolSearchHost, ToolSearcher, TopPTunable, Tunable,
};
use reqwest::header::AUTHORIZATION;
use tokio_util::sync::CancellationToken;

use crate::llm::error::LlmError;
use crate::llm::responses::{
    InputItem, ItemKind, RespBuiltinTool, RespContent, RespEvent, RespFunctionCall,
    RespFunctionCallOutput, RespMsg, RespOutputItem, RespPart, RespReasoning, RespRequest,
    RespTool, RespToolSearch, RespToolSearchOutput, Responses, ToolEntry,
};
use crate::provider::common::{
    HasCore, OPENAI_DEFAULT_BASE_URL, ProviderCore, b64, b64_decode, credential_header, data_url,
    make_client,
};
use crate::provider::think::{StreamThinkSplitter, split_inline_think};
use crate::provider::usage_conv::openai_usage;

/// Description of the client-executed `tool_search` tool.
pub(crate) const TOOL_SEARCH_DESCRIPTION: &str =
    "Search and load additional deferred tools by capability keywords before first use.";
/// Description of the tool-search `query` parameter.
pub(crate) const TOOL_SEARCH_QUERY_DESCRIPTION: &str = "Capability keywords";
/// Maximum tool-search legs per round.
pub(crate) const MAX_SEARCH_LEGS: usize = 4;

/// The name the server-side image built-in composes under, so the interactive lifecycle
/// widget can label it (openresponses.go:332).
const IMAGE_GENERATION: &str = "image_generation";
/// The stand-in "delta" for a composing event that carries no argument bytes of its own —
/// an announcement, not a fragment (openresponses.go:332,349). It must be NON-EMPTY: an
/// empty delta is the atomic-backend signal the observer ignores (`TUI_CONTRACTS` §4).
const COMPOSING_TICK: &str = "…";

/// Responses provider (tool, tunable, `top_p`, `image_tunable`, `tool_search_host`; `RawContent::OpenResponses`).
pub struct OpenResponsesProvider {
    core: ProviderCore,
    client: Responses,
    image_output: bool,
    searcher: Option<ToolSearcher>,
}

/// One accumulating function call, keyed by `call_id` (never the item id: Bedrock-backed gateways collapse
/// parallel calls into one output item, reusing its `id` while giving each a distinct `call_id`).
struct FnCallAcc {
    name: String,
    args: String,
}

/// A `tool_search_call` this leg produced, to be answered client-side before the next leg.
struct SearchCall {
    call_id: String,
    query: String,
}

/// The `tool_search` parameters schema (openresponses.go:244-250).
fn tool_search_parameters() -> JsonObject {
    let mut query = JsonObject::new();
    query.insert("type".to_owned(), "string".into());
    query.insert(
        "description".to_owned(),
        TOOL_SEARCH_QUERY_DESCRIPTION.into(),
    );
    let mut properties = JsonObject::new();
    properties.insert("query".to_owned(), query.into());
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), "object".into());
    schema.insert("properties".to_owned(), properties.into());
    schema.insert("required".to_owned(), serde_json::json!(["query"]));
    schema
}

/// `image/<output_format>`, or `image/png` when the format is absent (openresponses.go:202-204,369-372).
fn image_mime(output_format: &str) -> String {
    if output_format.is_empty() {
        "image/png".to_owned()
    } else {
        format!("image/{output_format}")
    }
}

impl OpenResponsesProvider {
    /// base "" → `OPENAI_DEFAULT_BASE_URL`; header `Authorization: Bearer <key>`.
    pub fn new(
        api_key: &str,
        base_url: &str,
        model: &str,
        temperature: Option<f64>,
        http: impl Into<HttpTransport>,
    ) -> Self {
        let client = make_client(base_url, OPENAI_DEFAULT_BASE_URL, Some(http.into())).with_header(
            AUTHORIZATION,
            credential_header(&format!("Bearer {api_key}")),
        );
        Self {
            core: ProviderCore {
                kind: ProviderKind::OpenResponses,
                model: model.to_owned(),
                temperature,
                top_p: None,
                effort: None,
            },
            client: Responses { client },
            image_output: false,
            searcher: None,
        }
    }

    /// openresponses.go:86-184. System prompts ride in `instructions` (last wins; a system-tools mount is
    /// skipped entirely), user attachments become `input_image`/`input_file` parts with the text part LAST,
    /// assistant tool rounds replay their raw items verbatim, and the image built-in leads the tools array.
    fn build_request(&self, messages: &[Message]) -> RespRequest {
        let mut req = RespRequest {
            model: self.core.model.clone(),
            instructions: None,
            input: Vec::new(),
            temperature: self.core.temperature,
            top_p: self.core.top_p,
            reasoning: self.core.effort.map(|e| RespReasoning {
                effort: e.as_str().to_owned(),
            }),
            tools: Vec::new(),
            stream: false,
        };
        for msg in messages {
            match msg.role() {
                Role::System => {
                    if msg.is_tools_mount() {
                        continue; // a system-tools mount (K3 wire shape): chatcomp-only, skip here
                    }
                    req.instructions = Some(msg.content.clone());
                }
                Role::User => req.input.push(InputItem::Msg(RespMsg {
                    role: "user",
                    content: if msg.attachments.is_empty() {
                        RespContent::Text(msg.content.clone())
                    } else {
                        RespContent::Parts(Self::user_parts(msg))
                    },
                })),
                Role::Assistant => Self::push_assistant(&mut req, msg),
                Role::Tool => {
                    let output = RespFunctionCallOutput {
                        r#type: "function_call_output",
                        call_id: msg.tool_call_id().to_owned(),
                        output: msg.content.clone(),
                    };
                    req.input.push(InputItem::FunctionCallOutput(output));
                }
            }
        }
        if self.image_output {
            // The image switch advertises the server-side built-in on EVERY request path.
            req.tools.push(ToolEntry::Builtin(RespBuiltinTool {
                r#type: "image_generation",
                partial_images: Some(1),
            }));
        }
        req
    }

    /// Attachment parts in order, then the text part LAST (openresponses.go:106-120).
    fn user_parts(msg: &Message) -> Vec<RespPart> {
        let mut parts: Vec<RespPart> = msg
            .attachments
            .iter()
            .map(|att| {
                if att.is_image() {
                    RespPart::Image {
                        image_url: data_url(&att.mime_type, &att.data),
                        detail: "auto",
                    }
                } else {
                    // file_data is BARE base64, never a data URL.
                    RespPart::File {
                        file_data: b64(&att.data),
                        filename: att.filename.clone(),
                    }
                }
            })
            .collect();
        parts.push(RespPart::Text {
            text: msg.content.clone(),
        });
        parts
    }

    /// openresponses.go:121-173: a tool round replays this dialect's raw items (message items skipped,
    /// `function_call` ids stripped), any other raw content falls back to reconstruction.
    fn push_assistant(req: &mut RespRequest, msg: &Message) {
        if msg.tool_calls().is_empty() {
            req.input.push(InputItem::Msg(RespMsg {
                role: "assistant",
                content: RespContent::Text(msg.content.clone()),
            }));
            return;
        }
        if let Some(RawContent::OpenResponses(items)) = msg.raw_content() {
            for item in items {
                if let Some(replayed) = Self::replay(item) {
                    req.input.push(replayed);
                }
            }
            return;
        }
        if !msg.content.is_empty() {
            req.input.push(InputItem::Msg(RespMsg {
                role: "assistant",
                content: RespContent::Text(msg.content.clone()),
            }));
        }
        for call in msg.tool_calls() {
            req.input.push(InputItem::FunctionCall(RespFunctionCall {
                r#type: "function_call",
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
            }));
        }
    }

    /// One replayed raw item, or `None` when it must be dropped. `message` items are SKIPPED (some APIs reject
    /// them on replay); `function_call` items lose their `id`: `OpenAI` validates the `fc`-prefix, Bedrock
    /// gateways reuse one id across parallel calls, and blobs persisted by older builds carry a
    /// `call_`-prefixed rewrite — `call_id` alone pairs the call with its `function_call_output`. Anything that
    /// does not parse, and every other item type, replays byte-verbatim.
    fn replay(item: &Raw) -> Option<InputItem> {
        match ItemKind::of_raw(item) {
            Some(ItemKind::Message) => None,
            Some(ItemKind::FunctionCall) => {
                if let Ok(mut obj) = serde_json::from_str::<JsonObject>(item.get()) {
                    obj.remove("id");
                    if let Ok(reencoded) = Raw::from_value(&obj) {
                        return Some(InputItem::Raw(reencoded));
                    }
                }
                Some(InputItem::Raw(item.clone()))
            }
            _ => Some(InputItem::Raw(item.clone())),
        }
    }

    /// One streaming round, including up to `MAX_SEARCH_LEGS` client-executed tool-search legs
    /// (openresponses.go:222-514). Search legs are invisible upstream: they continue ONE logical response.
    async fn stream_internal(
        &self,
        cancel: &CancellationToken,
        messages: &[Message],
        tools: &[ToolDef],
        sink: &mut dyn StreamSink,
    ) -> Result<RoundResult, ProviderError> {
        let mut req = self.build_request(messages);
        let mut any_deferred = false;
        for tool in tools {
            any_deferred |= tool.deferred;
            req.tools.push(ToolEntry::Function(RespTool {
                r#type: "function",
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
                strict: false,
                defer_loading: tool.deferred && self.searcher.is_some(),
            }));
        }
        if any_deferred && self.searcher.is_some() {
            // defer_mode tool-search: the model searches, we answer (client execution) inside this call.
            req.tools.push(ToolEntry::ToolSearch(RespToolSearch {
                r#type: "tool_search",
                execution: "client",
                description: TOOL_SEARCH_DESCRIPTION.to_owned(),
                parameters: Some(tool_search_parameters()),
            }));
        }

        let mut gate = ReasoningGate::new(sink);
        let mut split = StreamThinkSplitter::new();
        let mut think_full = String::new();
        // item_id → arguments accumulated from deltas, flushed on output_item.done.
        let mut pending_args: HashMap<String, String> = HashMap::new();
        // item_id → function name, learned from `output_item.added` (the name arrives ONCE,
        // before any argument delta) so the composing observer can name each fragment.
        let mut item_names: HashMap<String, String> = HashMap::new();
        let mut fn_calls: HashMap<String, FnCallAcc> = HashMap::new();
        let mut fn_call_order: Vec<String> = Vec::new();
        let mut raw_items: Vec<Raw> = Vec::new();
        let mut images: Vec<Attachment> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut stream_err: Option<LlmError> = None;

        for _leg in 0..MAX_SEARCH_LEGS {
            let mut leg_searches: Vec<SearchCall> = Vec::new();
            let leg_start = raw_items.len();
            let mut stream = self
                .client
                .stream_response(cancel, &mut req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Stream, e))?;
            loop {
                let event = match stream.next().await {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                };
                match event {
                    RespEvent::ReasoningDelta(delta) => {
                        gate.reasoning(&delta);
                        think_full.push_str(&delta);
                    }
                    RespEvent::TextDelta(delta) => split.write(&delta, &mut gate),
                    RespEvent::ImageGenerating => {
                        // Generation started: raise the lifecycle widget through the composing
                        // observer, the same channel function calls use (openresponses.go:330-332).
                        gate.close();
                        gate.tool_delta(Some(IMAGE_GENERATION), COMPOSING_TICK);
                    }
                    RespEvent::ImagePartial(b64) => {
                        // One refining thumbnail: decoded straight through the gate, which hands
                        // it to the widget body (openresponses.go:333-338). An unparseable or
                        // empty frame is dropped — a preview is never worth an error.
                        if !b64.is_empty()
                            && let Ok(data) = b64_decode(&b64)
                            && !data.is_empty()
                        {
                            gate.image_partial(&data);
                        }
                    }
                    RespEvent::ItemAdded(item) => {
                        // The function's name arrives HERE, once, before any argument delta.
                        if let Ok(added) = serde_json::from_str::<RespOutputItem>(item.get())
                            && added.kind() == ItemKind::FunctionCall
                            && let Some(name) = added.name
                        {
                            gate.close();
                            // Raise the widget on the announcement itself: a call whose arguments
                            // are still empty is already work in progress, and waiting for the
                            // first delta leaves the gap this event exists to close
                            // (openresponses.go:341-351).
                            gate.tool_delta(Some(&name), COMPOSING_TICK);
                            item_names.insert(added.id.unwrap_or_default(), name);
                        }
                    }
                    RespEvent::ArgumentsDelta { item_id, delta } => {
                        gate.close(); // thinking is over once tool args stream
                        // An item whose `output_item.added` never named it stays anonymous: the
                        // delta still cuts the content stream, but raises no widget (T-11).
                        gate.tool_delta(item_names.get(&item_id).map(String::as_str), &delta);
                        pending_args.entry(item_id).or_default().push_str(&delta);
                    }
                    RespEvent::ArgumentsDone { item_id, arguments } => {
                        pending_args.insert(item_id, arguments);
                    }
                    RespEvent::ItemDone(item) => Self::on_item_done(
                        &item,
                        &mut ItemSinks {
                            raw_items: &mut raw_items,
                            images: &mut images,
                            leg_searches: &mut leg_searches,
                            pending_args: &pending_args,
                            fn_calls: &mut fn_calls,
                            fn_call_order: &mut fn_call_order,
                        },
                    ),
                    RespEvent::Completed(response) => {
                        // A completed response without a usage block reports nothing rather than an all-zero
                        // call: zeros would read as a real (free) call to the accounting.
                        if let Some(u) = response.as_ref().and_then(|r| r.usage.as_ref()) {
                            usage = Some(openai_usage(u));
                        }
                    }
                    RespEvent::Other => {}
                }
            }
            drop(stream);
            let Some(searcher) = self.searcher.as_ref() else {
                break;
            };
            if leg_searches.is_empty() || stream_err.is_some() {
                break;
            }
            // Replay EVERYTHING this leg produced, in arrival order: gpt-5.x pairs its reasoning items with the
            // tool_search_call and rejects the call replayed without them.
            for item in &raw_items[leg_start..] {
                req.input.push(InputItem::Raw(item.clone()));
            }
            for search in leg_searches {
                let out = RespToolSearchOutput {
                    r#type: "tool_search_output",
                    call_id: search.call_id,
                    status: "completed",
                    execution: "client",
                    tools: searcher(&search.query)
                        .into_iter()
                        .map(|hit| RespTool {
                            r#type: "function",
                            name: hit.name,
                            description: hit.description,
                            parameters: hit.input_schema,
                            strict: false,
                            defer_loading: true,
                        })
                        .collect(),
                };
                // The answer joins the request input AND the raw replay blob: later rounds must carry the pair
                // so mounted tools stay loaded (they live in history, not the array).
                if let Ok(raw) = Raw::from_value(&out) {
                    req.input.push(InputItem::Raw(raw.clone()));
                    raw_items.push(raw);
                }
            }
        }

        split.flush(&mut gate);
        gate.close();
        drop(gate);
        let content = split.content;
        let reasoning = think_full + &split.think;
        if let Some(err) = stream_err
            && content.is_empty()
            && reasoning.is_empty()
            && fn_calls.is_empty()
        {
            // Nothing was received: surface the failure. Otherwise the partial result stands.
            return Err(ProviderError::wire(WireOp::Stream, err));
        }

        if fn_calls.is_empty() {
            // An image round also keeps its raw items: the image_generation_call id (payload stripped) carries
            // the server-side multiturn context.
            let raw_content = (!images.is_empty() && !raw_items.is_empty())
                .then_some(RawContent::OpenResponses(raw_items));
            return Ok(RoundResult {
                content,
                reasoning,
                tool_calls: Vec::new(),
                usage,
                raw_content,
                images,
            });
        }

        let tool_calls = fn_call_order
            .into_iter()
            .filter_map(|call_id| {
                let acc = fn_calls.remove(&call_id)?;
                // An empty or unparseable argument string yields an EMPTY (non-nil) map — Go parity.
                let arguments = if acc.args.is_empty() {
                    JsonObject::new()
                } else {
                    serde_json::from_str(&acc.args).unwrap_or_default()
                };
                Some(ToolCall {
                    id: call_id,
                    name: acc.name,
                    arguments,
                })
            })
            .collect();
        Ok(RoundResult {
            content,
            reasoning,
            tool_calls,
            usage,
            // Raw items are recorded VERBATIM; the id hygiene for replay happens in `build_request`, so it also
            // heals blobs persisted by older builds that rewrote id := call_id.
            raw_content: Some(RawContent::OpenResponses(raw_items)),
            images,
        })
    }

    /// `response.output_item.done` (openresponses.go:359-426): image items surrender their payload, tool-search
    /// calls queue a leg, everything else is recorded verbatim and function calls accumulate by `call_id`.
    fn on_item_done(item: &Raw, out: &mut ItemSinks<'_>) {
        let peeked = serde_json::from_str::<RespOutputItem>(item.get()).ok();
        let Some(peeked) = peeked else {
            out.raw_items.push(item.clone());
            return;
        };
        match peeked.kind() {
            ItemKind::ImageGenerationCall => {
                // The generated image: base64 in `result`. The raw replay keeps the item WITHOUT the payload
                // (megabytes of base64 — the id alone carries the multiturn context server-side).
                if let Ok(data) = b64_decode(&peeked.result)
                    && !data.is_empty()
                {
                    out.images.push(Attachment {
                        filename: String::new(),
                        mime_type: image_mime(&peeked.output_format),
                        data,
                    });
                }
                if let Ok(mut obj) = serde_json::from_str::<JsonObject>(item.get()) {
                    obj.remove("result");
                    if let Ok(reencoded) = Raw::from_value(&obj) {
                        out.raw_items.push(reencoded);
                    }
                }
            }
            ItemKind::ToolSearchCall => {
                let call_id = peeked
                    .call_id
                    .clone()
                    .or_else(|| peeked.id.clone())
                    .unwrap_or_default();
                // The live API nests the query in an arguments OBJECT (verified on gpt-5.5); a top-level
                // "query" is kept as a fallback shape.
                let nested = peeked
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("query"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|q| !q.is_empty());
                let query = nested.map_or_else(|| peeked.query.clone(), ToOwned::to_owned);
                out.leg_searches.push(SearchCall { call_id, query });
                out.raw_items.push(item.clone());
            }
            kind => {
                out.raw_items.push(item.clone());
                if kind != ItemKind::FunctionCall {
                    return;
                }
                let call_id = peeked
                    .call_id
                    .clone()
                    .or_else(|| peeked.id.clone())
                    .unwrap_or_default();
                if out.fn_calls.contains_key(&call_id) {
                    return; // first occurrence wins
                }
                // Prefer the authoritative arguments from the completed item; fall back to the deltas.
                let args_string = peeked.arguments_string();
                let args = if args_string.is_empty() {
                    out.pending_args
                        .get(peeked.id.as_deref().unwrap_or_default())
                        .cloned()
                        .unwrap_or_default()
                } else {
                    args_string
                };
                out.fn_calls.insert(
                    call_id.clone(),
                    FnCallAcc {
                        name: peeked.name.unwrap_or_default(),
                        args,
                    },
                );
                out.fn_call_order.push(call_id);
            }
        }
    }
}

/// The accumulators `on_item_done` writes into (one struct so the helper keeps a small signature).
struct ItemSinks<'a> {
    raw_items: &'a mut Vec<Raw>,
    images: &'a mut Vec<Attachment>,
    leg_searches: &'a mut Vec<SearchCall>,
    pending_args: &'a HashMap<String, String>,
    fn_calls: &'a mut HashMap<String, FnCallAcc>,
    fn_call_order: &'a mut Vec<String>,
}

impl HasCore for OpenResponsesProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut ProviderCore {
        &mut self.core
    }
}

impl Provider for OpenResponsesProvider {
    fn kind(&self) -> ProviderKind {
        self.core.kind
    }

    fn model(&self) -> &str {
        &self.core.model
    }

    fn set_model(&mut self, model: String) {
        self.core.model = model;
    }

    /// Go `var _ UsageReporter = (*OpenResponsesProvider)(nil)` (provider/openresponses.go:17)
    /// — the dialect records last-call usage (T-38).
    fn reports_usage(&self) -> bool {
        true
    }

    fn list_models<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async move {
            self.client
                .models(cancel)
                .await
                .map_err(|e| ProviderError::wire(WireOp::ListModels, e))
        })
    }

    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            let mut req = self.build_request(messages);
            let resp = self
                .client
                .create(cancel, &mut req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Chat, e))?;
            // The unary path surfaces generated images too (-m single-shot runs).
            let mut images = Vec::new();
            for item in &resp.output {
                if item.kind() != ItemKind::ImageGenerationCall || item.result.is_empty() {
                    continue;
                }
                if let Ok(data) = b64_decode(&item.result)
                    && !data.is_empty()
                {
                    images.push(Attachment {
                        filename: String::new(),
                        mime_type: image_mime(&item.output_format),
                        data,
                    });
                }
            }
            let (text, _) = split_inline_think(&resp.output_text());
            Ok(ChatResult {
                text,
                usage: resp.usage.as_ref().map(openai_usage),
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

    fn as_tool_search_host(&mut self) -> Option<&mut dyn ToolSearchHost> {
        Some(self)
    }
}

impl ToolProvider for OpenResponsesProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(self.stream_internal(cancel, messages, tools, sink))
    }
}

impl ImageTunable for OpenResponsesProvider {
    fn set_image_output(&mut self, on: bool) {
        self.image_output = on;
    }

    fn image_output(&self) -> bool {
        self.image_output
    }
}

impl ToolSearchHost for OpenResponsesProvider {
    fn set_tool_searcher(&mut self, f: Option<ToolSearcher>) {
        self.searcher = f;
    }
}

#[cfg(test)]
mod tests {
    use super::{TOOL_SEARCH_QUERY_DESCRIPTION, image_mime, tool_search_parameters};

    #[test]
    fn tool_search_schema_matches_go() {
        assert_eq!(
            serde_json::to_string(&tool_search_parameters()).unwrap(),
            r#"{"properties":{"query":{"description":"Capability keywords","type":"string"}},"required":["query"],"type":"object"}"#
        );
        assert_eq!(TOOL_SEARCH_QUERY_DESCRIPTION, "Capability keywords");
    }

    #[test]
    fn image_mime_defaults_to_png() {
        assert_eq!(image_mime(""), "image/png");
        assert_eq!(image_mime("png"), "image/png");
        assert_eq!(image_mime("webp"), "image/webp");
    }
}
