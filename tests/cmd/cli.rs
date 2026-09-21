//! End-to-end tests of the `iota` binary: the verb set, the run order (cmd/root.go's) and main.go's exit
//! codes.
//!
//! Every child process is launched with a CLEARED environment (`common::cleared_env`: the platform's home
//! variable, a fixed `PATH`, and on Windows the few variables the OS itself reads) and a temp working
//! directory, so no test reads or mutates the test process's own environment and no developer config file can
//! reach the run. HTTP goes to a `wiremock` server; nothing touches the network.

use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::common::{cleared_env, temp_project, transcript};
use tempfile::TempDir;
use wiremock::MockServer;

/// A project the child runs in: a temp cwd plus a temp `HOME`, neither holding a config file.
fn project() -> (TempDir, std::path::PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    (dir, home)
}

/// `iota …` with a cleared environment (`common::cleared_env`); the home and working directories are the
/// fixture's.
fn iota(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, home)
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

/// A first run — no `-c`, no config file at either tier — writes the starter to `~/.iota.yaml`, says so on
/// stderr, and goes on with it: with the key in the environment it reaches the interactive branch like any
/// configured run; without one the key error says what to set. The file is the one `iota config init`
/// writes, and a second run finds it and writes nothing.
#[test]
fn cli_first_run_writes_the_starter_and_goes_on() {
    let (dir, home) = project();
    let starter = home.join(".iota.yaml");
    assert!(!starter.exists());

    // No key: the run gets as far as the starter's provider and stops there.
    let o = iota(dir.path(), &home).output().expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(&o));
    let wrote = format!(
        "Wrote {} (a starter config: openai + gpt-5.2; edit it, or set OPENAI_API_KEY and go)",
        starter.display()
    );
    assert_eq!(
        err(&o),
        format!(
            "{wrote}\nError: API key is required: set OPENAI_API_KEY or providers.openai.key in your config\n"
        )
    );
    let written = std::fs::read_to_string(&starter).expect("the starter");
    assert!(written.contains("agents:\n  default:"), "{written}");

    // With the key: the same first run is a run (the interactive branch refuses the pipe, as for any agent).
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.env("OPENAI_API_KEY", "k");
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(&o));
    assert!(err(&o).starts_with("Wrote "), "{}", err(&o));
    assert!(
        err(&o).ends_with(&format!("Error: {}\n", branch_error())),
        "{}",
        err(&o)
    );
    // The second run finds the file: nothing written, nothing said about it.
    let mut cmd = iota(dir.path(), &home);
    cmd.env("OPENAI_API_KEY", "k");
    let o = cmd.output().expect("run");
    assert_eq!(err(&o), format!("Error: {}\n", branch_error()));
    assert_eq!(
        std::fs::read_to_string(home.join(".iota.yaml")).expect("still there"),
        written,
        "the starter is written once"
    );
}

/// What a first run does NOT do: write when `-c` names a file, when a project config exists without a user
/// one, or under a home that already has a config in the other extension.
#[test]
fn cli_first_run_leaves_an_existing_config_alone() {
    // `-c`: the named file is the only scope, and a missing `agents.default` there is the plain refusal.
    let (dir, home) = project();
    let path = dir.path().join("only.yaml");
    std::fs::write(&path, "providers:\n  openai: {key: k}\n").expect("write");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-c", path.to_str().expect("utf-8")]);
    assert_error(
        &cmd.output().expect("run"),
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)",
    );
    assert!(!home.join(".iota.yaml").exists(), "-c writes nothing");

    // A project config alone: found, so no starter — and its lack of an agent is the refusal.
    let (dir, home) = project();
    write_config(dir.path(), "providers:\n  openai: {key: k}\n");
    let o = iota(dir.path(), &home).output().expect("run");
    assert_error(
        &o,
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)",
    );
    assert!(
        !home.join(".iota.yaml").exists(),
        "a project config is a config"
    );

    // A user config in the `.yml` spelling: found too.
    let (dir, home) = project();
    std::fs::write(home.join(".iota.yml"), "providers:\n  openai: {key: k}\n").expect("write");
    let o = iota(dir.path(), &home).output().expect("run");
    assert_error(
        &o,
        "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)",
    );
    assert!(!home.join(".iota.yaml").exists(), ".yml counts");
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
        "unknown agent \"default\"\n  no agents are configured — add an `agents:` entry to your config (`iota config path` names the file)",
    );
}

/// DIVERGENCES D-22: the agent lookup runs BEFORE the headless/interactive branch, so an invocation without
/// `-m` still gets this error rather than `interactive mode is not available…`.
#[test]
fn cli_unknown_agent_text() {
    // With no config at all the starter is written first (its one agent is `default`), and the unknown name
    // is measured against that.
    let (dir, home) = project();
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "codr"]);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(&o));
    assert!(
        err(&o).starts_with("Wrote ")
            && err(&o).ends_with("Error: unknown agent \"codr\"\n  configured agents: default\n"),
        "{}",
        err(&o)
    );

    // A config with no `agents:` at all: the hint says where one goes.
    let (dir, home) = project();
    write_config(dir.path(), "providers:\n  openai: {key: k}\n");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["run", "codr"]);
    assert_error(
        &cmd.output().expect("run"),
        "unknown agent \"codr\"\n  no agents are configured — add an `agents:` entry to your config (`iota config path` names the file)",
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
            "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)",
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

// ---------------------------------------------------------------- `IOTA_LOG`

/// DIVERGENCES X-29: `IOTA_LOG=<path>` is the developer's tap — the file subscriber goes in before the run
/// and every line carries a timestamp, a level and a target; the run itself is unchanged (stdout is exactly
/// the version line, stderr empty). The pipe is proved by its own first event; the levels and the format are
/// the roadmap's backlog.
#[test]
fn cli_iota_log_writes_diagnostics_to_the_file() {
    let (dir, home) = project();
    let log = dir.path().join("diag").join("iota.log");
    fs::create_dir_all(log.parent().unwrap()).expect("log dir");
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("version").env("IOTA_LOG", &log);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("iota {}\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(err(&o), "", "the tap never speaks on stderr");
    let logged = fs::read_to_string(&log).expect("the log file was created");
    let first = logged.lines().next().unwrap_or_default();
    assert!(
        first.contains(" INFO iota::app::diag: iota diagnostics on version=\"")
            && first.starts_with("20")
            && first.contains('Z'),
        "the first line is the subscriber's own hello, timestamped: {first:?}"
    );
    assert!(
        !logged.contains('\x1b'),
        "a log file carries no SGR: {logged:?}"
    );

    // A second run APPENDS: the file is a log, not a snapshot.
    let mut again = iota(dir.path(), &home);
    again.arg("version").env("IOTA_LOG", &log);
    again.output().expect("run");
    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        2,
        "one hello per run"
    );
}

/// A path that cannot be opened is ONE warning on stderr and nothing else changes: the run answers, exit 0.
/// The switch is a side channel; the run never depends on it.
#[test]
fn cli_iota_log_unopenable_path_warns_and_runs() {
    let (dir, home) = project();
    let log = dir.path().join("no-such-dir").join("iota.log");
    let mut cmd = iota(dir.path(), &home);
    cmd.arg("version").env("IOTA_LOG", &log);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("iota {}\n", env!("CARGO_PKG_VERSION")));
    let stderr = err(&o);
    assert!(
        stderr.starts_with(&format!(
            "Warning: IOTA_LOG: cannot log to {}: ",
            log.display()
        )) && stderr.lines().count() == 1,
        "{stderr:?}"
    );
    assert!(!log.exists());
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

/// The built-in harness prompt (brain page `harness-prompt`), end to end: an agent with `tools:` sends the
/// binary's own paragraph — identity, `<environment>` with the facts of THIS run, and `<iota_cli>` when the
/// `shell` set is on — ahead of its `system:` inside `<instructions>`; an agent with only `code:` sends no
/// `<iota_cli>`; an agent without `tools:` sends the bare prompt it always did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_headless_run_sends_the_harness_for_an_agent_with_tools() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    // The config is the USER tier (`~/.iota.yaml`); the project tier does not exist, and the working
    // directory has no `.git` above it, so it is its own project root: the environment must say all three.
    let cwd = dir.path().join("proj");
    fs::create_dir_all(&cwd).expect("cwd");
    let user_config = home.join(".iota.yaml");
    let config = |tools: &str| {
        format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {{models: [m], system: be brief{tools}}}\n",
            server.uri()
        )
    };
    let system_message = |body: &[u8]| -> serde_json::Value {
        let body: serde_json::Value = serde_json::from_slice(body).expect("a JSON body");
        body["messages"][0].clone()
    };

    // code + shell: every block.
    fs::write(
        &user_config,
        config(", tools: {code: , shell: {sandbox: off, auto_run: true}}"),
    )
    .expect("user config");
    let mut cmd = iota(&cwd, &home);
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let requests = server.received_requests().await.expect("recorded");
    let first = system_message(&requests[0].body);
    assert_eq!(first["role"], "system");
    let content = first["content"].as_str().expect("system text");
    assert!(content.starts_with("You run inside iota"), "{content}");
    assert!(content.contains("<environment>\n"), "{content}");
    assert!(content.contains("<iota_cli>\n"), "{content}");
    assert!(
        content.ends_with("</iota_cli>\n\n<instructions>\nbe brief\n</instructions>"),
        "{content}"
    );
    // The environment says what THIS run found — not a fixture. The root is the process's own `getcwd`:
    // on macOS the canonical spelling of the temp path (`/private/var/…`), on Windows the path as given.
    let root = if cfg!(windows) {
        cwd.clone()
    } else {
        fs::canonicalize(&cwd).expect("canonical cwd")
    };
    assert!(
        content.contains(&format!("\nproject root: {}\n", root.display())),
        "{content}"
    );
    assert!(
        content.contains(&format!("\nuser config: {}\n", user_config.display())),
        "{content}"
    );
    assert!(
        content.contains("\nproject config: (absent)\n"),
        "{content}"
    );
    // The binary is canonical, minus the `\\?\` prefix Windows' `canonicalize` adds (`app::canonical`).
    let exe = fs::canonicalize(env!("CARGO_BIN_EXE_iota")).expect("canonical exe");
    let exe = exe.to_string_lossy();
    let exe = exe.trim_start_matches(r"\\?\");
    assert!(
        content.contains(&format!("\niota binary: {exe}\n")),
        "{content}"
    );
    assert!(content.contains("\nshell: "), "{content}");
    assert!(content.contains("\ndate: 20"), "{content}");
    server.reset().await;
    transcript::openai_transcript(&server).await;

    // code alone: the environment, not the CLI block.
    fs::write(&user_config, config(", tools: {code: }")).expect("user config");
    let mut cmd = iota(&cwd, &home);
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let requests = server.received_requests().await.expect("recorded");
    let content = system_message(&requests[0].body)["content"]
        .as_str()
        .expect("system text")
        .to_owned();
    assert!(content.contains("<environment>\n"), "{content}");
    assert!(!content.contains("<iota_cli>"), "{content}");
    server.reset().await;
    transcript::openai_transcript(&server).await;

    // No tools: the bare prompt, today's bytes exactly.
    fs::write(&user_config, config("")).expect("user config");
    let mut cmd = iota(&cwd, &home);
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let requests = server.received_requests().await.expect("recorded");
    let first = system_message(&requests[0].body);
    assert_eq!(first["role"], "system");
    assert_eq!(first["content"], "be brief");
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

/// A minimal MCP server in POSIX `sh` that advertises the SAME tool twice (`tests/mcp/manager.rs`'s
/// `SH_SERVER`, with a second `echo`): newline-delimited JSON-RPC over stdin/stdout, exiting on stdin EOF.
#[cfg(unix)]
const SH_SERVER_DUPLICATE_TOOL: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"first","inputSchema":{"type":"object"}},{"name":"echo","description":"second","inputSchema":{"type":"object"}}]}}' ;;
  esac
done
"#;

/// DIVERGENCES X-29: a server whose tool list collides on a wire name reaches the user as one `Warning:` line
/// on stderr — from the BINARY, whatever profile it was built with. It used to be a `tracing::warn!` that no
/// subscriber received and that the release build compiled out.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_mcp_duplicate_wire_name_warning() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    write_agent_config(dir.path(), "openai", &server.uri(), "gpt-test");
    let script = dir.path().join("dup-server.sh");
    fs::write(&script, SH_SERVER_DUPLICATE_TOOL).expect("write the server script");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi", "--mcp", &format!("sh {}", script.display())]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        err(&o),
        "Warning: mcp server sh: duplicate wire tool name mcp__sh__echo, skipping\n"
    );
    // The first registration won and the run went on to answer.
    assert_eq!(out(&o), format!("{}\n", transcript::REPLY));
}

/// A headless run inside a herdr pane (the `HERDR_*` variables on the child's cleared environment,
/// pointed at the mock socket) reports `working` on the way in, `idle` once the reply is on stdout
/// and releases the pane on exit — no session report, a stateless `-m` has none — and the
/// `<environment>` it sends names the host, the pane, the workspace and the tab, after the run's
/// own facts.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_headless_run_reports_to_herdr_and_names_the_pane() {
    let mock = crate::common::HerdrMock::start();
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home) = project();
    write_config(
        dir.path(),
        &format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {{models: [m], system: be brief, tools: {{code: }}}}\n",
            server.uri()
        ),
    );
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-m", "hi"]).envs(mock.env("w1:p2"));
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("{}\n", transcript::REPLY));

    assert_eq!(
        mock.summaries(),
        ["report_agent working", "report_agent idle", "release_agent"]
    );
    let requests = mock.requests();
    for r in &requests {
        assert_eq!(r.param("pane_id"), "w1:p2", "{r:?}");
        assert_eq!(r.param("source"), "iota", "{r:?}");
    }
    let seqs: Vec<u64> = requests.iter().map(|r| r.seq().expect("a seq")).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq is not strictly increasing: {seqs:?}"
    );

    let sent = server.received_requests().await.expect("recorded");
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).expect("a JSON body");
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("system text");
    let host_at = content
        .find("\nhost: herdr\nherdr pane: w1:p2\nherdr workspace: w1\nherdr tab: w1:t1\n</environment>")
        .unwrap_or_else(|| panic!("no host lines closing the environment: {content}"));
    // Right after the run's own facts — the config lines are the last of those.
    let previous_line = content[..host_at].rsplit('\n').next().unwrap_or_default();
    assert!(
        previous_line.starts_with("project config: "),
        "the host lines do not follow the run's facts: {content}"
    );
}
