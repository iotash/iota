//! Live union of dispatcher parts (tool/tool.go:538-673): tools are re-queried on every call, names dedup with
//! the earlier part winning, and calls route to the owning part.

use std::{collections::HashSet, sync::Arc};

use crate::BoxFuture;
use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{DeferredToolStatus, Dispatcher, Owner, Presentation, ToolResult, ToolSearcher};

/// The merged dispatcher; parts are consulted in order.
pub(crate) struct Merged {
    parts: Vec<Arc<dyn Dispatcher>>,
}

/// Hosts skip absent parts before calling. Never empty-panics.
pub fn merge(parts: Vec<Arc<dyn Dispatcher>>) -> Arc<dyn Dispatcher> {
    Arc::new(Merged { parts })
}

impl Merged {
    /// The part that owns `name`: a part with an [`Owner`] answers for itself — the registry, the MCP
    /// manager, a defer wrapper and a nested merge all have one, so the product graph never walks a
    /// `tools()` clone here; a part without one (a test fake) is scanned through `tools()`.
    fn owner(&self, name: &str) -> Option<&Arc<dyn Dispatcher>> {
        self.parts.iter().find(|p| {
            p.as_owner().map_or_else(
                || p.tools().iter().any(|d| d.name == name),
                |o| o.owns(name),
            )
        })
    }
}

impl Owner for Merged {
    /// Whether any part owns `name` — the parts' own oracles, first match.
    fn owns(&self, name: &str) -> bool {
        self.owner(name).is_some()
    }
}

impl Dispatcher for Merged {
    /// The merged oracle — only when EVERY part has one, so that an answer never falls back to a
    /// `tools()` scan (a merge with a scanning fake in it vouches for nothing, like before).
    fn as_owner(&self) -> Option<&dyn Owner> {
        self.parts
            .iter()
            .all(|p| p.as_owner().is_some())
            .then_some(self as &dyn Owner)
    }

    /// Re-queries every part; dedup by name, earlier part wins; part order then inner order.
    fn tools(&self) -> Vec<ToolDef> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for p in &self.parts {
            for def in p.tools() {
                if seen.insert(def.name.clone()) {
                    out.push(def);
                }
            }
        }
        out
    }

    /// Owner's call, or `ToolError::UnknownTool`.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        match self.owner(name) {
            Some(p) => p.call_tool(cx, name, args),
            None => Box::pin(async move { Err(ToolError::UnknownTool(name.to_owned())) }),
        }
    }

    /// Owner's answer or the default.
    fn requires_approval(&self, name: &str) -> bool {
        self.owner(name).is_some_and(|p| p.requires_approval(name))
    }

    /// Owner's answer or the default.
    fn presentation(&self, name: &str) -> Presentation {
        self.owner(name)
            .map_or(Presentation::Group, |p| p.presentation(name))
    }

    /// Owner's answer or the default.
    fn supports_parallel(&self, name: &str, args: Option<&JsonObject>) -> bool {
        self.owner(name)
            .is_some_and(|p| p.supports_parallel(name, args))
    }

    /// Owner's answer or the default.
    fn header_summary(&self, name: &str, args: &JsonObject) -> Option<String> {
        self.owner(name).and_then(|p| p.header_summary(name, args))
    }

    /// The FIRST part with a searcher speaks for the merge; none when no part has one.
    fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher> {
        self.parts
            .iter()
            .any(|p| p.as_tool_searcher().is_some())
            .then_some(self)
    }

    /// Concatenation over parts.
    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        self.parts.iter().flat_map(|p| p.deferred_tools()).collect()
    }

    /// Concatenation over parts.
    fn take_pending_loads(&self) -> Vec<ToolDef> {
        self.parts
            .iter()
            .flat_map(|p| p.take_pending_loads())
            .collect()
    }
}

impl ToolSearcher for Merged {
    /// The first part's hits, even when they are empty.
    fn search_tools(&self, query: &str) -> Vec<ToolDef> {
        self.parts
            .iter()
            .find_map(|p| p.as_tool_searcher())
            .map_or_else(Vec::new, |s| s.search_tools(query))
    }
}
