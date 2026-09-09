//! The delegate tool's runner (chat/delegate.go:58-162): `ChatDelegator` implements `crate::tool::Delegator` and
//! runs each child through `run_once` with a FRESH provider and dispatcher built by the injected `ChildFactory`.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use crate::chat::turns::RunCtx;
use crate::provider::Provider;
use crate::provider::model::{JsonObject, ToolCall, ToolDef};
use crate::tool::fmt::tool_call_detail;
use crate::tool::{
    AgentInfo, DeferredToolStatus, DelegateApprover, DelegateOutcome, DelegateResult, DelegateSpec,
    Delegator, Dispatcher, Owner, Presentation, ToolOutput, ToolResult, ToolSearcher,
};
use crate::{BoxError, BoxFuture};

use crate::chat::AgentOptions;
use crate::chat::error::ChatError;
use crate::chat::run::{QuietHost, RunRequest, install_tool_searcher, run_once};

/// Everything one delegated child run needs, freshly built per delegation.
pub struct Child {
    /// The child's own provider instance (effort override lands here).
    pub provider: Box<dyn Provider>,
    /// The child's own dispatcher (its own `Registry`/`CodeSet` read-before-edit ledger).
    pub dispatch: Arc<dyn Dispatcher>,
    /// The child's system prompt (`""` = none).
    pub system: String,
    /// The child's agent-mode settings.
    pub agent: AgentOptions,
    /// The child's local `--max-turns` cap (`None` = none; the shared `TurnBudget` still applies).
    pub max_turns: Option<std::num::NonZeroU32>,
}

/// Called on EVERY delegation. It must build a FRESH provider AND a FRESH dispatcher (cmd/delegate.go:137-186 calls
/// `buildChildTools` again inside the closure): each child gets its own `Registry`/`CodeSet` read-before-edit
/// ledger, so a later or concurrent child can never edit a file only an earlier child read. Reusing the dispatcher
/// built for startup validation is a contract violation (`child_dispatcher_is_fresh_per_delegation`).
pub type ChildFactory = Arc<dyn Fn(&str) -> Result<Child, BoxError> + Send + Sync>;

/// Runs delegated child agents through the headless loop.
pub struct ChatDelegator {
    agents: BTreeMap<String, AgentInfo>,
    names: Vec<String>,
    build: ChildFactory,
    /// The interactive parent's approval gate, when one exists (chat/delegate.go:76-77).
    approve: Mutex<Option<DelegateApprover>>,
    /// Serializes children waiting on the user: the terminal is single-threaded even
    /// when the delegations are not (delegate.go:126-131). In practice concurrent
    /// children never reach the gate at all — an agent may only run in parallel when it
    /// grants no state-changing tool — so the lock is what keeps that from being
    /// load-bearing.
    asking: Arc<tokio::sync::Mutex<()>>,
}

impl ChatDelegator {
    /// A delegator over `agents`; the name list is sorted once (a `BTreeMap` iterates in key order — Go's
    /// `sort.Strings`, delegate.go:85-92). No approver until [`ChatDelegator::set_approver`].
    pub fn new(agents: BTreeMap<String, AgentInfo>, build: ChildFactory) -> Self {
        let names = agents.keys().cloned().collect();
        Self {
            agents,
            names,
            build,
            approve: Mutex::new(None),
            asking: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Installs (or clears) the parent's approval gate (`TUI_CONTRACTS` §4;
    /// chat/delegate.go:100-108). A child has no user of its own but runs inside a parent
    /// that does: with an approver set, every state-changing call of every child is put to
    /// that ONE gate, labelled with the asking agent — which is also why the "allow for
    /// this session" memory is shared (the grant is "this session may edit files", and the
    /// child is part of the session). Cleared (`None`) the children fall back to the
    /// headless refusal.
    pub fn set_approver(&self, f: Option<DelegateApprover>) {
        *self.approve.lock().unwrap_or_else(PoisonError::into_inner) = f;
    }

    fn approver(&self) -> Option<DelegateApprover> {
        self.approve
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The child's dispatcher with the parent's approval gate spliced into `call_tool`.
///
/// Go put the approver on `quietHost` (delegate.go:34-51) because its gate call was
/// synchronous; the interactive gate is an `async` facade round-trip, and
/// `QuietHost::ask_approval` is a sync seam shared with the headless loop. Wrapping the
/// dispatcher keeps the whole change inside this file and produces the SAME history:
/// the loop sees `requires_approval == false` (the gate already ran) and a refusal comes
/// back as the call's `is_error` result text, exactly what Go appended
/// (DEVIATIONS3 `[WP49]`).
struct ApprovingDispatch {
    inner: Arc<dyn Dispatcher>,
    approve: DelegateApprover,
    agent: String,
    /// The delegator's one-child-at-a-time lock.
    asking: Arc<tokio::sync::Mutex<()>>,
}

impl Dispatcher for ApprovingDispatch {
    fn tools(&self) -> Vec<ToolDef> {
        self.inner.tools()
    }

    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if self.inner.requires_approval(name) {
                let tc = ToolCall {
                    id: String::new(),
                    name: name.to_owned(),
                    arguments: args.clone(),
                };
                let detail = tool_call_detail(&*self.inner, &tc);
                let (allowed, why) = {
                    let _turn = self.asking.lock().await;
                    (self.approve)(name, &detail, &self.agent).await
                };
                if !allowed {
                    return Ok(ToolOutput::err(why));
                }
            }
            self.inner.call_tool(cx, name, args).await
        })
    }

    /// Always false: the gate above already ran, so the child's loop must not ask again
    /// (its only other asker is the headless refusal).
    fn requires_approval(&self, _name: &str) -> bool {
        false
    }

    fn presentation(&self, name: &str) -> Presentation {
        self.inner.presentation(name)
    }

    fn supports_parallel(&self, name: &str, args: Option<&JsonObject>) -> bool {
        self.inner.supports_parallel(name, args)
    }

    fn header_summary(&self, name: &str, args: &JsonObject) -> Option<String> {
        self.inner.header_summary(name, args)
    }

    fn as_owner(&self) -> Option<&dyn Owner> {
        self.inner.as_owner()
    }

    fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher> {
        self.inner.as_tool_searcher()
    }

    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        self.inner.deferred_tools()
    }

    fn take_pending_loads(&self) -> Vec<ToolDef> {
        self.inner.take_pending_loads()
    }
}

/// A child that never reached its provider: zero accounting beside the error (Go `tool.DelegateResult{}`).
fn failed_before_start(err: ChatError) -> DelegateOutcome {
    DelegateOutcome {
        result: DelegateResult::default(),
        error: Some(BoxError::from(err)),
    }
}

impl Delegator for ChatDelegator {
    fn agent_names(&self) -> &[String] {
        &self.names
    }

    fn agent(&self, name: &str) -> Option<&AgentInfo> {
        self.agents.get(name)
    }

    /// unknown → error `UnknownAgent`, rounds 0; build error → `Child(e)`, rounds 0; effort →
    /// `as_tunable().set_effort(Some(e))` on the FRESH provider; `install_tool_searcher(child.provider,
    /// &child.dispatch)`; `host = QuietHost::new()`; `run_once(&cx.child(), &*child.provider, RunRequest { task,
    /// system, agent }, child.dispatch, child.max_turns, &mut host, None)`; result `{ reply, rounds:
    /// host.rec.round_count(), usage: host.rec.usage(), duration }`; `cx.ledger.add(rounds, usage)` ALWAYS;
    /// `DelegateOutcome { result, error }` (delegate.go:117-162).
    fn run<'a>(&'a self, cx: &'a RunCtx, spec: DelegateSpec) -> BoxFuture<'a, DelegateOutcome> {
        Box::pin(async move {
            if !self.agents.contains_key(&spec.agent) {
                return failed_before_start(ChatError::UnknownAgent(spec.agent));
            }
            let mut child = match (self.build)(&spec.agent) {
                Ok(child) => child,
                Err(e) => return failed_before_start(ChatError::Child(e)),
            };
            // The per-task effort override lands on a provider built for this call alone, so it cannot leak
            // into the parent's or another child's sampling.
            if let Some(effort) = spec.effort
                && let Some(tunable) = child.provider.as_tunable()
            {
                tunable.set_effort(Some(effort));
            }
            install_tool_searcher(&mut *child.provider, &child.dispatch);
            // With a parent gate installed, the child's state-changing calls are put to
            // the one user who exists — labelled with the agent that asked — instead of
            // taking the non-interactive refusal (delegate.go:100-131).
            if let Some(approve) = self.approver() {
                child.dispatch = Arc::new(ApprovingDispatch {
                    inner: child.dispatch,
                    approve,
                    agent: spec.agent.clone(),
                    asking: Arc::clone(&self.asking),
                });
            }

            // The child gets its OWN recorder but the SHARED budget (through `cx.child()`); `QuietHost`'s own
            // approver stays None, so without a parent gate above a gated call takes the non-interactive
            // refusal like any other headless run.
            let mut host = QuietHost::new();
            let req = RunRequest {
                message: spec.task,
                system: child.system,
                agent: child.agent,
                // Children never resume a session (the struct literal must name the field, E0063).
                history: Vec::new(),
            };
            let child_cx = cx.child();
            let started = Instant::now();
            // Children never save images (POLICY I-06): `images_dir` is None.
            let outcome = run_once(
                &child_cx,
                &*child.provider,
                &req,
                Arc::clone(&child.dispatch),
                child.max_turns,
                &mut host,
                None,
            )
            .await;
            let duration = started.elapsed();
            let rounds = host.rec.round_count();
            let usage = host.rec.usage();
            let (reply, error) = match outcome {
                Ok(o) => (o.reply, None),
                Err(e) => (String::new(), Some(BoxError::from(e))),
            };
            // The run's own report has to account for this: a failed child counts — its rounds were billed.
            if let Some(ledger) = &cx.ledger {
                ledger.add(rounds, usage);
            }
            DelegateOutcome {
                result: DelegateResult {
                    reply,
                    rounds,
                    usage,
                    duration,
                },
                error,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use crate::BoxFuture;
    use crate::chat::turns::RunCtx;
    use crate::provider::RoundResult;
    use crate::provider::model::{JsonObject, ToolCall, ToolDef};
    use crate::testing::FakeToolProvider;
    use crate::tool::ToolOutput;
    use crate::tool::{AgentInfo, DelegateSpec, Delegator, ToolResult};

    use super::{ChatDelegator, Child, ChildFactory, Dispatcher};
    use crate::chat::AgentOptions;

    /// One `write_file` tool that always needs approval and counts its executions.
    #[derive(Default)]
    struct GatedDispatch {
        ran: Mutex<u32>,
    }

    impl Dispatcher for GatedDispatch {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "write_file".to_owned(),
                ..ToolDef::default()
            }]
        }

        fn call_tool<'a>(
            &'a self,
            _cx: &'a RunCtx,
            _name: &'a str,
            _args: JsonObject,
        ) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                *self.ran.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                Ok(ToolOutput::ok("wrote"))
            })
        }

        fn requires_approval(&self, _name: &str) -> bool {
            true
        }
    }

    /// A delegator whose one agent asks for `write_file` and then answers.
    fn delegator(dispatch: Arc<GatedDispatch>) -> ChatDelegator {
        let build: ChildFactory = Arc::new(move |_agent| {
            let mut args = JsonObject::new();
            args.insert("path".to_owned(), serde_json::Value::from("a.txt"));
            Ok(Child {
                provider: Box::new(FakeToolProvider::scripted(
                    vec![RoundResult {
                        tool_calls: vec![ToolCall {
                            id: "c1".to_owned(),
                            name: "write_file".to_owned(),
                            arguments: args.clone(),
                        }],
                        ..RoundResult::default()
                    }],
                    "done",
                )),
                dispatch: Arc::clone(&dispatch) as Arc<dyn Dispatcher>,
                system: String::new(),
                agent: AgentOptions::default(),
                max_turns: None,
            })
        });
        ChatDelegator::new(
            BTreeMap::from([("writer".to_owned(), AgentInfo::default())]),
            build,
        )
    }

    fn spec() -> DelegateSpec {
        DelegateSpec {
            agent: "writer".to_owned(),
            task: "edit it".to_owned(),
            effort: None,
        }
    }

    // Go: chat/delegate.go:100-131 — with the parent's gate installed, a child's
    // state-changing call is put to the one user who exists, labelled with the AGENT that
    // asked, and a refusal comes back as the call's result instead of the headless text.
    #[tokio::test]
    async fn set_approver_routes_child_calls_to_the_parent_gate() {
        let dispatch = Arc::new(GatedDispatch::default());
        let d = delegator(Arc::clone(&dispatch));
        let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();

        // Denied: the child never executes the call and the refusal is its result.
        let asked = Arc::clone(&seen);
        d.set_approver(Some(Arc::new(move |name, detail, agent| {
            asked.lock().unwrap_or_else(PoisonError::into_inner).push((
                name.to_owned(),
                detail.to_owned(),
                agent.to_owned(),
            ));
            Box::pin(std::future::ready((
                false,
                "The user declined this call.".to_owned(),
            )))
        })));
        let out = d.run(&RunCtx::default(), spec()).await;
        assert!(out.error.is_none(), "a refusal is not a child failure");
        assert_eq!(
            *dispatch.ran.lock().unwrap_or_else(PoisonError::into_inner),
            0,
            "a declined call must not run"
        );
        let asked = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(
            asked,
            vec![(
                "write_file".to_owned(),
                "path:a.txt".to_owned(),
                "writer".to_owned()
            )],
            "the gate is told what the call is about and who asked"
        );

        // Approved: the call runs.
        let dispatch = Arc::new(GatedDispatch::default());
        let d = delegator(Arc::clone(&dispatch));
        d.set_approver(Some(Arc::new(|_, _, _| {
            Box::pin(std::future::ready((true, String::new())))
        })));
        let out = d.run(&RunCtx::default(), spec()).await;
        assert!(out.error.is_none());
        assert_eq!(out.result.reply, "done");
        assert_eq!(
            *dispatch.ran.lock().unwrap_or_else(PoisonError::into_inner),
            1
        );
    }

    // Without an approver the child keeps the non-interactive refusal (delegate.go:44-51) —
    // headless behavior is unchanged by the seam.
    #[tokio::test]
    async fn no_approver_keeps_the_headless_refusal() {
        let dispatch = Arc::new(GatedDispatch::default());
        let d = delegator(Arc::clone(&dispatch));
        let out = d.run(&RunCtx::default(), spec()).await;
        assert!(out.error.is_none());
        assert_eq!(out.result.reply, "done");
        assert_eq!(
            *dispatch.ran.lock().unwrap_or_else(PoisonError::into_inner),
            0,
            "nobody to ask: the call is refused, not executed"
        );
        // Clearing an installed approver goes back to the same shape.
        d.set_approver(Some(Arc::new(|_, _, _| {
            Box::pin(std::future::ready((true, String::new())))
        })));
        d.set_approver(None);
        let out = d.run(&RunCtx::default(), spec()).await;
        assert!(out.error.is_none());
        assert_eq!(
            *dispatch.ran.lock().unwrap_or_else(PoisonError::into_inner),
            0
        );
    }
}
