//! The token meter and context budget (`chat/tokens.go`).
//!
//! Every call site in the loop — `note`, `settle`, `record`, `reset`, `snap`, `restore`,
//! `update`, `reseed` — is unconditional, exactly as Go's nil-receiver-safe `ctxMeter` made
//! it: a provider without token accounting gets [`CtxMeter::disabled`], whose methods are
//! all no-ops, so nothing in the loop branches on the meter's state (`TUI_CONTRACTS` §7).
//!
//! **Settled versus pending is the whole design.** Two quantities that must not be
//! confused:
//!
//! - `settled` is what the last API call actually carried (`Usage::context_tokens`), or a
//!   local count when the provider reported none. ONLY a settle point writes it.
//! - `pending` estimates what has landed SINCE — the user's message, tool results. It is
//!   RECOMPUTED from those messages, never accumulated into, so an over-estimate cannot
//!   outlive the messages that caused it.
//!
//! Folding both into one running total meant a stale estimate could only be corrected by
//! the next settle, and nothing distinguished "measured" from "guessed" inside the figure.
//!
//! **Where the usage comes from.** Go asked the provider (`UsageReporter.LastUsageFull`),
//! which reset its flag at the start of every call so a figure could never be read twice.
//! The Rust providers return each call's usage in its `RoundResult`/`ChatResult` and the
//! turn engine stamps it on the message the call produced (the D-55 shape), so the meter
//! books what it is HANDED and remembers it for exactly one settle: [`CtxMeter::book_call`]
//! stores it, and the next [`ContextBudget::update`] TAKES it. Consuming rather than
//! peeking is what replaces Go's per-call reset — a settle can never re-use an older
//! round's figure, it falls back to a local count and says so with the `≈`.
//!
//! **Why the meter owns the status row.** Go handed `ctxMeter` a `push` closure so the
//! context figure moves WHILE a turn streams instead of jumping once at turn end (a long
//! tool loop used to freeze it for minutes). The Rust twin keeps an `Arc<dyn Ui>` and the
//! model label the loop last published, so `note`/`settle`/`record` can repaint the row
//! from inside the tool loop, where the loop's own `push_status` cannot reach.
//!
//! Everything here runs on the chat-loop task. The `Mutex` around the occupancy exists to
//! SHARE it between the budget handle and the meter, not to arbitrate contention.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::llm::reqlog::RequestLog;
use crate::provider::model::Message;
use crate::provider::usage::Usage;
use crate::ui::facade::{StatusData, Ui};

use crate::repl::tokens::{
    COMPACT_RESERVE_TOKENS, COMPACT_SNOOZE_PERCENT, COMPACT_THRESHOLD_PERCENT,
    DEFAULT_CONTEXT_WINDOW, TokenCounter,
};

/// The window and its occupancy — the state [`ContextBudget`] and [`CtxMeter`] share.
#[derive(Clone, Copy, Debug, Default)]
struct Occupancy {
    /// The context window in tokens (always `> 0`).
    window: u64,
    /// Measured: what the last settle point wrote.
    settled: u64,
    /// Estimated: what has landed since that settle.
    pending: u64,
    /// Whether `settled` came from the provider rather than the local tokenizer.
    have_usage: bool,
    /// The last booked call's usage, CONSUMED by the next settle (Go's per-call
    /// `LastUsageFull` reset, expressed as ownership).
    last_usage: Option<Usage>,
    /// The last booked call's usage, NEVER consumed — `/status`'s "Last turn" row, which
    /// Go read straight off the provider (`provider.UsageReporter.LastUsageFull`), where
    /// it survives every settle.
    last_full: Option<Usage>,
}

impl Occupancy {
    fn used(self) -> u64 {
        self.settled + self.pending
    }

    /// The settle rule, in ONE place: the last booked call's real occupancy when there is
    /// one — CONSUMED, so it can never be measured twice — else the local count that was
    /// taken outside the lock. `pending` is superseded either way.
    fn settle_with(&mut self, counted: u64) {
        self.pending = 0;
        if let Some(u) = self.last_usage.take() {
            self.settled = u.context_tokens();
            self.have_usage = true;
        } else {
            self.settled = counted;
            self.have_usage = false;
        }
    }
}

/// The usage at which the next request should compact first (Go `budget.threshold`).
///
/// Two rules, whichever leaves MORE window to work with: a share of the window, which keeps
/// small windows safe (80% of 20k still leaves 4k, where a flat 16k reserve would put the
/// trigger at or below zero and compact forever), and window-minus-reserve, which stops
/// large windows throwing away a fifth of their capacity (80% of 1M would interrupt the
/// user with 200k still free).
fn threshold_of(window: u64) -> u64 {
    let pct = window * COMPACT_THRESHOLD_PERCENT / 100;
    window.saturating_sub(COMPACT_RESERVE_TOKENS).max(pct)
}

type Shared = Arc<Mutex<Occupancy>>;

fn lock(st: &Shared) -> MutexGuard<'_, Occupancy> {
    st.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A budget snapshot taken before a turn and restored per retry attempt (Go `budgetSnap`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetSnap {
    settled: u64,
    pending: u64,
    have_usage: bool,
}

/// The context budget: the window, and how much of it the conversation occupies
/// (`chat/tokens.go` `contextBudget`).
pub struct ContextBudget {
    st: Shared,
    counter: TokenCounter,
}

impl ContextBudget {
    /// A budget over `window` tokens (`0` → the 128k default; `chat/tokens.go`
    /// `newContextBudget`).
    pub fn new(window: u64) -> Self {
        Self {
            st: Arc::new(Mutex::new(Occupancy {
                window: if window == 0 {
                    DEFAULT_CONTEXT_WINDOW
                } else {
                    window
                },
                ..Occupancy::default()
            })),
            counter: TokenCounter::new(),
        }
    }

    /// The tokenizer this budget estimates with — shared with the thinking meter so one
    /// chat has ONE counter (Go passed `budget.counter` straight into `newTranscript`).
    pub fn counter(&self) -> TokenCounter {
        self.counter
    }

    /// The live meter over this budget, publishing the status row through `ui`
    /// (`chat/tokens.go` `newCtxMeter`). Built only for a provider that reports usage;
    /// everything else keeps [`CtxMeter::disabled`].
    ///
    /// `model` seeds the row's label so the meter's FIRST publish — Go seeds the session
    /// totals before the loop's own `pushStatus` — already carries it; the loop keeps it
    /// current through [`CtxMeter::publish_status`].
    ///
    /// `reqlog` is the run's `/debug` request log: the meter's own repaints read its `verbose()`
    /// for the status row's `debug` segment, so the two publishers (this and the loop's
    /// `push_status`) can never drift (T3 design §4.3).
    pub fn meter(&self, ui: Arc<dyn Ui>, model: String, reqlog: Arc<RequestLog>) -> CtxMeter {
        CtxMeter(Some(MeterInner {
            ui,
            st: Arc::clone(&self.st),
            counter: self.counter,
            since: Vec::new(),
            session: Usage::default(),
            model: Mutex::new(model),
            reqlog,
        }))
    }

    /// The configured window — what `/save` and `/model` stamp into session meta.
    pub fn window(&self) -> u64 {
        lock(&self.st).window
    }

    /// Adopts a new window (`/model`'s Context tab, a resumed session's meta).
    ///
    /// A zero never clobbers a real one: the shell made that choice and both mounts must
    /// agree, and every caller (`apply_session_tuning`, the Context presets) already only
    /// offers positive figures.
    pub fn set_window(&mut self, window: u64) {
        if window > 0 {
            lock(&self.st).window = window;
        }
    }

    /// Tokens the next request is projected to carry: measured plus estimated.
    pub fn used(&self) -> u64 {
        lock(&self.st).used()
    }

    /// Whether [`ContextBudget::used`] came from the provider rather than a local estimate
    /// (the `"≈"` prefix marks estimates).
    pub fn have_usage(&self) -> bool {
        lock(&self.st).have_usage
    }

    /// The last API call's full usage, or `None` before the first one — `/status`'s
    /// "Last turn" row (Go's `provider.UsageReporter.LastUsageFull`). Unlike the figure a
    /// settle consumes (`Occupancy::last_usage`), this one survives every settle, which is
    /// exactly the pair of fields Go had.
    #[allow(clippy::misnamed_getters)]
    pub fn last_usage(&self) -> Option<Usage> {
        lock(&self.st).last_full
    }

    /// Re-measures at a turn boundary (Go `budget.update`): the last booked call's real
    /// occupancy when there is one, else a local count over the whole history. Either way
    /// the pending estimate is superseded.
    pub fn update(&mut self, history: &[Message]) {
        let counted = self.counter.count_messages(history);
        lock(&self.st).settle_with(counted);
    }

    /// Re-seeds from a locally counted history — compaction and session swaps, where the
    /// provider's last figure no longer describes what will be sent (Go `budget.reseed`).
    pub fn reseed(&mut self, history: &[Message]) {
        let counted = self.counter.count_messages(history);
        let mut st = lock(&self.st);
        st.settled = counted;
        st.pending = 0;
        st.have_usage = false;
        st.last_usage = None;
    }

    /// The per-attempt rollback snapshot (Go `budget.snap`).
    pub fn snap(&self) -> BudgetSnap {
        let st = lock(&self.st);
        BudgetSnap {
            settled: st.settled,
            pending: st.pending,
            have_usage: st.have_usage,
        }
    }

    /// Restores a [`ContextBudget::snap`] (Go `budget.restore`) — a retried, interrupted or
    /// failed turn takes its live estimates back with its messages.
    pub fn restore(&mut self, snap: BudgetSnap) {
        let mut st = lock(&self.st);
        st.settled = snap.settled;
        st.pending = snap.pending;
        st.have_usage = snap.have_usage;
    }

    /// The usage at which the next request should compact first (Go `budget.threshold`;
    /// see [`threshold_of`] for the two rules).
    pub fn threshold(&self) -> u64 {
        threshold_of(lock(&self.st).window)
    }

    /// Whether the next request (current usage plus `extra` tokens of new, not-yet-sent
    /// content) would reach the threshold (Go `budget.shouldCompact`).
    pub fn should_compact(&self, extra: u64) -> bool {
        let st = lock(&self.st);
        st.window > 0 && st.used().saturating_add(extra) >= threshold_of(st.window)
    }

    /// Whether the auto-compaction confirmation should be offered before the next request
    /// (Go `budget.shouldOfferCompact`): the threshold is reached and — if the user
    /// declined before (`declined_at` = the usage recorded at that decline; `0` = never) —
    /// usage has since grown by 5% of the window.
    pub fn should_offer_compact(&self, extra: u64, declined_at: u64) -> bool {
        if !self.should_compact(extra) {
            return false;
        }
        if declined_at == 0 {
            return true;
        }
        let st = lock(&self.st);
        let step = st.window * COMPACT_SNOOZE_PERCENT / 100;
        st.used().saturating_add(extra) >= declined_at.saturating_add(step)
    }

    /// The `"[≈]<used> / <window> (<pct>%)"` figure (`chat/tokens.go` `budget.status`) — a
    /// leading `≈` marks a local estimate.
    pub fn status(&self) -> String {
        let st = lock(&self.st);
        status_text(st.used(), st.window, st.have_usage)
    }
}

/// The shared renderer behind `budget.status()` (kept out of the lock so the two mounts
/// can be compared line for line).
fn status_text(used: u64, window: u64, have_usage: bool) -> String {
    let pct = (used * 100).checked_div(window).unwrap_or(0);
    let prefix = if have_usage { "" } else { "≈" };
    format!(
        "{prefix}{} / {} ({pct}%)",
        crate::text::tokens(used),
        crate::text::tokens(window),
    )
}

/// The context meter (`chat/tokens.go` `ctxMeter`): keeps the status line's context figure
/// honest across a turn.
///
/// The user's message and every tool result move it the moment they land, and each
/// completed round settles it with the provider's real usage. `None` — a provider without
/// token accounting — is Go's nil meter: a no-op everywhere, so the call sites stay
/// unconditional.
///
/// Streamed output is deliberately NOT counted. It was, and it was the least trustworthy
/// input the figure had: the reasoning text a provider streams is a summary of thinking
/// whose real cost the next request carries in a wholly different form, so the meter would
/// climb through a long stream and then drop when the settle measured what had actually
/// been sent.
pub struct CtxMeter(Option<MeterInner>);

/// The live meter internals.
struct MeterInner {
    /// The status row's sink.
    ui: Arc<dyn Ui>,
    /// The occupancy shared with the budget.
    st: Shared,
    /// The estimator behind `pending`.
    counter: TokenCounter,
    /// Every message appended since the last settle. `pending` is RECOMPUTED from it
    /// rather than accumulated, so a re-estimate corrects itself and a reset cannot leave
    /// residue behind in the figure.
    since: Vec<Message>,
    /// What every API call of this session cost — the status line's ↑/↓ figures. Unlike
    /// the budget (which measures what the NEXT request carries) it only grows, and it
    /// survives a resume.
    session: Usage,
    /// The model label the loop last published, so a meter-driven repaint keeps it.
    model: Mutex<String>,
    /// The run's `/debug` log — the status row's `debug` segment reads `verbose()` live.
    reqlog: Arc<RequestLog>,
}

impl CtxMeter {
    /// The nil meter: a provider without token accounting (Go's `ctxm == nil`).
    pub fn disabled() -> Self {
        Self(None)
    }

    /// Whether token accounting is live — the gate `/compact`'s registration, the
    /// auto-offer and the `/status` token rows read.
    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Books a message into the live estimate the moment it lands (Go `ctxMeter.note`): a
    /// 50k-token file read should move the meter when it lands, not a round later.
    pub fn note(&mut self, m: &Message) {
        let Some(inner) = self.0.as_mut() else { return };
        inner.since.push(m.clone());
        let pending = inner.counter.count_messages(&inner.since);
        lock(&inner.st).pending = pending;
        inner.publish();
    }

    /// Settles against the real history at a round boundary (Go `ctxMeter.settle`): the
    /// running estimate is replaced by the last booked call's occupancy, or by a local
    /// count when that call reported nothing.
    pub fn settle(&mut self, history: &[Message]) {
        let Some(inner) = self.0.as_mut() else { return };
        inner.since.clear(); // superseded by the real figure
        let counted = inner.counter.count_messages(history);
        lock(&inner.st).settle_with(counted);
        inner.publish();
    }

    /// Books the API call that produced `m` (Go `ctxMeter.record`).
    ///
    /// Go stamped the message here from the provider's last usage; the Rust turn engine
    /// has already stamped it from the round's own result (D-55), so this side of the port
    /// READS the stamp and owns only the half Go's single call site also owned — the
    /// session totals and the figure the next settle measures with. A message that carries
    /// no usage books nothing, which is Go's "a call that reported nothing records
    /// nothing".
    pub fn record(&mut self, m: Option<&mut Message>) {
        let usage = m.and_then(|m| m.usage());
        self.book_call(usage);
    }

    /// Books a finished call whose cost rides no message — the compaction pass, which is a
    /// billed call of its own (Go's `ctxm.record(nil)` reading the provider). Returns what
    /// was booked, which is what the compaction marker persists.
    pub fn book_call(&mut self, usage: Option<Usage>) -> Option<Usage> {
        let inner = self.0.as_mut()?;
        let u = usage?;
        inner.session += u;
        {
            let mut st = lock(&inner.st);
            st.last_usage = Some(u);
            st.last_full = Some(u);
        }
        inner.publish();
        Some(u)
    }

    /// Clears the turn's live estimates (Go `ctxMeter.reset`) so residue cannot be charged
    /// to the next turn. The budget itself is the caller's business.
    pub fn reset(&mut self) {
        let Some(inner) = self.0.as_mut() else { return };
        inner.since.clear();
        lock(&inner.st).pending = 0;
    }

    /// Replaces the cumulative session figures wholesale — a resumed bundle brings its own
    /// log's totals (Go `ctxMeter.seedTotals`).
    pub fn seed_totals(&mut self, u: Usage) {
        let Some(inner) = self.0.as_mut() else { return };
        inner.session = u;
        inner.publish();
    }

    /// The cumulative session usage; zero while disabled (Go `ctxMeter.totals`).
    pub fn totals(&self) -> Usage {
        self.0.as_ref().map_or_else(Usage::default, |m| m.session)
    }

    /// Publishes the whole status row with `model` as its label, returning whether the
    /// meter owns the row.
    ///
    /// The loop calls this from `push_status`; a `false` return means there is no token
    /// half and the caller should publish the model alone. Routing the loop's own push
    /// through the meter is what lets the meter repaint later, mid-turn, without knowing
    /// anything about providers.
    pub fn publish_status(&self, model: &str) -> bool {
        let Some(inner) = self.0.as_ref() else {
            return false;
        };
        {
            let mut slot = inner.model.lock().unwrap_or_else(PoisonError::into_inner);
            model.clone_into(&mut slot);
        }
        inner.publish();
        true
    }
}

impl MeterInner {
    /// Repaints the status row from the shared occupancy and the session totals.
    fn publish(&self) {
        let st = *lock(&self.st);
        let model = self
            .model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.ui.set_status(StatusData {
            model,
            ctx_used: st.used(),
            ctx_window: st.window,
            estimated: !st.have_usage,
            in_tokens: self.session.input,
            out_tokens: self.session.output,
            cache_hit_pct: self.session.cache_hit_rate(),
            debug: self.reqlog.verbose(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::provider::model::{Body, Message, ToolBody};
    use crate::provider::usage::Usage;
    use crate::testing::ScriptedUi;

    use super::{ContextBudget, CtxMeter};

    fn budget(window: u64) -> ContextBudget {
        ContextBudget::new(window)
    }

    fn meter(b: &ContextBudget) -> CtxMeter {
        b.meter(
            ScriptedUi::new(Vec::new()),
            "gpt-4o".to_owned(),
            Arc::new(crate::llm::reqlog::RequestLog::new()),
        )
    }

    fn tool(content: &str) -> Message {
        Message {
            content: content.to_owned(),
            body: Body::Tool(ToolBody {
                ..ToolBody::default()
            }),
            ..Message::default()
        }
    }

    // Go: chat/run_test.go:62 TestContextBudgetStatus — "used / window (pct)" with a
    // leading ≈ while usage is a local estimate. Both halves run here: the token-less half
    // is the shell's (src/meter.rs), the usage-bearing one needs the live budget.
    #[test]
    fn test_context_budget_status() {
        let mut b = budget(128_000);
        assert_eq!(b.status(), "≈0 / 128k (0%)");
        assert_eq!(budget(0).status(), "≈0 / 128k (0%)");

        let mut m = meter(&b);
        m.book_call(Some(Usage {
            input: 64_000,
            total: 64_000,
            ..Usage::default()
        }));
        b.update(&[]);
        assert_eq!(b.status(), "64k / 128k (50%)");
    }

    // Go: chat/tokens_test.go:55 TestCtxMeterLiveFlow — the meter moves the figure DURING a
    // turn: appended messages land in the pending estimate immediately, a settle replaces
    // the whole thing with the provider's real usage, and a snapshot restore rolls a failed
    // turn back out. pending is REPLACED on every note, never added to.
    #[test]
    fn test_ctx_meter_live_flow() {
        let mut b = budget(100_000);
        let mut m = meter(&b);
        // Seed the settled base the Go test constructs the struct with.
        m.book_call(Some(Usage {
            input: 10_000,
            total: 10_000,
            ..Usage::default()
        }));
        b.update(&[]);
        assert_eq!(b.used(), 10_000);
        assert!(b.have_usage());
        let snap = b.snap();

        m.note(&tool("a big tool result"));
        assert!(
            b.used() > 10_000,
            "note must move the figure above the settled base: {}",
            b.used()
        );
        let first = b.used() - 10_000;

        // A second note re-estimates the WHOLE pending set: two identical messages cost
        // exactly twice one, never more.
        m.note(&tool("a big tool result"));
        assert_eq!(b.used() - 10_000, 2 * first);

        // The next call's real figure supersedes the estimate wholesale.
        m.book_call(Some(Usage {
            input: 12_000,
            output: 500,
            total: 12_500,
            ..Usage::default()
        }));
        m.settle(&[]);
        assert_eq!(b.used(), 12_500);
        assert!(b.have_usage());

        // A failed turn: the estimate rolls back with the messages.
        m.note(&Message::user("a turn that will fail"));
        b.restore(snap);
        assert_eq!(b.used(), 10_000);
        assert!(b.have_usage());
    }

    // Go: chat/tokens_test.go:97 TestCtxMeterResetClearsPending — reset drops the pending
    // set at a turn boundary; residue there would be charged to the next turn.
    #[test]
    fn test_ctx_meter_reset_clears_pending() {
        let mut b = budget(100_000);
        let mut m = meter(&b);
        m.book_call(Some(Usage {
            input: 5_000,
            total: 5_000,
            ..Usage::default()
        }));
        b.update(&[]);
        m.note(&Message::user("some pending content"));
        assert!(b.used() > 5_000, "note recorded nothing");
        m.reset();
        assert_eq!(b.used(), 5_000);
    }

    // Go: chat/tokens_test.go:117 TestCtxMeterRecord — a finished call is booked in TWO
    // places at once (the message that goes to disk and the session totals), so a resumed
    // session recomputes exactly what the live status line showed. A call that reported
    // nothing books nothing.
    //
    // Adjusted for D-55: the Rust turn engine stamps the message from the round result, so
    // `record` READS the stamp instead of writing it (the totals half is unchanged).
    #[test]
    fn test_ctx_meter_record() {
        let b = budget(100_000);
        let mut m = meter(&b);
        let first = Usage {
            input: 1_200,
            output: 300,
            ..Usage::default()
        };
        let mut msg = Message::assistant("hi").with_usage(Some(first));
        m.record(Some(&mut msg));
        assert_eq!(m.totals(), first);

        // A call the provider reported no usage for: nothing added.
        let mut quiet = Message::assistant("quiet");
        m.record(Some(&mut quiet));
        assert_eq!(m.totals(), first, "a usage-less call moved the totals");

        // Another accounted call accumulates on top — including one carrying no message
        // at all (the compaction pass).
        m.book_call(Some(Usage {
            input: 800,
            output: 100,
            ..Usage::default()
        }));
        assert_eq!(
            m.totals(),
            Usage {
                input: 2_000,
                output: 400,
                ..Usage::default()
            }
        );

        // Seeding replaces them wholesale: resume, or a switch to another session.
        m.seed_totals(Usage {
            input: 9,
            output: 8,
            ..Usage::default()
        });
        assert_eq!(
            m.totals(),
            Usage {
                input: 9,
                output: 8,
                ..Usage::default()
            }
        );
    }

    // Go: chat/tokens_test.go:167 TestCtxMeterNilSafe — a provider without token accounting
    // is a no-op everywhere, so the call sites stay unconditional.
    #[test]
    fn test_ctx_meter_nil_safe() {
        let mut m = CtxMeter::disabled();
        assert!(!m.is_enabled());
        m.note(&Message::user("x"));
        m.settle(&[]);
        m.reset();
        let mut msg = Message::assistant("a").with_usage(Some(Usage {
            input: 5,
            ..Usage::default()
        }));
        m.record(Some(&mut msg));
        assert_eq!(m.book_call(Some(Usage::default())), None);
        assert_eq!(m.totals(), Usage::default());
        m.seed_totals(Usage {
            input: 1,
            ..Usage::default()
        });
        assert_eq!(m.totals(), Usage::default());
        assert!(
            !m.publish_status("gpt-4o"),
            "a nil meter owns no status row"
        );
    }

    // Go: chat/tokens_test.go:196 TestCompactThreshold — the trigger point takes whichever
    // rule leaves more room: the flat percentage on small windows, window-minus-reserve on
    // large ones.
    #[test]
    fn test_compact_threshold() {
        for (window, want, why) in [
            (
                20_000_u64,
                16_000_u64,
                "small window: 80% (a 16k reserve would leave 4k)",
            ),
            (128_000, 112_000, "128k: reserve beats 80% (102.4k)"),
            (1_000_000, 984_000, "1m: 80% would waste 184k of window"),
            (
                16_000,
                12_800,
                "window == reserve: percentage keeps it usable",
            ),
        ] {
            let b = budget(window);
            assert_eq!(b.threshold(), want, "window {window}: {why}");
            assert!(
                b.threshold() < window,
                "window {window}: the threshold must stay below the window"
            );
        }
    }

    // Go: chat/tokens_test.go:216 TestShouldOfferCompact — window 100k → threshold
    // max(80k, 100k-16k) = 84k; snooze step 5% → 5k.
    #[test]
    fn test_should_offer_compact() {
        // `settled` is only written by a settle point, so the fixture drives one.
        fn set(b: &mut ContextBudget, m: &mut CtxMeter, n: u64) {
            m.book_call(Some(Usage {
                input: n,
                total: n,
                ..Usage::default()
            }));
            b.update(&[]);
        }

        let mut b = budget(100_000);
        let mut m = meter(&b);
        set(&mut b, &mut m, 70_000);
        assert!(!b.should_offer_compact(0, 0), "below threshold: offered");

        set(&mut b, &mut m, 84_000);
        assert!(
            b.should_offer_compact(0, 0),
            "at threshold, never declined: not offered"
        );

        // Declined at 84k: snoozed until usage grows by 5% of the window.
        assert!(!b.should_offer_compact(0, 84_000), "just declined: offered");
        set(&mut b, &mut m, 88_000);
        assert!(!b.should_offer_compact(0, 84_000), "grown <5%: offered");
        set(&mut b, &mut m, 89_000);
        assert!(b.should_offer_compact(0, 84_000), "grown 5%: not offered");

        // `extra` counts toward the growth, matching `should_compact`'s accounting.
        set(&mut b, &mut m, 87_000);
        assert!(
            b.should_offer_compact(2_000, 84_000),
            "used+extra grown 5%: not offered"
        );
    }

    /// The status row the meter publishes carries the model label the loop last gave it,
    /// the shared occupancy and the session totals — the segments WP44's frame renders.
    #[test]
    fn publish_status_carries_the_model_and_the_token_half() {
        let ui = ScriptedUi::new(Vec::new());
        let mut b = budget(128_000);
        let mut m = b.meter(
            Arc::clone(&ui) as Arc<dyn crate::ui::facade::Ui>,
            String::new(),
            Arc::new(crate::llm::reqlog::RequestLog::new()),
        );
        assert!(m.publish_status("gpt-4o"));
        m.book_call(Some(Usage {
            input: 1_000,
            output: 200,
            cache_read: 400,
            cache_write: 0,
            total: 1_200,
        }));
        b.update(&[]);
        // A note repaints WITHOUT the loop pushing again: the label must survive.
        m.note(&Message::user("hello"));
        let last = ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                crate::testing::UiEvent::Status(s) => Some(s),
                _ => None,
            })
            .next_back()
            .expect("a status row was published");
        assert_eq!(last.model, "gpt-4o");
        assert_eq!(last.ctx_window, 128_000);
        assert_eq!(last.in_tokens, 1_000);
        assert_eq!(last.out_tokens, 200);
        assert!(last.ctx_used > 1_200, "the note must be inside the figure");
        // `estimated` qualifies the SETTLED base, not the pending estimate riding on it:
        // Go's `Estimated = !haveUsage` and `note` never touches `haveUsage`.
        assert!(
            !last.estimated,
            "a measured settle must not read as an estimate"
        );
    }
}
