//! The `delegate` toolset (tool/delegate.go): one `delegate` tool that hands a self-contained task to a configured
//! child agent through the host's `Delegator`.

use std::{fmt::Write as _, sync::Arc};

use crate::BoxFuture;
use crate::chat::turns::RunCtx;
use crate::provider::Effort;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::{elapsed, go_quote, tokens};
use crate::tool::{
    Artifact, ArtifactKind, DelegateSpec, Delegator, Env, Tool, ToolOutput, ToolResult,
    post_artifact,
};
use serde_json::{Value, json};

use crate::tool::args::str_arg;
use crate::tool::sets::{RawNode, SetError};

/// The `delegate` tool.
pub(crate) struct DelegateTool {
    d: Arc<dyn Delegator>,
    names: Vec<String>,
}

/// `env.delegate` None → `Ok(vec![])`; names empty → `Err(NoAgents)`; else one `DelegateTool`.
pub fn new_delegate_set(
    env: &Env,
    _node: Option<&RawNode>,
) -> Result<Vec<Arc<dyn Tool>>, SetError> {
    // Same contract as the ask set: without the host seam the set contributes no tools, so the model never sees
    // what it cannot use.
    let Some(d) = env.delegate.clone() else {
        return Ok(Vec::new());
    };
    let names = d.agent_names().to_vec();
    if names.is_empty() {
        return Err(SetError::NoAgents);
    }
    Ok(vec![Arc::new(DelegateTool { d, names })])
}

/// The fixed head of the `delegate` description; the per-agent lines follow.
pub const DELEGATE_DESC_PREAMBLE: &str = "Delegate a task to a child agent and get back its answer.\n\nThe child starts with NO knowledge of this conversation and reports back only its final answer, so `task` must be self-contained — state the goal, the context it needs, and what shape the answer should take. Prefer delegating work whose intermediate steps you do not need to see (surveying a codebase, checking a hypothesis across many files); doing it yourself is better when you need to watch it happen.\n\nAvailable agents:\n";

/// `strings.TrimSpace(args["agent"] as string)` — the ONE reader shared by `supports_parallel` and `call`.
pub fn agent_arg(args: &JsonObject) -> &str {
    str_arg(args, "agent").trim()
}

impl Tool for DelegateTool {
    /// Name `delegate`; description = `DELEGATE_DESC_PREAMBLE` + per agent
    /// `- {name} ({read-only|can modify files}): {description|(no description configured)}\n`, trailing `\n`
    /// trimmed; schema per tool/delegate.go:122-144.
    fn def(&self) -> ToolDef {
        let mut b = DELEGATE_DESC_PREAMBLE.to_owned();
        for name in &self.names {
            let info = self.d.agent(name).cloned().unwrap_or_default();
            let desc = if info.description.is_empty() {
                "(no description configured)"
            } else {
                info.description.as_str()
            };
            let access = if info.read_only {
                "read-only"
            } else {
                "can modify files"
            };
            let _ = writeln!(b, "- {name} ({access}): {desc}");
        }
        let schema = json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "enum": self.names,
                    "description": "Which configured agent runs the task.",
                },
                "task": {
                    "type": "string",
                    "description": "The complete brief. The child sees this and nothing else — no history, no files you have already read.",
                },
                "effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh", "max"],
                    "description": "Optional reasoning effort override for this one task.",
                },
            },
            "required": ["agent", "task"],
        });
        ToolDef {
            name: "delegate".to_owned(),
            description: b.trim_end_matches('\n').to_owned(),
            input_schema: match schema {
                Value::Object(m) => Some(m),
                _ => None,
            },
            deferred: false,
        }
    }

    /// tool/delegate.go:147-192 — every failure is `is_error = true`, never `Err`.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let agent = agent_arg(args);
            if agent.is_empty() {
                return Ok(ToolOutput::err("missing required argument: agent"));
            }
            if self.d.agent(agent).is_none() {
                return Ok(ToolOutput::err(format!(
                    "unknown agent {} — configured agents: {}",
                    go_quote(agent),
                    self.names.join(", ")
                )));
            }
            let task = str_arg(args, "task").trim();
            if task.is_empty() {
                return Ok(ToolOutput::err("missing required argument: task"));
            }
            let effort_raw = str_arg(args, "effort").trim();
            let Ok(effort) = Effort::optional(effort_raw) else {
                return Ok(ToolOutput::err(format!(
                    "invalid effort {}: want low|medium|high|xhigh|max",
                    go_quote(effort_raw)
                )));
            };

            let spec = DelegateSpec {
                agent: agent.to_owned(),
                task: task.to_owned(),
                effort,
            };
            let outcome = self.d.run(cx, spec).await;
            // What the child cost goes to the USER through the artifact channel, not into
            // the result: reporting a delegation's price by spending tokens on the number
            // would be its own small joke (tool/delegate.go:162-180; the D-19 lift,
            // T-35). Headless runs inject no slot, so the post stays a no-op. A FAILED
            // child is billed for the rounds it completed, and it is the run worth
            // investigating — reporting the cost only on success would hide it exactly
            // where it is surprising.
            if outcome.result.rounds > 0 {
                let res = &outcome.result;
                let plural = if res.rounds == 1 { "" } else { "s" };
                post_artifact(
                    cx,
                    Artifact {
                        kind: ArtifactKind::Note,
                        title: String::new(),
                        lines: vec![
                            format!("{} round{plural}", res.rounds),
                            format!("{} tokens", tokens(res.usage.context_tokens())),
                            elapsed(res.duration),
                        ],
                    },
                );
            }
            if let Some(err) = outcome.error {
                // The child's failure is the parent's result, not the parent's crash: a returned error would
                // abort the whole round, while a tool error lets the model try something else.
                return Ok(ToolOutput::err(format!(
                    "delegation to {} failed: {err}",
                    go_quote(agent)
                )));
            }
            let res = outcome.result;
            if res.reply.trim().is_empty() {
                return Ok(ToolOutput::err(format!(
                    "agent {} finished without an answer after {} round(s)",
                    go_quote(agent),
                    res.rounds
                )));
            }
            Ok(ToolOutput::ok(res.reply))
        })
    }

    /// `args.and_then(agent(agent_arg)).map_or(false, |a| a.read_only)`.
    fn supports_parallel(&self, args: Option<&JsonObject>) -> bool {
        args.and_then(|a| self.d.agent(agent_arg(a)))
            .is_some_and(|info| info.read_only)
    }
}
