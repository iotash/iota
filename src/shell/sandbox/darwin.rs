//! macOS Seatbelt wrapper (internal/shell/`sandbox_darwin.go`): `sandbox-exec` with a deny-write profile that opens
//! the writable roots.

use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
};

/// The Seatbelt launcher.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The `/private` twins Seatbelt matches after the symlink resolution of `/tmp`, `/var` and `/etc`.
const PRIVATE_PREFIXES: [&str; 3] = ["/tmp", "/var", "/etc"];

/// Whether `SANDBOX_EXEC` is a regular file.
pub(crate) fn available() -> bool {
    std::fs::metadata(SANDBOX_EXEC).is_ok_and(|m| m.is_file())
}

/// Builds `sandbox-exec -p <profile> -D W{i}=<path>… <shell> <shell args…> <script>`; every writable path is
/// expanded to itself, `/private` + p for p under `/tmp`, `/var`, `/etc`, and its canonical form when
/// different.
// The three per-OS backends share the signature `exec` dispatches on; only the `other` stub can fail.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn command(
    shell: &crate::shell::interp::Interpreter,
    script: &str,
    writable: &[PathBuf],
    network: bool,
) -> Result<tokio::process::Command, String> {
    // /tmp and /var are symlinks into /private on macOS; Seatbelt matches the resolved path, so each writable
    // root contributes its /private twin and its fully-resolved form (sandbox_darwin.go:28-42).
    let mut expanded: Vec<PathBuf> = Vec::with_capacity(writable.len() * 2);
    for p in writable {
        expanded.push(p.clone());
        for prefix in PRIVATE_PREFIXES {
            if has_path_prefix(p, prefix) {
                let mut twin = OsString::from("/private");
                twin.push(p.as_os_str());
                expanded.push(PathBuf::from(twin));
            }
        }
        if let Ok(resolved) = std::fs::canonicalize(p)
            && resolved != *p
        {
            expanded.push(resolved);
        }
    }

    let mut profile = String::from("(version 1)\n(allow default)\n");
    if !network {
        profile.push_str("(deny network*)\n");
    }
    profile.push_str("(deny file-write*)\n(allow file-write*\n  (subpath \"/dev\")\n");
    let mut params: Vec<OsString> = Vec::with_capacity(expanded.len() * 2);
    for (i, p) in expanded.iter().enumerate() {
        // Paths go in as -D parameters, never spliced into the profile text — quoting stays sandbox-exec's
        // problem, not ours.
        let _ = writeln!(profile, "  (subpath (param \"W{i}\"))");
        params.push(OsString::from("-D"));
        let mut arg = OsString::from(format!("W{i}="));
        arg.push(p.as_os_str());
        params.push(arg);
    }
    profile.push_str(")\n");

    let mut cmd = tokio::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile);
    cmd.args(params);
    cmd.arg(&shell.program).args(shell.args()).arg(script);
    Ok(cmd)
}

/// Whether `p` is `prefix` or lies under it, comparing bytes exactly like Go's `strings.HasPrefix`.
fn has_path_prefix(p: &Path, prefix: &str) -> bool {
    let bytes = p.as_os_str().as_encoded_bytes();
    bytes == prefix.as_bytes() || bytes.starts_with(format!("{prefix}/").as_bytes())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{command, has_path_prefix};
    use crate::shell::interp::Interpreter;

    // New (tool-shell.md "SEATBELT PROFILE TEXT, exact bytes"): the profile and argv are byte-pinned.
    #[test]
    fn seatbelt_profile_and_argv() {
        let writable = vec![PathBuf::from("/no-such-iota-root"), PathBuf::from("/tmp")];
        let bash = Interpreter::new("/bin/bash");
        let cmd = command(&bash, "echo hi", &writable, false).expect("profile");
        let argv: Vec<String> = std::iter::once(cmd.as_std().get_program())
            .chain(cmd.as_std().get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "/usr/bin/sandbox-exec");
        assert_eq!(argv[1], "-p");
        // /tmp contributes its /private twin AND its canonical form (/private/tmp), so W1 and W2 both exist.
        assert_eq!(
            argv[2],
            "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write*\n  \
             (subpath \"/dev\")\n  (subpath (param \"W0\"))\n  (subpath (param \"W1\"))\n  \
             (subpath (param \"W2\"))\n  (subpath (param \"W3\"))\n)\n"
        );
        assert_eq!(
            &argv[3..],
            [
                "-D",
                "W0=/no-such-iota-root",
                "-D",
                "W1=/tmp",
                "-D",
                "W2=/private/tmp",
                "-D",
                "W3=/private/tmp",
                "/bin/bash",
                "-c",
                "echo hi",
            ]
        );

        // network: true drops the (deny network*) line and nothing else.
        let cmd = command(&bash, "x", &[PathBuf::from("/no-such-iota-root")], true).expect("p");
        let profile = cmd
            .as_std()
            .get_args()
            .nth(1)
            .expect("profile")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            profile,
            "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n  \
             (subpath \"/dev\")\n  (subpath (param \"W0\"))\n)\n"
        );
    }

    #[test]
    fn private_twin_prefix_rule() {
        assert!(has_path_prefix(Path::new("/tmp"), "/tmp"));
        assert!(has_path_prefix(Path::new("/var/folders/x/T/"), "/var"));
        assert!(!has_path_prefix(Path::new("/variable"), "/var"));
        assert!(!has_path_prefix(Path::new("/etcetera"), "/etc"));
        assert!(!has_path_prefix(Path::new("/proj"), "/tmp"));
    }
}
