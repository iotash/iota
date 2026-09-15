//! The built-in tool registry (tool/tool.go:343-528): sets are built from the `tools:` map in key order, tools
//! are registered first-wins by name, and the registry itself is a `Dispatcher`.

use std::{collections::HashMap, sync::Arc};

use crate::BoxFuture;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_quote;
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{Dispatcher, Presentation, Tool, ToolEnv, ToolResult};

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

impl Dispatcher for Registry {
    /// Definitions in registration order.
    fn tools(&self) -> Vec<ToolDef> {
        self.order.iter().map(|t| t.def()).collect()
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
