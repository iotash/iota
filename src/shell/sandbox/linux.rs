//! Linux bubblewrap wrapper (internal/shell/`sandbox_linux.go`): a read-only root with the writable roots bound
//! back in.

use std::path::PathBuf;

/// Whether `bwrap` is on `PATH`.
pub(crate) fn available() -> bool {
    crate::shell::exec::find_in_path("bwrap").is_some()
}

/// Builds `bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --die-with-parent [--bind p p for existing dirs]
/// [--unshare-net iff !network] -- <shell> <shell args…> <script>`.
// The three per-OS backends share the signature `exec` dispatches on; only the `other` stub can fail.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn command(
    shell: &crate::shell::interp::Interpreter,
    script: &str,
    writable: &[PathBuf],
    network: bool,
) -> Result<tokio::process::Command, String> {
    let mut cmd = tokio::process::Command::new("bwrap");
    cmd.args([
        "--ro-bind",
        "/",
        "/",
        "--dev-bind",
        "/dev",
        "/dev",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]);
    for p in writable {
        // Non-existent or non-directory write roots are silently skipped (sandbox_linux.go:30-34).
        if std::fs::metadata(p).is_ok_and(|m| m.is_dir()) {
            cmd.arg("--bind").arg(p).arg(p);
        }
    }
    if !network {
        cmd.arg("--unshare-net");
    }
    cmd.arg("--")
        .arg(&shell.program)
        .args(shell.args())
        .arg(script);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::command;
    use crate::shell::interp::Interpreter;

    // New (tool-shell.md "BWRAP ARGV"): the argv is byte-pinned, existing dirs only, `--unshare-net` last.
    #[test]
    fn bwrap_argv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let missing = root.join("nope");
        let bash = Interpreter::new("/bin/bash");
        let cmd = command(&bash, "echo hi", &[root.clone(), missing], false).expect("argv");
        let argv: Vec<String> = std::iter::once(cmd.as_std().get_program())
            .chain(cmd.as_std().get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let root = root.to_string_lossy().into_owned();
        assert_eq!(
            argv,
            vec![
                "bwrap".to_owned(),
                "--ro-bind".to_owned(),
                "/".to_owned(),
                "/".to_owned(),
                "--dev-bind".to_owned(),
                "/dev".to_owned(),
                "/dev".to_owned(),
                "--proc".to_owned(),
                "/proc".to_owned(),
                "--die-with-parent".to_owned(),
                "--bind".to_owned(),
                root.clone(),
                root,
                "--unshare-net".to_owned(),
                "--".to_owned(),
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "echo hi".to_owned(),
            ]
        );

        // network: true drops --unshare-net and nothing else.
        let cmd = command(&bash, "x", &[PathBuf::from("/nope")], true).expect("a");
        let networked: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !networked.contains(&"--unshare-net".to_owned()),
            "{networked:?}"
        );
        assert_eq!(
            &networked[networked.len() - 4..],
            ["--", "/bin/bash", "-c", "x"]
        );
    }
}
