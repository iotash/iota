//! `ChatDelegator` (`chat/delegate.go:117-162`): the ledger is booked even for a failed child, unknown agents and
//! factory failures are reported with rounds 0, the effort override lands on the fresh provider, and every
//! delegation builds a fresh child.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use iota::chat::turns::{DelegationLedger, RunCtx, TurnBudget};
use iota::chat::{AgentOptions, ChatDelegator, Child, ChildFactory};
use iota::provider::Effort;
use iota::testing::{FakeToolProvider, StaticDispatcher};
use iota::tool::{AgentInfo, DelegateSpec, Delegator};

use crate::common::{EffortProvider, lock};

/// The cell an `EffortProvider` records its effort into.
type EffortCell = Arc<Mutex<Option<Effort>>>;

fn agents(names: &[&str]) -> BTreeMap<String, AgentInfo> {
    names
        .iter()
        .map(|n| ((*n).to_owned(), AgentInfo::default()))
        .collect()
}

fn spec(agent: &str) -> DelegateSpec {
    DelegateSpec {
        agent: agent.to_owned(),
        task: "do it".to_owned(),
        effort: None,
    }
}

fn ledger_cx() -> (RunCtx, Arc<DelegationLedger>) {
    let ledger = Arc::new(DelegationLedger::default());
    let cx = RunCtx {
        ledger: Some(Arc::clone(&ledger)),
        ..RunCtx::default()
    };
    (cx, ledger)
}

// New (delegate.go:157-161): a FAILED child is still added to the ledger — its rounds were billed.
#[tokio::test]
async fn delegator_run_adds_to_ledger_even_on_error() {
    let build: ChildFactory = Arc::new(|_agent| {
        Ok(Child {
            provider: Box::new(FakeToolProvider::reporting(5, Some(3))),
            dispatch: Arc::new(StaticDispatcher::new(&["noop"])),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
        })
    });
    let d = ChatDelegator::new(agents(&["worker"]), build);
    let (cx, ledger) = ledger_cx();
    let out = d.run(&cx, spec("worker")).await;
    let err = out.error.expect("call 3 fails");
    assert_eq!(err.to_string(), "boom");
    assert_eq!(out.result.reply, "");
    assert_eq!(out.result.rounds, 2);
    assert_eq!(out.result.usage.input, 300);
    assert_eq!(out.result.usage.total, 330);
    let (rounds, usage) = ledger.snapshot().expect("a failed child still counts");
    assert_eq!(rounds, 2);
    assert_eq!(usage.input, 300);

    // A successful child books its rounds and hands back only the reply.
    let build: ChildFactory = Arc::new(|_agent| {
        Ok(Child {
            provider: Box::new(FakeToolProvider::reporting(2, None)),
            dispatch: Arc::new(StaticDispatcher::new(&["noop"])),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: None,
        })
    });
    let d = ChatDelegator::new(agents(&["worker"]), build);
    let (cx, ledger) = ledger_cx();
    let out = d.run(&cx, spec("worker")).await;
    assert!(
        out.error.is_none(),
        "{:?}",
        out.error.map(|e| e.to_string())
    );
    assert_eq!(out.result.reply, "final answer");
    assert_eq!(out.result.rounds, 3);
    assert_eq!(out.result.usage.input, 600);
    assert_eq!(ledger.snapshot(), Some((3, out.result.usage)));
    assert!(out.result.duration <= std::time::Duration::from_secs(5));

    // Without a ledger in the context nothing is booked and the run still completes.
    let out = d.run(&RunCtx::default(), spec("worker")).await;
    assert!(out.error.is_none());
    assert_eq!(out.result.rounds, 3);
}

// New (delegate.go:118-124): unknown agents and factory failures carry the exact text and rounds 0.
#[tokio::test]
async fn delegator_unknown_agent_text() {
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&factory_calls);
    let build: ChildFactory = Arc::new(move |agent| {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(format!("agent {agent:?}: API key is required").into())
    });
    let d = ChatDelegator::new(agents(&["zeta", "alpha", "mid"]), build);
    // Names sorted once (schema enum order).
    assert_eq!(d.agent_names(), ["alpha", "mid", "zeta"]);
    assert!(d.agent("mid").is_some());
    assert!(d.agent("nope").is_none());

    let (cx, ledger) = ledger_cx();
    let out = d.run(&cx, spec("nope")).await;
    assert_eq!(
        out.error.expect("unknown").to_string(),
        "unknown agent \"nope\""
    );
    assert_eq!(out.result.rounds, 0);
    assert_eq!(
        factory_calls.load(Ordering::SeqCst),
        0,
        "the factory is not consulted for an unknown agent"
    );
    assert!(ledger.snapshot().is_none(), "nothing ran, nothing booked");

    // A factory failure is passed through verbatim.
    let out = d.run(&cx, spec("alpha")).await;
    assert_eq!(
        out.error.expect("build failed").to_string(),
        "agent \"alpha\": API key is required"
    );
    assert_eq!(out.result.rounds, 0);
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert!(ledger.snapshot().is_none());
}

// New (delegate.go:125-131): the per-task effort lands on the FRESH provider built for this call alone, and every
// delegation builds a new child (the factory is called per run).
#[tokio::test]
async fn delegator_effort_override_hits_fresh_provider() {
    let efforts: Arc<Mutex<Vec<EffortCell>>> = Arc::new(Mutex::new(Vec::new()));
    let cells = Arc::clone(&efforts);
    let build: ChildFactory = Arc::new(move |_agent| {
        let cell: EffortCell = Arc::new(Mutex::new(None));
        lock(&cells).push(Arc::clone(&cell));
        Ok(Child {
            provider: Box::new(EffortProvider::new("child says hi", cell)),
            dispatch: Arc::new(StaticDispatcher::new(&[])),
            system: "you are a child".to_owned(),
            agent: AgentOptions::default(),
            max_turns: None,
        })
    });
    let d = ChatDelegator::new(agents(&["worker"]), build);
    let (cx, ledger) = ledger_cx();

    let out = d
        .run(
            &cx,
            DelegateSpec {
                effort: Some(Effort::High),
                ..spec("worker")
            },
        )
        .await;
    assert!(
        out.error.is_none(),
        "{:?}",
        out.error.map(|e| e.to_string())
    );
    assert_eq!(out.result.reply, "child says hi");
    // The unary path: exactly one round, its usage booked.
    assert_eq!(out.result.rounds, 1);
    assert_eq!(out.result.usage.input, 7);
    assert_eq!(ledger.snapshot(), Some((1, out.result.usage)));

    // No effort on the second delegation: a NEW provider, untouched.
    let out = d.run(&cx, spec("worker")).await;
    assert!(out.error.is_none());
    let cells = lock(&efforts);
    assert_eq!(cells.len(), 2, "one fresh child per delegation");
    assert_eq!(*lock(&cells[0]), Some(Effort::High));
    assert_eq!(*lock(&cells[1]), None);
    assert_eq!(ledger.snapshot().map(|(r, _)| r), Some(2));
}

// New (delegate.go:133, turns.go): the child draws on the PARENT's budget through `cx.child()`, and its own local
// cap is the Child's max_turns.
#[tokio::test]
async fn delegator_child_shares_the_budget_and_keeps_its_own_cap() {
    let build: ChildFactory = Arc::new(|_agent| {
        Ok(Child {
            provider: Box::new(FakeToolProvider::looping(1, 0)),
            dispatch: Arc::new(StaticDispatcher::new(&["noop"])),
            system: String::new(),
            agent: AgentOptions::default(),
            max_turns: std::num::NonZeroU32::new(2),
        })
    });
    let d = ChatDelegator::new(agents(&["worker"]), build);
    let ledger = Arc::new(DelegationLedger::default());
    let cx = RunCtx {
        budget: iota::chat::turns::turn_cap(5).map(TurnBudget::new),
        ledger: Some(Arc::clone(&ledger)),
        ..RunCtx::default()
    };
    // Local cap of 2 trips first.
    let out = d.run(&cx, spec("worker")).await;
    assert_eq!(
        out.error.expect("local cap").to_string(),
        "tool loop reached the --max-turns limit without a final response (2 turns)"
    );
    assert_eq!(out.result.rounds, 2);
    // Two of five shared turns are gone; the next child gets two more, then the pool is empty.
    let out = d.run(&cx, spec("worker")).await;
    assert_eq!(out.result.rounds, 2);
    let out = d.run(&cx, spec("worker")).await;
    assert_eq!(
        out.error.expect("shared cap").to_string(),
        "tool loop reached the --max-turns limit without a final response (5 turns, shared by this run and everything it delegated)"
    );
    assert_eq!(out.result.rounds, 1);
    assert_eq!(ledger.snapshot().map(|(r, _)| r), Some(5));
}
