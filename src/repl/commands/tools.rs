//! `/tools` — the LIVE tool and MCP view (chat/run.go:849-860, chat/chat.go:520-669).
//!
//! Two `View` tabs on a 500 ms refresh: a background MCP connect that finishes while the
//! panel is open appears without reopening it. The result is discarded — the surface is a
//! viewer, not a picker. Its MCP tab IS the policy's "/mcp": the Go binary has no such
//! command (T-23).
//!
//! The server SNAPSHOT comes through [`crate::repl::McpHooks`] when the command layer wired
//! one. The rendering
//! is here either way: without the hook (a build with no MCP at all, and every test in
//! this crate) the snapshot is simply empty, every tool is a built-in and the MCP tab says
//! so.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::provider::model::ToolDef;
use crate::tool::Dispatcher;
use crate::tool::{DeferState, DeferredToolStatus};
use crate::ui::facade::{Panel, TabbedSpec};

use crate::mcp::{ServerState, ServerStatus};
use crate::repl::render::styles::{bold, dim, green, red, yellow};
use crate::repl::run::Repl;

/// The refresh cadence of a live panel (chat/run.go:851 `RefreshEvery`).
const REFRESH_EVERY_MS: u64 = 500;

/// The Tools tab's rows (chat/chat.go:523-609 `toolStatusLines`).
///
/// Advertised tools first — name, source tag, description — then the deferred ones still
/// hidden, dimmed and sorted by (group, name), so a search-loaded tool moves up into the
/// advertised set rather than being listed twice. A tool a server registered carries that
/// server's `[mcp: <name>]` tag (plus its load state when it arrived through a search);
/// everything else is `[built-in]`.
pub(crate) fn tool_status_lines(
    dispatch: &dyn Dispatcher,
    servers: &[ServerStatus],
) -> Vec<String> {
    let defs: Vec<ToolDef> = dispatch.tools();
    let deferred: Vec<DeferredToolStatus> = dispatch.deferred_tools();
    let advertised: HashSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let mut hidden: Vec<&DeferredToolStatus> = deferred
        .iter()
        .filter(|st| !advertised.contains(st.name.as_str()))
        .collect();
    hidden.sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));

    if defs.is_empty() && hidden.is_empty() {
        return vec![dim("No tools available.")];
    }

    // An advertised tool that arrived via search keeps its "loaded" badge.
    let defer_state: HashMap<&str, &DeferredToolStatus> =
        deferred.iter().map(|st| (st.name.as_str(), st)).collect();
    // The wire name → server oracle, composed from each server's segment and raw tool names.
    let mut source: HashMap<String, &str> = HashMap::new();
    for srv in servers {
        for wire in srv.wire_names() {
            source.insert(wire, srv.name.as_str());
        }
    }

    // Pad names to the longest one so the source tags line up; namespaced display names
    // ("server:tool") routinely exceed any fixed width.
    let names: Vec<String> = defs
        .iter()
        .map(|d| crate::tool::fmt::display_tool_name(&d.name))
        .collect();
    let hidden_names: Vec<String> = hidden
        .iter()
        .map(|st| crate::tool::fmt::display_tool_name(&st.name))
        .collect();
    let width = names
        .iter()
        .chain(hidden_names.iter())
        .map(String::len)
        .max()
        .unwrap_or(0);

    let mut head = dim(&format!("{} tool(s) available", defs.len()));
    if !deferred.is_empty() {
        head.push_str(&dim(&format!(
            " · {} deferred ({} loaded)",
            deferred.len(),
            deferred.len() - hidden.len()
        )));
    }
    let mut lines = Vec::with_capacity(defs.len() + hidden.len() + 1);
    lines.push(head);
    for (d, name) in defs.iter().zip(&names) {
        let tag = match source.get(d.name.as_str()) {
            None => green("[built-in]"),
            Some(srv) => {
                let mut label = (*srv).to_owned();
                if let Some(st) = defer_state.get(d.name.as_str()) {
                    label.push_str(" · ");
                    label.push_str(st.state.as_str());
                }
                yellow(&format!("[mcp: {label}]"))
            }
        };
        let desc = d.description.replace('\n', " ");
        lines.push(format!(
            "{}  {}  {}",
            bold(&format!("{name:width$}")),
            tag,
            dim(&desc)
        ));
    }
    for (st, name) in hidden.iter().zip(&hidden_names) {
        let desc = st.description.replace('\n', " ");
        lines.push(format!(
            "{}  {}  {}",
            dim(&format!("{name:width$}")),
            dim(&format!("[mcp: {} · {}]", st.group, st.state.as_str())),
            dim(&desc)
        ));
    }
    lines
}

/// The MCP tab's rows (chat/chat.go:613-671 `mcpStatusLines`) — every configured server's
/// connection state, endpoint, tools and error, under a one-line summary. The per-server
/// defer tally counts `loaded` against the group's total.
pub(crate) fn mcp_status_lines(dispatch: &dyn Dispatcher, servers: &[ServerStatus]) -> Vec<String> {
    if servers.is_empty() {
        return vec![dim("No MCP servers configured.")];
    }
    let mut defer_by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for st in dispatch.deferred_tools() {
        let tally = defer_by.entry(st.group).or_insert((0, 0));
        tally.0 += 1;
        if st.state == DeferState::Loaded {
            tally.1 += 1;
        }
    }
    let total: usize = servers.iter().map(|s| s.tool_count).sum();
    let mut lines = vec![dim(&format!(
        "{} server(s) · {total} tool(s)",
        servers.len()
    ))];
    for srv in servers {
        let mut status = match &srv.state {
            ServerState::Connected { .. } => green("connected"),
            ServerState::Connecting => dim("connecting…"),
            ServerState::Failed(_) => red("disconnected"),
        };
        if let Some(&(total_g, loaded)) = defer_by.get(&srv.name)
            && total_g > 0
        {
            status.push_str(&dim(&format!(" · deferred ({loaded}/{total_g} loaded)")));
        }
        lines.push(format!("{}  [{status}]", bold(&srv.name)));
        lines.push(dim(&format!("  endpoint: {}", srv.endpoint)));
        if srv.tool_count == 0 {
            lines.push(dim("  tools: (none)"));
        } else {
            lines.push(dim(&format!(
                "  tools ({}): {}",
                srv.tool_count,
                srv.tools.join(", ")
            )));
        }
        if let ServerState::Failed(err) = &srv.state {
            lines.push(red(&format!(
                "  error: {}",
                err.split('\n').next().unwrap_or_default()
            )));
        }
    }
    lines
}

/// Builds a rows closure over the live server snapshot: the binary's hook when it wired
/// one, an empty snapshot otherwise. Two are needed per tab (initial lines + refresh).
fn rows_fn(
    hook: Option<&Arc<dyn Fn() -> Vec<ServerStatus> + Send + Sync>>,
    dispatch: &Arc<dyn Dispatcher>,
    render: fn(&dyn Dispatcher, &[ServerStatus]) -> Vec<String>,
) -> impl Fn() -> Vec<String> + Send + 'static {
    let hook = hook.map(Arc::clone);
    let dispatch = Arc::clone(dispatch);
    move || {
        let servers = hook.as_ref().map(|f| f()).unwrap_or_default();
        render(&*dispatch, &servers)
    }
}

/// `/tools`: two live viewer tabs; the commit — and a facade failure — are discarded.
pub(crate) async fn cmd_tools(repl: &Repl) {
    let tools_rows = rows_fn(
        repl.handles.mcp.servers.as_ref(),
        &repl.conv.dispatch,
        tool_status_lines,
    );
    let mcp_rows = rows_fn(
        repl.handles.mcp.servers.as_ref(),
        &repl.conv.dispatch,
        mcp_status_lines,
    );
    let spec = TabbedSpec {
        refresh_every_ms: REFRESH_EVERY_MS,
        panels: vec![
            Panel::view("Tools".to_owned(), tools_rows()).with_refresh(Box::new(tools_rows)),
            Panel::view("MCP".to_owned(), mcp_rows())
                .with_wrap(true)
                .with_refresh(Box::new(mcp_rows)),
        ],
        ..TabbedSpec::default()
    };
    let _ = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await;
}

#[cfg(test)]
mod tests {
    use crate::BoxFuture;
    use crate::provider::model::JsonObject;
    use crate::provider::model::ToolDef;
    use crate::text::ansi::strip_sgr;
    use crate::tool::context::RunCtx;
    use crate::tool::{ToolOutput, ToolResult};
    use pretty_assertions::assert_eq;

    use super::{DeferState, DeferredToolStatus, Dispatcher, ServerState, ServerStatus};

    /// Advertises `defs` and reports deferred state — the `/tools` view fixture.
    // Go: chat/toolstatus_test.go:13 deferStatusDispatcher
    struct DeferStatus {
        defs: Vec<ToolDef>,
        status: Vec<DeferredToolStatus>,
    }

    impl DeferStatus {
        fn new(defs: &[(&str, &str)], status: &[(&str, &str, &str, DeferState)]) -> Self {
            Self {
                defs: defs
                    .iter()
                    .map(|(n, d)| ToolDef {
                        name: (*n).to_owned(),
                        description: (*d).to_owned(),
                        ..ToolDef::default()
                    })
                    .collect(),
                status: status
                    .iter()
                    .map(|(n, d, g, st)| DeferredToolStatus {
                        name: (*n).to_owned(),
                        description: (*d).to_owned(),
                        group: (*g).to_owned(),
                        state: *st,
                    })
                    .collect(),
            }
        }
    }

    impl Dispatcher for DeferStatus {
        fn tools(&self) -> Vec<ToolDef> {
            self.defs.clone()
        }
        fn call_tool<'a>(
            &'a self,
            _cx: &'a RunCtx,
            _name: &'a str,
            _args: JsonObject,
        ) -> BoxFuture<'a, ToolResult> {
            Box::pin(async { Ok(ToolOutput::ok("ok")) })
        }
        fn deferred_tools(&self) -> Vec<DeferredToolStatus> {
            self.status.clone()
        }
    }

    fn plain(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| strip_sgr(l)).collect()
    }

    /// The `/tools` list shows deferred-but-hidden tools dimmed AFTER the advertised set,
    /// each row naming its group and state, sorted by (group, name); the header carries
    /// the tally; a search-loaded tool is advertised, never repeated in the hidden tail.
    // Go: chat/toolstatus_test.go:27 TestToolStatusLinesDeferred
    #[test]
    fn test_tool_status_lines_deferred() {
        let d = DeferStatus::new(
            &[("search_tools", "meta"), ("mcp__seo__serp", "SERP lookup")],
            &[
                (
                    "mcp__seo__serp",
                    "SERP lookup",
                    "DataForSEO",
                    DeferState::Loaded,
                ),
                (
                    "mcp__seo__keywords",
                    "keyword data",
                    "DataForSEO",
                    DeferState::Deferred,
                ),
                (
                    "mcp__cdt__click",
                    "click an element",
                    "chrome-devtools",
                    DeferState::Deferred,
                ),
            ],
        );
        let text = plain(&super::tool_status_lines(&d, &[]));
        assert_eq!(text[0], "2 tool(s) available · 3 deferred (1 loaded)");
        assert_eq!(
            text.len(),
            5,
            "want header + 2 advertised + 2 hidden:\n{}",
            text.join("\n")
        );
        assert!(
            text[3].contains("seo:keywords") && text[3].contains("[mcp: DataForSEO · deferred]"),
            "hidden row 1 = {:?}",
            text[3]
        );
        assert!(
            text[4].contains("cdt:click") && text[4].contains("[mcp: chrome-devtools · deferred]"),
            "hidden row 2 = {:?}",
            text[4]
        );
        let tail = text[3..].join("\n");
        assert!(
            !tail.contains("serp"),
            "loaded tool leaked into the hidden tail:\n{tail}"
        );
    }

    /// Without any deferred state the header stays bare — the pre-defer contract.
    // Go: chat/toolstatus_test.go:68 TestToolStatusLinesNoDefer
    #[test]
    fn test_tool_status_lines_no_defer() {
        let d = DeferStatus::new(&[("echo", "e")], &[]);
        assert_eq!(
            plain(&super::tool_status_lines(&d, &[]))[0],
            "1 tool(s) available"
        );
    }

    /// All tools hidden (nothing advertised yet) still renders the list — never
    /// "No tools available".
    // Go: chat/toolstatus_test.go:78 TestToolStatusLinesAllHidden
    #[test]
    fn test_tool_status_lines_all_hidden() {
        let d = DeferStatus::new(
            &[],
            &[("mcp__x__a", "d", "x", DeferState::DeferredProtocol)],
        );
        let text = plain(&super::tool_status_lines(&d, &[])).join("\n");
        assert!(
            !text.contains("No tools available"),
            "hidden-only set must still list:\n{text}"
        );
        assert!(
            text.contains("0 tool(s) available · 1 deferred (0 loaded)")
                && text.contains("deferred (protocol)"),
            "unexpected render:\n{text}"
        );
    }

    /// A tool a server registered carries that server's `[mcp: <name>]` tag, resolved
    /// through the wire-name oracle the binary hands over; everything else is
    /// `[built-in]`. A tool that arrived through a search keeps its load badge.
    // Go: chat/chat.go:556-604 (toolStatusLines source column)
    #[test]
    fn mcp_sourced_tools_carry_their_server_tag() {
        let d = DeferStatus::new(
            &[("shell", "run"), ("mcp__seo__serp", "SERP lookup")],
            &[(
                "mcp__seo__serp",
                "SERP lookup",
                "DataForSEO",
                DeferState::Loaded,
            )],
        );
        let servers = vec![ServerStatus {
            name: "DataForSEO".to_owned(),
            endpoint: "https://seo.example/mcp".to_owned(),
            state: ServerState::Connected {
                segment: "seo".to_owned(),
            },
            tool_count: 1,
            tools: vec!["serp".to_owned()],
            ..ServerStatus::default()
        }];
        let text = plain(&super::tool_status_lines(&d, &servers));
        assert!(text[1].contains("[built-in]"), "row 1 = {:?}", text[1]);
        assert!(
            text[2].contains("[mcp: DataForSEO · loaded]"),
            "row 2 = {:?}",
            text[2]
        );
        // Without the snapshot the same tool falls back to [built-in] — the no-MCP build.
        let bare = plain(&super::tool_status_lines(&d, &[]));
        assert!(bare[2].contains("[built-in]"), "row 2 = {:?}", bare[2]);
    }

    /// The MCP tab: a one-line summary, then per server the state, endpoint, tools and
    /// (when it failed) the first line of its error; the per-server defer tally counts
    /// `loaded` against the group total. No servers ⇒ the one-line notice.
    // Go: chat/chat.go:613-671 mcpStatusLines
    #[test]
    fn mcp_status_lines_render_every_server() {
        let d = DeferStatus::new(
            &[],
            &[
                ("mcp__seo__serp", "s", "DataForSEO", DeferState::Loaded),
                ("mcp__seo__kw", "k", "DataForSEO", DeferState::Deferred),
            ],
        );
        assert_eq!(
            plain(&super::mcp_status_lines(&d, &[])),
            vec!["No MCP servers configured."]
        );

        let servers = vec![
            ServerStatus {
                name: "DataForSEO".to_owned(),
                endpoint: "https://seo.example/mcp".to_owned(),
                state: ServerState::Connected {
                    segment: "seo".to_owned(),
                },
                tool_count: 2,
                tools: vec!["serp".to_owned(), "kw".to_owned()],
                ..ServerStatus::default()
            },
            ServerStatus {
                name: "broken".to_owned(),
                endpoint: "broken-cmd --stdio".to_owned(),
                state: ServerState::Failed("connect: no such file\nsecond line".to_owned()),
                ..ServerStatus::default()
            },
        ];
        assert_eq!(
            plain(&super::mcp_status_lines(&d, &servers)),
            vec![
                "2 server(s) · 2 tool(s)",
                "DataForSEO  [connected · deferred (1/2 loaded)]",
                "  endpoint: https://seo.example/mcp",
                "  tools (2): serp, kw",
                "broken  [disconnected]",
                "  endpoint: broken-cmd --stdio",
                "  tools: (none)",
                "  error: connect: no such file",
            ]
        );
    }
}
