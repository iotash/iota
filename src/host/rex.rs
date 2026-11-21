//! The rex host (docs/design/program-status.md §7). rex (Superlogical) is a client/server terminal
//! workspace: a server owns the sessions and their blocks, apps and the `rex` CLI attach to it. This
//! host is rex's IDENTITY and nothing else: `<environment>` gets `host: rex`, the block and the session,
//! so the model can `rex split` or `rex capture` beside itself. rex reads the Program Status Protocol off
//! the terminal — it authored it, and answers the OSC 7501 query with the same bytes as hc 0.1.21
//! (measured 2026-11-21) — so the state is `super::program_status`'s, built at assembly for any terminal
//! that accepted the query. `RexHost` has no [`StateReporter`](crate::host::StateReporter), so
//! `program_status::for_terminal` asks under it.
//!
//! **Detected from the environment.** `TERM_PROGRAM=rex` AND `REX_BLOCK` non-empty. `TERM_PROGRAM` is
//! what the terminal iota's tty belongs to says about itself: rex sets it in every block, and whatever
//! runs inside a block and starts terminals of its own sets its own (hc `ghostty`, tmux `tmux`, herdr
//! ≥0.9.2 `herdr`), so a `REX_BLOCK` inherited through one of them names an ancestor and is not taken.
//! `REX_BLOCK` alone is not enough for the same reason, and `TERM_PROGRAM=rex` alone is not a block the
//! CLI can address. The terminal is not asked: the environment already says which host this is, and the
//! question would be `for_terminal`'s anyway.
//!
//! **First in `super::DETECTORS`, before hc.** A rex server started from an hc terminal hands its
//! blocks the `HC_TERMINAL_ID` it was born with; rex answers hc's question as hc does, so the hc
//! detector would match and name a terminal that is not iota's. `TERM_PROGRAM=rex` says the innermost
//! terminal is rex's — rex wrote it last — so rex wins. The other way round, an hc terminal started from
//! a rex block, says `TERM_PROGRAM=ghostty` and is hc.
//!
//! **What it does not do.** No state, no clear, no in-band writer: the terminal's
//! (`super::program_status`). No [`MessageEnvelope`](crate::host::MessageEnvelope): rex's block API has
//! no envelope and no sender, `rex send` types raw into the pty. No
//! [`SessionReporter`](crate::host::SessionReporter): rex has no method for an agent's session. No
//! [`Notifier`](crate::host::Notifier): the ANSI host keeps OSC 9. No socket: `REX_SERVER` is the CLI's.
//! No [`ChildEnvironment`](crate::host::ChildEnvironment): the model's shell keeps `REX_BLOCK`, so `rex`
//! there targets iota's block.

use crate::host::{EnvironmentContributor, Host, Probe};

/// The host's name — the product's, as `TERM_PROGRAM` says it.
pub(crate) const REX_NAME: &str = "rex";
/// The block the chat runs in (`block:<uuid>`); rex sets it in every block.
pub(crate) const REX_BLOCK: &str = "REX_BLOCK";
/// The session the block belongs to (`session:<uuid>`).
pub(crate) const REX_SESSION: &str = "REX_SESSION";
/// `rex` in a rex block — the innermost terminal's own word (the module doc).
const TERM_PROGRAM: &str = "TERM_PROGRAM";

/// The rex host: the block and its session, as rex reports them.
pub struct RexHost {
    block: String,
    session: Option<String>,
}

impl RexHost {
    /// The host of block `block` in `session`.
    pub(crate) fn new(block: String, session: Option<String>) -> Self {
        Self { block, session }
    }
}

impl Host for RexHost {
    fn name(&self) -> &'static str {
        REX_NAME
    }

    fn as_environment(&self) -> Option<&dyn EnvironmentContributor> {
        Some(self)
    }
}

impl EnvironmentContributor for RexHost {
    /// `host: rex`, the block and the session — the targets `rex` takes, raw (`block:<uuid>`).
    fn environment(&self) -> Vec<(String, String)> {
        let mut lines = vec![
            ("host".to_owned(), REX_NAME.to_owned()),
            ("rex block".to_owned(), self.block.clone()),
        ];
        if let Some(s) = &self.session {
            lines.push(("rex session".to_owned(), s.clone()));
        }
        lines
    }
}

/// `Some(RexHost)` when `TERM_PROGRAM` is `rex` and `REX_BLOCK` is non-empty (the module doc).
pub(crate) fn detect_rex(probe: &Probe) -> Option<Box<dyn Host>> {
    if probe.env.var(TERM_PROGRAM).as_deref() != Some(REX_NAME) {
        return None;
    }
    let block = probe.env.var(REX_BLOCK)?;
    Some(Box::new(RexHost::new(block, probe.env.var(REX_SESSION))))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::detect_rex;
    use crate::app::env::Env;
    use crate::host::program_status::for_terminal;
    use crate::host::{Presenter, Probe, detect};

    /// A probe over `vars` whose terminal gives `answer` to the OSC 7501 query.
    fn probe(vars: &[(&str, &str)], answer: Option<bool>) -> Probe {
        Probe {
            env: Env::fixed(vars),
            look_path: Box::new(|_| None),
            program_status: Box::new(move || answer),
        }
    }

    const BLOCK: &str = "block:5f0c1e2a-0000-4000-8000-000000000001";
    const SESSION: &str = "session:5f0c1e2a-0000-4000-8000-000000000002";

    /// What a rex block carries (measured 2026-11-21).
    fn rex() -> Vec<(&'static str, &'static str)> {
        vec![
            ("TERM_PROGRAM", "rex"),
            ("TERM_PROGRAM_VERSION", "0.1.0-preview.21"),
            ("TERM", "xterm-256color"),
            ("REX_BLOCK", BLOCK),
            ("REX_SESSION", SESSION),
            ("REX_SERVER", "unix:///nonexistent/rex/server.sock"),
            ("REX_BIN_DIR", "/nonexistent/Rex Server.app/Contents/MacOS"),
        ]
    }

    /// `TERM_PROGRAM=rex` and a block is rex; either alone is not; the terminal is never asked.
    #[test]
    fn rex_is_its_term_program_and_a_block() {
        let never = || Probe {
            program_status: Box::new(|| panic!("the rex detector asked the terminal")),
            ..probe(&[], None)
        };
        let with = |vars: &[(&str, &str)]| Probe {
            env: Env::fixed(vars),
            ..never()
        };
        assert!(detect_rex(&with(&rex())).is_some_and(|h| h.name() == "rex"));
        let tp = ("TERM_PROGRAM", "rex");
        assert!(detect_rex(&with(&[tp])).is_none(), "no block");
        assert!(detect_rex(&with(&[tp, ("REX_BLOCK", "")])).is_none());
        assert!(detect_rex(&with(&[("REX_BLOCK", BLOCK)])).is_none());
        let ghostty = [("TERM_PROGRAM", "ghostty"), ("REX_BLOCK", BLOCK)];
        assert!(detect_rex(&with(&ghostty)).is_none());
        let herdr = [("TERM_PROGRAM", "herdr"), ("REX_BLOCK", BLOCK)];
        assert!(detect_rex(&with(&herdr)).is_none(), "herdr inside rex");
    }

    /// rex's `<environment>` is the host, the block and the session, raw, and nothing else; no child
    /// loses a variable (the model's `rex` targets iota's block); no capability but the environment.
    #[test]
    fn rex_names_itself_the_block_and_the_session_and_nothing_else() {
        let h = detect_rex(&probe(&rex(), None)).unwrap();
        assert_eq!(
            h.as_environment().unwrap().environment(),
            vec![
                ("host".to_owned(), "rex".to_owned()),
                ("rex block".to_owned(), BLOCK.to_owned()),
                ("rex session".to_owned(), SESSION.to_owned()),
            ]
        );
        assert!(
            h.as_state_reporter().is_none(),
            "rex's state is the terminal's"
        );
        assert!(h.as_notifier().is_none());
        assert!(h.as_session_reporter().is_none());
        assert!(h.as_message_envelope().is_none());
        assert!(h.as_child_env().is_none());
        assert!(h.as_closer().is_none());
        assert!(h.as_background().is_none());
        assert!(h.as_in_band().is_none());
        let p = Presenter::over(Some(h), Vec::new(), false);
        assert_eq!(p.message_element(), None);
    }

    /// The ways rex meets the others, through `detect` so the order of `DETECTORS` is what is tested,
    /// each yielding one host and the writer wherever the terminal accepted (rex has no state channel,
    /// so the terminal is asked under it).
    ///
    /// **The collision:** a rex server started from an hc terminal gives its blocks `HC_TERMINAL_ID`,
    /// and rex answers the 7501 query as hc does (measured), so hc's detector would match. rex wins: it
    /// is first in `DETECTORS`, and `TERM_PROGRAM=rex` is the innermost terminal's word — rex set it
    /// last — while `HC_TERMINAL_ID` is an ancestor's. An hc terminal started from a rex block says
    /// `TERM_PROGRAM=ghostty` and is hc; tmux and herdr inside rex say their own and are not rex.
    #[test]
    fn the_detection_matrix_yields_one_host_each() {
        let rex = rex();
        let rex_in_hc = [&rex[..], &[("HC_TERMINAL_ID", "t4"), ("HC_SCOPE", "scope")]].concat();
        let over = |base: &[(&'static str, &'static str)], tp: &'static str| {
            let mut v: Vec<_> = base
                .iter()
                .copied()
                .filter(|(k, _)| *k != "TERM_PROGRAM")
                .collect();
            v.push(("TERM_PROGRAM", tp));
            v
        };
        let hc_in_rex = over(&[&rex[..], &[("HC_TERMINAL_ID", "t4")]].concat(), "ghostty");
        let tmux_in_rex = [
            &over(&rex, "tmux")[..],
            &[("TMUX", "/tmp/tmux-501/default,1,0")],
        ]
        .concat();
        let mut wrong = Vec::new();
        // (case, environment, the terminal's answer, the host, the writer)
        for (case, vars, answer, want, writer) in [
            ("plain rex", &rex[..], Some(true), Some("rex"), true),
            ("rex, not asked", &rex[..], None, Some("rex"), false),
            ("rex in hc", &rex_in_hc[..], Some(true), Some("rex"), true),
            (
                "hc in rex",
                &hc_in_rex[..],
                Some(true),
                Some("herdr-core"),
                true,
            ),
            ("tmux in rex", &tmux_in_rex[..], Some(false), None, false),
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
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The `<environment>` the presenter gathers in rex is rex's three lines — nothing from hc, whose
    /// id the block inherited.
    #[test]
    fn rex_in_hc_tells_the_model_rex_alone() {
        let vars = [&rex()[..], &[("HC_TERMINAL_ID", "t4")]].concat();
        let p = Presenter::over(detect(&probe(&vars, Some(true))), Vec::new(), false);
        let keys: Vec<String> = p.environment().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["host", "rex block", "rex session"]);
    }
}
