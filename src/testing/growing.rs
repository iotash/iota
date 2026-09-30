//! [`GrowingProvider`] — the long-run fake (docs/design/bot-mode.md §5.2). Where [`super::FakeProvider`] plays a
//! script, this one COMPUTES every answer from the request it was sent, so a conversation of thousands of turns
//! needs no script at all, and the window pressure is real:
//!
//! - the usage it reports is measured on the request: `input` is the bytes sent (every message's text, tool
//!   calls and results, and the advertised tool definitions) divided by four, `output` the reply's bytes divided
//!   by four;
//! - a request over the window is REFUSED with the error a real endpoint sends (a 400 carrying
//!   `context_length_exceeded`), never answered — otherwise "the view fits the window" could not fail;
//! - a user turn's prompt carries its number (`#17 …`), and the reply names it back (`[#17] …`);
//! - on the turns [`GrowingProvider::remembering_at`] names, and on every memory flush, it calls `remember`
//!   through the real tool path; when the memory is past its soft threshold, or a write is refused at the cap,
//!   it removes its oldest line instead, the way a model told to consolidate would.
//!
//! Its sizes come from the turn number and a seed, so the same prompt always gets the same reply — across a
//! restart too, which is what lets a run with restarts be compared with one without.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use super::lock;
use crate::BoxFuture;
use crate::llm::{LlmError, StatusError};
use crate::provider::error::{ProviderError, WireOp};
use crate::provider::model::{Body, JsonObject, Message, Role, ToolCall, ToolDef};
use crate::provider::sink::StreamSink;
use crate::provider::usage::Usage;
use crate::provider::{ChatResult, Provider, ProviderKind, RoundResult, ToolProvider};

/// A user turn's prompt starts with this and its number: `#17 …`.
pub const TURN_MARK: char = '#';
/// Every memory line this fake writes carries `fact-<id>;` — an `old` that matches exactly one line.
pub const FACT_MARK: &str = "fact-";

/// What a call was, as the fake read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    /// The first round of a user turn.
    Turn,
    /// A round answering the turn's own tool results.
    Followup,
    /// The first round of the memory-flush turn.
    Flush,
    /// The compaction's summary pass (the unary path).
    Summary,
}

/// One recorded call.
#[derive(Clone, Debug)]
pub struct GrowingCall {
    /// What the call was.
    pub kind: CallKind,
    /// The number of the last user turn in the request (`0`: none left in it).
    pub turn: u64,
    /// The history the call was sent.
    pub messages: Vec<Message>,
    /// The tool names advertised.
    pub tools: Vec<String>,
    /// The measured input (bytes sent / 4).
    pub input: u64,
    /// The reply's size (bytes / 4); `0` for a refused call.
    pub output: u64,
    /// The call was over the window and refused.
    pub refused: bool,
}

/// Runs inside every call with the call as recorded — where a test checks the disk while the loop is mid-run.
type Hook = Arc<dyn Fn(&GrowingCall) + Send + Sync>;

/// The shared state behind every clone.
struct Shared {
    window: u64,
    seed: u64,
    remember_at: BTreeSet<u64>,
    keep_lines: Option<usize>,
    calls: Mutex<Vec<GrowingCall>>,
    on_call: Mutex<Option<Hook>>,
}

/// The long-run fake — see the module docs. Clones share the script and the call log, so a test keeps one
/// handle while each run of the loop is handed its own box.
#[derive(Clone)]
pub struct GrowingProvider {
    shared: Arc<Shared>,
    model: String,
}

impl GrowingProvider {
    /// A usage-reporting tool provider (`openai`/`gpt-test`) with a `window`-token context; `seed` picks the
    /// reply sizes.
    pub fn new(window: u64, seed: u64) -> Self {
        Self::build(window, seed, BTreeSet::new(), None)
    }

    fn build(
        window: u64,
        seed: u64,
        remember_at: BTreeSet<u64>,
        keep_lines: Option<usize>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                window,
                seed,
                remember_at,
                keep_lines,
                calls: Mutex::new(Vec::new()),
                on_call: Mutex::new(None),
            }),
            model: "gpt-test".to_owned(),
        }
    }

    /// Turn `n`'s first round calls `remember` (a `[user]` line) for every `n` in `turns`.
    #[must_use]
    pub fn remembering_at(self, turns: impl IntoIterator<Item = u64>) -> Self {
        Self::build(
            self.shared.window,
            self.shared.seed,
            turns.into_iter().collect(),
            self.shared.keep_lines,
        )
    }

    /// A tidy model: it keeps at most `lines` of its own lines, removing the oldest before it adds when the
    /// memory it is shown already has that many. Without it the memory grows to the soft threshold (§3.5) and
    /// hovers there — the model consolidates only when told to.
    #[must_use]
    pub fn keeping(self, lines: usize) -> Self {
        Self::build(
            self.shared.window,
            self.shared.seed,
            self.shared.remember_at.clone(),
            Some(lines),
        )
    }

    /// `f` runs inside every call, after it is recorded.
    pub fn on_call(&self, f: impl Fn(&GrowingCall) + Send + Sync + 'static) {
        *lock(&self.shared.on_call) = Some(Arc::new(f));
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<GrowingCall> {
        lock(&self.shared.calls).clone()
    }

    /// How many calls so far.
    pub fn call_count(&self) -> usize {
        lock(&self.shared.calls).len()
    }

    /// The prompt of user turn `n` — what a test types. Its length (40 to 640 bytes) comes from the seed.
    pub fn prompt(&self, n: u64) -> String {
        let len = 40 + mix(self.shared.seed ^ 0x5eed ^ n) % 600;
        pad(format!("{TURN_MARK}{n} tell me about item {n}."), len)
    }

    /// The bytes a request carries: every message's text, its calls' names and arguments, and the tool
    /// definitions advertised (name, description and schema).
    pub fn request_bytes(messages: &[Message], tools: &[ToolDef]) -> usize {
        let calls = |m: &Message| {
            m.tool_calls()
                .iter()
                .map(|c| {
                    c.name.len()
                        + serde_json::Value::Object(c.arguments.clone())
                            .to_string()
                            .len()
                })
                .sum::<usize>()
        };
        let defs = tools
            .iter()
            .map(|t| {
                t.name.len()
                    + t.description.len()
                    + t.input_schema.as_ref().map_or(0, |s| {
                        serde_json::Value::Object(s.clone()).to_string().len()
                    })
            })
            .sum::<usize>();
        messages
            .iter()
            .map(|m| m.content.len() + calls(m))
            .sum::<usize>()
            + defs
    }

    /// Records the call, runs the hook, and answers it — or refuses it when it is over the window.
    fn answer(&self, messages: &[Message], tools: &[ToolDef]) -> Result<RoundResult, u64> {
        let input = u64::try_from(Self::request_bytes(messages, tools) / 4).unwrap_or(u64::MAX);
        let turn = last_turn(messages);
        let (kind, round) = self.round(messages, turn);
        let refused = input > self.shared.window;
        let output = if refused {
            0
        } else {
            let bytes = round.content.len()
                + round
                    .tool_calls
                    .iter()
                    .map(|c| {
                        serde_json::Value::Object(c.arguments.clone())
                            .to_string()
                            .len()
                    })
                    .sum::<usize>();
            u64::try_from(bytes / 4).unwrap_or(u64::MAX)
        };
        let call = GrowingCall {
            kind,
            turn,
            messages: messages.to_vec(),
            tools: tools.iter().map(|t| t.name.clone()).collect(),
            input,
            output,
            refused,
        };
        lock(&self.shared.calls).push(call.clone());
        let hook = lock(&self.shared.on_call).clone();
        if let Some(f) = hook {
            f(&call);
        }
        if refused {
            return Err(input);
        }
        Ok(RoundResult {
            usage: Some(Usage {
                input,
                output,
                total: input + output,
                ..Usage::default()
            }),
            ..round
        })
    }

    /// What the request asks for, read off its last message.
    fn round(&self, messages: &[Message], turn: u64) -> (CallKind, RoundResult) {
        let last = messages.last().map_or("", |m| m.content.as_str());
        if last.starts_with(crate::repl::commands::compact::SUMMARY_INSTRUCTION) {
            let len = 600 + mix(self.shared.seed ^ 0x5a ^ turn) % 400;
            return (
                CallKind::Summary,
                text(pad(
                    format!("Summary of the conversation through {TURN_MARK}{turn}."),
                    len,
                )),
            );
        }
        // The turn so far: everything after its prompt (a user message or the flush notice).
        let start = messages
            .iter()
            .rposition(|m| matches!(m.body, Body::User | Body::Notice))
            .unwrap_or(0);
        let prompt = messages.get(start).map_or("", |m| m.content.as_str());
        let flush = crate::repl::bot::is_flush_notice(prompt);
        let own = &messages[start + 1..];
        let round_no = own.iter().filter(|m| m.role() == Role::Tool).count();
        let id = |what: &str| format!("{}-{turn}-{round_no}", if flush { "f" } else { "t" }) + what;

        if let Some(result) = own.last().filter(|m| m.role() == Role::Tool) {
            // Refused at the cap: make room by dropping the oldest line the refusal shows, then stop.
            if result.is_error()
                && result.content.contains("cap; nothing was written")
                && let Some(old) = oldest_fact(&result.content, own)
            {
                return (CallKind::Followup, remove(&id("rm"), &old));
            }
            // Past the soft threshold: one line out per round, until the result stops asking (or the last
            // remove found nothing to take).
            if !result.is_error()
                && result.content.contains("consolidate soon")
                && let Some(old) = oldest_fact(&memory_block(messages), own)
            {
                return (CallKind::Followup, remove(&id("rm"), &old));
            }
            return (
                CallKind::Followup,
                text(format!("[{TURN_MARK}{turn}] Saved.")),
            );
        }

        let full = self
            .shared
            .keep_lines
            .is_some_and(|n| facts(&memory_block(messages)) >= n);
        if flush {
            if (full || prompt.contains("consolidate soon"))
                && let Some(old) = oldest_fact(&memory_block(messages), own)
            {
                return (CallKind::Flush, remove(&id("rm"), &old));
            }
            return (
                CallKind::Flush,
                add(
                    &id("add"),
                    &format!("{FACT_MARK}f{turn}; the flush before {TURN_MARK}{turn} kept this"),
                    "inferred",
                ),
            );
        }
        if self.shared.remember_at.contains(&turn) {
            if full && let Some(old) = oldest_fact(&memory_block(messages), own) {
                return (CallKind::Turn, remove(&id("rm"), &old));
            }
            return (
                CallKind::Turn,
                add(
                    &id("add"),
                    &format!("{FACT_MARK}u{turn}; the user said so in {TURN_MARK}{turn}"),
                    "user",
                ),
            );
        }
        // Mostly short answers; one in twenty is long (up to ~1.5k tokens) — the turn that jumps the threshold.
        let r = mix(self.shared.seed ^ 0xa5 ^ turn);
        let len = if r.is_multiple_of(20) {
            4000 + r / 20 % 2000
        } else {
            40 + r % 2400
        };
        (
            CallKind::Turn,
            text(pad(
                format!("[{TURN_MARK}{turn}] Here is item {turn}."),
                len,
            )),
        )
    }
}

/// The number of the last user turn in `messages` (`0` when none is left, e.g. right after a compaction kept
/// only the flush exchange).
fn last_turn(messages: &[Message]) -> u64 {
    messages
        .iter()
        .rev()
        .filter(|m| m.body == Body::User)
        .find_map(|m| turn_of(&m.content))
        .unwrap_or(0)
}

/// `#17 …` → 17; also finds a prompt behind the summary preamble a compaction puts before the kept turn.
fn turn_of(content: &str) -> Option<u64> {
    let at = content.rfind(TURN_MARK)?;
    let digits: String = content[at + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The memory block the overlay put in the system message (empty when there is none).
fn memory_block(messages: &[Message]) -> String {
    messages
        .first()
        .filter(|m| m.role() == Role::System)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// The oldest line of `text` carrying a fact id that this turn has not already removed: its `fact-<id>;`.
fn oldest_fact(text: &str, own: &[Message]) -> Option<String> {
    let removed: Vec<String> = own
        .iter()
        .flat_map(Message::tool_calls)
        .filter_map(|c| {
            c.arguments
                .get("old")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .collect();
    text.lines()
        .filter(|l| l.starts_with("- ["))
        .filter_map(|l| {
            let at = l.find(FACT_MARK)?;
            let end = l[at..].find(';')?;
            Some(l[at..=at + end].to_owned())
        })
        .find(|id| !removed.contains(id))
}

/// How many of this fake's lines `text` shows.
fn facts(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("- [") && l.contains(FACT_MARK))
        .count()
}

/// A deterministic 64-bit mix (splitmix64's finaliser).
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// `head` padded with filler words to `len` bytes (never cut below `head`).
fn pad(mut head: String, len: u64) -> String {
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    let words = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
    ];
    let mut i = 0;
    while head.len() < len {
        let _ = write!(head, " {}", words[i % words.len()]);
        i += 1;
    }
    head
}

fn text(s: String) -> RoundResult {
    RoundResult {
        content: s,
        ..RoundResult::default()
    }
}

fn call(id: &str, args: &[(&str, &str)]) -> RoundResult {
    let mut arguments = JsonObject::new();
    for (k, v) in args {
        arguments.insert((*k).to_owned(), serde_json::Value::from(*v));
    }
    RoundResult {
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: crate::tool::builtins::memory::REMEMBER.to_owned(),
            arguments,
        }],
        ..RoundResult::default()
    }
}

fn add(id: &str, line: &str, source: &str) -> RoundResult {
    call(id, &[("action", "add"), ("text", line), ("source", source)])
}

fn remove(id: &str, old: &str) -> RoundResult {
    call(id, &[("action", "remove"), ("old", old)])
}

/// What a real endpoint answers a request over the window: a 400 in the `openai` dialect's envelope.
fn overflow(op: WireOp, window: u64, input: u64) -> ProviderError {
    ProviderError::wire(
        op,
        LlmError::Status(StatusError {
            status: 400,
            status_text: "Bad Request".to_owned(),
            method: "POST".to_owned(),
            url: "https://api.openai.com/v1/chat/completions".to_owned(),
            body: format!(
                r#"{{"error":{{"message":"This model's maximum context length is {window} tokens. However, your messages resulted in {input} tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}}}"#
            ),
        }),
    )
}

impl Provider for GrowingProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(std::future::ready(Ok(vec![self.model.clone()])))
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        let r = self
            .answer(messages, &[])
            .map(|r| ChatResult {
                text: r.content,
                usage: r.usage,
                images: r.images,
            })
            .map_err(|input| overflow(WireOp::Chat, self.shared.window, input));
        Box::pin(std::future::ready(r))
    }

    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        Some(self)
    }

    fn reports_usage(&self) -> bool {
        true
    }
}

impl ToolProvider for GrowingProvider {
    fn stream_chat_with_tools<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        let r = self
            .answer(messages, tools)
            .map_err(|input| overflow(WireOp::Stream, self.shared.window, input));
        if let Ok(round) = &r {
            sink.reasoning_done();
            if !round.content.is_empty() {
                sink.content(&round.content);
            }
        }
        Box::pin(std::future::ready(r))
    }
}

#[cfg(test)]
mod tests {
    use super::{CallKind, GrowingProvider, last_turn, oldest_fact};
    use crate::provider::Provider;
    use crate::provider::model::Message;
    use tokio_util::sync::CancellationToken;

    /// The usage is the request measured, the reply names the turn, and a request over the window is refused
    /// with the context-overflow shape the chat layer reroutes to /compact.
    #[tokio::test]
    async fn answers_are_computed_from_the_request() {
        let p = GrowingProvider::new(100, 7);
        let cancel = CancellationToken::new();
        let prompt = p.prompt(3);
        assert!(prompt.starts_with("#3 "), "{prompt}");
        let small = [Message::user("#3 hi".to_owned())];
        let r = p.chat(&cancel, &small).await.expect("fits");
        assert!(r.text.starts_with("[#3] "), "{}", r.text);
        let u = r.usage.expect("usage");
        assert_eq!(u.input, 5 / 4);
        assert_eq!(u.output, u64::try_from(r.text.len() / 4).expect("fits"));

        let big = [Message::user(format!("#4 {}", "x".repeat(500)))];
        let err = p.chat(&cancel, &big).await.expect_err("over the window");
        assert!(err.to_string().contains("context_length_exceeded"), "{err}");
        let calls = p.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].refused && calls[1].input > 100);
        assert_eq!(calls[0].kind, CallKind::Turn);
    }

    #[test]
    fn turn_numbers_and_fact_ids_are_read_back() {
        assert_eq!(
            last_turn(&[Message::user("#12 x"), Message::assistant("[#12] y")]),
            12
        );
        assert_eq!(last_turn(&[Message::assistant("x")]), 0);
        let text = "# coder memory\n\n## User\n- [user] fact-u3; a (2026-10-01)\n- [inferred] fact-f9; b (2026-10-01)\n";
        assert_eq!(oldest_fact(text, &[]).as_deref(), Some("fact-u3;"));
    }
}
