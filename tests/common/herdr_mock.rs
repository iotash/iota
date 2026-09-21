//! A stand-in herdr (`HerdrMock`): a Unix socket server in a temp dir that records every request it
//! is sent — one JSON line each, `{"id","method","params"}` — and answers each with
//! `{"id":…,"result":{"type":"ok"}}`, exactly the exchange `src/host/herdr.rs` expects.
//!
//! Every test that talks to it sets `HERDR_ENV`, `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` EXPLICITLY
//! — in a fixed `Env` for an in-process presenter, or on a child's cleared environment — so no test
//! ever reads the developer's own herdr variables, let alone reports to the real socket (which
//! would change the state of the pane the suite itself runs in). Unix only: the transport is a Unix
//! socket, and so is this.

#![cfg(unix)]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};

/// One request the mock received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrRequest {
    /// The `id` field (`iota:<seq>`).
    pub id: String,
    /// The `method` field (`pane.report_agent`, …).
    pub method: String,
    /// The `params` object.
    pub params: serde_json::Value,
}

impl HerdrRequest {
    /// `params.<key>` as a string, `""` when absent or not a string.
    pub fn param(&self, key: &str) -> String {
        self.params
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// `params.seq`, or `None` when absent.
    pub fn seq(&self) -> Option<u64> {
        self.params.get("seq").and_then(serde_json::Value::as_u64)
    }

    /// `method` plus the state, the session id or nothing — the one-word summary the sequence
    /// assertions compare: `report_agent working`, `report_agent_session s-1`, `release_agent`.
    pub fn summary(&self) -> String {
        let method = self.method.trim_start_matches("pane.");
        match method {
            "report_agent" => format!("{method} {}", self.param("state")),
            "report_agent_session" => format!("{method} {}", self.param("agent_session_id")),
            _ => method.to_owned(),
        }
    }
}

/// The server: alive until dropped.
pub struct HerdrMock {
    _dir: tempfile::TempDir,
    path: PathBuf,
    requests: Arc<Mutex<Vec<HerdrRequest>>>,
    accept: JoinHandle<()>,
}

impl HerdrMock {
    /// Binds `<tempdir>/herdr.sock` and starts accepting (inside the runtime: the accept loop is a
    /// task on it).
    pub fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir for the herdr socket");
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).expect("bind the herdr mock socket");
        let requests: Arc<Mutex<Vec<HerdrRequest>>> = Arc::default();
        let log = Arc::clone(&requests);
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&log)));
            }
        });
        Self {
            _dir: dir,
            path,
            requests,
            accept,
        }
    }

    /// The variables a herdr pane has, pointed at this mock: the three the detector needs, the
    /// binary the CLI transport would want (nonexistent: nothing here runs it), and the workspace
    /// and tab ids (`w1` and `w1:t1`, for the `<environment>` lines).
    pub fn env(&self, pane: &str) -> Vec<(String, String)> {
        [
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", pane),
            ("HERDR_SOCKET_PATH", &self.path.to_string_lossy()),
            ("HERDR_BIN_PATH", "/nonexistent/herdr"),
            ("HERDR_WORKSPACE_ID", "w1"),
            ("HERDR_TAB_ID", "w1:t1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    /// Everything received so far, in arrival order.
    pub fn requests(&self) -> Vec<HerdrRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The one-word summaries of everything received (`HerdrRequest::summary`).
    pub fn summaries(&self) -> Vec<String> {
        self.requests().iter().map(HerdrRequest::summary).collect()
    }
}

impl Drop for HerdrMock {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// One connection: every line is a request, recorded and answered.
async fn serve(stream: UnixStream, log: Arc<Mutex<Vec<HerdrRequest>>>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => panic!("the herdr mock got a line that is not JSON: {e}: {line}"),
        };
        let req = HerdrRequest {
            id: value["id"].as_str().unwrap_or_default().to_owned(),
            method: value["method"].as_str().unwrap_or_default().to_owned(),
            params: value["params"].clone(),
        };
        let reply = serde_json::json!({ "id": req.id, "result": { "type": "ok" } });
        log.lock().unwrap_or_else(PoisonError::into_inner).push(req);
        let mut bytes = reply.to_string().into_bytes();
        bytes.push(b'\n');
        if write.write_all(&bytes).await.is_err() {
            return;
        }
    }
}
