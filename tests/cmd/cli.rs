//! End-to-end tests of the `iota` binary (cmd/root.go's order, main.go's exit codes).
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

// ---------------------------------------------------------------- resolution errors (no network)

/// POLICY F-03: `-m ""` is an error, not the TUI.
#[test]
fn cli_message_empty() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["openai", "-k", "sk-x", "-M", "gpt-test", "-m", ""]);
    assert_error(&cmd.output().expect("run"), "--message must not be empty");
}

/// root.go:53-55.
#[test]
fn cli_provider_required() {
    let (dir, home) = project();
    let o = iota(dir.path(), &home).output().expect("run");
    assert_error(
        &o,
        "provider argument is required (e.g. openai, anthropic, gemini), or use -l to list available providers",
    );
}

/// A DECLARED `agents.default` is what a bare `iota` runs: the invocation gets all the way to Go's
/// interactive branch instead of the "provider argument is required" refusal.
#[test]
fn cli_bare_invocation_uses_the_default_agent() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  openai: {key: k}\nagents:\n  default:\n    models: [\"openai:gpt-4o\"]\n",
    );
    assert_error(
        &iota(dir.path(), &home).output().expect("run"),
        branch_error(),
    );

    // A one-layer `providers.default` block is the OLD shape of a provider entry, not a declared default:
    // the migration synthesises `agents.default` from it, and a bare `iota` still refuses.
    let (dir, home) = project();
    write_config(
        dir.path(),
        "providers:\n  default:\n    type: openai\n    key: k\n    model: gpt-4o\n    tools: {code: {}}\n",
    );
    let o = iota(dir.path(), &home).output().expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(&o));
    assert_eq!(
        err(&o),
        "Warning: config providers.default: model, tools now belong under `models:` / `agents:` (still accepted; see README)\n\
         Error: provider argument is required (e.g. openai, anthropic, gemini), or use -l to list available providers\n"
    );
    // …while naming it reaches the interactive branch, so nothing about the entry itself changed.
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("default");
    let o = cmd.output().expect("run");
    assert!(
        err(&o).ends_with(&format!("Error: {}\n", branch_error())),
        "{}",
        err(&o)
    );
}

/// root.go:538-546 — and DIVERGENCES D-22: the name check runs BEFORE the headless/interactive branch, so an
/// invocation without `-m` still gets this error rather than `interactive mode is not available…`.
#[test]
fn cli_unknown_provider_text() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("opnai");
    assert_error(
        &cmd.output().expect("run"),
        "unknown provider \"opnai\": not a configured alias or a built-in type\n  built-in types: openai, anthropic, gemini, vertexai, openresponses, imagen, images",
    );

    // With aliases configured the hint lists them, sorted.
    write_config(
        dir.path(),
        "providers:\n  zeta: {type: openai, key: k}\n  alpha: {type: openai, key: k}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("opnai");
    assert_error(
        &cmd.output().expect("run"),
        "unknown provider \"opnai\": not a configured alias or a built-in type\n  configured aliases: alpha, zeta\n  built-in types: openai, anthropic, gemini, vertexai, openresponses, imagen, images",
    );
}

/// POLICY F-02 (Go panicked). `assemble::build_mcp_configs` names no `iota_mcp` type, so this text is produced in
/// EVERY feature set — the unit test `assemble::tests::mcp_flag_empty_is_feature_independent` pins the half a
/// build without the `mcp` feature runs, and `ci.sh` compiles that build.
#[test]
fn cli_mcp_flag_empty() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai", "-k", "sk-x", "-M", "gpt-test", "-m", "hi", "--mcp", "",
    ]);
    assert_error(
        &cmd.output().expect("run"),
        "--mcp: empty server specification",
    );
}

/// DIVERGENCES D-23 (`--resume` left this set in phase 2 slice 1 — see `cli_blank_resume_rejected`).
#[test]
fn cli_unsupported_flag_s() {
    let (dir, home) = project();
    for (flag, name) in [("-S", "-S/--system-input"), ("--no-save", "--no-save")] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(["openai", "-k", "sk-x", "-M", "gpt-test", "-m", "hi", flag]);
        assert_error(
            &cmd.output().expect("run"),
            &format!("flag {name} is not supported in headless mode"),
        );
    }
}

/// DIVERGENCES D-42: the blank `--resume` forms report the missing PICKER, not a missing flag — and they lose
/// to `-S` and `--no-save`, which keep Go's precedence (root.go:284-286).
#[test]
fn cli_blank_resume_rejected() {
    let (dir, home) = project();
    for flag in ["--resume", "--resume="] {
        let mut cmd = iota(dir.path(), &home);
        cmd.args(["openai", "-k", "sk-x", "-M", "gpt-test", "-m", "hi", flag]);
        assert_error(
            &cmd.output().expect("run"),
            "--resume requires a session id in headless mode (--resume=<id>)",
        );
    }
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-m",
        "hi",
        "--no-save",
        "--resume=abc",
    ]);
    assert_error(
        &cmd.output().expect("run"),
        "flag --no-save is not supported in headless mode",
    );
}

/// DIVERGENCES D-22 still wins for a valued `--resume` without `-m`: the resume stage sits AFTER Go's
/// headless-vs-interactive branch (root.go:259), so no session is ever touched on that path.
#[test]
fn cli_resume_without_message_is_interactive_unavailable() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["openai", "-k", "sk-x", "-M", "gpt-test", "--resume=abc"]);
    assert_error(&cmd.output().expect("run"), branch_error());
}

/// A fully valid invocation with neither `-m` nor `-l` reaches Go's root.go:259 branch — the interactive run,
/// which refuses a piped stdout with Go's text (cmd/root.go:400).
#[test]
fn cli_interactive_refuses_a_piped_stdout() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["openai", "-k", "sk-x", "-M", "gpt-test"]);
    assert_error(&cmd.output().expect("run"), branch_error());
}

/// root.go:253: the flag describes a single `-m` run, so a misplaced one is an error — and it is raised BEFORE
/// the interactive branch's terminal check.
#[test]
fn cli_output_format_without_message() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "--output-format",
        "json",
    ]);
    assert_error(
        &cmd.output().expect("run"),
        "--output-format applies to -m runs only",
    );

    // An unknown value is rejected earlier still (chat/output.go:44-53).
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-m",
        "hi",
        "--output-format",
        "yaml",
    ]);
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
        "models:\n  openai: {provider: openai, id: gpt-test, effort: turbo}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-m",
        "hi",
        "--output-format",
        "yaml",
    ]);
    assert_error(
        &cmd.output().expect("run"),
        "config effort \"turbo\": want low|medium|high|xhigh|max",
    );
}

/// DIVERGENCES D-24: argument errors belong to clap — usage block, exit 2.
#[test]
fn cli_unknown_flag_exits_2() {
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("--nope");
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("iota [openai|anthropic|gemini|vertexai|openresponses|imagen|images]"),
        "clap prints the usage line: {}",
        err(&o)
    );
}

// ---------------------------------------------------------------- `-l` (root.go:439-529)

/// root.go:448-482: only configured providers WITH a key are listed, one sorted line each.
#[test]
fn cli_list_with_config_file() {
    let (dir, home) = project();
    let path = dir.path().join("listing.yaml");
    fs::write(
        &path,
        "
providers:
  zeta:   {type: openai, key: k, model: m}
  alpha:  {type: anthropic, key: k}
  nokey:  {type: openai}
  openai: {key: k, model: gpt-4o}
  withurl: {type: openai, key: k, url: https://x/v1, model: m2}
",
    )
    .expect("write config");

    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-l", "-c"]).arg(&path);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        out(&o),
        "Available providers:\n  alpha (type: anthropic)\n  openai (default model: gpt-4o)\n  withurl (type: openai, url: https://x/v1, model: m2)\n  zeta (type: openai, model: m)\n"
    );

    // Nothing configured (an empty explicit file): the suggestion, not an empty list.
    let empty = dir.path().join("empty.yaml");
    fs::write(&empty, "providers: {}\n").expect("write config");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-l", "-c"]).arg(&empty);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        out(&o),
        "No providers configured. Set API keys via environment variables or ~/.iota.yaml\n"
    );
}

/// root.go:520-523 wraps the provider's own `failed to list models: %w` (openai.go:64) in a second one — the
/// double prefix is Go's, and it is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_list_models_double_prefix_on_error() {
    let server = MockServer::start().await;
    transcript::openai_models_fail(&server).await;
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-l", "openai", "-k", "sk-x", "-u", &server.uri()]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(1));
    let stderr = err(&o);
    assert!(
        stderr.starts_with("Fetching available models...\n"),
        "the notice precedes the request: {stderr}"
    );
    assert!(
        stderr.contains("Error: failed to list models: failed to list models: "),
        "{stderr}"
    );
    assert!(stderr.contains("500 Internal Server Error"), "{stderr}");
}

// ---------------------------------------------------------------- `-m` runs against wiremock

/// The JSON report is the whole stdout of a `--output-format json` run (chat/output.go).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_openai_json_report_against_wiremock() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-u",
        &server.uri(),
        "-m",
        "hi",
        "--output-format",
        "json",
    ]);
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
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-u",
        &server.uri(),
        "-m",
        "hi",
    ]);
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
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-u",
        &server.uri(),
        "-m",
        "hi",
    ]);
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
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-u",
        &server.uri(),
        "-m",
        "hi",
        "--output-format",
        "json",
    ])
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
",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("pic");
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

/// The soft-migration layer, end to end: a one-layer block behaves exactly as the three-layer file above —
/// the same warnings, in the same order, with the same exit — preceded by ONE deprecation line naming what
/// moved (`docs/MIGRATION-ROADMAP.md` Phase 1b, step 8).
#[test]
fn cli_one_layer_config_still_runs_and_says_so() {
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
    effort: high
    top_p: 0.5
    temperature: 0.5
",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("pic");
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        format!(
            "Warning: config providers.pic: model, effort, temperature, top_p, image now belong under `models:` / `agents:` (still accepted; see README)\n\
             Warning: `image: true` is redundant for provider type imagen (it always generates images)\n\
             Warning: `effort` does not apply to provider type imagen (ignored)\n\
             Warning: `top_p` does not apply to provider type imagen (ignored)\n\
             Warning: temperature does not apply to provider type imagen (ignored)\n\
             Error: {}\n",
            branch_error()
        )
    );
}

/// A `defer_mode:` the provider's dialect cannot speak now stops the run where it is written, before any
/// provider is built (it used to warn at dispatcher-assembly time and quietly use `normal`).
#[test]
fn cli_defer_mode_mismatch_fails_at_config_load() {
    let (dir, home) = project();
    write_config(
        dir.path(),
        "models:\n  m: {provider: openai, id: gpt-test, defer_mode: reference}\n",
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["m", "-k", "sk-x", "-m", "hi"]);
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
    let mut cmd = iota(dir.path(), &home);
    cmd.args([
        "openai",
        "-k",
        "sk-x",
        "-M",
        "gpt-test",
        "-u",
        &server.uri(),
        "-m",
        "hi",
        "--mcp",
        "sh -c 'exit 1'",
    ]);
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
