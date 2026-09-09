//! Fake MCP dispatcher fixture (replaces Go's `fakeMCP` + `staticPrefix`, tool/defer_test.go:14-33).

use std::sync::{Arc, Mutex};

use iota::BoxFuture;
use iota::chat::turns::RunCtx;
use iota::provider::model::{JsonObject, ToolDef};
use iota::tool::{Dispatcher, PrefixOf, Presentation, ToolOutput, ToolResult};

/// A stand-in for the MCP manager: a live tool list plus recorded calls, with approval/presentation capabilities
/// to verify pass-through. `call_tool` answers `"ok:<name>"`; `requires_approval` is true only for
/// `mcp__gh__danger`; `presentation` is always `Group`.
#[derive(Debug, Default)]
pub struct FakeMcp {
    /// The advertised definitions (mutable so a test can grow the set between rounds).
    pub tools: Mutex<Vec<ToolDef>>,
    /// Every tool name called, in order.
    pub called: Mutex<Vec<String>>,
}

impl FakeMcp {
    /// A fake advertising `tools`.
    pub fn new(tools: Vec<ToolDef>) -> Self {
        Self {
            tools: Mutex::new(tools),
            called: Mutex::new(Vec::new()),
        }
    }

    /// `(name, description)` pairs → a fake with `input_schema: None`.
    pub fn with_defs(defs: &[(&str, &str)]) -> Self {
        Self::new(
            defs.iter()
                .map(|(name, description)| ToolDef {
                    name: (*name).to_owned(),
                    description: (*description).to_owned(),
                    input_schema: None,
                    deferred: false,
                })
                .collect(),
        )
    }

    /// Snapshot of the recorded call names.
    pub fn calls(&self) -> Vec<String> {
        self.called.lock().expect("fake mcp call log").clone()
    }
}

impl Dispatcher for FakeMcp {
    fn tools(&self) -> Vec<ToolDef> {
        self.tools.lock().expect("fake mcp tools").clone()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            self.called
                .lock()
                .expect("fake mcp call log")
                .push(name.to_owned());
            Ok(ToolOutput::ok(format!("ok:{name}")))
        })
    }

    fn requires_approval(&self, name: &str) -> bool {
        name == "mcp__gh__danger"
    }

    fn presentation(&self, _name: &str) -> Presentation {
        Presentation::Group
    }
}

/// A prefix oracle answering `prefix` for EVERY group name (`static_prefix("")` = nothing connected yet).
pub fn static_prefix(prefix: &str) -> PrefixOf {
    let prefix = prefix.to_owned();
    Arc::new(move |_group: &str| prefix.clone())
}

/// Go's `staticPrefix(server)`: `prefix` for the named `group`, `""` for every other group.
pub fn prefix_for(group: &str, prefix: &str) -> PrefixOf {
    let group = group.to_owned();
    let prefix = prefix.to_owned();
    Arc::new(move |g: &str| {
        if g == group {
            prefix.clone()
        } else {
            String::new()
        }
    })
}
