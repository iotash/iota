//! The two `Dispatcher` implementations of the framework: the built-in tool registry (tool/tool.go:343-528 —
//! sets are built from the `tools:` map in key order, tools are registered first-wins by name) and the live
//! union of dispatcher parts (tool/tool.go:538-673 — tools are re-queried on every call, names dedup with the
//! earlier part winning, and calls route to the owning part).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::BoxFuture;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_quote;
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{
    DeferredToolStatus, Dispatcher, Owner, Presentation, Tool, ToolEnv, ToolResult, ToolSearcher,
};

use crate::tool::sets::{RawNode, ToolsConfig, set_factory};
use crate::tool::yaml11::is_false_scalar;

/// Ordered, name-indexed set of built-in tools. First registration by name wins.
#[derive(Default)]
pub struct Registry {
    order: Vec<Arc<dyn Tool>>,
    index: HashMap<String, usize>,
}

impl Registry {
    /// Keys in `BTreeMap` order; `is_false_scalar` → skipped silently; unknown → warn `unknown toolset {name:?}
    /// (ignored)`; factory error → warn `toolset {name:?}: {err} (ignored)`; never aborts. (`{name:?}` = `go_quote`.)
    pub fn build(env: &ToolEnv, raw: &ToolsConfig, warn: &mut dyn FnMut(String)) -> Registry {
        let mut r = Registry::default();
        for (name, node) in raw {
            if is_false_scalar(node) {
                continue; // explicit opt-out (e.g. ask: false)
            }
            r.build_set(env, name, Some(node), warn);
        }
        r
    }

    /// Same warnings; factory called with `None`; tools already registered by name are skipped.
    pub fn enable_set(&mut self, env: &ToolEnv, name: &str, warn: &mut dyn FnMut(String)) {
        self.build_set(env, name, None, warn);
    }

    /// tool/tool.go:388-402 / 412-426: resolve the factory, run it, register what it built; every failure is one
    /// warning and never an abort.
    fn build_set(
        &mut self,
        env: &ToolEnv,
        name: &str,
        node: Option<&RawNode>,
        warn: &mut dyn FnMut(String),
    ) {
        let Some(factory) = set_factory(name) else {
            warn(format!("unknown toolset {} (ignored)", go_quote(name)));
            return;
        };
        match factory(env, node) {
            Ok(tools) => {
                for t in tools {
                    self.add(t);
                }
            }
            Err(e) => warn(format!("toolset {}: {e} (ignored)", go_quote(name))),
        }
    }

    /// First registration by name wins; returns false when skipped.
    pub fn add(&mut self, t: Arc<dyn Tool>) -> bool {
        let name = t.def().name;
        if self.index.contains_key(&name) {
            return false;
        }
        self.index.insert(name, self.order.len());
        self.order.push(t);
        true
    }

    /// Whether no tool is registered.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// The tool registered under `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.index.get(name).and_then(|&i| self.order.get(i))
    }
}

impl Owner for Registry {
    /// One index lookup — never a `tools()` walk.
    fn owns(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }
}

impl Dispatcher for Registry {
    /// Definitions in registration order.
    fn tools(&self) -> Vec<ToolDef> {
        self.order.iter().map(|t| t.def()).collect()
    }

    fn as_owner(&self) -> Option<&dyn Owner> {
        Some(self)
    }

    /// Routes to the named tool; unknown → `ToolError::UnknownTool(name)`.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        match self.get(name) {
            Some(t) => Box::pin(async move { t.call(cx, &args).await }),
            None => Box::pin(async move { Err(ToolError::UnknownTool(name.to_owned())) }),
        }
    }

    /// The tool's answer; unknown → false.
    fn requires_approval(&self, name: &str) -> bool {
        self.get(name).is_some_and(|t| t.requires_approval())
    }

    /// The tool's answer; unknown → `Group`.
    fn presentation(&self, name: &str) -> Presentation {
        self.get(name)
            .map_or(Presentation::Group, |t| t.presentation())
    }

    /// The tool's per-call answer; unknown → false.
    fn supports_parallel(&self, name: &str, args: Option<&JsonObject>) -> bool {
        self.get(name).is_some_and(|t| t.supports_parallel(args))
    }

    /// The tool's answer; unknown → None.
    fn header_summary(&self, name: &str, args: &JsonObject) -> Option<String> {
        self.get(name).and_then(|t| t.header_summary(args))
    }
}

/// Missing key → false; else `yaml11::is_false_scalar`.
pub fn set_disabled(raw: &ToolsConfig, name: &str) -> bool {
    raw.get(name).is_some_and(is_false_scalar)
}

// ---- the live union of parts (tool/tool.go:538-673) ----

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
