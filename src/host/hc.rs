//! The herdr-core host (docs/design/herdr-core-host.md). herdr-core (`hc`) is the terminal core behind
//! herdr's desktop app — a new product, not a version of the herdr multiplexer (`super::herdr`); both
//! are live, and iota supports both. hc has no socket method for a program's state: it reads the
//! program's own OSC 7501 reports ("Program Status Protocol", rev 0.4) off the terminal, so this host
//! speaks IN BAND — `ESC ] 7501 ; state=<word>:app=iota ST`, one whole record per report (every report
//! replaces the record; `state=clear` removes it).
//!
//! **Only inside an hc terminal.** An OSC another terminal does not know is at best ignored and at
//! worst garbage, so nothing here is written unless [`detect_hc`] matched, and it asks the terminal
//! itself (docs/design/herdr-core-host.md §2). Without `HC_TERMINAL_ID` it is not an hc terminal and
//! not one byte goes to the tty. With it, the probe ([`probe_tty`], injected through
//! [`Probe::program_status`]) writes `ESC ] 7501 ; ? ST` chased by a DSR and reads the reply: hc
//! answers the query (in well under a millisecond, measured), while whatever stands between iota and
//! hc — a tmux, a herdr pane — answers only the DSR, and the host is then not hc. The environment
//! cannot say this: a variable names an ancestor, not what is across the tty, and hc's own
//! `TERM_PROGRAM=ghostty` is a value it is already asked to change. Only when the probe cannot run (no
//! controlling terminal, no answer in 100 ms, a background process group — whose raw mode would stop
//! it with `SIGTTOU`) does the environment decide: `TERM_PROGRAM` not `herdr`. Without a terminal the
//! host writes nothing anyway and the rule only keeps a leaked `HERDR_*` from taking the reports; a
//! background process can write, which is why the chat's own children do not get the id (below). The detector sits BEFORE herdr's (`super::DETECTORS`): an hc terminal started
//! from a herdr pane inherits the pane's `HERDR_*`, and reporting to that pane would be the wrong
//! surface.
//!
//! **Children do not get the id** ([`ChildEnvironment`]). A child of the chat that saw
//! `HC_TERMINAL_ID` would probe the same tty — racing the chat's input reader for the reply — and, an
//! `iota -m` being one, report over the chat's record and clear it when it ends. Withheld, a child does
//! not probe and writes nothing; the model is told the id is missing on purpose, and that its
//! `hc run` passes `--parent <id>` (the `hc` CLI takes an explicit id everywhere, measured).
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
//! Withholding the id does not stop a child that writes 7501 to the tty on its own (Claude Code
//! does): that replaces iota's record whatever its environment (measured; the design doc §4).

use std::fs::File;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use crate::BoxFuture;
use crate::host::{
    ChildEnvironment, Closer, EnvironmentContributor, Host, InBand, MessageEnvelope, Probe, State,
    StateReporter,
};
use crate::sync::lock;
use crate::ui::facade::Ui;

/// The host's name — the product's, as `hc --skill` names it.
pub(crate) const HC_NAME: &str = "herdr-core";
/// The terminal the chat runs in (`t4`); hc sets it in every terminal it starts.
pub(crate) const HC_TERMINAL_ID: &str = "HC_TERMINAL_ID";
/// `herdr` inside a herdr pane (herdr ≥0.9.2): the detector's rule when the terminal cannot be asked.
pub(crate) const TERM_PROGRAM: &str = "TERM_PROGRAM";
/// The `app` key of every report: the program name.
pub(crate) const HC_APP: &str = crate::app::NAME;

/// The probe: the OSC 7501 capability query (the DSR that ends the read is added by the round-trip).
pub(crate) const PROBE_QUERY: &[u8] = b"\x1b]7501;?\x1b\\";
/// What hc's answer to [`PROBE_QUERY`] starts with (`ESC ] 7501 ; ? : states=…`).
const PROBE_ANSWER: &[u8] = b"\x1b]7501;?";

/// What the model is told about the withheld id (the module doc). Short: every host line is paid for in
/// every request.
pub(crate) const HC_CHILD_NOTE: &str = "HC_TERMINAL_ID is unset in your shell on purpose; give hc the terminal id, and hc run --parent <id>";

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

    fn as_child_env(&self) -> Option<&dyn ChildEnvironment> {
        Some(self)
    }

    fn as_message_envelope(&self) -> Option<&dyn MessageEnvelope> {
        Some(self)
    }
}

/// herdr-core hands a sub-agent's message to the conversation as `<hc from="…">…</hc>`: the user
/// block shows the inner text under an `hc from:…` tab (DIVERGENCES X-107).
impl MessageEnvelope for HcHost {
    fn element(&self) -> &'static str {
        "hc"
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
    /// `host: herdr-core`, the terminal — the address `hc get`, `hc read` and `hc label` take — and the
    /// note on the id the model's shell lacks.
    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("host".to_owned(), HC_NAME.to_owned()),
            ("hc terminal".to_owned(), self.terminal.clone()),
            ("hc note".to_owned(), HC_CHILD_NOTE.to_owned()),
        ]
    }
}

impl ChildEnvironment for HcHost {
    /// `HC_TERMINAL_ID` (the module doc).
    fn withheld(&self) -> Vec<String> {
        vec![HC_TERMINAL_ID.to_owned()]
    }
}

/// `Some(HcHost)` when `HC_TERMINAL_ID` is non-empty and the terminal answers the OSC 7501 query — or,
/// when it cannot be asked, `TERM_PROGRAM` is not `herdr` (the module doc). Without the id the terminal
/// is not asked. Opens nothing for the reports: the tty is opened by the first one, if one is ever made.
pub(crate) fn detect_hc(probe: &Probe) -> Option<Box<dyn Host>> {
    let terminal = probe.env.var(HC_TERMINAL_ID)?;
    let hc = match (probe.program_status)() {
        Some(answered) => answered,
        None => probe.env.var(TERM_PROGRAM).as_deref() != Some("herdr"),
    };
    hc.then(|| Box::new(HcHost::new(terminal)) as Box<dyn Host>)
}

/// The real [`Probe::program_status`]: one [`PROBE_QUERY`] round-trip on the controlling terminal
/// (`ui::runtime::osc::round_trip`, the OSC 11 detect's mechanism). `Some(true)` when the reply
/// carries hc's answer, `Some(false)` when only the DSR came back, `None` without a terminal or a
/// reply in time.
pub(crate) fn probe_tty() -> Option<bool> {
    let reply = crate::ui::runtime::osc::round_trip(PROBE_QUERY).ok()?;
    Some(answered(&reply))
}

/// Whether a reply carries hc's answer to the probe.
fn answered(reply: &[u8]) -> bool {
    reply.windows(PROBE_ANSWER.len()).any(|w| w == PROBE_ANSWER)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{HC_CHILD_NOTE, HcHost, PROBE_QUERY, answered, detect_hc, report};
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

    /// A probe over `vars` whose terminal gives `answer` to the OSC 7501 query.
    fn probe(vars: &[(&str, &str)], answer: Option<bool>) -> Probe {
        Probe {
            env: Env::fixed(vars),
            look_path: Box::new(|_| None),
            program_status: Box::new(move || answer),
        }
    }

    /// What a herdr pane carries (`HERDR_BIN_PATH` too, so the CLI transport detects on Windows).
    const HERDR: [(&str, &str); 4] = [
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock"),
        ("HERDR_BIN_PATH", "/nonexistent/herdr"),
    ];

    /// The seven ways hc meets the others (docs/design/herdr-core-host.md §2), each yielding exactly
    /// one host — the right one, or none. Run through `detect`, so the order of `DETECTORS` is what is
    /// tested; the terminal's answer to the probe is what each nesting gives (measured for hc and tmux;
    /// herdr's is assumed, the design doc says why).
    #[tokio::test]
    async fn the_detection_matrix_yields_one_host_each() {
        let hc = [
            ("HC_TERMINAL_ID", "t4"),
            ("HC_SCOPE", "scope"),
            ("TERM_PROGRAM", "ghostty"),
        ];
        let id = ("HC_TERMINAL_ID", "t4");
        // An hc core started from a herdr pane: its terminals inherit the pane's HERDR_* (measured).
        let hc_in_herdr = [&HERDR[..], &hc[..]].concat();
        // herdr ≥0.9.2 inside an hc terminal: it says TERM_PROGRAM=herdr; an older one says nothing.
        let herdr_in_hc = [&HERDR[..], &[id, ("TERM_PROGRAM", "herdr")]].concat();
        let old_herdr_in_hc = [&HERDR[..], &[id]].concat();
        // tmux inside an hc terminal: the id is inherited, tmux answers only the DSR (measured).
        let tmux_in_hc = [
            id,
            ("TERM_PROGRAM", "tmux"),
            ("TMUX", "/tmp/tmux-501/default,1,0"),
        ];
        // `hc attach` inside a tmux pane: iota's tty is hc's terminal; tmux only watches it.
        let hc_in_tmux = [&hc[..], &[("TMUX", "/tmp/tmux-501/default,1,0")]].concat();
        let mut wrong = Vec::new();
        for (case, vars, answer, want) in [
            ("plain hc", &hc[..], Some(true), Some("herdr-core")),
            (
                "hc in herdr",
                &hc_in_herdr[..],
                Some(true),
                Some("herdr-core"),
            ),
            ("herdr in hc", &herdr_in_hc[..], Some(false), Some("herdr")),
            (
                "old herdr in hc",
                &old_herdr_in_hc[..],
                Some(false),
                Some("herdr"),
            ),
            ("tmux in hc", &tmux_in_hc[..], Some(false), None),
            (
                "hc in tmux",
                &hc_in_tmux[..],
                Some(true),
                Some("herdr-core"),
            ),
            ("no hc", &[][..], Some(true), None),
        ] {
            let host = detect(&probe(vars, answer));
            let got = host.as_ref().map(|h| h.name());
            if got != want {
                wrong.push(format!("{case}: {got:?}, want {want:?}"));
            }
            if let Some(c) = host.as_ref().and_then(|h| h.as_closer())
                && want == Some("herdr")
            {
                c.close().await; // the release goes to a socket nobody listens on
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Without an id the terminal is never asked; with one, its answer decides, and only when it
    /// cannot be asked does `TERM_PROGRAM` (the fallback rule).
    #[test]
    fn the_detector_asks_the_terminal_only_with_an_id() {
        let asked = Arc::new(Mutex::new(0));
        let counted = |vars: &[(&str, &str)]| {
            let n = Arc::clone(&asked);
            Probe {
                env: Env::fixed(vars),
                look_path: Box::new(|_| None),
                program_status: Box::new(move || {
                    *n.lock().unwrap() += 1;
                    Some(true)
                }),
            }
        };
        assert!(detect_hc(&counted(&[])).is_none());
        assert!(detect_hc(&counted(&[("HC_TERMINAL_ID", "")])).is_none());
        assert_eq!(*asked.lock().unwrap(), 0, "asked without an id");
        assert!(detect_hc(&counted(&[("HC_TERMINAL_ID", "t1")])).is_some());
        assert_eq!(*asked.lock().unwrap(), 1);

        let t1 = ("HC_TERMINAL_ID", "t1");
        assert!(detect_hc(&probe(&[t1, ("TERM_PROGRAM", "herdr")], Some(true))).is_some());
        assert!(detect_hc(&probe(&[t1, ("TERM_PROGRAM", "ghostty")], Some(false))).is_none());
        assert!(detect_hc(&probe(&[t1], None)).is_some());
        assert!(detect_hc(&probe(&[t1, ("TERM_PROGRAM", "ghostty")], None)).is_some());
        assert!(detect_hc(&probe(&[t1, ("TERM_PROGRAM", "herdr")], None)).is_none());
    }

    /// The probe's query, and the replies it reads: hc's (measured bytes, 0.1.21) and a DSR alone.
    #[test]
    fn the_probe_reads_hc_s_answer_and_not_the_dsr() {
        assert_eq!(PROBE_QUERY, b"\x1b]7501;?\x1b\\");
        assert!(answered(
            b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\\x1b[1;1R"
        ));
        assert!(!answered(b"\x1b[24;1R"));
        assert!(!answered(b""));
    }

    /// The id is withheld from every child (the module doc), and the model is told so.
    #[test]
    fn children_are_spawned_without_the_id() {
        let h = HcHost::with_writer("t4", Box::new(Recorder::default()));
        assert_eq!(
            h.as_child_env().expect("hc withholds").withheld(),
            vec!["HC_TERMINAL_ID".to_owned()]
        );
        let lines = h.as_environment().unwrap().environment();
        assert_eq!(
            lines.last().unwrap(),
            &("hc note".to_owned(), HC_CHILD_NOTE.to_owned())
        );
        let bytes: usize = lines.iter().map(|(k, v)| k.len() + 2 + v.len() + 1).sum();
        let share = crate::agents::harness::HARNESS_CAP / 10;
        assert!(
            bytes <= share,
            "hc's lines are {bytes} bytes, its share {share}"
        );
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
                ("hc note".to_owned(), HC_CHILD_NOTE.to_owned()),
            ]
        );
    }

    /// herdr-core claims `hc`, its own envelope, and no other host claims anything: under herdr's
    /// multiplexer, under cmux or in a plain terminal no message is ever parsed as an element.
    #[tokio::test]
    async fn only_hc_claims_an_element_and_it_is_hc() {
        use crate::host::{AnsiHost, Presenter};
        let ui = ScriptedUi::new(Vec::new());
        let ansi =
            || Some(Box::new(AnsiHost::new(Arc::clone(&ui) as Arc<dyn Ui>)) as Box<dyn Host>);
        let hc = HcHost::with_writer("t4", Box::new(Recorder::default()));
        assert_eq!(
            hc.as_message_envelope()
                .map(crate::host::MessageEnvelope::element),
            Some("hc")
        );
        let p = Presenter::over(Some(Box::new(hc)), ansi(), false);
        assert_eq!(p.message_element(), Some("hc"));

        let send: crate::host::herdr::SendFn = Arc::new(|_| Box::pin(async {}));
        let herdr = crate::host::herdr::HerdrHost::with_send(
            crate::host::herdr::Pane {
                id: "p1".to_owned(),
                ..Default::default()
            },
            1,
            send,
        );
        let exec: crate::host::cmux::ExecFn = Arc::new(|_| Box::pin(async {}));
        let cmux = crate::host::cmux::CmuxHost::with_exec("s1", exec, None);
        for other in [Box::new(herdr) as Box<dyn Host>, Box::new(cmux)] {
            let name = other.name().to_owned();
            assert!(other.as_message_envelope().is_none(), "{name}");
            let p = Presenter::over(Some(other), ansi(), false);
            assert_eq!(p.message_element(), None, "{name}");
        }
        assert_eq!(Presenter::over(None, ansi(), false).message_element(), None);
        assert_eq!(
            Presenter::with_hosts(Vec::new(), false).message_element(),
            None
        );
    }
}
