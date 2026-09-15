//! The `shell` toolset (`tool/shell_test.go`) and its execution mechanism (`internal/shell/shell_test.go`).
//!
//! Every test that runs a command line drives a REAL child. Most of those command lines are POSIX, so they
//! ask [`skip_unless_posix`] first: on Unix, and on any Windows machine with Git Bash, that is always yes and
//! the test runs; where the interpreter resolved to PowerShell or `cmd.exe` the test prints a `SKIP:` line
//! instead of failing on a script that shell cannot parse. The two sandbox tests probe the platform sandbox
//! the same way (no sandbox binary, or a nested sandbox that refuses to nest — and Windows has none at all),
//! unless `IOTA_SANDBOX_REQUIRED=1` is set, which turns that skip into a red test naming what is missing
//! (`ci.sh` sets it: a machine without bubblewrap fails the gate instead of passing it quietly).

// The ★ WP00 fixture is included directly: `mod common;` would also pull in the MCP/stub fixtures this file
// never uses, and `tests/common/mod.rs` (not ours to edit) does not allow `unused_imports`.
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use iota::provider::model::JsonObject;
use iota::shell::exec;
use iota::shell::exec::{
    CappedBuffer, HEAD_BYTES, MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, Options, Outcome, RunResult,
    Sandbox, TAIL_BYTES, truncate_output, writable_paths,
};
use iota::shell::interp::{Family, Interpreter};
use iota::tool::Registry;
use iota::tool::context::RunCtx;
use iota::tool::sets::{RawNode, SetError, ToolsConfig};
use iota::tool::shell::{
    BASH_DESC_PREFIX, CMD_DESC_PREFIX, PWSH_DESC_PREFIX, SHELL_DESC_SANDBOXED,
    SHELL_DESC_UNSANDBOXED, SHELL_TOOL_NAME, background_desc, desc_prefix, new_shell_set,
};
use iota::tool::{Dispatcher, Tool, ToolEnv};
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::common::temp_project;

/// What this machine resolved to: `bash` on Unix, and on Windows whatever `shell::interp`'s ladder found
/// (Git Bash on a developer box and on CI, PowerShell or `cmd.exe` elsewhere).
pub(crate) fn shell() -> Interpreter {
    iota::shell::interp::resolve().expect("this machine has no shell interpreter at all")
}

/// Whether to skip a test whose command line is POSIX, printing Go's `t.Skipf` line when it is.
///
/// The question is about the INTERPRETER, never the platform: Git Bash makes `sleep 30` a runnable command
/// line on Windows, and that is the whole of what this answers. A test that also needs the POSIX PROCESS
/// MODEL — a process group to signal, a `ps` that can be asked about a pid — carries a second requirement
/// this gate does not cover and has to say so itself (`jobs::kill_all_stops_everything_at_once`).
pub(crate) fn skip_unless_posix(test: &str) -> bool {
    if shell().is_posix() {
        return false;
    }
    println!(
        "SKIP: {test} — the resolved interpreter is {}, not a POSIX shell",
        shell().program.display()
    );
    true
}

/// A path as the POSIX shell running the command will read it: on Windows an absolute path's backslashes
/// are escape characters to Git Bash, and `C:/…` is the spelling it wants instead.
pub(crate) fn shell_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The `shell:` config value as a YAML node; `""` is Go's zero `yaml.Node` (set defaults).
fn node(yaml: &str) -> Option<RawNode> {
    if yaml.is_empty() {
        return None;
    }
    Some(serde_norway::from_str(yaml).expect("yaml"))
}

/// The body of a provider's `tools:` block.
fn raw_tools(yaml: &str) -> ToolsConfig {
    #[derive(serde::Deserialize)]
    struct Raw {
        tools: ToolsConfig,
    }
    serde_norway::from_str::<Raw>(yaml).expect("yaml").tools
}

/// An `ToolEnv` rooted in a temp project (the host shape: project root + injected host dirs, never the process
/// environment).
fn shell_env() -> (TempDir, ToolEnv, PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let root = dir.path().to_path_buf();
    let env = ToolEnv {
        project_root: Some(root.clone()),
        dirs,
        ..ToolEnv::default()
    };
    (dir, env, root)
}

/// Go's `newBash`: the shell set over a temp project root with the given `shell:` config.
fn new_shell(cfg_yaml: &str) -> (TempDir, PathBuf, Arc<dyn Tool>) {
    let (dir, env, root) = shell_env();
    let tools = new_shell_set(&env, node(cfg_yaml).as_ref()).expect("shell set");
    assert_eq!(tools.len(), 1, "shell set must build exactly one tool");
    assert_eq!(tools[0].def().name, SHELL_TOOL_NAME);
    let tool = Arc::clone(&tools[0]);
    (dir, root, tool)
}

/// Go's `call(t, tool, args)`: `(text, is_error)`.
async fn call(tool: &Arc<dyn Tool>, args: serde_json::Value) -> (String, bool) {
    let args: JsonObject = match args {
        serde_json::Value::Object(m) => m,
        _ => panic!("object literal expected"),
    };
    let cx = RunCtx::default();
    let out = tool
        .call(&cx, &args)
        .await
        .expect("the shell tool never hard-fails");
    (out.text, out.is_error)
}

/// A sandbox over a fresh fixture directory OUTSIDE the system temp dir (the injected `temp_dir`/`cache_dir` and
/// the literal `/tmp` are always writable, so a probe target under them would prove nothing).
fn sandbox_fixture() -> (TempDir, PathBuf, PathBuf, Sandbox) {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).expect("target tmp dir");
    let dir = tempfile::tempdir_in(&base).expect("fixture dir");
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    let temp = dir.path().join("tmp");
    let cache = dir.path().join("cache");
    for p in [&root, &outside, &temp, &cache] {
        fs::create_dir_all(p).expect("fixture subdirectory");
    }
    let sb = Sandbox {
        root: root.clone(),
        network: false,
        write: Vec::new(),
        temp_dir: temp,
        cache_dir: Some(cache),
    };
    (dir, root, outside, sb)
}

/// Whether to skip a test that needs the OS sandbox, printing Go's `t.Skipf` line when it does: there is no
/// sandbox on this platform, or `echo probe` inside one does not run here (a sandbox inside a sandbox, or
/// bubblewrap without user namespaces). Under `IOTA_SANDBOX_REQUIRED` the same miss is a panic that names it.
async fn skip_unless_sandboxed(test: &str, sb: &Sandbox, dir: &Path) -> bool {
    let missing = if !exec::available() {
        "no OS sandbox on this platform (sandbox-exec on macOS, bwrap on PATH on Linux)"
    } else if !sandbox_runs(sb, dir).await {
        "the sandbox does not run in this environment (nested, or bwrap without user namespaces)"
    } else {
        return false;
    };
    assert!(
        std::env::var_os("IOTA_SANDBOX_REQUIRED").is_none(),
        "{test}: IOTA_SANDBOX_REQUIRED is set and {missing}"
    );
    println!("SKIP: {test} — {missing}");
    true
}

/// Runs `echo probe` inside the sandbox; false when this environment cannot nest one.
async fn sandbox_runs(sb: &Sandbox, dir: &Path) -> bool {
    let res = run(Options {
        command: "echo probe".to_owned(),
        dir: dir.to_path_buf(),
        timeout: None,
        sandbox: Some(sb.clone()),
    })
    .await;
    res.outcome == Outcome::Exited(0)
}

/// `shell.Run` under a token nobody cancels.
async fn run(opts: Options) -> RunResult {
    exec::run(&CancellationToken::new(), opts).await
}
#[tokio::test]
async fn the_shell_tool_runs_a_command_and_reports_its_output_and_status() {
    if skip_unless_posix("test_shell_call") {
        return;
    }
    let (_dir, root, tool) = new_shell("sandbox: off\n");

    let (out, is_err) = call(&tool, json!({"command": "echo hello | tr a-z A-Z"})).await;
    assert!(
        !is_err && out.contains("HELLO"),
        "pipe failed: ({out:?}, {is_err})"
    );

    // cwd defaults to the project root; a relative cwd resolves against it.
    fs::create_dir(root.join("sub")).expect("mkdir sub");
    let (out, _) = call(&tool, json!({"command": "pwd"})).await;
    let leaf = root.file_name().expect("root name").to_string_lossy();
    assert!(
        out.contains(leaf.as_ref()),
        "default cwd = {out:?}, want the project root"
    );
    let (out, _) = call(&tool, json!({"command": "pwd", "cwd": "sub"})).await;
    assert!(
        out.trim().ends_with("/sub"),
        "relative cwd = {out:?}, want .../sub"
    );

    // Exit codes and empty output are reported model-facing.
    let (out, is_err) = call(&tool, json!({"command": "exit 3"})).await;
    assert!(
        is_err && out.contains("[exit code 3]"),
        "exit code = ({out:?}, {is_err})"
    );
    let (out, is_err) = call(&tool, json!({"command": "true"})).await;
    assert_eq!(
        (out.as_str(), is_err),
        ("[command produced no output]", false)
    );
    let (out, is_err) = call(&tool, json!({})).await;
    assert!(
        is_err && out.contains("missing required argument"),
        "missing command = ({out:?}, {is_err})"
    );
}

// New (DIVERGENCES X-06): the `timeout` argument end to end — it caps the run, its own line names
// the number the call chose, and a value outside 1…3600 is refused BEFORE anything is executed.
#[tokio::test]
async fn shell_timeout_argument_caps_the_call() {
    if skip_unless_posix("shell_timeout_argument_caps_the_call") {
        return;
    }
    let (_dir, root, tool) = new_shell("sandbox: off\n");

    let started = std::time::Instant::now();
    let (out, is_err) = call(&tool, json!({"command": "sleep 30", "timeout": 1})).await;
    assert!(
        is_err && out.contains("[command timed out after 1s]"),
        "timed-out result = ({out:?}, {is_err})"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the call outlived its own timeout"
    );

    // Out of range: the refusal is the result and the command never ran.
    let marker = root.join("ran.txt");
    for bad in [json!(0), json!(-1), json!(3601), json!("30")] {
        let cmd = format!("touch {}", shell_path(&marker));
        let (out, is_err) = call(&tool, json!({"command": cmd, "timeout": bad})).await;
        assert_eq!(
            (out.as_str(), is_err),
            ("timeout must be between 1 and 3600 seconds", true),
            "timeout {bad} must be refused"
        );
        assert!(!marker.exists(), "a refused call ran the command anyway");
    }

    // An accepted one does run it.
    let cmd = format!("touch {}", shell_path(&marker));
    let (_, is_err) = call(&tool, json!({"command": cmd, "timeout": 30})).await;
    assert!(!is_err && marker.exists());
}

/// The shell set over a temp project root WITH a job registry bound (what both entry points build).
fn new_shell_with_jobs(cfg_yaml: &str) -> (TempDir, Arc<iota::shell::jobs::Jobs>, Arc<dyn Tool>) {
    let (dir, mut env, _root) = shell_env();
    let jobs = iota::shell::jobs::Jobs::new(dir.path());
    env.jobs = Some(Arc::clone(&jobs));
    let tools = new_shell_set(&env, node(cfg_yaml).as_ref()).expect("shell set");
    (dir, jobs, Arc::clone(&tools[0]))
}

// New (DIVERGENCES X-07): `background: true` returns a receipt instead of the output — the job id the
// notice will carry, the pid, and the file to tail — and the turn is free while the command runs.
#[tokio::test]
async fn shell_background_returns_a_receipt_and_keeps_running() {
    if skip_unless_posix("shell_background_returns_a_receipt_and_keeps_running") {
        return;
    }
    let (dir, jobs, tool) = new_shell_with_jobs("sandbox: off\nauto_run: true\n");
    let marker = dir.path().join("done.txt");
    let cmd = format!("sleep 0.2; echo finished > {}", shell_path(&marker));

    let started = std::time::Instant::now();
    let (out, is_err) = call(&tool, json!({"command": cmd, "background": true})).await;
    assert!(!is_err, "background start failed: {out}");
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "the call waited for the job: {:?}",
        started.elapsed()
    );
    assert!(out.starts_with("Started background job b1 (pid "), "{out}");
    assert!(out.contains("b1.log"), "{out}");
    assert!(out.contains("tail -n 50 "), "{out}");
    assert_eq!(jobs.running(), 1, "the job must still be running");

    // It really ran, and it lands as ONE completion.
    let cancel = CancellationToken::new();
    let done = jobs.wait_any(&cancel).await.expect("a completion");
    assert_eq!(done.id, "b1");
    assert_eq!(done.exit, Some(0));
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        "finished\n"
    );
    assert!(jobs.wait_any(&cancel).await.is_none());
}

// The argument checks are the foreground ones, in the same order: a bad `timeout` is refused before
// anything is started, and a run with no registry refuses rather than silently blocking the turn.
#[tokio::test]
async fn shell_background_keeps_the_argument_rules() {
    let (_dir, jobs, tool) = new_shell_with_jobs("sandbox: off\nauto_run: true\n");
    let (out, is_err) = call(
        &tool,
        json!({"command": "true", "background": true, "timeout": 0}),
    )
    .await;
    assert_eq!(
        (out.as_str(), is_err),
        ("timeout must be between 1 and 3600 seconds", true)
    );
    assert_eq!(jobs.running(), 0, "a refused call must start nothing");

    let (out, is_err) = call(&tool, json!({"command": "  ", "background": true})).await;
    assert!(is_err && out.contains("missing required argument"), "{out}");

    // Without the host seam a background call is refused, never run in the foreground — the model asked
    // NOT to wait for it.
    let (_dir, _root, seamless) = new_shell("sandbox: off\nauto_run: true\n");
    let (out, is_err) = call(&seamless, json!({"command": "true", "background": true})).await;
    assert_eq!(
        (out.as_str(), is_err),
        ("background jobs are not available in this run", true)
    );
}

// The header names the mode: a row that settles while its command is still going has to say so.
#[test]
fn shell_background_header_is_marked() {
    let (_dir, _jobs, tool) = new_shell_with_jobs("sandbox: off\n");
    let args = |v: serde_json::Value| -> JsonObject {
        match v {
            serde_json::Value::Object(m) => m,
            _ => panic!("object literal expected"),
        }
    };
    assert_eq!(
        tool.header_summary(&args(json!({"command": "make test", "background": true}))),
        Some("(background) make test".to_owned())
    );
    assert_eq!(
        tool.header_summary(&args(json!({"command": "make test"}))),
        Some("make test".to_owned())
    );
}

// New (DIVERGENCES X-05): the `shell` tool opts every call into the round's parallel batch, whatever it was
// asked to run — the answer is not a property of the arguments.
#[test]
fn shell_calls_batch() {
    let (_dir, _root, tool) = new_shell("sandbox: off\n");
    assert!(
        tool.supports_parallel(None),
        "the nil-args probe must say yes"
    );
    for args in [
        json!({"command": "ls"}),
        json!({"command": "rm -rf /tmp/x"}),
        json!({}),
    ] {
        let args: JsonObject = match args {
            serde_json::Value::Object(m) => m,
            _ => panic!("object literal expected"),
        };
        assert!(tool.supports_parallel(Some(&args)));
    }
}
#[test]
fn shell_approval_follows_the_auto_run_and_write_matrix() {
    let approval = |cfg: &str| {
        let (_dir, _root, tool) = new_shell(cfg);
        tool.requires_approval()
    };
    assert!(
        approval("sandbox: off\n"),
        "an unsandboxed shell tool should require approval"
    );
    assert!(
        !approval("sandbox: off\nauto_run: true\n"),
        "auto_run should waive approval"
    );
    // auto: approval exactly when no sandbox is available.
    assert_eq!(
        approval(""),
        !exec::available(),
        "auto approval must mirror exec::available()"
    );
}
#[test]
fn a_bad_shell_config_names_its_fault() {
    // The pre-shell allow-list shape is no longer valid: warn and skip the set.
    let (_dir, env, _root) = shell_env();
    let mut warned = Vec::new();
    let r = Registry::build(
        &env,
        &raw_tools("tools:\n  shell:\n    - git\n"),
        &mut |w| {
            warned.push(w);
        },
    );
    assert!(
        r.is_empty() && warned.len() == 1,
        "legacy list config: tools={:?} warned={warned:?}, want skip+1 warning",
        r.tools()
    );
    assert!(
        warned[0].starts_with(
            "toolset \"shell\": config must be a mapping (sandbox, network, auto_run, write): "
        ) && warned[0].ends_with(" (ignored)"),
        "{:?}",
        warned[0]
    );

    let Err(err) = new_shell_set(&env, node("sandbox: bogus\n").as_ref()) else {
        panic!("invalid sandbox mode should error");
    };
    assert_eq!(err, SetError::BadSandbox("bogus".to_owned()));
    assert_eq!(
        err.to_string(),
        "sandbox must be \"auto\" or \"off\", got \"bogus\""
    );

    // An empty `sandbox:` falls back to auto, and both spellings are accepted.
    for cfg in [
        "sandbox: \"\"\n",
        "sandbox: auto\n",
        "sandbox: off\n",
        "{}\n",
    ] {
        assert!(new_shell_set(&env, node(cfg).as_ref()).is_ok(), "{cfg:?}");
    }
}
#[tokio::test]
async fn a_shell_key_enables_the_shell_tool() {
    let (_dir, env, _root) = shell_env();
    let mut warned: Vec<String> = Vec::new();
    let r = Registry::build(
        &env,
        &raw_tools("tools:\n  shell:\n    sandbox: off\n"),
        &mut |w| warned.push(w),
    );
    let defs = r.tools();
    assert_eq!(
        defs.len(),
        1,
        "expected the shell tool enabled, got {defs:?}"
    );
    assert_eq!(defs[0].name, SHELL_TOOL_NAME);
    assert!(warned.is_empty(), "unexpected warnings: {warned:?}");

    let mut args = JsonObject::new();
    args.insert("command".to_owned(), json!("echo registry"));
    let out = r
        .call_tool(&RunCtx::default(), SHELL_TOOL_NAME, args)
        .await
        .expect("the shell tool via registry");
    assert!(
        !out.is_error && out.text.contains("registry"),
        "the shell tool via registry: {out:?}"
    );
    // The registry routes the tool's approval answer (sandbox: off, no auto_run).
    assert!(r.requires_approval(SHELL_TOOL_NAME));
}
#[test]
fn the_shell_description_states_the_shell_state_contract() {
    // The contract is the same whichever interpreter runs the calls; the DIALECT it teaches is that
    // interpreter's own, and the first sentence names it. All three are checked on every platform — the
    // description is the model's only source for both facts, and it must never describe a different shell
    // from the one that will read the script.
    for (family, head, opener, dialect, detach) in [
        (
            Family::Posix,
            BASH_DESC_PREFIX,
            "Run a bash command line",
            "pipes, redirects, globbing, && chaining, heredocs",
            "(nohup/setsid).",
        ),
        (
            Family::PowerShell,
            PWSH_DESC_PREFIX,
            "Run a PowerShell command line",
            "NOT a POSIX shell",
            "(Start-Process).",
        ),
        (
            Family::Cmd,
            CMD_DESC_PREFIX,
            "Run a cmd.exe command line",
            "neither bash nor PowerShell",
            "(start /b).",
        ),
    ] {
        assert_eq!(desc_prefix(family), head, "{family:?}");
        let background = background_desc(family);
        let sandboxed_blocked = format!(
            "{head}{}{background}",
            SHELL_DESC_SANDBOXED.replace("{net}", "network access is BLOCKED")
        );
        let sandboxed_open = format!(
            "{head}{}{background}",
            SHELL_DESC_SANDBOXED.replace("{net}", "network access is allowed")
        );
        let unsandboxed = format!("{head}{SHELL_DESC_UNSANDBOXED}{background}");
        for desc in [&sandboxed_blocked, &sandboxed_open, &unsandboxed] {
            for want in [
                opener,
                dialect,
                "FRESH shell",
                "do not carry over",
                // The two post-parity facts the model has to know (DIVERGENCES X-05/X-06).
                "Calls issued together run concurrently.",
                "killed after 600 seconds",
                "maximum 3600",
                // The background mode, its notice and its lifetime (phase C).
                "\"background\": true",
                "Up to 16 background jobs at a time",
                "killed when iota exits",
            ] {
                assert!(desc.contains(want), "description missing {want:?}:\n{desc}");
            }
            assert!(
                desc.starts_with(opener),
                "the interpreter must be named in the first sentence:\n{desc}"
            );
            assert!(
                desc.ends_with(&format!("has to detach itself {detach}")),
                "the background paragraph must come last, in this dialect:\n{desc}"
            );
        }
        assert!(
            sandboxed_blocked.contains("network access is BLOCKED.\n\n"),
            "the sandbox suffix still precedes the background paragraph"
        );
        assert!(sandboxed_open.contains("network access is allowed.\n\n"));
        assert!(unsandboxed.contains("full permissions — be conservative.\n\n"));
    }

    // The live tool picks the prefix ITS interpreter dictates, the suffix its sandbox state dictates, and
    // the schema is tool/shell.go:130-143 with the one example rewritten in that same dialect.
    let (_dir, _root, tool) = new_shell("sandbox: off\n");
    let def = tool.def();
    let family = shell().family;
    assert_eq!(
        def.description,
        format!(
            "{}{SHELL_DESC_UNSANDBOXED}{}",
            desc_prefix(family),
            background_desc(family)
        )
    );
    assert!(!def.deferred);
    let command_desc = match family {
        Family::Posix => "Bash command line to execute, e.g. \'go test ./... 2>&1 | tail -20\'.",
        Family::PowerShell => {
            "PowerShell command line to execute, e.g. \'cargo test 2>&1 | Select-Object -Last 20\'."
        }
        Family::Cmd => "cmd.exe command line to execute, e.g. \'cargo test 2>&1 | more\'.",
    };
    assert_eq!(
        serde_json::Value::Object(def.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": command_desc,
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory (defaults to the project root).",
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional wall-clock cap in seconds (default 600, maximum 3600). The command is killed when it expires.",
                    "minimum": 1,
                    "maximum": 3600,
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the command in the background and return immediately with its job id and output file (default false).",
                },
            },
            "required": ["command"],
        })
    );
}
#[tokio::test]
async fn cancelling_a_run_kills_the_whole_process_tree() {
    if skip_unless_posix("test_run_cancel_kills_the_tree") {
        return;
    }
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        token.cancel();
    });
    let start = Instant::now();
    let res = exec::run(
        &cancel,
        Options {
            command: "sleep 30 & sleep 30 & wait".to_owned(),
            dir: PathBuf::new(),
            timeout: None,
            sandbox: None,
        },
    )
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "cancelled run took {elapsed:?}, want a prompt return"
    );
    assert!(
        res.outcome == Outcome::Cancelled,
        "result = {res:?}, want Cancelled"
    );
}
#[tokio::test]
async fn cancelling_a_sandboxed_run_kills_the_whole_process_tree() {
    let (_dir, root, _outside, sb) = sandbox_fixture();
    if skip_unless_sandboxed("test_run_cancel_kills_the_sandboxed_tree", &sb, &root).await {
        return;
    }
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        token.cancel();
    });
    let start = Instant::now();
    // The group kill must reach THROUGH the wrapper: the direct child is sandbox-exec / bwrap.
    let res = exec::run(
        &cancel,
        Options {
            command: "sleep 30".to_owned(),
            dir: root,
            timeout: None,
            sandbox: Some(sb),
        },
    )
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "cancelled sandboxed run took {elapsed:?}, want a prompt return"
    );
    assert!(
        res.outcome == Outcome::Cancelled,
        "result = {res:?}, want Cancelled"
    );
}
#[tokio::test]
async fn a_background_child_does_not_wedge_the_run() {
    if skip_unless_posix("test_run_background_child_does_not_wedge") {
        return;
    }
    let start = Instant::now();
    let res = run(Options {
        command: "sleep 30 & echo started".to_owned(),
        dir: PathBuf::new(),
        timeout: None,
        sandbox: None,
    })
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(15),
        "run with a lingering child took {elapsed:?}, want the WaitDelay bound"
    );
    assert!(
        res.outcome == Outcome::Exited(0) && res.output.contains("started"),
        "result = {res:?}, want Exited(0) with the foreground output"
    );
}
#[tokio::test]
async fn the_run_keeps_shell_semantics() {
    if skip_unless_posix("test_run_shell_semantics") {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path().to_path_buf();

    // Pipes, expansion, chaining — a real shell.
    let res = run(Options {
        command: "echo hello | tr a-z A-Z && x=5; echo $((x+1))".to_owned(),
        dir: dir.clone(),
        timeout: None,
        sandbox: None,
    })
    .await;
    assert!(
        res.outcome == Outcome::Exited(0)
            && res.output.contains("HELLO")
            && res.output.contains('6'),
        "shell semantics: {res:?}"
    );

    // Exit codes are reported, not turned into Err.
    let res = run(Options {
        command: "exit 3".to_owned(),
        dir: dir.clone(),
        timeout: None,
        sandbox: None,
    })
    .await;
    assert!(res.outcome == Outcome::Exited(3), "exit code: {res:?}");

    // The working directory applies (and PWD is exported to it).
    let res = run(Options {
        command: "pwd".to_owned(),
        dir: dir.clone(),
        timeout: None,
        sandbox: None,
    })
    .await;
    let base = dir.file_name().expect("dir name").to_string_lossy();
    assert!(res.output.contains(base.as_ref()), "cwd: {res:?}");

    // Combined output: stderr and stdout share ONE pipe, so interleaving is write order.
    let res = run(Options {
        command: "echo out; echo err 1>&2; echo out2".to_owned(),
        dir,
        timeout: None,
        sandbox: None,
    })
    .await;
    assert_eq!(res.output, "out\nerr\nout2\n", "{res:?}");
}
#[tokio::test]
async fn a_run_cancelled_before_it_starts_reports_cancelled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let res = exec::run(
        &cancel,
        Options {
            command: "echo hi".to_owned(),
            dir: dir.path().to_path_buf(),
            timeout: None,
            sandbox: None,
        },
    )
    .await;
    assert!(
        res.outcome == Outcome::Cancelled && res.output.is_empty(),
        "pre-cancelled token should report Cancelled: {res:?}"
    );
}
#[tokio::test]
async fn the_sandbox_isolates_the_filesystem_and_the_network() {
    let (_dir, root, outside, sb) = sandbox_fixture();
    if skip_unless_sandboxed("test_sandbox_isolation", &sb, &root).await {
        return;
    }

    // Writes inside the project root succeed.
    let res = run(Options {
        command: "echo data > inside.txt && cat inside.txt".to_owned(),
        dir: root.clone(),
        timeout: None,
        sandbox: Some(sb.clone()),
    })
    .await;
    assert!(
        res.outcome == Outcome::Exited(0) && res.output.contains("data"),
        "in-root write failed: {res:?}"
    );

    // Writes outside the writable roots are denied (Go probes a fresh dir under $HOME; the port uses a
    // sibling of the root, since HOME cannot be read by a test and the temp dirs ARE writable).
    let res = run(Options {
        command: format!("echo x > {}/f.txt", shell_path(&outside)),
        dir: root.clone(),
        timeout: None,
        sandbox: Some(sb.clone()),
    })
    .await;
    assert!(
        res.outcome != Outcome::Exited(0),
        "outside-root write should be denied: {res:?}"
    );
    assert!(!outside.join("f.txt").exists(), "the write landed anyway");

    // An extra Write root opens exactly that directory.
    let opened = Sandbox {
        write: vec![outside.clone()],
        ..sb
    };
    let res = run(Options {
        command: format!("echo y > {}/g.txt", shell_path(&outside)),
        dir: root,
        timeout: None,
        sandbox: Some(opened),
    })
    .await;
    assert!(
        res.outcome == Outcome::Exited(0),
        "configured write root should be writable: {res:?}"
    );
    assert!(outside.join("g.txt").exists());
}
#[test]
fn the_writable_paths_are_the_project_and_the_temp_dirs() {
    let (dir, dirs) = temp_project(&[]);
    let root = dir.path().to_path_buf();
    let cache = dirs.cache.clone().expect("fixture cache");
    let paths = writable_paths(&Sandbox {
        root: root.clone(),
        network: false,
        write: vec![root.clone(), PathBuf::new(), PathBuf::from("/tmp")],
        temp_dir: dirs.temp.clone(),
        cache_dir: Some(cache.clone()),
    });
    assert_eq!(paths[0], root, "project root must lead: {paths:?}");
    let mut seen = std::collections::HashSet::new();
    for p in &paths {
        assert!(
            seen.insert(p.clone()),
            "duplicate writable path {p:?}: {paths:?}"
        );
        assert!(!p.as_os_str().is_empty(), "empty path in {paths:?}");
    }
    assert!(paths.contains(&PathBuf::from("/tmp")));
    assert!(paths.contains(&dirs.temp));
    assert!(
        paths.contains(&cache),
        "the cache dir is writable: {paths:?}"
    );
    assert!(cache.is_dir(), "the cache dir is created if missing");

    // Without a cache dir the list is just root, temp and /tmp (deduped).
    let paths = writable_paths(&Sandbox {
        root: root.clone(),
        network: false,
        write: Vec::new(),
        temp_dir: PathBuf::from("/tmp"),
        cache_dir: None,
    });
    assert_eq!(paths, vec![root, PathBuf::from("/tmp")]);
}
#[test]
fn truncate_output_keeps_head_and_tail_and_says_what_it_dropped() {
    // Byte cap: head and tail survive, the middle is elided.
    let head = "H".repeat(HEAD_BYTES);
    let tail = "T".repeat(TAIL_BYTES);
    let out = truncate_output(&format!("{head}{}{tail}", "M".repeat(10 * 1024)));
    assert!(
        out.starts_with('H') && out.ends_with('T'),
        "head/tail not preserved"
    );
    assert!(
        out.contains("bytes omitted") && !out.contains('M'),
        "middle should be dropped with a marker"
    );

    // Line cap: many short lines are elided by count, first and last kept.
    let mut b = String::new();
    for i in 1..=3 * MAX_OUTPUT_LINES {
        let _ = writeln!(b, "line-{i}");
    }
    let out = truncate_output(&b);
    assert!(
        out.contains("lines omitted"),
        "line marker missing:\n{}",
        &out[..200]
    );
    assert!(
        out.contains("line-1\n") && out.contains(&format!("line-{}", 3 * MAX_OUTPUT_LINES)),
        "first/last lines not preserved"
    );
    let n = out.matches('\n').count();
    assert!(n <= MAX_OUTPUT_LINES + 2, "still {n} lines after the cap");
    assert!(out.contains(
        "\n[... 1025 lines omitted — pipe through head/tail/grep to narrow the output ...]\n"
    ));

    // Small output passes through untouched.
    assert_eq!(truncate_output("short output"), "short output");
}
#[test]
fn the_capped_buffer_stops_at_its_cap() {
    let mut b = CappedBuffer::default();
    b.write(b"start-");
    let chunk = "x".repeat(8 * 1024);
    for _ in 0..40 {
        // ~320KB through a ~52KB window
        b.write(chunk.as_bytes());
    }
    b.write(b"-end");
    let out = b.into_string();
    assert!(
        out.starts_with("start-") && out.ends_with("-end"),
        "stream head/tail not preserved"
    );
    assert!(out.contains("bytes omitted"), "missing omission marker");
    assert!(
        out.len() <= HEAD_BYTES + TAIL_BYTES + 64,
        "reassembled {} bytes, want ≈ head+tail",
        out.len()
    );

    // Small writes pass through exactly.
    let mut s = CappedBuffer::default();
    s.write(b"hello ");
    s.write(b"world");
    assert_eq!(s.into_string(), "hello world");
}
#[tokio::test]
async fn a_runs_output_is_capped() {
    if skip_unless_posix("test_run_output_capped") {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let res = run(Options {
        command: "seq 1 50000".to_owned(),
        dir: dir.path().to_path_buf(),
        timeout: None,
        sandbox: None,
    })
    .await;
    assert!(res.outcome == Outcome::Exited(0), "seq failed: {res:?}");
    assert!(
        res.output.len() <= MAX_OUTPUT_BYTES + 2048,
        "output {} bytes, want ≈ ≤ {MAX_OUTPUT_BYTES}",
        res.output.len()
    );
    assert!(
        res.output.contains("omitted") && res.output.contains("50000"),
        "capped output should keep the tail and a marker:\n{}",
        &res.output[..200]
    );
}

// New (CONTRACTS §4.7 classification order): the deadline reports TimedOut, and a cancelled token that fires
// first reports Cancelled — the two are never both set.
#[tokio::test]
async fn run_deadline_and_cancel_are_exclusive() {
    if skip_unless_posix("run_deadline_and_cancel_are_exclusive") {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let start = Instant::now();
    let res = run(Options {
        command: "sleep 30".to_owned(),
        dir: dir.path().to_path_buf(),
        timeout: Some(Duration::from_millis(200)),
        sandbox: None,
    })
    .await;
    assert!(res.outcome == Outcome::TimedOut, "deadline: {res:?}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the group was killed"
    );

    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        token.cancel();
    });
    let res = exec::run(
        &cancel,
        Options {
            command: "sleep 30".to_owned(),
            dir: dir.path().to_path_buf(),
            timeout: Some(Duration::from_secs(600)),
            sandbox: None,
        },
    )
    .await;
    assert!(
        res.outcome == Outcome::Cancelled,
        "cancel under a long deadline: {res:?}"
    );
}

// New (DIVERGENCES D-16): a bad cwd is a spawn failure, reported as `failed to run: <io error>` with the
// Rust text (Go says `chdir …`), and it never becomes an exit code.
#[tokio::test]
async fn shell_bad_cwd_reports_failed_to_run() {
    let (_dir, _root, tool) = new_shell("sandbox: off\n");
    let (out, is_err) = call(
        &tool,
        json!({"command": "pwd", "cwd": "/nonexistent-dir-xyz-iota"}),
    )
    .await;
    assert!(is_err, "{out:?}");
    assert!(out.starts_with("failed to run: "), "{out:?}");
    assert!(!out.contains("[exit code"), "{out:?}");
}

// For the shell tool the command IS the call: no
// `command:` label, a width budget that fits a real pipeline, the first line only, and an explicit
// cwd folded into the shell idiom for it.
#[tokio::test]
async fn the_shell_header_is_the_first_command_line_within_budget() {
    let (_dir, root, tool) = new_shell("sandbox: off\n");
    let deploy = root.join("deploy");
    let args = |v: serde_json::Value| -> JsonObject {
        v.as_object().cloned().expect("object literal expected")
    };

    for (name, arg, want) in [
        ("plain", json!({"command": "git status"}), "git status"),
        (
            "a real pipeline survives the old 24-column budget",
            json!({"command": "go test ./... 2>&1 | tail -20"}),
            "go test ./... 2>&1 | tail -20",
        ),
        ("trimmed", json!({"command": "  ls -la  "}), "ls -la"),
        ("missing", json!({}), ""),
        (
            "cwd folds into cd, in the running interpreter's own idiom",
            json!({"command": "make release", "cwd": deploy.to_string_lossy()}),
            match shell().family {
                Family::Posix => "cd deploy && make release",
                Family::PowerShell => "cd deploy; make release",
                Family::Cmd => "cd /d deploy && make release",
            },
        ),
    ] {
        assert_eq!(
            tool.header_summary(&args(arg)).as_deref(),
            Some(want),
            "{name}"
        );
    }
}
