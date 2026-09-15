//! Process execution (internal/shell/shell.go + `proc_unix.go`): one interpreter child in its own process
//! group, combined stdout/stderr, cancellation and timeout by `killpg(SIGKILL)`, then the output caps.
//!
//! WHICH interpreter is `crate::shell::interp`'s question, asked here once per call the way Go asked
//! `exec.LookPath` once per call: `bash -c` on Unix, and on Windows the first of Git Bash, PowerShell and
//! `cmd.exe` that the machine has. Everything below is the same code either way — an interpreter is a
//! program plus the arguments that precede the script.
//!
//! The three stages are separate so a background job (`crate::shell::jobs`) can reuse the first two without
//! the third: [`spawn`] starts the child (sandbox, working directory, `setpgid`, one destination for fd 1 and
//! fd 2), [`Started::wait`] supervises it (deadline, cancellation, `killpg`, the bounded reap), and only the
//! in-memory [`Capture::Memory`] destination collects output at all. [`run`] is those three in a row — the
//! foreground call, unchanged.
//!
//! Two of those stages are the OS's, not ours, so they are the only things this module forks by platform
//! (`Child`, `spawn_supervised`, `Started::kill_tree`, `kill_group`): Unix keeps `setpgid` +
//! `killpg(SIGKILL)` verbatim, Windows gets the twin primitive — a Job Object, whose `TerminateJobObject`
//! kills the whole tree — through `process-wrap`. Everything else (the caps, the single combined pipe, the
//! deadline, the bounded reap) is the same code on both, because `std::io::pipe` is.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

#[cfg(unix)]
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// The supervised child: a plain tokio child in its own process group on Unix, and on Windows one wrapped in
/// a Job Object by `process-wrap`. Both answer `id`, `wait` and `start_kill` the same way, so only the
/// functions named in the module doc ever have to know which is which.
#[cfg(unix)]
type Child = tokio::process::Child;
/// The supervised child — see the Unix twin above.
#[cfg(windows)]
type Child = Box<dyn process_wrap::tokio::ChildWrapper>;

/// Byte cap of the captured output.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// Bytes kept from the head when the byte cap trips.
pub const HEAD_BYTES: usize = 8 * 1024;
/// Bytes kept from the tail when the byte cap trips.
pub const TAIL_BYTES: usize = 22 * 1024;
/// Line cap of the returned output.
pub const MAX_OUTPUT_LINES: usize = 512;
/// Lines kept from the head when the line cap trips.
pub(crate) const HEAD_LINES: usize = 128;
/// Lines kept from the tail when the line cap trips.
pub(crate) const TAIL_LINES: usize = 384;
/// How long to wait for the pipe to drain after the child exits (Go `WaitDelay`).
pub(crate) const WAIT_DELAY: Duration = Duration::from_secs(3);

/// Size of one read from the child's pipe.
const READ_CHUNK: usize = 8 * 1024;

/// The sandbox a command runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sandbox {
    /// Project root (always writable).
    pub root: PathBuf,
    /// Whether the network is reachable.
    pub network: bool,
    /// Extra writable roots.
    pub write: Vec<PathBuf>,
    /// Injected `os.TempDir()`.
    pub temp_dir: PathBuf,
    /// Injected `os.UserCacheDir()`.
    pub cache_dir: Option<PathBuf>,
}

/// One execution request.
#[derive(Clone, Debug)]
pub struct Options {
    /// The script handed to the interpreter.
    pub command: String,
    /// Working directory.
    pub dir: PathBuf,
    /// Wall-clock deadline.
    pub timeout: Option<Duration>,
    /// Sandbox, when the command is confined.
    pub sandbox: Option<Sandbox>,
}

/// Why a command could not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    /// No interpreter could be resolved (`crate::shell::interp`).
    #[error("{0}")]
    NoShell(#[from] super::interp::NoShell),
    /// The sandbox wrapper could not be built.
    #[error("failed to prepare the sandbox: {0}")]
    Sandbox(#[from] super::sandbox::SandboxError),
    /// Spawn failed; the `io::Error` text (differs from Go's `chdir …` — DIVERGENCES D-16).
    #[error("{0}")]
    Spawn(String),
}

/// What a foreground run produced: the capped, combined stdout/stderr and how the child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// Combined, capped stdout/stderr.
    pub output: String,
    /// How it ended.
    pub outcome: Outcome,
}

impl RunResult {
    /// A result carrying nothing but the outcome (shell.go:71,84 — no output was produced).
    fn without_output(outcome: Outcome) -> Self {
        Self {
            output: String::new(),
            outcome,
        }
    }
}

/// Where a child's combined stdout/stderr goes.
pub enum Capture {
    /// A capped in-memory buffer, read back by [`Started::into_output`] — the foreground `bash` call.
    Memory,
    /// Appended straight to this file, UNCAPPED — a background job's log, which the reader caps when it
    /// reads it back.
    File(std::fs::File),
}

/// Why [`spawn`] produced no child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnFail {
    /// The token was already done: Go never spawns under a cancelled context (its `Start` returns
    /// `ctx.Err()` and the run reports Cancelled).
    Cancelled,
    /// The command could not be started.
    Failed(ShellError),
}

/// A started child: the handle to wait on, its process-group leader, and the in-memory sink when the
/// output is captured.
pub struct Started {
    child: Child,
    /// The group leader — what `kill_group` signals. Public so a supervisor outside this module (the job
    /// registry) can kill the tree without awaiting anything. On Windows it names the child but cannot reach
    /// its tree: see `kill_group`.
    pub pid: Option<i32>,
    reader: Option<tokio::task::JoinHandle<()>>,
    buf: Option<Arc<Mutex<CappedBuffer>>>,
}

/// How a child ended (shell.go:97-121). One thing at a time, by construction: the deadline wins over
/// cancellation, both win over the exit status, and a child that never started has no status at all. Until
/// 2026-09-15 this was four bools and an `Option` (`Waited`, and the same five fields again on `RunResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The child ended on its own; -1 on signal death (Go's `ExitError.ExitCode()`).
    Exited(i32),
    /// The deadline killed it.
    TimedOut,
    /// The token killed it.
    Cancelled,
    /// It could not be started (no interpreter, the sandbox, the spawn itself), or `wait` failed.
    Failed(ShellError),
}

/// Starts `opts.command` under the resolved interpreter, in its own process group, with fd 1 and fd 2 joined
/// into `capture` (shell.go:69-96). The order of the refusals is Go's: no interpreter → sandbox → a done
/// token → the spawn itself.
pub fn spawn(
    cancel: &CancellationToken,
    opts: &Options,
    capture: Capture,
) -> Result<Started, SpawnFail> {
    let fail = |e: ShellError| SpawnFail::Failed(e);
    // The interpreter is resolved per call, like Go's exec.LookPath (shell.go:69-72).
    let shell = match super::interp::resolve() {
        Ok(s) => s,
        Err(e) => return Err(fail(ShellError::NoShell(e))),
    };
    let mut cmd = if let Some(sb) = &opts.sandbox {
        match super::sandbox::command(&shell, &opts.command, &writable_paths(sb), sb.network) {
            Ok(c) => c,
            Err(e) => return Err(fail(ShellError::Sandbox(e))),
        }
    } else {
        let mut c = tokio::process::Command::new(&shell.program);
        c.args(shell.args()).arg(&opts.command);
        c
    };
    // Go never spawns under a done context: Start returns ctx.Err() and the run reports Cancelled.
    if cancel.is_cancelled() {
        return Err(SpawnFail::Cancelled);
    }
    if !opts.dir.as_os_str().is_empty() {
        cmd.current_dir(&opts.dir);
        // Go's os/exec appends PWD=<abs Dir> when Dir is set and Env is nil; Rust does not. Not on Windows:
        // there PWD means something only to the POSIX shell that may be running, and what we would hand it
        // is a `C:\...` path — a spelling whose separators that shell reads as escapes.
        if !cfg!(windows)
            && let Ok(abs) = std::path::absolute(&opts.dir)
        {
            cmd.env("PWD", abs);
        }
    }
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(false);

    // ONE destination for fd 1 and fd 2, like Go's shared cappedBuffer: interleaving is preserved in write
    // order either way. `std::io::pipe` is the portable spelling of what `nix::unistd::pipe` gave us — the
    // same `pipe(2)`, now with `O_CLOEXEC`, so the read end no longer leaks into the child as a stray fd.
    let sink = match capture {
        Capture::File(file) => {
            let dup = file
                .try_clone()
                .map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            cmd.stdout(Stdio::from(file));
            cmd.stderr(Stdio::from(dup));
            None
        }
        Capture::Memory => {
            let (rx, tx) = std::io::pipe().map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            let tx_dup = tx
                .try_clone()
                .map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            cmd.stdout(Stdio::from(tx));
            cmd.stderr(Stdio::from(tx_dup));
            Some(rx)
        }
    };

    // The reader is armed BEFORE the child exists, as it was when the pipe end went straight to the
    // reactor: a failure here must not leave a running child nobody reaps. It cannot hang waiting for a
    // spawn that never happens either — `cmd` still owns the parent's copies of the write end, and
    // dropping it (which `spawn_supervised` does, success or failure, by taking it BY VALUE) is exactly
    // what lets this task see EOF.
    let (reader, buf) = match sink {
        None => (None, None),
        Some(rx) => {
            let buf = Arc::new(Mutex::new(CappedBuffer::default()));
            let task = start_drain(rx, Arc::clone(&buf))
                .map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            (Some(task), Some(buf))
        }
    };
    let child = spawn_supervised(cmd).map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
    let pid = child.id().and_then(|p| i32::try_from(p).ok());
    Ok(Started {
        child,
        pid,
        reader,
        buf,
    })
}

impl Started {
    /// Supervises the child to its end (shell.go:97-121): the deadline and the token race `wait()`, either
    /// one `killpg`s the group and reaps it under Go's `WaitDelay`, and a capture reader is given the same
    /// bounded window to drain — a background grandchild holding the pipe can never wedge the caller.
    pub async fn wait(&mut self, cancel: &CancellationToken, timeout: Option<Duration>) -> Outcome {
        let waited = tokio::select! {
            s = self.child.wait() => Ok(s),
            () = cancel.cancelled() => Err(Outcome::Cancelled),
            () = deadline(timeout) => Err(Outcome::TimedOut),
        };
        let outcome = match waited {
            // ExitStatus::code() is None on signal death — Go's ExitError.ExitCode() reports -1 there.
            Ok(Ok(st)) => Outcome::Exited(st.code().unwrap_or(-1)),
            Ok(Err(e)) => Outcome::Failed(ShellError::Spawn(e.to_string())),
            Err(killed) => {
                // Cancelled or timed out: SIGKILL the group, then bound the reap like Go's WaitDelay.
                self.kill_tree();
                if tokio::time::timeout(WAIT_DELAY, self.child.wait())
                    .await
                    .is_err()
                {
                    let _ = self.child.start_kill();
                }
                killed
            }
        };
        if let Some(reader) = &mut self.reader
            && tokio::time::timeout(WAIT_DELAY, &mut *reader)
                .await
                .is_err()
        {
            reader.abort();
        }
        outcome
    }

    /// Kills the child AND everything it started: `killpg(SIGKILL)` on Unix, `TerminateJobObject` on
    /// Windows. Both make the same promise — no descendant survives the call — through the platform's own
    /// tree primitive.
    fn kill_tree(&mut self) {
        #[cfg(unix)]
        kill_group(self.pid);
        #[cfg(windows)]
        if let Err(e) = self.child.start_kill() {
            tracing::debug!("TerminateJobObject failed: {e}");
        }
    }

    /// Everything the in-memory capture collected ([`Capture::File`] collects nothing here — the file has it).
    pub fn into_output(self) -> String {
        self.buf.map_or_else(String::new, |buf| {
            std::mem::take(&mut *lock(&buf)).into_string()
        })
    }
}

/// The single foreground entry point: [`spawn`] with [`Capture::Memory`], [`Started::wait`], then the output
/// caps. Order after the child finishes (shell.go:97-121): `timed_out` (deadline) → cancelled (token) →
/// wait-delay expiry (exited, code) → Ok(0) → nonzero/signal (-1) → spawn error.
pub async fn run(cancel: &CancellationToken, opts: Options) -> RunResult {
    let mut started = match spawn(cancel, &opts, Capture::Memory) {
        Ok(s) => s,
        Err(SpawnFail::Cancelled) => return RunResult::without_output(Outcome::Cancelled),
        Err(SpawnFail::Failed(e)) => return RunResult::without_output(Outcome::Failed(e)),
    };
    let outcome = started.wait(cancel, opts.timeout).await;
    RunResult {
        output: truncate_output(&started.into_output()),
        outcome,
    }
}

/// Fires after `d`, or never when the run has no deadline.
async fn deadline(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Starts the child in its own process group (`setpgid`, `proc_unix.go:21`), so cancellation kills the whole
/// tree and not just the `bash` wrapper. Takes the command by value: its copies of the pipe's write end must
/// be gone before the reader can ever see EOF.
#[cfg(unix)]
fn spawn_supervised(mut cmd: tokio::process::Command) -> std::io::Result<Child> {
    cmd.process_group(0);
    cmd.spawn()
}

/// The Windows twin of `setpgid`: the child is assigned to a Job Object, which owns every process it goes on
/// to start, so one `TerminateJobObject` ends the tree. `CREATE_NO_WINDOW` rides along because a console
/// child spawned by a parent that has no console otherwise flashes — and steals focus with — a console window
/// per command (goose#6701); it goes through `process-wrap` rather than `Command::creation_flags` because the
/// Job Object wrapper writes that same field itself and would overwrite a flag set behind its back.
#[cfg(windows)]
fn spawn_supervised(cmd: tokio::process::Command) -> std::io::Result<Child> {
    use process_wrap::tokio::{CommandWrap, CreationFlags, JobObject};
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    let mut wrap = CommandWrap::from(cmd);
    wrap.wrap(JobObject);
    wrap.wrap(CreationFlags(CREATE_NO_WINDOW));
    wrap.spawn()
}

/// Puts the pipe's read end on a task that streams it into the capped buffer — one the caller can bound and
/// abandon.
#[cfg(unix)]
fn start_drain(
    rx: std::io::PipeReader,
    buf: Arc<Mutex<CappedBuffer>>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let rx = tokio::net::unix::pipe::Receiver::from_owned_fd(std::os::fd::OwnedFd::from(rx))?;
    Ok(tokio::spawn(drain(rx, buf)))
}

/// Windows has no reactor registration for an anonymous pipe, so the same drain is a blocking read on the
/// blocking pool. The difference is the abandonment case: [`Started::wait`] still stops WAITING for this task
/// after `WAIT_DELAY`, but `abort` cannot interrupt a blocking read, so the thread stays parked until the
/// last write end closes. Bounded (one per in-flight command) and never in the caller's way.
///
/// The `Result` is the Unix twin's, kept so the call site is one line on both platforms; nothing here fails.
#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)]
fn start_drain(
    rx: std::io::PipeReader,
    buf: Arc<Mutex<CappedBuffer>>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    Ok(tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut rx = rx;
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            match rx.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => lock(&buf).write(&chunk[..n]),
            }
        }
    }))
}

/// Streams the child's combined output into the capped buffer until EOF (or a read error).
#[cfg(unix)]
async fn drain(mut rx: tokio::net::unix::pipe::Receiver, buf: Arc<Mutex<CappedBuffer>>) {
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        match rx.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => lock(&buf).write(&chunk[..n]),
        }
    }
}

/// The capped buffer is only ever appended to, so a poisoned lock still holds usable output.
fn lock(buf: &Mutex<CappedBuffer>) -> MutexGuard<'_, CappedBuffer> {
    crate::sync::lock(buf)
}

/// `kill(-pid, SIGKILL)` (proc_unix.go:22-28): the whole group dies, and an already-gone group (`ESRCH`) is
/// success.
#[cfg(unix)]
pub(crate) fn kill_group(pid: Option<i32>) {
    let Some(pid) = pid else { return };
    match nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(e) => tracing::debug!("killpg({pid}) failed: {e}"),
    }
}

/// Nothing: a Windows process tree is addressed by its Job Object handle, which only the [`Started`] that
/// spawned it holds, so a pid alone cannot reach it. The callers that pass one are the synchronous exit paths
/// (`crate::shell::jobs::Jobs::kill_all`), and they also cancel the job's token — which makes its supervisor
/// call `Started::kill_tree`, the handle-carrying route that does work. The remaining gap is one process
/// dying before its supervisors run, which the exit paths already bound: `kill_all` cancels every token and
/// the runtime drains the supervisors before the process leaves.
#[cfg(windows)]
pub(crate) fn kill_group(_pid: Option<i32>) {}

/// darwin: `/usr/bin/sandbox-exec` is a regular file; linux: `bwrap` on `PATH`; else false.
pub fn available() -> bool {
    super::sandbox::available()
}

/// `[root, temp_dir, "/tmp", cache_dir (create_dir_all attempted, error ignored), write…(absolute)]` — empties and
/// duplicates dropped, first wins.
pub fn writable_paths(sb: &Sandbox) -> Vec<PathBuf> {
    let mut paths = vec![sb.root.clone(), sb.temp_dir.clone(), PathBuf::from("/tmp")];
    if let Some(cache) = sb.cache_dir.as_ref().filter(|c| !c.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(cache); // must exist to be bind-mounted / allowed
        paths.push(cache.clone());
    }
    for p in &sb.write {
        if p.as_os_str().is_empty() {
            continue;
        }
        // Go's filepath.Abs = Clean(Join(wd, p)).
        if let Ok(abs) = std::path::absolute(p) {
            paths.push(crate::app::paths::clean(&abs));
        }
    }
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()))
        .collect()
}

/// 15-line `PATH` scan (replaces `which`).
pub(crate) fn find_in_path(name: &str) -> Option<PathBuf> {
    // A name with a directory in it is checked where it stands, never searched. `components()` is the
    // platform's own answer to "does this have a directory in it", so `C:\Program Files\Git\bin\bash.exe`
    // counts on Windows exactly as `/bin/bash` does on Unix.
    let direct = PathBuf::from(name);
    if direct.is_absolute() || direct.components().count() > 1 {
        return spellings(&direct).into_iter().find(|p| is_executable(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| {
            // Go's LookPath reads an empty PATH element as the current directory.
            let dir = if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir
            };
            spellings(&dir.join(name))
        })
        .find(|candidate| is_executable(candidate))
}

/// The one spelling of `p` a Unix `PATH` entry can have.
#[cfg(not(windows))]
fn spellings(p: &Path) -> Vec<PathBuf> {
    vec![p.to_path_buf()]
}

/// Every spelling of `p` a Windows `PATH` entry can have (Go's `lookExtensions`,
/// `os/exec/lp_windows.go`): the caller writes `bash`, the directory holds `bash.exe`, and `%PATHEXT%`
/// is what bridges the two. A name that already carries an extension is tried as written FIRST — and
/// then with the extensions anyway, so a `foo.bat.exe` still resolves.
#[cfg(windows)]
fn spellings(p: &Path) -> Vec<PathBuf> {
    // The list Go falls back to when PATHEXT is unset or empty.
    const DEFAULT_PATHEXT: &str = ".com;.exe;.bat;.cmd";
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    let pathext = if pathext.trim().is_empty() {
        DEFAULT_PATHEXT.to_owned()
    } else {
        pathext
    };
    let mut out = Vec::new();
    // `foo.bar` has an extension; `C:\dir.d\foo` does not — `Path::extension` draws that line for us.
    if p.extension().is_some() {
        out.push(p.to_path_buf());
    }
    for ext in pathext.split(';') {
        let ext = ext.trim();
        if ext.is_empty() {
            continue;
        }
        let mut spelled = p.as_os_str().to_owned();
        if !ext.starts_with('.') {
            spelled.push(".");
        }
        spelled.push(ext.to_ascii_lowercase());
        out.push(PathBuf::from(spelled));
    }
    // An extensionless PATHEXT-less name is still worth a look rather than nothing at all.
    if out.is_empty() {
        out.push(p.to_path_buf());
    }
    out
}

/// A regular file with at least one execute bit (Go's `findExecutable`).
fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Head/tail byte buffer (shell.go:163-200): keeps the first `HEAD_BYTES` and the last `TAIL_BYTES`.
#[derive(Debug, Default)]
pub struct CappedBuffer {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
}

impl CappedBuffer {
    /// Appends `p`.
    pub fn write(&mut self, p: &[u8]) {
        self.total += p.len();
        let mut rest = p;
        let room = HEAD_BYTES.saturating_sub(self.head.len());
        if room > 0 {
            let take = room.min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if !rest.is_empty() {
            self.tail.extend_from_slice(rest);
            if self.tail.len() > 2 * TAIL_BYTES {
                self.tail = self.tail[self.tail.len() - TAIL_BYTES..].to_vec();
            }
        }
    }

    /// The captured text with the marker `\n[... {n} bytes omitted ...]\n` when bytes were dropped.
    pub fn into_string(self) -> String {
        if self.total <= self.head.len() + self.tail.len() {
            let mut all = self.head;
            all.extend_from_slice(&self.tail);
            return String::from_utf8_lossy(&all).into_owned();
        }
        let head = trim_back_to_rune_start(&self.head);
        let mut tail: &[u8] = &self.tail;
        if tail.len() > TAIL_BYTES {
            tail = &tail[tail.len() - TAIL_BYTES..];
        }
        let tail = trim_front_to_rune_start(tail);
        let omitted = self.total.saturating_sub(head.len() + tail.len());
        format!(
            "{}\n[... {omitted} bytes omitted ...]\n{}",
            String::from_utf8_lossy(head),
            String::from_utf8_lossy(tail)
        )
    }
}

/// Reads `path` under the SAME byte caps a captured run gets: the whole file up to [`MAX_OUTPUT_BYTES`],
/// else its head and its tail with the omission marker between them — byte-identical to what
/// [`truncate_output`] would produce for the same content.
///
/// Two seeks, never a stream: a background job that wrote gigabytes costs the reader one open and ~30 KB,
/// so rendering its completion notice can never stall the loop that renders it.
pub fn read_capped(path: &Path) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut f = std::fs::File::open(path)?;
    let total = usize::try_from(f.metadata()?.len()).unwrap_or(usize::MAX);
    if total <= MAX_OUTPUT_BYTES {
        let mut all = Vec::with_capacity(total);
        f.read_to_end(&mut all)?;
        return Ok(String::from_utf8_lossy(&all).into_owned());
    }
    let mut head = vec![0u8; HEAD_BYTES];
    f.read_exact(&mut head)?;
    let back = i64::try_from(TAIL_BYTES).unwrap_or(i64::MAX);
    f.seek(SeekFrom::End(-back))?;
    let mut tail = vec![0u8; TAIL_BYTES];
    f.read_exact(&mut tail)?;
    let head = trim_back_to_rune_start(&head);
    let tail = trim_front_to_rune_start(&tail);
    let omitted = total.saturating_sub(head.len() + tail.len());
    Ok(format!(
        "{}\n[... {omitted} bytes omitted ...]\n{}",
        String::from_utf8_lossy(head),
        String::from_utf8_lossy(tail)
    ))
}

/// Line cap then byte cap (shell.go:204-234). Line marker
/// `\n[... {n} lines omitted — pipe through head/tail/grep to narrow the output ...]\n`.
pub fn truncate_output(s: &str) -> String {
    if s.matches('\n').count() + 1 > MAX_OUTPUT_LINES {
        let lines: Vec<&str> = s.split('\n').collect();
        let omitted = lines.len().saturating_sub(HEAD_LINES + TAIL_LINES);
        let capped = format!(
            "{}\n[... {omitted} lines omitted — pipe through head/tail/grep to narrow the output ...]\n{}",
            lines[..HEAD_LINES].join("\n"),
            lines[lines.len() - TAIL_LINES..].join("\n")
        );
        return truncate_middle(&capped);
    }
    truncate_middle(s)
}

/// shell.go:216-230: keeps the head and tail of oversized output, both cut at a rune boundary.
fn truncate_middle(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() <= MAX_OUTPUT_BYTES {
        return s.to_owned();
    }
    let head = trim_back_to_rune_start(&b[..HEAD_BYTES]);
    let tail = trim_front_to_rune_start(&b[b.len() - TAIL_BYTES..]);
    let omitted = b.len().saturating_sub(head.len() + tail.len());
    format!(
        "{}\n[... {omitted} bytes omitted ...]\n{}",
        String::from_utf8_lossy(head),
        String::from_utf8_lossy(tail)
    )
}

/// Drops trailing UTF-8 continuation bytes (`b & 0xC0 == 0x80`).
fn trim_back_to_rune_start(mut b: &[u8]) -> &[u8] {
    while let Some((last, rest)) = b.split_last() {
        if is_rune_start(*last) {
            break;
        }
        b = rest;
    }
    b
}

/// Drops leading UTF-8 continuation bytes.
fn trim_front_to_rune_start(mut b: &[u8]) -> &[u8] {
    while let Some((first, rest)) = b.split_first() {
        if is_rune_start(*first) {
            break;
        }
        b = rest;
    }
    b
}

/// Whether `b` can begin a UTF-8 sequence (shell.go:234).
const fn is_rune_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

#[cfg(test)]
mod tests {
    use super::{
        CappedBuffer, HEAD_BYTES, TAIL_BYTES, find_in_path, is_rune_start, truncate_output,
    };

    // Go: internal/shell/shell_test.go:191 — the memory bound needs the private head/tail lengths; the
    // observable half of TestCappedBuffer is `test_capped_buffer` in tests/shell.rs.
    #[test]
    fn capped_buffer_memory_bound() {
        let mut b = CappedBuffer::default();
        b.write(b"start-");
        let chunk = vec![b'x'; 8 * 1024];
        for _ in 0..40 {
            // ~320KB through a ~52KB window
            b.write(&chunk);
        }
        b.write(b"-end");
        let cap = HEAD_BYTES + 2 * TAIL_BYTES + 16 * 1024;
        assert!(
            b.head.len() + b.tail.len() <= cap,
            "buffer grew to {} bytes, want ≤ {cap}",
            b.head.len() + b.tail.len()
        );
        assert_eq!(b.total, 6 + 40 * 8 * 1024 + 4);
        assert_eq!(b.head.len(), HEAD_BYTES);
    }

    /// The byte cap, spelled once for the boundary test.
    const MAX: usize = super::MAX_OUTPUT_BYTES;

    // New: the byte cap cuts on the Go rule — trim back while the last byte is a CONTINUATION byte
    // (shell.go:221-227), which stops on the lead byte of a split character; the lossy conversion then marks
    // that dangling byte (Go keeps it raw). The tail is trimmed forward the same way.
    #[test]
    fn truncation_keeps_char_boundaries() {
        let s = "é".repeat(MAX); // 2-byte characters: the 8192-byte cut lands on a continuation byte
        let out = truncate_output(&s);
        let head = out.split("\n[... ").next().expect("head");
        assert_eq!(head.chars().filter(|c| *c == 'é').count(), 4095);
        assert!(
            head.ends_with('\u{fffd}'),
            "the split character's lead byte is kept, like Go"
        );
        let tail = out.rsplit(" ...]\n").next().expect("tail");
        assert!(tail.starts_with('é') && tail.ends_with('é'));
        assert_eq!(tail.len(), TAIL_BYTES);
        assert!(out.contains("bytes omitted"));
        assert!(is_rune_start(b'a') && !is_rune_start(0x80));

        // Anything at or below the cap passes through untouched.
        let small = "é".repeat(MAX / 2);
        assert_eq!(truncate_output(&small), small);
    }

    // New: the PATH scan finds a real binary and rejects a name that is not one.
    #[test]
    fn find_in_path_resolves_the_shell() {
        // Windows asks for `cmd`, which is also the PATHEXT assertion: `PATH` carries `cmd.exe` and
        // never `cmd`, so a scan that did not complete the extension would find nothing.
        let name = if cfg!(windows) { "cmd" } else { "bash" };
        let found = find_in_path(name).expect("the platform shell must exist for the shell tests");
        assert!(found.is_absolute() || found.starts_with("."), "{found:?}");
        if cfg!(windows) {
            assert_eq!(
                found.extension().map(std::ffi::OsStr::to_ascii_lowercase),
                Some("exe".into()),
                "PATHEXT was not applied: {found:?}"
            );
        }
        assert!(find_in_path("iota-definitely-not-a-binary").is_none());
        assert!(find_in_path("/definitely/not/a/binary").is_none());
    }
}
