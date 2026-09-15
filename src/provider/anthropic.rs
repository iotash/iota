//! The `anthropic` provider (provider/anthropic.go) on the messages dialect.

use std::collections::BTreeMap;

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{
    Attachment, JsonObject, Message, Raw, RawContent, Role, ToolCall, ToolDef,
};
use crate::provider::sink::{ReasoningGate, StreamSink};
use crate::provider::usage::Usage;
use crate::provider::{
    ChatResult, HttpTransport, Provider, ProviderKind, RoundResult, ToolProvider, TopPTunable,
    Tunable,
};
use reqwest::header::{HeaderName, HeaderValue};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::llm::anthropic::{
    ANTHROPIC_VERSION, Anthropic, AnthropicEvent, AnthropicMsg, AnthropicRequest, AnthropicTool,
    AnthropicToolEntry, AnthropicUsage, Block, BlockKind, DeltaKind, MAX_TOKENS, OutputConfig,
    RespBlock, SERVER_SEARCH_TOOL_NAME, SERVER_SEARCH_TOOL_TYPE, ServerTool, Source, StopReason,
    TextBlock, ToolSchema, TypedBlock,
};
use crate::llm::error::LlmError;
use crate::provider::common::{HasCore, ProviderCore, credential_header, make_client};
use crate::provider::think::{StreamThinkSplitter, split_inline_think};
use crate::provider::usage_conv::anthropic_usage;

/// Default base URL (`https://api.anthropic.com`).
pub(crate) const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic provider (tool, tunable, `top_p`; `RawContent::Anthropic`, replayed only with a deferred tool).
pub struct AnthropicProvider {
    core: ProviderCore,
    client: Anthropic,
}

impl AnthropicProvider {
    /// base "" → `ANTHROPIC_DEFAULT_BASE_URL`; headers `x-api-key`, `anthropic-version`.
    pub fn new(
        api_key: &str,
        base_url: &str,
        model: &str,
        temperature: Option<f64>,
        http: impl Into<HttpTransport>,
    ) -> Self {
        let client = make_client(base_url, ANTHROPIC_DEFAULT_BASE_URL, Some(http.into()))
            .with_header(
                HeaderName::from_static("x-api-key"),
                credential_header(api_key),
            )
            .with_header(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static(ANTHROPIC_VERSION),
            );
        Self {
            core: ProviderCore {
                kind: ProviderKind::Anthropic,
                model: model.to_owned(),
                temperature,
                top_p: None,
                effort: None,
            },
            client: Anthropic { client },
        }
    }

    /// provider/anthropic.go:91-190. `replay_server_blocks` is the reference-protocol gate: without a deferred
    /// tool in THIS request an assistant message's captured server blocks are stripped.
    fn build_request(&self, messages: &[Message], replay_server_blocks: bool) -> AnthropicRequest {
        let mut req = AnthropicRequest {
            model: self.core.model.clone(),
            max_tokens: MAX_TOKENS,
            messages: Vec::new(),
            system: Vec::new(),
            temperature: self.core.temperature,
            top_p: self.core.top_p,
            output_config: self.core.effort.map(|e| OutputConfig {
                effort: e.as_str().to_owned(),
            }),
            tools: Vec::new(),
            stream: false,
        };

        // Consecutive tool results coalesce into ONE user message; a user message that follows them merges in
        // instead of flushing first (the API rejects two consecutive user-role messages).
        let mut pending: Vec<Block> = Vec::new();

        for msg in messages {
            if msg.role() != Role::Tool && msg.role() != Role::User {
                flush_tool_results(&mut req.messages, &mut pending);
            }
            match msg.role() {
                Role::System => {
                    if !msg.tools().is_empty() {
                        continue; // a system-tools mount (K3 wire shape): chatcomp-only, skip here
                    }
                    req.system.push(TextBlock {
                        r#type: "text",
                        text: msg.content.clone(),
                    });
                }
                Role::User => {
                    for att in &msg.attachments {
                        pending.push(Block::Typed(attachment_block(att)));
                    }
                    // The message text is ALWAYS the last block, even when empty.
                    pending.push(Block::Typed(TypedBlock::Text {
                        text: msg.content.clone(),
                    }));
                    flush_tool_results(&mut req.messages, &mut pending);
                }
                Role::Assistant => {
                    let mut blocks: Vec<Block> = Vec::new();
                    // Replayable blocks lead the reconstructed content (thinking precedes the text
                    // it reasoned about; a search result block follows its server_tool_use).
                    // Thinking goes back on EVERY request that replays this turn — the API rejects
                    // a thinking-mode assistant message whose thinking is missing — while server
                    // blocks stay behind the reference-protocol gate.
                    if let Some(RawContent::Anthropic(raw)) = msg.raw_content() {
                        blocks.extend(
                            raw.iter()
                                .filter(|b| replay_server_blocks || is_thinking_block(b))
                                .cloned()
                                .map(Block::Raw),
                        );
                    }
                    if msg.tool_calls().is_empty() {
                        if !msg.content.is_empty() || blocks.is_empty() {
                            blocks.push(Block::Typed(TypedBlock::Text {
                                text: msg.content.clone(),
                            }));
                        }
                    } else {
                        if !msg.content.is_empty() {
                            blocks.push(Block::Typed(TypedBlock::Text {
                                text: msg.content.clone(),
                            }));
                        }
                        for tc in msg.tool_calls() {
                            blocks.push(Block::Typed(TypedBlock::ToolUse {
                                id: tc.id.clone(),
                                input: tc.arguments.clone(),
                                name: tc.name.clone(),
                            }));
                        }
                    }
                    req.messages.push(AnthropicMsg {
                        role: "assistant",
                        content: blocks,
                    });
                }
                Role::Tool => pending.push(Block::Typed(TypedBlock::ToolResult {
                    tool_use_id: msg.tool_call_id().to_owned(),
                    content: vec![TextBlock {
                        r#type: "text",
                        text: msg.content.clone(),
                    }],
                    is_error: msg.is_error(),
                })),
            }
        }
        flush_tool_results(&mut req.messages, &mut pending);
        req
    }

    /// provider/anthropic.go:220-424. `sink` receives the deltas; the returned `RoundResult` carries everything
    /// Go kept on the provider (usage, the captured server blocks).
    async fn stream_internal(
        &self,
        cancel: &CancellationToken,
        messages: &[Message],
        tools: &[ToolDef],
        sink: &mut dyn StreamSink,
    ) -> Result<RoundResult, ProviderError> {
        // Deferred tools (defer_mode "reference") carry defer_loading and summon the server-side search tool;
        // `any_deferred` is the protocol gate for BOTH capture and replay.
        let any_deferred = tools.iter().any(|t| t.deferred);
        let mut req = self.build_request(messages, any_deferred);
        for t in tools {
            req.tools.push(AnthropicToolEntry::Tool(AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: tool_schema(t),
                defer_loading: t.deferred,
            }));
        }
        if any_deferred {
            req.tools.push(AnthropicToolEntry::Server(ServerTool {
                r#type: SERVER_SEARCH_TOOL_TYPE,
                name: SERVER_SEARCH_TOOL_NAME,
            }));
        }

        let mut stream = self
            .client
            .stream_message(cancel, &mut req)
            .await
            .map_err(|e| ProviderError::wire(WireOp::Stream, e))?;

        let mut gate = ReasoningGate::new(sink);
        let mut split = StreamThinkSplitter::new();
        // Content blocks accumulate BY INDEX: content_block_delta events address blocks by `index` and can
        // interleave across open blocks (parallel tool_use). A BTreeMap assembles in ascending index order.
        let mut blocks: BTreeMap<u32, BlockAcc> = BTreeMap::new();
        let mut stop_reason: Option<StopReason> = None;
        // Usage arrives in two events: message_start reports the input side (plus cache counts) and
        // message_delta the message's CUMULATIVE usage — the output figure always, and the input side too
        // on the real API and on the compatible endpoints that send placeholder zeros at message_start
        // (GLM's `/api/anthropic`). The delta is laid over the start field by field and published once it
        // lands (`AnthropicUsage::overlay`, DIVERGENCES X-30).
        let mut usage = AnthropicUsage::default();
        let mut published: Option<Usage> = None;

        loop {
            let evt = match stream.next().await {
                Ok(Some(evt)) => evt,
                Ok(None) => break,
                Err(LlmError::Cancelled) => return Err(ProviderError::Cancelled),
                Err(e) => {
                    split.flush(&mut gate);
                    return Err(ProviderError::wire(WireOp::Stream, e));
                }
            };
            match evt {
                AnthropicEvent::MessageStart { usage: start } => {
                    usage = start.unwrap_or_default();
                }
                AnthropicEvent::BlockStart {
                    index,
                    kind,
                    id,
                    name,
                    raw,
                } => {
                    blocks.insert(
                        index,
                        BlockAcc {
                            kind,
                            id,
                            name,
                            raw: Some(raw),
                            ..BlockAcc::default()
                        },
                    );
                }
                AnthropicEvent::Delta { index, delta } => match delta {
                    DeltaKind::Thinking(s) => {
                        gate.reasoning(&s);
                        block(&mut blocks, index, BlockKind::Thinking)
                            .content
                            .push_str(&s);
                    }
                    DeltaKind::Text(s) => {
                        split.write(&s, &mut gate);
                        block(&mut blocks, index, BlockKind::Text)
                            .content
                            .push_str(&s);
                    }
                    DeltaKind::InputJson(s) => {
                        gate.close(); // thinking is over once tool args stream
                        let acc = block(&mut blocks, index, BlockKind::ToolUse);
                        acc.args.push_str(&s);
                        // The composing observer (anthropic.go:392). Server search args are NOT a client
                        // tool call: raising the lifecycle widget for one would leave it dangling, since
                        // no `CallTool` ever settles it — so only `tool_use` blocks report.
                        if acc.kind == BlockKind::ToolUse {
                            let name = (!acc.name.is_empty()).then_some(acc.name.as_str());
                            gate.tool_delta(name, &s);
                        }
                    }
                    DeltaKind::Signature(s) => {
                        block(&mut blocks, index, BlockKind::Thinking)
                            .sig
                            .push_str(&s);
                    }
                    DeltaKind::Other => {}
                },
                AnthropicEvent::MessageDelta {
                    stop_reason: reason,
                    usage: delta,
                } => {
                    stop_reason = reason;
                    if let Some(delta) = delta {
                        usage.overlay(&delta); // cumulative: what it carries is the message's figure
                        published = Some(anthropic_usage(&usage));
                    }
                }
                AnthropicEvent::Other => {} // content_block_stop / message_stop carry nothing
            }
        }
        split.flush(&mut gate);
        gate.close();

        let assembled = assemble(&blocks);
        // Thinking blocks replay UNCONDITIONALLY (the API rejects a thinking-mode turn whose
        // thinking is missing); server blocks only under the reference protocol, so a future
        // web_search's blocks are not dragged into history as orphans. Both lists were built in
        // stream order and thinking always opens a turn, so appending server blocks keeps the wire
        // order.
        let mut replay = assembled.replay_blocks;
        if any_deferred {
            replay.extend(assembled.server_blocks);
        }
        let raw_content = if replay.is_empty() {
            None
        } else {
            Some(RawContent::Anthropic(replay))
        };
        let tool_calls =
            if stop_reason == Some(StopReason::ToolUse) && !assembled.tool_calls.is_empty() {
                assembled.tool_calls
            } else {
                Vec::new()
            };
        Ok(RoundResult {
            content: split.content,
            reasoning: assembled.think + &split.think,
            tool_calls,
            usage: published,
            raw_content,
            images: Vec::new(),
        })
    }
}

/// One user attachment: `image/*` → an image block, exactly `application/pdf` → a document block, anything else
/// is inlined as text (provider/anthropic.go:129-147).
fn attachment_block(att: &Attachment) -> TypedBlock {
    if att.is_image() {
        TypedBlock::Image {
            source: Source {
                r#type: "base64",
                media_type: att.mime_type.clone(),
                data: crate::provider::common::b64(&att.data),
            },
        }
    } else if att.mime_type == "application/pdf" {
        TypedBlock::Document {
            source: Source {
                r#type: "base64",
                media_type: "application/pdf".to_owned(),
                data: crate::provider::common::b64(&att.data),
            },
        }
    } else {
        TypedBlock::Text {
            text: format!(
                "[File: {}]\n{}",
                att.filename,
                String::from_utf8_lossy(&att.data)
            ),
        }
    }
}

/// Emits the buffered tool-result blocks as ONE user message (provider/anthropic.go:105-110).
fn flush_tool_results(messages: &mut Vec<AnthropicMsg>, pending: &mut Vec<Block>) {
    if !pending.is_empty() {
        messages.push(AnthropicMsg {
            role: "user",
            content: std::mem::take(pending),
        });
    }
}

/// Only `properties` and `required` are forwarded from the incoming JSON schema; `required` only when it is an
/// array, and only its string entries (provider/anthropic.go:237-247).
fn tool_schema(t: &ToolDef) -> ToolSchema {
    let properties = t
        .input_schema
        .as_ref()
        .and_then(|s| s.get("properties"))
        // Go stores the value in an `any` field with omitempty: only an untyped nil is omitted.
        .filter(|v| !v.is_null())
        .cloned();
    let required = t
        .input_schema
        .as_ref()
        .and_then(|s| s.get("required"))
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ToolSchema {
        r#type: "object",
        properties,
        required,
    }
}

/// One streamed content block (provider/anthropic.go:284-291).
#[derive(Default)]
struct BlockAcc {
    /// The block's class.
    kind: BlockKind,
    /// `tool_use` / `server_tool_use`.
    id: String,
    /// `tool_use` / `server_tool_use`.
    name: String,
    /// text/thinking deltas.
    content: String,
    /// `signature_delta` fragments (thinking blocks).
    sig: String,
    /// `input_json_delta` fragments.
    args: String,
    /// The start-event JSON (server result blocks replay verbatim).
    raw: Option<Raw>,
}

/// Get-or-create the accumulator at `idx` (a delta may arrive without its `content_block_start`).
fn block(blocks: &mut BTreeMap<u32, BlockAcc>, idx: u32, kind: BlockKind) -> &mut BlockAcc {
    blocks.entry(idx).or_insert_with(|| BlockAcc {
        kind,
        ..BlockAcc::default()
    })
}

/// Whether a stored replay block goes back unconditionally: the two thinking shapes do, every other
/// block stays behind the reference-protocol gate. The class is read off the block's own `"type"`
/// rather than stored beside it, so the persisted blob stays the bare array sessions already hold.
fn is_thinking_block(b: &Raw) -> bool {
    BlockKind::of_raw(b).is_thinking()
}

/// What `assemble` walks out of the per-index accumulators.
#[derive(Default)]
struct Assembled {
    /// Concatenated `thinking` blocks.
    think: String,
    /// `tool_use` blocks in index order.
    tool_calls: Vec<ToolCall>,
    /// `server_tool_use` (recomposed) and `tool_search_tool_result` (verbatim) blocks in index order.
    server_blocks: Vec<Raw>,
    /// `thinking` (recomposed with its signature) and `redacted_thinking` (verbatim) blocks in
    /// index order — the ones that replay UNCONDITIONALLY.
    replay_blocks: Vec<Raw>,
}

/// A recomposed `thinking` block. The signature is omitted when the endpoint sent none (relays that
/// implement thinking mode without sealing), so what goes back is exactly what came in.
#[derive(Serialize)]
struct ThinkingBlock<'a> {
    r#type: &'static str,
    thinking: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    signature: &'a str,
}

/// A recomposed `server_tool_use` block; the field order is Go's anonymous struct (anthropic.go:327-332).
#[derive(Serialize)]
struct ServerToolUse<'a> {
    r#type: &'static str,
    id: &'a str,
    name: &'a str,
    input: Raw,
}

/// provider/anthropic.go:301-342. The text accumulator is assembled but unused — the returned content is the
/// think-tag splitter's, not the raw deltas'.
fn assemble(blocks: &BTreeMap<u32, BlockAcc>) -> Assembled {
    let mut out = Assembled::default();
    for acc in blocks.values() {
        match &acc.kind {
            BlockKind::Thinking => {
                out.think.push_str(&acc.content);
                // A thinking block must be handed back verbatim on the next request, signature
                // included; `replay_blocks` keeps it in stream order so it precedes the text it
                // reasoned about.
                // A signature-only block (empty thinking, sealed) is still part of the turn and
                // must replay: DeepSeek's endpoint emits them, and dropping one fails the same
                // replay check as dropping a full block. Only a block with neither body nor
                // signature is genuinely absent.
                if (!acc.content.is_empty() || !acc.sig.is_empty())
                    && let Ok(raw) = Raw::from_value(&ThinkingBlock {
                        r#type: "thinking",
                        thinking: &acc.content,
                        signature: &acc.sig,
                    })
                {
                    out.replay_blocks.push(raw);
                }
            }
            // Opaque and complete at `content_block_start`; replays verbatim.
            BlockKind::RedactedThinking => {
                if let Some(raw) = acc.raw.clone() {
                    out.replay_blocks.push(raw);
                }
            }
            BlockKind::ToolUse => {
                // An unparseable argument payload silently yields an empty map (Go parity, F-09).
                let arguments = if acc.args.is_empty() {
                    JsonObject::new()
                } else {
                    serde_json::from_str(&acc.args).unwrap_or_default()
                };
                out.tool_calls.push(ToolCall {
                    id: acc.id.clone(),
                    name: acc.name.clone(),
                    arguments,
                });
            }
            BlockKind::ServerToolUse => {
                // The input streams via input_json_delta; recompose the block. An empty or unparseable payload
                // becomes `{}` (that fallback literal always parses, so the second arm never fails).
                if let Ok(input) = Raw::from_string(acc.args.clone())
                    .or_else(|_| Raw::from_string("{}".to_owned()))
                    && let Ok(raw) = Raw::from_value(&ServerToolUse {
                        r#type: "server_tool_use",
                        id: &acc.id,
                        name: &acc.name,
                        input,
                    })
                {
                    out.server_blocks.push(raw);
                }
            }
            // Arrives complete in content_block_start; replays verbatim.
            BlockKind::ToolSearchToolResult => out.server_blocks.extend(acc.raw.clone()),
            // Text is accumulated for parity but never read back.
            BlockKind::Text | BlockKind::Other(_) => {}
        }
    }
    out
}

impl HasCore for AnthropicProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut ProviderCore {
        &mut self.core
    }
}

impl Provider for AnthropicProvider {
    fn kind(&self) -> ProviderKind {
        self.core.kind
    }

    fn model(&self) -> &str {
        &self.core.model
    }

    fn set_model(&mut self, model: String) {
        self.core.model = model;
    }

    /// Go `var _ UsageReporter = (*AnthropicProvider)(nil)` (provider/anthropic.go:18) —
    /// the dialect records last-call usage (T-38).
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
            let mut req = self.build_request(messages, false);
            let resp = self
                .client
                .message(cancel, &mut req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Chat, e))?;
            // Only text blocks are read; a unary response never carries tool calls (Chat sends no tools).
            let mut result = String::new();
            for block in &resp.content {
                if let RespBlock::Text { text } = block {
                    result.push_str(text);
                }
            }
            let (content, _) = split_inline_think(&result);
            Ok(ChatResult {
                text: content,
                usage: resp.usage.as_ref().map(anthropic_usage),
                images: Vec::new(),
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
}

impl ToolProvider for AnthropicProvider {
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
