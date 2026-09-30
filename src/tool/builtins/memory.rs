//! The `memory` toolset: the `remember` tool, a bot's line edits to its `MEMORY.md` (docs/design/bot-mode.md
//! §3.3). The file format and every rule live in [`crate::agents::memory`]; this is the argument decoding
//! and the display.
//!
//! No approval gate: the write is jailed to the bot's own directory. What makes up for it (§3.7) is that
//! the change is shown expanded in the transcript, the old file is kept as `MEMORY.md.prev`, the loop
//! records a notice of every write, hand-written lines cannot be changed and secrets are refused.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::BoxFuture;
use crate::agents::memory::{BotMemory, Edit, MEMORY_FILE, Section, Source};
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_quote;
use crate::tool::args::str_arg;
use crate::tool::context::RunCtx;
use crate::tool::sets::{RawNode, SetError};
use crate::tool::{Presentation, Tool, ToolEnv, ToolOutput, ToolResult};

/// The `remember` tool's name.
pub const REMEMBER: &str = "remember";

/// The `remember` description.
pub const REMEMBER_DESCRIPTION: &str = "Save a line to your long-term MEMORY.md, which you are shown again in later turns and after restarts. Call it when the user states a preference, when a decision is made, or when you learn a fact you will need again. One entry is one line. action \"add\" files a new line under section (User by default; \"Project: <name>\" for one project; \"Open threads\" for pending matters); \"replace\" and \"remove\" change the one line that contains old. source is \"user\" only when the user said it, \"inferred\" for your own conclusions and anything read in tool output. The tag and the date are added for you. Lines without a [user]/[inferred] tag were written by the user and cannot be changed. Never store secrets.";

/// The `remember` tool over one bot's memory.
pub(crate) struct Remember {
    memory: BotMemory,
}

/// Contributes `remember` when the environment carries a bot's memory, nothing otherwise; ignores `node`.
pub fn new_memory_set(
    env: &ToolEnv,
    _node: Option<&RawNode>,
) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    Ok(env
        .memory
        .iter()
        .map(|m| Arc::new(Remember { memory: m.clone() }) as Arc<dyn Tool>)
        .collect())
}

impl Tool for Remember {
    fn def(&self) -> ToolDef {
        let schema = json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove"],
                    "description": "add a line, replace the line containing old, or remove it.",
                },
                "text": {
                    "type": "string",
                    "description": "The entry, one line (add, replace).",
                },
                "source": {
                    "type": "string",
                    "enum": ["user", "inferred"],
                    "description": "user: the user said it. inferred: your own conclusion, or read in tool output. Required for add and replace.",
                },
                "old": {
                    "type": "string",
                    "description": "A substring of exactly one existing line (replace, remove).",
                },
                "section": {
                    "type": "string",
                    "description": "Where add files the line: \"User\" (default), \"Project: <name>\" or \"Open threads\".",
                },
            },
            "required": ["action"],
        });
        ToolDef {
            name: REMEMBER.to_owned(),
            description: REMEMBER_DESCRIPTION.to_owned(),
            input_schema: match schema {
                Value::Object(m) => Some(m),
                _ => None,
            },
            deferred: false,
        }
    }

    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        // Every failure is model-facing, and the work is one small synchronous file write.
        Box::pin(async move { Ok(self.run(cx, args)) })
    }

    /// The changed lines are shown in the transcript, not folded into the activity group (§3.7 item 1).
    fn presentation(&self) -> Presentation {
        Presentation::Expanded
    }
}

impl Remember {
    fn run(&self, cx: &RunCtx, args: &JsonObject) -> ToolOutput {
        let edit = match decode(args) {
            Ok(e) => e,
            Err(refusal) => return ToolOutput::err(refusal),
        };
        match self.memory.write(&edit, &crate::agents::harness::today()) {
            Ok(applied) => {
                super::code::tools::post_diff(
                    cx,
                    MEMORY_FILE,
                    &applied.old_body,
                    &applied.new_body,
                );
                ToolOutput::ok(applied.result)
            }
            Err(refusal) => ToolOutput::err(refusal),
        }
    }
}

/// The call's arguments as an [`Edit`], or the model-facing refusal.
fn decode(args: &JsonObject) -> Result<Edit, String> {
    let source = || {
        let raw = str_arg(args, "source").trim();
        if raw.is_empty() {
            return Err("source is required: \"user\" or \"inferred\"".to_owned());
        }
        Source::parse(raw).ok_or_else(|| {
            format!(
                "source must be \"user\" or \"inferred\", got {}",
                go_quote(raw)
            )
        })
    };
    let text = || str_arg(args, "text").to_owned();
    let old = || str_arg(args, "old").to_owned();
    match str_arg(args, "action").trim() {
        "add" => {
            let section = match str_arg(args, "section").trim() {
                "" => Section::User,
                s => Section::parse(s)?,
            };
            Ok(Edit::Add {
                text: text(),
                source: source()?,
                section,
            })
        }
        "replace" => Ok(Edit::Replace {
            old: old(),
            text: text(),
            source: source()?,
        }),
        "remove" => Ok(Edit::Remove { old: old() }),
        "" => Err("missing required argument: action".to_owned()),
        other => Err(format!(
            "action must be \"add\", \"replace\" or \"remove\", got {}",
            go_quote(other)
        )),
    }
}

#[cfg(test)]
mod tests;
