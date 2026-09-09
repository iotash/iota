//! The run-wide turn budget and the delegated section of the report (`chat/turns_test.go`, the loop-level half —
//! the pure `TurnBudget`/`DelegationLedger` tests live in `iota-core`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, atomic::Ordering};

use iota::chat::report::delegated_report;
use iota::chat::turns::{DelegationLedger, RunCtx, TurnBudget};
use iota::chat::{ChatError, QuietHost, RunRecorder, execute_with_tools};
use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::provider::usage::Usage;
use iota::testing::{FakeToolProvider, StaticDispatcher};
use iota::tool::Dispatcher;

/// One runaway loop drawing on `cx`'s pool; returns the number of model calls it got to make.
async fn spend(cx: &RunCtx) -> usize {
    let tp = FakeToolProvider::looping(1, 0);
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut history = vec![Message::user("go")];
    let err = execute_with_tools(
        cx,
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut QuietHost::new(),
    )
    .await
    .expect_err("the budget must stop the loop");
    assert!(
        matches!(err, ChatError::SharedCap { turns: 5 }),
        "loop ended with {err}, want the budget to stop it"
    );
    assert_eq!(
        err.to_string(),
        "tool loop reached the --max-turns limit without a final response (5 turns, shared by this run and everything it delegated)"
    );
    tp.calls.load(Ordering::SeqCst)
}

// Go: chat/turns_test.go:72
#[tokio::test]
async fn test_turn_budget_is_shared_across_loops() {
    // The budget belongs to the RUN. Two loops sharing one pool stop at the total between them, not at the
    // total each.
    let cx = RunCtx {
        budget: iota::chat::turns::turn_cap(5).map(TurnBudget::new),
        ..RunCtx::default()
    };
    let first = spend(&cx).await;
    let second = spend(&cx.child()).await;
    assert_eq!(first, 5, "first loop must run the whole budget");
    assert_eq!(
        second, 0,
        "second loop must run 0 rounds after the pool was spent"
    );
}

// Go: chat/turns_test.go:161
#[test]
fn test_report_keeps_delegated_separate() {
    // The delegated total stays out of the parent's own figures: one says what this agent spent, the other what
    // it spent by delegating.
    let mut rec = RunRecorder::start();
    rec.observe(
        Some(Usage {
            input: 9,
            output: 1,
            total: 10,
            ..Usage::default()
        }),
        Vec::new(),
    );
    let ledger = DelegationLedger::default();
    ledger.add(
        4,
        Usage {
            input: 3600,
            output: 400,
            total: 4000,
            ..Usage::default()
        },
    );
    let rep = rec.report(
        ProviderKind::OpenAi,
        "gpt-test",
        "done",
        Vec::new(),
        Vec::new(),
        delegated_report(&ledger),
        None,
    );
    assert_eq!(
        rep.usage.total_tokens, 10,
        "own usage = the parent's own calls only"
    );
    assert_eq!(rep.rounds, 1, "own rounds = the parent's own rounds only");
    let delegated = rep.delegated.expect("want the children's total");
    assert_eq!(delegated.rounds, 4);
    assert_eq!(delegated.usage.total_tokens, 4000);
    assert_eq!(delegated.usage.input_tokens, 3600);

    // And it is omitted entirely when nothing was delegated.
    let bare = RunRecorder::start();
    let rep = bare.report(
        ProviderKind::OpenAi,
        "gpt-test",
        "x",
        Vec::new(),
        Vec::new(),
        delegated_report(&DelegationLedger::default()),
        None,
    );
    assert!(
        rep.delegated.is_none(),
        "a run that delegated nothing carries a delegated section"
    );
    assert_eq!(rep.rounds, 0);
}

// New: the budget is spent BEFORE the model call, so a round that errors still cost a turn, and the local cap is
// checked before the pool.
#[tokio::test]
async fn budget_is_taken_before_the_call_and_local_cap_wins() {
    let cx = RunCtx {
        budget: iota::chat::turns::turn_cap(3).map(TurnBudget::new),
        ..RunCtx::default()
    };
    // Fails on call 1: the pool still lost that turn.
    let tp = FakeToolProvider::reporting(5, Some(1));
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut history = vec![Message::user("go")];
    let err = execute_with_tools(
        &cx,
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut QuietHost::new(),
    )
    .await
    .expect_err("call 1 fails");
    assert!(matches!(err, ChatError::Provider(_)), "{err}");
    assert_eq!(err.to_string(), "boom");
    assert!(cx.budget.as_ref().unwrap().take());
    assert!(cx.budget.as_ref().unwrap().take());
    assert!(!cx.budget.as_ref().unwrap().take(), "three turns are gone");

    // A local cap of 2 with a pool of 10: the local cap fires first with its own text.
    let cx = RunCtx {
        budget: iota::chat::turns::turn_cap(10).map(TurnBudget::new),
        ..RunCtx::default()
    };
    let tp = FakeToolProvider::looping(1, 0);
    let err = execute_with_tools(
        &cx,
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        std::num::NonZeroU32::new(2),
        &mut QuietHost::new(),
    )
    .await
    .expect_err("local cap");
    assert!(
        matches!(err, ChatError::LocalCap { turns } if turns.get() == 2),
        "{err}"
    );
    assert_eq!(tp.calls.load(Ordering::SeqCst), 2);
}
