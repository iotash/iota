//! The `openai` provider (provider/openai.go) on the chat-completions dialect.

use std::collections::BTreeMap;

use crate::BoxFuture;
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{JsonObject, Message, Raw, RawContent, Role, ToolCall, ToolDef};
use crate::provider::sink::{ReasoningGate, StreamSink};
use crate::provider::usage::Usage;
use crate::provider::{
    ChatResult, HttpTransport, Provider, ProviderKind, RoundResult, ToolProvider, TopPTunable,
    Tunable,
};
use reqwest::header::AUTHORIZATION;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::llm::chatcomp::{
    ChatComp, ChatCompRequest, ChatContent, ChatFileData, ChatImageUrl, ChatMessage, ChatMsg,
    ChatPart, ChatTool, ChatToolCall, ChatToolCallFunc, ChatToolFunction, ChatToolsMsg,
};
use crate::provider::common::{
    HasCore, OPENAI_DEFAULT_BASE_URL, ProviderCore, b64, credential_header, data_url, make_client,
};
use crate::provider::think::{StreamThinkSplitter, split_inline_think};
use crate::provider::usage_conv::openai_usage;

/// The finish reason that turns accumulated fragments into tool calls (openai.go:273).
const FINISH_TOOL_CALLS: &str = "tool_calls";

/// Chat-completions provider (tool, tunable, `top_p`; `RawContent::OpenAi`).
pub struct OpenAiProvider {
    core: ProviderCore,
    client: ChatComp,
}

impl OpenAiProvider {
    /// base "" → `OPENAI_DEFAULT_BASE_URL`; header `Authorization: Bearer <key>`.
    ///
    /// A key that cannot be spelled as an HTTP header value (control bytes) is sent as an EMPTY header —
    /// the convention every provider shares (`common::credential_header`); Go would have failed the
    /// request when writing it.
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
                kind: ProviderKind::OpenAi,
                model: model.to_owned(),
                temperature,
                top_p: None,
                effort: None,
            },
            client: ChatComp { client },
        }
    }

    /// provider/openai.go:69-142: model + tuning, then every message mapped IN ORDER.
    ///
    /// A system message carrying tools becomes the Kimi-K3 mount (`role` + `tools`, no content key); a user
    /// message with attachments becomes `image_url` / `file` parts with the text part LAST; an assistant
    /// tool-call turn whose recorded `RawContent::OpenAi` is present is replayed verbatim.
    fn build_request(&self, messages: &[Message]) -> ChatCompRequest {
        let mut req = ChatCompRequest {
            model: self.core.model.clone(),
            messages: Vec::with_capacity(messages.len()),
            temperature: self.core.temperature,
            top_p: self.core.top_p,
            reasoning_effort: self.core.effort.map(|e| e.as_str().to_owned()),
            tools: Vec::new(),
            stream: false,
            stream_options: None,
        };
        for msg in messages {
            match msg.role() {
                Role::System => {
                    if msg.tools().is_empty() {
                        req.messages.push(text_msg("system", msg.content.clone()));
                    } else {
                        // Dynamically loaded tools (defer_mode system-tools): the K3 wire shape — tools, no
                        // content (their API 400s on a system message carrying both).
                        req.messages.push(ChatMessage::ToolsMount(ChatToolsMsg {
                            role: "system",
                            tools: msg.tools().iter().map(chat_tool).collect(),
                        }));
                    }
                }
                Role::User => {
                    let content = if msg.attachments.is_empty() {
                        ChatContent::Text(msg.content.clone())
                    } else {
                        let mut parts = Vec::with_capacity(msg.attachments.len() + 1);
                        for att in &msg.attachments {
                            if att.is_image() {
                                parts.push(ChatPart::ImageUrl {
                                    image_url: ChatImageUrl {
                                        url: data_url(&att.mime_type, &att.data),
                                    },
                                });
                            } else {
                                parts.push(ChatPart::File {
                                    file: ChatFileData {
                                        file_data: b64(&att.data),
                                        filename: att.filename.clone(),
                                    },
                                });
                            }
                        }
                        // The text part is always appended LAST, even when the content is empty.
                        parts.push(ChatPart::Text {
                            text: msg.content.clone(),
                        });
                        ChatContent::Parts(parts)
                    };
                    req.messages.push(ChatMessage::Msg(ChatMsg {
                        role: "user",
                        content,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    }));
                }
                Role::Assistant => {
                    if msg.tool_calls().is_empty() {
                        req.messages
                            .push(text_msg("assistant", msg.content.clone()));
                        continue;
                    }
                    // The recorded payload is replayed verbatim: it preserves provider-specific fields
                    // (e.g. kimi `reasoning`). Only this dialect's own variant is trusted.
                    if let Some(RawContent::OpenAi(raw)) = msg.raw_content() {
                        req.messages.push(ChatMessage::Raw(raw.clone()));
                        continue;
                    }
                    req.messages.push(ChatMessage::Msg(ChatMsg {
                        role: "assistant",
                        content: ChatContent::Text(msg.content.clone()),
                        tool_calls: msg
                            .tool_calls()
                            .iter()
                            .map(|tc| ChatToolCall {
                                id: tc.id.clone(),
                                r#type: "function",
                                function: ChatToolCallFunc {
                                    name: tc.name.clone(),
                                    arguments: serde_json::to_string(&tc.arguments)
                                        .unwrap_or_default(),
                                },
                            })
                            .collect(),
                        tool_call_id: None,
                    }));
                }
                Role::Tool => req.messages.push(ChatMessage::Msg(ChatMsg {
                    role: "tool",
                    content: ChatContent::Text(msg.content.clone()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(msg.tool_call_id().to_owned()),
                })),
            }
        }
        req
    }
}

/// A `ToolDef` as the advertised wire shape (`description`/`parameters` omitted when empty).
fn chat_tool(t: &ToolDef) -> ChatTool {
    ChatTool {
        r#type: "function",
        function: ChatToolFunction {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone().filter(|p| !p.is_empty()),
        },
    }
}

/// A plain `{"role":…,"content":…}` message.
fn text_msg(role: &'static str, content: String) -> ChatMessage {
    ChatMessage::Msg(ChatMsg {
        role,
        content: ChatContent::Text(content),
        tool_calls: Vec::new(),
        tool_call_id: None,
    })
}

/// One tool call being assembled: `id`/`name` arrive once, `arguments` concatenate across deltas.
#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

/// One entry of the recorded assistant message's `tool_calls` (openai.go:287-291).
fn raw_tool_call(acc: &ToolAcc) -> Value {
    let mut function = serde_json::Map::new();
    function.insert("name".to_owned(), Value::String(acc.name.clone()));
    function.insert("arguments".to_owned(), Value::String(acc.args.clone()));
    let mut call = serde_json::Map::new();
    call.insert("id".to_owned(), Value::String(acc.id.clone()));
    call.insert("type".to_owned(), Value::String("function".to_owned()));
    call.insert("function".to_owned(), Value::Object(function));
    Value::Object(call)
}

impl HasCore for OpenAiProvider {
    fn core(&self) -> &ProviderCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut ProviderCore {
        &mut self.core
    }
}

impl Provider for OpenAiProvider {
    fn kind(&self) -> ProviderKind {
        self.core.kind
    }

    fn model(&self) -> &str {
        &self.core.model
    }

    fn set_model(&mut self, model: String) {
        self.core.model = model;
    }

    /// Go `var _ UsageReporter = (*OpenAIProvider)(nil)` (provider/openai.go:18) — the
    /// dialect records last-call usage (T-38).
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
                .complete(cancel, &mut req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Chat, e))?;
            // This call owns its figures: a response without a usage block reports nothing rather than
            // leaving the previous call's numbers standing (base.go:31-46).
            let usage = resp.usage.as_ref().map(openai_usage);
            let Some(choice) = resp.choices.first() else {
                return Err(ProviderError::NoChoices);
            };
            let (text, _) =
                split_inline_think(choice.message.content.as_deref().unwrap_or_default());
            Ok(ChatResult {
                text,
                usage,
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

impl ToolProvider for OpenAiProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(async move {
            let mut req = self.build_request(messages);
            // `ToolDef::deferred` is ignored by this dialect: every tool is advertised normally.
            req.tools = tools.iter().map(chat_tool).collect();

            let mut stream = self
                .client
                .stream_completion(cancel, &mut req)
                .await
                .map_err(|e| ProviderError::wire(WireOp::Stream, e))?;

            let mut gate = ReasoningGate::new(sink);
            let mut split = StreamThinkSplitter::new();
            // Wire-level reasoning (the `reasoning`/`reasoning_content` fields), kept apart from the
            // tag-extracted think text so it is never duplicated into the recorded payload.
            let mut think_full = String::new();
            // Every content delta verbatim, inline `<think>` tags included (the replay copy).
            let mut raw_content = String::new();
            // Accumulators keyed by stream index. Sparse indices are KEPT (DIVERGENCES F-04): the sorted
            // key set is iterated, where Go walked `0..len(map)` and silently dropped calls after a gap.
            let mut accs: BTreeMap<i64, ToolAcc> = BTreeMap::new();
            let mut finish_reason = String::new();
            let mut usage: Option<Usage> = None;

            loop {
                let chunk = match stream.next().await {
                    Ok(Some(c)) => c,
                    Ok(None) => break,
                    Err(e) => {
                        // Go returns the partial content and reasoning beside the error; the headless
                        // caller drops both (chat.go:311), so only the sink traffic is observable.
                        split.flush(&mut gate);
                        return Err(ProviderError::wire(WireOp::Stream, e));
                    }
                };
                // The final chunk (include_usage) carries token usage and no choices; the last such
                // chunk wins. A streaming usage block without a total is ignored (unary accepts one).
                if let Some(u) = &chunk.usage
                    && u.total_tokens > 0
                {
                    usage = Some(openai_usage(u));
                }
                for choice in &chunk.choices {
                    if let Some(fr) = &choice.finish_reason
                        && !fr.is_empty()
                    {
                        finish_reason.clone_from(fr);
                    }

                    // Thinking deltas: aggregators send `reasoning`, deepseek's own API sends
                    // `reasoning_content` — accept either, prefer the first.
                    let reasoning = choice.delta.reasoning.as_deref().unwrap_or_default();
                    let think = if reasoning.is_empty() {
                        choice
                            .delta
                            .reasoning_content
                            .as_deref()
                            .unwrap_or_default()
                    } else {
                        reasoning
                    };
                    if !think.is_empty() {
                        gate.reasoning(think);
                        think_full.push_str(think);
                    }

                    for tc in &choice.delta.tool_calls {
                        let acc = accs.entry(tc.index.max(0)).or_default();
                        if let Some(id) = &tc.id
                            && !id.is_empty()
                        {
                            acc.id.clone_from(id);
                        }
                        if let Some(name) = &tc.function.name
                            && !name.is_empty()
                        {
                            acc.name.push_str(name);
                        }
                        if let Some(args) = &tc.function.arguments
                            && !args.is_empty()
                        {
                            gate.close(); // thinking is over once tool args stream
                            acc.args.push_str(args);
                            // The composing observer (openai.go:255): the name accumulated
                            // SO FAR rides along, so an argument fragment that arrives
                            // before its name is anonymous and raises no widget — the
                            // zombie-spinner rule (TUI_CONTRACTS §4, T-11).
                            let name = (!acc.name.is_empty()).then_some(acc.name.as_str());
                            gate.tool_delta(name, args);
                        }
                    }

                    if let Some(delta) = &choice.delta.content
                        && !delta.is_empty()
                    {
                        raw_content.push_str(delta);
                        split.write(delta, &mut gate);
                    }
                }
            }
            split.flush(&mut gate);
            gate.close();

            let content = std::mem::take(&mut split.content);
            let reasoning = think_full.clone() + &split.think;

            if finish_reason != FINISH_TOOL_CALLS || accs.is_empty() {
                // The fragments of an unfinished tool round are discarded along with the replay payload.
                return Ok(RoundResult {
                    content,
                    reasoning,
                    tool_calls: Vec::new(),
                    usage,
                    raw_content: None,
                    images: Vec::new(),
                });
            }

            let mut tool_calls = Vec::with_capacity(accs.len());
            let mut raw_calls = Vec::with_capacity(accs.len());
            for acc in accs.into_values() {
                // Unparseable (or empty) arguments yield an empty map, exactly like Go's ignored error.
                let arguments: JsonObject = if acc.args.is_empty() {
                    JsonObject::new()
                } else {
                    serde_json::from_str(&acc.args).unwrap_or_default()
                };
                raw_calls.push(raw_tool_call(&acc));
                tool_calls.push(ToolCall {
                    id: acc.id,
                    name: acc.name,
                    arguments,
                });
            }

            // The recorded assistant message replayed verbatim next round. Only the wire `reasoning`
            // field is stored: tag-extracted think text is already inline in the verbatim content.
            let mut raw_msg = serde_json::Map::new();
            raw_msg.insert("role".to_owned(), Value::String("assistant".to_owned()));
            raw_msg.insert("content".to_owned(), Value::String(raw_content));
            raw_msg.insert("tool_calls".to_owned(), Value::Array(raw_calls));
            if !think_full.is_empty() {
                raw_msg.insert("reasoning".to_owned(), Value::String(think_full));
            }
            let raw_content = Raw::from_value(&Value::Object(raw_msg))
                .ok()
                .map(RawContent::OpenAi);

            Ok(RoundResult {
                content,
                reasoning,
                tool_calls,
                usage,
                raw_content,
                images: Vec::new(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::error::LlmError;
    use crate::provider::Effort;
    use crate::provider::model::Attachment;

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new("k", "http://h/v1/", "m", Some(0.5), http())
    }

    #[test]
    fn base_url_defaults_and_trailing_slashes_are_trimmed() {
        let p = OpenAiProvider::new("k", "", "m", None, http());
        assert_eq!(p.client.client.base_url(), OPENAI_DEFAULT_BASE_URL);
        assert_eq!(p.kind(), ProviderKind::OpenAi);
        assert_eq!(p.model(), "m");
        assert_eq!(provider().client.client.base_url(), "http://h/v1");
    }

    /// The tuning fields reach the request; `reasoning_effort` is omitted while no effort is set.
    #[test]
    fn build_request_carries_the_tuning() {
        let mut p = provider();
        p.set_top_p(Some(0.9));
        let req = p.build_request(&[]);
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.top_p, Some(0.9));
        assert!(req.reasoning_effort.is_none());
        p.set_effort(Some(Effort::XHigh));
        assert_eq!(
            p.build_request(&[]).reasoning_effort.as_deref(),
            Some("xhigh")
        );
    }

    /// Attachment parts keep the attachment order with the text part LAST; images are data URLs and every
    /// other mime type is BARE base64 beside its filename.
    #[test]
    fn attachment_parts_put_the_text_last() {
        let msg = Message {
            attachments: vec![
                Attachment {
                    filename: "a.png".to_owned(),
                    mime_type: "image/png".to_owned(),
                    data: vec![1],
                },
                Attachment {
                    filename: "b.pdf".to_owned(),
                    mime_type: "application/pdf".to_owned(),
                    data: vec![2],
                },
            ],
            ..Message::user("look")
        };
        let req = provider().build_request(&[msg]);
        assert_eq!(
            serde_json::to_string(&req.messages).unwrap(),
            r#"[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AQ=="}},{"type":"file","file":{"file_data":"Ag==","filename":"b.pdf"}},{"type":"text","text":"look"}]}]"#
        );
    }

    /// An assistant tool-call turn without a recorded payload is rebuilt; a `RawContent` variant belonging
    /// to another dialect is NOT trusted and falls back to reconstruction.
    #[test]
    fn assistant_tool_calls_without_our_raw_content_are_rebuilt() {
        let mut arguments = JsonObject::new();
        arguments.insert("q".to_owned(), Value::String("x".to_owned()));
        let call = ToolCall {
            id: "c1".to_owned(),
            name: "f".to_owned(),
            arguments,
        };
        let foreign =
            RawContent::Google(Raw::from_string("{\"role\":\"model\"}".to_owned()).unwrap());
        let msg = Message::assistant_with_calls("prev", vec![call.clone()], Some(foreign));
        let req = provider().build_request(&[msg]);
        assert_eq!(
            serde_json::to_string(&req.messages).unwrap(),
            r#"[{"role":"assistant","content":"prev","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{\"q\":\"x\"}"}}]}]"#
        );

        // Empty arguments serialise as `{}` (Rust has no nil map; Go emitted `null` — see DEVIATIONS).
        let bare = Message::assistant_with_calls(
            "",
            vec![ToolCall {
                id: "c1".to_owned(),
                name: "f".to_owned(),
                arguments: JsonObject::new(),
            }],
            None,
        );
        let req = provider().build_request(&[bare]);
        assert!(
            serde_json::to_string(&req.messages)
                .unwrap()
                .contains(r#""arguments":"{}""#)
        );
    }

    /// A tool result carries `tool_call_id`; a plain system/assistant message is a bare role+content pair.
    #[test]
    fn tool_and_plain_messages() {
        let call = ToolCall {
            id: "c1".to_owned(),
            ..ToolCall::default()
        };
        let msgs = [
            Message::system("sys"),
            Message::assistant("hi"),
            Message::tool_result(&call, "result", false),
        ];
        let req = provider().build_request(&msgs);
        assert_eq!(
            serde_json::to_string(&req.messages).unwrap(),
            r#"[{"role":"system","content":"sys"},{"role":"assistant","content":"hi"},{"role":"tool","content":"result","tool_call_id":"c1"}]"#
        );
    }

    /// `description` and `parameters` disappear when empty (Go `omitempty` on a map covers the empty map).
    #[test]
    fn advertised_tools_omit_empty_fields() {
        let def = ToolDef {
            name: "f".to_owned(),
            input_schema: Some(JsonObject::new()),
            ..ToolDef::default()
        };
        assert_eq!(
            serde_json::to_string(&chat_tool(&def)).unwrap(),
            r#"{"type":"function","function":{"name":"f"}}"#
        );
    }

    /// Cancellation surfaces as `Cancelled`, never wrapped in Go's prefix.
    #[test]
    fn cancelled_is_never_wrapped() {
        assert!(matches!(
            ProviderError::wire(WireOp::Stream, LlmError::Cancelled),
            ProviderError::Cancelled
        ));
        assert_eq!(
            ProviderError::wire(WireOp::Stream, LlmError::NoEvents).to_string(),
            "stream error: stream ended without any SSE events (server did not stream?)"
        );
    }

    /// An unrepresentable key sends an EMPTY `Authorization` header (idiom-8: the shared convention, not
    /// a silent omit), and the sensitive mark keeps it out of any `Debug` rendering (security-4).
    #[test]
    fn illegal_key_sends_an_empty_sensitive_authorization_header() {
        let p = OpenAiProvider::new("bad\nkey", "http://h", "m", None, http());
        let dump = format!("{:?}", p.client.client);
        assert!(dump.contains("authorization"), "{dump}");
        assert!(dump.contains("Sensitive"), "{dump}");
        assert!(!dump.contains("bad\nkey"), "{dump}");
    }
}
