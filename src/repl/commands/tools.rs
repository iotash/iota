//! `/tools` — the live list of where the tools come from (docs/design/tools-command.md, DIVERGENCES X-79).
//!
//! ONE list, two levels, no new surface machinery: the first level is a `List` panel with one row per
//! source — `built in` (every tool iota ships with) and then each configured MCP server in config order —
//! naming it, counting its tools and saying how it stands; Enter opens that source's page, Esc closes. The
//! page is a `View` panel: the server's details (status and why, endpoint, transport, auth, tool names,
//! call deadline, its stderr) above its tools; Enter there reconnects the server (`Manager::reconnect`, in
//! the background — the page shows it happening), Esc goes back to the list with the cursor on the row it
//! came from. Both levels re-render every 500 ms, so a background connect, a server going down and a
//! reconnect all show without reopening anything.
//!
//! Until 2026-10-15 this was two read-only tabs, Tools and MCP (chat/run.go:849-860, chat/chat.go:520-669),
//! and the MCP tab was the only place a server's state was read: there is still no `/mcp` (T-23, X-42).
//!
//! The server snapshot and the manager come through [`crate::repl::McpHooks`] when the command layer wired
//! them. Without them (a build with no MCP, and most tests in this crate) the list has only `built in`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use crate::mcp::config::AuthMode;
use crate::mcp::{Manager, ServerDetail, ServerState, ServerStatus};
use crate::repl::render::styles::{bold, dim, green, red, yellow};
use crate::repl::render::transcript::Transcript;
use crate::repl::run::Repl;
use crate::sync::lock;
use crate::text::human_duration;
use crate::tool::Dispatcher;
use crate::tool::{DeferState, DeferredToolStatus};
use crate::ui::facade::{Panel, Refreshed, TabbedSpec};

/// The refresh cadence of a live panel (chat/run.go:851 `RefreshEvery`).
const REFRESH_EVERY_MS: u64 = 500;

/// The first row's name.
pub(crate) const BUILT_IN: &str = "built in";

/// How many characters of a tool's description a page shows: one line of it, the rest is the model's.
const DESC_CHARS: usize = 100;

/// How many lines of a live stdio server's stderr its page shows (a failure's detail keeps its own, longer
/// tail).
const STDERR_PAGE_LINES: usize = 10;

/// One tool as a page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRow {
    /// The tool's own name: a built-in's, or an MCP tool's RAW name (what its server calls it).
    name: String,
    /// Its description, on one line.
    desc: String,
    /// Its deferred-loading state, when it is in a deferred group.
    defer: Option<DeferState>,
    /// A deferred tool not advertised yet (no search has loaded it).
    hidden: bool,
}

/// Every tool, sorted by where it comes from: built in, or the server (index into the snapshot) that
/// registered it.
#[derive(Debug, Default)]
struct Inventory {
    built_in: Vec<ToolRow>,
    servers: BTreeMap<usize, Vec<ToolRow>>,
}

impl Inventory {
    /// Advertised tools first, in the dispatcher's order, then the deferred ones still hidden, sorted by name.
    /// A wire name a server's status registered is that server's; everything else is built in.
    fn of(dispatch: &dyn Dispatcher, servers: &[ServerStatus]) -> Self {
        let mut owner: HashMap<String, (usize, String)> = HashMap::new();
        for (i, srv) in servers.iter().enumerate() {
            for (wire, raw) in srv.wire_names().into_iter().zip(&srv.tools) {
                owner.insert(wire, (i, raw.clone()));
            }
        }
        let group_of: HashMap<&str, usize> = servers
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.as_str(), i))
            .collect();
        let deferred: Vec<DeferredToolStatus> = dispatch.deferred_tools();
        let defer_state: HashMap<&str, DeferState> = deferred
            .iter()
            .map(|d| (d.name.as_str(), d.state))
            .collect();

        let mut inv = Self::default();
        let defs = dispatch.tools();
        let advertised: HashSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        for d in &defs {
            let defer = defer_state.get(d.name.as_str()).copied();
            match owner.get(&d.name) {
                Some((i, raw)) => inv.servers.entry(*i).or_default().push(ToolRow {
                    name: raw.clone(),
                    desc: d.description.replace('\n', " "),
                    defer,
                    hidden: false,
                }),
                None => inv.built_in.push(ToolRow {
                    name: crate::tool::fmt::display_tool_name(&d.name),
                    desc: d.description.replace('\n', " "),
                    defer,
                    hidden: false,
                }),
            }
        }
        let mut hidden: Vec<&DeferredToolStatus> = deferred
            .iter()
            .filter(|d| !advertised.contains(d.name.as_str()))
            .collect();
        hidden.sort_by(|a, b| a.name.cmp(&b.name));
        for d in hidden {
            let row = |name: String| ToolRow {
                name,
                desc: d.description.replace('\n', " "),
                defer: Some(d.state),
                hidden: true,
            };
            match (owner.get(&d.name), group_of.get(d.group.as_str())) {
                (Some((i, raw)), _) => inv.servers.entry(*i).or_default().push(row(raw.clone())),
                // A deferred group the snapshot names but whose wire name it did not compose (a server not
                // connected): the group decides.
                (None, Some(i)) => inv
                    .servers
                    .entry(*i)
                    .or_default()
                    .push(row(raw_of(&d.name))),
                (None, None) => inv
                    .built_in
                    .push(row(crate::tool::fmt::display_tool_name(&d.name))),
            }
        }
        inv
    }

    fn of_server(&self, i: usize) -> &[ToolRow] {
        self.servers.get(&i).map_or(&[], Vec::as_slice)
    }
}

/// The tool part of a wire name `mcp__<segment>__<tool>`; the name itself when it is not one.
fn raw_of(wire: &str) -> String {
    wire.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map_or_else(|| wire.to_owned(), |(_, tool)| tool.to_owned())
}

/// `N tool(s)`, and the deferred tally when the source has deferred tools.
fn count(tools: &[ToolRow]) -> String {
    let n = tools.len();
    let mut text = format!("{n} tool{}", if n == 1 { "" } else { "s" });
    let deferred = tools.iter().filter(|t| t.defer.is_some()).count();
    if deferred > 0 {
        let loaded = tools
            .iter()
            .filter(|t| t.defer.is_some() && !t.hidden)
            .count();
        let _ = write!(text, " · deferred ({loaded}/{deferred} loaded)");
    }
    text
}

// ---------------------------------------------------------------- level 1: the list

/// How a server stands, as the list says it: the state word (coloured), then — for one that failed, is
/// down or waits for a login — the summary of why, X-77's words (`crate::mcp::explain`).
pub(crate) fn status_word(srv: &ServerStatus) -> String {
    match &srv.state {
        ServerState::Connecting => dim("connecting…"),
        ServerState::Connected { .. } => green("connected"),
        ServerState::Down { .. } => {
            let (summary, _) = srv.failure().unwrap_or_default();
            format!("{}{}", red("down"), dim(&format!(": {summary}")))
        }
        ServerState::Failed(_) => {
            let (summary, _) = srv.failure().unwrap_or_default();
            match summary.strip_prefix("not logged in: ") {
                Some(what) => format!("{}{}", yellow("needs login"), dim(&format!(": {what}"))),
                None => format!("{}{}", red("failed"), dim(&format!(": {summary}"))),
            }
        }
    }
}

/// The first level: one row per source — `built in`, then every server in config order — as
/// `<name>  <tool count>  <status>`, columns aligned; one key per row (`built in`, `server:<i>`) so the
/// cursor follows a row across a refresh; and the prompt line above them.
pub(crate) fn overview(dispatch: &dyn Dispatcher, servers: &[ServerStatus]) -> Refreshed {
    let inv = Inventory::of(dispatch, servers);
    let mut names = vec![BUILT_IN.to_owned()];
    let mut counts = vec![count(&inv.built_in)];
    let mut statuses = vec![green("available")];
    let mut keys = vec![BUILT_IN.to_owned()];
    for (i, srv) in servers.iter().enumerate() {
        names.push(srv.name.clone());
        counts.push(match srv.state {
            ServerState::Connected { .. } | ServerState::Down { .. } => count(inv.of_server(i)),
            _ => "—".to_owned(),
        });
        statuses.push(status_word(srv));
        keys.push(format!("server:{i}"));
    }
    let name_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let count_w = counts.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    let rows = names
        .iter()
        .zip(&counts)
        .zip(&statuses)
        .map(|((name, count), status)| {
            let pad_n = " ".repeat(name_w - name.chars().count());
            let pad_c = " ".repeat(count_w - count.chars().count());
            format!("{}{pad_n}  {}{pad_c}  {status}", bold(name), dim(count))
        })
        .collect();
    let tools: usize = inv.built_in.len() + inv.servers.values().map(Vec::len).sum::<usize>();
    let prompt = format!(
        "{} MCP server{} · {tools} tool{} · Enter: open · Esc: close",
        servers.len(),
        if servers.len() == 1 { "" } else { "s" },
        if tools == 1 { "" } else { "s" },
    );
    Refreshed {
        rows,
        keys,
        prompt: Some(prompt),
    }
}

// ---------------------------------------------------------------- level 2: a source's page

/// `label` padded to the page's label column, then `value`.
fn field(label: &str, value: &str) -> String {
    format!("{}{value}", dim(&format!("{label:<14}")))
}

/// A description on one line, cut to [`DESC_CHARS`] with an ellipsis.
fn short(desc: &str) -> String {
    let flat = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(DESC_CHARS) {
        Some((cut, _)) => format!("{}…", flat[..cut].trim_end()),
        None => flat,
    }
}

/// The tool list at the foot of a page: `tools (N)`, then one row per tool — name, the first
/// [`DESC_CHARS`] characters of its description, and the deferred state when it has one (a hidden one
/// dimmed whole).
fn tool_lines(tools: &[ToolRow]) -> Vec<String> {
    let mut lines = vec![bold(&format!("tools ({})", tools.len()))];
    if tools.is_empty() {
        lines.push(dim("  (none)"));
        return lines;
    }
    let w = tools
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0);
    for t in tools {
        let pad = " ".repeat(w - t.name.chars().count());
        let state = t
            .defer
            .map(|s| format!("  [{}]", s.as_str()))
            .unwrap_or_default();
        let desc = short(&t.desc);
        if t.hidden {
            lines.push(dim(&format!("  {}{pad}  {desc}{state}", t.name)));
        } else {
            lines.push(format!(
                "  {}{pad}  {}{}",
                t.name,
                dim(&desc),
                yellow(&state)
            ));
        }
    }
    lines
}

/// How the server authenticates, and where its login stands when it has one. A stdio server has no login
/// to speak of: `none`, as `iota mcp list` says it.
fn auth_line(srv: &ServerStatus, stdio: bool) -> String {
    if stdio {
        return "none".to_owned();
    }
    let mode = match srv.auth {
        AuthMode::Auto => "auto",
        AuthMode::Oauth => "oauth",
        AuthMode::None => "none",
    };
    match srv.login {
        Some(login) => format!("{mode} · {}", login.as_str()),
        None => mode.to_owned(),
    }
}

/// A server's page: what it is and how it stands, then its tools. For a server that failed or is down,
/// the WHOLE text of why — the summary, then its detail lines (the cause chain, the stderr tail); for a
/// live stdio server, the last lines it wrote to stderr.
pub(crate) fn server_page(
    dispatch: &dyn Dispatcher,
    servers: &[ServerStatus],
    i: usize,
    detail: Option<&ServerDetail>,
) -> Vec<String> {
    let Some(srv) = servers.get(i) else {
        return vec![dim("No such server.")];
    };
    let inv = Inventory::of(dispatch, servers);
    let mut lines = vec![field("status", &status_word(srv))];
    lines.push(field("endpoint", &srv.endpoint));
    if let Some(d) = detail {
        lines.push(field("transport", if d.stdio { "stdio" } else { "http" }));
    }
    lines.push(field(
        "auth",
        &auth_line(srv, detail.is_some_and(|d| d.stdio)),
    ));
    let prefix = srv.wire_prefix();
    if !prefix.is_empty() {
        lines.push(field("tool names", &format!("{prefix}<tool>")));
    }
    if let Some(d) = detail {
        lines.push(field("call timeout", &human_duration(d.call_timeout)));
    }
    if let Some((_, why)) = srv.failure() {
        if !why.is_empty() {
            lines.push(String::new());
            lines.push(bold("why"));
            lines.extend(why.iter().map(|l| dim(&format!("  {l}"))));
        }
    } else if let Some(d) = detail.filter(|d| !d.stderr.trim().is_empty()) {
        let tail: Vec<&str> = d.stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        let skip = tail.len().saturating_sub(STDERR_PAGE_LINES);
        lines.push(String::new());
        lines.push(bold(&format!(
            "stderr (last {} line{})",
            tail.len() - skip,
            if tail.len() - skip == 1 { "" } else { "s" }
        )));
        lines.extend(tail[skip..].iter().map(|l| dim(&format!("  {l}"))));
    }
    lines.push(String::new());
    lines.extend(tool_lines(inv.of_server(i)));
    lines
}

/// The `built in` page: what these tools are — the ones iota ships with, no server behind them, so no
/// endpoint and nothing to reconnect — and the list.
pub(crate) fn built_in_page(dispatch: &dyn Dispatcher, servers: &[ServerStatus]) -> Vec<String> {
    let inv = Inventory::of(dispatch, servers);
    let mut lines = vec![
        field("status", &green("available")),
        field(
            "source",
            "the tools iota ships with — no server behind them, so nothing to reconnect",
        ),
        String::new(),
    ];
    lines.extend(tool_lines(&inv.built_in));
    lines
}

// ---------------------------------------------------------------- reconnecting

/// Where one server's reconnect stands, for its page's prompt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reconnect {
    /// `Manager::reconnect` is running.
    Running,
    /// It connected, with this many tools.
    Done(usize),
    /// It did not: the summary of why (X-77's words).
    Failed(String),
}

/// The reconnects of one `/tools`, by server index; the background task writes, the page's refresh reads.
type Reconnects = Arc<Mutex<HashMap<usize, Reconnect>>>;

/// A server page's prompt line: the keys, and how the last reconnect from this `/tools` went.
pub(crate) fn page_prompt(can_reconnect: bool, reconnect: Option<&Reconnect>) -> String {
    match (can_reconnect, reconnect) {
        (false, _) => "Esc: back".to_owned(),
        (true, None) => "Enter: reconnect · Esc: back".to_owned(),
        (true, Some(Reconnect::Running)) => "Reconnecting… · Esc: back".to_owned(),
        (true, Some(Reconnect::Done(n))) => format!(
            "Reconnected · {n} tool{} · Enter: reconnect · Esc: back",
            if *n == 1 { "" } else { "s" }
        ),
        (true, Some(Reconnect::Failed(why))) => {
            format!("Reconnect failed: {why} · Enter: try again · Esc: back")
        }
    }
}

/// Reconnects server `i` in the background — the page stays open and shows it — and records the outcome,
/// in the transcript too: `MCP <name> reconnected · N tools`, or the startup notice's own line
/// (`⚠ MCP <name>: <summary> · details in /tools`). A reconnect already running is left to finish.
fn start_reconnect(
    manager: &Arc<Manager>,
    servers: &[ServerStatus],
    i: usize,
    reconnects: &Reconnects,
    tr: &Arc<Transcript>,
) {
    let Some(name) = servers.get(i).map(|s| s.name.clone()) else {
        return;
    };
    {
        let mut map = lock(reconnects);
        if map.get(&i) == Some(&Reconnect::Running) {
            return;
        }
        map.insert(i, Reconnect::Running);
    }
    let manager = Arc::clone(manager);
    let reconnects = Arc::clone(reconnects);
    let tr = Arc::clone(tr);
    tokio::spawn(async move {
        let outcome = match manager.reconnect(&name).await {
            Some(st) if st.connected() => {
                tr.notice(&format!(
                    "MCP {name} reconnected · {} tool{}",
                    st.tool_count,
                    if st.tool_count == 1 { "" } else { "s" }
                ));
                Reconnect::Done(st.tool_count)
            }
            Some(st) => {
                let (summary, detail) = st.failure().unwrap_or_default();
                tr.error(&crate::repl::render::mcpreport::notice_line(
                    &name,
                    summary,
                    !detail.is_empty(),
                ));
                Reconnect::Failed(summary.to_owned())
            }
            None => Reconnect::Failed(format!("no MCP server named {name:?} in this run")),
        };
        lock(&reconnects).insert(i, outcome);
    });
}

// ---------------------------------------------------------------- the command

/// Which row of the list a page belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    BuiltIn,
    Server(usize),
}

/// `/tools`: the list, and Enter on a row opens its page until Esc closes the list. A facade failure ends
/// the command.
pub(crate) async fn cmd_tools(repl: &Repl) {
    let hook = repl.handles.mcp.servers.clone();
    let snapshot = move || hook.as_ref().map(|f| f()).unwrap_or_default();
    let dispatch = Arc::clone(&repl.conv.dispatch);
    let manager = repl.handles.mcp.manager.clone();
    let reconnects: Reconnects = Arc::default();
    let mut cursor = 0;
    loop {
        let list = {
            let (dispatch, snapshot) = (Arc::clone(&dispatch), snapshot.clone());
            move || overview(&*dispatch, &snapshot())
        };
        let first = list();
        let panel = Panel::list("Tools", first.rows)
            .with_keys(first.keys)
            .with_cursor(cursor)
            .with_prompt(first.prompt.unwrap_or_default())
            .with_refresh(list);
        let spec = TabbedSpec {
            refresh_every_ms: REFRESH_EVERY_MS,
            panels: vec![panel],
            ..TabbedSpec::default()
        };
        let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await else {
            return;
        };
        if r.cancelled {
            return;
        }
        cursor = r.panels.first().map_or(0, |p| p.cursor);
        let source = match cursor {
            0 => Source::BuiltIn,
            n => Source::Server(n - 1),
        };
        // The page, until Esc: Enter reconnects a server (when there is a manager to do it) and reopens
        // the page; on `built in` it goes back like Esc.
        loop {
            let page = {
                let (dispatch, snapshot) = (Arc::clone(&dispatch), snapshot.clone());
                let (manager, reconnects) = (manager.clone(), Arc::clone(&reconnects));
                move || {
                    let servers = snapshot();
                    match source {
                        Source::BuiltIn => Refreshed {
                            rows: built_in_page(&*dispatch, &servers),
                            keys: Vec::new(),
                            prompt: Some(page_prompt(false, None)),
                        },
                        Source::Server(i) => {
                            let detail = manager.as_ref().and_then(|m| m.detail(i));
                            Refreshed {
                                rows: server_page(&*dispatch, &servers, i, detail.as_ref()),
                                keys: Vec::new(),
                                prompt: Some(page_prompt(
                                    manager.is_some(),
                                    lock(&reconnects).get(&i),
                                )),
                            }
                        }
                    }
                }
            };
            let first = page();
            let title = match source {
                Source::BuiltIn => BUILT_IN.to_owned(),
                Source::Server(i) => snapshot()
                    .get(i)
                    .map_or_else(String::new, |s| s.name.clone()),
            };
            let panel = Panel::view(title, first.rows)
                .with_wrap(true)
                .with_prompt(first.prompt.unwrap_or_default())
                .with_refresh(page);
            let spec = TabbedSpec {
                refresh_every_ms: REFRESH_EVERY_MS,
                panels: vec![panel],
                ..TabbedSpec::default()
            };
            let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await else {
                return;
            };
            match (r.cancelled, source, &manager) {
                (false, Source::Server(i), Some(manager)) => {
                    start_reconnect(manager, &snapshot(), i, &reconnects, &repl.handles.tr);
                }
                _ => break,
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use crate::BoxFuture;
    use crate::mcp::ServerDetail;
    use crate::mcp::config::AuthMode;
    use crate::provider::model::JsonObject;
    use crate::provider::model::ToolDef;
    use crate::text::ansi::strip_sgr;
    use crate::tool::context::RunCtx;
    use crate::tool::{ToolOutput, ToolResult};
    use pretty_assertions::assert_eq;

    use super::{DeferState, DeferredToolStatus, Dispatcher, Reconnect, ServerState, ServerStatus};

    /// Advertises `defs` and reports deferred state — the `/tools` view fixture.
    // Go: chat/toolstatus_test.go:13 deferStatusDispatcher
    pub(crate) struct DeferStatus {
        defs: Vec<ToolDef>,
        status: Vec<DeferredToolStatus>,
    }

    impl DeferStatus {
        pub(crate) fn new(
            defs: &[(&str, &str)],
            status: &[(&str, &str, &str, DeferState)],
        ) -> Self {
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

    fn server(name: &str, state: ServerState, tools: &[&str]) -> ServerStatus {
        ServerStatus {
            name: name.to_owned(),
            endpoint: format!("https://{name}.example/mcp"),
            tool_count: tools.len(),
            tools: tools.iter().map(|t| (*t).to_owned()).collect(),
            state,
            ..ServerStatus::default()
        }
    }

    fn connected(segment: &str) -> ServerState {
        ServerState::Connected {
            segment: segment.to_owned(),
        }
    }

    /// The four servers every list test reads: connected (with a deferred group), down, failed, waiting for
    /// a login — and one still connecting.
    fn fleet() -> (DeferStatus, Vec<ServerStatus>) {
        let d = DeferStatus::new(
            &[
                ("shell", "run a command"),
                ("read_file", "read a file"),
                ("mcp__seo__serp", "SERP lookup"),
                ("mcp__db__query", "run SQL"),
            ],
            &[
                ("mcp__seo__serp", "SERP lookup", "seo", DeferState::Loaded),
                (
                    "mcp__seo__keywords",
                    "keyword data",
                    "seo",
                    DeferState::Deferred,
                ),
            ],
        );
        let servers = vec![
            server("seo", connected("seo"), &["serp", "keywords"]),
            server(
                "db",
                ServerState::Down {
                    segment: "db".to_owned(),
                    reason: "exited (its last stderr line: \"killed\") — its tools will fail until it is \
                             reconnected from /tools\nstderr:\n  killed"
                        .to_owned(),
                },
                &["query"],
            ),
            server(
                "nb",
                ServerState::Failed(
                    "can't reach https://nb.example/mcp: connection refused — check the url, and that the \
                     server is running\nConnection refused (os error 61)"
                        .to_owned(),
                ),
                &[],
            ),
            server(
                "gh",
                ServerState::Failed("not logged in: run iota mcp login gh".to_owned()),
                &[],
            ),
            server("slow", ServerState::Connecting, &[]),
        ];
        (d, servers)
    }

    // X-79: the first level — `built in` first, one row per server in config order, its tool count and how
    // it stands in X-77's words; keys that follow the rows across a refresh.
    #[test]
    fn the_list_names_every_source_its_count_and_how_it_stands() {
        let (d, servers) = fleet();
        let r = super::overview(&d, &servers);
        assert_eq!(
            plain(&r.rows),
            vec![
                "built in  2 tools                          available",
                "seo       2 tools · deferred (1/2 loaded)  connected",
                "db        1 tool                           down: exited (its last stderr line: \"killed\") — its \
                 tools will fail until it is reconnected from /tools",
                "nb        —                                failed: can't reach https://nb.example/mcp: connection \
                 refused — check the url, and that the server is running",
                "gh        —                                needs login: run iota mcp login gh",
                "slow      —                                connecting…",
            ]
        );
        assert_eq!(
            r.keys,
            vec![
                "built in", "server:0", "server:1", "server:2", "server:3", "server:4"
            ]
        );
        assert_eq!(
            r.prompt.as_deref(),
            Some("5 MCP servers · 5 tools · Enter: open · Esc: close")
        );
    }

    #[test]
    fn with_no_server_the_list_is_built_in_alone() {
        let d = DeferStatus::new(&[("shell", "run")], &[]);
        let r = super::overview(&d, &[]);
        assert_eq!(plain(&r.rows), vec!["built in  1 tool  available"]);
        assert_eq!(
            r.prompt.as_deref(),
            Some("0 MCP servers · 1 tool · Enter: open · Esc: close")
        );
    }

    // X-79: a server's page — what it is and how it stands, the whole text of why, then its tools by their
    // own names, a hidden deferred one dimmed with its state.
    #[test]
    fn a_server_page_has_its_details_why_and_tools() {
        let (d, servers) = fleet();
        let detail = ServerDetail {
            stdio: false,
            call_timeout: Duration::from_mins(30),
            stderr: String::new(),
        };
        assert_eq!(
            plain(&super::server_page(&d, &servers, 1, Some(&detail))),
            vec![
                "status        down: exited (its last stderr line: \"killed\") — its tools will fail until it is \
                 reconnected from /tools",
                "endpoint      https://db.example/mcp",
                "transport     http",
                "auth          auto",
                "tool names    mcp__db__<tool>",
                "call timeout  30 minutes",
                "",
                "why",
                "  stderr:",
                "    killed",
                "",
                "tools (1)",
                "  query  run SQL",
            ]
        );
        let seo = plain(&super::server_page(&d, &servers, 0, None));
        assert_eq!(
            seo[seo.len() - 3..],
            [
                "tools (2)",
                "  serp      SERP lookup  [loaded]",
                "  keywords  keyword data  [deferred]",
            ]
        );
    }

    // A live stdio server's page shows the last lines it wrote to stderr.
    #[test]
    fn a_live_stdio_server_page_shows_its_stderr_tail() {
        let d = DeferStatus::new(&[("mcp__fs__read", "read")], &[]);
        let mut fs = server("fs", connected("fs"), &["read"]);
        fs.endpoint = "npx -y fs-server".to_owned();
        fs.auth = AuthMode::None;
        let stderr = (1..=12)
            .map(|n| format!("log {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let detail = ServerDetail {
            stdio: true,
            call_timeout: Duration::from_secs(90),
            stderr,
        };
        let page = plain(&super::server_page(&d, &[fs], 0, Some(&detail)));
        assert_eq!(
            page[1..6],
            [
                "endpoint      npx -y fs-server",
                "transport     stdio",
                "auth          none",
                "tool names    mcp__fs__<tool>",
                "call timeout  90 seconds",
            ]
        );
        let at = page
            .iter()
            .position(|l| l == "stderr (last 10 lines)")
            .expect("the tail");
        assert_eq!(page[at + 1], "  log 3");
        assert_eq!(page[at + 10], "  log 12");
    }

    // X-79: `built in` has no endpoint and nothing to reconnect: its page says what these tools are and
    // lists them.
    #[test]
    fn the_built_in_page_lists_what_ships_with_iota() {
        let (d, servers) = fleet();
        assert_eq!(
            plain(&super::built_in_page(&d, &servers)),
            vec![
                "status        available",
                "source        the tools iota ships with — no server behind them, so nothing to reconnect",
                "",
                "tools (2)",
                "  shell      run a command",
                "  read_file  read a file",
            ]
        );
    }

    // A long description is one line on the page, cut with an ellipsis; the model still gets all of it.
    #[test]
    fn a_page_cuts_a_long_description_to_one_line() {
        let long = format!("Ask the user.\n{}", "word ".repeat(40));
        let cut = super::short(&long);
        assert_eq!(cut.chars().count(), 101, "{cut}");
        assert!(cut.starts_with("Ask the user. word") && cut.ends_with('…'));
        assert_eq!(super::short("short one"), "short one");
    }

    // X-79: the page's prompt line carries the reconnect — offered, running, and how it went (X-77's words).
    #[test]
    fn the_page_prompt_says_how_a_reconnect_stands() {
        use super::page_prompt;
        assert_eq!(page_prompt(false, None), "Esc: back");
        assert_eq!(page_prompt(true, None), "Enter: reconnect · Esc: back");
        assert_eq!(
            page_prompt(true, Some(&Reconnect::Running)),
            "Reconnecting… · Esc: back"
        );
        assert_eq!(
            page_prompt(true, Some(&Reconnect::Done(3))),
            "Reconnected · 3 tools · Enter: reconnect · Esc: back"
        );
        assert_eq!(
            page_prompt(
                true,
                Some(&Reconnect::Failed(
                    "\"sh\" exited during startup: boom".to_owned()
                ))
            ),
            "Reconnect failed: \"sh\" exited during startup: boom · Enter: try again · Esc: back"
        );
    }
}
