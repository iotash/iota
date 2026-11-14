//! The herdr-core host end to end: the real `iota` binary inside a terminal of a real, ISOLATED hc core
//! (`HC_SESSION` plus temporary XDG state and config directories — the developer's own core is never
//! touched), asserted through `hc get`: `status.app = iota` and the state of a turn, an approval and an
//! error, the record gone after the exit, and a `-m` run reporting through `/dev/tty`. The provider is
//! the L4 SSE mock (`tests/ui_tmux/mock.rs`), which picks its reply by the typed message.
//!
//! **Gated on `hc`.** Without an `hc` on `PATH` every test prints a `SKIP:` line and passes, unless
//! `IOTA_HC_REQUIRED=1` is set, which turns the skip into a failure naming what is missing.
//!
//! **The terminal's environment is the CORE's** (measured: a variable on the `hc run` client does not
//! reach the terminal). So each test starts its own core under the environment its iota must see — a
//! cleared one, plus `HERDR_*` for the nesting case.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::HerdrMock;

/// How long a state may take to show up.
const DEADLINE: Duration = Duration::from_secs(20);

/// `hc` on this process's `PATH`, or the skip (a failure under `IOTA_HC_REQUIRED=1`).
fn hc_binary() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("hc"))
            .find(|f| f.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("IOTA_HC_REQUIRED").is_none_or(|v| v != "1"),
            "IOTA_HC_REQUIRED=1 but no `hc` on PATH"
        );
        println!("SKIP: no `hc` on PATH (herdr-core); the isolated-core tests did not run");
    }
    found
}

/// One isolated core: its directories, its session name, and the environment every `hc` call and
/// every terminal it starts runs under. Stopped on drop.
struct Core {
    hc: PathBuf,
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Core {
    /// Starts a core whose terminals see `extra` on top of a cleared environment, with `config` as the
    /// home's `.iota.yaml`.
    async fn start(hc: PathBuf, config: &str, extra: &[(String, String)]) -> Self {
        // Short: hc refuses a socket path past 103 bytes, and the one it measures is 16 bytes longer
        // than `<state>/herdr-core/<session>/<session>.sock` (measured: a 65-byte root fails).
        let dir = tempfile::Builder::new()
            .prefix("h")
            .rand_bytes(3)
            .tempdir()
            .expect("tempdir");
        // Canonical, as hc measures it (`/var` is `/private/var` on macOS). The session needs no
        // unique name: every core has state directories of its own.
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("h");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".iota.yaml"), config).unwrap();
        let session = "io".to_owned();
        let state = root.join("s");
        let mut env: Vec<(String, String)> = [
            ("HOME", home.to_string_lossy().into_owned()),
            ("PATH", "/bin:/usr/bin".to_owned()),
            ("HC_SESSION", session.clone()),
            ("XDG_STATE_HOME", state.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME",
                root.join("c").to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        env.extend_from_slice(extra);
        let hcc = Self { hc, dir, env };
        let started = hcc.run(&["server", "start"]).await;
        assert_eq!(started.0, Some(0), "hc server start: {}", started.1);
        hcc
    }

    /// The project directory iota runs in.
    fn cwd(&self) -> PathBuf {
        let p = self.dir.path().join("p");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Runs `hc <args>` under the core's environment: the exit code and stdout + stderr.
    async fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let out = tokio::process::Command::new(&self.hc)
            .args(args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(self.dir.path())
            .output()
            .await
            .expect("run hc");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code(), text)
    }

    /// Starts `sh -c <script>` in a new terminal named `name`, in the background.
    async fn spawn(&self, name: &str, script: &str) {
        let cwd = self.cwd();
        let (code, out) = self
            .run(&[
                "run",
                "--bg",
                "--json",
                "--no-parent",
                "--name",
                name,
                "--cwd",
                &cwd.to_string_lossy(),
                "sh",
                "-c",
                script,
            ])
            .await;
        assert_eq!(code, Some(0), "hc run: {out}");
    }

    /// `hc get <t>` as JSON.
    async fn get(&self, t: &str) -> Value {
        let (code, out) = self.run(&["get", "--json", t]).await;
        assert_eq!(code, Some(0), "hc get: {out}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("hc get is not JSON ({e}): {out}"))
    }

    /// Polls `hc get <t>` until `want` holds of it; panics with the last answer and the screen after
    /// [`DEADLINE`].
    async fn until(&self, t: &str, what: &str, want: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let got = self.get(t).await;
            if want(&got) {
                return got;
            }
            if start.elapsed() > DEADLINE {
                let screen = self.run(&["read", t]).await.1;
                panic!("{what}: never happened; last `hc get`: {got}\nscreen:\n{screen}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Polls until the record is iota's, in state `state`.
    async fn until_state(&self, t: &str, state: &str) -> Value {
        self.until(t, &format!("status {state}"), |g| {
            g["status"]["app"] == "iota" && g["status"]["state"] == state
        })
        .await
    }

    /// Types `text` and Enter into `t` (`hc send`): the exit code and output.
    async fn send(&self, t: &str, text: &str) -> (Option<i32>, String) {
        self.run(&["send", t, text]).await
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = std::process::Command::new(&self.hc)
            .args(["server", "stop"])
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .output();
    }
}

/// The config every run uses: the mock provider, and the `shell` set unsandboxed without `auto_run` —
/// the configuration that ASKS before a call.
fn config(port: u16) -> String {
    format!(
        "providers:\n  mock: {{type: openai, key: test, url: \"http://127.0.0.1:{port}\"}}\nmodels:\n  m: mock:fake\nagents:\n  default:\n    model: m\n    tools: {{shell: {{sandbox: off}}}}\n"
    )
}

/// The command line of the run: iota, then a shell that stays — so the record going away is iota's
/// own `state=clear`, not hc noticing the terminal's program exit.
fn script(iota_args: &str) -> String {
    format!(
        "'{}' {iota_args}; echo iota-exited; sleep 60",
        env!("CARGO_BIN_EXE_iota")
    )
}

/// An interactive chat in an hc terminal: idle once it is up, `working` through a turn, `blocked` at an
/// approval (and `hc send` refused with `kind: blocked`, exit 9), `error` after a failed turn, and no
/// record once it exits — while the terminal is still live.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interactive_chat_reports_its_states_to_hc() {
    let Some(hc) = hc_binary() else { return };
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &[]).await;
    hcc.spawn("chat", &script("")).await;

    let up = hcc.until_state("chat", "idle").await;
    assert_eq!(up["reported"], true, "{up}");

    // A turn: `working` while the reply drips, `idle` after it.
    let (code, out) = hcc.send("chat", "stream 30 100").await;
    assert_eq!(code, Some(0), "hc send: {out}");
    hcc.until_state("chat", "working").await;
    hcc.until_state("chat", "idle").await;

    // The approval: `blocked`, and a supervisor's `hc send` is refused while it is open.
    let (code, out) = hcc.send("chat", "runfg:echo approved").await;
    assert_eq!(code, Some(0), "hc send: {out}");
    hcc.until_state("chat", "blocked").await;
    let (code, out) = hcc.send("chat", "typed into the dialog").await;
    assert_eq!(code, Some(9), "hc send into the approval: {out}");
    assert!(out.contains("blocked"), "{out}");
    let (code, out) = hcc.run(&["keys", "chat", "enter"]).await;
    assert_eq!(code, Some(0), "hc keys: {out}");
    hcc.until("chat", "the approved call ran and the turn ended", |g| {
        g["status"]["state"] == "idle"
    })
    .await;
    let screen = hcc.run(&["read", "chat"]).await.1;
    assert!(screen.contains("ran: approved"), "{screen}");

    // A failed turn: `error` (a 400 is not retried).
    let (code, out) = hcc.send("chat", "fail:400:1 boom").await;
    assert_eq!(code, Some(0), "hc send: {out}");
    hcc.until_state("chat", "error").await;

    // The exit: iota's own clear, with the shell after it still the terminal's program.
    let (code, out) = hcc.run(&["keys", "chat", "ctrl+d"]).await;
    assert_eq!(code, Some(0), "hc keys: {out}");
    let gone = hcc
        .until("chat", "the record cleared on exit", |g| {
            g["status"].is_null()
        })
        .await;
    assert_eq!(
        gone["state"], "live",
        "the shell after iota is gone: {gone}"
    );
    let screen = hcc.run(&["read", "chat"]).await.1;
    assert!(screen.contains("iota-exited"), "{screen}");
}

/// A `-m` run has no facade: the host writes `/dev/tty` itself, so a supervisor sees it `working`, and
/// the record is cleared when it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_headless_run_reports_through_the_tty() {
    let Some(hc) = hc_binary() else { return };
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &[]).await;
    hcc.spawn("once", &script("-m 'stream 30 100' > reply.txt"))
        .await;
    hcc.until_state("once", "working").await;
    let gone = hcc
        .until("once", "the record cleared after the reply", |g| {
            g["status"].is_null() && g["reported"] == true
        })
        .await;
    assert_eq!(gone["state"], "live", "{gone}");
    let reply = std::fs::read_to_string(hcc.cwd().join("reply.txt")).unwrap();
    assert!(reply.contains("l#29 line"), "the reply: {reply}");
}

/// Nesting A: an hc core started from a herdr pane gives its terminals the pane's `HERDR_*`. iota
/// reports to hc, and the herdr pane it inherited hears nothing (before the hc host, it heard all).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inside_hc_started_from_herdr_only_hc_hears() {
    let Some(hc) = hc_binary() else { return };
    let herdr = HerdrMock::start();
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &herdr.env("w1:p2")).await;
    hcc.spawn("nested", &script("-m 'stream 10 100' > reply.txt"))
        .await;
    hcc.until_state("nested", "working").await;
    hcc.until("nested", "the record cleared", |g| g["status"].is_null())
        .await;
    assert!(
        herdr.requests().is_empty(),
        "the inherited herdr pane was told: {:?}",
        herdr.summaries()
    );
}

/// The child-agent measurement (docs/design/herdr-core-host.md, risk 2), pinned: a command the model
/// runs writes its own root record to the chat's tty — what an agent like Claude Code does — and it
/// REPLACES iota's until iota's next state change, which is the end of the turn. Accepted in v1; this
/// test says so, and turns red if hc or iota ever behave otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_writing_7501_replaces_the_record_until_the_next_state() {
    let Some(hc) = hc_binary() else { return };
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &[]).await;
    hcc.spawn("chat", &script("")).await;
    hcc.until_state("chat", "idle").await;
    // The second's wait is the agent starting up; a write in the same instant as the approval's answer
    // lands BEFORE iota's own `working`, which the UI loop writes on its next pass, and is overwritten
    // by it at once (measured).
    let child =
        r"runfg:sleep 1; printf '\033]7501;state=idle:app=claude-code\033\\' >/dev/tty; sleep 3";
    let (code, out) = hcc.send("chat", child).await;
    assert_eq!(code, Some(0), "hc send: {out}");
    hcc.until_state("chat", "blocked").await;
    let before = hcc.get("chat").await["turn"].as_u64().unwrap();
    hcc.run(&["keys", "chat", "enter"]).await;
    let taken = hcc
        .until("chat", "the child's record", |g| {
            g["status"]["app"] == "claude-code"
        })
        .await;
    assert_eq!(taken["status"]["state"], "idle", "{taken}");
    assert!(
        taken["turn"].as_u64().unwrap() > before,
        "the child's idle did not end a turn for hc: {taken}"
    );
    // The turn ends after the call: iota's Idle is a new state, so it is reported and the record is
    // iota's again.
    hcc.until_state("chat", "idle").await;
    hcc.run(&["keys", "chat", "ctrl+d"]).await;
    hcc.until("chat", "the record cleared", |g| g["status"].is_null())
        .await;
}
