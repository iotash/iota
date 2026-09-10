//! End-to-end tests of the `iota` binary: the verb set, the run order (cmd/root.go's) and main.go's exit
//! codes.
//!
//! Every child process is launched with a CLEARED environment (`HOME` and a fixed `PATH` are the only variables
//! it gets) and a temp working directory, so no test reads or mutates the test process's own environment and no
//! developer config file can reach the run. HTTP goes to a `wiremock` server; nothing touches the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::common::{temp_project, transcript};
use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;
use wiremock::MockServer;

/// A project the child runs in: a temp cwd plus a temp `HOME`, neither holding a config file.
fn project() -> (TempDir, std::path::PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    (dir, home)
}

/// `iota …` with a cleared environment. `PATH` is a literal (never read from this process) so a `--mcp` stdio
/// server can still find `/bin/sh`; `HOME` and the working directory are the fixture's.
fn iota(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("iota").expect("the iota binary is built by `cargo test`");
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/bin:/usr/bin")
        .current_dir(cwd)
        .stdin(Stdio::null());
    cmd
}

/// Runs `cmd` off the async runtime (wiremock needs its worker threads to keep serving).
async fn output(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.output().expect("run iota"))
        .await
        .expect("child join")
}

/// stdout as UTF-8.
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// stderr as UTF-8.
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// How the branch at root.go:259 ends here (`TUI_CONTRACTS` §11): the None arm IS the interactive run, which
/// refuses a non-terminal stdout byte-exact (cmd/root.go:400) — and every child here has a piped stdout.
fn branch_error() -> &'static str {
    "interactive mode requires a terminal; use -m/--message for piped input"
}

/// Asserts the run failed with exit 1 and printed exactly `Error: {message}` (DIVERGENCES I-04: no usage block).
fn assert_error(o: &Output, message: &str) {
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(o));
    assert_eq!(err(o), format!("Error: {message}\n"));
    assert!(out(o).is_empty(), "a failed run printed to stdout");
}

/// Writes `<cwd>/.iota.yaml`.
fn write_config(cwd: &Path, body: &str) {
    fs::write(cwd.join(".iota.yaml"), body).expect("write config");
}

/// The config almost every run here needs: one endpoint (with its key, and its URL when a mock server is
/// serving), one model, and `agents.default` — the entry a bare `iota` runs.
///
/// Since `-k` and `-u` were retired, a test says what it is talking to the same way a user does: in the three
/// layers (brain page `cli-surface-agent-first`).
fn write_agent_config(cwd: &Path, kind: &str, url: &str, model: &str) {
    let url = if url.is_empty() {
        String::new()
    } else {
        format!(", url: {url}")
    };
    write_config(
        cwd,
        &format!(
            "providers:\n  p: {{type: {kind}, key: sk-x{url}}}\nmodels:\n  m: p:{model}\nagents:\n  default: {{models: [m]}}\n"
        ),
    );
}

// ---------------------------------------------------------------- resolution errors (no network)

/// POLICY F-03: `-m ""` is an error, not the TUI.
#[test]
fn cli_message_empty() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", ""]);
    assert_error(&cmd.output().expect("run"), "--message must not be empty");
}

/// root.go:53-55, as the agent-first surface says it: with nothing configured there is no `agents.default`,
/// and the refusal names both ways forward.
#[test]
fn cli_no_agent_to_run() {
    let (dir, home) = project();
    let o = iota(dir.path(), &home).output().expect("run");
    assert_error(
        &o,
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry — `iota config init` writes a starter config",
    );
}

/// A DECLARED `agents.default` is what a bare `iota` runs — and so is a bare `iota run`: the invocation gets
/// all the way to Go's interactive branch instead of the refusal.
#[test]
fn cli_bare_invocation_uses_the_default_agent() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  openai: {key: k}\nagents:\n  default:\n    models: [\"openai:gpt-4o\"]\n",
    );
    for args in [&[][..], &["run"][..], &["run", "default"][..]] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(args);
        assert_error(&cmd.output().expect("run"), branch_error());
    }

    // A `providers.default` or a `models.default` is not an entry point: only an agent says how to drive a
    // model, so only an agent can be run.
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  default: {type: openai, key: k}\nmodels:\n  default: default:gpt-4o\n",
    );
    let o = iota(dir.path(), &home).output().expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(&o));
    assert!(
        err(&o).starts_with("Error: no agent to run:"),
        "{}",
        err(&o)
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "default"]);
    assert_error(
        &cmd.output().expect("run"),
        "unknown agent \"default\"\n  no agents are configured — run `iota config init` to write a starter config",
    );
}

/// DIVERGENCES D-22: the agent lookup runs BEFORE the headless/interactive branch, so an invocation without
/// `-m` still gets this error rather than `interactive mode is not available…`.
#[test]
fn cli_unknown_agent_text() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "codr"]);
    assert_error(
        &cmd.output().expect("run"),
        "unknown agent \"codr\"\n  no agents are configured — run `iota config init` to write a starter config",
    );

    // With agents configured the hint lists them, sorted.
    write_config(
        dir.path(),
        "providers:\n  openai: {key: k}\nagents:\n  zeta: {models: [\"openai:x\"]}\n  alpha: {models: [\"openai:x\"]}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "codr"]);
    assert_error(
        &cmd.output().expect("run"),
        "unknown agent \"codr\"\n  configured agents: alpha, zeta",
    );
}

/// POLICY F-02 (Go panicked). `assemble::build_mcp_configs` names no `iota_mcp` type, so this text is produced in
/// EVERY feature set — the unit test `assemble::tests::mcp_flag_empty_is_feature_independent` pins the half a
/// build without the `mcp` feature runs, and `ci.sh` compiles that build.
#[test]
fn cli_mcp_flag_empty() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--mcp", ""]);
    assert_error(
        &cmd.output().expect("run"),
        "--mcp: empty server specification",
    );
}

/// DIVERGENCES D-23: `--no-save` is the one interactive-only flag left, and a headless run still refuses it.
#[test]
fn cli_unsupported_flag_no_save() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--no-save"]);
    assert_error(
        &cmd.output().expect("run"),
        "flag --no-save is not supported in headless mode",
    );
}

/// A headless `iota resume` with no id reports the missing PICKER, not a missing command — and it loses to
/// `--no-save`, which keeps Go's precedence (root.go:284-286).
#[test]
fn cli_headless_resume_needs_an_id() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["resume", "-m", "hi"]);
    assert_error(
        &cmd.output().expect("run"),
        "iota resume needs a session id with -m (the picker is interactive): try `iota list sessions`",
    );

    let mut cmd = iota(dir.path(), &home);
    cmd.args(["resume", "-m", "hi", "--no-save"]);
    assert_error(
        &cmd.output().expect("run"),
        "flag --no-save is not supported in headless mode",
    );
}

/// DIVERGENCES D-22 still wins for `iota resume <id>` without `-m`: the resume stage sits AFTER Go's
/// headless-vs-interactive branch (root.go:259), so no session is ever touched on that path.
#[test]
fn cli_resume_without_message_is_interactive_unavailable() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["resume", "abc"]);
    assert_error(&cmd.output().expect("run"), branch_error());
}

/// A fully valid invocation without `-m` reaches Go's root.go:259 branch — the interactive run, which refuses
/// a piped stdout with Go's text (cmd/root.go:400).
#[test]
fn cli_interactive_refuses_a_piped_stdout() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    assert_error(
        &iota(dir.path(), &home).output().expect("run"),
        branch_error(),
    );
}

/// root.go:253: the flag describes a single `-m` run, so a misplaced one is an error — and it is raised BEFORE
/// the interactive branch's terminal check.
#[test]
fn cli_output_format_without_message() {
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", "", "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["--output-format", "json"]);
    assert_error(
        &cmd.output().expect("run"),
        "--output-format applies to -m runs only",
    );

    // An unknown value is rejected earlier still (chat/output.go:44-53).
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--output-format", "yaml"]);
    assert_error(
        &cmd.output().expect("run"),
        "unknown output format \"yaml\" (want text or json)",
    );
}

/// root.go:249: `chat.ParseOutputFormat` runs AFTER provider construction and the config tuning checks, so when
/// a bad `effort:` and a bad `--output-format` coexist the effort error wins — the format error is the loser in
/// Go's order.
#[test]
fn cli_output_format_parse_runs_after_tuning() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  p: {type: openai, key: sk-x}\nmodels:\n  m: {provider: p, id: gpt-test, effort: turbo}\nagents:\n  default: {models: [m]}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--output-format", "yaml"]);
    assert_error(
        &cmd.output().expect("run"),
        "config effort \"turbo\": want low|medium|high|xhigh|max",
    );
}

/// DIVERGENCES D-24: argument errors belong to clap — usage block, exit 2. That now covers the six retired
/// flags: nothing silently ignores them.
#[test]
fn cli_unknown_flag_exits_2() {
    let (dir, home) = project();
    for args in [
        &["--nope"][..],
        &["-k", "sk-x"][..],
        &["-u", "https://x"][..],
        &["-t", "0.5"][..],
        &["-S"][..],
        &["--context-window", "200k"][..],
        &["--agent"][..],
        &["-l"][..],
        &["--resume=abc"][..],
    ] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(args);
        let o = cmd.output().expect("run");
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
        assert!(
            err(&o).contains("Usage: iota"),
            "clap prints the usage line for {args:?}: {}",
            err(&o)
        );
    }
}

/// `-c/--config` is global end to end: before the verb or after it, every command reads the same file — and
/// a `run` flag in the pre-verb position is refused with the exit-2 argument error instead.
#[test]
fn cli_config_flag_is_global() {
    let (dir, home) = project();
    let path = dir.path().join("global.yaml");
    fs::write(
        &path,
        "providers:\n  p: {type: openai, key: k}\nagents:\n  solo: {models: [\"p:gpt-4o\"]}\n",
    )
    .expect("write config");

    // `run` reads it from either side (both reach the same refusal: `solo` is not `default`).
    for args in [
        vec![
            "-c".to_owned(),
            path.display().to_string(),
            "run".to_owned(),
        ],
        vec![
            "run".to_owned(),
            "-c".to_owned(),
            path.display().to_string(),
        ],
    ] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(&args);
        assert_error(
            &cmd.output().expect("run"),
            "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry — `iota config init` writes a starter config",
        );
    }

    // `resume` too — the id and the flag never compete.
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("-c").arg(&path).args(["resume", "abc", "-m", "hi"]);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));

    // A run flag before another verb is the one thing the pre-verb position refuses, and it says where the
    // flag belongs (clap's own error: exit 2, usage block).
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "list"]);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).starts_with(
            "error: '-m/--message' is a flag of `iota run`; put it after the 'list' command"
        ),
        "{}",
        err(&o)
    );
}

/// `iota version` and `--version` say the same thing.
#[test]
fn cli_version() {
    let (dir, home) = project();
    let want = format!("iota {}\n", env!("CARGO_PKG_VERSION"));
    for args in [&["version"][..], &["--version"][..]] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(args);
        let o = cmd.output().expect("run");
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", err(&o));
        assert_eq!(out(&o), want, "{args:?}");
    }
}

// ---------------------------------------------------------------- `iota list` / `iota config`

/// The listings read the config and stop — no key, no network, exit 0.
#[test]
fn cli_list_reads_the_config() {
    let (dir, home) = project();
    let path = dir.path().join("listing.yaml");
    fs::write(
        &path,
        "
providers:
  zeta:   {type: openai, key: k}
  openai: {key: k}
models:
  gpt: openai:gpt-4o
agents:
  coder:
    models: [gpt, \"zeta:*\"]
    description: Writes code
",
    )
    .expect("write config");

    // `-c` is global, so every listing here is run BOTH ways round the verb and the two must agree.
    let run = |args: &[&str]| {
        let after = {
            let mut cmd = iota(dir.path(), &home);
            cmd.args(args).arg("-c").arg(&path);
            let o = cmd.output().expect("run");
            assert_eq!(o.status.code(), Some(0), "{args:?}: {}", err(&o));
            out(&o)
        };
        let before = {
            let mut cmd = iota(dir.path(), &home);
            cmd.arg("-c").arg(&path).args(args);
            let o = cmd.output().expect("run");
            assert_eq!(o.status.code(), Some(0), "-c before {args:?}: {}", err(&o));
            out(&o)
        };
        assert_eq!(
            before, after,
            "-c must mean the same on either side of {args:?}"
        );
        after
    };
    assert_eq!(run(&["list"]), "Agents:\n  coder  2 models  Writes code\n");
    assert_eq!(run(&["list", "agents"]), run(&["list"]));
    assert_eq!(run(&["list", "models"]), "Models:\n  gpt  openai:gpt-4o\n");
    assert_eq!(
        run(&["list", "models", "coder"]),
        "Models for agent coder:\n  gpt (openai:gpt-4o)\n  zeta:* (every model zeta lists)\n"
    );
    assert_eq!(
        run(&["list", "providers"]),
        "Providers:\n  openai  [key: config]\n  zeta (type: openai)  [key: config]\n"
    );
    assert_eq!(run(&["list", "sessions"]), "No saved sessions.\n");

    // Nothing configured: the listing says what to do about it.
    let empty = dir.path().join("empty.yaml");
    fs::write(&empty, "providers: {}\n").expect("write config");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["list", "-c"]).arg(&empty);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        out(&o),
        "No agents configured. Run `iota config init` to write a starter config.\n"
    );
}

/// `iota config init` writes a config the binary itself accepts, refuses to overwrite one, and `check`/`path`
/// report what a run would load.
#[test]
fn cli_config_init_check_path() {
    let (dir, home) = project();
    let cfg_path = home.join(".iota.yaml");

    let mut cmd = iota(dir.path(), &home);
    cmd.arg("config").arg("init");
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        out(&o).starts_with(&format!("Wrote {}\n", cfg_path.display())),
        "{}",
        out(&o)
    );
    assert!(cfg_path.is_file(), "the file is there");

    // A second init never touches it.
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("config").arg("init");
    assert_error(
        &cmd.output().expect("run"),
        &format!(
            "{} already exists (use -c <path> to write somewhere else)",
            cfg_path.display()
        ),
    );

    // …and `-c` writes somewhere else, from either side of the verb.
    let alt = dir.path().join("alt.yaml");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["config", "init", "-c"]).arg(&alt);
    assert_eq!(cmd.output().expect("run").status.code(), Some(0));
    assert!(alt.is_file());
    let alt2 = dir.path().join("alt2.yaml");
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("-c").arg(&alt2).args(["config", "init"]);
    assert_eq!(cmd.output().expect("run").status.code(), Some(0));
    assert!(alt2.is_file(), "-c before the verb picks the same file");

    // What init wrote loads: `check` reports the file and the three layers, and warns about nothing.
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("config").arg("check");
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        out(&o),
        format!(
            "{}\nOK: 1 provider(s), 1 model(s), 1 agent(s), 0 mcp server(s)\n",
            cfg_path.display()
        )
    );
    assert_eq!(err(&o), "", "a clean config warns about nothing");

    // `path` names the same file, in merge order.
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("config").arg("path");
    let o = cmd.output().expect("run");
    assert_eq!(out(&o), format!("{}\n", cfg_path.display()));

    // `check` on a broken config fails with the coordinate and the file.
    let broken = dir.path().join("broken.yaml");
    fs::write(&broken, "providers:\n  p: {type: openai, model: gpt-4o}\n").expect("write");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["config", "check", "-c"]).arg(&broken);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with(&format!(
            "Error: config {}: providers.p.model: `model` is now a `models:` entry",
            broken.display()
        )),
        "{}",
        err(&o)
    );

    // A config with no `agents.default` still checks out, with one line saying a bare `iota` has nothing.
    let no_default = dir.path().join("nodefault.yaml");
    fs::write(
        &no_default,
        "providers:\n  p: {type: openai, key: k}\nagents:\n  coder: {models: [\"p:x\"]}\n",
    )
    .expect("write");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["config", "check", "-c"]).arg(&no_default);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        err(&o),
        "Warning: no `agents.default` entry: a bare `iota` has nothing to run\n"
    );
}

// ---------------------------------------------------------------- `-m` runs against wiremock

/// The JSON report is the whole stdout of a `--output-format json` run (chat/output.go).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_openai_json_report_against_wiremock() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--output-format", "json"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));

    let report: serde_json::Value = serde_json::from_str(&out(&o)).expect("stdout is one JSON doc");
    assert_eq!(report["type"], "result");
    assert_eq!(report["provider"], "openai");
    assert_eq!(report["model"], "gpt-test");
    assert_eq!(report["reply"], transcript::REPLY);
    assert_eq!(report["rounds"], 1);
    assert_eq!(report["usage"]["input_tokens"], transcript::INPUT_TOKENS);
    assert_eq!(report["usage"]["output_tokens"], transcript::OUTPUT_TOKENS);
    assert_eq!(report["usage"]["total_tokens"], transcript::TOTAL_TOKENS);
    assert_eq!(report["round_usage"][0]["round"], 1);
    assert_eq!(
        report["round_usage"][0]["usage"]["total_tokens"],
        transcript::TOTAL_TOKENS
    );
    // A successful run reports no error.
    assert!(report.get("error").is_none());
}

/// Text mode prints the reply and nothing else (chat.go:56-66).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_openai_text_mode_bare() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("{}\n", transcript::REPLY));
    assert!(err(&o).is_empty(), "text mode is quiet: {}", err(&o));
}

/// A terminal 4xx is not retried and reaches the user as `Error: chat error: …` with exit 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_400_prints_error_and_exits_1() {
    let server = MockServer::start().await;
    transcript::openai_bad_request(&server).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(1));
    let stderr = err(&o);
    assert!(stderr.starts_with("Error: chat error: POST \""), "{stderr}");
    assert!(stderr.contains("400 Bad Request"), "{stderr}");
    assert!(stderr.contains("bad model"), "{stderr}");
    assert!(out(&o).is_empty());
    // Exactly one attempt: a 4xx other than 408/409/429 is terminal (client.go retry policy).
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
}

/// DIVERGENCES I-03: SIGINT cancels the run, JSON mode still prints its report — with `"error": "interrupted"` —
/// and the process exits 130.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_sigint_exits_130_with_interrupted_json() {
    use std::time::Duration;

    let server = MockServer::start().await;
    transcript::openai_transcript_delayed(&server, Duration::from_secs(30)).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--output-format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn iota");
    let pid = child.id();

    // Wait until the request is actually in flight, so the signal cannot land before the run starts.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let seen = server
            .received_requests()
            .await
            .map_or(0, |reqs| reqs.len());
        if seen > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the run never reached the server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // `kill` is a POSIX shell builtin, so no extra dependency is needed to raise the signal.
    let signalled = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("kill -INT {pid}"))
        .status()
        .expect("send SIGINT");
    assert!(signalled.success(), "kill -INT failed");

    let o = tokio::task::spawn_blocking(move || child.wait_with_output().expect("wait"))
        .await
        .expect("child join");
    assert_eq!(o.status.code(), Some(130), "stderr: {}", err(&o));
    let report: serde_json::Value = serde_json::from_str(&out(&o)).expect("stdout is one JSON doc");
    assert_eq!(report["type"], "result");
    assert_eq!(report["error"], "interrupted");
    assert_eq!(report["reply"], "");
}

// ---------------------------------------------------------------- warnings

/// root.go:133-181, in order: an `imagen` entry that sets `image`, `effort`, `top_p` and `temperature` gets one
/// warning per knob the provider cannot use, before the run stops at Go's interactive branch.
#[test]
fn cli_tuning_warnings_order() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "
providers:
  pic:
    type: imagen
    key: k
models:
  pic:
    id: imagen-4
    image: true
    effort: high
    top_p: 0.5
    temperature: 0.5
agents:
  pic:
    models: [pic]
",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "pic"]);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        format!(
            "Warning: `image: true` is redundant for provider type imagen (it always generates images)\n\
             Warning: `effort` does not apply to provider type imagen (ignored)\n\
             Warning: `top_p` does not apply to provider type imagen (ignored)\n\
             Warning: temperature does not apply to provider type imagen (ignored)\n\
             Error: {}\n",
            branch_error()
        )
    );
}

/// The key audit, end to end: a config written the way iota's FIRST shape wanted it fails at load with the
/// coordinate, the file and the layer that owns the key. Nothing is migrated any more.
#[test]
fn cli_one_layer_config_is_refused_with_its_coordinate() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "
providers:
  pic:
    type: imagen
    key: k
    model: imagen-4
    image: true
",
    );
    let o = iota(dir.path(), &home).output().expect("run");
    assert_eq!(o.status.code(), Some(1));
    // The path is the one the child resolved (macOS canonicalises the temp dir), so only its tail is pinned.
    assert!(
        err(&o).starts_with("Error: config ")
            && err(&o).ends_with(
                ".iota.yaml: providers.pic.model: `model` is now a `models:` entry — write `models.<name>: <provider>:<id>` and list it in `agents.<name>.models`\n"
            ),
        "{}",
        err(&o)
    );
}

/// A `defer_mode:` the provider's dialect cannot speak now stops the run where it is written, before any
/// provider is built (it used to warn at dispatcher-assembly time and quietly use `normal`).
#[test]
fn cli_defer_mode_mismatch_fails_at_config_load() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  p: {type: openai, key: sk-x}\nmodels:\n  m: {provider: p, id: gpt-test, defer_mode: reference}\nagents:\n  default: {models: [m]}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi"]);
    assert_error(
        &cmd.output().expect("run"),
        "models.m: defer_mode \"reference\" does not apply to provider type openai (see docs/design/tool-defer.md)",
    );
}

/// DIVERGENCES I-05: a configured MCP server that does not connect is one `Warning:` line on stderr (Go was
/// silent on the `-m` path) and never fails the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_mcp_failed_server_warning() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--mcp", "sh -c 'exit 1'"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.starts_with("Warning: mcp server sh: connect failed: "),
        "{stderr}"
    );
    // The run still answered: a server that failed to connect degrades, it does not abort.
    assert_eq!(out(&o), format!("{}\n", transcript::REPLY));
}
