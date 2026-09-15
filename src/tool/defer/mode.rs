//! Defer modes: how hidden groups are presented to a provider —
//! the `search_tools` wrapper (`normal`), the provider-side protocols (`reference`, `tool-search`) and the
//! frozen system-tools mount.
//!
//! Which modes a provider can speak is a property of its DIALECT ([`DeferMode::supports`]), so the check
//! belongs where the model that names the mode is written: `crate::config` refuses a mismatch when the file
//! is loaded, and by the time a dispatcher is assembled there is nothing left to decide.

use std::sync::Arc;

use crate::BoxFuture;
use crate::provider::ProviderKind;
use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use crate::tool::{
    DeferState, DeferredToolStatus, Dispatcher, Presentation, ToolResult, ToolSearcher,
};

use super::{DeferredGroup, PrefixOf, SEARCH_TOP_K, defer, defer_frozen, rank_tools};

/// The four defer modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferMode {
    /// `normal`: the `search_tools` wrapper (every provider).
    Normal,
    /// `reference`: Anthropic's deferred-tool protocol.
    Reference,
    /// `tool-search`: the Responses tool-search protocol.
    ToolSearch,
    /// `system-tools`: the frozen system-message mount (chat-completions).
    SystemTools,
}

impl DeferMode {
    /// The mode used when none is configured.
    pub const DEFAULT: DeferMode = DeferMode::Normal;

    /// `"normal"` | `"reference"` | `"tool-search"` | `"system-tools"`; anything else → None.
    pub fn from_name(s: &str) -> Option<DeferMode> {
        match s {
            "normal" => Some(Self::Normal),
            "reference" => Some(Self::Reference),
            "tool-search" => Some(Self::ToolSearch),
            "system-tools" => Some(Self::SystemTools),
            _ => None,
        }
    }

    /// The config spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Reference => "reference",
            Self::ToolSearch => "tool-search",
            Self::SystemTools => "system-tools",
        }
    }

    /// `Normal` → always; `Reference` → `Anthropic`; `ToolSearch` → `OpenResponses`; `SystemTools` → `OpenAi`.
    pub fn supports(self, kind: ProviderKind) -> bool {
        match self {
            Self::Normal => true,
            Self::Reference => kind == ProviderKind::Anthropic,
            Self::ToolSearch => kind == ProviderKind::OpenResponses,
            Self::SystemTools => kind == ProviderKind::OpenAi,
        }
    }

    /// `Normal` → `defer()`; `Reference` → `MarkedDispatcher`; `ToolSearch` → `SearchingDispatcher`; `SystemTools` →
    /// `defer_frozen()`.
    pub fn wrap(
        self,
        inner: Arc<dyn Dispatcher>,
        groups: Vec<DeferredGroup>,
        prefix_of: PrefixOf,
    ) -> Arc<dyn Dispatcher> {
        match self {
            Self::Normal => defer(inner, groups, prefix_of),
            Self::Reference => Arc::new(MarkedDispatcher {
                inner,
                groups,
                prefix_of,
            }),
            Self::ToolSearch => Arc::new(SearchingDispatcher(MarkedDispatcher {
                inner,
                groups,
                prefix_of,
            })),
            Self::SystemTools => defer_frozen(inner, groups, prefix_of),
        }
    }
}

/// `reference` mode: inner defs are copied with `deferred = true` under a connected prefix; calls, approval and
/// presentation pass through; `owns` is None; `deferred_tools` reports `DeferredProtocol`.
pub(crate) struct MarkedDispatcher {
    inner: Arc<dyn Dispatcher>,
    groups: Vec<DeferredGroup>,
    prefix_of: PrefixOf,
}

impl MarkedDispatcher {
    /// Whether `name` sits under a CONNECTED deferred group's prefix.
    fn deferred_prefix(&self, name: &str) -> bool {
        self.groups.iter().any(|g| {
            let p = (self.prefix_of)(&g.name);
            !p.is_empty() && name.starts_with(&p)
        })
    }
}

impl Dispatcher for MarkedDispatcher {
    /// Inner defs, marked `deferred` under a connected prefix.
    fn tools(&self) -> Vec<ToolDef> {
        self.inner
            .tools()
            .into_iter()
            .map(|mut def| {
                if self.deferred_prefix(&def.name) {
                    def.deferred = true;
                }
                def
            })
            .collect()
    }

    /// Pass-through.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        self.inner.call_tool(cx, name, args)
    }

    /// Pass-through.
    fn requires_approval(&self, name: &str) -> bool {
        self.inner.requires_approval(name)
    }

    /// Pass-through.
    fn presentation(&self, name: &str) -> Presentation {
        self.inner.presentation(name)
    }

    /// Every marked def with `DeferState::DeferredProtocol` (groups outer, inner tools inner).
    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        // One live-view snapshot per invocation (like DeferDispatcher::resolve): re-querying per group would
        // rebuild — and, under the MCP manager's read lock, clone — the whole tool list G times.
        let defs = self.inner.tools();
        let mut out = Vec::new();
        for g in &self.groups {
            let p = (self.prefix_of)(&g.name);
            if p.is_empty() {
                continue;
            }
            for def in defs.iter().filter(|def| def.name.starts_with(&p)) {
                out.push(DeferredToolStatus {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    group: g.name.clone(),
                    state: DeferState::DeferredProtocol,
                });
            }
        }
        out
    }
}

/// `tool-search` mode: a `MarkedDispatcher` plus `search_tools(q)` = `rank_tools(marked-deferred defs, q)[..5]`.
pub(crate) struct SearchingDispatcher(MarkedDispatcher);

impl Dispatcher for SearchingDispatcher {
    /// As `MarkedDispatcher`.
    fn tools(&self) -> Vec<ToolDef> {
        self.0.tools()
    }

    /// As `MarkedDispatcher`.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        self.0.call_tool(cx, name, args)
    }

    /// As `MarkedDispatcher`.
    fn requires_approval(&self, name: &str) -> bool {
        self.0.requires_approval(name)
    }

    /// As `MarkedDispatcher`.
    fn presentation(&self, name: &str) -> Presentation {
        self.0.presentation(name)
    }

    fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher> {
        Some(self)
    }

    /// As `MarkedDispatcher`.
    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        self.0.deferred_tools()
    }
}

impl ToolSearcher for SearchingDispatcher {
    /// `rank_tools(marked-deferred defs, query)` capped at `SEARCH_TOP_K`.
    fn search_tools(&self, query: &str) -> Vec<ToolDef> {
        let deferred: Vec<ToolDef> = self.0.tools().into_iter().filter(|d| d.deferred).collect();
        let mut hits = rank_tools(&deferred, query);
        hits.truncate(SEARCH_TOP_K);
        hits
    }
}
