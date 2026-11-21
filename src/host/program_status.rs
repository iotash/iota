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
//! worst garbage. Any terminal that answers gets it, except under a detected host with a state channel of
//! its own (herdr's socket, cmux's CLI): that host takes the state and the terminal is never asked — a
//! 7501 terminal started from a herdr pane or a cmux surface reports to the inherited pane (accepted).
//! The answer is the only gate: no environment rule writes without it.
//!
//! **One reporter per controlling terminal.** Every run gives its children [`STATUS_SESSION`] = its
//! session id ([`session`], `getsid(0)`), and a run whose own session id equals the one it inherited is
//! on the terminal of the run that started it — an `iota -m` the chat's shell tool or an MCP server ran:
//! [`probe_tty`] returns `None` before a byte is written, so it neither asks nor reports, and the
//! parent's record stays. A worker in a terminal of its own (a new pane is a new session), or a run with
//! no inherited id, asks as usual. A session, not a flag: a controlling terminal belongs to one.
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
use crate::app::env::Env;
use crate::host::{Closer, Host, InBand, Probe, State, StateReporter};
use crate::sync::lock;
use crate::ui::facade::Ui;

/// The variable every child is given: the giving run's [`session`] (the module doc).
pub(crate) const STATUS_SESSION: &str = "IOTA_STATUS_SESSION";

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

/// The terminal-side host to put in front of the ANSI host, or `None` (the design's §2 table): a detected
/// host with a state channel of its own (herdr, cmux) takes the state and the terminal is not asked;
/// under any other detected host, or none, the writer is built when the terminal accepted the query.
/// The real probe remembers its answer, so the terminal is asked once between this and the hc detector.
pub(crate) fn for_terminal(detected: Option<&dyn Host>, probe: &Probe) -> Option<Box<dyn Host>> {
    if let Some(h) = detected.filter(|h| h.as_state_reporter().is_some()) {
        tracing::debug!(
            "program status: {} reports state, the terminal is not asked",
            h.name()
        );
        return None;
    }
    let answer = (probe.program_status)();
    tracing::debug!("program status: the terminal's answer is {answer:?}");
    (answer == Some(true)).then(|| Box::new(ProgramStatus::new()) as Box<dyn Host>)
}

/// `ask` asked at most once: its first answer is every later call's (`cmd::host_probe`'s probe, which
/// the hc detector and [`for_terminal`] share).
pub(crate) fn remembered(
    ask: impl Fn() -> Option<bool> + Send + Sync + 'static,
) -> crate::host::ProgramStatusFn {
    let answer = std::sync::OnceLock::new();
    Box::new(move || *answer.get_or_init(&ask))
}

/// This process's session id (`getsid(0)`), given to every child as [`STATUS_SESSION`]. `None` where
/// there is none to read (Windows, which has no probe to guard).
pub(crate) fn session() -> Option<String> {
    #[cfg(unix)]
    {
        nix::unistd::getsid(None).ok().map(|sid| sid.to_string())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The real [`Probe::program_status`]: one [`PROBE_QUERY`] round-trip on the controlling terminal —
/// unless `env` names this run's own [`session`] (the module doc). `Some(true)` when the reply accepts
/// the four states, `Some(false)` when it does not (the DSR alone, garbage, a partial `states=`), `None`
/// in the parent's session, without a terminal or without a reply in time.
pub(crate) fn probe_tty(env: &Env) -> Option<bool> {
    unless_nested(env, session().as_deref(), || {
        answer(crate::ui::runtime::osc::round_trip(PROBE_QUERY))
    })
}

/// `ask`'s answer, or `None` without asking when `env`'s [`STATUS_SESSION`] is `own`.
fn unless_nested(env: &Env, own: Option<&str>, ask: impl FnOnce() -> Option<bool>) -> Option<bool> {
    if own.is_some() && env.var(STATUS_SESSION).as_deref() == own {
        tracing::debug!(
            "program status: in the session of the run that started this one, not asked"
        );
        return None;
    }
    ask()
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

    use super::{
        PROBE_QUERY, ProgramStatus, STATUS_SESSION, accepts, for_terminal, remembered, report,
        unless_nested,
    };
    use crate::app::env::Env;
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
        use super::answer;
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

    /// A run whose inherited [`STATUS_SESSION`] is its own session does not ask the terminal (and so
    /// writes nothing); no inherited id, an empty one or another session's asks as before.
    #[test]
    fn a_run_in_its_parents_session_does_not_ask() {
        let asked = std::cell::Cell::new(0);
        let ask = || {
            asked.set(asked.get() + 1);
            Some(true)
        };
        let own = || Some("42");
        let nested = Env::fixed(&[(STATUS_SESSION, "42")]);
        assert_eq!(unless_nested(&nested, own(), ask), None);
        assert_eq!(asked.get(), 0, "asked in the parent's session");
        for env in [
            Env::default(),
            Env::fixed(&[(STATUS_SESSION, "")]),
            Env::fixed(&[(STATUS_SESSION, "7")]),
        ] {
            assert_eq!(unless_nested(&env, own(), ask), Some(true), "{env:?}");
        }
        // Without a session of its own (no getsid), a run asks whatever it inherited.
        assert_eq!(unless_nested(&nested, None, ask), Some(true));
        assert_eq!(asked.get(), 4);
        #[cfg(unix)]
        assert_eq!(
            super::session(),
            Some(nix::unistd::getsid(None).unwrap().to_string())
        );
    }

    /// The design's §2 table through the assembly (`detect`, then [`for_terminal`], over one
    /// [`remembered`] probe, as `cmd::host_probe` builds it): under herdr and cmux the terminal is never
    /// asked and no writer is built, whatever it would say; under hc and in a bare terminal it is asked
    /// once between the detector and the assembly, and only an accepting answer builds the writer.
    #[tokio::test]
    async fn the_terminal_is_asked_only_when_no_host_reports_state() {
        use crate::host::detect;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let herdr = [
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p2"),
            ("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock"),
            ("HERDR_BIN_PATH", "/nonexistent/herdr"),
        ];
        let cmux = [("CMUX_SURFACE_ID", "s1")];
        let hc = [("HC_TERMINAL_ID", "t4")];
        let mut wrong = Vec::new();
        // (case, environment, the terminal's answer, the host, times asked, the writer)
        for (case, vars, says, host, asks, writer) in [
            ("herdr", &herdr[..], Some(true), Some("herdr"), 0, false),
            ("cmux", &cmux[..], Some(true), Some("cmux"), 0, false),
            ("hc", &hc[..], Some(true), Some("herdr-core"), 1, true),
            ("hc, not asked", &hc[..], None, Some("herdr-core"), 1, false),
            ("bare, answering", &[][..], Some(true), None, 1, true),
            ("bare, mute", &[][..], Some(false), None, 1, false),
            ("bare, nothing", &[][..], None, None, 1, false),
        ] {
            let asked = Arc::new(AtomicUsize::new(0));
            let n = Arc::clone(&asked);
            let probe = crate::host::Probe {
                env: Env::fixed(vars),
                look_path: Box::new(|name| (name == "cmux").then(|| "/nonexistent/cmux".into())),
                program_status: remembered(move || {
                    n.fetch_add(1, Ordering::SeqCst);
                    says
                }),
            };
            let detected = detect(&probe);
            let got = detected.as_ref().map(|h| h.name());
            let built = for_terminal(detected.as_deref(), &probe).is_some();
            let times = asked.load(Ordering::SeqCst);
            if (got, times, built) != (host, asks, writer) {
                wrong.push(format!(
                    "{case}: {got:?} asked {times} writer {built}, want {host:?} {asks} {writer}"
                ));
            }
            if let Some(c) = detected.as_ref().and_then(|h| h.as_closer())
                && host == Some("herdr")
            {
                c.close().await; // the release goes to a socket nobody listens on
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Where the child of [`a_real_terminal_that_answers_nothing_gets_no_report`] says it is done.
    #[cfg(unix)]
    const REAL_TTY_OUT: &str = "IOTA_TEST_PROGRAM_STATUS_OUT";

    /// The child half: this test binary run again under `script(1)`, assembling as a `-m` run in a bare
    /// terminal does — the real probe, `detect`, [`for_terminal`] — and reporting a state and the clear
    /// through whatever that built. Does nothing unless it is that child.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "the child of a_real_terminal_that_answers_nothing_gets_no_report"]
    async fn real_tty_child() {
        let Some(out) = std::env::var_os(REAL_TTY_OUT) else {
            return;
        };
        let probe = crate::cmd::host_probe(&Env::default());
        let detected = crate::host::detect(&probe);
        let built = for_terminal(detected.as_deref(), &probe);
        if let Some(h) = &built {
            h.as_state_reporter().unwrap().set_state(State::Busy);
            h.as_closer().unwrap().close().await;
        }
        crate::ui::testutil::report(
            std::path::Path::new(&out),
            &format!(
                "answer {:?} writer {}",
                (probe.program_status)(),
                built.is_some()
            ),
        );
    }

    /// A real `/dev/tty` that answers nothing (`script(1)`'s pty): the run asks once, the answer is
    /// `None`, no writer is built, and the typescript — every byte the run wrote to its terminal — holds
    /// the query once and no `state=`.
    #[cfg(unix)]
    #[test]
    fn a_real_terminal_that_answers_nothing_gets_no_report() {
        use crate::ui::testutil::{await_report, under_script_to};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("said");
        let typescript = dir.path().join("typescript");
        let mut script = under_script_to(
            "host::program_status::tests::real_tty_child",
            &[(REAL_TTY_OUT, out.as_os_str())],
            &typescript,
        );
        let said = await_report(&out, Duration::from_secs(20));
        // `script` writes its typescript as it goes and exits when the child does: wait for that
        // rather than kill it, which would lose what it has not written yet.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while script.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = script.kill();
        let _ = script.wait();
        assert_eq!(
            said.as_deref(),
            Some("answer None writer false"),
            "the child never said, or said otherwise"
        );
        let bytes = std::fs::read(&typescript).unwrap();
        let count = |what: &[u8]| bytes.windows(what.len()).filter(|w| *w == what).count();
        let shown = String::from_utf8_lossy(&bytes);
        assert_eq!(count(PROBE_QUERY), 1, "the query, once: {shown:?}");
        assert_eq!(count(b"state="), 0, "a report was written: {shown:?}");
    }
}
