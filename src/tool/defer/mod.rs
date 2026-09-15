//! Deferred tool groups: MCP servers whose tools stay hidden behind a `search_tools` meta tool
//! until searched for or called by name; the `search_tools` description, catalog and search-result texts are
//! model-facing text the tests pin. How a hidden group is presented to a provider — the four defer modes — is `mode`.

pub(crate) mod mode;

use std::{
    collections::HashSet,
    fmt::Write as _,
    sync::{Arc, Mutex, MutexGuard},
};

use crate::BoxFuture;
use crate::provider::model::{JsonObject, ToolDef};
use crate::text::go_quote;
use crate::tool::context::RunCtx;
use crate::tool::{
    DeferState, DeferredToolStatus, Dispatcher, Owner, Presentation, ToolOutput, ToolResult,
};
use serde_json::{Value, json};

pub(crate) use crate::tool::PrefixOf; // "mcp__<segment>__" once the server connected, "" before; queried lazily per call

use crate::tool::args::str_arg;

/// One hidden group: the MCP server name and its configured one-line summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredGroup {
    /// Server name (the `defer:` entry).
    pub name: String,
    /// Capability summary shown in the `search_tools` description and catalog.
    pub summary: String,
}

/// The meta tool the deferring wrapper advertises.
pub const SEARCH_TOOL_NAME: &str = "search_tools";
/// Cap on the `search_tools` description (the tightest known dialect limits sit around 1 KB).
pub const DESC_BUDGET: usize = 800;
/// Bound of one group line's summary inside the description; the full text appears in the catalog.
pub(crate) const SUMMARY_CLAMP: usize = 120;
/// Bound of a tool's one-liner in the catalog.
pub(crate) const CATALOG_DESC_CLAMP: usize = 100;
/// Above this many hidden tools the catalog lists names only.
pub const CATALOG_NAMES_ONLY_AT: usize = 50;
/// How many matched tools one search enables.
pub const SEARCH_TOP_K: usize = 5;
/// First line of the `search_tools` description.
pub(crate) const SEARCH_HEAD: &str =
    "Search and load additional tools before first use. Hidden tool groups:\n";
/// Last line of the `search_tools` description.
pub(crate) const SEARCH_FOOT: &str = "Query by capability keywords (e.g. \"create pull request\"); matched tools load and stay available. An empty query lists every hidden tool. Calling a hidden tool directly by name also loads it.";
/// Description of the `query` parameter.
pub(crate) const SEARCH_QUERY_DESCRIPTION: &str =
    "Capability keywords; empty lists everything hidden.";

/// Normal mode wrapper (frozen = false).
pub fn defer(
    inner: Arc<dyn Dispatcher>,
    groups: Vec<DeferredGroup>,
    prefix_of: PrefixOf,
) -> Arc<dyn Dispatcher> {
    Arc::new(DeferDispatcher::new(inner, groups, prefix_of, false))
}

/// system-tools wrapper (frozen = true): claimed defs never appear in `tools()`; first enable queues into pending.
pub(crate) fn defer_frozen(
    inner: Arc<dyn Dispatcher>,
    groups: Vec<DeferredGroup>,
    prefix_of: PrefixOf,
) -> Arc<dyn Dispatcher> {
    Arc::new(DeferDispatcher::new(inner, groups, prefix_of, true))
}

/// Keyword scoring: terms = lowercase whitespace split; +2 per term in name, +1 in description, +1 in
/// `param_corpus`; score > 0; stable sort score desc, name asc.
pub(crate) fn rank_tools(defs: &[ToolDef], query: &str) -> Vec<ToolDef> {
    let lowered = query.to_lowercase();
    let terms: Vec<&str> = lowered.split_whitespace().collect();
    let mut hits: Vec<(ToolDef, usize)> = defs
        .iter()
        .filter_map(|def| {
            let name = def.name.to_lowercase();
            let desc = def.description.to_lowercase();
            let params = param_corpus(def.input_schema.as_ref());
            let score: usize = terms
                .iter()
                .map(|t| {
                    usize::from(name.contains(t)) * 2
                        + usize::from(desc.contains(t))
                        + usize::from(params.contains(t))
                })
                .sum();
            (score > 0).then(|| (def.clone(), score))
        })
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    hits.into_iter().map(|(def, _)| def).collect()
}

/// The searchable text of a schema: property names + descriptions, recursing into `properties` and `items`; lowercased. None → "".
pub(crate) fn param_corpus(schema: Option<&JsonObject>) -> String {
    fn walk(m: &JsonObject, out: &mut String) {
        if let Some(Value::Object(props)) = m.get("properties") {
            for (name, v) in props {
                out.push_str(name);
                out.push(' ');
                if let Value::Object(pm) = v {
                    if let Some(Value::String(desc)) = pm.get("description") {
                        out.push_str(desc);
                        out.push(' ');
                    }
                    walk(pm, out);
                }
            }
        }
        if let Some(Value::Object(items)) = m.get("items") {
            walk(items, out);
        }
    }
    let Some(schema) = schema else {
        return String::new();
    };
    let mut out = String::new();
    walk(schema, &mut out);
    out.to_lowercase()
}

/// First line, trimmed; > max chars → first max-1 chars + "…".
pub(crate) fn clamp_line(s: &str, max_chars: usize) -> String {
    let first = s.split('\n').next().unwrap_or("").trim();
    if first.chars().count() <= max_chars {
        return first.to_owned();
    }
    let mut out: String = first.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Keeps whole lines while used+len+1 <= budget (BYTES); returns (kept, omitted).
pub(crate) fn fit_lines(lines: &[String], budget: usize) -> (Vec<String>, usize) {
    let mut used = 0;
    let mut kept = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if used + ln.len() + 1 > budget {
            return (kept, lines.len() - i);
        }
        kept.push(ln.clone());
        used += ln.len() + 1;
    }
    (kept, 0)
}

/// One group resolved against the live tool set.
struct GroupView<'a> {
    group: &'a DeferredGroup,
    /// `""` = still connecting.
    prefix: String,
    tools: Vec<ToolDef>,
}

impl GroupView<'_> {
    fn count_label(&self) -> String {
        if self.prefix.is_empty() {
            "connecting…".to_owned()
        } else {
            format!("{} tools", self.tools.len())
        }
    }
}

/// Load state guarded by the wrapper's mutex.
#[derive(Default)]
struct LoadState {
    /// Wire names searched (or implicitly called) in.
    enabled: HashSet<String>,
    /// Frozen mode: loads awaiting `take_pending_loads`.
    pending: Vec<ToolDef>,
}

impl LoadState {
    /// Records a load; in frozen mode the schema also queues for the history mount
    /// (deduplicated — a re-search must not re-append).
    fn enable(&mut self, def: &ToolDef, frozen: bool) {
        if !self.enabled.insert(def.name.clone()) {
            return;
        }
        if frozen {
            self.pending.push(def.clone());
        }
    }
}

/// The `search_tools` wrapper.
struct DeferDispatcher {
    inner: Arc<dyn Dispatcher>,
    groups: Vec<DeferredGroup>,
    prefix_of: PrefixOf,
    /// The "system-tools" mode: the tools array NEVER grows — loaded schemas queue in `pending` instead.
    frozen: bool,
    state: Mutex<LoadState>,
}

impl DeferDispatcher {
    fn new(
        inner: Arc<dyn Dispatcher>,
        groups: Vec<DeferredGroup>,
        prefix_of: PrefixOf,
        frozen: bool,
    ) -> Self {
        Self {
            inner,
            groups,
            prefix_of,
            frozen,
            state: Mutex::new(LoadState::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LoadState> {
        crate::sync::lock(&self.state)
    }

    /// Snapshots `inner.tools()` and buckets the deferred groups' tools; the remainder
    /// (non-deferred tools) is returned alongside.
    fn resolve(&self) -> (Vec<GroupView<'_>>, Vec<ToolDef>) {
        let mut views: Vec<GroupView<'_>> = self
            .groups
            .iter()
            .map(|g| GroupView {
                group: g,
                prefix: (self.prefix_of)(&g.name),
                tools: Vec::new(),
            })
            .collect();
        let mut rest = Vec::new();
        for def in self.inner.tools() {
            if def.name == SEARCH_TOOL_NAME {
                continue; // the wrapper owns this name; a server-side homonym loses
            }
            match views
                .iter_mut()
                .find(|v| !v.prefix.is_empty() && def.name.starts_with(&v.prefix))
            {
                Some(v) => v.tools.push(def),
                None => rest.push(def),
            }
        }
        (views, rest)
    }

    /// The `search_tools` definition — a fixed template plus as many group lines as the budget
    /// admits (level 0 → all lines; level 1 → prefix + "+N more"; level 2 → counts only, when even one line
    /// won't fit).
    fn search_def(views: &[GroupView<'_>]) -> ToolDef {
        let lines: Vec<String> = views
            .iter()
            .map(|v| {
                format!(
                    "- {} ({}): {}",
                    v.group.name,
                    v.count_label(),
                    clamp_line(&v.group.summary, SUMMARY_CLAMP)
                )
            })
            .collect();
        let (body, omitted) = fit_lines(
            &lines,
            DESC_BUDGET - SEARCH_HEAD.len() - SEARCH_FOOT.len() - 1,
        );
        let mut desc = SEARCH_HEAD.to_owned();
        if body.is_empty() && omitted > 0 {
            let tools: usize = views.iter().map(|v| v.tools.len()).sum();
            desc = format!(
                "Search and load additional tools before first use. {} tool groups ({} tools) are hidden.\n",
                views.len(),
                tools
            );
        } else {
            desc.push_str(&body.join("\n"));
            desc.push('\n');
            if omitted > 0 {
                let _ = writeln!(desc, "… +{omitted} more groups — empty query lists all");
            }
        }
        desc.push_str(SEARCH_FOOT);
        ToolDef {
            name: SEARCH_TOOL_NAME.to_owned(),
            description: desc,
            input_schema: schema_object(json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": SEARCH_QUERY_DESCRIPTION,
                    },
                },
            })),
            deferred: false,
        }
    }

    /// The meta tool — keyword scoring over the deferred tools; the top `SEARCH_TOP_K` hits
    /// load; an empty query returns the full catalog instead.
    fn search(&self, args: &JsonObject) -> ToolOutput {
        let query = str_arg(args, "query");
        let (views, _) = self.resolve();
        if query.trim().is_empty() {
            return ToolOutput::ok(self.catalog(&views));
        }

        let all: Vec<ToolDef> = views.iter().flat_map(|v| v.tools.iter().cloned()).collect();
        let hits = rank_tools(&all, query);
        if hits.is_empty() {
            let mut b = format!("No tools matched {}. Hidden groups:\n", go_quote(query));
            for v in &views {
                let _ = writeln!(
                    b,
                    "- {} ({}): {}",
                    v.group.name,
                    v.count_label(),
                    v.group.summary
                );
            }
            b.push_str("Try different keywords, or an empty query to list every tool.");
            return ToolOutput::ok(b);
        }

        let extra = hits.len().saturating_sub(SEARCH_TOP_K);
        let loaded = &hits[..hits.len().min(SEARCH_TOP_K)];
        {
            let mut st = self.lock();
            for def in loaded {
                st.enable(def, self.frozen);
            }
        }

        let mut b = format!(
            "Loaded {} tool(s) — available from the next step:\n",
            loaded.len()
        );
        for def in loaded {
            let _ = writeln!(
                b,
                "- {} — {}",
                def.name,
                clamp_line(&def.description, CATALOG_DESC_CLAMP)
            );
        }
        if extra > 0 {
            let _ = writeln!(
                b,
                "+{extra} more matched but were not loaded — refine the query, list everything with an empty query, or call a tool by its exact name."
            );
        }
        ToolOutput::ok(b.trim_end_matches('\n').to_owned())
    }

    /// The empty-query listing — ALWAYS every group line with its full summary, then the
    /// per-tool index (one-liners up to `CATALOG_NAMES_ONLY_AT` tools, names only beyond).
    fn catalog(&self, views: &[GroupView<'_>]) -> String {
        let total: usize = views.iter().map(|v| v.tools.len()).sum();
        let names_only = total > CATALOG_NAMES_ONLY_AT;

        let st = self.lock();
        let mut b = format!(
            "Hidden tool groups ({} groups, {} tools):\n",
            views.len(),
            total
        );
        for v in views {
            let _ = write!(
                b,
                "\n{} ({}): {}\n",
                v.group.name,
                v.count_label(),
                v.group.summary
            );
            for def in &v.tools {
                let mark = if st.enabled.contains(&def.name) {
                    " (loaded)"
                } else {
                    ""
                };
                if names_only {
                    let _ = writeln!(b, "  {}{mark}", def.name);
                } else {
                    let _ = writeln!(
                        b,
                        "  {} — {}{mark}",
                        def.name,
                        clamp_line(&def.description, CATALOG_DESC_CLAMP)
                    );
                }
            }
        }
        b.push_str("\nQuery by capability keywords to load tools, or call a listed tool directly to load and run it in one step.");
        b
    }

    /// A direct call to a hidden-but-known tool enables it in one step — the safety net for
    /// models that skip the search.
    fn implicit_load(&self, name: &str) {
        let (views, _) = self.resolve();
        let def = views
            .iter()
            .find(|v| !v.prefix.is_empty() && name.starts_with(&v.prefix))
            .and_then(|v| v.tools.iter().find(|d| d.name == name));
        if let Some(def) = def {
            self.lock().enable(def, self.frozen);
        }
    }
}

/// A `json!` object literal as the `JsonObject` a `ToolDef` carries.
fn schema_object(v: Value) -> Option<JsonObject> {
    match v {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

impl Dispatcher for DeferDispatcher {
    /// `[search_tools] ++ rest ++ (unless frozen) every loaded deferred tool`.
    fn tools(&self) -> Vec<ToolDef> {
        let (views, rest) = self.resolve();
        let st = self.lock();
        let mut out = vec![Self::search_def(&views)];
        out.extend(rest);
        if self.frozen {
            return out; // loaded schemas travel via history, never the array
        }
        for v in &views {
            for def in &v.tools {
                if st.enabled.contains(&def.name) {
                    out.push(def.clone());
                }
            }
        }
        out
    }

    /// `search_tools` → search; anything else → implicit load, then ALWAYS forward to inner.
    fn call_tool<'a>(
        &'a self,
        cx: &'a RunCtx,
        name: &'a str,
        args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if name == SEARCH_TOOL_NAME {
                return Ok(self.search(&args));
            }
            self.implicit_load(name);
            self.inner.call_tool(cx, name, args).await
        })
    }

    /// Pass-through.
    fn requires_approval(&self, name: &str) -> bool {
        self.inner.requires_approval(name)
    }

    /// Pass-through.
    fn presentation(&self, name: &str) -> Presentation {
        self.inner.presentation(name)
    }

    fn as_owner(&self) -> Option<&dyn Owner> {
        Some(self)
    }

    /// Every deferred tool with its load state.
    fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
        let (views, _) = self.resolve();
        let st = self.lock();
        views
            .iter()
            .flat_map(|v| {
                v.tools.iter().map(|def| DeferredToolStatus {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    group: v.group.name.clone(),
                    state: if st.enabled.contains(&def.name) {
                        DeferState::Loaded
                    } else {
                        DeferState::Deferred
                    },
                })
            })
            .collect()
    }

    /// Drains schemas loaded since the last take (frozen mode).
    fn take_pending_loads(&self) -> Vec<ToolDef> {
        std::mem::take(&mut self.lock().pending)
    }
}

impl Owner for DeferDispatcher {
    /// Ownership including HIDDEN tools — Merge routes direct calls here so the implicit-load
    /// path works through the merged dispatcher.
    fn owns(&self, name: &str) -> bool {
        name == SEARCH_TOOL_NAME
            || self.inner.as_owner().map_or_else(
                || self.inner.tools().iter().any(|d| d.name == name),
                |o| o.owns(name),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{SEARCH_FOOT, SEARCH_HEAD, clamp_line, fit_lines};

    #[test]
    fn template_byte_lengths_match_go() {
        // budget = 800 - len(head) - len(foot) - 1 = 537.
        assert_eq!(SEARCH_HEAD.len(), 71);
        assert_eq!(SEARCH_FOOT.len(), 191);
    }

    #[test]
    fn clamp_line_counts_chars_and_cuts_at_the_first_line() {
        assert_eq!(clamp_line("  hello \nworld", 10), "hello");
        assert_eq!(clamp_line("abcdef", 6), "abcdef");
        assert_eq!(clamp_line("abcdefg", 6), "abcde…");
        assert_eq!(clamp_line("ééééééé", 6), "ééééé…");
        assert_eq!(clamp_line("", 3), "");
    }

    #[test]
    fn fit_lines_counts_a_newline_per_line() {
        let lines = vec!["aaaa".to_owned(), "bbbb".to_owned(), "cccc".to_owned()];
        assert_eq!(fit_lines(&lines, 10), (lines[..2].to_vec(), 1));
        assert_eq!(fit_lines(&lines, 9), (lines[..1].to_vec(), 2));
        assert_eq!(fit_lines(&lines, 4), (Vec::new(), 3));
        assert_eq!(fit_lines(&lines, 15), (lines.clone(), 0));
    }
}
