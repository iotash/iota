//! Steering: the queued-message drain at each ROUND boundary — the only place a user
//! message can legally enter mid-turn (a round's tool results must directly follow its
//! calls; chat/run.go:1016-1029, 1639-1644) — plus the `injected` record a turn-level
//! retry re-lands (run.go:1050-1058).

use std::sync::Arc;

use crate::provider::model::Message;
use crate::ui::facade::Ui;

use crate::repl::meter::CtxMeter;
use crate::repl::transcript::Transcript;

/// The steering bookkeeping of one turn (Go's `steer` + `injected` closures on `Run`'s
/// stack): draining echoes the `❯` block — which SETTLES the running activity group, the
/// user being a stronger boundary than content — and remembers every taken injection so
/// a retried attempt can re-land it (the queue no longer holds it).
pub(crate) struct Steerer {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    injected: Vec<Message>,
}

impl Steerer {
    /// A steerer for one turn over the facade and the transcript.
    pub(crate) fn new(ui: Arc<dyn Ui>, tr: Arc<Transcript>) -> Self {
        Self {
            ui,
            tr,
            injected: Vec::new(),
        }
    }

    /// Drains the contiguous non-command prefix of the queue (the facade's
    /// `take_queued_messages` law — a queued slash command stops the take): each taken
    /// input is echoed FIRST (`tr.user` settles the activity group before the injection
    /// lands), recorded for retry re-landing, and booked into the meter (run.go:1019-1029).
    pub(crate) async fn drain(&mut self, ctxm: &mut CtxMeter) -> Vec<Message> {
        let mut out = Vec::new();
        for input in self.ui.take_queued_messages().await {
            self.tr.user(&input.display);
            let m = Message::user(input.text);
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
}
