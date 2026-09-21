//! The herdr host (brain page `host-integration`, decision of 2026-09-21). herdr is a terminal
//! multiplexer for coding agents: it recognises the agent in each pane by watching the screen —
//! until the agent REPORTS. iota reports. Over the Unix socket herdr injects into the pane, one
//! JSON line per request, the conversation's state becomes the pane's lifecycle (`working`,
//! `blocked`, `idle`), the session it writes into becomes the pane's agent session, and the exit
//! releases the pane back to screen recognition. Once a source has reported, herdr takes it as the
//! pane's authority; a `seq` no newer than the last is ignored, which is why the counter starts at
//! the process's start time in nanoseconds — a restarted iota in the same pane never goes backwards.
//!
//! The transport is the cmux pattern: one worker task drains a bounded queue off the chat loop,
//! every request is bounded, every failure is a `tracing::debug!` and nothing else — a pane's badge
//! must never fail or delay the chat. Unix talks to the socket directly; a platform without Unix
//! sockets (Windows) runs the CLI equivalent through `HERDR_BIN_PATH`. The queue keeps order rather
//! than coalescing like cmux's mailbox: a session report and the release are not states a newer
//! state may replace.
//!
//! herdr also tells the model where it is — `host: herdr`, the pane id, and the workspace and tab
//! ids when the pane has them — through the `<environment>` capability.
//!
//! Like cmux, herdr deliberately does NOT implement [`Notifier`](crate::host::Notifier): the
//! presenter pings the first notifier only, so a host that implements it takes the OSC 9 channel
//! away from the ANSI host — and herdr notifies from the semantic state it is given here, and
//! handles the pane's OSC 9 itself. A `blocked` report therefore carries no `message`. Nor does it
//! implement [`BackgroundReporter`](crate::host::BackgroundReporter): a herdr pane answers OSC 11
//! itself, and the ANSI probe is enough.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::BoxFuture;
use crate::host::{
    Closer, EnvironmentContributor, Host, Probe, SessionReporter, State, StateReporter,
};
use crate::sync::lock;

/// The host's name.
pub(crate) const HERDR_NAME: &str = "herdr";
/// `HERDR_ENV=1` means "inside herdr" (its own skill checks exactly this).
pub(crate) const HERDR_ENV: &str = "HERDR_ENV";
/// The pane the chat runs in — what every report names.
pub(crate) const HERDR_PANE_ID: &str = "HERDR_PANE_ID";
/// The Unix socket the API listens on.
pub(crate) const HERDR_SOCKET_PATH: &str = "HERDR_SOCKET_PATH";
/// The `herdr` binary, for the CLI transport (the platforms without Unix sockets).
#[cfg(not(unix))]
pub(crate) const HERDR_BIN_PATH: &str = "HERDR_BIN_PATH";
/// The pane's workspace (`w1`), told to the model when present.
pub(crate) const HERDR_WORKSPACE_ID: &str = "HERDR_WORKSPACE_ID";
/// The pane's tab (`w1:t2`), told to the model when present.
pub(crate) const HERDR_TAB_ID: &str = "HERDR_TAB_ID";
/// `source` AND `agent` of every report: the program name. herdr keys its lifecycle authority on
/// the source, and shows the agent label.
pub(crate) const HERDR_SOURCE: &str = crate::app::NAME;
/// One request, connect to reply.
pub(crate) const HERDR_TIMEOUT: Duration = Duration::from_millis(500);
/// How long `close` waits for the worker to flush the release.
pub(crate) const HERDR_CLOSE_WAIT: Duration = Duration::from_secs(2);
/// Requests waiting for the worker; a burst past this is dropped (a `debug!`), never awaited.
pub(crate) const HERDR_QUEUE: usize = 32;

/// What one request says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// `pane.report_agent`: the lifecycle state (`working` | `blocked` | `idle`). Never with the
    /// protocol's optional `message`: herdr words its own notification from the state.
    State(&'static str),
    /// `pane.report_agent_session`: the session the chat persists into.
    Session {
        /// The session id.
        id: String,
        /// The bundle directory.
        path: PathBuf,
    },
    /// `pane.release_agent`: the exit.
    Release,
}

/// One request on the wire: the pane, the sequence number and the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    /// `HERDR_PANE_ID`.
    pub(crate) pane: String,
    /// Monotonic across the process, and across restarts in the same pane.
    pub(crate) seq: u64,
    /// What it says.
    pub(crate) report: Report,
}

impl Request {
    /// The JSON-RPC-style method.
    pub(crate) fn method(&self) -> &'static str {
        match self.report {
            Report::State(_) => "pane.report_agent",
            Report::Session { .. } => "pane.report_agent_session",
            Report::Release => "pane.release_agent",
        }
    }

    /// The request id: `iota:<seq>`.
    #[cfg(any(unix, test))]
    pub(crate) fn id(&self) -> String {
        format!("{HERDR_SOURCE}:{}", self.seq)
    }

    /// The one JSON line of the socket protocol, without its newline — the Unix transport; pinned
    /// by the unit test on every platform.
    #[cfg(any(unix, test))]
    pub(crate) fn json_line(&self) -> String {
        let mut params = serde_json::json!({
            "pane_id": self.pane,
            "source": HERDR_SOURCE,
            "agent": HERDR_SOURCE,
            "seq": self.seq,
        });
        match &self.report {
            Report::State(state) => {
                params["state"] = serde_json::Value::from(*state);
            }
            Report::Session { id, path } => {
                params["agent_session_id"] = serde_json::Value::from(id.as_str());
                params["agent_session_path"] = serde_json::Value::from(path.display().to_string());
            }
            Report::Release => {}
        }
        serde_json::json!({
            "id": self.id(),
            "method": self.method(),
            "params": params,
        })
        .to_string()
    }

    /// The CLI equivalent: `pane report-agent <pane> --source iota --agent iota --state … --seq N`,
    /// `pane report-agent-session …`, `pane release-agent …` — the transport where there is no
    /// Unix socket; pinned by the unit test on every platform.
    #[cfg(any(not(unix), test))]
    pub(crate) fn argv(&self) -> Vec<String> {
        let verb = match self.report {
            Report::State(_) => "report-agent",
            Report::Session { .. } => "report-agent-session",
            Report::Release => "release-agent",
        };
        let mut argv: Vec<String> = [
            "pane",
            verb,
            self.pane.as_str(),
            "--source",
            HERDR_SOURCE,
            "--agent",
            HERDR_SOURCE,
            "--seq",
            &self.seq.to_string(),
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        match &self.report {
            Report::State(state) => {
                argv.push("--state".to_owned());
                argv.push((*state).to_owned());
            }
            Report::Session { id, path } => {
                argv.push("--agent-session-id".to_owned());
                argv.push(id.clone());
                argv.push("--agent-session-path".to_owned());
                argv.push(path.display().to_string());
            }
            Report::Release => {}
        }
        argv
    }
}

/// The lifecycle word of a state. `Error` is `idle`: the turn is over and the user may type, which
/// is not waiting on a decision (`blocked`).
pub(crate) fn lifecycle(s: State) -> &'static str {
    match s {
        State::Busy => "working",
        State::NeedsInput => "blocked",
        State::Idle | State::Error => "idle",
    }
}

/// The pane's identity from the environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pane {
    /// `HERDR_PANE_ID`.
    pub(crate) id: String,
    /// `HERDR_WORKSPACE_ID`, when injected.
    pub(crate) workspace: Option<String>,
    /// `HERDR_TAB_ID`, when injected.
    pub(crate) tab: Option<String>,
}

/// The transport seam: sends one request, bounded, and never fails (tests inject a recorder;
/// production is the socket, or the CLI child).
pub(crate) type SendFn = Arc<dyn Fn(Request) -> BoxFuture<'static, ()> + Send + Sync>;

/// The herdr host: a bounded, ordered queue feeding one worker task. Every capability call assigns
/// the next `seq` and enqueues without awaiting; the worker sends in order.
pub struct HerdrHost {
    pane: Pane,
    seq: AtomicU64,
    tx: Mutex<Option<mpsc::Sender<Request>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl HerdrHost {
    /// A host for `pane` over `send`, numbering requests from `first_seq`; spawns the worker task
    /// (must run inside the runtime).
    pub(crate) fn with_send(pane: Pane, first_seq: u64, send: SendFn) -> Self {
        let (tx, mut rx) = mpsc::channel::<Request>(HERDR_QUEUE);
        let worker = tokio::spawn(async move {
            // The queue drains in order and the channel closes only once it is empty, so the release
            // `close` enqueues last is the last request sent.
            while let Some(req) = rx.recv().await {
                send(req).await;
            }
        });
        Self {
            pane,
            seq: AtomicU64::new(first_seq),
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Numbers `report` and hands it to the worker. Never blocks: a full queue drops the request
    /// (herdr would ignore a late one anyway), a closed one — after `close` — drops it silently.
    fn post(&self, report: Report) {
        let req = Request {
            pane: self.pane.id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            report,
        };
        if let Some(tx) = lock(&self.tx).as_ref()
            && let Err(e) = tx.try_send(req)
        {
            tracing::debug!("herdr: request dropped: {e}");
        }
    }
}

impl Host for HerdrHost {
    fn name(&self) -> &'static str {
        HERDR_NAME
    }

    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        Some(self)
    }

    fn as_closer(&self) -> Option<&dyn Closer> {
        Some(self)
    }

    fn as_session_reporter(&self) -> Option<&dyn SessionReporter> {
        Some(self)
    }

    fn as_environment(&self) -> Option<&dyn EnvironmentContributor> {
        Some(self)
    }
}

impl StateReporter for HerdrHost {
    fn set_state(&self, s: State) {
        self.post(Report::State(lifecycle(s)));
    }
}

impl SessionReporter for HerdrHost {
    fn report_session(&self, id: &str, path: &Path) {
        self.post(Report::Session {
            id: id.to_owned(),
            path: path.to_path_buf(),
        });
    }
}

impl EnvironmentContributor for HerdrHost {
    /// `host: herdr`, the pane, and the workspace and tab when herdr injected them — the ids its
    /// CLI takes (`herdr pane split --current`, `herdr tab list --workspace <id>`).
    fn environment(&self) -> Vec<(String, String)> {
        let mut out = vec![
            ("host".to_owned(), HERDR_NAME.to_owned()),
            ("herdr pane".to_owned(), self.pane.id.clone()),
        ];
        if let Some(ws) = &self.pane.workspace {
            out.push(("herdr workspace".to_owned(), ws.clone()));
        }
        if let Some(tab) = &self.pane.tab {
            out.push(("herdr tab".to_owned(), tab.clone()));
        }
        out
    }
}

impl Closer for HerdrHost {
    /// Releases the pane — herdr goes back to reading the screen, and the badge does not outlive
    /// the process — then waits (bounded by [`HERDR_CLOSE_WAIT`]) for the worker to flush.
    /// Idempotent.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.post(Report::Release);
            // Dropping the sender closes the queue; what is queued, the release last, still drains.
            drop(lock(&self.tx).take());
            let worker = lock(&self.worker).take();
            if let Some(h) = worker {
                let _ = tokio::time::timeout(HERDR_CLOSE_WAIT, h).await;
            }
        })
    }
}

/// `Some(HerdrHost)` when `HERDR_ENV` is `1` and both `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` are
/// non-empty — the three herdr injects into every pane it manages (on a platform without Unix
/// sockets, `HERDR_BIN_PATH` too, for the CLI transport). MUST run inside the tokio runtime: the
/// host spawns its worker task.
pub(crate) fn detect_herdr(probe: &Probe) -> Option<Box<dyn Host>> {
    if probe.env.var(HERDR_ENV).as_deref() != Some("1") {
        return None;
    }
    let id = probe.env.var(HERDR_PANE_ID)?;
    let socket = probe.env.var(HERDR_SOCKET_PATH)?;
    let send = transport(probe, &socket)?;
    let pane = Pane {
        id,
        workspace: probe.env.var(HERDR_WORKSPACE_ID),
        tab: probe.env.var(HERDR_TAB_ID),
    };
    Some(Box::new(HerdrHost::with_send(pane, start_seq(), send)))
}

/// Nanoseconds since the epoch at start-up — the first `seq`, so a restart in the same pane
/// outranks everything the previous process reported. Zero if the clock is before 1970.
fn start_seq() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// The socket transport: one connection per request. The `Option` is the CLI transport's — it may
/// lack `HERDR_BIN_PATH` — and `detect_herdr` reads one shape on every platform.
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn transport(_probe: &Probe, socket: &str) -> Option<SendFn> {
    let path = PathBuf::from(socket);
    Some(Arc::new(move |req| send_socket(path.clone(), req)))
}

/// The CLI transport, where there is no Unix socket to write.
#[cfg(not(unix))]
fn transport(probe: &Probe, _socket: &str) -> Option<SendFn> {
    let bin = PathBuf::from(probe.env.var(HERDR_BIN_PATH)?);
    Some(Arc::new(move |req| send_cli(bin.clone(), req)))
}

/// Connects to `path`, writes the request line, reads the one reply line — the whole exchange under
/// [`HERDR_TIMEOUT`]. A refused connection, a timeout and an error reply are each one `debug!`.
#[cfg(unix)]
pub(crate) fn send_socket(path: PathBuf, req: Request) -> BoxFuture<'static, ()> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    Box::pin(async move {
        let method = req.method();
        let line = req.json_line();
        let exchange = async {
            let mut stream = tokio::net::UnixStream::connect(&path).await?;
            stream.write_all(line.as_bytes()).await?;
            stream.write_all(b"\n").await?;
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).await?;
            Ok::<String, std::io::Error>(reply)
        };
        match tokio::time::timeout(HERDR_TIMEOUT, exchange).await {
            Ok(Ok(reply)) => {
                if reply.contains("\"error\"") {
                    tracing::debug!("herdr {method}: {}", reply.trim_end());
                }
            }
            Ok(Err(e)) => tracing::debug!("herdr {method}: {e}"),
            Err(_) => tracing::debug!("herdr {method}: no reply within {HERDR_TIMEOUT:?}"),
        }
    })
}

/// Runs `herdr <argv>` under [`HERDR_TIMEOUT`], output discarded; a wedged child is killed when the
/// future's `Child` drops (`kill_on_drop`).
#[cfg(not(unix))]
pub(crate) fn send_cli(bin: PathBuf, req: Request) -> BoxFuture<'static, ()> {
    use std::process::Stdio;
    Box::pin(async move {
        let method = req.method();
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(req.argv())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match cmd.spawn() {
            Ok(mut child) => {
                if tokio::time::timeout(HERDR_TIMEOUT, child.wait())
                    .await
                    .is_err()
                {
                    tracing::debug!("herdr {method}: the CLI took longer than {HERDR_TIMEOUT:?}");
                }
            }
            Err(e) => tracing::debug!("herdr {method}: {}: {e}", bin.display()),
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{HerdrHost, Pane, Report, Request, SendFn, detect_herdr, lifecycle};
    use crate::app::env::Env;
    use crate::host::{Closer, Probe, SessionReporter, State, StateReporter};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    fn probe(vars: &[(&str, &str)]) -> Probe {
        Probe {
            env: Env::fixed(vars),
            look_path: Box::new(|_| None),
        }
    }

    /// The three variables every herdr pane has, plus the binary the CLI transport wants.
    const FULL: &[(&str, &str)] = &[
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock"),
        ("HERDR_BIN_PATH", "/nonexistent/herdr"),
    ];

    /// A `SendFn` recording every request in order.
    fn recorder() -> (SendFn, Arc<Mutex<Vec<Request>>>) {
        let log: Arc<Mutex<Vec<Request>>> = Arc::default();
        let sink = Arc::clone(&log);
        let send: SendFn = Arc::new(move |req| {
            let sink = Arc::clone(&sink);
            Box::pin(async move {
                sink.lock().unwrap().push(req);
            })
        });
        (send, log)
    }

    fn pane() -> Pane {
        Pane {
            id: "w1:p2".to_owned(),
            workspace: Some("w1".to_owned()),
            tab: Some("w1:t3".to_owned()),
        }
    }

    /// `HERDR_ENV=1` and both the pane and the socket: any of the three missing — or `HERDR_ENV`
    /// set to something else — is no host.
    #[tokio::test]
    async fn test_detect_herdr() {
        let without = |name: &str| -> Vec<(&str, &str)> {
            FULL.iter().copied().filter(|(k, _)| *k != name).collect()
        };
        for missing in ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_SOCKET_PATH"] {
            assert!(
                detect_herdr(&probe(&without(missing))).is_none(),
                "detected herdr without {missing}"
            );
        }
        let mut off = without("HERDR_ENV");
        off.push(("HERDR_ENV", "0"));
        assert!(
            detect_herdr(&probe(&off)).is_none(),
            "detected herdr with HERDR_ENV=0"
        );

        let h = detect_herdr(&probe(FULL)).expect("herdr not detected");
        assert_eq!(h.name(), "herdr");
        assert!(h.as_state_reporter().is_some());
        assert!(h.as_session_reporter().is_some());
        // The two channels herdr leaves to the ANSI host: the OSC 9 ping and the OSC 11 answer.
        assert!(
            h.as_notifier().is_none(),
            "herdr must not take the ping channel"
        );
        assert!(h.as_background().is_none(), "herdr knows no background");
        assert_eq!(
            h.as_environment().expect("herdr contributes").environment(),
            vec![
                ("host".to_owned(), "herdr".to_owned()),
                ("herdr pane".to_owned(), "w1:p2".to_owned()),
            ]
        );
        // The release goes to a socket nobody listens on: a debug line, and `close` returns.
        h.as_closer().expect("herdr closes").close().await;
    }

    /// The workspace and tab lines appear only when herdr injected them.
    #[tokio::test]
    async fn the_environment_names_the_workspace_and_tab_when_present() {
        let mut vars = FULL.to_vec();
        vars.push(("HERDR_WORKSPACE_ID", "w1"));
        vars.push(("HERDR_TAB_ID", "w1:t3"));
        let h = detect_herdr(&probe(&vars)).expect("herdr not detected");
        assert_eq!(
            h.as_environment().expect("herdr contributes").environment(),
            vec![
                ("host".to_owned(), "herdr".to_owned()),
                ("herdr pane".to_owned(), "w1:p2".to_owned()),
                ("herdr workspace".to_owned(), "w1".to_owned()),
                ("herdr tab".to_owned(), "w1:t3".to_owned()),
            ]
        );
        h.as_closer().expect("herdr closes").close().await;
    }

    /// `Busy` is `working`, `NeedsInput` is `blocked`, and BOTH `Idle` and `Error` are `idle`: after
    /// a failed turn the user may type, which is not a decision the agent waits on.
    #[test]
    fn test_lifecycle_words() {
        assert_eq!(lifecycle(State::Busy), "working");
        assert_eq!(lifecycle(State::NeedsInput), "blocked");
        assert_eq!(lifecycle(State::Idle), "idle");
        assert_eq!(lifecycle(State::Error), "idle");
    }

    /// The wire shapes: the JSON line herdr's socket reads, and the argv its CLI takes. No
    /// `message` on a state, ever.
    #[test]
    fn test_request_shapes() {
        let req = |report| Request {
            pane: "w1:p2".to_owned(),
            seq: 41,
            report,
        };
        let parsed =
            |r: &Request| -> serde_json::Value { serde_json::from_str(&r.json_line()).unwrap() };

        let state = req(Report::State("blocked"));
        assert_eq!(
            parsed(&state),
            serde_json::json!({
                "id": "iota:41",
                "method": "pane.report_agent",
                "params": {
                    "pane_id": "w1:p2", "source": "iota", "agent": "iota", "seq": 41,
                    "state": "blocked",
                },
            })
        );
        assert_eq!(
            state.argv().join(" "),
            "pane report-agent w1:p2 --source iota --agent iota --seq 41 --state blocked"
        );
        assert!(
            !state.json_line().contains('\n'),
            "one line, no newline inside"
        );
        assert!(!state.json_line().contains("message"));

        let session = req(Report::Session {
            id: "s-1".to_owned(),
            path: PathBuf::from("/tmp/sessions/s-1"),
        });
        assert_eq!(
            parsed(&session),
            serde_json::json!({
                "id": "iota:41",
                "method": "pane.report_agent_session",
                "params": {
                    "pane_id": "w1:p2", "source": "iota", "agent": "iota", "seq": 41,
                    "agent_session_id": "s-1", "agent_session_path": "/tmp/sessions/s-1",
                },
            })
        );
        assert_eq!(
            session.argv().join(" "),
            "pane report-agent-session w1:p2 --source iota --agent iota --seq 41 --agent-session-id s-1 --agent-session-path /tmp/sessions/s-1"
        );

        let release = req(Report::Release);
        assert_eq!(
            parsed(&release),
            serde_json::json!({
                "id": "iota:41",
                "method": "pane.release_agent",
                "params": { "pane_id": "w1:p2", "source": "iota", "agent": "iota", "seq": 41 },
            })
        );
        assert_eq!(
            release.argv().join(" "),
            "pane release-agent w1:p2 --source iota --agent iota --seq 41"
        );
    }

    /// The capability calls become requests in call order with a strictly increasing `seq` from the
    /// start value, and `close` ends the sequence with the release.
    #[tokio::test]
    async fn test_host_sequence() {
        let (send, log) = recorder();
        let h = HerdrHost::with_send(pane(), 1000, send);
        h.set_state(State::Busy);
        h.set_state(State::NeedsInput);
        h.set_state(State::Busy);
        h.report_session("s-1", Path::new("/tmp/s-1"));
        h.set_state(State::Idle);
        h.set_state(State::Error);
        h.close().await;

        let got = log.lock().unwrap().clone();
        let seqs: Vec<u64> = got.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![1000, 1001, 1002, 1003, 1004, 1005, 1006]);
        assert!(got.iter().all(|r| r.pane == "w1:p2"));
        let reports: Vec<Report> = got.into_iter().map(|r| r.report).collect();
        assert_eq!(
            reports,
            vec![
                Report::State("working"),
                Report::State("blocked"),
                Report::State("working"),
                Report::Session {
                    id: "s-1".to_owned(),
                    path: PathBuf::from("/tmp/s-1"),
                },
                Report::State("idle"),
                Report::State("idle"),
                Report::Release,
            ]
        );
        // After close, a late call is dropped, not panicked on.
        h.set_state(State::Busy);
        h.close().await;
    }

    /// A transport that never answers stalls the worker, not the chat: a burst past the queue is
    /// dropped without awaiting, and `close` gives up after its bound.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_transport_never_blocks_the_caller() {
        let send: SendFn = Arc::new(|_| Box::pin(std::future::pending()));
        let h = HerdrHost::with_send(pane(), 0, send);
        let started = std::time::Instant::now();
        for _ in 0..(super::HERDR_QUEUE * 4) {
            h.set_state(State::Busy);
            h.set_state(State::Idle);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "posting blocked for {:?}",
            started.elapsed()
        );
        let before = tokio::time::Instant::now();
        h.close().await;
        assert_eq!(
            before.elapsed(),
            super::HERDR_CLOSE_WAIT,
            "close must wait exactly its bound for a worker that never finishes"
        );
    }
}
