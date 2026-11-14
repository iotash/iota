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
//!
//! **No core outlives its test.** `Drop` stops it; when `server stop` fails it kills every process that
//! holds a file in the core's state directory (the core, by its `core.lock`, and the holders whose
//! terminals die with them) and says so on stderr. A panic unwinds into that `Drop`; an interrupt does
//! not, so each core also gets a reaper in its own process group — out of reach of the terminal's
//! Ctrl+C — that waits for this test process to end and then does the same to whatever is left.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::HerdrMock;

/// How long a state may take to show up.
const DEADLINE: Duration = Duration::from_secs(20);

/// `name` on this process's `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|f| f.is_file())
    })
}

/// `hc` on this process's `PATH`, or the skip (a failure under `IOTA_HC_REQUIRED=1`).
fn hc_binary() -> Option<PathBuf> {
    let found = which("hc");
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

/// Kills every process holding a file under `$1` (a core's state directory: the core holds its
/// `core.lock`, each holder its socket). Shared by `Drop` and the reaper.
const KILL_HOLDERS: &str =
    "pids=$(/usr/sbin/lsof -t +D \"$1\" 2>/dev/null); [ -z \"$pids\" ] || kill $pids";

/// The reaper (the module doc): `$1` this process, `$2` the core's root, `$3` `hc`. Exits at once
/// when the root is gone — the test cleaned up after itself.
const REAPER: &str = r#"while kill -0 "$1" 2>/dev/null; do sleep 1; done
[ -d "$2" ] || exit 0
"$3" server stop >/dev/null 2>&1 || { pids=$(/usr/sbin/lsof -t +D "$2/s" 2>/dev/null); [ -z "$pids" ] || kill $pids; }
rm -rf "$2""#;

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
        hcc.reaper(&root);
        let started = hcc.run(&["server", "start"]).await;
        assert_eq!(started.0, Some(0), "hc server start: {}", started.1);
        hcc
    }

    /// Starts the reaper (the module doc) for the core rooted at `root`.
    fn reaper(&self, root: &std::path::Path) {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("/bin/sh")
            .args(["-c", REAPER, "reaper"])
            .arg(std::process::id().to_string())
            .arg(root)
            .arg(&self.hc)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start the core's reaper");
    }

    /// The core's state directory (`XDG_STATE_HOME`).
    fn state(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("s")
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
    /// Stops the core; when that fails, kills what holds its state directory (the module doc).
    fn drop(&mut self) {
        let stop = std::process::Command::new(&self.hc)
            .args(["server", "stop"])
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .output();
        let failed = match &stop {
            Ok(out) if out.status.success() => return,
            Ok(out) => format!(
                "{}: {}{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => e.to_string(),
        };
        let state = self.state();
        let killed = std::process::Command::new("/bin/sh")
            .args(["-c", KILL_HOLDERS, "kill"])
            .arg(&state)
            .status();
        eprintln!(
            "hc server stop failed ({failed}); killed what holds {}: {killed:?}",
            state.display()
        );
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

/// tmux inside an hc terminal, iota inside tmux: tmux answers only the probe's DSR, so iota does not
/// take hc for its host — the terminal record stays unreported — and the tmux pane gets the OSC 9;4
/// progress the plain terminal is told.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inside_tmux_inside_hc_the_terminal_keeps_the_progress() {
    let Some(hc) = hc_binary() else { return };
    let Some(tmux) = which("tmux") else {
        assert!(
            std::env::var_os("IOTA_HC_REQUIRED").is_none_or(|v| v != "1"),
            "IOTA_HC_REQUIRED=1 but no `tmux` on PATH"
        );
        println!("SKIP: no `tmux` on PATH; the tmux-inside-hc test did not run");
        return;
    };
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &[]).await;
    let sock = hcc.dir.path().join("t");
    let raw = hcc.dir.path().join("raw");
    let go = hcc.dir.path().join("go");
    let tm = |args: &[&str]| {
        std::process::Command::new(&tmux)
            .arg("-S")
            .arg(&sock)
            .args(args)
            .output()
            .expect("run tmux")
    };
    // The pane waits for the raw tap before iota starts, so its first bytes are captured.
    let pane = format!(
        "while [ ! -e '{}' ]; do sleep 0.1; done; {}",
        go.display(),
        script("")
    );
    hcc.spawn(
        "tmux",
        &format!(
            "'{}' -S '{}' -f /dev/null new-session -s s \"{pane}\"",
            tmux.display(),
            sock.display()
        ),
    )
    .await;
    let start = Instant::now();
    while !tm(&["has-session", "-t", "s"]).status.success() {
        assert!(start.elapsed() < DEADLINE, "tmux never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let piped = tm(&[
        "pipe-pane",
        "-t",
        "s",
        "-o",
        &format!("cat >> '{}'", raw.display()),
    ]);
    assert!(piped.status.success(), "pipe-pane: {piped:?}");
    std::fs::write(&go, "").unwrap();

    let has = |needle: &[u8]| {
        std::fs::read(&raw).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
    };
    // Up: the input box is drawn. Then a turn, typed into the pane.
    let start = Instant::now();
    while !String::from_utf8_lossy(&tm(&["capture-pane", "-p", "-t", "s"]).stdout).contains('>') {
        assert!(start.elapsed() < DEADLINE, "iota never came up in tmux");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        tm(&["send-keys", "-t", "s", "stream 10 100", "Enter"])
            .status
            .success()
    );
    let start = Instant::now();
    while !has(b"\x1b]9;4;3\x07") {
        assert!(
            start.elapsed() < DEADLINE,
            "the pane never got OSC 9;4 busy"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !has(b"\x1b]7501;state="),
        "iota reported to a terminal that is tmux"
    );
    let got = hcc.get("tmux").await;
    assert_eq!(got["reported"], false, "hc heard iota through tmux: {got}");
    tm(&["kill-server"]);
}

/// A shell-tool call that runs `iota -m` (the chat's child) leaves the chat's record alone: the child
/// gets no `HC_TERMINAL_ID`, so it neither asks the tty nor reports, and the record is still the
/// chat's `working` after the child is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_headless_iota_leaves_the_chat_s_record() {
    let Some(hc) = hc_binary() else { return };
    let port = crate::mock::start().expect("mock provider");
    let hcc = Core::start(hc, &config(port), &[]).await;
    hcc.spawn("chat", &script("")).await;
    hcc.until_state("chat", "idle").await;
    let child = format!(
        "runfg:{} -m 'stream 5 50' > child.txt 2>&1; sleep 4",
        env!("CARGO_BIN_EXE_iota")
    );
    let (code, out) = hcc.send("chat", &child).await;
    assert_eq!(code, Some(0), "hc send: {out}");
    hcc.until_state("chat", "blocked").await;
    hcc.run(&["keys", "chat", "enter"]).await;
    let reply = hcc.cwd().join("child.txt");
    let start = Instant::now();
    while !std::fs::read_to_string(&reply).is_ok_and(|r| r.contains("l#04 line")) {
        if start.elapsed() > DEADLINE {
            let screen = hcc.run(&["read", "chat"]).await.1;
            panic!(
                "the nested iota never replied: {:?}\nscreen:\n{screen}",
                std::fs::read_to_string(&reply)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The child is done; the call's `sleep` still runs, so the chat is still in its turn.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = hcc.get("chat").await;
    assert_eq!(got["status"]["app"], "iota", "{got}");
    assert_eq!(got["status"]["state"], "working", "{got}");
    hcc.until_state("chat", "idle").await;
    hcc.run(&["keys", "chat", "ctrl+d"]).await;
    hcc.until("chat", "the record cleared", |g| g["status"].is_null())
        .await;
}
