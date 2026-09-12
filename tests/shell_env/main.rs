//! `IOTA_SHELL` end to end: a real process whose environment carries the override from birth, running a
//! real command line under the interpreter it names.
//!
//! The override is read from the PROCESS environment, and `std::env::set_var` is both forbidden here
//! (`unsafe_code = "forbid"`) and wrong — every other test binary spawns shells concurrently, and a variable
//! set mid-run would reach them too. So the environment is set the only way a user ever sets it: on a child
//! process. The two `#[ignore]d` tests below ARE that child — the parent re-invokes this binary with the
//! variable in place and asserts the child's verdict.
//!
//! Unix-only, because the fixture is a `#!/bin/sh` script. The Windows rungs of the same ladder are unit
//! tests over an injected machine (`src/shell/interp.rs`), which run everywhere, this platform included.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use iota::shell::exec::{self, Options};
use tokio_util::sync::CancellationToken;

/// A shell of our own, which echoes the argv it was handed: its output then proves both halves of the
/// resolution — the program that ran, and the `-c` the family put in front of the script.
fn fake_shell(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let fake = dir.join("fakesh");
    std::fs::write(&fake, "#!/bin/sh\necho \"FAKE:$*\"\n").expect("write the fake shell");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    fake
}

/// Re-invokes THIS test binary for one `#[ignore]d` test, with `IOTA_SHELL` set to `shell`.
fn child(test: &str, shell: &Path) -> Output {
    Command::new(std::env::current_exe().expect("the test binary's own path"))
        .args(["--exact", "--ignored", "--nocapture", test])
        .env("IOTA_SHELL", shell)
        .output()
        .expect("run the child test")
}

/// The child's stdout and stderr, for the parent's failure message.
fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn iota_shell_replaces_the_interpreter_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = fake_shell(dir.path());

    for (test, shell) in [
        ("the_override_runs_the_named_shell", fake.as_path()),
        (
            "an_unrunnable_override_fails_the_run",
            Path::new("/definitely/not/a/shell"),
        ),
    ] {
        let out = child(test, shell);
        let said = said(&out);
        assert!(out.status.success(), "{test}:\n{said}");
        // A filter that matches nothing also exits 0, so the count is part of the claim.
        assert!(
            said.contains("1 passed"),
            "{test} did not run as the child:\n{said}"
        );
    }
}

/// One run under the interpreter the environment names.
async fn run(command: &str, dir: &Path) -> exec::RunResult {
    exec::run(
        &CancellationToken::new(),
        Options {
            command: command.to_owned(),
            dir: dir.to_path_buf(),
            timeout: Some(Duration::from_secs(30)),
            sandbox: None,
        },
    )
    .await
}

// The child half: `IOTA_SHELL` names our fake shell, so that is what runs — bash never does.
#[tokio::test]
#[ignore = "the child of iota_shell_replaces_the_interpreter_end_to_end, which sets IOTA_SHELL for it"]
async fn the_override_runs_the_named_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = std::env::var("IOTA_SHELL").expect("the parent set it");

    let res = run("echo hi", dir.path()).await;
    assert!(
        res.err.is_none() && res.exited && res.exit_code == 0,
        "{res:?}"
    );
    assert_eq!(res.output, "FAKE:-c echo hi\n", "bash ran instead: {res:?}");

    // The same answer the `bash` tool builds its description from.
    let shell = iota::shell::interp::resolve().expect("the override resolves");
    assert_eq!(shell.program, PathBuf::from(fake));
    assert!(shell.is_posix(), "{shell:?}");
}

// The child half: an override naming something unrunnable fails the run. It never falls back to the bash
// that is certainly on this machine — the user asked for something else, and the tool description says so.
#[tokio::test]
#[ignore = "the child of iota_shell_replaces_the_interpreter_end_to_end, which sets IOTA_SHELL for it"]
async fn an_unrunnable_override_fails_the_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let res = run("echo hi", dir.path()).await;
    let err = res
        .err
        .as_ref()
        .expect("a bad override must fail the run")
        .to_string();
    assert_eq!(
        err,
        "IOTA_SHELL is set to \"/definitely/not/a/shell\", which is not an executable on this system"
    );
    assert!(res.output.is_empty() && !res.exited, "{res:?}");
}
