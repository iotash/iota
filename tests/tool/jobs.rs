//! Background jobs (`shell/jobs.rs`) driven end to end: real children, the two delivery modes, the
//! concurrency cap and the synchronous kill.
//!
//! Every command line here is POSIX (`true`, `sleep`, `echo … >&2`), so every test asks
//! `shell::skip_unless_posix` first — see the note in `tests/tool/main.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iota::shell::exec::{Options, Outcome};
use iota::shell::jobs::{CallEnd, JobDone, Jobs, MAX_JOBS, notice_headline, notice_text};
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
///
/// POSIX, and asked only where the answer means something: the `ps` a Windows box has is Git Bash's, which
/// has no `-o` at all and enumerates MSYS processes rather than native pids, so it reports every live child
/// as gone. Its one caller gates the question on `cfg!(unix)`.
fn alive(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Waits for `n` job records to be PARKED, up to eight seconds. `kill_all` drops the sink, so a supervisor
/// that has ended leaves its `JobDone` in the registry instead of delivering it — and it ends only after its
/// own kill path has run, which on Windows is where the tree actually dies.
async fn parked(jobs: &Arc<Jobs>, n: usize) -> Vec<JobDone> {
    let mut out = Vec::new();
    for _ in 0..400 {
        out.extend(jobs.take_finished());
        if out.len() >= n {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("only {} of {n} jobs ever reported", out.len());
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
// behind even if its tasks are never polled again. "Nothing behind" is spelled twice below, because the
// platforms reach it by different routes — the pids on Unix, the parked records everywhere.
#[tokio::test]
async fn kill_all_stops_everything_at_once() {
    if skip_unless_posix("kill_all_stops_everything_at_once") {
        return;
    }
    let (_dir, jobs) = registry();
    let a = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    let b = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    assert_eq!(jobs.running(), 2);
    // Dying AT ONCE is `killpg`'s promise and Unix's alone. `exec::kill_group` is a documented no-op on
    // Windows — a tree there is reachable only through the Job Object handle its own `Started` holds — so
    // `kill_all` cancels the tokens and each supervisor kills its tree when it next runs, which is the
    // parked records below, not the two assertions around the call. `alive` is POSIX for the same reason
    // (its `ps` is Git Bash's there, and answers for no native pid), so the pids go with it. Same gate and
    // same reason as `tests/repl/jobs.rs`, which is where the Windows shape was first written down.
    let pids: Vec<i32> = if cfg!(unix) {
        [a.pid, b.pid].into_iter().flatten().collect()
    } else {
        Vec::new()
    };
    // The assertion after the kill only means something if `alive` can say "yes" as well.
    for &pid in &pids {
        assert!(alive(pid), "pid {pid} never started");
    }

    jobs.kill_all();
    // The children are dead now, whatever the supervisors do next — dead, not yet REAPED: that is the
    // supervisor's job and it has not been polled. `alive` asks for the state, which is why this can be
    // checked the instant `kill_all` returns (see its doc comment).
    for &pid in &pids {
        assert!(!alive(pid), "pid {pid} survived kill_all");
    }
    // The registry is empty the moment the call returns, on every platform: `kill_all` took the map.
    assert_eq!(jobs.running(), 0);
    // And every supervisor runs to its end rather than sitting on a `sleep 30` — the half of the claim
    // Windows keeps, since that end is where its Job Object is terminated. Both records are parked, never
    // delivered: the sink went with the jobs. Both say `killed`: the token's verdict, even when the
    // child's death reached the wait before the token did.
    for d in &parked(&jobs, 2).await {
        assert!(d.killed && d.exit.is_none(), "{d:?}");
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

// `kill` — the `/jobs` Kill tab — ends ONE job the way `kill_all` ends them all, but through the job's own
// supervisor: the child dies at once, the record says `killed`, the sink gets the notice and the watch hears
// the set shrink, while the other job runs on. The end is remembered (`ended`); an unknown id is nothing.
#[tokio::test]
async fn kill_ends_one_job_through_its_own_supervisor() {
    if skip_unless_posix("kill_ends_one_job_through_its_own_supervisor") {
        return;
    }
    let (_dir, jobs) = registry();
    let log = watched(&jobs);
    let delivered: Arc<Mutex<Vec<JobDone>>> = Arc::default();
    let sink = Arc::clone(&delivered);
    jobs.set_sink(Some(Box::new(move |done: JobDone| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(done);
    })));
    let a = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    let _b = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    assert_eq!(jobs.running(), 2);
    assert_eq!(jobs.ended("b1"), None, "a running job has no end yet");

    jobs.kill("nope");
    assert_eq!(jobs.running(), 2, "an unknown id kills nothing");

    jobs.kill("b1");
    // Dead the moment the call returns (`killpg`, Unix); reaped and recorded by the supervisor, which
    // is what the sink and the watch wait for below.
    if cfg!(unix)
        && let Some(pid) = a.pid
    {
        assert!(!alive(pid), "pid {pid} survived kill");
    }
    for _ in 0..400 {
        if !delivered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let done = delivered
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(done.len(), 1, "one notice, for the killed job: {done:?}");
    assert_eq!(done[0].id, "b1");
    assert!(done[0].killed && !done[0].timed_out && done[0].exit.is_none());
    assert_eq!(
        notice_headline(&done[0]),
        "[background job b1 finished: killed] sleep 30"
    );
    assert_eq!(jobs.running(), 1, "the other job runs on");
    assert_eq!(
        seen(&log).last(),
        Some(&vec!["b2".to_owned()]),
        "the watch heard b1 go"
    );
    // The end is remembered — the page open on b1 reads its verdict here — and b2's is not there to read.
    assert_eq!(jobs.ended("b1"), Some(done[0].clone()));
    assert_eq!(jobs.ended("b2"), None);

    jobs.kill_all();
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

// ---- the call path (`Jobs::run`): every `shell` call starts in job shape and lets go at the window ----

/// The ids the watch was told, one list per call, in order.
fn watched(jobs: &Arc<Jobs>) -> Arc<Mutex<Vec<Vec<String>>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_watch(Some(Box::new(move |running| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(running.into_iter().map(|j| j.id).collect());
    })));
    seen
}

fn seen(log: &Arc<Mutex<Vec<Vec<String>>>>) -> Vec<Vec<String>> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The names of the log files the registry's directory holds.
fn logs(dir: &TempDir) -> Vec<String> {
    let jobs_dir = dir
        .path()
        .join(iota::shell::jobs::JOBS_DIR)
        .join(std::process::id().to_string());
    let Ok(entries) = std::fs::read_dir(jobs_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// A command that exits inside the window is a foreground call as before: the output, the exit — and no job,
// no file, no notice.
#[tokio::test]
async fn a_call_that_ends_inside_the_window_is_a_foreground_call() {
    if skip_unless_posix("a_call_that_ends_inside_the_window_is_a_foreground_call") {
        return;
    }
    let (dir, jobs) = registry();
    let watch = watched(&jobs);
    let cancel = CancellationToken::new();
    let CallEnd::Ended(res) = jobs
        .run(
            &cancel,
            &opts("echo out; echo err >&2; exit 3", None),
            Duration::from_secs(10),
        )
        .await
    else {
        panic!("a two-millisecond command yielded")
    };
    assert_eq!(res.output, "out\nerr\n");
    assert_eq!(res.outcome, Outcome::Exited(3));
    assert_eq!(jobs.running(), 0);
    assert!(jobs.snapshot().is_empty());
    assert!(
        jobs.take_finished().is_empty(),
        "nothing finished: nothing was a job"
    );
    assert!(
        seen(&watch).is_empty(),
        "the watch heard about a call that never was a job"
    );
    assert_eq!(
        logs(&dir),
        Vec::<String>::new(),
        "the call's log was not removed"
    );
}

// A command still running at the window becomes a job where it stands: the next id, the log renamed after
// it, the output so far — and the notice, when it ends, carries the whole output and the whole time.
#[tokio::test]
async fn a_call_still_running_at_the_window_becomes_a_job() {
    if skip_unless_posix("a_call_still_running_at_the_window_becomes_a_job") {
        return;
    }
    let (dir, jobs) = registry();
    let watch = watched(&jobs);
    let cancel = CancellationToken::new();
    let CallEnd::Yielded { start, output } = jobs
        .run(
            &cancel,
            &opts("echo early; sleep 2; echo late", None),
            Duration::from_millis(500),
        )
        .await
    else {
        panic!("a two-second command did not yield at half a second")
    };
    assert_eq!(start.id, "b1");
    assert!(start.pid.is_some(), "the OS reported no pid");
    assert!(
        start.output_path.ends_with("b1.log"),
        "{:?}",
        start.output_path
    );
    assert_eq!(
        output, "early\n",
        "the output so far is what the file held at the window"
    );
    assert_eq!(jobs.running(), 1);
    let listed = jobs.snapshot();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "b1");
    assert_eq!(listed[0].command, "echo early; sleep 2; echo late");
    assert_eq!(listed[0].output_path, start.output_path);
    assert!(listed[0].started.elapsed() >= Duration::from_millis(500));
    assert_eq!(
        logs(&dir),
        vec!["b1.log".to_owned()],
        "the log was not renamed"
    );
    assert_eq!(seen(&watch), vec![vec!["b1".to_owned()]]);

    // The call's own token has no hold over the job any more.
    cancel.cancel();
    let done = drain(&jobs).await;
    assert_eq!(done.len(), 1);
    let done = &done[0];
    assert_eq!(done.id, "b1");
    assert_eq!(done.exit, Some(0));
    assert!(!done.killed, "the call's token killed the job it let go of");
    assert!(
        done.elapsed >= Duration::from_secs(2),
        "the elapsed time counts from the spawn, not the yield: {:?}",
        done.elapsed
    );
    let text = notice_text(done);
    assert!(
        text.ends_with("] echo early; sleep 2; echo late\nearly\nlate\n"),
        "{text}"
    );
    // The watch heard the job go BEFORE the notice was parked.
    assert_eq!(seen(&watch), vec![vec!["b1".to_owned()], Vec::new()]);
    assert!(jobs.snapshot().is_empty());
}

// The window and the timeout are two marks on ONE clock, measured from the spawn: a timeout inside the window
// fires as the timeout, and the call's token still kills a command inside the window.
#[tokio::test]
async fn inside_the_window_the_timeout_and_the_token_still_end_the_call() {
    if skip_unless_posix("inside_the_window_the_timeout_and_the_token_still_end_the_call") {
        return;
    }
    let (_dir, jobs) = registry();
    let cancel = CancellationToken::new();
    let CallEnd::Ended(res) = jobs
        .run(&cancel, &opts("sleep 30", Some(1)), Duration::from_secs(10))
        .await
    else {
        panic!("a one-second timeout inside a ten-second window yielded")
    };
    assert_eq!(res.outcome, Outcome::TimedOut);
    assert_eq!(jobs.running(), 0);

    let cancel = CancellationToken::new();
    let killer = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        killer.cancel();
    });
    let CallEnd::Ended(res) = jobs
        .run(&cancel, &opts("sleep 30", None), Duration::from_secs(10))
        .await
    else {
        panic!("a cancelled call yielded")
    };
    assert_eq!(res.outcome, Outcome::Cancelled);
    assert_eq!(jobs.running(), 0, "a cancelled call left a job behind");
}

// At the cap a call does not yield: it is waited for to its end, and the result says so — without a job, an
// id or a notice.
#[tokio::test]
async fn at_the_cap_a_call_is_waited_for_instead() {
    if skip_unless_posix("at_the_cap_a_call_is_waited_for_instead") {
        return;
    }
    let (_dir, jobs) = registry();
    for _ in 0..MAX_JOBS {
        jobs.spawn(&opts("sleep 30", None)).expect("under the cap");
    }
    let cancel = CancellationToken::new();
    let CallEnd::Waited(res) = jobs
        .run(
            &cancel,
            &opts("sleep 1; echo done", None),
            Duration::from_millis(200),
        )
        .await
    else {
        panic!("a call yielded into a full run")
    };
    assert_eq!(res.output, "done\n");
    assert_eq!(res.outcome, Outcome::Exited(0));
    assert_eq!(jobs.running(), MAX_JOBS, "the waited call took a slot");
    jobs.kill_all();
    drain(&jobs).await;
}

// The watch is the listing's seam: it hears a `background: true` start, a yield and every finish, oldest
// first, and a watch installed late hears what is already running.
#[tokio::test]
async fn the_watch_hears_the_running_set_change() {
    if skip_unless_posix("the_watch_hears_the_running_set_change") {
        return;
    }
    let (_dir, jobs) = registry();
    let first = jobs.spawn(&opts("sleep 30", None)).expect("spawn");
    assert_eq!(first.id, "b1");
    let watch = watched(&jobs);
    assert_eq!(
        seen(&watch),
        vec![vec!["b1".to_owned()]],
        "a late watch was not told what runs"
    );

    let cancel = CancellationToken::new();
    let CallEnd::Yielded { start, .. } = jobs
        .run(&cancel, &opts("sleep 1", None), Duration::from_millis(100))
        .await
    else {
        panic!("a one-second command did not yield at a tenth")
    };
    assert_eq!(start.id, "b2");
    assert_eq!(
        seen(&watch),
        vec![
            vec!["b1".to_owned()],
            vec!["b1".to_owned(), "b2".to_owned()]
        ]
    );
    let ids: Vec<String> = jobs.snapshot().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, ["b1", "b2"], "oldest first");

    // b2 ends on its own; b1 is killed on the way out, and the dropped watch hears nothing of that.
    let cancel = CancellationToken::new();
    let done = jobs.wait_any(&cancel).await.expect("b2 finishes");
    assert_eq!(done.id, "b2");
    assert_eq!(seen(&watch).last(), Some(&vec!["b1".to_owned()]));
    jobs.kill_all();
    drain(&jobs).await;
    assert_eq!(
        seen(&watch).len(),
        3,
        "kill_all reported through a dropped watch"
    );
}
