//! Parallel batches (`chat/parallel_test.go`): run boundaries, per-call capability, the batch's concurrency and
//! call-order guarantees, and the quiet loop batching on its own.

use std::sync::Arc;

use iota::headless::batch::{parallel_run, run_batch};
use iota::headless::{QuietHost, execute_with_tools};
use iota::provider::RoundResult;
use iota::provider::model::{Message, Role};
use iota::testing::{FakeProvider, NoCapDispatch, ParallelDispatch, tool_call, tool_call_with};
use iota::tool::context::RunCtx;
use tokio_util::sync::CancellationToken;

#[test]
fn a_parallel_run_ends_at_the_first_serial_call() {
    // parallel_run batches only a RUN of consecutive parallel-capable calls: a serial tool separates the ones
    // before it from the ones after.
    let d = ParallelDispatch::by_name(&["read_file", "grep"]);
    let calls = [
        tool_call("1", "read_file"),
        tool_call("2", "grep"),
        tool_call("3", "edit_file"),
        tool_call("4", "read_file"),
        tool_call("5", "read_file"),
        tool_call("6", "read_file"),
    ];
    for (from, want) in [
        (0, 2), // the leading pair
        (2, 2), // edit_file batches nothing
        (3, 6), // the trailing run
        (5, 6),
        (6, 6), // past the end
    ] {
        assert_eq!(
            parallel_run(&d, &calls, from),
            want,
            "parallel_run(from={from})"
        );
    }
}

#[test]
fn a_parallel_run_splits_calls_to_one_tool_by_their_arguments() {
    // The same boundaries hold when the calls share a NAME and differ only in their arguments — the per-call
    // shape.
    let d = ParallelDispatch::by_agent(&[("search", true), ("implement", false)]);
    let task = |id: &str, agent: &str| tool_call_with(id, "task", &[("agent", agent)]);
    let calls = [
        task("1", "search"),
        task("2", "search"),
        task("3", "implement"),
        task("4", "search"),
    ];
    for (from, want) in [
        (0, 2), // the two searches batch
        (2, 2), // the write-capable one runs alone
        (3, 4), // and the search after it batches again
    ] {
        assert_eq!(
            parallel_run(&d, &calls, from),
            want,
            "parallel_run(from={from})"
        );
    }
}

#[test]
fn a_dispatcher_without_the_capability_serializes_every_call() {
    // A dispatcher without the capability serializes everything.
    let plain = NoCapDispatch;
    let calls = [tool_call("1", "read_file"), tool_call("2", "read_file")];
    assert_eq!(parallel_run(&plain, &calls, 0), 0);
    assert_eq!(parallel_run(&plain, &calls, 1), 1);
}

// The batch core alone; the transcript widget is interactive-only.
#[tokio::test]
async fn a_parallel_batch_runs_its_calls_concurrently() {
    // The calls in a batch really do overlap: every call must reach the barrier before any is allowed to
    // finish; serial execution would deadlock here, which is the assertion.
    const N: usize = 4;
    let d = ParallelDispatch::by_name(&["read_file"]).with_barrier(N);
    let calls: Vec<_> = (0..N)
        .map(|i| {
            tool_call(
                &format!("{}", char::from(b'a' + u8::try_from(i).unwrap())),
                "read_file",
            )
        })
        .collect();
    let outcomes = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_batch(&RunCtx::default(), &d, &calls),
    )
    .await
    .expect("the batch ran serially and deadlocked at the barrier");
    assert_eq!(d.peak(), N, "peak concurrency");
    assert_eq!(outcomes.len(), N);
    for o in &outcomes {
        assert_eq!(o.text, "out:read_file");
        assert!(!o.is_error);
    }
}

#[tokio::test]
async fn a_parallel_batch_answers_in_call_order() {
    // Results answer their calls in CALL order however the calls finish.
    let d = ParallelDispatch::by_name(&["read_file", "grep"]);
    let calls = [
        tool_call("c1", "grep"),
        tool_call("c2", "read_file"),
        tool_call("c3", "grep"),
    ];
    let outcomes = run_batch(&RunCtx::default(), &d, &calls).await;
    assert_eq!(outcomes.len(), calls.len());
    let msgs: Vec<Message> = calls
        .iter()
        .zip(&outcomes)
        .map(|(tc, o)| Message::tool_result(tc, o.text.clone(), o.is_error))
        .collect();
    for (i, m) in msgs.iter().enumerate() {
        assert_eq!(m.role(), Role::Tool);
        assert_eq!(m.tool_call_id(), calls[i].id, "result {i}");
        assert_eq!(m.tool_call_name(), calls[i].name, "result {i}");
        assert_eq!(m.content, format!("out:{}", calls[i].name));
        assert!(!m.is_error());
    }
}

#[tokio::test]
async fn a_cancelled_batch_still_answers_every_call() {
    // A cancelled batch still returns a result for every call it made: a call without a result would leave the
    // round's history unable to answer itself.
    let d = ParallelDispatch::by_name(&["read_file"]);
    let calls = [tool_call("a", "read_file"), tool_call("b", "read_file")];
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cx = RunCtx::new(cancel);
    let outcomes = run_batch(&cx, &d, &calls).await;
    assert_eq!(
        outcomes.len(),
        calls.len(),
        "cancelled batch returned {} results for {} calls",
        outcomes.len(),
        calls.len()
    );
    // The tools observed the cancellation and answered with their own error.
    for o in &outcomes {
        assert!(o.is_error);
        assert_eq!(o.text, "Error calling tool: interrupted");
    }
}

#[tokio::test]
async fn the_quiet_loop_batches_parallel_calls() {
    // The quiet (-m) loop batches too: three parallel-capable calls run concurrently (peak >= 3) and their
    // results land in call order 1,2,3. The barrier releases only once every call has started: a serial loop
    // would never start the second and deadlock into the timeout.
    let d = Arc::new(ParallelDispatch::by_name(&["read_file"]).with_barrier(3));
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            tool_calls: vec![
                tool_call("1", "read_file"),
                tool_call("2", "read_file"),
                tool_call("3", "read_file"),
            ],
            ..RoundResult::default()
        }],
        "done",
    );
    let mut history = vec![Message::user("go")];
    let mut host = QuietHost::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        execute_with_tools(
            &RunCtx::default(),
            &tp,
            d.clone(),
            &mut history,
            Vec::new(),
            "",
            None,
            &mut host,
        ),
    )
    .await
    .expect("the quiet loop ran the calls serially")
    .expect("quiet loop failed");
    assert_eq!(outcome.content, "done");
    assert!(
        d.peak() >= 3,
        "peak concurrency = {}, want 3 — the calls did not overlap",
        d.peak()
    );
    // Results still answer their calls in call order, batch or not.
    let ids: Vec<&str> = history
        .iter()
        .filter(|m| m.role() == Role::Tool)
        .map(Message::tool_call_id)
        .collect();
    assert_eq!(ids, ["1", "2", "3"]);
    assert_eq!(host.rec.rounds()[0].tools, ["read_file"; 3]);
}

// New: a run of length 1 falls through to the serial path (and its approval gate), never to a batch.
#[tokio::test]
async fn single_parallel_call_runs_serially() {
    let d = Arc::new(ParallelDispatch::by_name(&["read_file"]));
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            tool_calls: vec![
                tool_call("1", "read_file"),
                tool_call("2", "edit_file"),
                tool_call("3", "read_file"),
            ],
            ..RoundResult::default()
        }],
        "done",
    );
    let mut history = vec![Message::user("go")];
    execute_with_tools(
        &RunCtx::default(),
        &tp,
        d.clone(),
        &mut history,
        Vec::new(),
        "",
        None,
        &mut QuietHost::new(),
    )
    .await
    .expect("loop failed");
    assert_eq!(d.peak(), 1);
    let ids: Vec<&str> = history
        .iter()
        .filter(|m| m.role() == Role::Tool)
        .map(Message::tool_call_id)
        .collect();
    assert_eq!(ids, ["1", "2", "3"]);
}
