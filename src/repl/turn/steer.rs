//! Steering: the queued-message drain at each ROUND boundary — the only place a user
//! message can legally enter mid-turn (a round's tool results must directly follow its
//! calls; chat/run.go:1016-1029, 1639-1644) — plus the `injected` record a turn-level
//! retry re-lands (run.go:1050-1058).

use std::sync::Arc;

use crate::provider::model::Message;
use crate::ui::facade::{Input, InputKind, Ui};

use crate::repl::context::meter::CtxMeter;
use crate::repl::render::transcript::Transcript;

/// The steering bookkeeping of one turn (Go's `steer` + `injected` closures on `Run`'s
/// stack): draining echoes the `❯` block — which SETTLES the running activity group, the
/// user being a stronger boundary than content — and remembers every taken injection so
/// a retried attempt can re-land it (the queue no longer holds it). A background job's
/// completion notice arrives the same way and is echoed as a notice line instead.
///
/// A bot's memory-flush notice is never injected: it is a turn of its own (docs/design/bot-mode.md §3.6.1).
/// One taken off the queue mid-turn is HELD and handed back to the loop, which queues it again after the
/// turn. And the flush turn itself takes nothing — a steerer built closed leaves the queue alone, so what
/// the user typed meanwhile is answered after the flush, by a turn with all its tools.
pub(crate) struct Steerer {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    injected: Vec<Message>,
    open: bool,
    held: Vec<Input>,
}

impl Steerer {
    /// A steerer for one turn over the facade and the transcript; `open: false` never drains.
    pub(crate) fn new(ui: Arc<dyn Ui>, tr: Arc<Transcript>, open: bool) -> Self {
        Self {
            ui,
            tr,
            injected: Vec::new(),
            open,
            held: Vec::new(),
        }
    }

    /// Drains the contiguous non-command prefix of the queue (the facade's
    /// `take_queued_messages` law — a queued slash command stops the take): each taken
    /// input is echoed FIRST (`tr.user` settles the activity group before the injection
    /// lands), recorded for retry re-landing, and booked into the meter (run.go:1019-1029).
    pub(crate) async fn drain(&mut self, ctxm: &mut CtxMeter) -> Vec<Message> {
        let mut out = Vec::new();
        if !self.open {
            return out;
        }
        for input in self.ui.take_queued_messages().await {
            if input.kind == InputKind::Notice && crate::repl::bot::is_flush_notice(&input.text) {
                self.held.push(input);
                continue;
            }
            // A host notice (a background job finished) rides the SAME queue and lands at the same
            // boundary, but it is not the user speaking: one dim headline, and the message says so.
            // It settles the running group exactly as the `❯` block does — the headline goes
            // under the call that was running when the job ended, not above its rows.
            let m = if input.kind == InputKind::Notice {
                self.tr.boundary_notice(&input.display);
                Message::notice(input.text)
            } else {
                self.tr.user(&input.display);
                Message::user(input.text)
            };
            out.push(m.clone());
            self.injected.push(m.clone());
            ctxm.note(&m);
        }
        out
    }

    /// Every injection taken so far this turn — what a retried attempt re-appends right
    /// after the user message (run.go:1050-1058; the queue no longer holds them).
    pub(crate) fn injected(&self) -> &[Message] {
        &self.injected
    }

    /// The flush notices taken off the queue and not injected, for the loop to queue again.
    pub(crate) fn take_held(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.held)
    }
}
