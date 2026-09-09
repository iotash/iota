//! The `delegate` tool (`tool/delegate_test.go`) against `iota::testing::FakeDelegator`; the Go artifact-note
//! assertions are re-targeted at the run's `DelegationLedger` (DIVERGENCES D-19).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{collections::BTreeMap, sync::Arc};

use iota::chat::turns::{DelegationLedger, RunCtx};
use iota::provider::Effort;
use iota::provider::model::JsonObject;
use iota::provider::usage::Usage;
use iota::testing::{DelegateOutcomeSpec, FakeDelegator};
use iota::tool::delegate::{DELEGATE_DESC_PREAMBLE, agent_arg, new_delegate_set};
use iota::tool::sets::SetError;
use iota::tool::{AgentInfo, DelegateSpec, Delegator, Env, Tool, ToolOutput};
use pretty_assertions::assert_eq;
use serde_json::json;

fn agents(table: &[(&str, &str, bool)]) -> BTreeMap<String, AgentInfo> {
    table
        .iter()
        .map(|(name, desc, read_only)| {
            (
                (*name).to_owned(),
                AgentInfo {
                    description: (*desc).to_owned(),
                    read_only: *read_only,
                },
            )
        })
        .collect()
}

/// The delegate tool over `d`, built through the set factory.
fn delegate_tool(d: &Arc<FakeDelegator>) -> Arc<dyn Tool> {
    let env = Env {
        delegate: Some(Arc::clone(d) as Arc<dyn Delegator>),
        ..Env::default()
    };
    let mut tools = new_delegate_set(&env, None).expect("delegate set");
    assert_eq!(tools.len(), 1);
    tools.pop().unwrap()
}

fn args(v: &serde_json::Value) -> JsonObject {
    v.as_object().cloned().expect("object literal")
}

/// A run context carrying a fresh ledger.
fn ctx_with_ledger() -> (RunCtx, Arc<DelegationLedger>) {
    let ledger = Arc::new(DelegationLedger::default());
    let cx = RunCtx {
        ledger: Some(Arc::clone(&ledger)),
        ..RunCtx::default()
    };
    (cx, ledger)
}

fn ran(d: &FakeDelegator) -> Vec<DelegateSpec> {
    d.ran.lock().unwrap().clone()
}

// Go: tool/delegate_test.go:39
#[tokio::test]
async fn test_delegate_resolves_the_agent_once_for_both_paths() {
    let d = Arc::new(
        FakeDelegator::new(agents(&[("reader", "", true), ("writer", "", false)])).with_outcome(
            DelegateOutcomeSpec {
                reply: "done".to_owned(),
                rounds: 1,
                ..DelegateOutcomeSpec::default()
            },
        ),
    );
    let tl = delegate_tool(&d);

    for spelling in ["writer", " writer", "writer ", "  writer  "] {
        let a = args(&json!({"agent": spelling, "task": "t"}));
        assert!(
            !tl.supports_parallel(Some(&a)),
            "agent {spelling:?} classified as parallel-safe"
        );
        d.ran.lock().unwrap().clear();
        let out = tl.call(&RunCtx::default(), &a).await.unwrap();
        assert!(
            !out.is_error,
            "agent {spelling:?} was rejected by Call but accepted by the classifier: {out:?}"
        );
        assert_eq!(out.text, "done");
        let specs = ran(&d);
        assert!(
            specs.len() == 1 && specs[0].agent == "writer",
            "agent {spelling:?} ran {specs:?}, want the write-capable agent"
        );
        assert_eq!(agent_arg(&a), "writer");
    }
    // And the read-only spelling stays parallel-safe with the same padding.
    assert!(
        tl.supports_parallel(Some(&args(&json!({"agent": " reader "})))),
        "a padded read-only agent must still be parallel-safe"
    );
    assert!(!tl.supports_parallel(None));
    assert!(!tl.supports_parallel(Some(&args(&json!({"agent": "nonesuch"})))));
    assert!(!tl.supports_parallel(Some(&args(&json!({"agent": 7})))));
}

// Go: tool/delegate_test.go:68 (the "note" artifact is the ledger here)
#[tokio::test]
async fn test_delegate_reports_cost_of_a_failed_child() {
    let usage = Usage {
        input: 900,
        output: 100,
        total: 1000,
        ..Usage::default()
    };
    let d = Arc::new(
        FakeDelegator::new(agents(&[("a", "", false)])).with_outcome(DelegateOutcomeSpec {
            reply: String::new(),
            rounds: 3,
            usage,
            error: Some("upstream exploded".to_owned()),
        }),
    );
    let tl = delegate_tool(&d);

    let (cx, ledger) = ctx_with_ledger();
    let out = tl
        .call(&cx, &args(&json!({"agent": "a", "task": "t"})))
        .await
        .expect("a child's failure must be the parent's result, not its error");
    assert!(
        out.is_error && out.text.contains("upstream exploded"),
        "Call = {out:?}, want the failure as an error result"
    );
    assert_eq!(out.text, "delegation to \"a\" failed: upstream exploded");
    let (rounds, booked) = ledger
        .snapshot()
        .expect("a failed delegation booked no accounting");
    assert_eq!(rounds, 3, "accounting must bill the rounds that were run");
    assert_eq!(booked, usage);
}

// Go: tool/delegate_test.go:95
#[tokio::test]
async fn test_delegate_skips_accounting_when_nothing_ran() {
    let d = Arc::new(
        FakeDelegator::new(agents(&[("a", "", false)])).with_outcome(DelegateOutcomeSpec {
            error: Some("no api key".to_owned()),
            ..DelegateOutcomeSpec::default()
        }),
    );
    let tl = delegate_tool(&d);
    let (cx, ledger) = ctx_with_ledger();
    let out = tl
        .call(&cx, &args(&json!({"agent": "a", "task": "t"})))
        .await
        .unwrap();
    assert!(out.is_error, "a build failure must be an error result");
    assert_eq!(out.text, "delegation to \"a\" failed: no api key");
    assert!(
        ledger.snapshot().is_none(),
        "booked accounting for a child that never ran: {:?}",
        ledger.snapshot()
    );
}

// New: the definition sent to the model (tool/delegate.go:98-145), byte-exact.
#[test]
fn delegate_def_schema_and_description() {
    let d = Arc::new(FakeDelegator::new(agents(&[
        ("search", "Finds things fast", true),
        ("review", "", false),
    ])));
    let def = delegate_tool(&d).def();
    assert_eq!(def.name, "delegate");
    assert!(!def.deferred);
    assert_eq!(
        def.description,
        format!(
            "{DELEGATE_DESC_PREAMBLE}- review (can modify files): (no description configured)\n- search (read-only): Finds things fast"
        )
    );
    assert!(def.description.starts_with(
        "Delegate a task to a child agent and get back its answer.\n\nThe child starts with NO knowledge of this conversation and reports back only its final answer, so `task` must be self-contained — state the goal, the context it needs, and what shape the answer should take. Prefer delegating work whose intermediate steps you do not need to see (surveying a codebase, checking a hypothesis across many files); doing it yourself is better when you need to watch it happen.\n\nAvailable agents:\n"
    ));
    assert_eq!(
        def.input_schema,
        json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "enum": ["review", "search"],
                    "description": "Which configured agent runs the task."
                },
                "task": {
                    "type": "string",
                    "description": "The complete brief. The child sees this and nothing else — no history, no files you have already read."
                },
                "effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh", "max"],
                    "description": "Optional reasoning effort override for this one task."
                }
            },
            "required": ["agent", "task"]
        })
        .as_object()
        .cloned()
    );
    // Neither capability the headless loop does not need is claimed.
    assert!(!delegate_tool(&d).requires_approval());
    assert!(
        delegate_tool(&d)
            .header_summary(&JsonObject::new())
            .is_none()
    );
}

// New: every argument error is a tool error with Go's text, never a hard error (tool/delegate.go:147-163).
#[tokio::test]
async fn delegate_call_argument_errors() {
    let d = Arc::new(
        FakeDelegator::new(agents(&[("a", "", true), ("b", "", false)])).with_outcome(
            DelegateOutcomeSpec {
                reply: "  ".to_owned(),
                rounds: 2,
                ..DelegateOutcomeSpec::default()
            },
        ),
    );
    let tl = delegate_tool(&d);
    let cx = RunCtx::default();
    for (a, want) in [
        (json!({}), "missing required argument: agent"),
        (json!({"agent": "  "}), "missing required argument: agent"),
        (json!({"agent": 3}), "missing required argument: agent"),
        (
            json!({"agent": "zed", "task": "t"}),
            "unknown agent \"zed\" — configured agents: a, b",
        ),
        (json!({"agent": "a"}), "missing required argument: task"),
        (
            json!({"agent": "a", "task": " \n"}),
            "missing required argument: task",
        ),
        (
            json!({"agent": "a", "task": "t", "effort": "turbo"}),
            "invalid effort \"turbo\": want low|medium|high|xhigh|max",
        ),
        (
            json!({"agent": "a", "task": "t"}),
            "agent \"a\" finished without an answer after 2 round(s)",
        ),
    ] {
        let out = tl.call(&cx, &args(&a)).await.unwrap();
        assert_eq!(out, ToolOutput::err(want), "{a}");
    }
    // Only the last case reached the delegator.
    assert_eq!(ran(&d).len(), 1);

    // A successful run: the reply is returned untrimmed; the trimmed task and parsed effort reach the spec.
    *d.outcome.lock().unwrap() = Some(DelegateOutcomeSpec {
        reply: " answer \n".to_owned(),
        rounds: 1,
        ..DelegateOutcomeSpec::default()
    });
    let out = tl
        .call(
            &cx,
            &args(&json!({"agent": " b ", "task": "  do it  ", "effort": " high "})),
        )
        .await
        .unwrap();
    assert_eq!(out, ToolOutput::ok(" answer \n"));
    assert_eq!(
        ran(&d).last().unwrap(),
        &DelegateSpec {
            agent: "b".to_owned(),
            task: "do it".to_owned(),
            effort: Some(Effort::High),
        }
    );
    let out = tl
        .call(
            &cx,
            &args(&json!({"agent": "a", "task": "t", "effort": ""})),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(ran(&d).last().unwrap().effort, None);
}

// New: the factory contract (tool/delegate.go:41-52).
#[test]
fn delegate_set_factory_contract() {
    // Without the host seam the set contributes no tools and never errors.
    assert!(new_delegate_set(&Env::default(), None).unwrap().is_empty());
    let node = serde_norway::from_str("agents: {a: p}").unwrap();
    assert!(
        new_delegate_set(&Env::default(), Some(&node))
            .unwrap()
            .is_empty()
    );
    // A delegator with no agents is a configuration error.
    let env = Env {
        delegate: Some(Arc::new(FakeDelegator::new(BTreeMap::new()))),
        ..Env::default()
    };
    assert_eq!(new_delegate_set(&env, None).err(), Some(SetError::NoAgents));
    assert_eq!(
        SetError::NoAgents.to_string(),
        "no agents configured (add `agents:` mapping agent names to provider names)"
    );
    // The node is ignored: the host decodes the agents table.
    let d = Arc::new(FakeDelegator::new(agents(&[("a", "", true)])));
    let env = Env {
        delegate: Some(Arc::clone(&d) as Arc<dyn Delegator>),
        ..Env::default()
    };
    let node = serde_norway::from_str("[not, a, mapping]").unwrap();
    assert_eq!(new_delegate_set(&env, Some(&node)).unwrap().len(), 1);
}
