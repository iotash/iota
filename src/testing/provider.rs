//! The one fake `Provider`. [`FakeProvider`] is a builder over three things every hand-rolled test
//! provider used to reimplement: an identity (kind, model, model listing, usage reporting), a set of
//! optional capabilities switched on one at a time (tools, tuning, image knobs, progressive frames), and a
//! script of [`Round`]s played in call order — with a [`Log`] of every call that a test reads back after
//! the provider itself was boxed into the run loop.
//!
//! It stays a recorder plus a script. Behaviour that depends on what was sent goes through
//! [`FakeProvider::answering`], a closure that sees the call number and the history; nothing here
//! interprets messages on its own.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{lock, tool_call};
use crate::BoxFuture;
use crate::provider::error::ProviderError;
use crate::provider::model::{Attachment, Message, ToolCall, ToolDef};
use crate::provider::sink::StreamSink;
use crate::provider::usage::Usage;
use crate::provider::{
    ChatResult, Effort, ImageEditJsonTunable, ImageGenOptions, ImageGenParams, ImageGenTunable,
    ImagePartialProvider, ImageTunable, Provider, ProviderKind, RoundResult, ToolProvider, Tunable,
};

/// How a round fails instead of answering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// `ProviderError::other(msg)` — the retryable shape.
    Other(String),
    /// `ProviderError::permanent_msg(msg)` — a failure retrying cannot fix.
    Permanent(String),
}

impl Failure {
    fn error(&self) -> ProviderError {
        match self {
            Self::Other(msg) => ProviderError::other(msg.clone()),
            Self::Permanent(msg) => ProviderError::permanent_msg(msg.clone()),
        }
    }
}

/// What a round cancels just before it returns — the user pressing ESC mid-stream.
#[derive(Clone, Debug)]
pub enum Interrupt {
    /// The token handed to the call itself (the turn's own token).
    Call,
    /// A token the test holds.
    Token(CancellationToken),
}

/// One scripted round: what the provider streams into the sink, then what it returns. The unary path
/// (`Provider::chat`) plays the same round without a sink — `result.content`, `usage` and `images`
/// become the `ChatResult`.
#[derive(Clone, Debug, Default)]
pub struct Round {
    /// Reasoning deltas, streamed before `reasoning_done`.
    pub reasoning: Vec<String>,
    /// Content deltas, streamed after `reasoning_done`.
    pub content: Vec<String>,
    /// Tool-argument deltas (`(name, chunk)`), streamed after the content.
    pub deltas: Vec<(Option<String>, String)>,
    /// What the round returns.
    pub result: RoundResult,
    /// Cancelled just before returning.
    pub interrupt: Option<Interrupt>,
    /// Returned instead of `result`.
    pub fail: Option<Failure>,
}

impl Round {
    /// A reply that is only RETURNED (`result.content`), never streamed — the shape of a provider that
    /// closes reasoning and hands back the finished text.
    pub fn reply(s: &str) -> Self {
        Self::result(RoundResult {
            content: s.to_owned(),
            ..RoundResult::default()
        })
    }

    /// A reply streamed as one content delta and returned as `result.content`.
    pub fn text(s: &str) -> Self {
        Self {
            content: vec![s.to_owned()],
            ..Self::reply(s)
        }
    }

    /// A round requesting `calls`.
    pub fn calls(calls: Vec<ToolCall>) -> Self {
        Self::result(RoundResult {
            tool_calls: calls,
            ..RoundResult::default()
        })
    }

    /// A round returning `result` verbatim, streaming nothing but `reasoning_done`.
    pub fn result(result: RoundResult) -> Self {
        Self {
            result,
            ..Self::default()
        }
    }

    /// A round failing with `ProviderError::other(msg)`.
    pub fn failing(msg: &str) -> Self {
        Self {
            fail: Some(Failure::Other(msg.to_owned())),
            ..Self::default()
        }
    }

    /// A round failing with `ProviderError::permanent_msg(msg)`.
    pub fn permanent(msg: &str) -> Self {
        Self {
            fail: Some(Failure::Permanent(msg.to_owned())),
            ..Self::default()
        }
    }

    /// The round reports `usage`.
    #[must_use]
    pub fn usage(mut self, usage: Usage) -> Self {
        self.result.usage = Some(usage);
        self
    }

    /// The round returns `images`.
    #[must_use]
    pub fn images(mut self, images: Vec<Attachment>) -> Self {
        self.result.images = images;
        self
    }

    /// The round cancels `interrupt` just before it returns.
    #[must_use]
    pub fn interrupting(mut self, interrupt: Interrupt) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    /// Fires the interrupt, then the failure or the result.
    fn settle(self, call: &CancellationToken) -> Result<RoundResult, ProviderError> {
        match self.interrupt {
            Some(Interrupt::Call) => call.cancel(),
            Some(Interrupt::Token(t)) => t.cancel(),
            None => {}
        }
        match self.fail {
            Some(f) => Err(f.error()),
            None => Ok(self.result),
        }
    }
}

/// The unary view of a round's result.
fn chat_result(r: RoundResult) -> ChatResult {
    ChatResult {
        text: r.content,
        usage: r.usage,
        images: r.images,
    }
}

/// Which entry point a call came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// `Provider::chat` — the unary path.
    Chat,
    /// `ToolProvider::stream_chat_with_tools`.
    Tools,
    /// `ImagePartialProvider::chat_observed`.
    Observed,
}

/// One recorded call.
#[derive(Clone, Debug)]
pub struct Call {
    /// The entry point.
    pub path: Path,
    /// The history the call was sent.
    pub messages: Vec<Message>,
    /// The tool names advertised (the tool path only).
    pub tools: Vec<String>,
}

/// The provider's call log, shared with every handle: `FakeProvider::log` before the provider is boxed
/// into the run loop, the accessors afterwards.
#[derive(Clone, Debug, Default)]
pub struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    /// Appends `call` and returns its 1-based number.
    fn record(&self, call: Call) -> usize {
        let mut calls = lock(&self.0);
        calls.push(call);
        calls.len()
    }

    /// Calls so far, every entry point counted.
    pub fn calls(&self) -> usize {
        lock(&self.0).len()
    }

    /// Every call, in order.
    pub fn records(&self) -> Vec<Call> {
        lock(&self.0).clone()
    }

    /// The history of every call, in order.
    pub fn sent(&self) -> Vec<Vec<Message>> {
        lock(&self.0).iter().map(|c| c.messages.clone()).collect()
    }

    /// The history of call `n` (0-based); empty when there was no such call.
    pub fn send(&self, n: usize) -> Vec<Message> {
        lock(&self.0)
            .get(n)
            .map(|c| c.messages.clone())
            .unwrap_or_default()
    }

    /// The content of the LAST message of every call — the prompt each call answered.
    pub fn prompts(&self) -> Vec<String> {
        lock(&self.0)
            .iter()
            .map(|c| {
                c.messages
                    .last()
                    .map(|m| m.content.clone())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The tool names advertised on each tool-path call, in order.
    pub fn seen_tools(&self) -> Vec<Vec<String>> {
        lock(&self.0)
            .iter()
            .filter(|c| c.path == Path::Tools)
            .map(|c| c.tools.clone())
            .collect()
    }

    /// Whether any call came through `chat_observed`.
    pub fn observed(&self) -> bool {
        lock(&self.0).iter().any(|c| c.path == Path::Observed)
    }
}

/// Runs inside call `n` (1-based) with the history it was sent — the only place a test can change the
/// world mid-run.
type Hook = Arc<dyn Fn(usize, &[Message]) + Send + Sync>;
/// Answers call `n` (1-based) from the history it was sent.
type Answer = Arc<dyn Fn(usize, &[Message]) -> Round + Send + Sync>;

/// The `Tunable` state, present only when the capability is switched on.
#[derive(Clone, Copy, Debug, Default)]
struct Tuning {
    temperature: Option<f64>,
    effort: Option<Effort>,
}

/// The one fake provider — see the module docs. `new()` is a unary `openai`/`gpt-test` provider with no
/// optional capability, an empty model listing, no usage reporting and an empty reply; every `with_*`
/// switches one thing on.
pub struct FakeProvider {
    kind: ProviderKind,
    model: String,
    models: Result<Vec<String>, String>,
    list_delay: Duration,
    usage: bool,
    tools: bool,
    tuning: Option<Tuning>,
    image_output: Option<bool>,
    image_gen: Option<(ImageGenOptions, ImageGenParams)>,
    json_edits: Option<bool>,
    frames: Option<Vec<Vec<u8>>>,
    rounds: Mutex<VecDeque<Round>>,
    tail: Round,
    answer: Option<Answer>,
    on_call: Option<Hook>,
    log: Log,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            kind: ProviderKind::OpenAi,
            model: "gpt-test".to_owned(),
            models: Ok(Vec::new()),
            list_delay: Duration::ZERO,
            usage: false,
            tools: false,
            tuning: None,
            image_output: None,
            image_gen: None,
            json_edits: None,
            frames: None,
            rounds: Mutex::new(VecDeque::new()),
            tail: Round::default(),
            answer: None,
            on_call: None,
            log: Log::default(),
        }
    }
}

/// The empty parameters an `ImageGenTunable` without knobs reports.
static NO_IMAGE_GEN_PARAMS: ImageGenParams = ImageGenParams {
    aspect_ratio: None,
    image_size: None,
    negative_prompt: None,
};

impl FakeProvider {
    /// The default described on the type.
    pub fn new() -> Self {
        Self::default()
    }

    // --- identity ------------------------------------------------------------

    /// The provider type (`openai` by default).
    #[must_use]
    pub fn with_kind(mut self, kind: ProviderKind) -> Self {
        self.kind = kind;
        self
    }

    /// The model id (`gpt-test` by default); `set_model` replaces it.
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        model.clone_into(&mut self.model);
        self
    }

    /// What `list_models` answers (empty by default).
    #[must_use]
    pub fn with_models(mut self, ids: &[&str]) -> Self {
        self.models = Ok(ids.iter().map(|s| (*s).to_owned()).collect());
        self
    }

    /// `list_models` fails with `ProviderError::other(msg)`.
    #[must_use]
    pub fn with_models_failing(mut self, msg: &str) -> Self {
        self.models = Err(msg.to_owned());
        self
    }

    /// `list_models` answers only after `delay` (tokio time — a paused clock compresses it).
    #[must_use]
    pub fn with_models_after(mut self, delay: Duration) -> Self {
        self.list_delay = delay;
        self
    }

    /// `reports_usage()` is true — the dialect keeps last-call accounting.
    #[must_use]
    pub fn reporting_usage(mut self) -> Self {
        self.usage = true;
        self
    }

    // --- capabilities --------------------------------------------------------

    /// Switches the `ToolProvider` capability on: rounds play through the sink.
    #[must_use]
    pub fn with_tools(mut self) -> Self {
        self.tools = true;
        self
    }

    /// Switches the `Tunable` capability on (temperature and effort both unset).
    #[must_use]
    pub fn tunable(mut self) -> Self {
        self.tuning.get_or_insert_default();
        self
    }

    /// `Tunable` with this temperature.
    #[must_use]
    pub fn with_temperature(mut self, t: Option<f64>) -> Self {
        self.tuning.get_or_insert_default().temperature = t;
        self
    }

    /// `Tunable` with this effort.
    #[must_use]
    pub fn with_effort(mut self, e: Option<Effort>) -> Self {
        self.tuning.get_or_insert_default().effort = e;
        self
    }

    /// Switches the `ImageTunable` capability on, image output initially `on`.
    #[must_use]
    pub fn with_image_output(mut self, on: bool) -> Self {
        self.image_output = Some(on);
        self
    }

    /// Switches the `ImageGenTunable` capability on with these choice lists and current parameters.
    #[must_use]
    pub fn with_image_gen(mut self, options: ImageGenOptions, params: ImageGenParams) -> Self {
        self.image_gen = Some((options, params));
        self
    }

    /// Switches the `ImageEditJsonTunable` capability on, JSON edits initially `on`.
    #[must_use]
    pub fn with_json_edits(mut self, on: bool) -> Self {
        self.json_edits = Some(on);
        self
    }

    /// Switches the `ImagePartialProvider` capability on: `chat_observed` hands `frames` to the observer,
    /// in order, before answering the round.
    #[must_use]
    pub fn with_frames(mut self, frames: Vec<Vec<u8>>) -> Self {
        self.frames = Some(frames);
        self
    }

    // --- the script ----------------------------------------------------------

    /// Appends one round to the script.
    #[must_use]
    pub fn round(self, round: Round) -> Self {
        lock(&self.rounds).push_back(round);
        self
    }

    /// Appends rounds to the script.
    #[must_use]
    pub fn rounds(self, rounds: impl IntoIterator<Item = Round>) -> Self {
        lock(&self.rounds).extend(rounds);
        self
    }

    /// The round every call past the script gets (a copy each time; empty by default).
    #[must_use]
    pub fn tail(mut self, round: Round) -> Self {
        self.tail = round;
        self
    }

    /// `tail(Round::reply(s))`: every call past the script answers `s`.
    #[must_use]
    pub fn replying(self, s: &str) -> Self {
        self.tail(Round::reply(s))
    }

    /// Every call is answered by `f` instead of the script: `f(n, history)` with `n` 1-based.
    #[must_use]
    pub fn answering(
        mut self,
        f: impl Fn(usize, &[Message]) -> Round + Send + Sync + 'static,
    ) -> Self {
        self.answer = Some(Arc::new(f));
        self
    }

    /// `f(n, history)` runs inside every call, before it is answered.
    #[must_use]
    pub fn on_call(mut self, f: impl Fn(usize, &[Message]) + Send + Sync + 'static) -> Self {
        self.on_call = Some(Arc::new(f));
        self
    }

    // --- the tool-loop shapes ------------------------------------------------

    /// A tool provider requesting `calls` calls of `noop` per round until `stop_after` rounds, then the
    /// text `"done"`; `stop_after == 0` never stops (the runaway `--max-turns` guards against).
    pub fn looping(calls: u32, stop_after: u32) -> Self {
        let p = Self::new().with_tools();
        if stop_after == 0 {
            return p.answering(move |n, _| {
                Round::result(looping_round(u32::try_from(n).unwrap_or(u32::MAX), calls))
            });
        }
        p.rounds((1..=stop_after).map(|n| Round::result(looping_round(n, calls))))
            .replying("done")
    }

    /// A tool provider whose round `n` reports usage `{input: 100n, output: 10n, cache_read: n, total: 110n}`
    /// and calls `noop` until `stop_after`, then answers `"final answer"` (with usage once, then without);
    /// `fail_on = Some(n)` fails call `n` with `ProviderError::other("boom")` without consuming a round
    /// (`n` at most `stop_after + 1`).
    pub fn reporting(stop_after: u32, fail_on: Option<u32>) -> Self {
        let usage = |n: u32| {
            let n = u64::from(n);
            Usage {
                input: 100 * n,
                output: 10 * n,
                cache_read: n,
                total: 110 * n,
                ..Usage::default()
            }
        };
        let mut rounds: Vec<Round> = (1..=stop_after)
            .map(|n| Round::calls(vec![tool_call(&format!("c{n}"), "noop")]).usage(usage(n)))
            .collect();
        // The terminating round reports usage too.
        rounds.push(Round::reply("final answer").usage(usage(stop_after + 1)));
        if let Some(n) = fail_on {
            let at = usize::try_from(n).unwrap_or(usize::MAX).saturating_sub(1);
            rounds.insert(at.min(rounds.len()), Round::failing("boom"));
        }
        Self::new()
            .with_tools()
            .rounds(rounds)
            .replying("final answer")
    }

    /// A tool provider playing `rounds` verbatim (nothing streamed but `reasoning_done`), then answering
    /// `final_text` forever.
    pub fn scripted(rounds: Vec<RoundResult>, final_text: &str) -> Self {
        Self::new()
            .with_tools()
            .rounds(rounds.into_iter().map(Round::result))
            .replying(final_text)
    }

    // --- observation -----------------------------------------------------------

    /// Records into `log` — a handle a fixture created before the provider, so it can hand out the log
    /// without holding the provider.
    #[must_use]
    pub fn with_log(mut self, log: Log) -> Self {
        self.log = log;
        self
    }

    /// A handle on the call log that outlives this value.
    pub fn log(&self) -> Log {
        self.log.clone()
    }

    /// Calls so far, every entry point counted.
    pub fn calls(&self) -> usize {
        self.log.calls()
    }

    /// The history of every call, in order.
    pub fn sent(&self) -> Vec<Vec<Message>> {
        self.log.sent()
    }

    /// The history of call `n` (0-based).
    pub fn send(&self, n: usize) -> Vec<Message> {
        self.log.send(n)
    }

    /// The tool names advertised on each tool-path call.
    pub fn seen_tools(&self) -> Vec<Vec<String>> {
        self.log.seen_tools()
    }

    /// Records the call and picks its round: the hook runs first, then the answer closure or the script.
    fn next(&self, path: Path, messages: &[Message], tools: &[ToolDef]) -> Round {
        let n = self.log.record(Call {
            path,
            messages: messages.to_vec(),
            tools: tools.iter().map(|t| t.name.clone()).collect(),
        });
        if let Some(f) = &self.on_call {
            f(n, messages);
        }
        if let Some(f) = &self.answer {
            return f(n, messages);
        }
        lock(&self.rounds)
            .pop_front()
            .unwrap_or_else(|| self.tail.clone())
    }
}

/// Round `n` of [`FakeProvider::looping`]: `calls` requests for `noop` with ids `call-<n>` (or
/// `call-<n>-<i>` when a round carries several).
fn looping_round(n: u32, calls: u32) -> RoundResult {
    RoundResult {
        tool_calls: (1..=calls)
            .map(|i| {
                let id = if calls == 1 {
                    format!("call-{n}")
                } else {
                    format!("call-{n}-{i}")
                };
                tool_call(&id, "noop")
            })
            .collect(),
        ..RoundResult::default()
    }
}

impl Provider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
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
        Box::pin(async move {
            if !self.list_delay.is_zero() {
                tokio::time::sleep(self.list_delay).await;
            }
            self.models.clone().map_err(ProviderError::other)
        })
    }

    /// The unary path plays the same script: the next round's result as a `ChatResult`.
    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            self.next(Path::Chat, messages, &[])
                .settle(cancel)
                .map(chat_result)
        })
    }

    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        self.tools.then_some(self)
    }

    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        self.tuning.is_some().then_some(self as &mut dyn Tunable)
    }

    fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
        self.image_output
            .is_some()
            .then_some(self as &mut dyn ImageTunable)
    }

    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        self.image_gen
            .is_some()
            .then_some(self as &mut dyn ImageGenTunable)
    }

    fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
        self.json_edits
            .is_some()
            .then_some(self as &mut dyn ImageEditJsonTunable)
    }

    fn reports_usage(&self) -> bool {
        self.usage
    }

    fn as_image_partial_provider(&self) -> Option<&dyn ImagePartialProvider> {
        self.frames.is_some().then_some(self)
    }
}

impl ToolProvider for FakeProvider {
    /// Streams the round — reasoning, `reasoning_done`, content, tool deltas — then settles it.
    fn stream_chat_with_tools<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>> {
        Box::pin(async move {
            let round = self.next(Path::Tools, messages, tools);
            for d in &round.reasoning {
                sink.reasoning(d);
            }
            sink.reasoning_done();
            for d in &round.content {
                sink.content(d);
            }
            for (name, d) in &round.deltas {
                sink.tool_delta(name.as_deref(), d);
            }
            round.settle(cancel)
        })
    }
}

impl ImagePartialProvider for FakeProvider {
    fn chat_observed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        on_partial: &'a mut (dyn FnMut(&[u8]) + Send),
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async move {
            let round = self.next(Path::Observed, messages, &[]);
            for frame in self.frames.iter().flatten() {
                on_partial(frame);
            }
            round.settle(cancel).map(chat_result)
        })
    }
}

impl Tunable for FakeProvider {
    fn set_temperature(&mut self, t: Option<f64>) {
        self.tuning.get_or_insert_default().temperature = t;
    }

    fn temperature(&self) -> Option<f64> {
        self.tuning.and_then(|t| t.temperature)
    }

    fn set_effort(&mut self, e: Option<Effort>) {
        self.tuning.get_or_insert_default().effort = e;
    }

    fn effort(&self) -> Option<Effort> {
        self.tuning.and_then(|t| t.effort)
    }
}

impl ImageTunable for FakeProvider {
    fn set_image_output(&mut self, on: bool) {
        self.image_output = Some(on);
    }

    fn image_output(&self) -> bool {
        self.image_output.unwrap_or(false)
    }
}

impl ImageGenTunable for FakeProvider {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        if let Some(g) = &mut self.image_gen {
            g.1 = p;
        }
    }

    fn image_gen_params(&self) -> &ImageGenParams {
        self.image_gen
            .as_ref()
            .map_or(&NO_IMAGE_GEN_PARAMS, |g| &g.1)
    }

    fn image_gen_options(&self) -> ImageGenOptions {
        self.image_gen
            .as_ref()
            .map_or_else(ImageGenOptions::default, |g| g.0.clone())
    }
}

impl ImageEditJsonTunable for FakeProvider {
    fn set_json_edits(&mut self, on: bool) {
        self.json_edits = Some(on);
    }

    fn json_edits(&self) -> bool {
        self.json_edits.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{FakeProvider, Interrupt, Path, Round};
    use crate::provider::model::{Attachment, Message, ToolDef};
    use crate::provider::usage::Usage;
    use crate::provider::{
        ImageGenOptions, ImageGenParams, Provider, ProviderKind, RoundResult, ToolProvider,
    };
    use crate::testing::{RecordingSink, SinkEvent, tool_call};

    fn noop() -> [ToolDef; 1] {
        [ToolDef {
            name: "noop".to_owned(),
            ..ToolDef::default()
        }]
    }

    #[tokio::test]
    async fn looping_plays_its_rounds_then_answers_done_and_endlessly_when_told_to() {
        let cancel = CancellationToken::new();
        let tools = noop();

        // looping(1, 2): two rounds with one call each, then "done".
        let p = FakeProvider::looping(1, 2);
        let mut sink = RecordingSink::default();
        for n in 1..=2 {
            let r = p
                .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
                .await
                .expect("round");
            assert_eq!(r.tool_calls.len(), 1);
            assert_eq!(r.tool_calls[0].id, format!("call-{n}"));
            assert_eq!(r.tool_calls[0].name, "noop");
        }
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("final");
        assert_eq!(r.content, "done");
        assert!(r.tool_calls.is_empty());
        assert_eq!(p.calls(), 3);
        assert_eq!(p.seen_tools().len(), 3);
        assert_eq!(p.seen_tools()[0], vec!["noop".to_owned()]);
        assert_eq!(sink.events, vec![SinkEvent::ReasoningDone; 3]);

        let endless = FakeProvider::looping(3, 0);
        for n in 1..=100 {
            let r = endless
                .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
                .await
                .expect("round");
            assert_eq!(r.tool_calls.len(), 3);
            assert_eq!(r.tool_calls[2].id, format!("call-{n}-3"));
        }
    }

    #[tokio::test]
    async fn reporting_reports_usage_per_round_and_fails_the_named_call_without_consuming_one() {
        let cancel = CancellationToken::new();
        let tools = noop();
        let mut sink = RecordingSink::default();

        // reporting(2, Some(2)): round 1 reports usage, call 2 fails, round 2 is still queued.
        let p = FakeProvider::reporting(2, Some(2));
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("round 1");
        assert_eq!(r.tool_calls[0].id, "c1");
        assert_eq!(
            r.usage,
            Some(Usage {
                input: 100,
                output: 10,
                cache_read: 1,
                total: 110,
                ..Usage::default()
            })
        );
        let err = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect_err("boom");
        assert_eq!(err.to_string(), "boom");
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("round 2");
        assert_eq!(r.tool_calls[0].id, "c2");
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("final");
        assert_eq!(r.content, "final answer");
        assert_eq!(r.usage.map(|u| u.total), Some(330));
        let r = p
            .stream_chat_with_tools(&cancel, &[], &tools, &mut sink)
            .await
            .expect("past the script");
        assert_eq!(r.content, "final answer");
        assert!(r.usage.is_none());
    }

    #[tokio::test]
    async fn scripted_rounds_play_verbatim_on_both_paths() {
        let cancel = CancellationToken::new();
        let p = FakeProvider::scripted(
            vec![RoundResult {
                content: "scripted".to_owned(),
                usage: Some(Usage {
                    input: 1,
                    ..Usage::default()
                }),
                ..Default::default()
            }],
            "fin",
        );
        assert_eq!(p.kind(), ProviderKind::OpenAi);
        assert_eq!(p.model(), "gpt-test");
        assert!(p.as_tool_provider().is_some());
        assert!(p.list_models(&cancel).await.expect("models").is_empty());
        let c = p.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(c.text, "scripted");
        assert_eq!(c.usage.map(|u| u.input), Some(1));
        let c = p.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(c.text, "fin");
        assert!(c.usage.is_none());
    }

    #[tokio::test]
    async fn a_round_streams_reasoning_then_content_then_tool_deltas_and_can_interrupt_or_fail() {
        let cancel = CancellationToken::new();
        let held = CancellationToken::new();
        let p = FakeProvider::new()
            .with_tools()
            .round(Round {
                reasoning: vec!["th".to_owned(), "ink".to_owned()],
                deltas: vec![(Some("write".to_owned()), "{\"a\"".to_owned())],
                ..Round::text("hi")
            })
            .round(Round::failing("down").interrupting(Interrupt::Token(held.clone())))
            .round(Round::permanent("gone").interrupting(Interrupt::Call));
        let mut sink = RecordingSink::default();
        let r = p
            .stream_chat_with_tools(&cancel, &[], &[], &mut sink)
            .await
            .expect("round");
        assert_eq!(r.content, "hi");
        assert_eq!(
            sink.events,
            vec![
                SinkEvent::Reasoning("th".to_owned()),
                SinkEvent::Reasoning("ink".to_owned()),
                SinkEvent::ReasoningDone,
                SinkEvent::Content("hi".to_owned()),
            ]
        );
        assert!(!held.is_cancelled());
        let err = p
            .stream_chat_with_tools(&cancel, &[], &[], &mut sink)
            .await
            .expect_err("down");
        assert_eq!(err.to_string(), "down");
        assert!(held.is_cancelled());
        assert!(!cancel.is_cancelled());
        let err = p.chat(&cancel, &[]).await.expect_err("gone");
        assert_eq!(err.to_string(), "gone");
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn the_log_keeps_every_history_and_the_hook_and_answer_see_the_call_number() {
        let cancel = CancellationToken::new();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook = std::sync::Arc::clone(&seen);
        let p = FakeProvider::new()
            .on_call(move |n, _| crate::testing::lock(&hook).push(n))
            .answering(|n, messages| {
                let last = messages
                    .last()
                    .map(|m| m.content.as_str())
                    .unwrap_or_default();
                Round::reply(&format!("{n}: saw {last}"))
            });
        let log = p.log();
        let c = p
            .chat(&cancel, &[Message::user("one")])
            .await
            .expect("chat");
        assert_eq!(c.text, "1: saw one");
        let c = p
            .chat(&cancel, &[Message::user("one"), Message::user("two")])
            .await
            .expect("chat");
        assert_eq!(c.text, "2: saw two");
        assert_eq!(*crate::testing::lock(&seen), [1, 2]);
        drop(p);
        assert_eq!(log.calls(), 2);
        assert_eq!(log.prompts(), ["one", "two"]);
        assert_eq!(log.send(1).len(), 2);
        assert!(log.send(7).is_empty());
        assert_eq!(log.sent()[0], vec![Message::user("one")]);
        assert!(
            log.seen_tools().is_empty(),
            "the unary path advertises no tools"
        );
        assert!(!log.observed());

        // A log handed in is shared: a fixture can own it before the provider exists.
        let twin = FakeProvider::new().with_log(log.clone());
        twin.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(log.calls(), 3);
    }

    #[tokio::test]
    async fn capabilities_are_absent_until_switched_on() {
        let cancel = CancellationToken::new();
        let mut bare = FakeProvider::new();
        assert!(bare.as_tool_provider().is_none());
        assert!(bare.as_tunable().is_none());
        assert!(bare.as_image_tunable().is_none());
        assert!(bare.as_image_gen_tunable().is_none());
        assert!(bare.as_image_edit_json_tunable().is_none());
        assert!(bare.as_image_partial_provider().is_none());
        assert!(!bare.reports_usage());
        assert_eq!(bare.chat(&cancel, &[]).await.expect("chat").text, "");

        let mut full = FakeProvider::new()
            .with_kind(ProviderKind::Images)
            .with_model("gpt-image-1")
            .with_models(&["a", "b"])
            .reporting_usage()
            .with_temperature(Some(0.7))
            .with_image_output(true)
            .with_image_gen(
                ImageGenOptions {
                    image_sizes: vec!["auto"],
                    ..ImageGenOptions::default()
                },
                ImageGenParams::default(),
            )
            .with_json_edits(false)
            .with_frames(vec![b"f1".to_vec(), b"f2".to_vec()])
            .replying("ok");
        assert_eq!(full.kind(), ProviderKind::Images);
        assert_eq!(full.model(), "gpt-image-1");
        full.set_model("gpt-image-2".to_owned());
        assert_eq!(full.model(), "gpt-image-2");
        assert_eq!(full.list_models(&cancel).await.expect("models"), ["a", "b"]);
        assert!(full.reports_usage());
        let tuning = full.as_tunable().expect("tunable");
        assert_eq!(tuning.temperature(), Some(0.7));
        assert_eq!(tuning.effort(), None);
        tuning.set_effort(Some(crate::provider::Effort::High));
        assert_eq!(tuning.effort(), Some(crate::provider::Effort::High));
        let image = full.as_image_tunable().expect("image");
        assert!(image.image_output());
        image.set_image_output(false);
        assert!(!image.image_output());
        let knobs = full.as_image_gen_tunable().expect("image gen");
        assert_eq!(knobs.image_gen_options().image_sizes, ["auto"]);
        assert!(knobs.image_gen_params().is_empty());
        knobs.set_image_gen_params(ImageGenParams::from_raw("1:1", "", ""));
        assert_eq!(
            knobs.image_gen_params().aspect_ratio.as_deref(),
            Some("1:1")
        );
        let edits = full.as_image_edit_json_tunable().expect("json edits");
        assert!(!edits.json_edits());
        edits.set_json_edits(true);
        assert!(edits.json_edits());

        let mut frames = Vec::new();
        let c = full
            .as_image_partial_provider()
            .expect("partial")
            .chat_observed(&cancel, &[], &mut |f| frames.push(f.to_vec()))
            .await
            .expect("observed");
        assert_eq!(c.text, "ok");
        assert_eq!(frames, [b"f1".to_vec(), b"f2".to_vec()]);
        assert!(full.log().observed());
        assert_eq!(full.log().records()[0].path, Path::Observed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_or_delayed_model_listing() {
        let cancel = CancellationToken::new();
        let failing = FakeProvider::new().with_models_failing("models are down");
        assert_eq!(
            failing
                .list_models(&cancel)
                .await
                .expect_err("fail")
                .to_string(),
            "models are down"
        );
        let slow = FakeProvider::new()
            .with_models(&["m"])
            .with_models_after(Duration::from_secs(3));
        let started = tokio::time::Instant::now();
        assert_eq!(slow.list_models(&cancel).await.expect("models"), ["m"]);
        assert!(started.elapsed() >= Duration::from_secs(3));
    }

    #[tokio::test]
    async fn images_ride_the_round_on_both_paths() {
        let cancel = CancellationToken::new();
        let att = Attachment {
            filename: "image-1.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: vec![1, 2, 3],
        };
        let p = FakeProvider::new()
            .with_tools()
            .tail(Round::reply("").images(vec![att.clone()]))
            .round(Round::calls(vec![tool_call("c1", "noop")]));
        let mut sink = RecordingSink::default();
        let r = p
            .stream_chat_with_tools(&cancel, &[], &noop(), &mut sink)
            .await
            .expect("round");
        assert_eq!(r.tool_calls[0].id, "c1");
        let c = p.chat(&cancel, &[]).await.expect("chat");
        assert_eq!(c.images, [att]);
    }
}
