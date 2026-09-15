//! One run through the model (chat/chat.go:73-127,284-379): the unary path, the tool loop
//! `execute_with_tools`, and the headless `QuietHost` that answers every approval request with a refusal.

use std::num::NonZeroU32;
use std::{path::Path, sync::Arc};

use crate::agents::{Overlay, compose_send_history};
use crate::provider::model::{
    AssistantBody, Attachment, Body, Message, RawContent, ToolCall, ToolDef,
};
use crate::provider::sink::NullSink;
use crate::provider::usage::Usage;
use crate::provider::{Provider, RoundResult, ToolProvider};
use crate::tool::Dispatcher;
use crate::tool::context::{BudgetExt, RunCtx};

use crate::chat::AgentOptions;
use crate::chat::batch::{parallel_run, run_batch};
use crate::chat::error::ChatError;
use crate::chat::images::save_images_for_turn;
use crate::chat::report::{RunRecorder, tool_names};

/// A test-injected approval oracle: `(approved, refusal text)` for a tool call and its header detail.
pub(crate) type Approver = Box<dyn Fn(&ToolCall, &str) -> (bool, String) + Send + Sync>;

/// The headless host: records every round and refuses every approval request (nobody is there to ask). It
/// also carries the run's background-job registry, because a headless run has no idle loop for a finished
/// job to wake — the tool loop itself is where a notice can enter and where the run waits for one.
#[derive(Default)]
pub struct QuietHost {
    /// Per-round accounting of this run.
    pub rec: RunRecorder,
    /// `None` headlessly (always); tests inject an `Approver`.
    pub approve: Option<Approver>,
    /// The run's background jobs; `None` = this run has none and never waits.
    pub jobs: Option<Arc<crate::shell::jobs::Jobs>>,
}

impl QuietHost {
    /// A host with no approver (headless ALWAYS); tests inject an `Approver`.
    pub fn new() -> Self {
        Self::default()
    }

    /// approve None → `(false, refusal_text(&tc.name))`; else the approver's answer verbatim.
    /// `detail` = `dispatch.header_summary(&tc.name, &tc.arguments).unwrap_or_default()` — Go's fallback
    /// sorted-key argument digest (chat.go:461-480, `toolHeaderMaxArgs`/`truncateRunes`) is NOT ported
    /// (DIVERGENCES D-12); with no built-in `header_summary` the headless detail is always "".
    /// `test_quiet_loop_forwards_approval` pins "".
    pub fn ask_approval(&self, tc: &ToolCall, detail: &str) -> (bool, String) {
        match &self.approve {
            None => (false, refusal_text(&tc.name)),
            Some(approve) => approve(tc, detail),
        }
    }
}

/// The model-facing text of a refused tool call (byte-equal to Go; `name` is the WIRE name).
pub fn refusal_text(name: &str) -> String {
    format!(
        "{name} was not executed: it requires interactive approval, which is unavailable in this non-interactive run. Set the toolset's auto-approve option (tools.code.auto_write / tools.shell.auto_run) to permit it here."
    )
}

/// What one run sends.
#[derive(Clone, Debug, Default)]
pub struct RunRequest {
    /// The user message.
    pub message: String,
    /// The system prompt (`""` = none).
    pub system: String,
    /// Agent-mode overlay settings.
    pub agent: AgentOptions,
    /// Imported history (a resumed session's view). A NON-EMPTY history WINS over `system` (chat/run.go:69-74):
    /// the resumed session keeps the system message from its own log and `-s` is ignored.
    pub history: Vec<Message>,
}

/// What one run produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOutcome {
    /// The final reply text.
    pub reply: String,
    /// Absolute paths of the images saved.
    pub images: Vec<String>,
    /// `saving image failed: …` lines.
    pub image_errors: Vec<String>,
    /// The turn's message delta — Go's `history[persisted:]` (chat/run.go:221-229): everything appended after the
    /// imported history, which is exactly what a session persists. Always populated; the stateless path just
    /// ignores it.
    pub delta: Vec<Message>,
}

/// Installs the dispatcher's tool search as the provider's tool-searcher closure when the provider has a
/// `ToolSearchHost` (chat.go:95-101) — what makes `defer_mode: tool-search` work headlessly. A dispatcher
/// without a searcher answers every query with no hits.
pub fn install_tool_searcher(provider: &mut dyn Provider, dispatch: &Arc<dyn Dispatcher>) {
    if let Some(host) = provider.as_tool_search_host() {
        let dispatch = Arc::clone(dispatch);
        host.set_tool_searcher(Some(Arc::new(move |q| {
            dispatch
                .as_tool_searcher()
                .map_or_else(Vec::new, |s| s.search_tools(q))
        })));
    }
}

/// messages = `req.history` (the watermark) + [system?] + user — the system message is pushed ONLY when the
/// imported history is empty, so a resumed session keeps its own (chat/run.go:68-74); overlay composed ONCE via
/// `Overlay::new(root, cwd|root, home)` when `agent.enabled` (NOT called otherwise — `Overlay` may stay a stub);
/// tools = `dispatch.tools()`;
/// `as_tool_provider && !tools.is_empty()` → `execute_with_tools` → images = `outcome.images` (the TERMINATING
/// round's `RoundResult.images`, chat.go:112 `saveImagesQuiet(tp)` after the loop);
/// else `provider.chat(compose_send_history(..))` + `rec.observe(usage, vec![])` → images = `result.images`
/// (chat.go:122).
/// BOTH paths then run `save_images_for_turn(&images, images_dir)`; images saved only when `images_dir` is Some
/// (children pass None — POLICY I-06).
/// The final assistant message (Go's `amsg`, chat/run.go:1092-1095) is then pushed onto the history with the
/// terminating round's usage and the SAVED image subset, and `RunOutcome.delta` is everything past the
/// watermark — what a session persists.
/// A run whose token is already cancelled fails with `ChatError::Interrupted` before it reaches the provider
/// (DIVERGENCES I-03; Go ran under `context.Background()` and could not be interrupted at all).
pub async fn run_once(
    cx: &RunCtx,
    provider: &dyn Provider,
    req: &RunRequest,
    dispatch: Arc<dyn Dispatcher>,
    max_turns: Option<NonZeroU32>,
    host: &mut QuietHost,
    images_dir: Option<&Path>,
) -> Result<RunOutcome, ChatError> {
    if cx.cancel.is_cancelled() {
        return Err(ChatError::Interrupted);
    }
    // The imported history is the watermark: everything past it is this turn's delta (chat/run.go:68-74).
    let mut messages = req.history.clone();
    let watermark = messages.len();
    if messages.is_empty() && !req.system.is_empty() {
        messages.push(Message::system(req.system.clone()));
    }
    messages.push(Message::user(req.message.clone()));

    // Agent mode composes the AGENTS.md/skills overlay once for the single send (chat.go:80-92).
    let overlay = if req.agent.enabled {
        let root = req.agent.root.as_path();
        let cwd = req.agent.cwd.as_deref().unwrap_or(root);
        Overlay::new(root, cwd, req.agent.home.as_deref()).content()
    } else {
        String::new()
    };

    let tools = dispatch.tools();
    let outcome = match provider.as_tool_provider() {
        Some(tp) if !tools.is_empty() => {
            execute_with_tools(
                cx,
                tp,
                dispatch,
                &mut messages,
                tools,
                &overlay,
                max_turns,
                host,
            )
            .await?
        }
        _ => {
            // The UNARY chat, not the streaming call (chat.go:116).
            let send = compose_send_history(&messages, &overlay);
            let result = provider.chat(&cx.cancel, &send).await?;
            // The tool loop records each of its rounds; this path has exactly one (chat.go:121).
            host.rec.observe(result.usage, Vec::new());
            // `ChatResult` carries no reasoning: the unary path has no thinking text to persist.
            LoopOutcome {
                content: result.text,
                reasoning: String::new(),
                images: result.images,
                usage: result.usage,
                raw_content: None,
            }
        }
    };
    let saved = save_images_for_turn(&outcome.images, images_dir);
    // Go's `amsg` (chat/run.go:1092-1095): the final assistant message carries the terminating round's cost and
    // exactly `collectImages`' saved subset, so a persisted turn is complete.
    messages.push(Message {
        content: outcome.content.clone(),
        attachments: saved.attachments,
        body: Body::Assistant(AssistantBody {
            reasoning: outcome.reasoning,
            raw_content: outcome.raw_content,
            usage: outcome.usage,
            ..AssistantBody::default()
        }),
    });
    let delta = messages.split_off(watermark);
    Ok(RunOutcome {
        reply: outcome.content,
        images: saved.paths,
        image_errors: saved.failures,
        delta,
    })
}

/// What the loop hands back: Go's `(reply, think, err)` PLUS `LastImages()` of the final round, which `runOnce`
/// read afterwards.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoopOutcome {
    /// Visible content of the terminating round.
    pub content: String,
    /// Reasoning text of the terminating round.
    pub reasoning: String,
    /// Images the terminating round generated.
    pub images: Vec<Attachment>,
    /// The TERMINATING round's usage, so `run_once` can stamp the final assistant message (chat/run.go:1093).
    pub usage: Option<Usage>,
    /// Raw blocks of the terminating round. A thinking block must go back on every later request that
    /// replays this turn, and a turn ending in text is the common case (chat/run.go:1092-1099).
    pub raw_content: Option<RawContent>,
}

/// Appends one [`crate::provider::model::Body::Notice`] message per job that finished since the last check.
/// A run with no registry (every test that builds a bare `QuietHost`) appends nothing.
fn push_job_notices(host: &QuietHost, history: &mut Vec<Message>) {
    let Some(jobs) = &host.jobs else { return };
    for done in jobs.take_finished() {
        history.push(Message::notice(crate::shell::jobs::notice_text(&done)));
    }
}

/// Blocks for the next background job to finish. `None` = there is nothing to wait for (no registry, no
/// runner) or the run was cancelled — either way the caller returns its reply instead of spending a round.
async fn wait_for_job(host: &QuietHost, cx: &RunCtx) -> Option<crate::shell::jobs::JobDone> {
    let jobs = host.jobs.as_ref()?;
    if jobs.running() == 0 {
        return jobs.take_finished().into_iter().next();
    }
    jobs.wait_any(&cx.cancel).await
}

/// Round order per chat.go:284-379 (see ARCHITECTURE §8): cancellation check (I-03) → local cap →
/// `budget.take()` → live `tools()` after round 0 → `take_pending_loads()` mount → the request with
/// `compose_send_history(history, overlay)` and a `NullSink` → `rec.observe` → termination (reasoning-only rule) →
/// assistant message with `raw_content` and the round's `usage` (D-55) → execution walk (`parallel_run` /
/// `run_batch` / serial with the approval gate). `images` and `usage` on the outcome are the terminating
/// (no-tool-call) round's; earlier rounds' images are dropped (Go: `LastImages` is per call) and are therefore
/// never persisted (D-53).
#[allow(clippy::too_many_arguments)] // frozen contract signature (CONTRACTS §6.3)
pub async fn execute_with_tools(
    cx: &RunCtx,
    tp: &dyn ToolProvider,
    dispatch: Arc<dyn Dispatcher>,
    history: &mut Vec<Message>,
    mut tools: Vec<ToolDef>,
    overlay: &str,
    max_turns: Option<NonZeroU32>,
    host: &mut QuietHost,
) -> Result<LoopOutcome, ChatError> {
    let mut rounds: u32 = 0;
    loop {
        if cx.cancel.is_cancelled() {
            return Err(ChatError::Interrupted);
        }
        // Jobs that finished while the last round ran enter here — before the request is composed, so the
        // model sees them in the same turn it would have seen a tool result.
        push_job_notices(host, history);
        // Two caps, and they are different things: max_turns bounds THIS loop, while the budget is the
        // whole run's.
        if let Some(cap) = max_turns
            && rounds == cap.get()
        {
            return Err(ChatError::LocalCap { turns: cap });
        }
        if !cx.budget.take() {
            return Err(ChatError::SharedCap {
                turns: cx.budget.cap(),
            });
        }
        if rounds > 0 {
            // The advertised set is LIVE: tools a search_tools round loaded (and late-connecting MCP servers)
            // must appear the very next request, not next turn.
            tools = dispatch.tools();
        }
        // Frozen-mount defer (system-tools): loaded schemas append to history as a system message carrying
        // tools.
        let pending = dispatch.take_pending_loads();
        if !pending.is_empty() {
            history.push(Message::system_tools(pending));
        }

        let round = {
            let send = compose_send_history(history, overlay);
            let mut sink = NullSink;
            tp.stream_chat_with_tools(&cx.cancel, &send, &tools, &mut sink)
                .await?
        };
        // The accounting is read NOW, from the round that incurred it (chat.go:314-317).
        host.rec.observe(round.usage, tool_names(&round.tool_calls));
        let RoundResult {
            content,
            reasoning,
            tool_calls,
            raw_content,
            images,
            usage,
        } = round;
        if tool_calls.is_empty() {
            let outcome = if content.is_empty() && !reasoning.is_empty() {
                // Reasoning-only response: the reasoning IS the answer (chat.go:319-322).
                LoopOutcome {
                    content: reasoning.clone(),
                    reasoning,
                    images,
                    usage,
                    raw_content,
                }
            } else {
                LoopOutcome {
                    content,
                    reasoning,
                    images,
                    usage,
                    raw_content,
                }
            };
            // The model is done, but the run is not while a background job it started is still going: a
            // headless run has no idle loop to wake it, so this IS the wait. The reply lands in the history
            // first (it is a real turn, and `run_once` must not append it twice), then the notice, then one
            // more round — billed to `--max-turns` like any other (DIVERGENCES X-09).
            let Some(done) = wait_for_job(host, cx).await else {
                return Ok(outcome);
            };
            // Images this round generated are dropped, exactly as any other non-terminating round's are
            // (D-53: `LastImages` is per call, and only the round that ends the loop is saved).
            history.push(Message::assistant_body(
                outcome.content,
                AssistantBody {
                    reasoning: outcome.reasoning,
                    raw_content: outcome.raw_content,
                    usage: outcome.usage,
                    ..AssistantBody::default()
                },
            ));
            history.push(Message::notice(crate::shell::jobs::notice_text(&done)));
            rounds += 1;
            continue;
        }

        // Reasoning is NOT stored on the message in the quiet loop; raw model content (Vertex thought
        // signatures, …) is, so later rounds replay it (chat.go:326-331). The round's cost rides the message it
        // paid for, so a persisted session sums to what the run actually spent (DIVERGENCES D-55).
        history.push(
            Message::assistant_with_calls(content, tool_calls.clone(), raw_content)
                .with_usage(usage),
        );

        let mut i = 0;
        while i < tool_calls.len() {
            // A run of concurrent-safe calls goes out together; a run of length 1 (or 0) falls through to the
            // serial path with its approval gate (chat.go:333-345).
            let j = parallel_run(&*dispatch, &tool_calls, i);
            if j - i >= 2 {
                let batch = &tool_calls[i..j];
                let outcomes = run_batch(cx, &*dispatch, batch).await;
                for (tc, o) in batch.iter().zip(&outcomes) {
                    history.push(Message::tool_result(tc, o.text.clone(), o.is_error));
                }
                i = j;
                continue;
            }
            let tc = &tool_calls[i];
            i += 1;
            // Approval gate: with nobody to ask, refuse and say how to enable the call (chat.go:348-364).
            if dispatch.requires_approval(&tc.name) {
                let detail = dispatch
                    .header_summary(&tc.name, &tc.arguments)
                    .unwrap_or_default();
                let (allowed, why) = host.ask_approval(tc, &detail);
                if !allowed {
                    history.push(Message::tool_result(tc, why, true));
                    continue;
                }
            }
            let (text, is_error) = crate::tool::model_text(
                dispatch.call_tool(cx, &tc.name, tc.arguments.clone()).await,
            );
            history.push(Message::tool_result(tc, text, is_error));
        }
        rounds += 1;
    }
}

#[cfg(test)]
mod tests {
    use crate::provider::model::ToolCall;

    use super::{QuietHost, refusal_text};

    #[test]
    fn refusal_names_the_wire_name() {
        let tc = ToolCall {
            name: "mcp__srv__edit".to_owned(),
            ..ToolCall::default()
        };
        let (ok, why) = QuietHost::new().ask_approval(&tc, "ignored");
        assert!(!ok);
        assert_eq!(why, refusal_text("mcp__srv__edit"));
        assert!(
            why.starts_with("mcp__srv__edit was not executed: it requires interactive approval, ")
        );
        assert!(why.ends_with("(tools.code.auto_write / tools.shell.auto_run) to permit it here."));
    }
}
