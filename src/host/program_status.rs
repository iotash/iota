//! The Program Status Protocol (OSC 7501, rev 0.4; docs/design/program-status.md): the terminal is told
//! the chat's state IN BAND — `ESC ] 7501 ; state=<word>:app=iota ST`, one whole record per report
//! (every report replaces the record; `state=clear` removes it) — and only after it said it reads it.
//!
//! **A terminal's capability, not a host's.** [`ProgramStatus`] is not a detected host: it is built at
//! assembly beside the detected one ([`for_terminal`]) and handed to the presenter on the terminal side,
//! in front of the ANSI host — `[ProgramStatus?, AnsiHost]` interactively, `[ProgramStatus?]` in a `-m`
//! run — so the first [`StateReporter`] takes the state and OSC 9;4 is not written where 7501 is, while
//! the ANSI host keeps the OSC 9 ping. It has three capabilities and nothing else: the state, the clear
//! on close, and the in-band writer. `<environment>` and the banner say nothing about it.
//!
//! **Only a terminal that answers.** The probe ([`probe_tty`], injected through
//! [`Probe::program_status`]) writes `ESC ] 7501 ; ? ST` chased by a DSR on the controlling terminal and
//! reads the reply (`crate::ui::runtime::osc::round_trip`, the OSC 11 detect's mechanism). The writer is
//! built only when the answer accepts the four words iota writes ([`accepts`]: its `states=` list, or
//! the protocol's default five when it lists none). A terminal that answers the DSR alone, garbage, or
//! nothing — no controlling terminal, no reply in 100 ms, a background process group — gets not one
//! report byte: an unknown OSC in a terminal that never said it reads it is at best ignored and at
//! worst garbage. For now the writer is built only where herdr-core was detected (the design's task 1);
//! any other terminal keeps OSC 9;4.
//!
//! **The writer.** Interactively the bytes go through the facade ([`InBand::write_through`], given by
//! `Presenter::show_turn_on`), so the UI thread writes them between draws and never inside one. A `-m`
//! run has no facade; there the host opens `/dev/tty` on its first report and writes it directly — the
//! reply on stdout may be a pipe, the record belongs to the terminal — and does nothing at all when
//! there is no controlling terminal. A failed write is a `tracing::debug!` and nothing else.
//!
//! **The states.** `Busy` is `working`, `NeedsInput` is `blocked` (no `kind`: the state carries no
//! reason), `Error` is `error`, `Idle` is `idle` — not `done`, whose "finished, not yet seen"
//! rendering is unknown.

use std::fs::File;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use crate::BoxFuture;
use crate::host::{Closer, Host, InBand, Probe, State, StateReporter};
use crate::sync::lock;
use crate::ui::facade::Ui;

/// The `app` key of every report: the program name.
pub(crate) const APP: &str = crate::app::NAME;

/// The probe: the OSC 7501 capability query (the DSR that ends the read is added by the round-trip).
pub(crate) const PROBE_QUERY: &[u8] = b"\x1b]7501;?\x1b\\";
/// What a terminal's answer to [`PROBE_QUERY`] starts with (`ESC ] 7501 ; ? : states=…`).
const PROBE_ANSWER: &[u8] = b"\x1b]7501;?";
/// The states iota writes: an answer that does not accept all four builds no writer (the design's §3).
const WRITTEN: [&str; 4] = ["working", "blocked", "idle", "error"];
/// The states a terminal accepts when its answer lists none (the protocol's base five).
const DEFAULT_STATES: [&str; 5] = ["idle", "working", "done", "blocked", "error"];

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
        Some(s) => format!("state={}:app={APP}", word(s)),
        None => "state=clear".to_owned(),
    };
    format!("\x1b]7501;{body}\x1b\\")
}

/// The terminal-side host to put in front of the ANSI host, or `None`: built only when herdr-core was
/// detected AND the terminal accepted the query (the module doc). The real probe remembers its answer,
/// so the terminal is asked once between this and the hc detector.
pub(crate) fn for_terminal(detected: Option<&dyn Host>, probe: &Probe) -> Option<Box<dyn Host>> {
    if detected.map(Host::name) != Some(super::hc::HC_NAME) {
        return None;
    }
    let answer = (probe.program_status)();
    tracing::debug!("program status: the terminal's answer is {answer:?}");
    (answer == Some(true)).then(|| Box::new(ProgramStatus::new()) as Box<dyn Host>)
}

/// The real [`Probe::program_status`]: one [`PROBE_QUERY`] round-trip on the controlling terminal.
/// `Some(true)` when the reply accepts the four states, `Some(false)` when it does not (the DSR alone,
/// garbage, a partial `states=`), `None` without a terminal or a reply in time.
pub(crate) fn probe_tty() -> Option<bool> {
    answer(crate::ui::runtime::osc::round_trip(PROBE_QUERY))
}

/// What a round-trip's outcome says (`probe_tty`, and the socket-pair test).
fn answer(reply: std::io::Result<Vec<u8>>) -> Option<bool> {
    reply.ok().map(|r| accepts(&r))
}

/// Whether a reply carries the answer to the query and that answer accepts every state iota writes: its
/// `states=` field, comma-separated, or — without one — the protocol's default five. The answer ends at
/// ST or BEL; fields are `:`-separated.
fn accepts(reply: &[u8]) -> bool {
    let Some(at) = reply
        .windows(PROBE_ANSWER.len())
        .position(|w| w == PROBE_ANSWER)
    else {
        return false;
    };
    let rest = &reply[at + PROBE_ANSWER.len()..];
    let end = rest
        .iter()
        .position(|&b| b == 0x1b || b == 0x07)
        .unwrap_or(rest.len());
    let body = String::from_utf8_lossy(&rest[..end]);
    let states: Vec<&str> = match body.split(':').find_map(|f| f.strip_prefix("states=")) {
        Some(list) => list.split(',').collect(),
        None => DEFAULT_STATES.to_vec(),
    };
    WRITTEN.iter().all(|w| states.contains(w))
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

/// The terminal that answered the query: the sink its reports are written to.
pub(crate) struct ProgramStatus {
    sink: Mutex<Sink>,
}

impl ProgramStatus {
    /// A writer to `/dev/tty` until it is given a facade.
    pub(crate) fn new() -> Self {
        Self {
            sink: Mutex::new(Sink::Tty),
        }
    }

    /// A writer to `w` until it is given a facade (tests).
    #[cfg(test)]
    pub(crate) fn with_writer(w: Box<dyn std::io::Write + Send>) -> Self {
        Self {
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
                    tracing::debug!("program status: no terminal to report to: {e}");
                    Sink::Off
                }
            };
        }
        match &mut *sink {
            Sink::Ui(ui) => ui.program_status(seq),
            Sink::Write(w) => {
                if let Err(e) = w.write_all(seq.as_bytes()).and_then(|()| w.flush()) {
                    tracing::debug!("program status: report not written: {e}");
                }
            }
            Sink::Tty | Sink::Off => {}
        }
    }
}

impl Host for ProgramStatus {
    fn name(&self) -> &'static str {
        "program-status"
    }

    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        Some(self)
    }

    fn as_closer(&self) -> Option<&dyn Closer> {
        Some(self)
    }

    fn as_in_band(&self) -> Option<&dyn InBand> {
        Some(self)
    }
}

impl StateReporter for ProgramStatus {
    fn set_state(&self, s: State) {
        self.write(&report(Some(s)));
    }
}

impl Closer for ProgramStatus {
    /// Removes the record (`state=clear`). The terminal also drops `working` and `blocked` when the
    /// program exits; the clear says so first, and is the whole of the clean-up when iota is not the
    /// terminal's program (it ran from a shell, which the terminal does not see return until it is in
    /// the foreground again).
    fn close(&self) -> BoxFuture<'_, ()> {
        self.write(&report(None));
        Box::pin(async {})
    }
}

impl InBand for ProgramStatus {
    fn write_through(&self, ui: Arc<dyn Ui>) {
        *lock(&self.sink) = Sink::Ui(ui);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{PROBE_QUERY, ProgramStatus, accepts, answer, report};
    use crate::host::{Host, State};
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

    /// The byte pin: each state's exact sequence, and the clear on close.
    #[tokio::test]
    async fn each_state_is_one_exact_report_and_close_clears() {
        let rec = Recorder::default();
        let h = ProgramStatus::with_writer(Box::new(rec.clone()));
        let r = h.as_state_reporter().expect("it reports state");
        for s in [State::Busy, State::NeedsInput, State::Error, State::Idle] {
            r.set_state(s);
        }
        h.as_closer().expect("it closes").close().await;
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
        assert!(h.as_environment().is_none(), "the model is told nothing");
        assert!(h.as_notifier().is_none(), "the terminal keeps the ping");
    }

    /// Given a facade, the host writes through it and no longer to its own writer.
    #[tokio::test]
    async fn the_facade_takes_over_the_writes() {
        let rec = Recorder::default();
        let h = ProgramStatus::with_writer(Box::new(rec.clone()));
        let ui = ScriptedUi::new(Vec::new());
        h.as_in_band()
            .expect("it is in band")
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

    /// The replies the probe reads: an answer that accepts the four states is one; the DSR alone, a
    /// partial `states=` list and nothing are not.
    #[test]
    fn the_reply_is_read_for_the_answer_and_its_states() {
        assert_eq!(PROBE_QUERY, b"\x1b]7501;?\x1b\\");
        let mut wrong = Vec::new();
        for (case, reply, want) in [
            (
                "hc 0.1.21 (measured)",
                &b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\\x1b[1;1R"[..],
                true,
            ),
            ("bare: the default five", b"\x1b]7501;?\x1b\\\x1b[1;1R", true),
            (
                "BEL-terminated",
                b"\x1b]7501;?:states=error,blocked,idle,working\x07\x1b[3;1R",
                true,
            ),
            (
                "no error",
                b"\x1b]7501;?:states=idle,working,done,blocked\x1b\\\x1b[1;1R",
                false,
            ),
            ("the DSR alone", b"\x1b[24;1R", false),
            ("empty", b"", false),
        ] {
            if accepts(reply) != want {
                wrong.push(format!("{case}: want {want}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The probe over a socket pair standing in for the terminal: the query and its DSR leave in one
    /// write; an answering end is `Some(true)`, a DSR-only end `Some(false)`, and a silent end costs the
    /// deadline, is `None`, and keeps the next byte for whoever reads the tty next.
    #[cfg(unix)]
    #[test]
    fn the_probe_over_a_socket_pair() {
        use crate::ui::runtime::osc::{REPLY_TIMEOUT, exchange};
        use std::io::{Read as _, Write as _};
        use std::time::{Duration, Instant};

        let pair = || {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            (
                std::fs::File::from(std::os::fd::OwnedFd::from(ours)),
                theirs,
            )
        };
        for (case, says, want) in [
            (
                "answers",
                &b"\x1b]7501;?:states=idle,working,done,blocked,error\x1b\\\x1b[1;1R"[..],
                Some(true),
            ),
            ("the DSR alone", b"\x1b[24;1R", Some(false)),
        ] {
            let (mut tty, mut terminal) = pair();
            let says = says.to_vec();
            let end = std::thread::spawn(move || {
                let mut asked = [0u8; 64];
                let n = terminal.read(&mut asked).unwrap();
                terminal.write_all(&says).unwrap();
                (asked[..n].to_vec(), terminal)
            });
            let got = answer(exchange(&mut tty, PROBE_QUERY, Duration::from_secs(5)));
            let (asked, _terminal) = end.join().unwrap();
            assert_eq!(asked, b"\x1b]7501;?\x1b\\\x1b[6n", "{case}");
            assert_eq!(got, want, "{case}");
        }

        let (mut tty, mut terminal) = pair();
        let started = Instant::now();
        assert_eq!(answer(exchange(&mut tty, PROBE_QUERY, REPLY_TIMEOUT)), None);
        assert!(started.elapsed() >= REPLY_TIMEOUT * 9 / 10);
        terminal.write_all(b"x").unwrap();
        let mut key = [0u8; 8];
        assert_eq!(tty.read(&mut key).unwrap(), 1);
        assert_eq!(key[0], b'x', "the next byte was taken");
    }
}
