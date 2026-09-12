//! Background `shell` jobs: the run's registry of children started with `background: true`, and the
//! completion notice each one produces.
//!
//! A job is an ordinary [`exec::spawn`] child — same sandbox, same `setpgid`, same `killpg` deadline — with
//! its combined output going to a file instead of the round's 32 KB buffer, so the model's turn ends while
//! the work continues. When it finishes, a [`JobDone`] is DELIVERED (interactive: the installed sink pushes
//! it at the UI, which enqueues it as the next input) or PARKED (headless: the loop drains
//! [`Jobs::take_finished`] each round and blocks on [`Jobs::wait_any`] when it has nothing else to do).
//!
//! Nothing here outlives the process: [`Jobs::kill_all`] is synchronous `killpg` precisely so a `/quit` or a
//! failed headless run cannot leave a tree behind, and a resumed session therefore never sees a job it
//! started last time (documented in the README).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

use crate::shell::exec::{self, Capture, Options, SpawnFail};
use crate::text::go_duration;

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
    /// Wall-clock time from spawn to end.
    pub elapsed: Duration,
    /// The log file, still on disk (the model can `tail` it).
    pub output_path: PathBuf,
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

/// One running job, as the registry tracks it.
struct Running {
    /// The group leader, for a synchronous `killpg`.
    pid: Option<i32>,
    /// Fires to make the supervisor kill and reap it.
    cancel: CancellationToken,
}

/// The registry's mutable half.
#[derive(Default)]
struct State {
    /// Next id number (`b1` is the first).
    next: u32,
    /// Jobs in flight, by id.
    running: BTreeMap<String, Running>,
    /// Completed jobs nobody has taken yet — always empty while a sink is installed.
    finished: Vec<JobDone>,
    /// The interactive delivery seam.
    sink: Option<Arc<JobSink>>,
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

    /// Starts one background job. The child is [`exec::spawn`]ed exactly as a foreground call is — same
    /// sandbox, same working directory, same `timeout` — but its output goes to a file and a detached task
    /// supervises it.
    pub fn spawn(self: &Arc<Self>, opts: &Options) -> Result<JobStart, JobError> {
        // The slot is CLAIMED under the same lock that checks the cap, so two `shell` calls in one parallel
        // batch cannot both read 15 and both start.
        let (id, cancel) = {
            let mut st = self.lock();
            if st.running.len() >= MAX_JOBS {
                return Err(JobError::TooMany);
            }
            st.next += 1;
            let id = format!("b{}", st.next);
            let cancel = CancellationToken::new();
            st.running.insert(
                id.clone(),
                Running {
                    pid: None,
                    cancel: cancel.clone(),
                },
            );
            (id, cancel)
        };
        let output_path = self.dir.join(format!("{id}.log"));
        let started = match self.start_child(&cancel, opts, &output_path) {
            Ok(started) => started,
            Err(e) => {
                // A slot claimed by a job that never started is a slot leaked forever.
                self.lock().running.remove(&id);
                return Err(e);
            }
        };
        let pid = started.pid;
        if let Some(job) = self.lock().running.get_mut(&id) {
            job.pid = pid;
        }
        let done = JobDone {
            id: id.clone(),
            command: opts.command.clone(),
            exit: None,
            timed_out: false,
            killed: false,
            elapsed: Duration::ZERO,
            output_path: output_path.clone(),
        };
        tokio::spawn(supervise(
            Arc::clone(self),
            started,
            cancel,
            opts.timeout,
            done,
        ));
        Ok(JobStart {
            id,
            pid,
            output_path,
        })
    }

    /// The log file and the child behind one job — everything between claiming a slot and owning a process.
    fn start_child(
        &self,
        cancel: &CancellationToken,
        opts: &Options,
        output_path: &Path,
    ) -> Result<exec::Started, JobError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| JobError::Output(e.to_string()))?;
        let file =
            std::fs::File::create(output_path).map_err(|e| JobError::Output(e.to_string()))?;
        exec::spawn(cancel, opts, Capture::File(file)).map_err(|e| match e {
            SpawnFail::Cancelled => JobError::Spawn("the run was cancelled".to_owned()),
            SpawnFail::Failed(e) => JobError::Spawn(e.to_string()),
        })
    }

    /// How many jobs are still in flight.
    pub fn running(&self) -> usize {
        self.lock().running.len()
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

    /// Kills every job still running, synchronously — the exit path, which cannot await. The sink is
    /// dropped with them: nobody is left to deliver a notice to.
    pub fn kill_all(&self) {
        let mut st = self.lock();
        st.sink = None;
        for (_, job) in std::mem::take(&mut st.running) {
            job.cancel.cancel();
            exec::kill_group(job.pid);
        }
        drop(st);
        self.done.notify_waiters();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Supervises one job to its end, then delivers or parks the [`JobDone`].
async fn supervise(
    jobs: Arc<Jobs>,
    mut started: exec::Started,
    cancel: CancellationToken,
    timeout: Option<Duration>,
    mut done: JobDone,
) {
    let at = Instant::now();
    let w = started.wait(&cancel, timeout).await;
    done.elapsed = at.elapsed();
    done.timed_out = w.timed_out;
    done.killed = w.cancelled;
    done.exit = w.exited.then_some(w.exit_code);
    let sink = {
        let mut st = jobs.lock();
        st.running.remove(&done.id);
        let sink = st.sink.clone();
        if sink.is_none() {
            st.finished.push(done.clone());
        }
        sink
    };
    // The waiter is woken either way: a delivered job still changes `running()`, which is what ends a
    // headless wait that has nothing left to wait for.
    jobs.done.notify_waiters();
    if let Some(sink) = sink {
        sink(done);
    }
}

/// The notice's first line: what happened, in a fixed shape the model can pattern-match.
pub fn notice_headline(done: &JobDone) -> String {
    let status = if done.timed_out {
        format!("timed out after {}", go_duration(done.elapsed))
    } else if done.killed {
        "killed".to_owned()
    } else {
        match done.exit {
            Some(code) => format!("exit {code} after {}", go_duration(done.elapsed)),
            None => "killed".to_owned(),
        }
    };
    format!(
        "[background job {} finished: {status}] {}",
        done.id, done.command
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
            "[background job b1 finished: timed out after 10m0s] make test"
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
