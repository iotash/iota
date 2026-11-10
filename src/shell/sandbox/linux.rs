//! Linux bubblewrap wrapper (internal/shell/`sandbox_linux.go`): a read-only root with the writable roots bound
//! back in, and the protected paths under them pinned again.

use std::path::PathBuf;

use crate::app::protected::{Protected, Reach};

/// Whether `bwrap` is on `PATH`.
pub(crate) fn available() -> bool {
    crate::shell::exec::find_in_path("bwrap").is_some()
}

/// Builds `bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --die-with-parent [--bind p p for existing dirs]
/// [protected binds] [--unshare-net iff !network] -- <shell> <shell args…> <script>`.
///
/// The protected binds come after every writable one (a later mount covers an earlier one), for each protected
/// path that exists and lies under a bound writable root — elsewhere the read-only `/` already covers it: a
/// [`Reach::Tree`] path is `--ro-bind`ed over itself; a [`Reach::Entry`] directory is `--bind`ed over itself,
/// which leaves its contents writable but makes it a mount point the command cannot rename, remove or replace
/// (`EBUSY`), and an [`Reach::Entry`] file (`.git` as a `gitdir:` file) is `--ro-bind`ed. A path that does not
/// exist cannot be bound, so a command may still CREATE one (a `.iota.yaml`, the `.git/hooks` of a repository
/// it inits): on Linux the `code` writers' refusal is what covers that case, not the sandbox.
// The three per-OS backends share the signature `exec` dispatches on; only the `other` stub can fail.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn command(
    shell: &crate::shell::interp::Interpreter,
    script: &str,
    writable: &[PathBuf],
    protected: &[Protected],
    network: bool,
) -> Result<tokio::process::Command, super::SandboxError> {
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
    // Non-existent or non-directory write roots are silently skipped (sandbox_linux.go:30-34).
    let bound: Vec<&PathBuf> = writable
        .iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_dir()))
        .collect();
    for p in &bound {
        cmd.arg("--bind").arg(p).arg(p);
    }
    for e in protected {
        let Ok(meta) = std::fs::metadata(&e.path) else {
            continue;
        };
        if !bound.iter().any(|w| e.path.starts_with(w)) {
            continue;
        }
        let bind = match e.reach {
            Reach::Entry if meta.is_dir() => "--bind",
            Reach::Entry | Reach::Tree => "--ro-bind",
        };
        cmd.arg(bind).arg(&e.path).arg(&e.path);
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
        let cmd = command(&bash, "echo hi", &[root.clone(), missing], &[], false).expect("argv");
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
        let cmd = command(&bash, "x", &[PathBuf::from("/nope")], &[], true).expect("a");
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

    // The protected binds follow every writable bind: an existing tree path under a bound root is ro-bound over
    // itself, an existing entry directory (`.git`) bound over itself so it cannot be renamed or replaced; a
    // missing path (`.iota.yaml`, `.git/config` here) cannot be bound and is skipped, and so is one outside every
    // bound root (the trust record here), which the read-only `/` already covers.
    #[test]
    fn bwrap_pins_the_protected_paths_after_the_writable_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let extra = dir.path().join("extra");
        let home = dir.path().join("home/.iota");
        for d in [root.join(".git/hooks"), extra.clone(), home.clone()] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        std::fs::write(home.join("trusted.json"), "{}").expect("record");
        let protected = crate::app::protected::table(&root, home.parent());
        let bash = Interpreter::new("/bin/bash");
        let cmd =
            command(&bash, "x", &[root.clone(), extra.clone()], &protected, true).expect("argv");
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let (git, hooks) = (root.join(".git"), root.join(".git/hooks"));
        let want: Vec<String> = [
            ("--bind", s(&root)),
            ("--bind", s(&extra)),
            ("--bind", s(&git)),
            ("--ro-bind", s(&hooks)),
        ]
        .into_iter()
        .flat_map(|(flag, p)| [flag.to_owned(), p.clone(), p])
        .collect();
        assert_eq!(&argv[9..argv.len() - 4], want.as_slice(), "{argv:?}");
    }
}
