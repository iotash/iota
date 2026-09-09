//! The plain-terminal host (internal/host/ansi.go): the fallback every run appends. It maps the
//! conversation state onto the terminal's native progress indicator (OSC 9;4) and forwards
//! attention pings as OSC 9 notifications — both through the [`Ui`] facade, so the bytes are
//! written by the UI loop thread and nowhere else (T3 design §5.2).

use std::sync::Arc;

use crate::host::{Event, Host, Notifier, State, StateReporter};
use crate::ui::facade::{ProgressState, Ui};

/// The ANSI terminal host; named `"terminal"` (ansi.go:22).
pub struct AnsiHost {
    ui: Arc<dyn Ui>,
}

impl AnsiHost {
    /// A host over the live facade.
    pub fn new(ui: Arc<dyn Ui>) -> Self {
        Self { ui }
    }
}

impl Host for AnsiHost {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        Some(self)
    }

    fn as_notifier(&self) -> Option<&dyn Notifier> {
        Some(self)
    }
}

impl StateReporter for AnsiHost {
    fn set_state(&self, s: State) {
        self.ui.set_progress(progress_of(s));
    }
}

impl Notifier for AnsiHost {
    fn notify(&self, e: &Event) {
        self.ui.notify(&e.text);
    }
}

/// The OSC 9;4 state of a conversation state (ansi.go:24-37).
pub(crate) fn progress_of(s: State) -> ProgressState {
    match s {
        State::Busy => ProgressState::Busy,
        State::NeedsInput => ProgressState::Input,
        State::Error => ProgressState::Error,
        State::Idle => ProgressState::None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::AnsiHost;
    use crate::host::{Event, Host, Kind, State};
    use crate::testing::{ScriptedUi, UiEvent};
    use crate::ui::facade::{ProgressState, Ui};
    use std::sync::Arc;

    // Go: internal/host/ansi_test.go:19 TestANSIMapping — the ANSI host is a pure mapping onto
    // the ui facade: host semantics in, renderer-facing progress states and text out.
    #[test]
    fn test_ansi_mapping() {
        let ui = ScriptedUi::new(Vec::new());
        let a = AnsiHost::new(Arc::clone(&ui) as Arc<dyn Ui>);
        assert_eq!(a.name(), "terminal");

        let reporter = a.as_state_reporter().expect("ANSI reports state");
        for s in [State::Busy, State::NeedsInput, State::Error, State::Idle] {
            reporter.set_state(s);
        }
        a.as_notifier().expect("ANSI notifies").notify(&Event {
            kind: Kind::Done,
            text: "iota: The fix".to_owned(),
        });

        assert_eq!(
            ui.events(),
            vec![
                UiEvent::Progress(ProgressState::Busy),
                UiEvent::Progress(ProgressState::Input),
                UiEvent::Progress(ProgressState::Error),
                UiEvent::Progress(ProgressState::None),
                UiEvent::Notify("iota: The fix".to_owned()),
            ]
        );
        // ANSI implements neither Closer nor BackgroundReporter (ansi.go has no such methods).
        assert!(a.as_closer().is_none());
        assert!(a.as_background().is_none());
    }
}
