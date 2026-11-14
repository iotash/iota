//! The herdr-core host (docs/design/herdr-core-host.md). herdr-core (`hc`) is the terminal core behind
//! herdr's desktop app — a new product, not a version of the herdr multiplexer (`super::herdr`); both
//! are live, and iota supports both. hc has no socket method for a program's state: it reads the
//! program's own OSC 7501 reports ("Program Status Protocol", rev 0.4) off the terminal, so this host
//! speaks IN BAND — `ESC ] 7501 ; state=<word>:app=iota ST`, one whole record per report (every report
//! replaces the record; `state=clear` removes it).
//!
//! **Only inside an hc terminal.** An OSC another terminal does not know is at best ignored and at
//! worst garbage, so nothing here is written unless [`detect_hc`] matched: `HC_TERMINAL_ID` is set and
//! `TERM_PROGRAM` is not `herdr`. The detector sits BEFORE herdr's (`super::DETECTORS`): an hc
//! terminal started from a herdr pane inherits the pane's `HERDR_*`, and reporting to that pane would
//! be the wrong surface; a herdr pane inside an hc terminal says `TERM_PROGRAM=herdr` (herdr ≥0.9.2),
//! which hands it back to herdr.
//!
//! **The writer.** Interactively the bytes go through the facade ([`InBand::write_through`], given by
//! `Presenter::show_turn_on`), so the UI thread writes them between draws and never inside one. A `-m`
//! run has no facade; there the host opens `/dev/tty` on its first report and writes it directly — the
//! reply on stdout may be a pipe, the record belongs to the terminal — and does nothing at all when
//! there is no controlling terminal. A failed write is a `tracing::debug!` and nothing else.
//!
//! **The states.** `Busy` is `working`, `NeedsInput` is `blocked` (no `kind`: the state carries no
//! reason), `Error` is `error` (hc has it natively), `Idle` is `idle` — not `done`, whose "finished,
//! not yet seen" rendering in the desktop app is unknown. While the record says `blocked`, `hc send`
//! refuses with `kind: blocked` (exit 9), so a supervisor cannot type into an approval.
//!
//! **What it does not do.** No [`Notifier`](crate::host::Notifier): the ANSI host keeps OSC 9, and hc
//! makes no event of it. No [`BackgroundReporter`](crate::host::BackgroundReporter): hc answers OSC 11
//! itself. No [`SessionReporter`](crate::host::SessionReporter): the protocol has no key for a session.
//! No [`ChildEnvironment`](crate::host::ChildEnvironment): the channel is the tty, not a variable — a
//! child that writes 7501 to the same tty replaces iota's record whatever its environment (measured;
//! the design doc), and the model's own `hc` CLI needs `HC_TERMINAL_ID`.

use std::fs::File;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use crate::BoxFuture;
use crate::host::{Closer, EnvironmentContributor, Host, InBand, Probe, State, StateReporter};
use crate::sync::lock;
use crate::ui::facade::Ui;

/// The host's name — the product's, as `hc --skill` names it.
pub(crate) const HC_NAME: &str = "herdr-core";
/// The terminal the chat runs in (`t4`); hc sets it in every terminal it starts.
pub(crate) const HC_TERMINAL_ID: &str = "HC_TERMINAL_ID";
/// `herdr` inside a herdr pane (herdr ≥0.9.2); hc sets its own value in an hc terminal.
pub(crate) const TERM_PROGRAM: &str = "TERM_PROGRAM";
/// The `app` key of every report: the program name.
pub(crate) const HC_APP: &str = crate::app::NAME;

/// The protocol word of a state (the module doc).
pub(crate) fn word(s: State) -> &'static str {
    match s {
        State::Busy => "working",
        State::NeedsInput => "blocked",
        State::Idle => "idle",
        State::Error => "error",
    }
}

/// One OSC 7501 report: `state=<word>:app=iota` for a state, `state=clear` for `None`, framed as
/// `ESC ] 7501 ; … ESC \`.
pub(crate) fn report(s: Option<State>) -> String {
    let body = match s {
        Some(s) => format!("state={}:app={HC_APP}", word(s)),
        None => "state=clear".to_owned(),
    };
    format!("\x1b]7501;{body}\x1b\\")
}

/// Where the reports go.
enum Sink {
    /// The controlling terminal, not opened yet (the first report opens it).
    Tty,
    /// A writer: the opened `/dev/tty`, or a test's recorder.
    Write(Box<dyn std::io::Write + Send>),
    /// The facade (interactive).
    Ui(Arc<dyn Ui>),
    /// Nowhere: there is no controlling terminal.
    Off,
}

/// The herdr-core host: the terminal's id and the sink its reports are written to.
pub struct HcHost {
    terminal: String,
    sink: Mutex<Sink>,
}

impl HcHost {
    /// A host for terminal `terminal` that writes `/dev/tty` until it is given a facade.
    pub(crate) fn new(terminal: String) -> Self {
        Self {
            terminal,
            sink: Mutex::new(Sink::Tty),
        }
    }

    /// A host writing to `w` until it is given a facade (tests).
    #[cfg(test)]
    pub(crate) fn with_writer(terminal: &str, w: Box<dyn std::io::Write + Send>) -> Self {
        Self {
            terminal: terminal.to_owned(),
            sink: Mutex::new(Sink::Write(w)),
        }
    }

    /// Writes one report to the sink, opening `/dev/tty` first if it is the first.
    fn write(&self, seq: &str) {
        let mut sink = lock(&self.sink);
        if matches!(*sink, Sink::Tty) {
            *sink = match File::options().write(true).open("/dev/tty") {
                Ok(f) => Sink::Write(Box::new(f)),
                Err(e) => {
                    tracing::debug!("herdr-core: no terminal to report to: {e}");
                    Sink::Off
                }
            };
        }
        match &mut *sink {
            Sink::Ui(ui) => ui.program_status(seq),
            Sink::Write(w) => {
                if let Err(e) = w.write_all(seq.as_bytes()).and_then(|()| w.flush()) {
                    tracing::debug!("herdr-core: report not written: {e}");
                }
            }
            Sink::Tty | Sink::Off => {}
        }
    }
}

impl Host for HcHost {
    fn name(&self) -> &'static str {
        HC_NAME
    }

    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        Some(self)
    }

    fn as_closer(&self) -> Option<&dyn Closer> {
        Some(self)
    }

    fn as_environment(&self) -> Option<&dyn EnvironmentContributor> {
        Some(self)
    }

    fn as_in_band(&self) -> Option<&dyn InBand> {
        Some(self)
    }
}

impl StateReporter for HcHost {
    fn set_state(&self, s: State) {
        self.write(&report(Some(s)));
    }
}

impl Closer for HcHost {
    /// Removes the record (`state=clear`). hc also drops it by itself when the program exits; the
    /// clear says so first, and is the whole of the clean-up when iota is not the terminal's program
    /// (it ran from a shell, which hc does not see return until it is in the foreground again).
    fn close(&self) -> BoxFuture<'_, ()> {
        self.write(&report(None));
        Box::pin(async {})
    }
}

impl InBand for HcHost {
    fn write_through(&self, ui: Arc<dyn Ui>) {
        *lock(&self.sink) = Sink::Ui(ui);
    }
}

impl EnvironmentContributor for HcHost {
    /// `host: herdr-core` and the terminal — the address `hc get`, `hc read` and `hc label` take.
    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("host".to_owned(), HC_NAME.to_owned()),
            ("hc terminal".to_owned(), self.terminal.clone()),
        ]
    }
}

/// `Some(HcHost)` when `HC_TERMINAL_ID` is non-empty and `TERM_PROGRAM` is not `herdr` (the module
/// doc). Opens nothing: the tty is opened by the first report, if one is ever made.
pub(crate) fn detect_hc(probe: &Probe) -> Option<Box<dyn Host>> {
    let terminal = probe.env.var(HC_TERMINAL_ID)?;
    if probe.env.var(TERM_PROGRAM).as_deref() == Some("herdr") {
        return None;
    }
    Some(Box::new(HcHost::new(terminal)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{HcHost, detect_hc, report};
    use crate::app::env::Env;
    use crate::host::{Host, Probe, State, detect};
    use crate::testing::{ScriptedUi, UiEvent};
    use crate::ui::facade::Ui;
    use std::sync::{Arc, Mutex};

    /// A writer whose bytes the test reads back.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Recorder {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn probe(vars: &[(&str, &str)]) -> Probe {
        Probe {
            env: Env::fixed(vars),
            look_path: Box::new(|_| None),
        }
    }

    /// What a herdr pane carries (`HERDR_BIN_PATH` too, so the CLI transport detects on Windows).
    const HERDR: [(&str, &str); 4] = [
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock"),
        ("HERDR_BIN_PATH", "/nonexistent/herdr"),
    ];

    /// The four ways the two products meet, each yielding exactly one host — the right one. Run
    /// through `detect`, so the order of `DETECTORS` is what is tested.
    #[tokio::test]
    async fn the_detection_matrix_yields_one_host_each() {
        let hc = [
            ("HC_TERMINAL_ID", "t4"),
            ("HC_SCOPE", "scope"),
            ("TERM_PROGRAM", "ghostty"),
        ];
        let plain_herdr = [&HERDR[..], &[("TERM_PROGRAM", "herdr")]].concat();
        // A: an hc core started from a herdr pane — its terminals inherit the pane's HERDR_*, and hc
        // sets its own TERM_PROGRAM (measured).
        let hc_in_herdr = [&HERDR[..], &hc[..]].concat();
        // B: a herdr pane inside an hc terminal — the pane may inherit HC_TERMINAL_ID, and herdr sets
        // TERM_PROGRAM=herdr.
        let herdr_in_hc = [
            &HERDR[..],
            &[("HC_TERMINAL_ID", "t4"), ("TERM_PROGRAM", "herdr")],
        ]
        .concat();
        for (case, vars, want) in [
            ("plain hc", &hc[..], "herdr-core"),
            ("plain herdr", &plain_herdr[..], "herdr"),
            ("A: hc nested in herdr", &hc_in_herdr[..], "herdr-core"),
            ("B: herdr inside hc", &herdr_in_hc[..], "herdr"),
        ] {
            let host = detect(&probe(vars)).unwrap_or_else(|| panic!("{case}: no host"));
            assert_eq!(host.name(), want, "{case}");
            if let Some(c) = host.as_closer()
                && want == "herdr"
            {
                c.close().await; // the release goes to a socket nobody listens on
            }
        }
    }

    /// An empty id is no hc terminal, and neither is a herdr pane that inherited one.
    #[test]
    fn the_detector_needs_an_id_and_not_herdr() {
        assert!(detect_hc(&probe(&[])).is_none());
        assert!(detect_hc(&probe(&[("HC_TERMINAL_ID", "")])).is_none());
        assert!(
            detect_hc(&probe(&[
                ("HC_TERMINAL_ID", "t1"),
                ("TERM_PROGRAM", "herdr")
            ]))
            .is_none()
        );
        assert!(detect_hc(&probe(&[("HC_TERMINAL_ID", "t1")])).is_some());
    }

    /// The byte pin: each state's exact sequence, and the clear on close.
    #[tokio::test]
    async fn each_state_is_one_exact_report_and_close_clears() {
        let rec = Recorder::default();
        let h = HcHost::with_writer("t4", Box::new(rec.clone()));
        let r = h.as_state_reporter().expect("hc reports state");
        for s in [State::Busy, State::NeedsInput, State::Error, State::Idle] {
            r.set_state(s);
        }
        h.as_closer().expect("hc closes").close().await;
        assert_eq!(
            rec.text(),
            concat!(
                "\x1b]7501;state=working:app=iota\x1b\\",
                "\x1b]7501;state=blocked:app=iota\x1b\\",
                "\x1b]7501;state=error:app=iota\x1b\\",
                "\x1b]7501;state=idle:app=iota\x1b\\",
                "\x1b]7501;state=clear\x1b\\",
            )
        );
    }

    /// Given a facade, the host writes through it and no longer to its own writer.
    #[tokio::test]
    async fn the_facade_takes_over_the_writes() {
        let rec = Recorder::default();
        let h = HcHost::with_writer("t4", Box::new(rec.clone()));
        let ui = ScriptedUi::new(Vec::new());
        h.as_in_band()
            .expect("hc is in band")
            .write_through(Arc::clone(&ui) as Arc<dyn Ui>);
        h.as_state_reporter().unwrap().set_state(State::Busy);
        h.as_closer().unwrap().close().await;
        assert_eq!(rec.text(), "", "the tty writer was used after the facade");
        assert_eq!(
            ui.events(),
            vec![
                UiEvent::ProgramStatus(report(Some(State::Busy))),
                UiEvent::ProgramStatus(report(None)),
            ]
        );
    }

    /// The presenter: hc is the first state reporter, so the ANSI host's OSC 9;4 is not written; OSC 9
    /// pings still go to the terminal; the facade is handed over by `show_turn_on`; the environment
    /// names the host and the terminal.
    #[tokio::test]
    async fn under_the_presenter_hc_takes_the_state_and_the_terminal_keeps_the_ping() {
        let ui = ScriptedUi::new(Vec::new());
        let hc = HcHost::with_writer("t4", Box::new(Recorder::default()));
        let p = crate::host::Presenter::over(
            Some(Box::new(hc)),
            Some(Box::new(crate::host::AnsiHost::new(
                Arc::clone(&ui) as Arc<dyn Ui>
            ))),
            true,
        )
        .show_turn_on(Arc::clone(&ui) as Arc<dyn Ui>);
        assert_eq!(p.detected_host(), Some("herdr-core"));
        p.set_state(State::Busy);
        p.notify(crate::host::Event {
            kind: crate::host::Kind::Done,
            text: "iota: done".to_owned(),
        });
        p.close().await;
        let events: Vec<UiEvent> = ui
            .events()
            .into_iter()
            .filter(|e| {
                matches!(
                    e,
                    UiEvent::ProgramStatus(_) | UiEvent::Progress(_) | UiEvent::Notify(_)
                )
            })
            .collect();
        assert_eq!(
            events,
            vec![
                UiEvent::ProgramStatus(report(Some(State::Busy))),
                UiEvent::Notify("iota: done".to_owned()),
                UiEvent::ProgramStatus(report(None)),
            ]
        );
        assert_eq!(
            p.environment(),
            vec![
                ("host".to_owned(), "herdr-core".to_owned()),
                ("hc terminal".to_owned(), "t4".to_owned()),
            ]
        );
    }
}
