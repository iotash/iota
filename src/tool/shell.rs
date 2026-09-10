//! The `shell` toolset (tool/shell.go): one `bash` tool running `bash -c` per call, sandboxed with Seatbelt on
//! macOS / bwrap on Linux when available.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::BoxFuture;
use crate::app::HostDirs;
use crate::chat::turns::RunCtx;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_duration;
use crate::tool::{Env, Tool, ToolOutput, ToolResult};
use serde::Deserialize;
use serde_json::Value;

use crate::tool::args::{int_arg, str_arg};
use crate::tool::sets::{RawNode, SetError};
use crate::tool::yaml11;

use crate::shell::exec;
use crate::shell::exec::{Options, RunResult, Sandbox};

/// Wall-clock cap of one `bash` call when it names no `timeout` (`[command timed out after 10m0s]`).
pub(crate) const DEFAULT_BASH_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds of the `timeout` argument, in seconds. One number cannot serve both a lint and a child agent's
/// whole run, so the model picks — inside a ceiling it cannot argue with (DIVERGENCES X-06).
const TIMEOUT_RANGE: std::ops::RangeInclusive<i64> = 1..=3600;

/// The refusal a `timeout` outside [`TIMEOUT_RANGE`] gets; the command does not run.
const TIMEOUT_ERR: &str = "timeout must be between 1 and 3600 seconds";

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

/// The `bash` tool.
pub(crate) struct BashTool {
    shell_cfg: ShellConfig,
    root: PathBuf,
    /// The process working directory — the display anchor for the optional `cwd`
    /// argument in call headers (tool/shell.go:74-78), NOT the execution dir (that
    /// defaults to `root`).
    cwd: PathBuf,
    sandboxed: bool,
    dirs: HostDirs,
}

/// Decode → `ShellConfig(err)`; sandbox "" → "auto"; not auto|off → `BadSandbox`; `sandboxed = sandbox == "auto"
/// && exec::available()` evaluated ONCE.
pub fn new_shell_set(env: &Env, node: Option<&RawNode>) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    let mut shell_cfg: ShellConfig = yaml11::decode_mapping(node).map_err(SetError::ShellConfig)?;
    match shell_cfg.sandbox.as_str() {
        "" => "auto".clone_into(&mut shell_cfg.sandbox),
        "auto" | "off" => {}
        other => return Err(SetError::BadSandbox(other.to_owned())),
    }
    // A sandbox binary appearing or disappearing later has no effect on this run (tool/shell.go:67).
    let sandboxed = shell_cfg.sandbox == "auto" && exec::available();
    Ok(vec![Arc::new(BashTool {
        shell_cfg,
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

impl Tool for BashTool {
    /// Name `bash`, description = `BASH_DESC_PREFIX` + one suffix, schema per tool/shell.go:129-144.
    fn def(&self) -> ToolDef {
        let mut description = String::from(BASH_DESC_PREFIX);
        if self.sandboxed {
            let net = if self.shell_cfg.network {
                "network access is allowed"
            } else {
                "network access is BLOCKED"
            };
            description.push_str(&BASH_DESC_SANDBOXED.replace("{net}", net));
        } else {
            description.push_str(BASH_DESC_UNSANDBOXED);
        }
        ToolDef {
            name: "bash".to_owned(),
            description,
            input_schema: Some(bash_schema()),
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
            let res = exec::run(
                &cx.cancel,
                Options {
                    command,
                    dir,
                    timeout: Some(timeout),
                    sandbox,
                },
            )
            .await;
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

    /// tool/shell.go:82-92 (the D-12 lift): the call IS the command — `"[bash git
    /// status]"`. The argument name is noise (a bash call has one thing to say), and an
    /// explicit cwd folds into the shell idiom for it (`"cd <path> && <cmd>"`) rather
    /// than eating a separate slot.
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        let cmd = crate::tool::fmt::header_command(str_arg(args, "command"));
        let dir = str_arg(args, "cwd").trim();
        if dir.is_empty() {
            return Some(cmd);
        }
        Some(format!(
            "cd {} && {cmd}",
            crate::tool::fmt::header_path(dir, &self.cwd, &self.root)
        ))
    }
}

/// The `timeout` argument as a duration: absent (or null) is [`DEFAULT_BASH_TIMEOUT`]; `None` means the
/// call named one outside [`TIMEOUT_RANGE`] (a non-number reads as `0`, which is out of range too).
fn timeout_arg(args: &JsonObject) -> Option<Duration> {
    match args.get("timeout") {
        None | Some(Value::Null) => Some(DEFAULT_BASH_TIMEOUT),
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

/// tool/shell.go:130-143, verbatim.
fn bash_schema() -> JsonObject {
    match serde_json::json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "Bash command line to execute, e.g. 'go test ./... 2>&1 | tail -20'.",
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
        },
        "required": ["command"],
    }) {
        serde_json::Value::Object(m) => m,
        _ => JsonObject::new(),
    }
}

/// Fixed head of the `bash` description.
pub const BASH_DESC_PREFIX: &str = "Run a bash command line on the user's machine and return its combined stdout/stderr. The full shell is available: pipes, redirects, globbing, && chaining, heredocs. The working directory defaults to the project root (override with \"cwd\"). Each call runs in a FRESH shell: environment variables, shell functions, aliases and `cd` do not carry over to the next call. Anything a later command depends on must be repeated in it — write the full path or command instead of defining a helper first. Calls issued together run concurrently. Each call is killed after 600 seconds unless \"timeout\" says otherwise (maximum 3600). ";
/// Sandboxed suffix; `{net}` = `network access is BLOCKED` | `network access is allowed`.
pub const BASH_DESC_SANDBOXED: &str = "Commands run inside an OS sandbox: file writes are confined to the project root and temp/cache directories (writes elsewhere fail with permission errors), and {net}.";
/// Unsandboxed suffix.
pub const BASH_DESC_UNSANDBOXED: &str = "Commands run WITHOUT a sandbox on this system, with the user's full permissions — be conservative.";

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        DEFAULT_BASH_TIMEOUT, Duration, JsonObject, RunResult, exec::ShellError, expand_home,
        format_result, go_duration, timeout_arg,
    };

    // New (tool-shell.md "MODEL-FACING RESULT SUFFIXES"): every branch of tool/shell.go:173-190.
    #[test]
    fn result_formatting_table() {
        let failed = RunResult {
            err: Some(ShellError::NoBash),
            ..RunResult::default()
        };
        let out = format_result(&failed, DEFAULT_BASH_TIMEOUT);
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
            format_result(&failed_with_output, DEFAULT_BASH_TIMEOUT).text,
            "partial\n\n[failed to run: boom]"
        );

        let timed_out = RunResult {
            output: "slow".to_owned(),
            timed_out: true,
            ..RunResult::default()
        };
        assert_eq!(
            format_result(&timed_out, DEFAULT_BASH_TIMEOUT).text,
            "slow\n[command timed out after 10m0s]"
        );
        assert_eq!(go_duration(DEFAULT_BASH_TIMEOUT), "10m0s");
        // The line names the cap the call actually ran under, not the default.
        assert_eq!(
            format_result(&timed_out, Duration::from_secs(5)).text,
            "slow\n[command timed out after 5s]"
        );

        let cancelled = RunResult {
            cancelled: true,
            ..RunResult::default()
        };
        let out = format_result(&cancelled, DEFAULT_BASH_TIMEOUT);
        assert_eq!(out.text, "\n[command cancelled]");
        assert!(out.is_error);

        let signalled = RunResult {
            exited: true,
            exit_code: -1,
            ..RunResult::default()
        };
        assert_eq!(
            format_result(&signalled, DEFAULT_BASH_TIMEOUT).text,
            "\n[exit code -1]"
        );

        let blank = RunResult {
            output: "  \n".to_owned(),
            exited: true,
            ..RunResult::default()
        };
        let out = format_result(&blank, DEFAULT_BASH_TIMEOUT);
        assert_eq!(out.text, "[command produced no output]");
        assert!(!out.is_error);

        let ok = RunResult {
            output: "hello\n".to_owned(),
            exited: true,
            ..RunResult::default()
        };
        let out = format_result(&ok, DEFAULT_BASH_TIMEOUT);
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
            Some(DEFAULT_BASH_TIMEOUT),
            "an absent timeout is the default"
        );
        assert_eq!(
            timeout_arg(&args(serde_json::json!({"timeout": null}))),
            Some(DEFAULT_BASH_TIMEOUT)
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
