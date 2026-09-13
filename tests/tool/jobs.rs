//! Background jobs (`shell/jobs.rs`) driven end to end: real children, the two delivery modes, the
//! concurrency cap and the synchronous kill.
//!
//! Every command line here is POSIX (`true`, `sleep`, `echo … >&2`), so every test asks
//! `shell::skip_unless_posix` first — see the note in `tests/tool/main.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iota::shell::exec::Options;
use iota::shell::jobs::{JobDone, Jobs, MAX_JOBS, notice_headline, notice_text};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::shell::skip_unless_posix;

/// A registry writing its logs under a fresh temp dir, plus the dir that must outlive it.
fn registry() -> (TempDir, Arc<Jobs>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let jobs = Jobs::new(dir.path());
    (dir, jobs)
}

/// One job request over `command`, with `timeout` seconds (`None` = no deadline).
fn opts(command: &str, timeout: Option<u64>) -> Options {
    Options {
        command: command.to_owned(),
        dir: PathBuf::new(),
        timeout: timeout.map(Duration::from_secs),
        sandbox: None,
    }
}

/// Whether `pid` is still a LIVE process. Not `/proc/<pid>`, and not `kill(pid, 0)`: a killed child stays a
/// zombie until its parent reaps it, and a zombie keeps its pid, its `/proc` entry and its answer to signal
/// 0 — so both of those report "alive" for a process that is already dead. The process STATE is the thing
/// being asked about, and `ps -o stat=` is the one spelling of it macOS and Linux share (empty output: gone;
/// `Z`: dead, not yet reaped). The twin of `tests/repl/jobs.rs::alive`.
fn alive(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Waits for the registry to have nothing left running (the tests never sleep blindly).
async fn drain(jobs: &Arc<Jobs>) -> Vec<JobDone> {
    let mut out = Vec::new();
    let cancel = CancellationToken::new();
    while let Some(done) = jobs.wait_any(&cancel).await {
        out.push(done);
    }
    out
}

// A job that exits on its own reports its code, its command and its output; a second one gets the next id.
#[tokio::test]
async fn a_finished_job_reports_its_status_and_output() {
    if skip_unless_posix("a_finished_job_reports_its_status_and_output") {
        return;
    }
    let (_dir, jobs) = registry();

    let start = jobs.spawn(&opts("echo hello", None)).expect("spawn");
    assert_eq!(start.id, "b1");
    assert!(start.pid.is_some(), "the OS reported no pid");
    assert!(start.output_path.ends_with("b1.log"));
    assert_eq!(jobs.running(), 1);

    let done = drain(&jobs).await;
    assert_eq!(done.len(), 1);
    let done = &done[0];
    assert_eq!(done.id, "b1");
    assert_eq!(done.command, "echo hello");
    assert_eq!(done.exit, Some(0));
    assert!(!done.timed_out && !done.killed);
    assert_eq!(jobs.running(), 0);
    let text = notice_text(done);
    assert!(
        text.starts_with("[background job b1 finished: exit 0 after "),
        "{text}"
    );
    assert!(text.ends_with("] echo hello\nhello\n"), "{text}");
    // The log file survives the notice: `tail` still works after the fact.
    assert_eq!(
        std::fs::read_to_string(&done.output_path).expect("log"),
        "hello\n"
    );

    // Ids are per run, not per outstanding job.
    let second = jobs.spawn(&opts("true", None)).expect("spawn");
    assert_eq!(second.id, "b2");
    drain(&jobs).await;
}

// A failing job is still a finished job: the exit code travels, and stderr is in the same log as stdout.
#[tokio::test]
async fn a_failing_job_carries_its_code_and_stderr() {
    if skip_unless_posix("a_failing_job_carries_its_code_and_stderr") {
        return;
    }
    let (_dir, jobs) = registry();
    jobs.spawn(&opts("echo out; echo err >&2; exit 3", None))
        .expect("spawn");
    let done = drain(&jobs).await;
    assert_eq!(done[0].exit, Some(3));
    let text = notice_text(&done[0]);
    assert!(text.contains("finished: exit 3 after "), "{text}");
    assert!(text.contains("out") && text.contains("err"), "{text}");
}

// The `timeout` a background call carries is the same deadline a foreground one gets: the tree is killed
// and the notice says so.
#[tokio::test]
async fn a_job_past_its_timeout_is_killed_and_says_so() {
    if skip_unless_posix("a_job_past_its_timeout_is_killed_and_says_so") {
        return;
    }
    let (_dir, jobs) = registry();
    jobs.spawn(&opts("sleep 30", Some(1))).expect("spawn");
    let done = drain(&jobs).await;
    assert!(done[0].timed_out, "{:?}", done[0]);
    assert_eq!(done[0].exit, None);
    assert!(
        notice_headline(&done[0]).contains("timed out after "),
        "{}",
        notice_headline(&done[0])
    );
}

// `kill_all` is synchronous: the process group is gone before it returns, so an exiting iota leaves nothing
// behind even if its tasks are never polled again.
#[tokio::test]
async fn kill_all_stops_everything_at_once() {
    if skip_unless_posix("kill_all_stops_everything_at_once") {
        return;
    }
    let (_dir, jobs) = registry();
    let a = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    let b = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    assert_eq!(jobs.running(), 2);
    // The assertion after the kill only means something if `alive` can say "yes" as well.
    for pid in [a.pid, b.pid].into_iter().flatten() {
        assert!(alive(pid), "pid {pid} never started");
    }

    jobs.kill_all();
    // The children are dead now, whatever the supervisors do next — dead, not yet REAPED: that is the
    // supervisor's job and it has not been polled. `alive` asks for the state, which is why this can be
    // checked the instant `kill_all` returns (see its doc comment).
    for pid in [a.pid, b.pid].into_iter().flatten() {
        assert!(!alive(pid), "pid {pid} survived kill_all");
    }
    let done = drain(&jobs).await;
    assert_eq!(jobs.running(), 0);
    // Whatever the supervisors managed to record, nothing is left waiting.
    for d in &done {
        assert!(d.killed || d.exit.is_some(), "{d:?}");
    }
}

// The cap refuses rather than queues, and a finished job frees its slot.
#[tokio::test]
async fn the_concurrency_cap_refuses_the_seventeenth() {
    if skip_unless_posix("the_concurrency_cap_refuses_the_seventeenth") {
        return;
    }
    let (_dir, jobs) = registry();
    for _ in 0..MAX_JOBS {
        jobs.spawn(&opts("sleep 30", None)).expect("under the cap");
    }
    assert_eq!(jobs.running(), MAX_JOBS);
    let err = jobs.spawn(&opts("true", None)).expect_err("over the cap");
    assert_eq!(
        err.to_string(),
        "too many background jobs running (16); wait for one to finish"
    );
    // The refused call left nothing behind — a leaked slot would shrink the cap for the rest of the run.
    assert_eq!(jobs.running(), MAX_JOBS);
    // A job that cannot even be started frees its slot too.
    let bad = Options {
        dir: PathBuf::from("/definitely/not/a/directory"),
        ..opts("true", None)
    };
    jobs.kill_all();
    drain(&jobs).await;
    assert!(jobs.spawn(&bad).is_err(), "a bad cwd must fail to spawn");
    assert_eq!(jobs.running(), 0, "a failed spawn leaked its slot");
    jobs.kill_all();
    drain(&jobs).await;
    // The slots came back.
    jobs.spawn(&opts("true", None)).expect("a freed slot");
    drain(&jobs).await;
}

// With a sink installed nothing parks: every completion is handed over the moment it lands, including one
// that finished BEFORE the sink existed.
#[tokio::test]
async fn a_sink_takes_delivery_including_the_backlog() {
    if skip_unless_posix("a_sink_takes_delivery_including_the_backlog") {
        return;
    }
    let (_dir, jobs) = registry();
    jobs.spawn(&opts("true", None)).expect("spawn");
    // Park it first.
    while jobs.running() > 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&seen);
    jobs.set_sink(Some(Box::new(move |done: JobDone| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(done.id);
    })));
    assert_eq!(
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        ["b1".to_owned()],
        "a job that landed before the sink must still be delivered"
    );
    assert!(
        jobs.take_finished().is_empty(),
        "a delivered job must not also park"
    );

    // …and one that finishes after it is delivered live.
    jobs.spawn(&opts("true", None)).expect("spawn");
    for _ in 0..200 {
        if seen.lock().unwrap_or_else(PoisonError::into_inner).len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        ["b1".to_owned(), "b2".to_owned()]
    );
}

// `wait_any` is the headless wait: it returns None rather than hanging when there is nothing to wait for,
// and a cancelled run ends it too.
#[tokio::test]
async fn wait_any_ends_on_nothing_running_or_a_cancelled_run() {
    if skip_unless_posix("wait_any_ends_on_nothing_running_or_a_cancelled_run") {
        return;
    }
    let (_dir, jobs) = registry();
    let cancel = CancellationToken::new();
    assert!(
        jobs.wait_any(&cancel).await.is_none(),
        "an empty registry must not park the run"
    );

    jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    cancel.cancel();
    assert!(
        jobs.wait_any(&cancel).await.is_none(),
        "a cancelled run must stop waiting"
    );
    jobs.kill_all();
}
