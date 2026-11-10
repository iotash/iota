//! macOS Seatbelt wrapper (internal/shell/`sandbox_darwin.go`): `sandbox-exec` with a deny-write profile that opens
//! the writable roots, then closes the protected paths again.

use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use crate::app::fs::physical;
use crate::app::protected::{Protected, Reach};

/// The Seatbelt launcher.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The `/private` twins Seatbelt matches after the symlink resolution of `/tmp`, `/var` and `/etc`.
const PRIVATE_PREFIXES: [&str; 3] = ["/tmp", "/var", "/etc"];

/// Whether `SANDBOX_EXEC` is a regular file.
pub(crate) fn available() -> bool {
    std::fs::metadata(SANDBOX_EXEC).is_ok_and(|m| m.is_file())
}

/// Builds `sandbox-exec -p <profile> -D W{i}=<path>… -D P{j}=<path>… <shell> <shell args…> <script>`; every
/// writable path is expanded to itself, `/private` + p for p under `/tmp`, `/var`, `/etc`, and its canonical form
/// when different. The protected paths follow the allow block as one deny block (a later Seatbelt rule wins over
/// an earlier one, so no writable root re-opens them): `subpath` for a [`Reach::Tree`], `literal` for a
/// [`Reach::Entry`], each expanded the same way but resolved with [`physical`], since a protected path need not
/// exist — a `.iota.yaml` that is not there yet is exactly what must not be created.
// The three per-OS backends share the signature `exec` dispatches on; only the `other` stub can fail.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn command(
    shell: &crate::shell::interp::Interpreter,
    script: &str,
    writable: &[PathBuf],
    protected: &[Protected],
    network: bool,
) -> Result<tokio::process::Command, super::SandboxError> {
    let mut profile = String::from("(version 1)\n(allow default)\n");
    if !network {
        profile.push_str("(deny network*)\n");
    }
    profile.push_str("(deny file-write*)\n(allow file-write*\n  (subpath \"/dev\")\n");
    let mut params: Vec<OsString> = Vec::new();
    // Paths go in as -D parameters, never spliced into the profile text — quoting stays sandbox-exec's
    // problem, not ours.
    let mut param = |profile: &mut String, rule: &str, name: String, p: &Path| {
        let _ = writeln!(profile, "  ({rule} (param \"{name}\"))");
        params.push(OsString::from("-D"));
        let mut arg = OsString::from(format!("{name}="));
        arg.push(p.as_os_str());
        params.push(arg);
    };
    let writable = writable
        .iter()
        .flat_map(|p| spellings(p, std::fs::canonicalize(p).ok()));
    for (i, p) in writable.enumerate() {
        param(&mut profile, "subpath", format!("W{i}"), &p);
    }
    profile.push_str(")\n");
    if !protected.is_empty() {
        profile.push_str("(deny file-write*\n");
        let denied = protected.iter().flat_map(|e| {
            let rule = match e.reach {
                Reach::Tree => "subpath",
                Reach::Entry => "literal",
            };
            spellings(&e.path, physical(&e.path)).map(move |p| (rule, p))
        });
        for (j, (rule, p)) in denied.enumerate() {
            param(&mut profile, rule, format!("P{j}"), &p);
        }
        profile.push_str(")\n");
    }

    let mut cmd = tokio::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile);
    cmd.args(params);
    cmd.arg(&shell.program).args(shell.args()).arg(script);
    Ok(cmd)
}

/// The spellings Seatbelt may see `p` under: `p`, its `/private` twin when it lies under `/tmp`, `/var` or
/// `/etc` (symlinks into `/private` on macOS, and Seatbelt matches the resolved path — `sandbox_darwin.go:28-42`),
/// and `resolved` when it is known and different.
fn spellings(p: &Path, resolved: Option<PathBuf>) -> impl Iterator<Item = PathBuf> {
    let twins = PRIVATE_PREFIXES
        .iter()
        .filter(|prefix| has_path_prefix(p, prefix))
        .map(|_| {
            let mut twin = OsString::from("/private");
            twin.push(p.as_os_str());
            PathBuf::from(twin)
        })
        .collect::<Vec<_>>();
    let resolved = resolved.filter(|r| r != p);
    std::iter::once(p.to_path_buf())
        .chain(twins)
        .chain(resolved)
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

    // New (tool-shell.md "SEATBELT PROFILE TEXT, exact bytes"): the profile and argv are byte-pinned. The
    // protected table closes its paths again AFTER the allow block — later rules win — `literal` for an entry,
    // `subpath` for a tree.
    #[test]
    fn seatbelt_profile_and_argv() {
        let writable = vec![PathBuf::from("/no-such-iota-root"), PathBuf::from("/tmp")];
        let protected = crate::app::protected::table(
            Path::new("/no-such-iota-root"),
            &[],
            Some(Path::new("/no-such-home")),
        );
        let bash = Interpreter::new("/bin/bash");
        let cmd = command(&bash, "echo hi", &writable, &protected, false).expect("profile");
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
             (subpath (param \"W2\"))\n  (subpath (param \"W3\"))\n)\n(deny file-write*\n  \
             (subpath (param \"P0\"))\n  (subpath (param \"P1\"))\n  (literal (param \"P2\"))\n  \
             (subpath (param \"P3\"))\n  (subpath (param \"P4\"))\n  (subpath (param \"P5\"))\n  \
             (subpath (param \"P6\"))\n  (subpath (param \"P7\"))\n  (literal (param \"P8\"))\n  \
             (subpath (param \"P9\"))\n)\n"
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
                "-D",
                "P0=/no-such-iota-root/.iota.yaml",
                "-D",
                "P1=/no-such-iota-root/.iota.yml",
                "-D",
                "P2=/no-such-iota-root/.git",
                "-D",
                "P3=/no-such-iota-root/.git/hooks",
                "-D",
                "P4=/no-such-iota-root/.git/config",
                "-D",
                "P5=/no-such-iota-root/.git/config.worktree",
                "-D",
                "P6=/no-such-home/.iota.yaml",
                "-D",
                "P7=/no-such-home/.iota.yml",
                "-D",
                "P8=/no-such-home/.iota",
                "-D",
                "P9=/no-such-home/.iota/trusted.json",
                "/bin/bash",
                "-c",
                "echo hi",
            ]
        );

        // network: true drops the (deny network*) line and nothing else; no protected paths, no deny block.
        let cmd = command(
            &bash,
            "x",
            &[PathBuf::from("/no-such-iota-root")],
            &[],
            true,
        )
        .expect("p");
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
