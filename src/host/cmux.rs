//! The cmux terminal-multiplexer host (internal/host/cmux.go): the conversation state becomes a
//! sidebar status row keyed `iota` plus a workspace loading indicator, driven by `cmux` CLI
//! batches from a last-wins mailbox (a `tokio::sync::watch` channel) with a bounded per-command
//! timeout; `close` flushes one final clear batch (T3 design §5.1).
//!
//! cmux deliberately does NOT implement [`Notifier`](crate::host::Notifier): its ingress already
//! forwards the ANSI host's OSC 9 ping, and taking the channel over would hand presence policy to
//! cmux (cmux.go:20-25). Sidebar dressing is best-effort — a single worker task executes the
//! batches off the chat loop, bursts coalesce to the newest, and every failure is silent.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::BoxFuture;
use crate::host::background;
use crate::host::{BackgroundReporter, Closer, Env, Host, State, StateReporter};

/// The status-row key (cmux.go:38) — the program name.
pub(crate) const CMUX_KEY: &str = crate::app::NAME;
/// The host's name (cmux.go:74).
pub(crate) const CMUX_NAME: &str = "cmux";
/// The environment variable whose presence means "inside cmux" (cmux.go:40).
pub(crate) const CMUX_ENV: &str = "CMUX_SURFACE_ID";
/// The CLI binary looked up on `PATH` (cmux.go:44).
pub(crate) const CMUX_BIN: &str = "cmux";
/// Per-command execution bound (cmux.go:88).
pub(crate) const CMUX_EXEC_TIMEOUT: Duration = Duration::from_secs(2);
/// How long `close` waits for the final batch (cmux.go:112).
pub(crate) const CMUX_CLOSE_WAIT: Duration = Duration::from_secs(3);
/// The background RPC bound (background.go:60).
pub(crate) const CMUX_RPC_TIMEOUT: Duration = Duration::from_secs(1);

/// One mailbox item: the argv lists to run, in order.
pub(crate) type Batch = Vec<Vec<String>>;
/// The command runner seam (tests inject one; production runs `cmux <argv>`).
pub(crate) type ExecFn = Arc<dyn Fn(Vec<String>) -> BoxFuture<'static, ()> + Send + Sync>;
/// The background probe seam (`Some(dark)` when known).
pub(crate) type BackgroundFn = Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// The cmux host: a last-wins mailbox feeding one worker task (cmux.go:60-72).
///
/// The mailbox is a `tokio::sync::watch` channel, which stores only the LATEST value — exactly
/// Go's "replace a batch still waiting" rule (cmux.go:85-99), so a stale `Running` can never land
/// after `Idle`. [`StateReporter::set_state`] never awaits.
pub struct CmuxHost {
    tx: Mutex<Option<tokio::sync::watch::Sender<Option<Batch>>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    background: Option<BackgroundFn>,
}

impl CmuxHost {
    /// A host over `exec`, spawning the worker task (must run inside the runtime).
    pub(crate) fn with_exec(exec: ExecFn, background: Option<BackgroundFn>) -> Self {
        let (tx, mut rx) = tokio::sync::watch::channel::<Option<Batch>>(None);
        // `changed()` reports a still-unseen value BEFORE it reports the closed sender (tokio's
        // `maybe_changed` checks the version first), so `close`'s final batch is never lost.
        let worker = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let batch = rx.borrow_and_update().clone();
                let Some(batch) = batch else { continue };
                for argv in batch {
                    exec(argv).await;
                }
            }
        });
        Self {
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            background,
        }
    }

    /// Hands the worker a batch, replacing one still waiting (cmux.go:87-99). Never blocks.
    fn post(&self, batch: Batch) {
        if let Some(tx) = self
            .tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            tx.send_replace(Some(batch));
        }
    }
}

impl Host for CmuxHost {
    fn name(&self) -> &'static str {
        CMUX_NAME
    }

    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        Some(self)
    }

    fn as_background(&self) -> Option<&dyn BackgroundReporter> {
        self.background
            .as_ref()
            .map(|_| self as &dyn BackgroundReporter)
    }

    fn as_closer(&self) -> Option<&dyn Closer> {
        Some(self)
    }
}

impl StateReporter for CmuxHost {
    fn set_state(&self, s: State) {
        self.post(cmux_batch(s));
    }
}

impl BackgroundReporter for CmuxHost {
    fn dark_background(&self) -> Option<bool> {
        self.background.as_ref().and_then(|f| f())
    }
}

impl Closer for CmuxHost {
    /// Clears the status row and the spinner — they outlive the process otherwise — then waits
    /// (bounded by [`CMUX_CLOSE_WAIT`]) for the worker to flush (cmux.go:105-116). Idempotent.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.post(close_batch());
            // Dropping the sender closes the mailbox; the queued clear batch still arrives first.
            drop(
                self.tx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
            let worker = self
                .worker
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(h) = worker {
                let _ = tokio::time::timeout(CMUX_CLOSE_WAIT, h).await;
            }
        })
    }
}

/// `Some(CmuxHost)` when `CMUX_SURFACE_ID` is non-empty AND `cmux` is on `PATH`, in that order
/// (cmux.go:40-58). MUST run inside the tokio runtime: the host spawns its worker task.
pub(crate) fn detect_cmux(env: &Env) -> Option<Box<dyn Host>> {
    let sid = (env.getenv)(CMUX_ENV);
    if sid.is_empty() {
        return None;
    }
    let path = (env.look_path)(CMUX_BIN)?;
    let exec_path = path.clone();
    let exec: ExecFn = Arc::new(move |argv| exec_cmux(&exec_path, argv));
    let query: background::CmuxQuery = Arc::new(background::cmux_query_exec);
    let background: BackgroundFn =
        Arc::new(move || background::cmux_background(&path, &sid, &query));
    Some(Box::new(CmuxHost::with_exec(exec, Some(background))))
}

/// The argv lists of a state (cmux.go:122-145). Icons and colours mirror what cmux's own Claude
/// Code hook handler sends, so the sidebar reads the same for iota as for the built-in agents.
pub(crate) fn cmux_batch(s: State) -> Batch {
    let argv = |parts: &[&str]| {
        parts
            .iter()
            .map(|p| (*p).to_owned())
            .collect::<Vec<String>>()
    };
    match s {
        State::Busy => vec![
            argv(&[
                "set-status",
                CMUX_KEY,
                "Running",
                "--icon",
                "bolt.fill",
                "--color",
                "#4C8DFF",
            ]),
            argv(&["workspace", "loading", "on", "--id", CMUX_KEY]),
        ],
        State::NeedsInput => vec![
            argv(&["workspace", "loading", "off", "--id", CMUX_KEY]),
            // "Needs input" is ONE argv element.
            argv(&["set-status", CMUX_KEY, "Needs input", "--icon", "bell.fill"]),
        ],
        State::Error => vec![
            argv(&["workspace", "loading", "off", "--id", CMUX_KEY]),
            argv(&[
                "set-status",
                CMUX_KEY,
                "Failed",
                "--icon",
                "exclamationmark.triangle.fill",
                "--color",
                "#FF3B30",
            ]),
        ],
        State::Idle => vec![
            argv(&["workspace", "loading", "off", "--id", CMUX_KEY]),
            argv(&[
                "set-status",
                CMUX_KEY,
                "Idle",
                "--icon",
                "pause.circle.fill",
                "--color",
                "#8E8E93",
            ]),
        ],
    }
}

/// The argv lists of the exit clean-up (cmux.go:105-108).
pub(crate) fn close_batch() -> Batch {
    vec![
        vec![
            "workspace".to_owned(),
            "loading".to_owned(),
            "off".to_owned(),
            "--id".to_owned(),
            CMUX_KEY.to_owned(),
        ],
        vec!["clear-status".to_owned(), CMUX_KEY.to_owned()],
    ]
}

/// Runs `cmux <argv>` under [`CMUX_EXEC_TIMEOUT`], output discarded, errors ignored
/// (cmux.go:48-54). A wedged child is killed when the future's `Child` drops
/// (`kill_on_drop`).
pub(crate) fn exec_cmux(path: &Path, argv: Vec<String>) -> BoxFuture<'static, ()> {
    let path: PathBuf = path.to_path_buf();
    Box::pin(async move {
        let mut cmd = tokio::process::Command::new(&path);
        cmd.args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Ok(mut child) = cmd.spawn() {
            // Sidebar dressing must never fail — or delay — the chat.
            let _ = tokio::time::timeout(CMUX_EXEC_TIMEOUT, child.wait()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Batch, CmuxHost, ExecFn, close_batch, cmux_batch, detect_cmux};
    use crate::host::{Closer, Env, State, StateReporter};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn join(batch: &Batch) -> String {
        batch
            .iter()
            .map(|argv| argv.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Go: internal/host/cmux_test.go:12 TestCmuxBatches — the CLI sequence per state, icon and
    // colour included: they are what gives the sidebar row parity with the built-in agents.
    #[test]
    fn test_cmux_batches() {
        for (state, want) in [
            (
                State::Busy,
                "set-status iota Running --icon bolt.fill --color #4C8DFF\nworkspace loading on --id iota",
            ),
            (
                State::NeedsInput,
                "workspace loading off --id iota\nset-status iota Needs input --icon bell.fill",
            ),
            (
                State::Error,
                "workspace loading off --id iota\nset-status iota Failed --icon exclamationmark.triangle.fill --color #FF3B30",
            ),
            (
                State::Idle,
                "workspace loading off --id iota\nset-status iota Idle --icon pause.circle.fill --color #8E8E93",
            ),
        ] {
            assert_eq!(join(&cmux_batch(state)), want, "state {state:?}");
        }
        assert_eq!(
            join(&close_batch()),
            "workspace loading off --id iota\nclear-status iota"
        );
    }

    // Go: internal/host/cmux_test.go:35 TestCmuxCoalescing — the mailbox is last-wins: states
    // posted while the worker is busy collapse to the newest, so a stale "Needs input" can never
    // land after the turn moved on. `close` flushes the clear sequence.
    #[tokio::test]
    async fn test_cmux_coalescing() {
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(release_rx));
        let got: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();

        let log = Arc::clone(&got);
        let exec: ExecFn = Arc::new(move |argv: Vec<String>| {
            let entered = entered_tx.clone();
            let release = Arc::clone(&release_rx);
            let log = Arc::clone(&log);
            Box::pin(async move {
                let _ = entered.send(());
                let _ = release.lock().await.recv().await;
                log.lock().unwrap().push(argv);
            })
        });
        let c = CmuxHost::with_exec(exec, None);

        c.set_state(State::Busy);
        let _ = entered_rx.recv().await; // the worker is inside the busy batch's first command

        c.set_state(State::NeedsInput);
        c.set_state(State::Idle); // replaces NeedsInput in the mailbox

        release_tx.send(()).unwrap(); // busy cmd 1 completes
        for _ in 0..3 {
            let _ = entered_rx.recv().await;
            release_tx.send(()).unwrap(); // busy cmd 2, then the idle batch's two commands
        }

        let closing = tokio::spawn(async move {
            c.close().await;
            c
        });
        for _ in 0..2 {
            let _ = entered_rx.recv().await;
            release_tx.send(()).unwrap(); // the close batch: loading off + clear-status
        }
        let _c = closing.await.expect("close task");

        let ran = got.lock().unwrap().clone();
        let heads: Vec<String> = ran
            .iter()
            .map(|argv| format!("{} {}", argv[0], argv[1]))
            .collect();
        assert_eq!(
            heads,
            [
                "set-status iota",
                "workspace loading", // busy
                "workspace loading",
                "set-status iota", // idle (needs-input dropped)
                "workspace loading",
                "clear-status iota", // close
            ]
        );
        assert!(
            !ran.iter()
                .any(|argv| argv.join(" ").contains("Needs input")),
            "the superseded Needs-input batch still executed: {ran:?}"
        );
    }

    // Go: internal/host/cmux_test.go:87 TestDetectCmux — both markers must be present: the env
    // var cmux injects into every pane, and its CLI on PATH.
    #[tokio::test]
    async fn test_detect_cmux() {
        let with_var = |k: &str| {
            if k == "CMUX_SURFACE_ID" {
                "surface-1".to_owned()
            } else {
                String::new()
            }
        };
        let found = |_: &str| Some(PathBuf::from("/usr/bin/true"));
        let absent = |_: &str| None;

        assert!(
            detect_cmux(&Env {
                getenv: Box::new(|_| String::new()),
                look_path: Box::new(found),
            })
            .is_none(),
            "detected cmux without CMUX_SURFACE_ID"
        );
        assert!(
            detect_cmux(&Env {
                getenv: Box::new(with_var),
                look_path: Box::new(absent),
            })
            .is_none(),
            "detected cmux without the CLI on PATH"
        );
        let h = detect_cmux(&Env {
            getenv: Box::new(with_var),
            look_path: Box::new(found),
        })
        .expect("cmux not detected");
        assert_eq!(h.name(), "cmux");
        h.as_closer().expect("cmux closes").close().await;
    }
}
