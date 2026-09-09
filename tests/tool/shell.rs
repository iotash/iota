//! The `shell` toolset (`tool/shell_test.go`) and its execution mechanism (`internal/shell/shell_test.go`).
//!
//! Every test drives a real `bash`. The two sandbox tests probe the platform sandbox first and print a `SKIP:`
//! line instead of failing where it cannot run (no sandbox binary, or a nested sandbox that refuses to nest).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The ★ WP00 fixture is included directly: `mod common;` would also pull in the MCP/stub fixtures this file
// never uses, and `tests/common/mod.rs` (not ours to edit) does not allow `unused_imports`.
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use iota::chat::turns::RunCtx;
use iota::provider::model::JsonObject;
use iota::shell::exec;
use iota::shell::exec::{
    CappedBuffer, HEAD_BYTES, MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, Options, RunResult, Sandbox,
    TAIL_BYTES, truncate_output, writable_paths,
};
use iota::tool::Registry;
use iota::tool::sets::{RawNode, SetError, ToolsConfig};
use iota::tool::shell::{
    BASH_DESC_PREFIX, BASH_DESC_SANDBOXED, BASH_DESC_UNSANDBOXED, new_shell_set,
};
use iota::tool::{Dispatcher, Env, Tool};
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::common::temp_project;

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

/// An `Env` rooted in a temp project (the host shape: project root + injected host dirs, never the process
/// environment).
fn shell_env() -> (TempDir, Env, PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let root = dir.path().to_path_buf();
    let env = Env {
        project_root: Some(root.clone()),
        dirs,
        ..Env::default()
    };
    (dir, env, root)
}

/// Go's `newBash`: the shell set over a temp project root with the given `shell:` config.
fn new_bash(cfg_yaml: &str) -> (TempDir, PathBuf, Arc<dyn Tool>) {
    let (dir, env, root) = shell_env();
    let tools = new_shell_set(&env, node(cfg_yaml).as_ref()).expect("shell set");
    assert_eq!(tools.len(), 1, "shell set must build exactly one tool");
    assert_eq!(tools[0].def().name, "bash");
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
    let out = tool.call(&cx, &args).await.expect("bash never hard-fails");
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

/// Runs `echo probe` inside the sandbox; false when this environment cannot nest one (Go's `t.Skipf`).
async fn sandbox_runs(sb: &Sandbox, dir: &Path) -> bool {
    let res = run(Options {
        command: "echo probe".to_owned(),
        dir: dir.to_path_buf(),
        timeout: None,
        sandbox: Some(sb.clone()),
    })
    .await;
    res.err.is_none() && res.exit_code == 0
}

/// `shell.Run` under a token nobody cancels.
async fn run(opts: Options) -> RunResult {
    exec::run(&CancellationToken::new(), opts).await
}

// Go: tool/shell_test.go:30
#[tokio::test]
async fn test_bash_call() {
    let (_dir, root, bash) = new_bash("sandbox: off\n");

    let (out, is_err) = call(&bash, json!({"command": "echo hello | tr a-z A-Z"})).await;
    assert!(
        !is_err && out.contains("HELLO"),
        "pipe failed: ({out:?}, {is_err})"
    );

    // cwd defaults to the project root; a relative cwd resolves against it.
    fs::create_dir(root.join("sub")).expect("mkdir sub");
    let (out, _) = call(&bash, json!({"command": "pwd"})).await;
    let leaf = root.file_name().expect("root name").to_string_lossy();
    assert!(
        out.contains(leaf.as_ref()),
        "default cwd = {out:?}, want the project root"
    );
    let (out, _) = call(&bash, json!({"command": "pwd", "cwd": "sub"})).await;
    assert!(
        out.trim().ends_with("/sub"),
        "relative cwd = {out:?}, want .../sub"
    );

    // Exit codes and empty output are reported model-facing.
    let (out, is_err) = call(&bash, json!({"command": "exit 3"})).await;
    assert!(
        is_err && out.contains("[exit code 3]"),
        "exit code = ({out:?}, {is_err})"
    );
    let (out, is_err) = call(&bash, json!({"command": "true"})).await;
    assert_eq!(
        (out.as_str(), is_err),
        ("[command produced no output]", false)
    );
    let (out, is_err) = call(&bash, json!({})).await;
    assert!(
        is_err && out.contains("missing required argument"),
        "missing command = ({out:?}, {is_err})"
    );
}

// Go: tool/shell_test.go:66
#[test]
fn test_bash_approval_matrix() {
    let approval = |cfg: &str| {
        let (_dir, _root, bash) = new_bash(cfg);
        bash.requires_approval()
    };
    assert!(
        approval("sandbox: off\n"),
        "unsandboxed bash should require approval"
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

// Go: tool/shell_test.go:87
#[test]
fn test_shell_set_config_errors() {
    // The pre-bash allow-list shape is no longer valid: warn and skip the set.
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

// Go: tool/shell_test.go:133 (the "shell set enables bash" subtest of TestBuildRegistry)
#[tokio::test]
async fn test_build_registry_shell_set_enables_bash() {
    let (_dir, env, _root) = shell_env();
    let mut warned: Vec<String> = Vec::new();
    let r = Registry::build(
        &env,
        &raw_tools("tools:\n  shell:\n    sandbox: off\n"),
        &mut |w| warned.push(w),
    );
    let defs = r.tools();
    assert_eq!(defs.len(), 1, "expected bash enabled, got {defs:?}");
    assert_eq!(defs[0].name, "bash");
    assert!(warned.is_empty(), "unexpected warnings: {warned:?}");

    let mut args = JsonObject::new();
    args.insert("command".to_owned(), json!("echo registry"));
    let out = r
        .call_tool(&RunCtx::default(), "bash", args)
        .await
        .expect("bash via registry");
    assert!(
        !out.is_error && out.text.contains("registry"),
        "bash via registry: {out:?}"
    );
    // The registry routes the tool's approval answer (sandbox: off, no auto_run).
    assert!(r.requires_approval("bash"));
}

// Go: tool/shell_test.go:179
#[test]
fn test_bash_description_states_shell_state_contract() {
    let sandboxed_blocked = format!(
        "{BASH_DESC_PREFIX}{}",
        BASH_DESC_SANDBOXED.replace("{net}", "network access is BLOCKED")
    );
    let sandboxed_open = format!(
        "{BASH_DESC_PREFIX}{}",
        BASH_DESC_SANDBOXED.replace("{net}", "network access is allowed")
    );
    let unsandboxed = format!("{BASH_DESC_PREFIX}{BASH_DESC_UNSANDBOXED}");
    for desc in [&sandboxed_blocked, &sandboxed_open, &unsandboxed] {
        for want in ["FRESH shell", "functions", "do not carry over"] {
            assert!(desc.contains(want), "description missing {want:?}:\n{desc}");
        }
    }
    assert!(sandboxed_blocked.ends_with("network access is BLOCKED."));
    assert!(sandboxed_open.ends_with("network access is allowed."));
    assert!(unsandboxed.ends_with("full permissions — be conservative."));

    // The live tool picks the suffix its sandbox state dictates, and the schema is tool/shell.go:130-143.
    let (_dir, _root, bash) = new_bash("sandbox: off\n");
    let def = bash.def();
    assert_eq!(def.description, unsandboxed);
    assert!(!def.deferred);
    assert_eq!(
        serde_json::Value::Object(def.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Bash command line to execute, e.g. 'go test ./... 2>&1 | tail -20'.",
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory (defaults to the project root).",
                },
            },
            "required": ["command"],
        })
    );
}

// Go: internal/shell/shell_test.go:17
#[tokio::test]
async fn test_run_cancel_kills_the_tree() {
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
    assert!(res.cancelled, "result = {res:?}, want Cancelled");
}

// Go: internal/shell/shell_test.go:32
#[tokio::test]
async fn test_run_cancel_kills_the_sandboxed_tree() {
    if !exec::available() {
        println!("SKIP: test_run_cancel_kills_the_sandboxed_tree — no sandbox on this platform");
        return;
    }
    let (_dir, root, _outside, sb) = sandbox_fixture();
    if !sandbox_runs(&sb, &root).await {
        println!("SKIP: test_run_cancel_kills_the_sandboxed_tree — sandbox not runnable here");
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
    assert!(res.cancelled, "result = {res:?}, want Cancelled");
}

// Go: internal/shell/shell_test.go:52
#[tokio::test]
async fn test_run_background_child_does_not_wedge() {
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
        res.exited && res.output.contains("started"),
        "result = {res:?}, want Exited with the foreground output"
    );
    assert_eq!(res.exit_code, 0);
}

// Go: internal/shell/shell_test.go:68
#[tokio::test]
async fn test_run_shell_semantics() {
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
        res.err.is_none()
            && res.exit_code == 0
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
    assert!(
        res.err.is_none() && res.exited && res.exit_code == 3,
        "exit code: {res:?}"
    );

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

// Go: internal/shell/shell_test.go:90
#[tokio::test]
async fn test_run_cancelled() {
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
        res.cancelled && res.output.is_empty() && !res.exited,
        "pre-cancelled token should report Cancelled: {res:?}"
    );
}

// Go: internal/shell/shell_test.go:101
#[tokio::test]
async fn test_sandbox_isolation() {
    if !exec::available() {
        println!("SKIP: test_sandbox_isolation — no OS sandbox on this platform");
        return;
    }
    let (_dir, root, outside, sb) = sandbox_fixture();
    if !sandbox_runs(&sb, &root).await {
        println!("SKIP: test_sandbox_isolation — sandbox not runnable in this environment");
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
        res.exit_code == 0 && res.output.contains("data"),
        "in-root write failed: {res:?}"
    );

    // Writes outside the writable roots are denied (Go probes a fresh dir under $HOME; the port uses a
    // sibling of the root, since HOME cannot be read by a test and the temp dirs ARE writable).
    let res = run(Options {
        command: format!("echo x > {}/f.txt", outside.display()),
        dir: root.clone(),
        timeout: None,
        sandbox: Some(sb.clone()),
    })
    .await;
    assert!(
        res.exit_code != 0 || res.err.is_some(),
        "outside-root write should be denied: {res:?}"
    );
    assert!(!outside.join("f.txt").exists(), "the write landed anyway");

    // An extra Write root opens exactly that directory.
    let opened = Sandbox {
        write: vec![outside.clone()],
        ..sb
    };
    let res = run(Options {
        command: format!("echo y > {}/g.txt", outside.display()),
        dir: root,
        timeout: None,
        sandbox: Some(opened),
    })
    .await;
    assert!(
        res.exit_code == 0 && res.err.is_none(),
        "configured write root should be writable: {res:?}"
    );
    assert!(outside.join("g.txt").exists());
}

// Go: internal/shell/shell_test.go:141
#[test]
fn test_writable_paths() {
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

// Go: internal/shell/shell_test.go:156
#[test]
fn test_truncate_output() {
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

// Go: internal/shell/shell_test.go:191
#[test]
fn test_capped_buffer() {
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

// Go: internal/shell/shell_test.go:220
#[tokio::test]
async fn test_run_output_capped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let res = run(Options {
        command: "seq 1 50000".to_owned(),
        dir: dir.path().to_path_buf(),
        timeout: None,
        sandbox: None,
    })
    .await;
    assert!(
        res.err.is_none() && res.exit_code == 0,
        "seq failed: {res:?}"
    );
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
    let dir = tempfile::tempdir().expect("tempdir");
    let start = Instant::now();
    let res = run(Options {
        command: "sleep 30".to_owned(),
        dir: dir.path().to_path_buf(),
        timeout: Some(Duration::from_millis(200)),
        sandbox: None,
    })
    .await;
    assert!(
        res.timed_out && !res.cancelled && !res.exited && res.err.is_none(),
        "deadline: {res:?}"
    );
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
        res.cancelled && !res.timed_out && !res.exited,
        "cancel under a long deadline: {res:?}"
    );
}

// New (DIVERGENCES D-16): a bad cwd is a spawn failure, reported as `failed to run: <io error>` with the
// Rust text (Go says `chdir …`), and it never becomes an exit code.
#[tokio::test]
async fn bash_bad_cwd_reports_failed_to_run() {
    let (_dir, _root, bash) = new_bash("sandbox: off\n");
    let (out, is_err) = call(
        &bash,
        json!({"command": "pwd", "cwd": "/nonexistent-dir-xyz-iota"}),
    )
    .await;
    assert!(is_err, "{out:?}");
    assert!(out.starts_with("failed to run: "), "{out:?}");
    assert!(!out.contains("[exit code"), "{out:?}");
}

// Go: tool/codepath_test.go:124 TestBashHeaderSummary — for bash the command IS the call: no
// `command:` label, a width budget that fits a real pipeline, the first line only, and an explicit
// cwd folded into the shell idiom for it.
#[tokio::test]
async fn test_bash_header_summary() {
    let (_dir, root, bash) = new_bash("sandbox: off\n");
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
            "cwd folds into cd",
            json!({"command": "make release", "cwd": deploy.to_string_lossy()}),
            "cd deploy && make release",
        ),
    ] {
        assert_eq!(
            bash.header_summary(&args(arg)).as_deref(),
            Some(want),
            "{name}"
        );
    }
}
