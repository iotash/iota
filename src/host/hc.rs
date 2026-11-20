//! The herdr-core host (docs/design/herdr-core-host.md). herdr-core (`hc`) is the terminal core behind
//! herdr's desktop app — a new product, not a version of the herdr multiplexer (`super::herdr`); both
//! are live, and iota supports both. This host is hc's IDENTITY: the terminal's id, the `hc` message
//! envelope and the `<environment>` lines. hc has no socket method for a program's state; it reads the
//! Program Status Protocol off the terminal, which `super::program_status` writes — a terminal-side host
//! built at assembly beside this one, for any terminal that accepted the query, hc's or not
//! (docs/design/program-status.md).
//!
//! **Detected by asking the terminal.** Without `HC_TERMINAL_ID` it is not an hc terminal and the
//! terminal is not asked. With it, the probe ([`Probe::program_status`], the real one
//! `program_status::probe_tty`) asks the terminal whether it reads the protocol: hc answers (in well
//! under a millisecond, measured), while whatever stands between iota and hc — a tmux, a herdr pane —
//! answers only the DSR, and the host is then not hc. The environment cannot say this: a variable names
//! an ancestor, not what is across the tty, and hc's own `TERM_PROGRAM=ghostty` is a value it is already
//! asked to change. Only when the probe cannot run (no controlling terminal, no answer in 100 ms, a
//! background process group) does the environment decide which host this is: `TERM_PROGRAM` not
//! `herdr`. That rule gives the identity only — nothing is written to a terminal that was not asked. The
//! detector sits BEFORE herdr's (`super::DETECTORS`): an hc terminal started from a herdr pane inherits
//! the pane's `HERDR_*`, and reporting to that pane would be the wrong surface.
//!
//! **Children get the id.** A nested `iota` — the chat's shell tool or an MCP server ran it — sees
//! `HC_TERMINAL_ID` and detects hc, but it is in the chat's session (`IOTA_STATUS_SESSION`,
//! `super::program_status`): the terminal is not asked, so the identity comes from the environment rule
//! and nothing is written over the chat's record. The model's shell keeps its terminal id, so `hc run`
//! finds its parent.
//!
//! **What it does not do.** No state, no clear, no in-band writer: those are the terminal's
//! (`super::program_status`). No [`Notifier`](crate::host::Notifier): the ANSI host keeps OSC 9, and hc
//! makes no event of it. No [`BackgroundReporter`](crate::host::BackgroundReporter): hc answers OSC 11
//! itself. No [`SessionReporter`](crate::host::SessionReporter): the protocol has no key for a session.

use crate::host::{EnvironmentContributor, Host, MessageEnvelope, Probe};

/// The host's name — the product's, as `hc --skill` names it.
pub(crate) const HC_NAME: &str = "herdr-core";
/// The terminal the chat runs in (`t4`); hc sets it in every terminal it starts.
pub(crate) const HC_TERMINAL_ID: &str = "HC_TERMINAL_ID";
/// `herdr` inside a herdr pane (herdr ≥0.9.2): the detector's rule when the terminal cannot be asked.
pub(crate) const TERM_PROGRAM: &str = "TERM_PROGRAM";

/// The herdr-core host: the terminal's id.
pub struct HcHost {
    terminal: String,
}

impl HcHost {
    /// The host of terminal `terminal`.
    pub(crate) fn new(terminal: String) -> Self {
        Self { terminal }
    }
}

impl Host for HcHost {
    fn name(&self) -> &'static str {
        HC_NAME
    }

    fn as_environment(&self) -> Option<&dyn EnvironmentContributor> {
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

impl EnvironmentContributor for HcHost {
    /// `host: herdr-core` and the terminal — the address `hc get`, `hc read` and `hc label` take.
    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("host".to_owned(), HC_NAME.to_owned()),
            ("hc terminal".to_owned(), self.terminal.clone()),
        ]
    }
}

/// `Some(HcHost)` when `HC_TERMINAL_ID` is non-empty and the terminal answers the OSC 7501 query — or,
/// when it cannot be asked, `TERM_PROGRAM` is not `herdr` (the module doc). Without the id the terminal
/// is not asked.
pub(crate) fn detect_hc(probe: &Probe) -> Option<Box<dyn Host>> {
    let terminal = probe.env.var(HC_TERMINAL_ID)?;
    let hc = match (probe.program_status)() {
        Some(answered) => answered,
        None => probe.env.var(TERM_PROGRAM).as_deref() != Some("herdr"),
    };
    hc.then(|| Box::new(HcHost::new(terminal)) as Box<dyn Host>)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{HcHost, detect_hc};
    use crate::app::env::Env;
    use crate::host::program_status::for_terminal;
    use crate::host::{Host, Probe, detect};
    use crate::testing::ScriptedUi;
    use crate::ui::facade::Ui;
    use std::sync::{Arc, Mutex};

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

    /// The ways hc meets the others (docs/design/herdr-core-host.md §2), each yielding exactly one host
    /// — the right one, or none — and the writer (`program_status::for_terminal`) wherever the terminal
    /// accepted and no detected host reports state itself: a terminal that could not be asked is hc by
    /// the environment rule and gets no report byte (docs/design/program-status.md §2). Run through `detect`, so the
    /// order of `DETECTORS` is what is tested; the terminal's answer to the probe is what each nesting
    /// gives (measured for hc and tmux; herdr's is assumed, the design doc says why).
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
        let hc_ = Some("herdr-core");
        let mut wrong = Vec::new();
        // (case, environment, the terminal's answer, the host, the writer)
        for (case, vars, answer, want, writer) in [
            ("plain hc", &hc[..], Some(true), hc_, true),
            ("hc, not asked", &hc[..], None, hc_, false),
            ("hc in herdr", &hc_in_herdr[..], Some(true), hc_, true),
            (
                "herdr in hc",
                &herdr_in_hc[..],
                Some(false),
                Some("herdr"),
                false,
            ),
            (
                "old herdr in hc",
                &old_herdr_in_hc[..],
                Some(false),
                Some("herdr"),
                false,
            ),
            ("tmux in hc", &tmux_in_hc[..], Some(false), None, false),
            ("hc in tmux", &hc_in_tmux[..], Some(true), hc_, true),
            ("no hc, answering", &[][..], Some(true), None, true),
            ("no hc, mute", &[][..], Some(false), None, false),
        ] {
            let probe = probe(vars, answer);
            let host = detect(&probe);
            let got = host.as_ref().map(|h| h.name());
            if got != want {
                wrong.push(format!("{case}: {got:?}, want {want:?}"));
            }
            let wrote = for_terminal(host.as_deref(), &probe).is_some();
            if wrote != writer {
                wrong.push(format!("{case}: writer {wrote}, want {writer}"));
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

    /// hc's `<environment>` is the host and the terminal, nothing else, and no child loses a variable:
    /// the id reaches the model's shell (the module doc).
    #[test]
    fn hc_names_itself_and_the_terminal_and_nothing_else() {
        let h = HcHost::new("t4".to_owned());
        assert!(h.as_child_env().is_none(), "hc withholds a variable");
        assert_eq!(
            h.as_environment().unwrap().environment(),
            vec![
                ("host".to_owned(), "herdr-core".to_owned()),
                ("hc terminal".to_owned(), "t4".to_owned()),
            ]
        );
        assert!(crate::host::child_env([&h as &dyn Host]).names().is_empty());
    }

    /// herdr-core claims `hc`, its own envelope, and no other host claims anything: under herdr's
    /// multiplexer, under cmux or in a plain terminal no message is ever parsed as an element.
    #[tokio::test]
    async fn only_hc_claims_an_element_and_it_is_hc() {
        use crate::host::{AnsiHost, Presenter};
        let ui = ScriptedUi::new(Vec::new());
        let ansi =
            || vec![Box::new(AnsiHost::new(Arc::clone(&ui) as Arc<dyn Ui>)) as Box<dyn Host>];
        let hc = HcHost::new("t4".to_owned());
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
