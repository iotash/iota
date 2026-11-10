//! The trust gate on a project's `.iota.yaml`, end to end through the binary (`config::trust`, `cmd::trust`).
//!
//! The project file under test starts an MCP server whose command touches a marker file: the marker existing
//! is the proof that project-controlled code ran. The home config is the user's own and always in force.
//! Every child runs with a cleared environment, a temp cwd and a temp `HOME` (the `tests/cmd/cli.rs`
//! discipline).
#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use crate::common::{cleared_env, temp_project, transcript};
use tempfile::TempDir;
use wiremock::MockServer;

/// A cwd holding a project config whose MCP server touches `<cwd>/marker`, a `HOME` whose `~/.iota.yaml`
/// points the default agent at `server`, and the marker's path.
fn fixture(server: &MockServer) -> (TempDir, PathBuf, PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    fs::write(
        home.join(".iota.yaml"),
        format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {{model: m}}\n",
            server.uri()
        ),
    )
    .expect("write home config");
    let marker = dir.path().join("marker");
    fs::write(
        dir.path().join(".iota.yaml"),
        format!(
            "mcp_servers:\n  planted: {{command: sh, args: [-c, \"touch '{}'\"]}}\n",
            marker.display()
        ),
    )
    .expect("write project config");
    (dir, home, marker)
}

/// `iota …` in `cwd` under `home`, stdin closed.
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

/// `<cwd>/.iota.yaml` as the child names it: its cwd is what the OS reports, symlinks resolved (macOS's
/// `/var` is `/private/var`).
fn as_child_names(cwd: &Path) -> PathBuf {
    fs::canonicalize(cwd)
        .expect("canonical cwd")
        .join(".iota.yaml")
}

/// A headless run: `-m hi`.
async fn headless(cwd: &Path, home: &Path) -> Output {
    let mut cmd = iota(cwd, home);
    cmd.args(["-m", "hi"]);
    output(cmd).await
}

/// `iota config trust` in `cwd`.
fn trust(cwd: &Path, home: &Path) -> Output {
    let o = iota(cwd, home)
        .args(["config", "trust"])
        .output()
        .expect("run iota config trust");
    assert!(o.status.success(), "{}", err(&o));
    o
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The project's MCP server ran (it touches the marker as it starts; the connect then fails, which is only a
/// warning).
fn ran(marker: &Path) -> bool {
    marker.exists()
}

/// A headless run never starts what an untrusted project file describes: one warning names the file,
/// `iota config trust` and `-c`, and the run answers on the home config. Trusted, the file takes effect;
/// one changed byte and it stops again, until it is trusted again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_headless_run_skips_an_untrusted_project_config() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home, marker) = fixture(&server);
    let project = dir.path().join(".iota.yaml");
    let shown = as_child_names(dir.path());

    let o = headless(dir.path(), &home).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        !ran(&marker),
        "an untrusted project config started its server"
    );
    let warning = err(&o);
    assert_eq!(warning.lines().count(), 1, "{warning}");
    assert!(
        warning.starts_with("Warning: skipping project config "),
        "{warning}"
    );
    assert!(
        warning.contains(".iota.yaml (it is not trusted)"),
        "{warning}"
    );
    assert!(warning.contains("`iota config trust`"), "{warning}");
    assert!(warning.contains("-c"), "{warning}");
    assert_eq!(
        out(&o),
        format!("{}\n", transcript::REPLY),
        "the home config ran"
    );

    // `iota config trust` records it and says what the file does.
    let o = trust(dir.path(), &home);
    assert_eq!(
        out(&o),
        format!(
            "Trusted {}\nIt sets:\n  mcp_servers.planted: runs sh -c \"touch '{}'\"\n",
            shown.display(),
            marker.display()
        )
    );
    let o = headless(dir.path(), &home).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(ran(&marker), "a trusted project config takes effect");
    assert!(!err(&o).contains("skipping"), "{}", err(&o));

    // One byte more: the record no longer matches.
    fs::remove_file(&marker).expect("reset the marker");
    let mut bytes = fs::read(&project).expect("read");
    bytes.push(b'\n');
    fs::write(&project, &bytes).expect("edit");
    let o = headless(dir.path(), &home).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        !ran(&marker),
        "an edited project config ran on an old trust"
    );
    assert!(
        err(&o).contains(&format!(
            "{} (it has changed since it was trusted)",
            shown.display()
        )),
        "{}",
        err(&o)
    );

    trust(dir.path(), &home);
    let o = headless(dir.path(), &home).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(ran(&marker), "re-trusted, it takes effect again");

    // `untrust` forgets it.
    fs::remove_file(&marker).expect("reset the marker");
    let o = iota(dir.path(), &home)
        .args(["config", "untrust"])
        .output()
        .expect("untrust");
    assert_eq!(out(&o), format!("No longer trusted: {}\n", shown.display()));
    let o = headless(dir.path(), &home).await;
    assert!(!ran(&marker), "untrusted again");
    assert!(err(&o).contains("(it is not trusted)"), "{}", err(&o));
}

/// `-c <file>` is a file the user named: it is used without a question and without a record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_config_needs_no_trust() {
    let server = MockServer::start().await;
    transcript::openai_transcript(&server).await;
    let (dir, home, marker) = fixture(&server);
    // The project file plus the home file's agent, as one file named with -c.
    let explicit = dir.path().join("explicit.yaml");
    fs::write(
        &explicit,
        format!(
            "{}{}",
            fs::read_to_string(home.join(".iota.yaml")).expect("home"),
            fs::read_to_string(dir.path().join(".iota.yaml")).expect("project")
        ),
    )
    .expect("write explicit");
    let mut cmd = iota(dir.path(), &home);
    cmd.args(["-c", explicit.to_str().expect("utf-8"), "-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(ran(&marker), "-c is used as named");
    assert!(!err(&o).contains("skipping"), "{}", err(&o));
    assert!(
        !home.join(".iota").join("trusted.json").exists(),
        "-c needs no record"
    );
}

/// An interactive start whose stdin is not a terminal is never asked: it skips the file with the warning and
/// exits on its own (here at the interactive branch's own refusal of a piped stdout) — it does not wait on
/// a stdin that is open and silent.
#[test]
fn a_non_terminal_interactive_start_does_not_wait_for_an_answer() {
    let server_less = "http://127.0.0.1:9";
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    fs::write(
        home.join(".iota.yaml"),
        format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {server_less}}}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {{model: m}}\n"
        ),
    )
    .expect("write home config");
    let marker = dir.path().join("marker");
    fs::write(
        dir.path().join(".iota.yaml"),
        format!(
            "mcp_servers:\n  planted: {{command: sh, args: [-c, \"touch '{}'\"]}}\n",
            marker.display()
        ),
    )
    .expect("write project config");

    let mut cmd = iota(dir.path(), &home);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn iota");
    // Held open and never written: a run that read it would block here forever.
    let stdin = child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().ok();
            panic!("the interactive start waited on a non-terminal stdin");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    let o = child.wait_with_output().expect("collect");
    assert_eq!(status.code(), Some(1), "stderr: {}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.starts_with("Warning: skipping project config "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with(
            "Error: interactive mode requires a terminal; use -m/--message for piped input\n"
        ),
        "{stderr}"
    );
    assert!(!ran(&marker));
}

/// The record `iota config trust` writes is the owner's alone, and `config path` says which project file a
/// run would skip.
#[test]
fn the_record_is_private_and_config_path_names_the_skip() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, dirs) = temp_project(&[(".iota.yaml", "write_roots: [/tmp]\n")]);
    let home = dirs.home.expect("fixture home");
    let project = as_child_names(dir.path());

    let o = iota(dir.path(), &home)
        .args(["config", "path"])
        .output()
        .expect("path");
    assert_eq!(
        out(&o),
        format!("{}  (not trusted: skipped)\n", project.display())
    );

    let o = trust(dir.path(), &home);
    assert_eq!(
        out(&o),
        format!(
            "Trusted {}\nIt sets:\n  write_roots: /tmp\n",
            project.display()
        )
    );
    let record = home.join(".iota").join("trusted.json");
    let mode = fs::metadata(&record).expect("record").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let o = iota(dir.path(), &home)
        .args(["config", "path"])
        .output()
        .expect("path");
    assert_eq!(out(&o), format!("{}\n", project.display()));

    // `--path` goes with trust/untrust alone.
    let o = iota(dir.path(), &home)
        .args(["config", "check", "--path", "x"])
        .output()
        .expect("check");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        "Error: --path goes with `iota config trust` and `iota config untrust`\n"
    );
    // `--path` names a file elsewhere.
    let elsewhere = dir.path().join("other.yaml");
    fs::write(&elsewhere, "outside_writes: refuse\n").expect("write");
    let o = iota(&home, &home)
        .args([
            "config",
            "trust",
            "--path",
            elsewhere.to_str().expect("utf-8"),
        ])
        .output()
        .expect("trust --path");
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).starts_with(&format!("Trusted {}\n", elsewhere.display())));
}
