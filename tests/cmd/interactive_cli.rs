//! End-to-end tests of the interactive BRANCH (cmd/root.go:259-414; `TUI_WPS` WP51).
//!
//! Every child runs with a CLEARED environment (`HOME` and a fixed `PATH` only) and a temp working directory,
//! so no developer config file reaches the run and no test reads or mutates the test process's environment.
//! Nothing here reaches the network, and — the point of the file — nothing reaches a terminal either: the
//! child's stdout is a pipe, which is exactly the shape (`echo hi | iota`) the refusal exists for.
//!
//! The interactive-only surface — `--no-save` and `iota resume` with no id — is lifted for a run without
//! `-m` (it means what Go means by it) and the branch at root.go:259 ends at the terminal check; a headless
//! `-m` run keeps every headless rejection.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::common::temp_project;
use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

/// cmd/root.go:400 — what the interactive branch says when stdout is not a terminal.
const NOT_A_TERMINAL: &str =
    "interactive mode requires a terminal; use -m/--message for piped input";

/// A project the child runs in: a temp cwd plus a temp `HOME`, neither holding a config file.
fn project() -> (TempDir, std::path::PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    (dir, home)
}

/// `iota …` with a cleared environment and a PIPED stdin+stdout — the `echo hi | iota` shape.
fn piped(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("iota").expect("the iota binary is built by `cargo test`");
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/bin:/usr/bin")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Spawns `cmd`, writes `hi\n` on its stdin (the `echo hi |` half) and collects the run.
fn run(mut cmd: Command) -> Output {
    let mut child = cmd.spawn().expect("spawn iota");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"hi\n")
        .ok();
    child.wait_with_output().expect("run iota")
}

/// The config a run needs now that `-k` is gone: one endpoint, one model, `agents.default`.
fn write_config(cwd: &Path) {
    std::fs::write(
        cwd.join(".iota.yaml"),
        "providers:\n  p: {type: openai, key: sk-test}\nmodels:\n  m: p:gpt-4o\nagents:\n  default: {models: [m]}\n",
    )
    .expect("write config");
}

/// stderr as UTF-8.
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Exit 1 and exactly `Error: {message}` on stderr (DIVERGENCES I-04: no usage block).
fn assert_error(o: &Output, message: &str) {
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(o));
    assert_eq!(err(o), format!("Error: {message}\n"));
}

/// L4 scenario 9, run in-process instead of under tmux: `echo hi | iota openai -k …` refuses byte-exact and
/// exits 1 (cmd/root.go:397-401).
#[test]
fn test_interactive_requires_a_terminal() {
    let (dir, home) = project();
    write_config(dir.path());
    let o = run(piped(dir.path(), &home));
    assert_error(&o, NOT_A_TERMINAL);
    assert!(o.stdout.is_empty(), "the refusal writes nothing to stdout");
}

/// The interactive LIFT (`TUI_CONTRACTS` §11): `--no-save` and a bare `iota resume` mean what Go means by
/// them, so a run without `-m` gets past `reject_unsupported` and dies at the terminal check instead of at a
/// headless rejection (D-23 / D-42 apply to `-m` runs only).
#[test]
fn test_interactive_only_flags_are_lifted_without_a_message() {
    let (dir, home) = project();
    write_config(dir.path());
    for args in [vec!["--no-save"], vec!["resume"]] {
        let mut cmd = piped(dir.path(), &home);
        cmd.args(&args);
        let o = run(cmd);
        assert_error(&o, NOT_A_TERMINAL);
    }
}

/// D-23 / D-42 stay byte-exact for a HEADLESS run: `-m` is the flag that decides the branch (root.go:259), and
/// the rejection is raised before anything else runs.
#[test]
fn test_headless_still_rejects_the_interactive_only_flags() {
    let (dir, home) = project();
    write_config(dir.path());
    for (args, message) in [
        (
            vec!["-m", "hi", "--no-save"],
            "flag --no-save is not supported in headless mode",
        ),
        (
            vec!["resume", "-m", "hi"],
            "iota resume needs a session id with -m (the picker is interactive): try `iota list sessions`",
        ),
    ] {
        let mut cmd = piped(dir.path(), &home);
        cmd.args(&args);
        let o = run(cmd);
        assert_error(&o, message);
    }
}

/// cmd/root.go:284-286 — an ephemeral start and a resumed bundle are opposite intents. Go raises this pure
/// argument error before the terminal check, so it wins over the refusal too.
#[test]
fn test_no_save_cannot_be_combined_with_resume() {
    let (dir, home) = project();
    write_config(dir.path());
    let mut cmd = piped(dir.path(), &home);
    cmd.args(["resume", "abc", "--no-save"]);
    let o = run(cmd);
    assert_error(&o, "--no-save cannot be combined with iota resume");
}

/// A listing has no terminal to require: it runs and exits 0 down a pipe, and it takes no run flags at all
/// (`--no-save` belongs to `run`, so clap refuses it here).
#[test]
fn test_list_runs_down_a_pipe() {
    let (dir, home) = project();
    let empty = dir.path().join("empty.yaml");
    std::fs::write(&empty, "providers: {}\n").expect("write config");
    let mut cmd = piped(dir.path(), &home);
    cmd.args(["list", "providers", "-c"]).arg(&empty);
    let o = run(cmd);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "No providers configured. Run `iota config init` to write a starter config.\n"
    );

    let mut cmd = piped(dir.path(), &home);
    cmd.args(["list", "--no-save"]);
    assert_eq!(
        run(cmd).status.code(),
        Some(2),
        "a run flag is not a listing flag"
    );
}

/// Every byte-pinned error Go raises BEFORE it decides headless-vs-interactive still wins over the interactive
/// branch (root.go:46-258): the branch swap must not have reordered the startup ladder.
#[test]
fn test_pre_branch_errors_still_win_over_the_interactive_branch() {
    let (dir, home) = project();
    // No agent at all (root.go:53-60).
    let o = run(piped(dir.path(), &home));
    assert_error(
        &o,
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry — `iota config init` writes a starter config",
    );
    // An agent whose endpoint has no key (root.go:88-92) — still before the branch.
    std::fs::write(
        dir.path().join(".iota.yaml"),
        "providers:\n  p: {type: openai}\nagents:\n  default: {models: [\"p:gpt-4o\"]}\n",
    )
    .expect("write config");
    let o = run(piped(dir.path(), &home));
    assert_error(
        &o,
        "API key is required: set OPENAI_API_KEY or providers.p.key in your config",
    );
    // An unknown `--mcp` value (POLICY F-02), raised during MCP config assembly.
    write_config(dir.path());
    let mut cmd = piped(dir.path(), &home);
    cmd.args(["--mcp", ""]);
    let o = run(cmd);
    assert_error(&o, "--mcp: empty server specification");
}

/// The ONE `/debug` request log of a run (WP66; cmd/root.go:125-131,410).
///
/// `cmd::run` builds one `RequestLog` beside the run's one `reqwest::Client`, wraps both in an
/// `HttpTransport` and clones that into the conversation provider and the async title instance — so
/// both record into the same ring. The MCP manager is handed the BARE client instead: Go never
/// records MCP traffic, and `From<reqwest::Client>` is the conversion that drops the recorder.
mod shared_request_log {
    use std::sync::Arc;

    use iota::llm::reqlog::RequestLog;
    use iota::provider::model::Message;
    use iota::provider::{HttpTransport, Provider, ProviderKind, ProviderParams, new_provider};
    use tokio_util::sync::CancellationToken;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// One chat-completions reply for every POST.
    async fn mock_chat(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// Sends one turn through `p`, so the seam records an attempt.
    async fn turn(p: &dyn Provider, text: &str) {
        p.chat(&CancellationToken::new(), &[Message::user(text)])
            .await
            .expect("chat");
    }

    #[tokio::test]
    async fn the_provider_and_the_title_pass_share_one_log() {
        let server = MockServer::start().await;
        mock_chat(&server).await;

        // root.go:125-131 — one client, one log, one transport.
        let reqlog = Arc::new(RequestLog::new());
        reqlog.set_verbose(true);
        let transport = HttpTransport {
            client: reqwest::Client::new(),
            recorder: Some(Arc::clone(&reqlog)),
        };
        let uri = server.uri();
        let params = || ProviderParams {
            api_key: "k",
            base_url: &uri,
            model: "gpt-4o",
            temperature: None,
        };

        // The conversation provider (root.go:132) and the title instance (interactive.rs:545-553)
        // are two SEPARATE provider objects over the same transport clone.
        let main = new_provider(ProviderKind::OpenAi, params(), Some(transport.clone()))
            .expect("main provider");
        let title = new_provider(ProviderKind::OpenAi, params(), Some(transport.clone()))
            .expect("title provider");

        turn(main.as_ref(), "parent turn").await;
        turn(title.as_ref(), "title turn").await;

        // Both landed in the SAME ring, newest first.
        let summaries: Vec<String> = reqlog.entries().iter().map(|e| e.summary.clone()).collect();
        assert_eq!(
            summaries,
            vec!["title turn".to_owned(), "parent turn".to_owned()],
            "the provider and the title pass record into one log"
        );

        // …and the transport clones carry the very same `Arc`, not equal copies of it.
        let a = transport.clone().recorder.expect("recorder");
        let b = transport.recorder.expect("recorder");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&a, &reqlog), "and it is the run's own log");
    }

    /// The MCP manager keeps the bare `reqwest::Client` (cmd/mod.rs:86-88): the `From` conversion
    /// every non-recording call site uses carries no recorder, so MCP traffic can never reach the
    /// ring — parity with Go by construction, not by filtering.
    #[test]
    fn the_bare_client_conversion_carries_no_recorder() {
        let t = HttpTransport::from(reqwest::Client::new());
        assert!(t.recorder.is_none());
        assert!(HttpTransport::default().recorder.is_none());
    }
}
