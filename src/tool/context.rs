//! Run context (chat/turns.go): `RunCtx` replaces Go's context values with an explicit struct carrying the
//! cancellation token and the run-wide `TurnBudget`.

use std::num::NonZeroU32;
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU32, Ordering},
};

use tokio_util::sync::CancellationToken;

/// Replaces Go's context values; cloned into every tool call and child run. Never task-local.
#[derive(Clone, Default)]
pub struct RunCtx {
    /// Cancellation of the whole run.
    pub cancel: CancellationToken,
    /// The run's turn budget; `None` = unlimited.
    pub budget: Option<Arc<TurnBudget>>,
    /// The call's display-artifact slot (tool/tool.go:160-198; the D-19 lift, T-35).
    /// `None` in headless loops and tests — every `post_artifact` is then a no-op (Go
    /// parity). The interactive walk injects a FRESH slot per call and drains it after.
    pub artifact: Option<ArtifactSlot>,
}

impl RunCtx {
    /// A context with `cancel` and no budget.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            ..Self::default()
        }
    }
}

/// `--max-turns` as the CLI carries it (`i64`, negatives allowed) folded into a cap: `None` for
/// `n <= 0` (unlimited; Go's nil budget), and for `n > u32::MAX` too (documented as unlimited).
pub fn turn_cap(n: i64) -> Option<NonZeroU32> {
    u32::try_from(n).ok().and_then(NonZeroU32::new)
}

/// The run's `--max-turns` pool.
#[derive(Debug)]
pub struct TurnBudget {
    remaining: AtomicU32,
    total: NonZeroU32,
}

impl TurnBudget {
    /// A budget of `cap` rounds.
    pub fn new(cap: NonZeroU32) -> Arc<TurnBudget> {
        Arc::new(TurnBudget {
            remaining: AtomicU32::new(cap.get()),
            total: cap,
        })
    }

    /// CAS loop: false once nothing remains; exact under contention.
    pub fn take(&self) -> bool {
        let mut cur = self.remaining.load(Ordering::Acquire);
        loop {
            if cur == 0 {
                return false;
            }
            match self.remaining.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// The cap the run was given.
    pub fn cap(&self) -> u32 {
        self.total.get()
    }
}

/// Nil-budget rule (chat/turns.go:52-63): None grants everything, cap 0.
pub(crate) trait BudgetExt {
    /// Claims one round; always true without a budget.
    fn take(&self) -> bool;
    /// The cap; 0 without a budget.
    fn cap(&self) -> u32;
}

impl BudgetExt for Option<Arc<TurnBudget>> {
    fn take(&self) -> bool {
        self.as_ref().is_none_or(|b| TurnBudget::take(b))
    }

    fn cap(&self) -> u32 {
        self.as_ref().map_or(0, |b| TurnBudget::cap(b))
    }
}

/// Last-post-wins display-artifact slot (Go `artifactSlot` twin, tool/tool.go:160-198).
/// The interactive walk injects a FRESH slot into the `RunCtx` handed to ONE call and
/// drains it after the call returns.
#[derive(Clone, Default)]
pub struct ArtifactSlot(Arc<Mutex<Option<crate::tool::Artifact>>>);

impl ArtifactSlot {
    /// Stores `a`, replacing any earlier post (last post wins).
    pub fn post(&self, a: crate::tool::Artifact) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(a);
    }

    /// Drains the slot.
    pub fn take(&self) -> Option<crate::tool::Artifact> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio_util::sync::CancellationToken;

    use super::{BudgetExt, RunCtx, TurnBudget, turn_cap};

    // Go: chat/turns_test.go:12
    #[test]
    fn test_turn_budget_unlimited_without_a_flag() {
        // A cap nobody chose is not invented: no --max-turns means no budget.
        for n in [0, -1, i64::MIN] {
            assert!(turn_cap(n).is_none(), "turn_cap({n}) must be None");
        }
        // Beyond u32 is unlimited too (documented).
        assert!(turn_cap(i64::from(u32::MAX) + 1).is_none());
        let nil: Option<Arc<TurnBudget>> = None;
        for _ in 0..1000 {
            assert!(nil.take(), "a nil budget must grant every round");
        }
        assert_eq!(nil.cap(), 0);
    }

    // Go: chat/turns_test.go:27
    #[test]
    fn test_turn_budget_spends_exactly_its_cap() {
        let b = turn_cap(3)
            .map(TurnBudget::new)
            .expect("a positive cap yields a budget");
        for i in 1..=3 {
            assert!(b.take(), "round {i} refused inside the cap");
        }
        assert!(!b.take(), "the budget granted a fourth round");
        assert_eq!(b.cap(), 3, "cap() must be the number the user wrote");
        // Through the Option extension too.
        let opt = turn_cap(1).map(TurnBudget::new);
        assert_eq!(opt.cap(), 1);
        assert!(opt.take());
        assert!(!opt.take());
    }

    // Go: chat/turns_test.go:45
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_turn_budget_is_exact_under_contention() {
        const CAP: i64 = 50;
        let b = turn_cap(CAP).map(TurnBudget::new).expect("budget");
        let granted = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let b = Arc::clone(&b);
            let granted = Arc::clone(&granted);
            tasks.push(tokio::spawn(async move {
                for _ in 0..100 {
                    if b.take() {
                        granted.fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::task::yield_now().await;
                }
            }));
        }
        for t in tasks {
            t.await.expect("task");
        }
        assert_eq!(
            granted.load(Ordering::SeqCst),
            50,
            "granted rounds against a cap of {CAP}"
        );
    }

    // Go: chat/turns_test.go:96
    #[test]
    fn test_turn_budget_travels_by_context() {
        // A context with no budget yields None; an absent budget is not published as present.
        let cx = RunCtx {
            budget: iota::chat::turns::turn_cap(0).map(TurnBudget::new),
            ..RunCtx::default()
        };
        assert!(
            cx.budget.is_none(),
            "an absent budget was published as present"
        );
        assert!(RunCtx::new(CancellationToken::new()).budget.is_none());

        // The budget survives a clone of the context: same Arc, same token.
        let b = iota::chat::turns::turn_cap(2)
            .map(TurnBudget::new)
            .expect("budget");
        let cx = RunCtx {
            cancel: CancellationToken::new(),
            budget: Some(Arc::clone(&b)),
            ..RunCtx::default()
        };
        let copy = cx.clone();
        let got = copy
            .budget
            .as_ref()
            .expect("the budget did not survive the context");
        assert!(Arc::ptr_eq(got, &b));
        cx.cancel.cancel();
        assert!(copy.cancel.is_cancelled(), "a clone shares the run's token");
        // Spending through the clone spends the one pool.
        assert!(copy.budget.take());
        assert!(cx.budget.take());
        assert!(!cx.budget.take());
    }

    // Go: tool/tool_test.go artifactSlot laws (tool/tool.go:160-198; T-35)
    #[test]
    fn test_artifact_slot_last_post_wins() {
        use super::ArtifactSlot;
        use crate::tool::{Artifact, ArtifactKind};

        let slot = ArtifactSlot::default();
        assert!(slot.take().is_none(), "a fresh slot must be empty");
        slot.post(Artifact {
            kind: ArtifactKind::Diff,
            title: "a.txt".to_owned(),
            lines: vec!["-x".to_owned()],
        });
        slot.post(Artifact {
            kind: ArtifactKind::Note,
            title: "note".to_owned(),
            lines: vec!["2 round(s)".to_owned()],
        });
        // Last post wins.
        let got = slot.take().expect("the last post");
        assert_eq!(got.kind, ArtifactKind::Note);
        assert_eq!(got.title, "note");
        // take() drains: a second take sees an empty slot.
        assert!(slot.take().is_none());
        // A clone shares the same slot (a cloned context posts into the same slot).
        let clone = slot.clone();
        clone.post(Artifact {
            kind: ArtifactKind::Diff,
            title: "b".to_owned(),
            lines: Vec::new(),
        });
        assert_eq!(slot.take().expect("shared").title, "b");
    }
}
