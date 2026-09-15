//! The busy-phase controller of a turn (chat/run.go:1184-1242 `turnPhases`) and the upload
//! phase's watcher (T3 design D9): [`Phases`] owns the status row's busy label — `set` is a no-op
//! when the label is unchanged, otherwise it stops the old phase and starts the new one; `end` is
//! idempotent. It is `Clone` over an `Arc<Mutex<_>>` because the send-progress closure runs on the
//! reqwest driver while the round's `RenderSink` owns the other clone.

use std::sync::{Arc, Mutex};

use crate::llm::progress::TurnProgress;
use crate::sync::lock;
use crate::ui::facade::{BusyGuard, Ui};

/// The label while the request is in flight (run.go:1222,1230).
pub(crate) const PHASE_WAITING: &str = "Waiting for the model";
/// The label while a large request body uploads (run.go:1220).
pub(crate) const PHASE_SENDING: &str = "Sending request";
/// Bodies at or above this size narrate their upload (progress.go:18).
pub(crate) const SEND_PROGRESS_MIN: u64 = 128 * 1024;

/// The busy-phase controller (see the module docs).
#[derive(Clone)]
pub(crate) struct Phases(Arc<Mutex<PhasesInner>>);

struct PhasesInner {
    ui: Arc<dyn Ui>,
    label: String,
    guard: Option<BusyGuard>,
}

impl Phases {
    /// A controller with no phase running.
    pub(crate) fn new(ui: Arc<dyn Ui>) -> Self {
        Self(Arc::new(Mutex::new(PhasesInner {
            ui,
            label: String::new(),
            guard: None,
        })))
    }

    /// Starts (or relabels) the busy phase; a repeated label is a no-op.
    pub(crate) fn set(&self, label: &str) {
        let mut inner = lock(&self.0);
        if inner.guard.is_some() && inner.label == label {
            return;
        }
        if let Some(g) = inner.guard.take() {
            g.stop();
        }
        label.clone_into(&mut inner.label);
        let guard = inner.ui.busy(label);
        inner.guard = Some(guard);
    }

    /// Ends the running phase, if any.
    pub(crate) fn end(&self) {
        let mut inner = lock(&self.0);
        if let Some(g) = inner.guard.take() {
            g.stop();
        }
        inner.label.clear();
    }

    /// Updates the busy detail (the upload's `done / total`) without touching the phase clock.
    pub(crate) fn detail(&self, d: &str) {
        let inner = lock(&self.0);
        inner.ui.busy_detail(d);
    }
}

/// Clears the reporter's handlers on drop (Go's deferred `rep.SetHandlers(nil, nil)`).
pub(crate) struct PhaseWatch(Arc<TurnProgress>);

impl Drop for PhaseWatch {
    fn drop(&mut self) {
        self.0.set_handlers(None);
    }
}

/// Narrates a request's upload on the busy row (run.go:1213-1231): installs the reporter's two
/// handlers, sets [`PHASE_WAITING`] now, and returns the watch that clears the handlers when the
/// round ends.
///
/// `on_send` only narrates bodies at or above [`SEND_PROGRESS_MIN`] — smaller uploads are not
/// worth a phase. It sets the label once (a repeated [`Phases::set`] is a no-op, so the phase
/// clock keeps running) and updates the detail on every report. `on_sent` fires when the
/// round-trip returns — headers received, or the attempt failed — and hands the row back to
/// "waiting".
pub(crate) fn watch_phases(tp: &Arc<TurnProgress>, phases: Phases) -> PhaseWatch {
    let sending = phases.clone();
    let start = phases.clone();
    tp.set_handlers(Some((
        Box::new(move |done, total| {
            if total >= SEND_PROGRESS_MIN {
                sending.set(PHASE_SENDING);
                sending.detail(&format!(
                    "{} / {}",
                    format_byte_size(done),
                    format_byte_size(total)
                ));
            }
        }),
        // `phases` itself moves into the "headers received" handler, which outlives this call.
        Box::new(move || phases.set(PHASE_WAITING)),
    )));
    start.set(PHASE_WAITING);
    PhaseWatch(Arc::clone(tp))
}

/// `≥ 1 MiB` → `"{:.1} MB"`, `≥ 1 KiB` → `"{:.1} KB"`, else `"{n} B"` (run.go:1233-1242).
#[allow(clippy::cast_precision_loss)] // Go's `float64(n)` — the same lossy widening, byte-exact
pub(crate) fn format_byte_size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / f64::from(1u32 << 20))
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / f64::from(1u32 << 10))
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{
        PHASE_SENDING, PHASE_WAITING, Phases, SEND_PROGRESS_MIN, format_byte_size, watch_phases,
    };
    use crate::llm::progress::TurnProgress;
    use crate::testing::{ScriptedUi, UiEvent};
    use crate::ui::facade::Ui;
    use std::sync::Arc;

    fn busy(label: &str) -> UiEvent {
        UiEvent::Busy(label.to_owned())
    }

    fn detail(d: &str) -> UiEvent {
        UiEvent::BusyDetail(d.to_owned())
    }

    #[test]
    fn set_dedups_and_end_is_idempotent() {
        let ui = ScriptedUi::new(Vec::new());
        let phases = Phases::new(Arc::clone(&ui) as Arc<dyn Ui>);
        let twin = phases.clone();
        phases.set("a");
        twin.set("a"); // same label through the other clone: no-op
        twin.set("b");
        phases.end();
        phases.end();
        assert_eq!(
            ui.events(),
            vec![busy("a"), UiEvent::BusyOff, busy("b"), UiEvent::BusyOff]
        );
    }

    // Go: chat/run.go:1233-1242 formatByteSize — `%.1f` on the binary value, integer bytes below
    // a kibibyte.
    #[test]
    fn format_byte_size_table() {
        for (n, want) in [
            (0u64, "0 B"),
            (1, "1 B"),
            (1023, "1023 B"),
            (1024, "1.0 KB"),
            (131_072, "128.0 KB"),
            (1_048_575, "1024.0 KB"),
            (1_048_576, "1.0 MB"),
            (1_258_291, "1.2 MB"),
            (5 * 1_048_576, "5.0 MB"),
        ] {
            assert_eq!(format_byte_size(n), want, "{n}");
        }
    }

    // Go: chat/run.go:1215-1231 watchPhases — the install sets "Waiting for the model" and the
    // handlers narrate nothing for a body under `sendProgressMinBytes`.
    #[test]
    fn watch_ignores_a_small_upload() {
        let ui = ScriptedUi::new(Vec::new());
        let phases = Phases::new(Arc::clone(&ui) as Arc<dyn Ui>);
        let tp = TurnProgress::new();
        {
            let _watch = watch_phases(&tp, phases.clone());
            let total = SEND_PROGRESS_MIN - 1;
            tp.send(total, total);
            tp.sent();
        }
        phases.end();
        assert_eq!(
            ui.events(),
            vec![busy(PHASE_WAITING), UiEvent::BusyOff],
            "an upload below 128 KiB must not raise the sending phase"
        );
    }

    // Go: chat/run.go:1215-1231 watchPhases — at 128 KiB the row switches to "Sending request"
    // with a `{done} / {total}` detail, then back to "Waiting for the model" once the round-trip
    // returns. The repeated `set` while the detail advances keeps the phase clock running.
    #[test]
    fn watch_narrates_a_large_upload() {
        let ui = ScriptedUi::new(Vec::new());
        let phases = Phases::new(Arc::clone(&ui) as Arc<dyn Ui>);
        let tp = TurnProgress::new();
        {
            let _watch = watch_phases(&tp, phases.clone());
            tp.send(65_536, SEND_PROGRESS_MIN);
            tp.send(SEND_PROGRESS_MIN, SEND_PROGRESS_MIN);
            tp.sent();
        }
        phases.end();
        assert_eq!(
            ui.events(),
            vec![
                busy(PHASE_WAITING),
                UiEvent::BusyOff,
                busy(PHASE_SENDING),
                detail("64.0 KB / 128.0 KB"),
                detail("128.0 KB / 128.0 KB"),
                UiEvent::BusyOff,
                busy(PHASE_WAITING),
                UiEvent::BusyOff,
            ]
        );
    }

    #[test]
    fn watch_sets_waiting_and_clears_handlers_on_drop() {
        let ui = ScriptedUi::new(Vec::new());
        let phases = Phases::new(Arc::clone(&ui) as Arc<dyn Ui>);
        let tp = TurnProgress::new();
        {
            let _watch = watch_phases(&tp, phases.clone());
        }
        // The watch is gone: a late report from a background call narrates nothing.
        tp.send(SEND_PROGRESS_MIN, SEND_PROGRESS_MIN);
        tp.sent();
        phases.end();
        assert_eq!(ui.events(), vec![busy(PHASE_WAITING), UiEvent::BusyOff]);
    }
}
