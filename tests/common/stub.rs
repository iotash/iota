//! Stub tool fixture (replaces Go's `perCallTool`, tool/parallel_test.go:79-88).

use std::sync::Arc;

use iota::BoxFuture;
use iota::chat::turns::RunCtx;
use iota::provider::model::{JsonObject, ToolDef};
use iota::tool::{Tool, ToolOutput, ToolResult};

/// A tool whose parallel answer is a property of the CALL: `supports_parallel(args)` is
/// true only when `args["agent"]` is one of `parallel` (or `parallel` contains `"*"`, which also answers `None`
/// args); `requires_approval()` is `approval`. `call` echoes `"<name>:<args json>"` and never fails.
pub fn stub_tool(name: &str, parallel: &[&str], approval: bool) -> Arc<dyn Tool> {
    Arc::new(StubTool {
        name: name.to_owned(),
        parallel: parallel.iter().map(|s| (*s).to_owned()).collect(),
        approval,
    })
}

struct StubTool {
    name: String,
    parallel: Vec<String>,
    approval: bool,
}

impl Tool for StubTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: self.name.clone(),
            description: format!("stub tool {}", self.name),
            input_schema: None,
            deferred: false,
        }
    }

    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args = serde_json::to_string(args).unwrap_or_default();
            Ok(ToolOutput::ok(format!("{}:{}", self.name, args)))
        })
    }

    fn requires_approval(&self) -> bool {
        self.approval
    }

    fn supports_parallel(&self, args: Option<&JsonObject>) -> bool {
        if self.parallel.iter().any(|p| p == "*") {
            return true;
        }
        args.and_then(|a| a.get("agent"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|agent| self.parallel.iter().any(|p| p == agent))
    }
}
