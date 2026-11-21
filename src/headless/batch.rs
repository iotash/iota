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

#[cfg(test)]
mod tests {
    use super::parallel_run;
    use crate::provider::model::{JsonObject, ToolCall};
    use crate::testing::StaticDispatcher;
    use crate::tool::grants::{Grants, Level, ToolKey};

    fn calls(names: &[&str]) -> Vec<ToolCall> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| ToolCall {
                id: format!("c{i}"),
                name: (*n).to_owned(),
                arguments: JsonObject::new(),
            })
            .collect()
    }

    /// A parallel-capable call the gate would ask is never in a batch — it ends the run before it, and a run
    /// of them is a run of serial calls; a tool grant that answers the question lets it batch again.
    #[test]
    fn a_call_the_gate_would_ask_never_batches() {
        let d = StaticDispatcher::new(&["read", "sh"])
            .with_parallel(&["read", "sh"])
            .with_approval(&["sh"]);
        let grants = Grants::default();
        let round = calls(&["read", "read", "sh", "sh", "read"]);
        assert_eq!(
            parallel_run(&d, &grants, &round, 0),
            2,
            "the asked call ends the run"
        );
        assert_eq!(
            parallel_run(&d, &grants, &round, 2),
            2,
            "an asked call is a run of none"
        );
        assert_eq!(parallel_run(&d, &grants, &round, 3), 3);

        grants.update(Level::Session, |s| {
            s.add_tool(ToolKey::Builtin("sh".to_owned()))
        });
        assert_eq!(
            parallel_run(&d, &grants, &round, 0),
            5,
            "granted, it runs unasked and batches"
        );
    }
}
