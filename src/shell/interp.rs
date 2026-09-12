//! Which interpreter runs a shell command, and how it is invoked.
//!
//! Unix has one answer and always had it: `bash -c <script>`. Windows has no single answer, so this module
//! asks the same question the three largest agent CLIs ask (Claude Code, Codex CLI, Gemini CLI all drive the
//! system shell rather than embedding one): Git Bash if the machine has it, PowerShell otherwise, `cmd.exe`
//! as the floor. Nothing is embedded and the binary does not grow — what changes is the [`Family`] the
//! `shell` tool then has to be honest about in its description.
//!
//! The order is:
//!
//! 1. `IOTA_SHELL` — an absolute path or a name on `PATH`. It wins on EVERY platform, not just Windows
//!    (goose's `GOOSE_SHELL` is the same shape), which is also what makes the override testable on a Mac.
//! 2. Windows only: Git Bash — `IOTA_GIT_BASH_PATH` when set, else the `../../bin/bash.exe` sibling of the
//!    `git.exe` on `PATH` (`CLAUDE_CODE_GIT_BASH_PATH` and `OPENCODE_GIT_BASH_PATH` both work this way),
//!    else the two default install roots. A bare `bash.exe` on `PATH` is deliberately NOT accepted: on
//!    Windows that name is usually the WSL launcher, whose filesystem is not the one the project is in.
//! 3. Windows only: `pwsh.exe`, then `powershell.exe`.
//! 4. Windows only: `cmd.exe` (`%COMSPEC%`). This level exists because dropping it has a price tag —
//!    Gemini CLI #26567: a group policy that blocks PowerShell left the CLI with nothing to run at all.
//!
//! An override that names something unrunnable is an error, never a silent fallback: a user who set
//! `IOTA_SHELL` and got PowerShell anyway would be told one thing by the tool description and another by
//! their configuration.
//!
//! [`detect`] is pure — the platform, the environment and the `PATH` scan all arrive as arguments — so the
//! Windows ladder is unit-testable on macOS, which is where it is in fact tested.

use std::path::{Path, PathBuf};

/// The environment-variable override, on every platform.
pub const SHELL_VAR: &str = "IOTA_SHELL";
/// The Windows-only override naming Git Bash explicitly.
pub const GIT_BASH_VAR: &str = "IOTA_GIT_BASH_PATH";

/// The dialect an interpreter speaks. The `shell` tool's description is written from this: a model told
/// "bash" while `powershell.exe` is what runs would write POSIX for a shell that does not read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `bash` and the rest of the POSIX family (Git Bash included): `-c <script>`.
    Posix,
    /// `pwsh.exe` / `powershell.exe`.
    PowerShell,
    /// `cmd.exe`.
    Cmd,
}

/// A resolved interpreter: the program to spawn and the dialect it speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interpreter {
    /// The executable to spawn.
    pub program: PathBuf,
    /// Its dialect, taken from the file name.
    pub family: Family,
}

/// Why no interpreter could be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoShell {
    /// Unix without `bash` — the text Go's `exec.LookPath` failure produced.
    #[error("bash is not installed on this system")]
    NoBash,
    /// Windows with neither Git Bash, PowerShell nor `cmd.exe`.
    #[error(
        "no shell interpreter found: tried Git Bash (install Git for Windows or set IOTA_GIT_BASH_PATH), pwsh.exe, powershell.exe and cmd.exe — set IOTA_SHELL to name one"
    )]
    NoInterpreter,
    /// An override naming something that is not an executable here.
    #[error("{var} is set to {spec:?}, which is not an executable on this system")]
    BadOverride {
        /// The variable that named it.
        var: &'static str,
        /// What it was set to.
        spec: String,
    },
}

/// What [`detect`] is allowed to know about the machine: the platform, the environment and the `PATH` scan.
/// All three are arguments so the Windows ladder can be walked in a test on any platform.
pub struct Probe<'a> {
    /// Whether to walk the Windows ladder (`cfg!(windows)` in production).
    pub windows: bool,
    /// One environment variable; `None` when unset.
    pub getenv: &'a dyn Fn(&str) -> Option<String>,
    /// The `PATH` scan, and — for a name with a directory in it — the executable check itself.
    pub look_path: &'a dyn Fn(&str) -> Option<PathBuf>,
}

impl Probe<'_> {
    /// One variable, with blank treated as unset (an exported-but-empty `IOTA_SHELL` is not a choice).
    fn var(&self, name: &str) -> Option<String> {
        (self.getenv)(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    /// A program named by the user: an absolute path, or a name on `PATH`. On Windows a name written
    /// without its extension (`IOTA_SHELL=pwsh`) is retried with the executable ones, since `PATH` entries
    /// carry them.
    fn program(&self, spec: &str) -> Option<PathBuf> {
        if let Some(found) = (self.look_path)(spec) {
            return Some(found);
        }
        if !self.windows {
            return None;
        }
        ["exe", "cmd", "bat"]
            .iter()
            .find_map(|ext| (self.look_path)(&format!("{spec}.{ext}")))
    }

    /// A path we built ourselves: kept only when it is an executable file.
    fn existing(&self, p: &Path) -> Option<PathBuf> {
        (self.look_path)(&p.to_string_lossy())
    }
}

impl Interpreter {
    /// The interpreter `program` is, with its dialect read off the file name.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let family = match program
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str()
        {
            "pwsh" | "powershell" => Family::PowerShell,
            "cmd" => Family::Cmd,
            _ => Family::Posix,
        };
        Self { program, family }
    }

    /// The arguments that precede the script — derived from the family, never from the platform, so
    /// `IOTA_SHELL=pwsh` is spelled correctly on a Mac too.
    ///
    /// PowerShell's three flags are the ones an agent needs: `-NoLogo` keeps the banner out of the captured
    /// output, `-NoProfile` keeps the user's interactive profile out of the command's environment (and off
    /// its startup path), and `-NonInteractive` makes a prompt an error instead of a hang — the child has no
    /// stdin.
    pub fn args(&self) -> &'static [&'static str] {
        match self.family {
            Family::Posix => &["-c"],
            Family::PowerShell => &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
            Family::Cmd => &["/C"],
        }
    }

    /// Whether this is a POSIX shell — what every POSIX-only test asks before it runs.
    pub fn is_posix(&self) -> bool {
        self.family == Family::Posix
    }

    /// How to show the tail of a background job's log in THIS dialect (the receipt the model is handed).
    pub fn tail_command(&self, path: &Path) -> String {
        let path = path.display();
        match self.family {
            Family::Posix => format!("tail -n 50 {path}"),
            Family::PowerShell => format!("Get-Content -Tail 50 {path}"),
            // cmd.exe has no tail; `type` is the whole file, which is the honest thing to offer.
            Family::Cmd => format!("type {path}"),
        }
    }

    /// `cd` into `dir` and then run `cmd`, in THIS dialect — the call header's rendering of an explicit
    /// `cwd` argument.
    pub fn chain_cd(&self, dir: &str, cmd: &str) -> String {
        match self.family {
            Family::Posix => format!("cd {dir} && {cmd}"),
            Family::PowerShell => format!("cd {dir}; {cmd}"),
            Family::Cmd => format!("cd /d {dir} && {cmd}"),
        }
    }
}

/// The interpreter for this machine: [`detect`] over the real platform, environment and `PATH`.
pub fn resolve() -> Result<Interpreter, NoShell> {
    detect(&Probe {
        windows: cfg!(windows),
        getenv: &|name| std::env::var(name).ok(),
        look_path: &super::exec::find_in_path,
    })
}

/// The ladder in the module doc, over an injected machine.
pub fn detect(probe: &Probe<'_>) -> Result<Interpreter, NoShell> {
    if let Some(spec) = probe.var(SHELL_VAR) {
        return probe
            .program(&spec)
            .map(Interpreter::new)
            .ok_or(NoShell::BadOverride {
                var: SHELL_VAR,
                spec,
            });
    }
    if !probe.windows {
        // Unix, unchanged since the port: bash is resolved per call, like Go's exec.LookPath.
        return (probe.look_path)("bash")
            .map(Interpreter::new)
            .ok_or(NoShell::NoBash);
    }
    if let Some(spec) = probe.var(GIT_BASH_VAR) {
        return probe
            .program(&spec)
            .map(Interpreter::new)
            .ok_or(NoShell::BadOverride {
                var: GIT_BASH_VAR,
                spec,
            });
    }
    if let Some(bash) = git_bash(probe) {
        return Ok(Interpreter::new(bash));
    }
    for name in ["pwsh.exe", "powershell.exe"] {
        if let Some(found) = (probe.look_path)(name) {
            return Ok(Interpreter::new(found));
        }
    }
    probe
        .var("COMSPEC")
        .and_then(|spec| probe.program(&spec))
        .or_else(|| (probe.look_path)("cmd.exe"))
        .map(Interpreter::new)
        .ok_or(NoShell::NoInterpreter)
}

/// Git Bash: the `bin/bash.exe` of the Git installation that owns the `git.exe` on `PATH` (which sits in
/// `<install>/cmd` or `<install>/mingw64/bin`, two levels under the root either way), then the system-wide
/// and per-user default install roots.
fn git_bash(probe: &Probe<'_>) -> Option<PathBuf> {
    let from_path = (probe.look_path)("git.exe")
        .and_then(|git| Some(git.parent()?.parent()?.join("bin").join("bash.exe")))
        .and_then(|bash| probe.existing(&bash));
    if from_path.is_some() {
        return from_path;
    }
    [
        ("ProgramFiles", "Git"),
        ("ProgramFiles(x86)", "Git"),
        ("LOCALAPPDATA", "Programs/Git"),
    ]
    .iter()
    .find_map(|(var, under)| {
        let root = PathBuf::from(probe.var(var)?);
        let bash = under
            .split('/')
            .fold(root, |p, seg| p.join(seg))
            .join("bin")
            .join("bash.exe");
        probe.existing(&bash)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{Family, GIT_BASH_VAR, Interpreter, NoShell, Probe, SHELL_VAR, detect};

    /// A fake machine: the set of paths that resolve, plus the environment. `look_path` answers a bare name
    /// from `PATH` (here: the `bin` directories the fixture names) and a path with directories in it by
    /// membership, exactly like the real `find_in_path` does.
    struct Machine {
        windows: bool,
        path: Vec<&'static str>,
        exists: Vec<String>,
        env: BTreeMap<&'static str, &'static str>,
    }

    impl Machine {
        fn windows(exists: &[&str]) -> Self {
            Self {
                windows: true,
                path: vec!["C:/Windows/System32", "C:/Program Files/Git/cmd"],
                exists: exists.iter().map(|s| (*s).to_owned()).collect(),
                env: BTreeMap::new(),
            }
        }

        fn unix(exists: &[&str]) -> Self {
            Self {
                windows: false,
                path: vec!["/usr/bin", "/bin"],
                exists: exists.iter().map(|s| (*s).to_owned()).collect(),
                env: BTreeMap::new(),
            }
        }

        fn with(mut self, var: &'static str, value: &'static str) -> Self {
            self.env.insert(var, value);
            self
        }

        fn detect(&self) -> Result<Interpreter, NoShell> {
            detect(&Probe {
                windows: self.windows,
                getenv: &|name| self.env.get(name).map(|v| (*v).to_owned()),
                look_path: &|name| {
                    let candidates: Vec<String> = if name.contains('/') {
                        vec![name.to_owned()]
                    } else {
                        self.path.iter().map(|d| format!("{d}/{name}")).collect()
                    };
                    candidates
                        .into_iter()
                        .find(|c| self.exists.contains(c))
                        .map(PathBuf::from)
                },
            })
        }
    }

    // The Windows ladder, one rung at a time — walked on whatever platform runs this test.
    #[test]
    fn windows_prefers_git_bash_then_powershell_then_cmd() {
        let git_bash = "C:/Program Files/Git/bin/bash.exe";
        let pwsh = "C:/Windows/System32/pwsh.exe";
        let ps = "C:/Windows/System32/powershell.exe";
        let cmd = "C:/Windows/System32/cmd.exe";
        let git = "C:/Program Files/Git/cmd/git.exe";

        // git.exe on PATH names the installation two levels up; its bin/bash.exe is the shell.
        let m = Machine::windows(&[git, git_bash, pwsh, ps, cmd]);
        let found = m.detect().expect("git bash");
        assert_eq!(found.program, PathBuf::from(git_bash));
        assert_eq!(found.family, Family::Posix);
        assert_eq!(found.args(), ["-c"]);

        // No Git at all: pwsh wins over powershell.
        let m = Machine::windows(&[pwsh, ps, cmd]);
        let found = m.detect().expect("pwsh");
        assert_eq!(found.program, PathBuf::from(pwsh));
        assert_eq!(found.family, Family::PowerShell);
        assert_eq!(
            found.args(),
            ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
        );

        // Windows PowerShell only.
        let m = Machine::windows(&[ps, cmd]);
        assert_eq!(m.detect().expect("powershell").program, PathBuf::from(ps));

        // The floor: cmd.exe, named by COMSPEC (a policy that blocks PowerShell is the case this exists
        // for — Gemini CLI #26567).
        let m = Machine::windows(&[cmd]).with("COMSPEC", cmd);
        let found = m.detect().expect("cmd");
        assert_eq!(found.program, PathBuf::from(cmd));
        assert_eq!(found.family, Family::Cmd);
        assert_eq!(found.args(), ["/C"]);
        // …and found on PATH when COMSPEC is unset.
        assert_eq!(
            Machine::windows(&[cmd]).detect().expect("cmd").program,
            PathBuf::from(cmd)
        );

        // Nothing at all.
        assert_eq!(
            Machine::windows(&[]).detect().expect_err("nothing"),
            NoShell::NoInterpreter
        );
    }

    // A bare `bash.exe` on PATH is the WSL launcher far more often than it is Git Bash, and its filesystem
    // is not the project's — it must not be picked up.
    #[test]
    fn windows_ignores_a_bare_bash_on_path() {
        let m = Machine::windows(&[
            "C:/Windows/System32/bash.exe",
            "C:/Windows/System32/powershell.exe",
        ]);
        assert_eq!(
            m.detect().expect("powershell").program,
            PathBuf::from("C:/Windows/System32/powershell.exe")
        );
    }

    // The default install roots, for a Git that is installed but not on PATH.
    #[test]
    fn windows_finds_git_bash_in_its_default_locations() {
        let system = "C:/Program Files/Git/bin/bash.exe";
        let user = "C:/Users/u/AppData/Local/Programs/Git/bin/bash.exe";
        let m = Machine::windows(&[system, "C:/Windows/System32/cmd.exe"])
            .with("ProgramFiles", "C:/Program Files");
        assert_eq!(m.detect().expect("git bash").program, PathBuf::from(system));

        let m = Machine::windows(&[user, "C:/Windows/System32/cmd.exe"])
            .with("LOCALAPPDATA", "C:/Users/u/AppData/Local");
        assert_eq!(m.detect().expect("git bash").program, PathBuf::from(user));
    }

    // IOTA_SHELL outranks everything, on both platforms, and names either a path or a PATH entry.
    #[test]
    fn the_shell_override_wins_everywhere() {
        let m = Machine::windows(&[
            "C:/Program Files/Git/cmd/git.exe",
            "C:/Program Files/Git/bin/bash.exe",
            "C:/tools/pwsh.exe",
        ])
        .with(SHELL_VAR, "C:/tools/pwsh.exe");
        let found = m.detect().expect("override");
        assert_eq!(found.program, PathBuf::from("C:/tools/pwsh.exe"));
        assert_eq!(found.family, Family::PowerShell);

        // A bare name is resolved through PATH…
        let m = Machine::unix(&["/usr/bin/zsh", "/bin/bash"]).with(SHELL_VAR, "zsh");
        let found = m.detect().expect("override");
        assert_eq!(found.program, PathBuf::from("/usr/bin/zsh"));
        assert_eq!(found.family, Family::Posix, "zsh takes -c like bash");

        // …and on Windows the extension may be left off, since PATH entries carry it.
        let m = Machine::windows(&["C:/Windows/System32/pwsh.exe"]).with(SHELL_VAR, "pwsh");
        assert_eq!(
            m.detect().expect("override").program,
            PathBuf::from("C:/Windows/System32/pwsh.exe")
        );

        // An override that names nothing runnable is an error, never a silent fallback to the ladder.
        let m =
            Machine::windows(&["C:/Windows/System32/cmd.exe"]).with(SHELL_VAR, "/no/such/shell");
        assert_eq!(
            m.detect().expect_err("bad override"),
            NoShell::BadOverride {
                var: SHELL_VAR,
                spec: "/no/such/shell".to_owned()
            }
        );
        assert_eq!(
            m.detect().expect_err("bad override").to_string(),
            "IOTA_SHELL is set to \"/no/such/shell\", which is not an executable on this system"
        );

        // Blank is not a choice.
        let m = Machine::unix(&["/bin/bash"]).with(SHELL_VAR, "  ");
        assert_eq!(
            m.detect().expect("blank falls through").program,
            PathBuf::from("/bin/bash")
        );
    }

    // IOTA_GIT_BASH_PATH is the Windows-only second rung, and has the same all-or-nothing rule.
    #[test]
    fn the_git_bash_override_is_explicit_and_strict() {
        let m = Machine::windows(&["C:/msys64/usr/bin/bash.exe", "C:/Windows/System32/pwsh.exe"])
            .with(GIT_BASH_VAR, "C:/msys64/usr/bin/bash.exe");
        assert_eq!(
            m.detect().expect("git bash override").program,
            PathBuf::from("C:/msys64/usr/bin/bash.exe")
        );

        let m =
            Machine::windows(&["C:/Windows/System32/pwsh.exe"]).with(GIT_BASH_VAR, "C:/nope.exe");
        assert_eq!(
            m.detect().expect_err("bad override"),
            NoShell::BadOverride {
                var: GIT_BASH_VAR,
                spec: "C:/nope.exe".to_owned()
            }
        );

        // On Unix the variable is not part of the ladder at all.
        let m = Machine::unix(&["/bin/bash"]).with(GIT_BASH_VAR, "C:/nope.exe");
        assert_eq!(
            m.detect().expect("unix").program,
            PathBuf::from("/bin/bash")
        );
    }

    // Unix is what it always was: bash on PATH, or the one failure Go had.
    #[test]
    fn unix_is_bash_or_nothing() {
        let m = Machine::unix(&["/bin/bash"]);
        let found = m.detect().expect("bash");
        assert_eq!(found.program, PathBuf::from("/bin/bash"));
        assert!(found.is_posix());
        assert_eq!(
            Machine::unix(&[]).detect().expect_err("no bash"),
            NoShell::NoBash
        );
        assert_eq!(
            NoShell::NoBash.to_string(),
            "bash is not installed on this system"
        );
    }

    // The dialect is read off the file name, so it is right for an interpreter nobody detected.
    #[test]
    fn the_family_comes_from_the_file_name() {
        for (program, family) in [
            ("/bin/bash", Family::Posix),
            ("C:/Program Files/Git/bin/bash.exe", Family::Posix),
            ("/usr/bin/sh", Family::Posix),
            (
                "C:/Windows/System32/WindowsPowerShell/v1.0/POWERSHELL.EXE",
                Family::PowerShell,
            ),
            ("C:/tools/pwsh.exe", Family::PowerShell),
            ("C:/Windows/System32/cmd.exe", Family::Cmd),
        ] {
            assert_eq!(Interpreter::new(program).family, family, "{program}");
        }

        // The per-dialect spellings the tool renders.
        let bash = Interpreter::new("/bin/bash");
        assert_eq!(bash.chain_cd("/p", "ls"), "cd /p && ls");
        assert_eq!(
            bash.tail_command(std::path::Path::new("/t/b1.log")),
            "tail -n 50 /t/b1.log"
        );
        let pwsh = Interpreter::new("C:/tools/pwsh.exe");
        assert_eq!(pwsh.chain_cd("C:/p", "ls"), "cd C:/p; ls");
        assert!(
            pwsh.tail_command(std::path::Path::new("C:/t/b1.log"))
                .starts_with("Get-Content -Tail 50 ")
        );
        let cmd = Interpreter::new("C:/Windows/System32/cmd.exe");
        assert_eq!(cmd.chain_cd("C:/p", "dir"), "cd /d C:/p && dir");
        assert_eq!(
            cmd.tail_command(std::path::Path::new("C:/t/b1.log")),
            "type C:/t/b1.log"
        );
    }
}
