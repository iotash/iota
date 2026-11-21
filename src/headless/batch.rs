//! Parallel tool-call batches (chat/parallel.go): the maximal run of consecutive parallel-capable calls, its
//! concurrent execution, and the tool-result message per call.

use std::time::{Duration, Instant};

use crate::provider::model::ToolCall;
use crate::tool::Dispatcher;
use crate::tool::approval::{Gate, gate};
use crate::tool::context::RunCtx;
use crate::tool::grants::Grants;

/// End index of the maximal run of consecutive calls from i that may batch; j == i when calls\[i\] may not
/// (parallel.go:42-48). A call batches when it is parallel-capable (`dispatch.supports_parallel(name,
/// Some(args))`) AND [`gate`] runs it unasked: the batch has no approval gate, so a call the gate would ask
/// takes the serial path, where it is asked — the invariant is this walk's, not each tool's (DIVERGENCES
/// X-05). Both are asked per CALL, so a round mixing batchable and serial calls to the SAME tool splits at
/// the boundaries.
pub fn parallel_run(
    dispatch: &dyn Dispatcher,
    grants: &Grants,
    calls: &[ToolCall],
    i: usize,
) -> usize {
    let mut j = i;
    while j < calls.len() && batches(dispatch, grants, &calls[j]) {
        j += 1;
    }
    j
}

fn batches(dispatch: &dyn Dispatcher, grants: &Grants, tc: &ToolCall) -> bool {
    dispatch.supports_parallel(&tc.name, Some(&tc.arguments))
        && gate(dispatch, grants, &tc.name, &tc.arguments) == Gate::Run
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
