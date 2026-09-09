//! Message and tool data model (provider/provider.go:11-59): roles, attachments, tool definitions and calls, the
//! dialect-owned `RawContent`, and `Message` with its constructors. The verbatim-JSON payload `Raw` and the
//! `JsonObject` alias are the wire layer's (`crate::llm::json`), re-exported here.

use serde::{Deserialize, Serialize};

pub use crate::llm::json::{JsonObject, Raw};

/// Message role; serialises as the exact Go strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// `"system"`.
    System,
    /// `"user"` — the default role.
    #[default]
    User,
    /// `"assistant"`.
    Assistant,
    /// `"tool"` (a tool-result message).
    Tool,
}

impl Role {
    /// The Go role string: `"system"` | `"user"` | `"assistant"` | `"tool"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// A file attached to a message (an input attachment or a generated image).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    /// Basename of the file.
    pub filename: String,
    /// MIME type, e.g. `image/png`.
    pub mime_type: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
}

impl Attachment {
    /// Whether the MIME type is `image/*`.
    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }
}

/// A tool definition advertised to the model (built-in or MCP).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolDef {
    /// Wire name of the tool.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema object forwarded verbatim; `None` = Go nil map (no object schema).
    pub input_schema: Option<JsonObject>,
    /// Set only by the protocol defer modes (reference / tool-search).
    pub deferred: bool,
}

/// The model requesting one tool invocation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCall {
    /// Call id assigned by the provider (or synthesised by the dialect).
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Parsed arguments (an empty map when the arguments failed to parse — Go parity).
    pub arguments: JsonObject,
}

/// Dialect-owned replay payload. A dialect trusts ONLY its own variant and falls back to reconstruction otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawContent {
    /// chat-completions: the recorded assistant message object, replayed verbatim.
    OpenAi(Raw),
    /// anthropic: captured `server_tool_use` / `tool_search_tool_result` blocks, verbatim, in order.
    Anthropic(Vec<Raw>),
    /// responses: raw output items of the last round, verbatim, in arrival order.
    OpenResponses(Vec<Raw>),
    /// gemini/vertexai: the model `Content` JSON (parts incl. thought signatures); re-sanitised on replay.
    Google(Raw),
}

/// One conversation message (provider.go:37-59). The text and the attachments belong to every role;
/// everything role-specific lives in [`Body`], so a message cannot carry fields its role has no use for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    /// Visible text (`""` for a tools mount).
    pub content: String,
    /// Attached files (input attachments or generated images).
    pub attachments: Vec<Attachment>,
    /// The role and what only that role carries.
    pub body: Body,
}

/// A message's role together with the data only that role has.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Body {
    /// The system prompt.
    System,
    /// A system-tools mount: tool definitions loaded mid-conversation (only chat-completions serialises it).
    ToolsMount(Vec<ToolDef>),
    /// The user's turn — the default.
    #[default]
    User,
    /// The model's turn.
    Assistant(AssistantBody),
    /// A tool result answering one call.
    Tool(ToolBody),
}

/// What an assistant message carries beside its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistantBody {
    /// Thinking text; display/bookkeeping only — NO dialect serialises it.
    pub reasoning: String,
    /// The tool calls the model requested.
    pub tool_calls: Vec<ToolCall>,
    /// Provider-specific replay payload.
    pub raw_content: Option<RawContent>,
    /// What the API call that produced this message cost (`None` = the provider reported nothing). Local
    /// bookkeeping only — NO dialect sends it upstream — but it IS persisted, so a resumed session recomputes its
    /// cumulative figures from the log.
    pub usage: Option<crate::provider::usage::Usage>,
    /// Cut short by the user; replayed as ordinary history. Persistence-only headlessly (no dialect reads it) —
    /// restored so a session line round-trips losslessly.
    pub interrupted: bool,
}

/// What a tool-result message carries beside its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolBody {
    /// Which call this answers.
    pub call_id: String,
    /// The function name.
    pub call_name: String,
    /// Whether the call failed.
    pub is_error: bool,
}

impl Message {
    /// Role System with `content`.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            body: Body::System,
            ..Self::default()
        }
    }

    /// Role User with `content`.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    /// Role Assistant with `content`.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::assistant_body(content, AssistantBody::default())
    }

    /// Role Assistant with `content` and everything else the role carries.
    pub fn assistant_body(content: impl Into<String>, body: AssistantBody) -> Self {
        Self {
            content: content.into(),
            body: Body::Assistant(body),
            ..Self::default()
        }
    }

    /// Role Assistant with `content`, the requested `tool_calls` and the dialect's `raw_content`.
    pub fn assistant_with_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        raw_content: Option<RawContent>,
    ) -> Self {
        Self::assistant_body(
            content,
            AssistantBody {
                tool_calls,
                raw_content,
                ..AssistantBody::default()
            },
        )
    }

    /// The assistant message with what its API call cost stamped on (a no-op for any other role).
    #[must_use]
    pub fn with_usage(mut self, usage: Option<crate::provider::usage::Usage>) -> Self {
        if let Some(a) = self.as_assistant_mut() {
            a.usage = usage;
        }
        self
    }

    /// A message of `role` with `content` and nothing else — the generic form test fixtures build from.
    pub fn of_role(role: Role, content: impl Into<String>) -> Self {
        match role {
            Role::System => Self::system(content),
            Role::User => Self::user(content),
            Role::Assistant => Self::assistant(content),
            Role::Tool => Self::tool_reply("", "", content, false),
        }
    }

    /// The assistant message with its thinking text set (a no-op for any other role).
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        if let Some(a) = self.as_assistant_mut() {
            a.reasoning = reasoning.into();
        }
        self
    }

    /// The assistant message with its requested tool calls set (a no-op for any other role).
    #[must_use]
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        if let Some(a) = self.as_assistant_mut() {
            a.tool_calls = tool_calls;
        }
        self
    }

    /// The assistant message with its replay payload set (a no-op for any other role).
    #[must_use]
    pub fn with_raw_content(mut self, raw_content: Option<RawContent>) -> Self {
        if let Some(a) = self.as_assistant_mut() {
            a.raw_content = raw_content;
        }
        self
    }

    /// The assistant message marked as cut short by the user (a no-op for any other role).
    #[must_use]
    pub fn with_interrupted(mut self, interrupted: bool) -> Self {
        if let Some(a) = self.as_assistant_mut() {
            a.interrupted = interrupted;
        }
        self
    }

    /// The tool result marked as a failed call (a no-op for any other role).
    #[must_use]
    pub fn with_error(mut self, is_error: bool) -> Self {
        if let Some(t) = self.as_tool_mut() {
            t.is_error = is_error;
        }
        self
    }

    /// Role Tool; copies `call.id` and `call.name`.
    pub fn tool_result(call: &ToolCall, text: impl Into<String>, is_error: bool) -> Self {
        Self::tool_reply(call.id.clone(), call.name.clone(), text, is_error)
    }

    /// Role Tool answering the call `call_id` made to `call_name`.
    pub fn tool_reply(
        call_id: impl Into<String>,
        call_name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            content: text.into(),
            body: Body::Tool(ToolBody {
                call_id: call_id.into(),
                call_name: call_name.into(),
                is_error,
            }),
            ..Self::default()
        }
    }

    /// A system-tools mount over `defs`; an empty list is a plain (empty) system message, as before.
    pub fn system_tools(defs: Vec<ToolDef>) -> Self {
        Self {
            body: if defs.is_empty() {
                Body::System
            } else {
                Body::ToolsMount(defs)
            },
            ..Self::default()
        }
    }

    /// The role, as the wire and the session log name it.
    pub fn role(&self) -> Role {
        match self.body {
            Body::System | Body::ToolsMount(_) => Role::System,
            Body::User => Role::User,
            Body::Assistant(_) => Role::Assistant,
            Body::Tool(_) => Role::Tool,
        }
    }

    /// Whether this is a system-tools mount.
    pub fn is_tools_mount(&self) -> bool {
        matches!(self.body, Body::ToolsMount(_))
    }

    /// The mounted tool definitions (empty for anything but a mount).
    pub fn tools(&self) -> &[ToolDef] {
        match &self.body {
            Body::ToolsMount(defs) => defs,
            _ => &[],
        }
    }

    /// The assistant data, when this is the model's turn.
    pub fn as_assistant(&self) -> Option<&AssistantBody> {
        match &self.body {
            Body::Assistant(a) => Some(a),
            _ => None,
        }
    }

    /// The assistant data for in-place bookkeeping (usage, the interrupted flag).
    pub fn as_assistant_mut(&mut self) -> Option<&mut AssistantBody> {
        match &mut self.body {
            Body::Assistant(a) => Some(a),
            _ => None,
        }
    }

    /// The tool-result data for in-place bookkeeping.
    pub fn as_tool_mut(&mut self) -> Option<&mut ToolBody> {
        match &mut self.body {
            Body::Tool(t) => Some(t),
            _ => None,
        }
    }

    /// The tool-result data, when this answers a call.
    pub fn as_tool(&self) -> Option<&ToolBody> {
        match &self.body {
            Body::Tool(t) => Some(t),
            _ => None,
        }
    }

    /// Thinking text (`""` for anything but an assistant message).
    pub fn reasoning(&self) -> &str {
        self.as_assistant().map_or("", |a| a.reasoning.as_str())
    }

    /// The tool calls an assistant message requested (empty otherwise).
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.as_assistant().map_or(&[], |a| a.tool_calls.as_slice())
    }

    /// The dialect's replay payload, when an assistant message carries one.
    pub fn raw_content(&self) -> Option<&RawContent> {
        self.as_assistant().and_then(|a| a.raw_content.as_ref())
    }

    /// What producing this assistant message cost, when reported.
    pub fn usage(&self) -> Option<crate::provider::usage::Usage> {
        self.as_assistant().and_then(|a| a.usage)
    }

    /// Whether the user cut this assistant message short.
    pub fn interrupted(&self) -> bool {
        self.as_assistant().is_some_and(|a| a.interrupted)
    }

    /// Whether this tool result reports a failed call.
    pub fn is_error(&self) -> bool {
        self.as_tool().is_some_and(|t| t.is_error)
    }

    /// Which call this tool result answers (`""` otherwise).
    pub fn tool_call_id(&self) -> &str {
        self.as_tool().map_or("", |t| t.call_id.as_str())
    }

    /// The function this tool result answers (`""` otherwise).
    pub fn tool_call_name(&self) -> &str {
        self.as_tool().map_or("", |t| t.call_name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{Attachment, Message, Raw, RawContent, Role, ToolCall, ToolDef};
    use crate::provider::usage::Usage;

    /// Option<Raw>: a literal null is ABSENT (POLICY F-05), a present object is kept verbatim.
    #[derive(Deserialize)]
    struct Envelope {
        error: Option<Raw>,
    }

    #[test]
    fn raw_eq_compares_json_text() {
        let first = Raw::from_string(r#"{"a":1}"#.to_owned()).unwrap();
        let second = Raw::from_string(r#"{"a":1}"#.to_owned()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.get(), r#"{"a":1}"#);
        // Whitespace-different JSON is a different payload: equality is textual, not semantic.
        let spaced = Raw::from_string(r#"{"a": 1}"#.to_owned()).unwrap();
        assert_ne!(first, spaced);
        assert!(Raw::from_string("{not json".to_owned()).is_err());
        // from_value serialises compactly with sorted keys (Go marshal order).
        let value = serde_json::json!({"z": 1, "a": [true, null]});
        assert_eq!(
            Raw::from_value(&value).unwrap().get(),
            r#"{"a":[true,null],"z":1}"#
        );
        assert_eq!(
            Raw::from_value(&value).unwrap(),
            Raw::from_string(r#"{"a":[true,null],"z":1}"#.to_owned()).unwrap()
        );

        let env: Envelope = serde_json::from_str(r#"{"error": null}"#).unwrap();
        assert!(env.error.is_none());
        let env: Envelope = serde_json::from_str(r#"{"error": {"m":  1}}"#).unwrap();
        assert_eq!(env.error.unwrap().get(), r#"{"m":  1}"#);
        let env: Envelope = serde_json::from_str("{}").unwrap();
        assert!(env.error.is_none());
        // Transparent on the way out too.
        assert_eq!(serde_json::to_string(&first).unwrap(), r#"{"a":1}"#);

        // RawContent trusts its variant; Message/RoundResult compare structurally through it.
        let openai_a =
            Message::assistant_with_calls("", vec![], Some(RawContent::OpenAi(first.clone())));
        let openai_b = Message::assistant_with_calls("", vec![], Some(RawContent::OpenAi(second)));
        let google = Message::assistant_with_calls("", vec![], Some(RawContent::Google(first)));
        assert_eq!(openai_a, openai_b);
        assert_ne!(openai_a, google);
    }

    #[test]
    fn message_constructors() {
        assert_eq!(Message::system("s").role(), Role::System);
        assert_eq!(Message::user("u").content, "u");
        assert_eq!(Message::assistant("a").role(), Role::Assistant);
        let call = ToolCall {
            id: "c1".to_owned(),
            name: "noop".to_owned(),
            ..ToolCall::default()
        };
        let r = Message::tool_result(&call, "ok", true);
        assert_eq!(r.role(), Role::Tool);
        assert_eq!(r.tool_call_id(), "c1");
        assert_eq!(r.tool_call_name(), "noop");
        assert!(r.is_error());
        assert_eq!(r.content, "ok");
        // Every role-specific accessor answers the neutral value off the wrong role.
        assert!(r.tool_calls().is_empty() && r.usage().is_none() && !r.interrupted());
        assert!(Message::assistant("a").tool_call_id().is_empty());
        let mount = Message::system_tools(vec![ToolDef {
            name: "late".to_owned(),
            ..ToolDef::default()
        }]);
        assert!(mount.is_tools_mount());
        assert_eq!(mount.tools().len(), 1);
        assert_eq!(mount.role(), Role::System);
        assert!(mount.content.is_empty());
        assert!(!Message::system("x").is_tools_mount());
        assert!(!Message::system_tools(vec![]).is_tools_mount());
        assert_eq!(Message::system_tools(vec![]), Message::system(""));
        assert_eq!(Role::default(), Role::User);
        for (role, want) in [
            (Role::System, "\"system\""),
            (Role::User, "\"user\""),
            (Role::Assistant, "\"assistant\""),
            (Role::Tool, "\"tool\""),
        ] {
            assert_eq!(serde_json::to_string(&role).unwrap(), want);
            assert_eq!(role.as_str(), want.trim_matches('"'));
        }
        assert!(
            Attachment {
                mime_type: "image/png".to_owned(),
                ..Attachment::default()
            }
            .is_image()
        );
        assert!(
            !Attachment {
                mime_type: "application/pdf".to_owned(),
                ..Attachment::default()
            }
            .is_image()
        );
    }

    /// The two session-persistence fields (provider.go:46,58) start unset and no constructor touches them.
    #[test]
    fn message_session_fields_default_unset() {
        let blank = Message::default();
        assert!(!blank.interrupted());
        assert!(blank.usage().is_none());

        let call = ToolCall {
            id: "c1".to_owned(),
            name: "noop".to_owned(),
            ..ToolCall::default()
        };
        let with_calls = Message::assistant_with_calls("a", vec![call.clone()], None);
        assert!(!with_calls.interrupted());
        assert!(with_calls.usage().is_none());
        let result = Message::tool_result(&call, "ok", false);
        assert!(!result.interrupted());
        assert!(result.usage().is_none());

        // The session layer fills both in after construction; both take part in equality.
        let mut cut_short = Message::assistant("done");
        let a = cut_short.as_assistant_mut().expect("assistant");
        a.interrupted = true;
        a.usage = Some(Usage {
            input: 11,
            output: 7,
            total: 18,
            ..Usage::default()
        });
        assert_ne!(cut_short, Message::assistant("done"));
        assert_eq!(cut_short.usage().unwrap_or_default().context_tokens(), 18);
    }
}
