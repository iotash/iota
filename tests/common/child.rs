//! The environment every child `iota` of the `tests/cmd` binaries runs under.
//!
//! One helper, three call sites (`cli.rs`, `interactive_cli.rs`, `session.rs`), because the
//! discipline is a property of the SUITE and not of any one file: the child gets a CLEARED
//! environment plus the handful of variables a run genuinely needs, so no developer config file
//! can reach it and no test reads or mutates the test process's own environment.

use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
};

/// `cmd` with that environment: `home` under the platform's home variable, a fixed `PATH` (a
/// literal, never read from this process, so a `--mcp` stdio server can still find a shell), and
/// on Windows the variables the OS itself reads out of a process environment.
///
/// The home variable is `$HOME` on unix and `%USERPROFILE%` on Windows — `app::user_home`'s own
/// rule (`os.UserHomeDir`), and setting only `HOME` is why eleven of these tests reported
/// `$HOME is not defined` against a fixture home that was sitting right there.
///
/// `env_clear` is a blunter instrument on Windows than on unix, where nothing below the program
/// reads the environment. `%SystemRoot%` is the one that bites: the Winsock catalog names its
/// service-provider DLLs as `%SystemRoot%\system32\…` and expands them against the CHILD's
/// environment, so a child without it cannot open a socket at all
/// (`WSAEPROVIDERFAILEDINIT`) — which is how five of these tests came to report
/// `error sending request for url (http://127.0.0.1:…)` against a wiremock server that was
/// serving perfectly well. `%TEMP%`/`%TMP%` are what `std::env::temp_dir` reads there, and with
/// both gone it falls back to `%USERPROFILE%` — the fixture home, which one test asserts stays
/// untouched.
pub fn cleared_env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    cmd.env_clear().env(home_var, home);
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        let drive = std::env::var_os("SystemDrive").unwrap_or_else(|| OsString::from("C:"));
        let temp = std::env::temp_dir();
        cmd.env("PATH", windows_path(&root))
            .env("SystemRoot", &root)
            .env("SystemDrive", drive)
            .env("TEMP", &temp)
            .env("TMP", &temp);
    } else {
        cmd.env("PATH", "/bin:/usr/bin");
    }
    cmd
}

/// The three directories a Windows `PATH` is expected to open with, spelled from `SystemRoot` the
/// way the machine's own default `PATH` does.
fn windows_path(root: &OsStr) -> OsString {
    let mut path = OsString::new();
    for (i, tail) in [r"\system32", "", r"\system32\Wbem"]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            path.push(";");
        }
        path.push(root);
        path.push(tail);
    }
    path
}
