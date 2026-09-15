//! The run-wide turn budget (`chat/turns_test.go`, the loop-level half — the pure `TurnBudget` tests live
//! beside the type).

use std::sync::Arc;

use iota::chat::turns::{RunCtx, TurnBudget};
use iota::chat::{ChatError, QuietHost, execute_with_tools};
use iota::provider::model::Message;
use iota::testing::{FakeProvider, StaticDispatcher};
use iota::tool::Dispatcher;

/// One runaway loop drawing on `cx`'s pool; returns the number of model calls it got to make.
async fn spend(cx: &RunCtx) -> usize {
    let tp = FakeProvider::looping(1, 0);
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
        "tool loop reached the --max-turns limit without a final response (5 turns, the whole run's budget)"
    );
    tp.calls()
}

#[tokio::test]
async fn the_turn_budget_is_shared_across_loops() {
    // The budget belongs to the RUN. Two loops sharing one pool stop at the total between them, not at the
    // total each.
    let cx = RunCtx {
        budget: iota::chat::turns::turn_cap(5).map(TurnBudget::new),
        ..RunCtx::default()
    };
    let first = spend(&cx).await;
    let second = spend(&cx.clone()).await;
    assert_eq!(first, 5, "first loop must run the whole budget");
    assert_eq!(
        second, 0,
        "second loop must run 0 rounds after the pool was spent"
    );
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
    let tp = FakeProvider::reporting(5, Some(1));
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
    let tp = FakeProvider::looping(1, 0);
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
    assert_eq!(tp.calls(), 2);
}
