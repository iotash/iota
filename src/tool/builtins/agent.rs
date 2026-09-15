//! The `skills` toolset: the `load_skill` tool that activates a skill from the catalog and
//! reads its bundled files. It was spelled `agent` before the three-layer config split.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::BoxFuture;
use crate::app::paths;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::{go_quote, split_lines, truncate_to_char_boundary};
use crate::tool::context::RunCtx;
use crate::tool::{Tool, ToolEnv, ToolOutput, ToolResult};
use serde_json::{Value, json};

use crate::agents::skills::{Skill, discover_skills, skill_body, skill_roots};
use crate::tool::args::{int_arg, read_file_limited, str_arg};
use crate::tool::sets::{RawNode, SetError};

/// Largest file `load_skill` reads (the rest is dropped with a marker).
pub(crate) const LOAD_SKILL_MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Output cap of `load_skill`.
pub(crate) const LOAD_SKILL_MAX_OUTPUT: usize = 65536;

/// The `load_skill` tool.
pub(crate) struct LoadSkill {
    root: PathBuf,
    home: Option<PathBuf>,
}

/// Ignores `node`; never fails.
pub fn new_skills_set(
    env: &ToolEnv,
    _node: Option<&RawNode>,
) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    Ok(vec![Arc::new(LoadSkill {
        // When neither the project root nor the working directory resolves, the root is the empty
        // string; discovery then finds nothing rather than failing the build.
        root: env.root().unwrap_or_default(),
        home: env.dirs.home.clone(),
    })])
}

/// The `load_skill` description.
pub const LOAD_SKILL_DESCRIPTION: &str = "Load a skill by name to activate it: returns the skill's instructions and its directory (the base for its bundled files and scripts). Available skills are listed in the system prompt. Pass the optional \"file\" to read a file the skill's instructions reference, as a path relative to the skill's directory. Long content is windowed by \"offset\"/\"limit\" lines.";

impl Tool for LoadSkill {
    fn def(&self) -> ToolDef {
        let schema = json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Skill name exactly as listed in the available skills catalog.",
                },
                "file": {
                    "type": "string",
                    "description": "Optional file to read instead of the skill's instructions, relative to the skill's directory (e.g. \"references/api.md\").",
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based line number to start reading from (default 1).",
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: all remaining lines).",
                },
            },
            "required": ["skill"],
        });
        ToolDef {
            name: "load_skill".to_owned(),
            description: LOAD_SKILL_DESCRIPTION.to_owned(),
            input_schema: match schema {
                Value::Object(m) => Some(m),
                _ => None,
            },
            deferred: false,
        }
    }

    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        // Every failure is model-facing: the tool never returns a hard error, and the whole subsystem is plain
        // synchronous std::fs work.
        Box::pin(async move { Ok(self.run(args)) })
    }
}

impl LoadSkill {
    fn run(&self, args: &JsonObject) -> ToolOutput {
        let name = str_arg(args, "skill").trim();
        if name.is_empty() {
            return ToolOutput::err("missing required argument: skill");
        }
        let sk = match self.resolve(name) {
            Ok(sk) => sk,
            Err(text) => return ToolOutput::err(text),
        };
        let file = str_arg(args, "file").trim();
        if file.is_empty() {
            Self::serve_instructions(&sk, args)
        } else {
            Self::serve_file(&sk, file, args)
        }
    }

    /// Re-discovers skills on every call (a few readdirs — cheap, and always consistent with the catalog the model
    /// just saw) and matches `name` exactly.
    fn resolve(&self, name: &str) -> Result<Skill, String> {
        let (skills, _warnings) = discover_skills(&skill_roots(&self.root, self.home.as_deref()));
        if let Some(sk) = skills.iter().find(|sk| sk.name == name) {
            return Ok(sk.clone());
        }
        if skills.is_empty() {
            return Err(format!(
                "unknown skill {}: no skills are installed",
                go_quote(name)
            ));
        }
        let mut names: Vec<&str> = skills.iter().map(|sk| sk.name.as_str()).collect();
        names.sort_unstable();
        Err(format!(
            "unknown skill {}; available skills: {}",
            go_quote(name),
            names.join(", ")
        ))
    }

    /// The SKILL.md body prefixed with the header naming the skill and its directory — the model needs the
    /// directory to run bundled scripts and to name files for `file` reads.
    fn serve_instructions(sk: &Skill, args: &JsonObject) -> ToolOutput {
        let data = match read_capped(&sk.path) {
            Ok(d) => d,
            Err(text) => return ToolOutput::err(text),
        };
        let body = match skill_body(&data) {
            Ok(b) => b,
            // Discovery validated the frontmatter; reaching this means the file changed since.
            Err(e) => return ToolOutput::err(format!("skill {}: {e}", go_quote(&sk.name))),
        };
        let what = format!("instructions of skill {}", go_quote(&sk.name));
        match window_lines(&body, args, &what) {
            Ok(out) => ToolOutput::ok(format!(
                "skill: {}\ndirectory: {}\n\n{out}",
                sk.name,
                sk.dir().display()
            )),
            Err(text) => ToolOutput::err(text),
        }
    }

    /// A file bundled inside the skill's directory, jailed to it: absolute paths and `..` escapes are rejected.
    /// (Symlinks pointing outside are accepted — still tighter than the machine-wide read this replaced.)
    fn serve_file(sk: &Skill, file: &str, args: &JsonObject) -> ToolOutput {
        let clean = paths::clean(Path::new(file));
        if clean.is_absolute() || clean == Path::new("..") || clean.starts_with("..") {
            return ToolOutput::err(format!(
                "file must be a relative path inside the skill's directory, got {}",
                go_quote(file)
            ));
        }
        let path = paths::clean(&sk.dir().join(&clean));
        let data = match read_capped(&path) {
            Ok(d) => d,
            Err(text) => return ToolOutput::err(text),
        };
        let content = String::from_utf8_lossy(&data);
        let what = format!("{} of skill {}", clean.display(), go_quote(&sk.name));
        match window_lines(&content, args, &what) {
            Ok(out) => ToolOutput::ok(out),
            Err(text) => ToolOutput::err(text),
        }
    }
}

/// Reads a regular file up to `LOAD_SKILL_MAX_BYTES`, returning a model-facing error string on failure. An
/// oversized file carries the marker as its last line, so it participates in line windowing.
fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    let (mut data, size) = read_file_limited(path, LOAD_SKILL_MAX_BYTES)?;
    if size > LOAD_SKILL_MAX_BYTES {
        let marker = format!(
            "\n[file is {size} bytes; only the first {} MB was read]",
            LOAD_SKILL_MAX_BYTES / (1024 * 1024)
        );
        data.extend_from_slice(marker.as_bytes());
    }
    Ok(data)
}

/// Applies the offset/limit line window and the output cap; `what` names the content in the continuation markers.
/// `Err` is a model-facing error.
fn window_lines(content: &str, args: &JsonObject, what: &str) -> Result<String, String> {
    let lines = split_lines(content);
    let total = lines.len();
    if total == 0 {
        return Ok("[content is empty]".to_owned());
    }
    let total_i = i64::try_from(total).unwrap_or(i64::MAX);

    let mut start = int_arg(args, "offset");
    if start < 1 {
        start = 1;
    }
    if start > total_i {
        return Err(format!(
            "offset {start} is past the end of the {what} ({total} lines)"
        ));
    }
    let mut end = total;
    let limit = int_arg(args, "limit");
    if limit > 0 {
        let last = start.saturating_sub(1).saturating_add(limit);
        if last < total_i {
            end = usize::try_from(last).unwrap_or(total);
        }
    }
    let first = usize::try_from(start.saturating_sub(1)).unwrap_or(0);

    let mut out = lines[first..end].join("\n");
    if out.len() > LOAD_SKILL_MAX_OUTPUT {
        out = truncate_to_char_boundary(&out, LOAD_SKILL_MAX_OUTPUT).to_owned();
        // The last surviving line is likely partial; a follow-up call resumes on it.
        let last = start.saturating_add(i64::try_from(out.matches('\n').count()).unwrap_or(0));
        let _ = write!(
            out,
            "\n[output truncated at {} KB — showing lines {start}-{last} of {total}; call load_skill again with offset={last} and a limit to continue]",
            LOAD_SKILL_MAX_OUTPUT / 1024
        );
    } else if start > 1 || end < total {
        let _ = write!(out, "\n[showing lines {start}-{end} of {total}]");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::provider::model::JsonObject;
    use serde_json::json;

    use super::{LOAD_SKILL_MAX_OUTPUT, window_lines};

    fn args(v: serde_json::Value) -> JsonObject {
        match v {
            serde_json::Value::Object(m) => m,
            _ => panic!("object literal expected"),
        }
    }

    // The window's edges — empty content, the default window, a limit past the end,
    // a negative offset, and the two markers.
    #[test]
    fn window_lines_markers() {
        let none = JsonObject::new();
        assert_eq!(
            window_lines("", &none, "x").expect("empty"),
            "[content is empty]"
        );
        assert_eq!(window_lines("a\nb\n", &none, "x").expect("all"), "a\nb");
        // A negative offset clamps to 1; a limit beyond the end keeps the whole window (no marker).
        assert_eq!(
            window_lines("a\nb\n", &args(json!({"offset": -3, "limit": 9})), "x").expect("clamp"),
            "a\nb"
        );
        assert_eq!(
            window_lines("a\nb\nc", &args(json!({"limit": 2})), "x").expect("limit"),
            "a\nb\n[showing lines 1-2 of 3]"
        );
        assert_eq!(
            window_lines("a\nb\nc", &args(json!({"offset": 3})), "x").expect("offset"),
            "c\n[showing lines 3-3 of 3]"
        );
        assert_eq!(
            window_lines("a\nb", &args(json!({"offset": 9})), "the file").expect_err("past end"),
            "offset 9 is past the end of the the file (2 lines)"
        );

        // Over the 64 KB cap: the output is cut at a char boundary and carries the continuation marker.
        let long = "x".repeat(LOAD_SKILL_MAX_OUTPUT + 100);
        let out = window_lines(&long, &none, "x").expect("cap");
        assert!(out.starts_with(&"x".repeat(LOAD_SKILL_MAX_OUTPUT)));
        assert!(
            out.ends_with(
                "\n[output truncated at 64 KB — showing lines 1-1 of 1; call load_skill again with offset=1 and a limit to continue]"
            ),
            "{out:?}"
        );
    }
}
