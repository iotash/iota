//! Shared test fakes (feature `testing`): the one fake provider ([`FakeProvider`]), the fake dispatchers
//! (`dispatch`), a recording sink, the scripted UI facade, and map-backed `VarResolver`/`EnvSource`
//! fixtures that replace `t.Setenv`.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::app::env::{EnvSource, VarResolver};
use crate::provider::model::{
    AssistantBody, Attachment, Body, JsonObject, Message, Raw, RawContent, ToolCall, ToolDef,
};
use crate::provider::sink::StreamSink;
use crate::provider::usage::Usage;

mod dispatch;
mod provider;
mod scripted;
pub use dispatch::{
    FakeMcp, GatedDispatch, GrowingDispatcher, HeaderDispatch, NoCapDispatch, ParallelDispatch,
    StaticDispatcher, prefix_for, static_prefix, stub_tool,
};
pub use provider::{Call, Failure, FakeProvider, Interrupt, Log, Path, Round};
pub use scripted::{PanelSummary, RecordingHost, Reply, ScriptedUi, TabbedSummary, UiEvent};

/// Locks a fixture mutex, tolerating poisoning (a panicking test must not hide the state from the next assertion).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every record shape a real session produces, in the order a turn writes them: a system message, a user
/// message with an attachment, an assistant message with tool calls, a dialect (`openai`) raw payload and
/// usage, that call's tool result, a final assistant with reasoning and usage, and an interrupted partial.
/// `examples/mkbundle.rs` writes them as the session-format smoke sample; `tests/session/roundtrip.rs`
/// proves the store hands every one of them back.
pub fn every_record_shape() -> Vec<Message> {
    let mut arguments = JsonObject::new();
    arguments.insert("q".to_owned(), serde_json::Value::from(1));
    let calls = vec![ToolCall {
        id: "c1".to_owned(),
        name: "f".to_owned(),
        arguments,
    }];
    let raw = Raw::from_string(r#"{"role":"assistant","content":null}"#.to_owned())
        .unwrap_or_else(|e| unreachable!("a literal object parses: {e}"));
    vec![
        Message::system("sys"),
        Message {
            attachments: vec![Attachment {
                filename: "a.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                data: b"hello".to_vec(),
            }],
            ..Message::user("hi")
        },
        Message::assistant_with_calls("", calls.clone(), Some(RawContent::OpenAi(raw))).with_usage(
            Some(Usage {
                input: 1000,
                output: 200,
                total: 1200,
                ..Usage::default()
            }),
        ),
        Message::tool_result(&calls[0], "ok", false),
        Message::assistant("done")
            .with_reasoning("th".to_owned())
            .with_usage(Some(Usage {
                input: 10,
                output: 5,
                ..Usage::default()
            })),
        // The one shape only an interrupt writes: a partial the loader must survive.
        Message {
            content: "cut".to_owned(),
            body: Body::Assistant(AssistantBody {
                interrupted: true,
                ..AssistantBody::default()
            }),
            ..Message::default()
        },
    ]
}

/// Counts `text` with the chat's own tokenizer (the o200k counter behind the meter) — for expectations
/// about published token figures.
pub fn count_tokens(text: &str) -> u64 {
    crate::repl::tokens::TokenCounter::new().count(text)
}

/// A `ToolDef` with just a name.
pub fn tool_def(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        ..ToolDef::default()
    }
}

/// A `ToolCall` with empty arguments.
pub fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: JsonObject::new(),
    }
}

/// A `ToolCall` with string arguments.
pub fn tool_call_with(id: &str, name: &str, args: &[(&str, &str)]) -> ToolCall {
    let mut arguments = JsonObject::new();
    for (k, v) in args {
        arguments.insert((*k).to_owned(), serde_json::Value::from(*v));
    }
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

/// One sink event, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkEvent {
    /// A content delta.
    Content(String),
    /// A reasoning delta.
    Reasoning(String),
    /// `reasoning_done` fired.
    ReasoningDone,
    /// A progressive image frame (`image_partial`) with its decoded bytes.
    ImagePartial(Vec<u8>),
}

/// Ordered event log; tests assert `ReasoningDone` precedes the first `Content`.
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// Every event, in order.
    pub events: Vec<SinkEvent>,
}

impl StreamSink for RecordingSink {
    fn content(&mut self, delta: &str) {
        self.events.push(SinkEvent::Content(delta.to_owned()));
    }

    fn reasoning(&mut self, delta: &str) {
        self.events.push(SinkEvent::Reasoning(delta.to_owned()));
    }

    fn reasoning_done(&mut self) {
        self.events.push(SinkEvent::ReasoningDone);
    }

    fn image_partial(&mut self, frame: &[u8]) {
        self.events.push(SinkEvent::ImagePartial(frame.to_vec()));
    }
}

impl RecordingSink {
    /// All content deltas concatenated.
    pub fn content(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::Content(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All reasoning deltas concatenated.
    pub fn reasoning(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::Reasoning(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether `ReasoningDone` was recorded before the first `Content` (Go's `closedBeforeContent`: true until a
    /// content write arrives while reasoning is still open; a stream with no content at all is trivially ordered).
    pub fn closed_before_content(&self) -> bool {
        let mut closed = false;
        for e in &self.events {
            match e {
                SinkEvent::ReasoningDone => closed = true,
                SinkEvent::Content(_) if !closed => return false,
                _ => {}
            }
        }
        true
    }
}

/// `HashMap`-backed `VarResolver` (env vars + fixed cwd/home) — replaces `t.Setenv`.
#[derive(Clone, Debug, Default)]
pub struct MapResolver {
    /// Environment variables.
    pub vars: HashMap<String, String>,
    /// The working directory.
    pub cwd: Option<PathBuf>,
    /// The home directory.
    pub home: Option<PathBuf>,
}

impl VarResolver for MapResolver {
    fn env_var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }

    fn cwd(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
}

/// A `MapResolver` over `vars` with no cwd/home.
pub fn map_resolver(vars: &[(&str, &str)]) -> MapResolver {
    MapResolver {
        vars: vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..MapResolver::default()
    }
}

/// `HashMap`-backed `EnvSource`.
#[derive(Clone, Debug, Default)]
pub struct MapEnv(pub HashMap<String, String>);

impl EnvSource for MapEnv {
    /// Like `ProcessEnv`, an empty value counts as unset.
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).filter(|v| !v.is_empty()).cloned()
    }
}

/// A `MapEnv` over `vars`.
pub fn map_env(vars: &[(&str, &str)]) -> MapEnv {
    MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{RecordingSink, SinkEvent, map_env, map_resolver};
    use crate::app::env::{EnvSource, VarResolver, expand};
    use crate::provider::sink::StreamSink;

    #[test]
    fn recording_sink_orders_events() {
        let mut s = RecordingSink::default();
        StreamSink::reasoning(&mut s, "th");
        StreamSink::reasoning(&mut s, "ink");
        s.reasoning_done();
        StreamSink::content(&mut s, "hi");
        assert_eq!(s.reasoning(), "think");
        assert_eq!(s.content(), "hi");
        assert!(s.closed_before_content());
        assert_eq!(s.events[2], SinkEvent::ReasoningDone);

        let mut late = RecordingSink::default();
        StreamSink::content(&mut late, "x");
        late.reasoning_done();
        assert!(!late.closed_before_content());
        let mut never = RecordingSink::default();
        StreamSink::content(&mut never, "x");
        assert!(!never.closed_before_content());
        assert!(RecordingSink::default().closed_before_content());
    }

    #[test]
    fn map_fixtures() {
        let r = map_resolver(&[("A", "1")]);
        assert_eq!(r.env_var("A"), Some("1".to_owned()));
        assert_eq!(r.env_var("B"), None);
        assert!(r.cwd().is_none() && r.home().is_none());
        assert_eq!(expand("${env:A}${cwd}", &r), "1${cwd}");
        let e = map_env(&[("K", "v"), ("EMPTY", "")]);
        assert_eq!(e.var("K"), Some("v".to_owned()));
        assert_eq!(e.var("EMPTY"), None);
        assert_eq!(e.var("MISSING"), None);
    }
}
