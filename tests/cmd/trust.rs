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

/// `iota mcp list --probe` starts what the files declare, so an untrusted project file's servers are listed
/// but never started (one warning says why); trusted, they are probed like any other.
#[test]
fn mcp_list_probe_never_starts_an_untrusted_project_server() {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    let marker = dir.path().join("marker");
    fs::write(
        dir.path().join(".iota.yaml"),
        format!(
            "mcp_servers:\n  planted: {{command: sh, args: [-c, \"touch '{}'\"]}}\n",
            marker.display()
        ),
    )
    .expect("write project config");

    let probe = || {
        iota(dir.path(), &home)
            .args(["mcp", "list", "--probe"])
            .output()
            .expect("mcp list --probe")
    };
    let o = probe();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(!ran(&marker), "--probe started an untrusted project server");
    assert!(
        err(&o).starts_with("Warning: skipping project config ")
            && err(&o).contains("(it is not trusted)"),
        "{}",
        err(&o)
    );
    assert!(
        out(&o).contains("planted") && out(&o).contains("not probed (project config not trusted)"),
        "{}",
        out(&o)
    );
    // `--json` says the same.
    let o = iota(dir.path(), &home)
        .args(["mcp", "list", "--probe", "--json"])
        .output()
        .expect("mcp list --probe --json");
    assert!(!ran(&marker));
    assert!(
        out(&o).contains("\"state\": \"not trusted\""),
        "{}",
        out(&o)
    );

    trust(dir.path(), &home);
    let o = probe();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(ran(&marker), "a trusted project server is probed");
    assert!(!err(&o).contains("skipping"), "{}", err(&o));
}

/// When the only config is a skipped project file, the refusal that follows says why the file's agent is
/// not there — instead of only telling the user to add one the file already has.
#[test]
fn no_agent_after_a_skip_names_the_skip() {
    let (dir, dirs) = temp_project(&[(
        ".iota.yaml",
        "providers:\n  p: {type: openai, key: sk-x}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {model: m}\n",
    )]);
    let home = dirs.home.expect("fixture home");
    let project = as_child_names(dir.path());
    let o = iota(dir.path(), &home)
        .args(["-m", "hi"])
        .output()
        .expect("run");
    assert_eq!(o.status.code(), Some(1), "stderr: {}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.starts_with("Warning: skipping project config "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with(&format!(
            "Error: no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry to your config (`iota config path` names the file)\n(the project config {} was skipped: it is not trusted — `iota config trust` trusts it)\n",
            project.display()
        )),
        "{stderr}"
    );
}

/// `iota mcp login` expands an entry's `client_secret` and sends it to the entry's url, so a server declared
/// in an untrusted project file is refused before anything is contacted — one line naming the file and
/// `iota config trust`. Trusted, the login goes ahead and reaches the server.
#[test]
fn mcp_login_refuses_a_server_from_an_untrusted_project_file() {
    use std::io::Read as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let contacted = Arc::new(AtomicBool::new(false));
    {
        let contacted = Arc::clone(&contacted);
        // Every connection is recorded and dropped unanswered: the login then fails fast, after the contact.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                contacted.store(true, Ordering::SeqCst);
                if let Ok(mut s) = stream {
                    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                    let _ = s.read(&mut [0; 1024]);
                }
            }
        });
    }
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    fs::write(
        dir.path().join(".iota.yaml"),
        format!(
            "mcp_servers:\n  nb: {{url: \"http://127.0.0.1:{port}/mcp\", client_id: app, client_secret: \"${{SOME_VAR}}\"}}\n"
        ),
    )
    .expect("write project config");
    let login = || {
        iota(dir.path(), &home)
            .env("SOME_VAR", "s3cret")
            .args(["mcp", "login", "nb", "--no-browser"])
            .output()
            .expect("mcp login")
    };

    let o = login();
    assert_eq!(o.status.code(), Some(1), "stderr: {}", err(&o));
    assert_eq!(
        err(&o),
        format!(
            "Error: cannot log in to nb: it is declared in project config {} (it is not trusted): run `iota config trust` to trust it, or pass it with -c\n",
            as_child_names(dir.path()).display()
        )
    );
    assert!(out(&o).is_empty(), "{}", out(&o));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !contacted.load(Ordering::SeqCst),
        "an untrusted project server was contacted"
    );

    trust(dir.path(), &home);
    let o = login();
    assert!(
        !err(&o).contains("cannot log in to nb"),
        "a trusted server was refused: {}",
        err(&o)
    );
    assert!(
        contacted.load(Ordering::SeqCst),
        "the trusted login never reached the server: {}",
        err(&o)
    );
}
