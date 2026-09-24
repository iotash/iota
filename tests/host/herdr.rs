//! The herdr host end to end over the mock socket: detection from an injected environment, the
//! request sequence the presenter's calls become, the session report, an unreachable socket, and
//! what the host tells the model. The interactive and headless runs over the same mock are in
//! `tests/repl/host.rs` and `tests/cmd/cli.rs`.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use iota::app::env::Env;
use iota::host::{Presenter, Probe, State};

use crate::common::HerdrMock;

/// A probe over exactly `vars`, with nothing on `PATH` (so cmux can never be detected).
fn probe(vars: &[(String, String)]) -> Probe {
    let borrowed: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Probe {
        env: Env::fixed(&borrowed),
        look_path: Box::new(|_| None),
    }
}

/// `vars` without the variable `name`.
fn without(vars: &[(String, String)], name: &str) -> Vec<(String, String)> {
    vars.iter().filter(|(k, _)| k != name).cloned().collect()
}

/// (a) With `HERDR_ENV` unset, or any of the three variables missing, there is no herdr host — and
/// nothing reaches the socket, whatever the presenter is told.
#[tokio::test]
async fn no_herdr_variables_means_no_host_and_no_request() {
    let mock = HerdrMock::start();
    let full = mock.env("w1:p2");
    for missing in ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_SOCKET_PATH"] {
        let p = Presenter::new(&probe(&without(&full, missing)), None, true);
        assert!(
            p.host_names().is_empty(),
            "detected a host without {missing}: {:?}",
            p.host_names()
        );
        p.set_state(State::Busy);
        p.set_session("s-1", Path::new("/tmp/s-1"));
        p.set_state(State::Idle);
        p.close().await;
    }
    let mut off = without(&full, "HERDR_ENV");
    off.push(("HERDR_ENV".to_owned(), "0".to_owned()));
    let p = Presenter::new(&probe(&off), None, true);
    assert!(p.host_names().is_empty(), "HERDR_ENV=0 detected a host");
    p.set_state(State::Busy);
    p.close().await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        mock.requests().is_empty(),
        "the socket was written to: {:?}",
        mock.requests()
    );
}

/// The presenter's calls become the socket's requests, in order, one connection each: `working`,
/// `blocked` (no message — herdr words its own notification), `working` again, the session, `idle`
/// for both `Idle` and `Error`, and the release on close. Every request names the pane, `iota` as
/// source and agent, an id `iota:<seq>`, and a strictly increasing `seq`.
#[tokio::test]
async fn the_presenter_reports_over_the_socket() {
    let mock = HerdrMock::start();
    let p = Presenter::new(&probe(&mock.env("w1:p2")), None, true);
    assert_eq!(p.host_names(), vec!["herdr"]);

    p.set_state(State::Busy);
    p.set_state(State::NeedsInput);
    p.set_state(State::Busy);
    p.set_session("s-1", Path::new("/tmp/sessions/s-1"));
    p.set_state(State::Idle);
    p.set_state(State::Error);
    p.close().await;

    assert_eq!(
        mock.summaries(),
        [
            "report_agent working",
            "report_agent blocked",
            "report_agent working",
            "report_agent_session s-1",
            "report_agent idle",
            "report_agent idle",
            "release_agent",
        ]
    );
    let requests = mock.requests();
    let seqs: Vec<u64> = requests.iter().map(|r| r.seq().expect("a seq")).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is not strictly increasing: {seqs:?}"
    );
    for r in &requests {
        assert_eq!(r.param("pane_id"), "w1:p2", "{r:?}");
        assert_eq!(r.param("source"), "iota", "{r:?}");
        assert_eq!(r.param("agent"), "iota", "{r:?}");
        assert_eq!(r.id, format!("iota:{}", r.seq().expect("a seq")), "{r:?}");
        assert!(
            r.params.get("message").is_none(),
            "a message was sent: {r:?}"
        );
    }
    let session = &requests[3];
    assert_eq!(session.param("agent_session_path"), "/tmp/sessions/s-1");
    assert!(session.params.get("state").is_none(), "{session:?}");
}

/// The first state always goes out — an `idle` at start-up is what lists the pane — a repeat is
/// deduplicated by the presenter before it reaches the socket, and a presenter whose herdr host
/// was never told anything still releases the pane on close.
#[tokio::test]
async fn the_first_state_goes_out_and_a_repeat_does_not() {
    let mock = HerdrMock::start();
    let p = Presenter::new(&probe(&mock.env("w1:p2")), None, true);
    p.set_state(State::Idle); // the first report: sent
    p.set_state(State::Idle); // a repeat: dropped
    p.set_state(State::Busy);
    p.set_state(State::Busy);
    p.close().await;
    assert_eq!(
        mock.summaries(),
        ["report_agent idle", "report_agent working", "release_agent"]
    );

    let p = Presenter::new(&probe(&mock.env("w1:p3")), None, true);
    p.close().await;
    assert_eq!(
        mock.summaries().last().map(String::as_str),
        Some("release_agent")
    );
    assert_eq!(
        mock.requests().last().expect("a request").param("pane_id"),
        "w1:p3"
    );
}

/// A background job started while the chat is idle puts the pane at `working`; the job ending
/// alone does not bring back `idle` — its notice is still owed — and the notice's turn does; a job
/// that starts and ends inside a turn sends nothing of its own.
#[tokio::test]
async fn a_running_job_keeps_the_pane_working() {
    let mock = HerdrMock::start();
    let p = Presenter::new(&probe(&mock.env("w1:p2")), None, true);
    p.set_state(State::Idle);
    p.set_jobs(1); // a job starts while the chat is idle
    p.set_jobs(0); // it ends; the notice is on the way
    p.set_state(State::Idle); // the loop wakes for it
    p.notice_taken();
    p.set_state(State::Busy); // the notice's turn
    p.set_jobs(1);
    p.set_jobs(0);
    p.notice_taken(); // drained at a round boundary
    p.set_state(State::Idle);
    p.close().await;
    assert_eq!(
        mock.summaries(),
        [
            "report_agent idle",
            "report_agent working",
            "report_agent idle",
            "release_agent"
        ]
    );
}

/// (e) A socket path nobody listens on: the host is still detected (the variables say herdr), every
/// request fails at connect, and the run completes at once — no panic, no wait. The bound is well
/// under the per-request timeout times the requests made, so a stall would show.
#[tokio::test]
async fn an_unreachable_socket_never_blocks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let absent = dir.path().join("absent.sock");
    let mut vars = HerdrMock::start().env("w1:p2");
    vars.retain(|(k, _)| k != "HERDR_SOCKET_PATH");
    vars.push((
        "HERDR_SOCKET_PATH".to_owned(),
        absent.to_string_lossy().into_owned(),
    ));
    let p = Presenter::new(&probe(&vars), None, true);
    assert_eq!(p.host_names(), vec!["herdr"]);

    let started = Instant::now();
    p.set_state(State::Busy);
    p.set_state(State::NeedsInput);
    p.set_session("s-1", Path::new("/tmp/s-1"));
    p.set_state(State::Idle);
    p.close().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "an unreachable socket held the run for {elapsed:?}"
    );
    assert!(!absent.exists(), "the host created the socket path");
}

/// What the model is told: `host: herdr`, the pane, and the workspace and tab when injected — in
/// that order — and nothing when the pane has no workspace or tab.
#[tokio::test]
async fn the_environment_names_the_host_and_its_ids() {
    let mock = HerdrMock::start();
    let pairs = |kv: &[(&str, &str)]| -> Vec<(String, String)> {
        kv.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let p = Presenter::new(&probe(&mock.env("w1:p2")), None, true);
    assert_eq!(
        p.environment(),
        pairs(&[
            ("host", "herdr"),
            ("herdr pane", "w1:p2"),
            ("herdr workspace", "w1"),
            ("herdr tab", "w1:t1"),
        ])
    );
    p.close().await;

    let bare = without(
        &without(&mock.env("w1:p2"), "HERDR_WORKSPACE_ID"),
        "HERDR_TAB_ID",
    );
    let p = Presenter::new(&probe(&bare), None, true);
    assert_eq!(
        p.environment(),
        pairs(&[("host", "herdr"), ("herdr pane", "w1:p2")])
    );
    p.close().await;
    assert_eq!(
        mock.summaries(),
        ["release_agent", "release_agent"],
        "asking for the environment reported something"
    );
}
