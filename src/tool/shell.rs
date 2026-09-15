//! The `shell` toolset (tool/shell.go): one `shell` tool running one command line per call, sandboxed with
//! Seatbelt on macOS / bwrap on Linux when available.
//!
//! WHAT runs the command line is `crate::shell::interp`'s answer, and on Windows it is not always a POSIX
//! shell. The tool's DESCRIPTION moves with it, completely: the first sentence names the interpreter that
//! will actually run (`Run a PowerShell command line…`), and the dialect it then teaches is that
//! interpreter's own. The model is never told bash while `powershell.exe` waits for the script.
//!
//! The NAME does not move: [`SHELL_TOOL_NAME`] is `shell` on every platform and under every interpreter, as
//! is the config key. A 378-call experiment across seven models settled it (DIVERGENCES X-20): a name and a
//! description that disagree are resolved by the models ASYMMETRICALLY — `powershell` in either slot wins,
//! `bash` is the unmarked default and loses — while a generic name plus a description that names the
//! interpreter matched the best case in both directions, 98% and 100%. Hence the hard requirement this
//! module carries: the first sentence of every prefix below names the interpreter. That is what makes the
//! generic name safe, and the experiment never tested it without.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::BoxFuture;
use crate::app::HostDirs;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_duration;
use crate::tool::context::RunCtx;
use crate::tool::{Env, Tool, ToolOutput, ToolResult};
use serde::Deserialize;
use serde_json::Value;

use crate::tool::args::{bool_arg, int_arg, str_arg};
use crate::tool::sets::{RawNode, SetError};
use crate::tool::yaml11;

use crate::shell::exec;
use crate::shell::exec::{Options, RunResult, Sandbox};
use crate::shell::interp::{Family, Interpreter};
use crate::shell::jobs::{JobStart, Jobs};

/// The tool's name, on every platform and under every interpreter (the config key is `shell` too).
pub const SHELL_TOOL_NAME: &str = "shell";

/// Wall-clock cap of one call when it names no `timeout` (`[command timed out after 10m0s]`).
pub(crate) const DEFAULT_SHELL_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds of the `timeout` argument, in seconds. One number cannot serve both a lint and a child agent's
/// whole run, so the model picks — inside a ceiling it cannot argue with (DIVERGENCES X-06).
const TIMEOUT_RANGE: std::ops::RangeInclusive<i64> = 1..=3600;

/// The refusal a `timeout` outside [`TIMEOUT_RANGE`] gets; the command does not run.
const TIMEOUT_ERR: &str = "timeout must be between 1 and 3600 seconds";

/// The refusal a `background` call gets when the run has no job registry (tests only — both entry points
/// bind one). Running it in the FOREGROUND instead would hold the turn for as long as the model asked to be
/// free of it, which is the opposite of what it requested.
const NO_JOBS_ERR: &str = "background jobs are not available in this run";

/// `tools.shell` configuration.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct ShellConfig {
    /// `"auto"` (sandbox when available) or `"off"`.
    pub(crate) sandbox: String,
    /// Whether sandboxed commands may use the network.
    #[serde(deserialize_with = "yaml11::deserialize_bool")]
    pub(crate) network: bool,
    /// Whether calls run without approval even unsandboxed.
    #[serde(deserialize_with = "yaml11::deserialize_bool")]
    pub(crate) auto_run: bool,
    /// Extra writable roots inside the sandbox (`~` expands to home).
    pub(crate) write: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            sandbox: "auto".to_owned(),
            network: false,
            auto_run: false,
            write: Vec::new(),
        }
    }
}

/// The one tool of the `shell` set.
pub(crate) struct ShellTool {
    shell_cfg: ShellConfig,
    /// The interpreter its calls run under, resolved ONCE at assembly so the description cannot describe a
    /// different shell from the one the first call finds.
    shell: Interpreter,
    /// The run's background-job registry (`Env.jobs`); None in a test env.
    jobs: Option<Arc<Jobs>>,
    root: PathBuf,
    /// The process working directory — the display anchor for the optional `cwd`
    /// argument in call headers (tool/shell.go:74-78), NOT the execution dir (that
    /// defaults to `root`).
    cwd: PathBuf,
    sandboxed: bool,
    dirs: HostDirs,
}

/// Decode → `ShellConfig(err)`; sandbox "" → "auto"; not auto|off → `BadSandbox`; `sandboxed = sandbox == "auto"
/// && exec::available()` evaluated ONCE — and so is the interpreter (`crate::shell::interp::resolve`).
///
/// A machine with no interpreter at all is the one case where the set refuses: on Windows, where that means
/// neither Git Bash nor PowerShell nor `cmd.exe` is reachable, a registered tool would spend the model's
/// turns discovering call by call what one warning says once (the registry turns this `Err` into exactly that
/// warning). Unix keeps its own answer verbatim: the tool is registered whatever `PATH` holds, and a missing
/// `bash` is the per-call `bash is not installed on this system` it always was.
pub fn new_shell_set(env: &Env, node: Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    let mut shell_cfg: ShellConfig = yaml11::decode_mapping(node).map_err(SetError::ShellConfig)?;
    match shell_cfg.sandbox.as_str() {
        "" => "auto".clone_into(&mut shell_cfg.sandbox),
        "auto" | "off" => {}
        other => return Err(SetError::BadSandbox(other.to_owned())),
    }
    let shell = match crate::shell::interp::resolve() {
        Ok(shell) => shell,
        Err(e) if cfg!(windows) => return Err(SetError::NoShell(e.to_string())),
        Err(_) => Interpreter::new("bash"),
    };
    // A sandbox binary appearing or disappearing later has no effect on this run (tool/shell.go:67).
    let sandboxed = shell_cfg.sandbox == "auto" && exec::available();
    Ok(vec![Arc::new(ShellTool {
        shell_cfg,
        shell,
        jobs: env.jobs.clone(),
        root: env.root().unwrap_or_default(),
        cwd: env
            .dirs
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default(),
        sandboxed,
        dirs: env.dirs.clone(),
    })])
}

impl Tool for ShellTool {
    /// Name [`SHELL_TOOL_NAME`] on every platform, description the running interpreter's ([`desc_prefix`])
    /// plus one sandbox suffix and the background paragraph; schema per tool/shell.go:129-144.
    fn def(&self) -> ToolDef {
        let mut description = String::from(desc_prefix(self.shell.family));
        if self.sandboxed {
            let net = if self.shell_cfg.network {
                "network access is allowed"
            } else {
                "network access is BLOCKED"
            };
            description.push_str(&SHELL_DESC_SANDBOXED.replace("{net}", net));
        } else {
            description.push_str(SHELL_DESC_UNSANDBOXED);
        }
        description.push_str(&background_desc(self.shell.family));
        ToolDef {
            name: SHELL_TOOL_NAME.to_owned(),
            description,
            input_schema: Some(shell_schema(self.shell.family)),
            deferred: false,
        }
    }

    /// tool/shell.go:147-190.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let command = str_arg(args, "command").trim().to_owned();
            if command.is_empty() {
                return Ok(ToolOutput::err("missing required argument: command"));
            }
            // No jail and no existence check: an absolute cwd outside the root is accepted, a bad one
            // surfaces as a spawn error.
            let cwd = str_arg(args, "cwd").trim();
            let dir = if cwd.is_empty() {
                self.root.clone()
            } else {
                let p = Path::new(cwd);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    crate::paths::clean(&self.root.join(p))
                }
            };
            let Some(timeout) = timeout_arg(args) else {
                return Ok(ToolOutput::err(TIMEOUT_ERR));
            };
            let sandbox = self.sandboxed.then(|| Sandbox {
                root: self.root.clone(),
                network: self.shell_cfg.network,
                write: self
                    .shell_cfg
                    .write
                    .iter()
                    .map(|p| expand_home(p.trim(), self.dirs.home.as_deref()))
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect(),
                temp_dir: self.dirs.temp.clone(),
                cache_dir: self.dirs.cache.clone(),
            });
            let opts = Options {
                command,
                dir,
                timeout: Some(timeout),
                sandbox,
            };
            if bool_arg(args, "background", false) {
                return Ok(self.start_background(&opts));
            }
            let res = exec::run(&cx.cancel, opts).await;
            Ok(format_result(&res, timeout))
        })
    }

    /// `!sandboxed && !auto_run`.
    fn requires_approval(&self) -> bool {
        !self.sandboxed && !self.shell_cfg.auto_run
    }

    /// Always — a deliberate break with Go's "only read-only tools batch" law (DIVERGENCES X-05).
    /// A round's consecutive `bash` calls run in ONE batch, results still in call order. The
    /// judgement Go's rule made for the model (is this command safe beside that one?) is the
    /// model's own here: it wrote both command lines, and `&`/`wait`/`xargs -P` inside a single
    /// call were never gated either.
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        true
    }

    /// tool/shell.go:82-92 (the D-12 lift): the call IS the command — `"[shell git
    /// status]"`. The argument name is noise (a shell call has one thing to say), and an
    /// explicit cwd folds into the running interpreter's idiom for it (`"cd <path> && <cmd>"`,
    /// `"cd <path>; <cmd>"` under PowerShell) rather than eating a separate slot. A background
    /// call is marked, because the row settles while the work is still going.
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        let cmd = crate::tool::fmt::header_command(str_arg(args, "command"));
        let dir = str_arg(args, "cwd").trim();
        let mut summary = if dir.is_empty() {
            cmd
        } else {
            self.shell.chain_cd(
                &crate::tool::fmt::header_path(dir, &self.cwd, &self.root),
                &cmd,
            )
        };
        if bool_arg(args, "background", false) {
            summary.insert_str(0, "(background) ");
        }
        Some(summary)
    }
}

impl ShellTool {
    /// Hands the command to the job registry and answers with the receipt the model needs to follow it: the
    /// id the notice will carry, the pid, and the file it can `tail` meanwhile.
    fn start_background(&self, opts: &Options) -> ToolOutput {
        let Some(jobs) = self.jobs.as_ref() else {
            return ToolOutput::err(NO_JOBS_ERR);
        };
        match jobs.spawn(opts) {
            Err(e) => ToolOutput::err(e.to_string()),
            Ok(JobStart {
                id,
                pid,
                output_path,
            }) => {
                let tail = self.shell.tail_command(&output_path);
                let path = output_path.display();
                let pid = pid.map_or_else(|| "?".to_owned(), |p| p.to_string());
                ToolOutput::ok(format!(
                    "Started background job {id} (pid {pid}). Output: {path}\nA notice with its exit status and output arrives when it finishes; run `{tail}` to see progress meanwhile."
                ))
            }
        }
    }
}

/// The `timeout` argument as a duration: absent (or null) is [`DEFAULT_SHELL_TIMEOUT`]; `None` means the
/// call named one outside [`TIMEOUT_RANGE`] (a non-number reads as `0`, which is out of range too).
fn timeout_arg(args: &JsonObject) -> Option<Duration> {
    match args.get("timeout") {
        None | Some(Value::Null) => Some(DEFAULT_SHELL_TIMEOUT),
        Some(_) => {
            let secs = int_arg(args, "timeout");
            if !TIMEOUT_RANGE.contains(&secs) {
                return None;
            }
            u64::try_from(secs).ok().map(Duration::from_secs)
        }
    }
}

/// tool/shell.go:173-190: the model-facing rendering of one run, checked in this order. `timeout` is the
/// cap the call actually ran under, so the timed-out line names the number the model chose.
fn format_result(res: &RunResult, timeout: Duration) -> ToolOutput {
    if let Some(e) = &res.err {
        if res.output.trim().is_empty() {
            return ToolOutput::err(format!("failed to run: {e}"));
        }
        return ToolOutput::err(format!("{}\n[failed to run: {e}]", res.output));
    }
    if res.timed_out {
        return ToolOutput::err(format!(
            "{}\n[command timed out after {}]",
            res.output,
            go_duration(timeout)
        ));
    }
    if res.cancelled {
        return ToolOutput::err(format!("{}\n[command cancelled]", res.output));
    }
    if res.exit_code != 0 {
        return ToolOutput::err(format!("{}\n[exit code {}]", res.output, res.exit_code));
    }
    if res.output.trim().is_empty() {
        return ToolOutput::ok("[command produced no output]");
    }
    ToolOutput::ok(res.output.clone())
}

/// tool/shell.go:194-206: resolves a leading `~` (alone or before `/`) to the home directory; anything else,
/// and a missing home, is left untouched.
fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    if path != "~" && !path.starts_with("~/") {
        return PathBuf::from(path);
    }
    let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    home.join(&path[2..])
}

/// tool/shell.go:130-143, verbatim but for the one field that names a dialect: the `command` example is
/// written in the shell that will actually read it.
fn shell_schema(family: Family) -> JsonObject {
    let command = match family {
        Family::Posix => "Bash command line to execute, e.g. 'go test ./... 2>&1 | tail -20'.",
        Family::PowerShell => {
            "PowerShell command line to execute, e.g. 'cargo test 2>&1 | Select-Object -Last 20'."
        }
        Family::Cmd => "cmd.exe command line to execute, e.g. 'cargo test 2>&1 | more'.",
    };
    match serde_json::json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": command,
            },
            "cwd": {
                "type": "string",
                "description": "Optional working directory (defaults to the project root).",
            },
            "timeout": {
                "type": "integer",
                "description": "Optional wall-clock cap in seconds (default 600, maximum 3600). The command is killed when it expires.",
                "minimum": 1,
                "maximum": 3600,
            },
            "background": {
                "type": "boolean",
                "description": "Run the command in the background and return immediately with its job id and output file (default false).",
            },
        },
        "required": ["command"],
    }) {
        serde_json::Value::Object(m) => m,
        _ => JsonObject::new(),
    }
}

/// Fixed head of the description when a POSIX shell runs the calls — Git Bash included, which is why it says
/// nothing about the platform.
pub const BASH_DESC_PREFIX: &str = "Run a bash command line on the user's machine and return its combined stdout/stderr. The full shell is available: pipes, redirects, globbing, && chaining, heredocs. The working directory defaults to the project root (override with \"cwd\"). Each call runs in a FRESH shell: environment variables, shell functions, aliases and `cd` do not carry over to the next call. Anything a later command depends on must be repeated in it — write the full path or command instead of defining a helper first. Calls issued together run concurrently. Each call is killed after 600 seconds unless \"timeout\" says otherwise (maximum 3600). ";
/// The same head for PowerShell. It spends its first half on the dialect because that is the half the model
/// gets wrong by default: everything it knows about shells is POSIX, and none of it applies here.
pub const PWSH_DESC_PREFIX: &str = "Run a PowerShell command line on the user's machine and return its combined stdout/stderr. This is PowerShell, NOT a POSIX shell, and bash habits do not carry over: separate statements with `;` (`&&` and `||` need PowerShell 7+), and remember the pipeline carries .NET objects rather than bytes — narrow output with `Select-Object -First 20`, `Select-String <pattern>` or `Where-Object`, and pipe through `Out-String` before anything that expects text. Redirection is `>`, `>>` and `2>&1`; the bit bucket is `$null`, not /dev/null. Variables are `$name` and interpolate inside double quotes only; a native program's exit code is `$LASTEXITCODE`; a multi-line literal is a here-string (`@\"…\"@`), not a heredoc. Native programs (git, cargo, node, python) run exactly as they do in any Windows console, but the POSIX tools (grep, sed, awk, ls, tail) are NOT here unless the user installed them — use the cmdlet. The working directory defaults to the project root (override with \"cwd\"). Each call runs in a FRESH shell: variables, functions, aliases and `Set-Location` do not carry over to the next call. Anything a later command depends on must be repeated in it — write the full path or command instead of defining a helper first. Calls issued together run concurrently. Each call is killed after 600 seconds unless \"timeout\" says otherwise (maximum 3600). ";
/// The same head for `cmd.exe` — the floor, reached only when the machine has neither Git Bash nor
/// PowerShell, so it says plainly how little is available.
pub const CMD_DESC_PREFIX: &str = "Run a cmd.exe command line on the user's machine and return its combined stdout/stderr. This is the Windows command interpreter — neither bash nor PowerShell, and the most limited of the three; it is what is left when this machine has no PowerShell. Chain with `&`, `&&` and `||`; redirect with `>`, `>>` and `2>&1`; the bit bucket is `NUL`, not /dev/null; variables are `%NAME%`; `^` escapes `& | < > ^`. There is no globbing (each program expands its own arguments) and none of the POSIX tools (grep, sed, awk, ls, tail) unless the user installed them, so prefer running programs (git, cargo, node) directly over cmd built-ins, and prefer one program's own flags over a pipeline. The working directory defaults to the project root (override with \"cwd\"). Each call runs in a FRESH shell: environment variables and `cd` do not carry over to the next call. Anything a later command depends on must be repeated in it — write the full path or command instead of defining a helper first. Calls issued together run concurrently. Each call is killed after 600 seconds unless \"timeout\" says otherwise (maximum 3600). ";
/// Sandboxed suffix; `{net}` = `network access is BLOCKED` | `network access is allowed`.
pub const SHELL_DESC_SANDBOXED: &str = "Commands run inside an OS sandbox: file writes are confined to the project root and temp/cache directories (writes elsewhere fail with permission errors), and {net}.";
/// Unsandboxed suffix — the only one Windows ever gets: no OS sandbox exists there, so every call runs with
/// the user's full permissions and needs approval unless `auto_run` waived it.
pub const SHELL_DESC_UNSANDBOXED: &str = "Commands run WITHOUT a sandbox on this system, with the user's full permissions — be conservative.";
/// The background-mode paragraph, appended after the sandbox suffix. `{tail}` and `{detach}` are the running
/// interpreter's spellings ([`background_desc`]) — the two places this paragraph would otherwise hand a
/// Windows model a POSIX command.
pub const SHELL_DESC_BACKGROUND: &str = "\n\nSet \"background\": true for work that outlasts a reply — a long build, a test suite, a child agent (`iota run <agent> -m \"<task>\"`). The call returns at once with a job id and an output file; when the job ends you are told its exit status and shown its output, so do not poll for it (`{tail}` the file only if you need progress meanwhile). Up to 16 background jobs at a time, and \"timeout\" still applies. Background jobs are killed when iota exits — a job that must survive that has to detach itself ({detach}).";

/// The description head of the interpreter that will run the calls.
pub fn desc_prefix(family: Family) -> &'static str {
    match family {
        Family::Posix => BASH_DESC_PREFIX,
        Family::PowerShell => PWSH_DESC_PREFIX,
        Family::Cmd => CMD_DESC_PREFIX,
    }
}

/// [`SHELL_DESC_BACKGROUND`] with its two dialect slots filled: how this interpreter reads the tail of a log,
/// and how a job detaches from it.
pub fn background_desc(family: Family) -> String {
    let (tail, detach) = match family {
        Family::Posix => ("tail", "nohup/setsid"),
        Family::PowerShell => ("Get-Content -Tail", "Start-Process"),
        Family::Cmd => ("type", "start /b"),
    };
    SHELL_DESC_BACKGROUND
        .replace("{tail}", tail)
        .replace("{detach}", detach)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        DEFAULT_SHELL_TIMEOUT, Duration, JsonObject, RunResult, exec::ShellError, expand_home,
        format_result, go_duration, timeout_arg,
    };

    // New (tool-shell.md "MODEL-FACING RESULT SUFFIXES"): every branch of tool/shell.go:173-190.
    #[test]
    fn result_formatting_table() {
        let failed = RunResult {
            err: Some(ShellError::NoShell(crate::shell::interp::NoShell::NoBash)),
            ..RunResult::default()
        };
        let out = format_result(&failed, DEFAULT_SHELL_TIMEOUT);
        assert_eq!(
            out.text,
            "failed to run: bash is not installed on this system"
        );
        assert!(out.is_error);

        let failed_with_output = RunResult {
            output: "partial\n".to_owned(),
            err: Some(ShellError::Spawn("boom".to_owned())),
            ..RunResult::default()
        };
        assert_eq!(
            format_result(&failed_with_output, DEFAULT_SHELL_TIMEOUT).text,
            "partial\n\n[failed to run: boom]"
        );

        let timed_out = RunResult {
            output: "slow".to_owned(),
            timed_out: true,
            ..RunResult::default()
        };
        assert_eq!(
            format_result(&timed_out, DEFAULT_SHELL_TIMEOUT).text,
            "slow\n[command timed out after 10m0s]"
        );
        assert_eq!(go_duration(DEFAULT_SHELL_TIMEOUT), "10m0s");
        // The line names the cap the call actually ran under, not the default.
        assert_eq!(
            format_result(&timed_out, Duration::from_secs(5)).text,
            "slow\n[command timed out after 5s]"
        );

        let cancelled = RunResult {
            cancelled: true,
            ..RunResult::default()
        };
        let out = format_result(&cancelled, DEFAULT_SHELL_TIMEOUT);
        assert_eq!(out.text, "\n[command cancelled]");
        assert!(out.is_error);

        let signalled = RunResult {
            exited: true,
            exit_code: -1,
            ..RunResult::default()
        };
        assert_eq!(
            format_result(&signalled, DEFAULT_SHELL_TIMEOUT).text,
            "\n[exit code -1]"
        );

        let blank = RunResult {
            output: "  \n".to_owned(),
            exited: true,
            ..RunResult::default()
        };
        let out = format_result(&blank, DEFAULT_SHELL_TIMEOUT);
        assert_eq!(out.text, "[command produced no output]");
        assert!(!out.is_error);

        let ok = RunResult {
            output: "hello\n".to_owned(),
            exited: true,
            ..RunResult::default()
        };
        let out = format_result(&ok, DEFAULT_SHELL_TIMEOUT);
        assert_eq!(out.text, "hello\n", "the trailing newline is preserved");
        assert!(!out.is_error);
    }

    // New (DIVERGENCES X-06): absent means the default, and only 1…3600 is a timeout at all.
    #[test]
    fn timeout_argument_bounds() {
        let args =
            |v: serde_json::Value| -> JsonObject { v.as_object().cloned().unwrap_or_default() };
        assert_eq!(
            timeout_arg(&JsonObject::new()),
            Some(DEFAULT_SHELL_TIMEOUT),
            "an absent timeout is the default"
        );
        assert_eq!(
            timeout_arg(&args(serde_json::json!({"timeout": null}))),
            Some(DEFAULT_SHELL_TIMEOUT)
        );
        for (secs, want) in [(1, 1), (30, 30), (3600, 3600)] {
            assert_eq!(
                timeout_arg(&args(serde_json::json!({"timeout": secs}))),
                Some(Duration::from_secs(want)),
                "timeout {secs} is inside the range"
            );
        }
        for bad in [
            serde_json::json!({"timeout": 0}),
            serde_json::json!({"timeout": -1}),
            serde_json::json!({"timeout": 3601}),
            // A non-number reads as 0, which is out of range too — never silently the default.
            serde_json::json!({"timeout": "30"}),
            serde_json::json!({"timeout": true}),
        ] {
            assert_eq!(
                timeout_arg(&args(bad.clone())),
                None,
                "{bad} must be refused"
            );
        }
    }

    // New: tool/shell.go:194-206 — `~` and `~/x` only, and only with a home.
    #[test]
    fn expand_home_rules() {
        let home = PathBuf::from("/home/u");
        assert_eq!(expand_home("~", Some(&home)), home);
        assert_eq!(
            expand_home("~/go/pkg", Some(&home)),
            PathBuf::from("/home/u/go/pkg")
        );
        assert_eq!(expand_home("~alice", Some(&home)), PathBuf::from("~alice"));
        assert_eq!(expand_home("/abs", Some(&home)), PathBuf::from("/abs"));
        assert_eq!(expand_home("~", None), PathBuf::from("~"));
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
        assert_eq!(expand_home("~", Some(Path::new(""))), PathBuf::from("~"));
        assert_eq!(expand_home("", Some(&home)), PathBuf::new());
    }
}
