//! Process execution (internal/shell/shell.go + `proc_unix.go`): one `bash -c` child in its own process group,
//! combined stdout/stderr, cancellation and timeout by `killpg(SIGKILL)`, then the output caps.
//!
//! The three stages are separate so a background job (`crate::shell::jobs`) can reuse the first two without
//! the third: [`spawn`] starts the child (sandbox, working directory, `setpgid`, one destination for fd 1 and
//! fd 2), [`Started::wait`] supervises it (deadline, cancellation, `killpg`, the bounded reap), and only the
//! in-memory [`Capture::Memory`] destination collects output at all. [`run`] is those three in a row — the
//! foreground `bash` call, unchanged.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

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
    /// The `bash -c` script.
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
    /// `bash` is not on `PATH`.
    #[error("bash is not installed on this system")]
    NoBash,
    /// The sandbox wrapper could not be built.
    #[error("failed to prepare the sandbox: {0}")]
    Sandbox(String),
    /// Spawn failed; the `io::Error` text (differs from Go's `chdir …` — DIVERGENCES D-16).
    #[error("{0}")]
    Spawn(String),
}

/// What a run produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// Combined, capped stdout/stderr.
    pub output: String,
    /// Exit code (-1 for a signal).
    pub exit_code: i32,
    /// Whether the child exited on its own.
    pub exited: bool,
    /// Whether the deadline killed it.
    pub timed_out: bool,
    /// Whether cancellation killed it.
    pub cancelled: bool,
    /// The spawn/sandbox failure, if any.
    pub err: Option<ShellError>,
}

impl RunResult {
    /// A result carrying nothing but the failure (shell.go:71,84 — no output was produced).
    fn failed(err: ShellError) -> Self {
        Self {
            err: Some(err),
            ..Self::default()
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
    child: tokio::process::Child,
    /// The group leader — what `kill_group` signals. Public so a supervisor outside this module (the job
    /// registry) can kill the tree without awaiting anything.
    pub pid: Option<i32>,
    reader: Option<tokio::task::JoinHandle<()>>,
    buf: Option<Arc<Mutex<CappedBuffer>>>,
}

/// What supervising a started child observed. Mutually exclusive by construction: the deadline wins over
/// cancellation, both win over the exit status.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Waited {
    /// Exit code (-1 on signal death); meaningful only with `exited`.
    pub exit_code: i32,
    /// Whether the child ended on its own.
    pub exited: bool,
    /// Whether the deadline killed it.
    pub timed_out: bool,
    /// Whether the token killed it.
    pub cancelled: bool,
    /// A `wait` failure.
    pub err: Option<ShellError>,
}

/// Starts `bash -c opts.command` in its own process group with fd 1 and fd 2 joined into `capture`
/// (shell.go:69-96). The order of the refusals is Go's: no bash → sandbox → a done token → the spawn itself.
pub fn spawn(
    cancel: &CancellationToken,
    opts: &Options,
    capture: Capture,
) -> Result<Started, SpawnFail> {
    let fail = |e: ShellError| SpawnFail::Failed(e);
    // bash is resolved per call, like Go's exec.LookPath (shell.go:69-72).
    let Some(bash) = find_in_path("bash") else {
        return Err(fail(ShellError::NoBash));
    };
    let mut cmd = if let Some(sb) = &opts.sandbox {
        match sandbox_command(&bash, &opts.command, &writable_paths(sb), sb.network) {
            Ok(c) => c,
            Err(e) => return Err(fail(ShellError::Sandbox(e))),
        }
    } else {
        let mut c = tokio::process::Command::new(&bash);
        c.arg("-c").arg(&opts.command);
        c
    };
    // Go never spawns under a done context: Start returns ctx.Err() and the run reports Cancelled.
    if cancel.is_cancelled() {
        return Err(SpawnFail::Cancelled);
    }
    if !opts.dir.as_os_str().is_empty() {
        cmd.current_dir(&opts.dir);
        // Go's os/exec appends PWD=<abs Dir> when Dir is set and Env is nil; Rust does not.
        if let Ok(abs) = std::path::absolute(&opts.dir) {
            cmd.env("PWD", abs);
        }
    }
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(false);
    // Setpgid: cancellation kills the whole tree, not just the wrapper (proc_unix.go:21).
    cmd.process_group(0);

    // ONE destination for fd 1 and fd 2, like Go's shared cappedBuffer: interleaving is preserved in write
    // order either way.
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
            let (rx_fd, tx_fd) =
                nix::unistd::pipe().map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            let tx_dup = tx_fd
                .try_clone()
                .map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            cmd.stdout(Stdio::from(tx_fd));
            cmd.stderr(Stdio::from(tx_dup));
            let rx = tokio::net::unix::pipe::Receiver::from_owned_fd(rx_fd)
                .map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
            Some(rx)
        }
    };

    let spawned = cmd.spawn();
    // The command still owns the parent's copies of the write end; dropping it lets the reader see EOF.
    drop(cmd);
    let child = spawned.map_err(|e| fail(ShellError::Spawn(e.to_string())))?;
    let pid = child.id().and_then(|p| i32::try_from(p).ok());
    let (reader, buf) = match sink {
        None => (None, None),
        Some(rx) => {
            let buf = Arc::new(Mutex::new(CappedBuffer::default()));
            let task = tokio::spawn(drain(rx, Arc::clone(&buf)));
            (Some(task), Some(buf))
        }
    };
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
    pub async fn wait(&mut self, cancel: &CancellationToken, timeout: Option<Duration>) -> Waited {
        let mut w = Waited::default();
        let finished = tokio::select! {
            s = self.child.wait() => Some(s),
            () = cancel.cancelled() => { w.cancelled = true; None },
            () = deadline(timeout) => { w.timed_out = true; None },
        };
        let finished = if finished.is_some() {
            finished
        } else {
            // Cancelled or timed out: SIGKILL the group, then bound the reap like Go's WaitDelay.
            kill_group(self.pid);
            let reaped = tokio::time::timeout(WAIT_DELAY, self.child.wait())
                .await
                .ok();
            if reaped.is_none() {
                let _ = self.child.start_kill();
            }
            reaped
        };
        if let Some(reader) = &mut self.reader
            && tokio::time::timeout(WAIT_DELAY, &mut *reader)
                .await
                .is_err()
        {
            reader.abort();
        }
        if w.timed_out || w.cancelled {
            return w;
        }
        match finished {
            // ExitStatus::code() is None on signal death — Go's ExitError.ExitCode() reports -1 there.
            Some(Ok(st)) => {
                w.exited = true;
                w.exit_code = st.code().unwrap_or(-1);
            }
            Some(Err(e)) => w.err = Some(ShellError::Spawn(e.to_string())),
            None => w.exited = true,
        }
        w
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
        Err(SpawnFail::Cancelled) => {
            return RunResult {
                cancelled: true,
                ..RunResult::default()
            };
        }
        Err(SpawnFail::Failed(e)) => return RunResult::failed(e),
    };
    let w = started.wait(cancel, opts.timeout).await;
    RunResult {
        output: truncate_output(&started.into_output()),
        exit_code: w.exit_code,
        exited: w.exited,
        timed_out: w.timed_out,
        cancelled: w.cancelled,
        err: w.err,
    }
}

/// Fires after `d`, or never when the run has no deadline.
async fn deadline(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Streams the child's combined output into the capped buffer until EOF (or a read error).
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
    buf.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `kill(-pid, SIGKILL)` (proc_unix.go:22-28): the whole group dies, and an already-gone group (`ESRCH`) is
/// success.
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

/// The platform sandbox wrapper for one command.
fn sandbox_command(
    bash: &Path,
    script: &str,
    writable: &[PathBuf],
    network: bool,
) -> Result<tokio::process::Command, String> {
    #[cfg(target_os = "macos")]
    {
        super::sandbox_darwin::command(bash, script, writable, network)
    }
    #[cfg(target_os = "linux")]
    {
        super::sandbox_linux::command(bash, script, writable, network)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        super::sandbox_other::command(bash, script, writable, network)
    }
}

/// darwin: `/usr/bin/sandbox-exec` is a regular file; linux: `bwrap` on `PATH`; else false.
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        super::sandbox_darwin::available()
    }
    #[cfg(target_os = "linux")]
    {
        super::sandbox_linux::available()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        super::sandbox_other::available()
    }
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
            paths.push(crate::paths::clean(&abs));
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
    if name.contains('/') {
        let direct = PathBuf::from(name);
        return is_executable(&direct).then_some(direct);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| {
            // Go's LookPath reads an empty PATH element as the current directory.
            let dir = if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir
            };
            dir.join(name)
        })
        .find(|candidate| is_executable(candidate))
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
    fn find_in_path_resolves_bash() {
        let bash = find_in_path("bash").expect("bash must exist for the shell tests");
        assert!(bash.is_absolute() || bash.starts_with("."), "{bash:?}");
        assert!(find_in_path("iota-definitely-not-a-binary").is_none());
        assert!(find_in_path("/definitely/not/a/binary").is_none());
    }
}
