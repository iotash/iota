//! Steering: the queued-message drain at each ROUND boundary — the only place a user
//! message can legally enter mid-turn (a round's tool results must directly follow its
//! calls; chat/run.go:1016-1029, 1639-1644). Go also kept an `injected` record for its turn-level
//! retry to re-land (run.go:1050-1058); that replay is gone here (X-73), and the record with it.

use std::sync::Arc;

use crate::provider::model::Message;
use crate::ui::facade::{Input, InputKind, Ui};

use crate::repl::context::meter::CtxMeter;
use crate::repl::render::transcript::Transcript;

/// The steering bookkeeping of one turn (Go's `steer` closure on `Run`'s
/// stack): draining echoes the `❯` block — which SETTLES the running activity group, the
/// user being a stronger boundary than content. A background job's
/// completion notice arrives the same way and is echoed as a notice line instead.
///
/// A bot's memory-flush notice is never injected: it is a turn of its own (docs/design/bot-mode.md §3.6.1).
/// One taken off the queue mid-turn is HELD and handed back to the loop, which queues it again after the
/// turn. And the flush turn itself takes nothing — a steerer built closed leaves the queue alone, so what
/// the user typed meanwhile is answered after the flush, by a turn with all its tools.
///
/// A job's notice it injects is also RECORDED (`injected`): when a later round's 429 rolls the turn back, the
/// loop hands those back to the notice gate instead of losing them with the history (X-84). This is not Go's
/// replay record (X-73 removed that): nothing is re-sent from it, the gate decides when they go.
///
/// A message from another process (`InputKind::Message`, `repl::inbound`) lands like a job's notice. In a chat
/// that holds them for the user (its tools act without asking), it is HELD the same way, in the same list: the
/// loop queues it again after the turn, and the idle loop asks the user before the model reads it.
pub(crate) struct Steerer {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    open: bool,
    hold_messages: bool,
    held: Vec<Input>,
    injected: Vec<Input>,
}

impl Steerer {
    /// A steerer for one turn over the facade and the transcript; `open: false` never drains;
    /// `hold_messages`: messages from other processes are held, not injected.
    pub(crate) fn new(
        ui: Arc<dyn Ui>,
        tr: Arc<Transcript>,
        open: bool,
        hold_messages: bool,
    ) -> Self {
        Self {
            ui,
            tr,
            open,
            hold_messages,
            held: Vec::new(),
            injected: Vec::new(),
        }
    }

    /// Drains the contiguous non-command prefix of the queue (the facade's
    /// `take_queued_messages` law — a queued slash command stops the take): each taken
    /// input is echoed FIRST (`tr.user` settles the activity group before the injection
    /// lands), then booked into the meter (run.go:1019-1029).
    pub(crate) async fn drain(&mut self, ctxm: &mut CtxMeter) -> Vec<Message> {
        let mut out = Vec::new();
        if !self.open {
            return out;
        }
        for input in self.ui.take_queued_messages().await {
            let flush =
                input.kind == InputKind::Notice && crate::repl::bot::is_flush_notice(&input.text);
            if flush || (input.kind == InputKind::Message && self.hold_messages) {
                self.held.push(input);
                continue;
            }
            // A host notice (a background job finished) rides the SAME queue and lands at the same
            // boundary, but it is not the user speaking: one dim headline, and the message says so.
            // It settles the running group exactly as the `❯` block does — the headline goes
            // under the call that was running when the job ended, not above its rows. A message from
            // another process is the same kind of thing and lands the same way.
            let m = if input.kind.is_notice() {
                self.tr.boundary_notice(&input.display);
                let m = Message::notice(input.text.clone());
                // Only a job's notice goes back to the gate: a message from outside is not automatic
                // work, and a rollback loses it as it loses a line the user steered in (X-85).
                if input.kind == InputKind::Notice {
                    self.injected.push(input);
                }
                m
            } else {
                self.tr.user(&input.display);
                Message::user(input.text)
            };
            ctxm.note(&m);
            out.push(m);
        }
        out
    }

    /// What was taken off the queue and not injected — flush notices and held messages, in the order
    /// taken — for the loop to queue again.
    pub(crate) fn take_held(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.held)
    }

    /// The jobs' notices this turn injected, in arrival order (X-84).
    pub(crate) fn take_injected(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.injected)
    }
}
