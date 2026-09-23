//! Background `shell` jobs: the run's registry of children that outlive their call — started with
//! `background: true`, or a foreground call that ran past its wait (`SHELL_YIELD`, 20 s) and let go — and
//! the completion notice each one produces.
//!
//! A job is an ordinary [`exec::spawn`] child — same sandbox, same `setpgid`, same `killpg` deadline — with
//! its combined output going to a file instead of the round's 32 KB buffer, so the model's turn ends while
//! the work continues. EVERY `shell` call starts in that shape ([`Jobs::run`]): the file is opened before
//! the child, the call waits for the exit or the window, and a command still running at the window is
//! adopted where it stands — an id, the file renamed after it, a supervisor — while the call answers with
//! what the file holds so far. A command that exits inside the window answers as a foreground call always
//! has, and its file is removed. When a job finishes, a [`JobDone`] is DELIVERED (interactive: the installed
//! sink pushes it at the UI, which enqueues it as the next input) or PARKED (headless: the loop drains
//! [`Jobs::take_finished`] each round and blocks on [`Jobs::wait_any`] when it has nothing else to do), and
//! the WATCH — the second interactive seam, [`Jobs::set_watch`] — hears the running set change (a job
//! adopted or started, a job gone), which is what puts `/jobs` in the command table and the job segment on
//! the status row, and takes them away again.
//!
//! Nothing here outlives the process: [`Jobs::kill_all`] is synchronous `killpg` precisely so a `/quit` or a
//! failed headless run cannot leave a tree behind, and a resumed session therefore never sees a job it
//! started last time (documented in the README).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

use crate::shell::exec::{self, Capture, Options, Outcome, RunResult, ShellError, SpawnFail};

/// How many jobs one run may have in flight. Past it the tool refuses rather than queues: a queue the model
/// cannot see would make `background` a lie.
pub const MAX_JOBS: usize = 16;

/// The directory under `dirs.temp` that holds every run's job logs.
pub const JOBS_DIR: &str = "iota-jobs";

/// What [`Jobs::spawn`] hands back to the tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobStart {
    /// The run-local job name (`b1`, `b2`, …).
    pub id: String,
    /// The process-group leader, when the OS reported one.
    pub pid: Option<i32>,
    /// Where the job's combined stdout/stderr is being written.
    pub output_path: PathBuf,
}

/// One job in flight, as [`Jobs::snapshot`] lists it — what `/jobs` and the status row's job segment show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    /// The run-local job name.
    pub id: String,
    /// The command line it runs.
    pub command: String,
    /// The process-group leader, when the OS reported one.
    pub pid: Option<i32>,
    /// When its child was spawned — for a yielded call, that is the CALL's start, not the yield.
    pub started: Instant,
    /// The log file it writes.
    pub output_path: PathBuf,
}

/// One finished job, in the shape the notice is rendered from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobDone {
    /// The run-local job name.
    pub id: String,
    /// The command line it ran.
    pub command: String,
    /// The exit code, when it ended on its own (`-1` on signal death).
    pub exit: Option<i32>,
    /// Whether its `timeout` expired.
    pub timed_out: bool,
    /// Whether [`Jobs::kill_all`] (or a cancelled run) killed it.
    pub killed: bool,
    /// Wall-clock time from spawn to end — for a yielded call, the wait it ran through included.
    pub elapsed: Duration,
    /// The log file, still on disk (the model can `tail` it).
    pub output_path: PathBuf,
}

/// How one `shell` call ended in the registry's hands ([`Jobs::run`]).
#[derive(Debug)]
pub enum CallEnd {
    /// The command ended inside the window (or never started): the foreground result, exactly as
    /// [`exec::run`] would have answered.
    Ended(RunResult),
    /// The window ran out with the command still running: it is a job now, and `output` is what it had
    /// written by then, under the foreground caps.
    Yielded {
        /// The job it became.
        start: JobStart,
        /// Its output so far.
        output: String,
    },
    /// The window ran out, but the run already had [`MAX_JOBS`] in flight, so the call was waited for to
    /// its end instead — the result is [`CallEnd::Ended`]'s, and the tool says why it took so long.
    Waited(RunResult),
}

/// Why a job could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// The run already has [`MAX_JOBS`] in flight.
    #[error("too many background jobs running ({MAX_JOBS}); wait for one to finish")]
    TooMany,
    /// The log file could not be created.
    #[error("failed to create the job's output file: {0}")]
    Output(String),
    /// The child could not be started (the [`exec::ShellError`] text, or a cancelled run).
    #[error("{0}")]
    Spawn(String),
}

/// Called with every [`JobDone`] the moment it lands (interactive delivery).
pub type JobSink = Box<dyn Fn(JobDone) + Send + Sync>;

/// Called with the whole running set — oldest first — whenever it changes: a job started or adopted, a
/// job finished (BEFORE its notice is delivered, so the row that lists it is gone by the time the notice
/// is read). Never called from under the registry's lock.
pub type JobWatch = Box<dyn Fn(Vec<JobInfo>) + Send + Sync>;

/// Why [`Jobs::start_child`] produced no child.
enum StartFail {
    /// The log file could not be created.
    Output(std::io::Error),
    /// [`exec::spawn`] refused.
    Spawn(SpawnFail),
}

/// One running job, as the registry tracks it.
struct Running {
    /// The group leader, for a synchronous `killpg`.
    pid: Option<i32>,
    /// Fires to make the supervisor kill and reap it.
    cancel: CancellationToken,
    /// The command line, for the listings.
    command: String,
    /// When the child was spawned.
    started: Instant,
    /// Its log file.
    output_path: PathBuf,
}

/// The registry's mutable half.
#[derive(Default)]
struct State {
    /// Next job number (`b1` is the first).
    next: u32,
    /// Next call number — the name a call's log carries until the call is a job, if it ever is.
    calls: u32,
    /// Jobs in flight, by id.
    running: BTreeMap<String, Running>,
    /// Completed jobs nobody has taken yet — always empty while a sink is installed.
    finished: Vec<JobDone>,
    /// The interactive delivery seam.
    sink: Option<Arc<JobSink>>,
    /// The interactive listing seam.
    watch: Option<Arc<JobWatch>>,
}

impl State {
    /// The running set, oldest first.
    fn snapshot(&self) -> Vec<JobInfo> {
        let mut jobs: Vec<JobInfo> = self
            .running
            .iter()
            .map(|(id, job)| JobInfo {
                id: id.clone(),
                command: job.command.clone(),
                pid: job.pid,
                started: job.started,
                output_path: job.output_path.clone(),
            })
            .collect();
        jobs.sort_by_key(|j| j.started);
        jobs
    }

    /// Claims the next job id and its slot, or `None` at the cap. Under ONE lock with the check, so two
    /// calls yielding in one parallel batch cannot both read 15 and both start.
    fn claim(&mut self, job: Running) -> Option<String> {
        if self.running.len() >= MAX_JOBS {
            return None;
        }
        self.next += 1;
        let id = format!("b{}", self.next);
        self.running.insert(id.clone(), job);
        Some(id)
    }
}

/// The run's background-job registry: ONE per run, created at tool assembly and shared by the `shell` tool
/// (which starts jobs), the loop (which delivers notices) and the exit path (which kills what is left).
pub struct Jobs {
    /// `<temp>/iota-jobs/<pid>` — this process's log directory.
    dir: PathBuf,
    state: Mutex<State>,
    /// Woken whenever a job lands in `finished` or the last runner leaves.
    done: tokio::sync::Notify,
}

impl Jobs {
    /// A registry writing its logs under `<temp>/iota-jobs/<this process's pid>`, so two iota processes
    /// sharing a temp dir never collide and a stale directory names the run that left it.
    pub fn new(temp: &Path) -> Arc<Self> {
        Arc::new(Self {
            dir: temp.join(JOBS_DIR).join(std::process::id().to_string()),
            state: Mutex::new(State::default()),
            done: tokio::sync::Notify::new(),
        })
    }

    /// Installs (or, with `None`, removes) the interactive delivery seam. Anything already parked in
    /// `finished` is handed over immediately, so a sink installed after a job landed still sees it.
    pub fn set_sink(&self, sink: Option<JobSink>) {
        let (sink, parked) = {
            let mut st = self.lock();
            st.sink = sink.map(Arc::new);
            let parked = match st.sink {
                Some(_) => std::mem::take(&mut st.finished),
                None => Vec::new(),
            };
            (st.sink.clone(), parked)
        };
        if let Some(sink) = sink {
            for done in parked {
                sink(done);
            }
        }
    }

    /// Installs (or, with `None`, removes) the interactive listing seam. A watch installed while jobs are
    /// already running hears about them at once.
    pub fn set_watch(&self, watch: Option<JobWatch>) {
        let (watch, running) = {
            let mut st = self.lock();
            st.watch = watch.map(Arc::new);
            (st.watch.clone(), st.snapshot())
        };
        if let Some(watch) = watch
            && !running.is_empty()
        {
            watch(running);
        }
    }

    /// Tells the watch, if any, what is running now.
    fn notify_watch(&self) {
        let (watch, running) = {
            let st = self.lock();
            (st.watch.clone(), st.snapshot())
        };
        if let Some(watch) = watch {
            watch(running);
        }
    }

    /// Starts one background job — `background: true`, the call that is not waited on at all. The child is
    /// [`exec::spawn`]ed exactly as a foreground call is — same sandbox, same working directory, same
    /// `timeout` — but its output goes to a file and a detached task supervises it. The slot is claimed
    /// BEFORE the child exists, so a refused call starts nothing.
    pub fn spawn(self: &Arc<Self>, opts: &Options) -> Result<JobStart, JobError> {
        let cancel = CancellationToken::new();
        let Some(id) = self.lock().claim(Running {
            pid: None,
            cancel: cancel.clone(),
            command: opts.command.clone(),
            started: Instant::now(),
            output_path: PathBuf::new(),
        }) else {
            return Err(JobError::TooMany);
        };
        let output_path = self.dir.join(format!("{id}.log"));
        let started = match self.start_child(&cancel, opts, &output_path) {
            Ok(started) => started,
            Err(e) => {
                // A slot claimed by a job that never started is a slot leaked forever.
                self.lock().running.remove(&id);
                return Err(match e {
                    StartFail::Output(e) => JobError::Output(e.to_string()),
                    StartFail::Spawn(SpawnFail::Cancelled) => {
                        JobError::Spawn("the run was cancelled".to_owned())
                    }
                    StartFail::Spawn(SpawnFail::Failed(e)) => JobError::Spawn(e.to_string()),
                });
            }
        };
        let pid = started.pid;
        if let Some(job) = self.lock().running.get_mut(&id) {
            job.pid = pid;
            job.started = started.started();
            job.output_path.clone_from(&output_path);
        }
        self.notify_watch();
        self.supervise(&id, started, cancel, opts, output_path.clone());
        Ok(JobStart {
            id,
            pid,
            output_path,
        })
    }

    /// Runs one `shell` call in job shape: the child writes to a file under this registry's directory
    /// from the start, the call waits for the exit or for `window` to pass since the spawn — whichever
    /// comes first — and a command still running at the window becomes a job where it stands
    /// ([`CallEnd::Yielded`]). At the cap the call is waited for to its end instead ([`CallEnd::Waited`]).
    /// `cancel` is the CALL's token: it kills a command still in the window, as a foreground call's always
    /// has, and has no hold over a job the call let go of — a job has its own.
    pub async fn run(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        opts: &Options,
        window: Duration,
    ) -> CallEnd {
        let pending = {
            let mut st = self.lock();
            st.calls += 1;
            self.dir.join(format!("call-{}.log", st.calls))
        };
        let mut started = match self.start_child(cancel, opts, &pending) {
            Ok(started) => started,
            Err(fail) => {
                let outcome = match fail {
                    StartFail::Output(e) => Outcome::Failed(ShellError::Spawn(format!(
                        "failed to create the call's output file: {e}"
                    ))),
                    StartFail::Spawn(SpawnFail::Cancelled) => Outcome::Cancelled,
                    StartFail::Spawn(SpawnFail::Failed(e)) => Outcome::Failed(e),
                };
                return CallEnd::Ended(RunResult {
                    output: String::new(),
                    outcome,
                });
            }
        };
        if let Some(outcome) = started
            .wait_or_yield(cancel, opts.timeout, Some(window))
            .await
        {
            return CallEnd::Ended(ended(&pending, outcome));
        }
        // Still running at the window: a job, if the run has room for one.
        let own = CancellationToken::new();
        let claimed = self.lock().claim(Running {
            pid: started.pid,
            cancel: own.clone(),
            command: opts.command.clone(),
            started: started.started(),
            output_path: pending.clone(),
        });
        let Some(id) = claimed else {
            let outcome = started.wait(cancel, opts.timeout).await;
            return CallEnd::Waited(ended(&pending, outcome));
        };
        // The log takes the job's name. A rename that fails leaves the call's name on it — the file is the
        // same either way, and the path the model is told is the one the registry lists.
        let named = self.dir.join(format!("{id}.log"));
        let output_path = if std::fs::rename(&pending, &named).is_ok() {
            named
        } else {
            pending
        };
        if let Some(job) = self.lock().running.get_mut(&id) {
            job.output_path.clone_from(&output_path);
        }
        let output = exec::read_capped(&output_path)
            .map(|s| exec::truncate_output(&s))
            .unwrap_or_default();
        self.notify_watch();
        let pid = started.pid;
        self.supervise(&id, started, own, opts, output_path.clone());
        CallEnd::Yielded {
            start: JobStart {
                id,
                pid,
                output_path,
            },
            output,
        }
    }

    /// The log file and the child behind one call — everything between naming a file and owning a
    /// process.
    fn start_child(
        &self,
        cancel: &CancellationToken,
        opts: &Options,
        output_path: &Path,
    ) -> Result<exec::Started, StartFail> {
        std::fs::create_dir_all(&self.dir).map_err(StartFail::Output)?;
        let file = std::fs::File::create(output_path).map_err(StartFail::Output)?;
        exec::spawn(cancel, opts, Capture::File(file)).map_err(StartFail::Spawn)
    }

    /// Detaches the task that sees the job to its end.
    fn supervise(
        self: &Arc<Self>,
        id: &str,
        started: exec::Started,
        cancel: CancellationToken,
        opts: &Options,
        output_path: PathBuf,
    ) {
        let done = JobDone {
            id: id.to_owned(),
            command: opts.command.clone(),
            exit: None,
            timed_out: false,
            killed: false,
            elapsed: Duration::ZERO,
            output_path,
        };
        tokio::spawn(supervise(
            Arc::clone(self),
            started,
            cancel,
            opts.timeout,
            done,
        ));
    }

    /// How many jobs are still in flight.
    pub fn running(&self) -> usize {
        self.lock().running.len()
    }

    /// The jobs in flight, oldest first.
    pub fn snapshot(&self) -> Vec<JobInfo> {
        self.lock().snapshot()
    }

    /// Drains every job that finished since the last call (the headless delivery seam; always empty while a
    /// sink is installed).
    pub fn take_finished(&self) -> Vec<JobDone> {
        std::mem::take(&mut self.lock().finished)
    }

    /// Blocks until one job finishes, `cancel` fires, or nothing is running any more. `None` means "stop
    /// waiting" — the caller has no reason to spend another round.
    pub async fn wait_any(&self, cancel: &CancellationToken) -> Option<JobDone> {
        loop {
            // The waiter is armed BEFORE the state is read, so a job landing in between wakes it rather
            // than being missed.
            let woken = self.done.notified();
            {
                let mut st = self.lock();
                if !st.finished.is_empty() {
                    return Some(st.finished.remove(0));
                }
                if st.running.is_empty() {
                    return None;
                }
            }
            tokio::select! {
                () = woken => {}
                () = cancel.cancelled() => return None,
            }
        }
    }

    /// Kills every job still running, synchronously — the exit path, which cannot await. The sink and the
    /// watch are dropped with them: nobody is left to deliver a notice to, or a listing.
    pub fn kill_all(&self) {
        let mut st = self.lock();
        st.sink = None;
        st.watch = None;
        for (_, job) in std::mem::take(&mut st.running) {
            job.cancel.cancel();
            exec::kill_group(job.pid);
        }
        drop(st);
        self.done.notify_waiters();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        crate::sync::lock(&self.state)
    }
}

/// A call that ended inside its window: the file read back under the foreground caps, then removed —
/// nothing lists it, so nothing should be left to find.
fn ended(pending: &Path, outcome: Outcome) -> RunResult {
    let output = exec::read_capped(pending)
        .map(|s| exec::truncate_output(&s))
        .unwrap_or_default();
    let _ = std::fs::remove_file(pending);
    RunResult { output, outcome }
}

/// Supervises one job to its end, then delivers or parks the [`JobDone`].
async fn supervise(
    jobs: Arc<Jobs>,
    mut started: exec::Started,
    cancel: CancellationToken,
    timeout: Option<Duration>,
    mut done: JobDone,
) {
    let outcome = started.wait(&cancel, timeout).await;
    done.elapsed = started.started().elapsed();
    done.timed_out = matches!(outcome, exec::Outcome::TimedOut);
    done.killed = matches!(outcome, exec::Outcome::Cancelled);
    done.exit = match outcome {
        exec::Outcome::Exited(code) => Some(code),
        _ => None,
    };
    let sink = {
        let mut st = jobs.lock();
        st.running.remove(&done.id);
        let sink = st.sink.clone();
        if sink.is_none() {
            st.finished.push(done.clone());
        }
        sink
    };
    // The listing hears the job is gone BEFORE the notice is delivered, so the row that names it never
    // outlives the line that says it finished.
    jobs.notify_watch();
    // The waiter is woken either way: a delivered job still changes `running()`, which is what ends a
    // headless wait that has nothing left to wait for.
    jobs.done.notify_waiters();
    if let Some(sink) = sink {
        sink(done);
    }
}

/// The notice's first line: what happened, in a fixed shape the model can pattern-match. The command is
/// the label the call's `[shell …]` header showed — `text::header_command`: one line, tail-cut at 64
/// runes — so what the user reads at the end is what they read at the start; the full text is the
/// `/jobs` detail page's.
pub fn notice_headline(done: &JobDone) -> String {
    let status = if done.timed_out {
        format!("timed out after {}", crate::text::elapsed(done.elapsed))
    } else if done.killed {
        "killed".to_owned()
    } else {
        match done.exit {
            Some(code) => format!("exit {code} after {}", crate::text::elapsed(done.elapsed)),
            None => "killed".to_owned(),
        }
    };
    format!(
        "[background job {} finished: {status}] {}",
        done.id,
        crate::text::header_command(&done.command)
    )
}

/// The full notice: the headline, then the job's output under the SAME caps a foreground call gets — the
/// byte cap by [`exec::read_capped`] (two seeks, never a stream) and the line cap by
/// [`exec::truncate_output`]. The file itself stays uncapped on disk, so `tail` still shows everything.
pub fn notice_text(done: &JobDone) -> String {
    let head = notice_headline(done);
    let output = exec::read_capped(&done.output_path).unwrap_or_default();
    if output.trim().is_empty() {
        return head;
    }
    format!("{head}\n{}", exec::truncate_output(&output))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{JobDone, notice_headline, notice_text};

    fn done() -> JobDone {
        JobDone {
            id: "b1".to_owned(),
            command: "make test".to_owned(),
            exit: Some(0),
            timed_out: false,
            killed: false,
            elapsed: Duration::from_secs(42),
            output_path: std::path::PathBuf::from("/nope/b1.log"),
        }
    }

    // The four headline shapes the notice contract pins.
    #[test]
    fn headline_shapes() {
        assert_eq!(
            notice_headline(&done()),
            "[background job b1 finished: exit 0 after 42s] make test"
        );
        let failed = JobDone {
            exit: Some(1),
            ..done()
        };
        assert_eq!(
            notice_headline(&failed),
            "[background job b1 finished: exit 1 after 42s] make test"
        );
        let slow = JobDone {
            exit: None,
            timed_out: true,
            elapsed: Duration::from_secs(600),
            ..done()
        };
        assert_eq!(
            notice_headline(&slow),
            "[background job b1 finished: timed out after 10m 0s] make test"
        );
        let killed = JobDone {
            exit: None,
            killed: true,
            ..done()
        };
        assert_eq!(
            notice_headline(&killed),
            "[background job b1 finished: killed] make test"
        );
        // A child that ended without a status of its own reads as killed too, never as "exit ?".
        let gone = JobDone {
            exit: None,
            ..done()
        };
        assert_eq!(
            notice_headline(&gone),
            "[background job b1 finished: killed] make test"
        );
    }

    /// The notice's duration is the UI's compact style, not Go's nanosecond string: a job that
    /// ran 6.009 s says `6s`, one that ran 72 s says `1m 12s` (the group summary and the status
    /// row say the same).
    #[test]
    fn notice_headline_rounds_like_every_other_timer() {
        let mut done = done();
        done.elapsed = Duration::from_millis(6_009);
        assert_eq!(
            notice_headline(&done),
            "[background job b1 finished: exit 0 after 6s] make test"
        );
        done.elapsed = Duration::from_secs(72);
        assert_eq!(
            notice_headline(&done),
            "[background job b1 finished: exit 0 after 1m 12s] make test"
        );
    }

    /// The headline's command is the header's label, not the command: a multi-line script is its
    /// first line and ` …`, a long line is cut at 64 runes — the user reads at the end what the
    /// `[shell …]` row showed at the start, and the full text is on the `/jobs` detail page.
    #[test]
    fn notice_headline_cuts_the_command_like_the_header() {
        let script = JobDone {
            command: "npm run build\nnpm test\nnpm publish".to_owned(),
            ..done()
        };
        assert_eq!(
            notice_headline(&script),
            "[background job b1 finished: exit 0 after 42s] npm run build …"
        );
        let long = JobDone {
            command: "x".repeat(100),
            ..done()
        };
        let head = notice_headline(&long);
        assert_eq!(
            head,
            format!(
                "[background job b1 finished: exit 0 after 42s] {}…",
                "x".repeat(63)
            )
        );
        assert_eq!(
            head,
            format!(
                "[background job b1 finished: exit 0 after 42s] {}",
                crate::text::header_command(&long.command)
            )
        );
    }

    // A missing (or empty) log leaves the headline alone; a real one is appended under the foreground caps.
    #[test]
    fn notice_body_is_the_capped_output() {
        assert_eq!(notice_text(&done()), notice_headline(&done()));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("b1.log");
        std::fs::write(&path, "hello\nworld\n").expect("write");
        let with_output = JobDone {
            output_path: path.clone(),
            ..done()
        };
        assert_eq!(
            notice_text(&with_output),
            format!("{}\nhello\nworld\n", notice_headline(&done()))
        );

        // Whitespace only counts as no output.
        std::fs::write(&path, "  \n\n").expect("write");
        assert_eq!(notice_text(&with_output), notice_headline(&done()));

        // 2000 lines: the line cap fires with the foreground marker, head 128 + tail 384.
        let many = (0..2000).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "line {i}");
            acc
        });
        std::fs::write(&path, &many).expect("write");
        let text = notice_text(&with_output);
        assert!(
            text.contains("lines omitted — pipe through head/tail/grep to narrow the output"),
            "the line cap did not fire:\n{}",
            &text[..text.len().min(400)]
        );
        assert!(text.contains("line 0\n") && text.contains("line 1999"));
        assert!(!text.contains("line 500\n"), "the middle must be elided");

        // A log far past the byte cap: the head and the tail survive, the middle is a marker, and the
        // reader never held more than the cap (one long line, so the LINE cap cannot fire first).
        let big = format!("HEAD{}TAIL", "x".repeat(200 * 1024));
        std::fs::write(&path, &big).expect("write");
        let text = notice_text(&with_output);
        assert!(text.contains("\nHEADxxx"), "the head is missing");
        assert!(text.ends_with("xxxTAIL"), "the tail is missing");
        assert!(text.contains("bytes omitted ...]"), "no omission marker");
        assert!(
            text.len() < 64 * 1024,
            "the notice grew to {} bytes",
            text.len()
        );
    }
}
