//! The interactive tool loop (chat/run.go:1449-1657 `toolLoop`) — the twin of the
//! headless `crate::headless::execute_with_tools`, not a fork of it.
//!
//! Go itself ships two loop functions over one helper layer, and this is the same split:
//! the round walk reuses `crate::headless::batch::{parallel_run, BatchOutcome}`,
//! `crate::tool::model_text`, the `Dispatcher` capability probes and
//! `crate::provider::model::Message`'s constructors verbatim. What is added here is everything a
//! terminal brings: streaming rendering through the transcript, `retry_round` per model
//! call, the approval gate, interactive surfaces, per-call cancel scopes and artifact
//! slots, steering injections, the context-meter call order, and per-round image
//! collection.
//!
//! Two things the headless loop has are deliberately ABSENT: a round cap and a turn
//! budget. Interactively the user is the brake (ESC cancels the turn, approval gates cover
//! mutating tools) — industry parity, and every round is granted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::headless::ChatError;
use crate::headless::batch::{BatchOutcome, parallel_run};
use crate::host::{Event, Kind, State};
use crate::provider::ToolProvider;
use crate::provider::model::{Message, ToolCall, ToolDef};
use crate::tool::Presentation;
use crate::tool::context::ArtifactSlot;
use crate::tool::context::RunCtx;
use crate::tool::fmt::{display_tool_name, tool_call_detail, tool_call_header};
use crate::ui::facade::UiError;

use crate::repl::context::meter::CtxMeter;
use crate::repl::render::styles::cyan;
use crate::repl::turn::approval::artifact_note;
use crate::repl::turn::retry::retry_round;
use crate::repl::turn::steer::Steerer;
use crate::repl::turn::{Turn, TurnFailure, TurnOutput, TurnReport, collect_images, stream_round};

/// What a refused call tells the model (chat/run.go:1570-1572).
const DECLINED: &str = "The user declined this call.";

/// One turn's rounds of streaming + tool execution (chat/run.go:1449-1657).
///
/// `tools` is the advertised set for round 0 — possibly EMPTY (T-36); from round 1 the
/// dispatcher is re-asked, because tools a `search_tools` round loaded (and
/// late-connecting MCP servers) must appear in the very next request, not next turn.
pub(crate) async fn tool_loop(
    t: &Turn<'_>,
    tp: &dyn ToolProvider,
    history: &mut Vec<Message>,
    ctxm: &mut CtxMeter,
    steer: &mut Steerer,
    tools: Vec<ToolDef>,
) -> TurnReport {
    let mut tools = tools;
    let mut side_fx: u32 = 0;
    let mut rounds: u32 = 0;
    loop {
        if rounds > 0 {
            tools = t.cx.dispatch.tools();
        }
        rounds += 1;

        // The send is composed ONCE per round: a retried attempt re-issues the SAME
        // request, so the rounds that already completed — and their side effects — stay
        // exactly where they are (chat/run.go:1418-1423).
        let send = crate::agents::compose_send_history(history, &t.cx.harness, &t.cx.overlay);
        let mut partial = String::new();
        let mut partial_reasoning = String::new();
        let round = retry_round(
            &t.cancel,
            t.cx.can_retry,
            |label| t.cx.ui.busy(label),
            |line| t.cx.tr.notice(&line),
            async || {
                let outcome = stream_round(t, tp, &send, &tools).await;
                partial = outcome.partial;
                partial_reasoning = outcome.partial_reasoning;
                outcome.result
            },
        )
        .await;
        let round = match round {
            Ok(r) => r,
            Err(e) => {
                return TurnReport {
                    outcome: Err(e.into()),
                    used_tools: false,
                    side_fx,
                    partial,
                    partial_reasoning,
                };
            }
        };
        if round.tool_calls.is_empty() {
            return TurnReport::success(
                TurnOutput {
                    content: round.content,
                    reasoning: round.reasoning,
                    images: round.images,
                    // The terminating round bills like any other; the run loop stamps it
                    // onto the assistant message it builds from this output (D-55).
                    usage: round.usage,
                    // Its raw blocks ride out the same way: the thinking block of a turn
                    // that ends in text has to replay on every later request
                    // (chat/run.go:1092-1099).
                    raw_content: round.raw_content,
                },
                side_fx,
            );
        }

        // The assistant message carries the calls, the round's own cost (the ONE record
        // call site, so live figures and what a resumed session recomputes cannot drift)
        // and the dialect's replay payload (Vertex thought signatures).
        let mut msg = Message::assistant_with_calls(
            round.content.clone(),
            round.tool_calls.clone(),
            round.raw_content.clone(),
        )
        .with_usage(round.usage);
        ctxm.record(Some(&mut msg));
        // T-39: a mid-loop round's images ride the message they were generated for, so a
        // persisted session keeps them (this is D-53's headless gap, closed interactively).
        // The directory resolves only when an image actually needs saving — the session
        // bundle's lazy-creation contract must survive image-less turns (images.go:115).
        if !round.images.is_empty() {
            collect_images(
                &t.cx.tr,
                usize::from(t.cx.ui.width()),
                (t.cx.images_dir)().as_deref(),
                &round.images,
                &mut msg,
            );
        }
        history.push(msg);
        // The round's stream just ended, so its real usage is fresh: settle here rather
        // than at turn end — the remaining rounds can run for minutes.
        ctxm.settle(history);

        if let Err(failure) = walk(t, &round.tool_calls, history, ctxm, &mut side_fx).await {
            return TurnReport::failed(failure, side_fx);
        }

        // Round boundary: anything the user typed while the round ran joins the
        // conversation NOW, so the next request carries it. `drain` already echoed the ❯
        // block (which settled the activity group) and booked the meter.
        history.extend(steer.drain(ctxm).await);

        // Frozen-mount defer (system-tools): schemas loaded this round ride into history
        // as a system message carrying tools — APPENDED at the bottom, never inserted, so
        // the provider-side prompt cache survives. The headless twin mounts at the round
        // TOP including before round 0; the asymmetry is deliberate (T-24).
        let pending = t.cx.dispatch.take_pending_loads();
        if !pending.is_empty() {
            let m = Message::system_tools(pending);
            ctxm.note(&m);
            history.push(m);
        }
    }
}

/// Executes one round's calls (chat/run.go:1487-1637): runs of consecutive
/// parallel-capable calls go out as one batch, everything else takes the serial path where
/// approval, interactive surfaces and expanded diffs live.
async fn walk(
    t: &Turn<'_>,
    calls: &[ToolCall],
    history: &mut Vec<Message>,
    ctxm: &mut CtxMeter,
    side_fx: &mut u32,
) -> Result<(), TurnFailure> {
    let dispatch = Arc::clone(&t.cx.dispatch);
    let mut i = 0;
    while i < calls.len() {
        // Batching a RUN rather than partitioning the round keeps a writer separating the
        // readers before it from the readers after, and leaves the serial path — where
        // approval, surfaces and file mutation all live — untouched.
        let j = parallel_run(&*dispatch, calls, i);
        if j - i >= 2 {
            let msgs = run_parallel_batch(t, &calls[i..j]).await;
            for m in &msgs {
                ctxm.note(m);
            }
            history.extend(msgs);
            if t.cancel.is_cancelled() {
                // The calls that completed already had their effects, so their results
                // must answer their calls — appended above, then the turn ends.
                return Err(interrupted());
            }
            i = j;
            continue;
        }
        let tc = &calls[i];
        i += 1;
        let header = cyan(&tool_call_header(&*dispatch, tc));
        let mode = dispatch.presentation(&tc.name);

        if mode == Presentation::Surface {
            surface_call(t, tc, history, ctxm, side_fx).await;
            if t.cancel.is_cancelled() {
                return Err(interrupted());
            }
            continue;
        }

        // The lifecycle widget the whole activity group shares; an expanded call
        // (a file mutation) instead takes a STANDALONE widget — a group boundary — and
        // settles into its diff block.
        let expanded = mode == Presentation::Expanded;
        if expanded {
            t.cx.tr.open_showcase(&header);
        } else {
            t.cx.tr.open_call(&header);
        }

        if dispatch.requires_approval(&tc.name) {
            let detail = tool_call_detail(&*dispatch, tc);
            let allowed = match t.cx.gate.ask(&t.cancel, &tc.name, &detail).await {
                Ok(a) => a,
                // A fired cancel scope resolves the blocking call as Interrupted (WP45);
                // a closed facade ends the loop itself.
                Err(UiError::Interrupted) => return Err(interrupted()),
                Err(e) => return Err(TurnFailure::Ui(e)),
            };
            if !allowed {
                if expanded {
                    t.cx.tr.settle_showcase(&header, None, DECLINED, true);
                } else {
                    t.cx.tr
                        .finish_call(&header, DECLINED, true, Duration::ZERO, "");
                }
                history.push(Message::tool_result(tc, DECLINED, true));
                continue;
            }
        }

        // Every call gets its own artifact slot — the user-facing payload (an expanded
        // call's diff) kept out of the result text so it is never billed to the model.
        // Only some calls post to one (T-35).
        let slot = ArtifactSlot::default();
        let call_cancel = t.cancel.child_token();
        let cx = RunCtx {
            cancel: call_cancel.clone(),
            artifact: Some(slot.clone()),
            ..RunCtx::default()
        };
        if !dispatch.supports_parallel(&tc.name, Some(&tc.arguments)) {
            *side_fx += 1; // counted at execution: this is what a replay would re-run
        }
        // ESC cancels the CALL (the innermost scope); Ctrl+C the turn.
        let scope = t.cx.ui.push_cancel_scope(call_cancel.clone());
        let started = Instant::now();
        let out = dispatch
            .call_tool(&cx, &tc.name, tc.arguments.clone())
            .await;
        let dur = started.elapsed();
        scope.pop();
        call_cancel.cancel();
        let (text, is_error) = crate::tool::model_text(out);
        let art = slot.take();
        if expanded {
            t.cx.tr
                .settle_showcase(&header, art.as_ref(), &text, is_error);
        } else {
            t.cx.tr
                .finish_call(&header, &text, is_error, dur, &artifact_note(art.as_ref()));
        }
        let result = Message::tool_result(tc, text, is_error);
        // A tool result consumes context the moment it lands — a big file read should move
        // the meter now, not a round later.
        ctxm.note(&result);
        history.push(result);
        if t.cancel.is_cancelled() {
            return Err(interrupted());
        }
    }
    Ok(())
}

/// An interactive tool's call (chat/run.go:1513-1546): it brings its own surface, so the
/// activity panel stays out of the way, the group clock freezes while the user answers,
/// and the Q&A lands as its own `?` record block.
async fn surface_call(
    t: &Turn<'_>,
    tc: &ToolCall,
    history: &mut Vec<Message>,
    ctxm: &mut CtxMeter,
    side_fx: &mut u32,
) {
    if !t
        .cx
        .dispatch
        .supports_parallel(&tc.name, Some(&tc.arguments))
    {
        *side_fx += 1; // a replay would re-ask the user
    }
    t.cx.tr.pause_for_input("waiting for your input");
    t.cx.pres.set_state(State::NeedsInput);
    t.cx.pres.notify(Event {
        kind: Kind::NeedsInput,
        text: format!("{} needs your input", display_tool_name(&tc.name)),
    });
    let call_cancel = t.cancel.child_token();
    let cx = RunCtx {
        cancel: call_cancel.clone(),
        ..RunCtx::default()
    };
    let scope = t.cx.ui.push_cancel_scope(call_cancel.clone());
    let out =
        t.cx.dispatch
            .call_tool(&cx, &tc.name, tc.arguments.clone())
            .await;
    scope.pop();
    call_cancel.cancel();
    t.cx.tr.resume_from_input();
    t.cx.pres.set_state(State::Busy);
    let (text, is_error) = crate::tool::model_text(out);
    t.cx.tr.ask_record(&text, is_error);
    let result = Message::tool_result(tc, text, is_error);
    ctxm.note(&result);
    history.push(result);
}

/// One batch of concurrent calls (chat/parallel.go `runParallelBatch`): ONE widget opened
/// on the first call's header, ONE cancel scope for the whole run (ESC cancels the run,
/// not a member of it — a half-cancelled batch would leave calls without results), then
/// event rows AND results in CALL order regardless of who finished first.
///
/// The concurrency itself is `crate::headless::batch::run_batch`'s shape, re-spelled here for
/// ONE reason: a batch needs a per-call artifact slot (a shared one would be a race with a
/// last-writer-wins result), and the headless helper passes one context to every call.
/// The order law — `parallel_run` — is the shared original.
async fn run_parallel_batch(t: &Turn<'_>, calls: &[ToolCall]) -> Vec<Message> {
    let dispatch = Arc::clone(&t.cx.dispatch);
    let headers: Vec<String> = calls
        .iter()
        .map(|tc| cyan(&tool_call_header(&*dispatch, tc)))
        .collect();
    // finishCall relabels the widget to "Working…" as the rows land, so the batch needs no
    // label of its own.
    if let Some(first) = headers.first() {
        t.cx.tr.open_call(first);
    }

    let batch_cancel = t.cancel.child_token();
    let scope = t.cx.ui.push_cancel_scope(batch_cancel.clone());
    let outcomes = futures::future::join_all(calls.iter().map(|tc| {
        let dispatch = Arc::clone(&dispatch);
        let cancel = batch_cancel.clone();
        async move {
            let slot = ArtifactSlot::default();
            let cx = RunCtx {
                cancel,
                artifact: Some(slot.clone()),
                ..RunCtx::default()
            };
            let started = Instant::now();
            let out = dispatch
                .call_tool(&cx, &tc.name, tc.arguments.clone())
                .await;
            let (text, is_error) = crate::tool::model_text(out);
            (
                BatchOutcome {
                    text,
                    is_error,
                    duration: started.elapsed(),
                },
                artifact_note(slot.take().as_ref()),
            )
        }
    }))
    .await;
    scope.pop();
    batch_cancel.cancel();

    let mut msgs = Vec::with_capacity(calls.len());
    for ((tc, header), (o, note)) in calls.iter().zip(&headers).zip(&outcomes) {
        t.cx.tr
            .finish_call(header, &o.text, o.is_error, o.duration, note);
        msgs.push(Message::tool_result(tc, o.text.clone(), o.is_error));
    }
    msgs
}

/// The user cancelled: no partials (a tool walk streams nothing), so the turn's finalize
/// keeps whatever rounds completed.
fn interrupted() -> TurnFailure {
    TurnFailure::Chat(ChatError::Interrupted)
}

#[cfg(test)]
mod tests;
