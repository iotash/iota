#![allow(clippy::unwrap_used, clippy::expect_used)]
//! [`HangupWatch`] on a real `/dev/tty`: the one device where `poll(2)` lies on macOS (it answered
//! `POLLNVAL` for a live terminal, and the loop exited at startup whenever stdin was not one). A
//! socket pair or a pty's own device would pass with the old code, so each test runs a child of
//! this binary under `script(1)` ([`under_script`]) and watches the child's `/dev/tty` there.

use super::HangupWatch;
use crate::ui::testutil::{await_report, report, under_script};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where the child reports.
const OUT: &str = "IOTA_TEST_WATCH_OUT";
/// What the child is for: `quiet` (nothing is wrong), `key` (a key is typed) or `hangup` (the
/// terminal will go away).
const MODE: &str = "IOTA_TEST_WATCH_MODE";
const CHILD: &str = "ui::runtime::event_loop::hangup_tests::watch_child";

/// The beside-file the `key` and `hangup` children report to once they watch, so the parent knows
/// when to type or to take the terminal away.
fn ready_path(out: &Path) -> PathBuf {
    out.with_extension("ready")
}

/// The child half: watches `/dev/tty` exactly as the loop does when stdin is not a terminal
/// (`HangupWatch::DevTty`). Does nothing unless it is run by one of the tests below.
#[test]
#[ignore = "the child of the HangupWatch tests"]
fn watch_child() {
    let Some(out) = std::env::var_os(OUT) else {
        return;
    };
    let out = PathBuf::from(out);
    let watch = HangupWatch::DevTty(std::fs::File::open("/dev/tty").unwrap());
    let mode = std::env::var_os(MODE);
    if mode.as_deref() == Some(OsStr::new("key")) {
        // Raw, as the loop has the terminal: one byte is input at once, not a line in the making.
        crossterm::terminal::enable_raw_mode().unwrap();
        report(&ready_path(&out), "watching");
        let started = Instant::now();
        let result = watch.wait(Duration::from_secs(5));
        let took = started.elapsed().as_millis();
        let what = result.map_or_else(|e| format!("{:?}", e.kind()), |()| "Ok".to_owned());
        // The key is still there for crossterm: the watch read nothing.
        let mut key = [0u8; 1];
        let n =
            std::io::Read::read(&mut std::fs::File::open("/dev/tty").unwrap(), &mut key).unwrap();
        let _ = crossterm::terminal::disable_raw_mode();
        report(&out, &format!("{what} {:?} {took}", &key[..n]));
    } else if mode.as_deref() == Some(OsStr::new("hangup")) {
        // The terminal going away sends this process (its session's leader) a SIGHUP: caught, so
        // the watch is what has to notice.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _hup = rt
            .block_on(async {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            })
            .unwrap();
        report(&ready_path(&out), "watching");
        let started = Instant::now();
        let said = loop {
            match watch.wait(Duration::from_millis(100)) {
                Ok(()) if started.elapsed() < Duration::from_secs(10) => {}
                Ok(()) => break "still Ok".to_owned(),
                Err(e) => break format!("{:?}", e.kind()),
            }
        };
        report(&out, &said);
    } else {
        let started = Instant::now();
        let result = (0..3).try_for_each(|_| watch.wait(Duration::from_millis(100)));
        let took = started.elapsed().as_millis();
        let what = result.map_or_else(|e| format!("{:?}", e.kind()), |()| "Ok".to_owned());
        report(&out, &format!("{what} {took}"));
    }
}

/// A live terminal with nothing to say is no hang-up: three waits of 100 ms each are three
/// timeouts, and they take their time — not a `BrokenPipe` at once.
#[test]
fn a_live_terminal_is_waited_on_not_hung_up() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("watch");
    let mut script = under_script(
        CHILD,
        &[(OUT, out.as_os_str()), (MODE, OsStr::new("quiet"))],
    );
    let said = await_report(&out, Duration::from_secs(20));
    let _ = script.kill();
    let _ = script.wait();
    let said = said.expect("the child never reported");

    let (what, took) = said.rsplit_once(' ').unwrap();
    assert_eq!(what, "Ok", "{said}");
    let took: u128 = took.parse().unwrap();
    assert!(
        (270..5000).contains(&took),
        "three waits took {took} ms, want about 300"
    );
}

/// A key typed on a live terminal is input, not a hang-up: the wait comes back as soon as the byte
/// is there (readable WITH something queued), and the byte is left for crossterm to read.
#[test]
fn a_key_on_a_live_terminal_is_input_not_a_hang_up() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("watch");
    let mut script = under_script(CHILD, &[(OUT, out.as_os_str()), (MODE, OsStr::new("key"))]);
    let watching = await_report(&ready_path(&out), Duration::from_secs(20));
    if watching.is_none() {
        let _ = script.kill();
        let _ = script.wait();
    }
    assert!(watching.is_some(), "the child never started watching");
    let mut typed = script.stdin.take().unwrap();
    typed.write_all(b"k").unwrap();
    typed.flush().unwrap();
    // Held open until the child has reported: a closed stdin would end `script`.
    let said = await_report(&out, Duration::from_secs(20));
    let _ = script.kill();
    let _ = script.wait();
    let said = said.expect("the child never reported");

    let (what, took) = said.rsplit_once(' ').unwrap();
    assert_eq!(
        what, "Ok [107]",
        "the watch's verdict and the byte read after it"
    );
    let took: u128 = took.parse().unwrap();
    assert!(
        took < 4000,
        "the wait ran to its timeout ({took} ms): the key did not wake it"
    );
}

/// The terminal really going away (`script` killed: the pty's master end closes) is reported, as
/// the loop's `BrokenPipe` exit.
#[test]
fn a_terminal_that_goes_away_is_a_hang_up() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("watch");
    let mut script = under_script(
        CHILD,
        &[(OUT, out.as_os_str()), (MODE, OsStr::new("hangup"))],
    );
    let watching = await_report(&ready_path(&out), Duration::from_secs(20));
    let _ = script.kill();
    let _ = script.wait();
    assert!(watching.is_some(), "the child never started watching");

    let said = await_report(&out, Duration::from_secs(20)).expect("the child never reported");
    assert_eq!(said, "BrokenPipe");
}
