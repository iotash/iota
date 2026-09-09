//! Parallel tool-call batches (chat/parallel.go): the maximal run of consecutive parallel-capable calls, its
//! concurrent execution, and the tool-result message per call.

use std::time::{Duration, Instant};

use crate::chat::turns::RunCtx;
use crate::provider::model::ToolCall;
use crate::tool::Dispatcher;

/// End index of the maximal run of consecutive calls from i with `dispatch.supports_parallel(name, Some(args))`;
/// j == i when calls\[i\] is not (parallel.go:42-48). The capability is asked per CALL, so a round mixing
/// concurrent-safe and serial calls to the SAME tool splits at the boundaries.
pub fn parallel_run(dispatch: &dyn Dispatcher, calls: &[ToolCall], i: usize) -> usize {
    let mut j = i;
    while j < calls.len() && dispatch.supports_parallel(&calls[j].name, Some(&calls[j].arguments)) {
        j += 1;
    }
    j
}

/// The outcome of one call inside a batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Model-facing result text (or `Error calling tool: …` on a hard error).
    pub text: String,
    /// Whether the call failed.
    pub is_error: bool,
    /// Wall-clock duration of the call.
    pub duration: Duration,
}

/// `futures::future::join_all` over per-call futures; hard error → text "Error calling tool: {e}", `is_error`
/// true; outcomes in CALL order regardless of who finished first (parallel.go:106-127). A cancelled run still
/// gets one outcome per call: the tools observe `cx.cancel` and answer with their own error.
pub async fn run_batch(
    cx: &RunCtx,
    dispatch: &dyn Dispatcher,
    calls: &[ToolCall],
) -> Vec<BatchOutcome> {
    futures::future::join_all(calls.iter().map(|tc| async move {
        let started = Instant::now();
        let (text, is_error) =
            crate::tool::model_text(dispatch.call_tool(cx, &tc.name, tc.arguments.clone()).await);
        BatchOutcome {
            text,
            is_error,
            duration: started.elapsed(),
        }
    }))
    .await
}
