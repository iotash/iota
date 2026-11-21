//! `/allow` — the chat's door to the grant store (docs/design/grants.md §5.2, DIVERGENCES X-111).
//!
//! The only door the shell has: a sandboxed command cannot be prompted mid-run, so a directory it must write
//! is made writable here, typed by the user. `/allow <dir>` grants the directory's REAL location and its
//! subtree for this session — the code jail and the shell sandbox read it at their next call, the harness's
//! `write roots:` line at the next turn (`refresh_harness`). `/allow <dir> project` grants it for the project,
//! in every session of it, in a run that HAS a project (§4.2); one without is refused, saying why
//! ([`no_project`]). The scope is the LAST word (docs/design/allow-completion.md §5): a line whose first word
//! is `project` is refused naming the line to type ([`scope`]). `/allow <server>:<tool> [project]` stops the question for one MCP tool,
//! `<server>:*` for every tool of the server, those it exposes later included: `<server>` is resolved against
//! the CONNECTED servers' names before the argument is read as a directory (a `--mcp` server is named by its
//! URL or `argv[0]`: [`named_server`]), and `<tool>` against its current tool list, so a typo is refused instead
//! of stored, and the grant is keyed by the server's identity (its configured name, or a `--mcp` server's whole flag value),
//! never the wire segment. `/allow <tool> [project]` — a bare word, neither a directory nor `<server>:<tool>` —
//! stops the question for a built-in tool, named by its wire name: one the run has and that asks about a call
//! bound to no location ([`asking`]), so a typo, or a tool that never asks, is refused naming those that do.
//!
//! The BARE `/allow` grants nothing: it opens a tabbed surface that lists and takes back. `Grants` is a
//! multi-select of every grant in effect, by level, and committing it takes each checked grant back from every
//! level that holds it; `Static`, shown only when there is one, is a read-only view of what the config and the
//! command line allow — not grants made here (the agent's `allow:`, `--allow`). With neither, no surface opens: one line says how to grant. This
//! file parses and words; every change is kept by the grants service, [`Door::apply`] (`repl::grants`), as the
//! dialog's and `/save`'s are.
//!
//! `/allow ` completes: [`grantable`] is the list of lines that would grant now, issued with the command table
//! (docs/design/allow-completion.md §3, §7).
//!
//! A message from the socket never reaches here: the loop's notice path runs no slash command
//! (`repl::inbound`), and the model has no way to type one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::app::{fs, paths, protected};
use crate::mcp::{ConnectedServer, Manager};
use crate::repl::grants::{Door, Footing, Unsaved, label, unsaved_session};
use crate::repl::run::Repl;
use crate::session::SessionError;
use crate::tool::Dispatcher;
use crate::tool::builtins::code::offers_root;
use crate::tool::grants::{self, Grants, Level, McpTool, ServerId, ToolKey};
use crate::ui::facade::{Panel, Suggestion, TabbedSpec};
use crate::ui::render::label::sgr_if;

/// What the user is told when the argument is not something `/allow` takes.
const EXPECTED: &str = "expected a directory (starting with /, ~ or .) or <server>:<tool>";

/// What the bare `/allow` says when nothing is granted — the hint printed in place of the surface, or the
/// `Grants` tab's prompt beside a `Static` tab — in a run with a project.
const NONE: &str = "No grants. /allow <dir> makes a directory writable for this session; /allow <dir> project for this project.";

/// [`NONE`] in a run with no project: there is no project level to point at.
const NONE_NO_PROJECT: &str =
    "No grants. /allow <dir> makes a directory writable for this session.";

/// The `Grants` tab's level column: `this session` and `this project` are both this wide, so every key starts at
/// the same column.
const LEVEL_W: usize = 12;

/// The `Static` tab's prompt: what its rows are, why none can be taken back here, and where to instead — the
/// agent's `allow:` when the run has a named agent.
fn static_prompt(agent: &str) -> String {
    let edit = if agent.is_empty() {
        "edit the config".to_owned()
    } else {
        format!("edit agents.{agent}.allow")
    };
    format!(
        "From the config and the command line: not grants made here, so none can be revoked here — {edit}, or drop the flag."
    )
}

/// A change made: the notice line, and — the session level changed but its bundle's meta could not be written —
/// why, said after it.
struct Done {
    line: String,
    unsaved: Option<SessionError>,
}

/// What a change [`Door::apply`] made left unsaved, for the notice: the session level's failed write, said after
/// it; a project file that could not be written refuses the change, said as `failed` (`could not save the grant`).
fn kept(r: Result<(), Unsaved>, failed: &str) -> Result<Option<SessionError>, String> {
    match r {
        Ok(()) => Ok(None),
        Err(Unsaved::Session(e)) => Ok(Some(e)),
        Err(Unsaved::Project(e)) => Err(format!("{failed} ({e})")),
        // `allow` refuses the level first, saying why ([`no_project`]).
        Err(Unsaved::NoProject) => Err(format!("{failed} (this run has no project)")),
    }
}

/// `/allow` with `arg`, the trimmed text after it (`""` opens the surface).
pub(crate) async fn cmd_allow(repl: &Repl, arg: &str) {
    if arg.is_empty() {
        panel(repl).await;
        return;
    }
    let servers = repl.handles.mcp.manager.as_deref();
    match allow(
        &repl.door(),
        &repl.handles.footing,
        home(repl),
        repl.conv.agent.enabled,
        servers,
        &*repl.conv.dispatch,
        arg,
    ) {
        Ok(done) => say(repl, done),
        Err(why) => repl.handles.tr.error(&format!("/allow: {why}")),
    }
}

/// The bare `/allow`: `Grants` (every grant in effect, checked rows meaning "take back"), then `Static` when
/// there is a static root ([`static_tab`]). With no grant and no static root it prints [`NONE`] and opens
/// nothing. A cancel, or a commit with nothing checked, changes nothing.
async fn panel(repl: &Repl) {
    let door = repl.door();
    let footing = &repl.handles.footing;
    let held = granted(&door.grants);
    let none = if footing.project.is_some() {
        NONE
    } else {
        NONE_NO_PROJECT
    };
    let fixed = static_tab(footing);
    if held.is_empty() && fixed.is_none() {
        repl.handles.tr.notice(none);
        return;
    }
    let color = crate::app::color::enabled();
    let rows = held.iter().map(|g| g.row(color)).collect();
    let mut grants_tab = Panel::multi("Grants", rows).with_search(true);
    if held.is_empty() {
        grants_tab = grants_tab.with_prompt(none);
    }
    let spec = TabbedSpec {
        panels: std::iter::once(grants_tab).chain(fixed).collect(),
        ..TabbedSpec::default()
    };
    // A facade failure is a cancel, as everywhere: a closed facade ends the loop at the next read.
    let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await else {
        return;
    };
    // `Static` is read-only: Enter there commits the surface, and nothing is done.
    if r.cancelled || r.focused != 0 {
        return;
    }
    let checked = r
        .panels
        .first()
        .map(|p| p.checked.clone())
        .unwrap_or_default();
    for done in revoke_checked(&door, &held, &checked) {
        match done {
            Ok(done) => say(repl, done),
            Err(why) => repl.handles.tr.error(&format!("/allow: {why}")),
        }
    }
}

/// The `Static` tab: a read-only view of what the config and the command line allow — not grants, so nothing
/// here is taken back — each row labelled with its source (the labels padded to one width): the agent's
/// `allow:`, then `--allow` (each its directories, then its tools). `None` when there is none:
/// the surface then has `Grants` alone.
fn static_tab(footing: &Footing) -> Option<Panel> {
    let shown = |p: &PathBuf| p.display().to_string();
    let of = |from: &'static str, set: &grants::GrantSet| -> Vec<(&'static str, String)> {
        let dirs = set.dirs.iter().map(|d| (from, shown(d)));
        dirs.chain(set.tools.iter().map(|k| (from, label(k))))
            .collect()
    };
    let rows: Vec<String> = of("allow:", &footing.config)
        .into_iter()
        .chain(of("--allow", &footing.flag))
        .map(|(from, what)| format!("{from:<7}  {what}"))
        .collect();
    (!rows.is_empty())
        .then(|| Panel::view("Static", rows).with_prompt(static_prompt(&footing.agent)))
}

/// The user's home directory: `~` and the protected table's home rows.
fn home(repl: &Repl) -> Option<&Path> {
    repl.conv.agent.home.as_deref()
}

/// The notice, then the session level's failed write, as every failed meta write is said.
fn say(repl: &Repl, done: Done) {
    repl.handles.tr.notice(&done.line);
    if let Some(e) = done.unsaved {
        repl.handles.tr.error(&unsaved_session(&e));
    }
}

/// The typed `arg` as an absolute, cleaned path: `~` is `home`, a relative one hangs off the project root.
/// `None` when `arg` is not spelled as a directory.
fn typed_path(footing: &Footing, home: Option<&Path>, arg: &str) -> Option<PathBuf> {
    if !arg.starts_with(['/', '~', '.']) {
        return None;
    }
    let p = crate::tool::expand_home(arg, home);
    Some(paths::clean(&footing.root.join(p)))
}

/// Grants the directory, the MCP tool or the built-in tool `arg` names for this session — or, with the last
/// word `project` ([`scope`]), for this project: the notice line, or why not. `workspace` is whether the run's
/// agent has one (`mode: agent` or `bot`); a run with no project refuses the scope whatever precedes it
/// ([`no_project`]). `servers` is the run's MCP manager, `None` in a run with no server; `tools` the run's
/// dispatcher. The target is read in this order: a connected server's name, a directory (a leading `/`, `~` or
/// `.`), `<server>:<tool>` (any other `:`), then a bare word, a built-in tool. A directory is never guessed:
/// when the whole of `arg`, scope word and all, is itself an existing directory (`~/my project` beside `~/my`),
/// the line is refused naming both spellings — a trailing `/` keeps it whole (`~/my project/`).
fn allow(
    door: &Door,
    footing: &Footing,
    home: Option<&Path>,
    workspace: bool,
    servers: Option<&Manager>,
    tools: &dyn Dispatcher,
    arg: &str,
) -> Result<Done, String> {
    let (level, target) = scope(arg)?;
    if level == Level::Project && footing.project.is_none() {
        return Err(no_project(footing, home, workspace));
    }
    let names = servers.map(Manager::connected_names).unwrap_or_default();
    if let Some((server, tool)) = named_server(target, &names) {
        return allow_tool(door, footing, servers, level, server, tool);
    }
    if typed_path(footing, home, target).is_some() {
        if target != arg && typed_path(footing, home, arg).is_some_and(|p| p.is_dir()) {
            let dir = target
                .strip_suffix('/')
                .filter(|d| !d.is_empty())
                .unwrap_or(target);
            return Err(format!(
                "\"{arg}\" is a directory too: write \"{arg}/\" for it, or \"{dir}/ {PROJECT}\" for {dir} in this project"
            ));
        }
        return allow_dir(door, footing, home, level, target);
    }
    if !target.contains(':') {
        return allow_builtin(door, footing, tools, level, target);
    }
    let (server, tool) = server_tool(target).ok_or_else(|| EXPECTED.to_owned())?;
    allow_tool(door, footing, servers, level, server, tool)
}

/// The level and the target of the trimmed `arg`: a last word `project` is the project level, the target what
/// precedes it; otherwise the session, the target all of `arg` (docs/design/allow-completion.md §5). A first
/// word `project` is refused naming the line to type — the scope word never starts a target, so `project` alone
/// and the old order, the scope before the target, grant nothing.
fn scope(arg: &str) -> Result<(Level, &str), String> {
    if let Some(rest) = arg.strip_prefix(PROJECT)
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        let rest = rest.trim_start();
        let target = if rest.is_empty() { "<target>" } else { rest };
        return Err(format!("the scope goes last: /allow {target} {PROJECT}"));
    }
    match arg.strip_suffix(PROJECT) {
        Some(target) if target.ends_with(char::is_whitespace) => {
            Ok((Level::Project, target.trim_end()))
        }
        _ => Ok((Level::Session, arg)),
    }
}

/// The scope word: the last word of a project grant.
const PROJECT: &str = "project";

/// Why a run with no project file has none (docs/design/grants.md §4.2), as `/allow … project` refuses: the
/// agent has no workspace; or its root is one no project stands on — the dialog's own rule for a directory
/// ([`offers_root`]); or there is no home to keep the file in.
fn no_project(footing: &Footing, home: Option<&Path>, workspace: bool) -> String {
    if !workspace {
        return "this run has no project (mode: chat); an agent with mode: agent has one"
            .to_owned();
    }
    let real = fs::physical(&footing.root).unwrap_or_else(|| footing.root.clone());
    if offers_root(home, &real) {
        "this run has no project (no home directory to keep its grants in)".to_owned()
    } else {
        format!(
            "this run has no project: {} is /, the home directory or an ancestor of it, which is never a project",
            real.display()
        )
    }
}

/// The built-in tools of `tools` a tool grant can answer, by wire name, in its order: those whose argument-less
/// probe asks ([`Dispatcher::requires_approval`]) — a tool that never asks is not among them.
fn asking(tools: &dyn Dispatcher) -> Vec<String> {
    tools
        .tools()
        .into_iter()
        .map(|t| t.name)
        .filter(|n| {
            matches!(tools.grant_key(n), ToolKey::Builtin(_)) && tools.requires_approval(n, None)
        })
        .collect()
}

/// The completion rows of `/allow ` (docs/design/allow-completion.md §3): one per line that would grant if it
/// were submitted now, computed with the command's own predicates. Per built-in that asks ([`asking`]), in the
/// dispatcher's order: its row (`/allow <tool>`, a session grant) unless a grant at either level already
/// covers it — [`grant_tool`]'s refusal — then, in a run with a project, its scope row (`/allow <tool>
/// project`, waiting behind `/allow <tool> `) unless the project level holds it. A session grant removes the
/// tool's row and keeps its scope row. Then each server of `servers` that `/allow` would take ([`server_rows`]),
/// in config order. The rows decide nothing: a submitted row is parsed like a typed line, and one that
/// outlived its truth is refused there.
pub(crate) fn grantable(
    door: &Door,
    tools: &dyn Dispatcher,
    servers: Option<&Manager>,
) -> Vec<Suggestion> {
    let rows = Rows {
        door,
        project: door
            .project
            .is_some()
            .then(|| (door.grants.snapshot(Level::Project), shown_root(door))),
    };
    let mut out = Vec::new();
    for name in asking(tools) {
        rows.target(
            &mut out,
            &ToolKey::Builtin(name.clone()),
            &format!("/allow {name}"),
            &name,
            "stop asking before it runs".to_owned(),
            "/allow ",
        );
    }
    if let Some(m) = servers {
        for name in m.connected_names() {
            if let [one] = m.connected_named(&name).as_slice() {
                out.extend(rows.server(&name, one));
            }
        }
    }
    out
}

/// What every row of [`grantable`] is computed against: the door, and — in a run with a project — the
/// project level's grants and the root its scope rows name.
struct Rows<'a> {
    door: &'a Door,
    project: Option<(grants::GrantSet, String)>,
}

impl Rows<'_> {
    /// The rows of one target, the line `line` granting `key`: its own row (labelled `label`, offered behind
    /// `after`) unless a grant at either level covers it, then its scope row unless the project level does.
    /// Whether its own row is there.
    fn target(
        &self,
        out: &mut Vec<Suggestion>,
        key: &ToolKey,
        line: &str,
        label: &str,
        desc: String,
        after: &str,
    ) -> bool {
        let own = !self.door.grants.allows(key);
        let fixed = self.door.grants.statics().allows(key);
        if own {
            out.push(Suggestion {
                value: line.to_owned(),
                label: label.to_owned(),
                desc,
                after: Some(after.to_owned()),
            });
        }
        if let Some((held, root)) = &self.project
            && !held.allows(key)
            && !fixed
        {
            out.push(Suggestion {
                value: format!("{line} {PROJECT}"),
                label: PROJECT.to_owned(),
                desc: format!("for this project ({root}), in every session"),
                after: Some(format!("{line} ")),
            });
        }
        own
    }

    /// The rows of the connected server `name` — one server under that name, as `/allow` requires: none when
    /// it is `auto_run` (its tools never ask), when the config's `allow:` holds its `*` (they run unasked
    /// already), or when its name cannot be typed back ([`typable`]). Its `*` row and
    /// then a row per tool that can be ([`typable`]), in the server's order, each followed by its scope row,
    /// all waiting behind `/allow <name>:`; and first the server's own row, the prefix, when any `*` or tool
    /// row is there to reach.
    fn server(&self, name: &str, server: &ConnectedServer) -> Vec<Suggestion> {
        let key = |tool| ToolKey::Mcp {
            server: server.id.clone(),
            tool,
        };
        if server.auto_run
            || self.door.grants.statics().allows(&key(McpTool::Any))
            || !typable(name)
        {
            return Vec::new();
        }
        let prefix = format!("/allow {name}:");
        let mut children = Vec::new();
        let any = self.target(
            &mut children,
            &key(McpTool::Any),
            &format!("{prefix}*"),
            "*",
            format!("every tool of {name}, those it exposes later included"),
            &prefix,
        );
        let mut count = 0;
        for tool in server.tools.iter().filter(|t| typable(&t.name)) {
            count += usize::from(self.target(
                &mut children,
                &key(McpTool::Named(tool.name.clone())),
                &format!("{prefix}{}", tool.name),
                &tool.name,
                cleaned(&tool.description),
                &prefix,
            ));
        }
        if !any && count == 0 {
            return children;
        }
        let mut rows = vec![Suggestion {
            value: prefix.clone(),
            label: format!("{name}:"),
            desc: format!(
                "MCP server · {count} tool{}",
                if count == 1 { "" } else { "s" }
            ),
            after: Some("/allow ".to_owned()),
        }];
        rows.extend(children);
        rows
    }
}

/// The tools the agent's `allow:` names that a connected server does not expose, as `(server, tool)` — the
/// late check of an MCP entry, which the load cannot make (docs/design/config-allow.md §3.3): read when a
/// server's tool list resolves, beside the completion rows. Such an entry grants nothing meanwhile.
pub(crate) fn unexposed(grants: &Grants, servers: &Manager) -> Vec<(String, String)> {
    grants
        .statics()
        .tools
        .iter()
        .filter_map(|k| {
            let ToolKey::Mcp {
                server: id @ ServerId::Config(name),
                tool: McpTool::Named(tool),
            } = k
            else {
                return None;
            };
            let connected = servers.connected_named(name);
            let server = connected.iter().find(|s| s.id == *id)?;
            (!server.tools.iter().any(|t| t.name == *tool)).then(|| (name.clone(), tool.clone()))
        })
        .collect()
}

/// Whether a server's or a tool's name can be a completed line the parse reads back: not empty, and no
/// whitespace or control character in it. One that cannot gets no row; it stays grantable from the dialog.
fn typable(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Server-written text as a row may show it: its ANSI sequences stripped and every other control character
/// dropped (the list draws its rows raw); whitespace stays, for the list to flatten.
fn cleaned(text: &str) -> String {
    crate::text::ansi::strip_ansi(text)
        .chars()
        .filter(|c| c.is_whitespace() || !c.is_control())
        .collect()
}

/// Grants the built-in tool `name` at `level`: the notice line, or why not — an unknown name, or a tool that
/// never asks, is refused naming those that do ([`asking`]).
fn allow_builtin(
    door: &Door,
    footing: &Footing,
    tools: &dyn Dispatcher,
    level: Level,
    name: &str,
) -> Result<Done, String> {
    let asks = asking(tools);
    if !asks.iter().any(|n| n == name) {
        let those = if asks.is_empty() {
            "no built-in tool of this run asks".to_owned()
        } else {
            format!("tools that ask: {}", asks.join(", "))
        };
        let known = tools.tools().iter().any(|t| t.name == name)
            && matches!(tools.grant_key(name), ToolKey::Builtin(_));
        return Err(if known {
            format!("{name} never asks ({those})")
        } else {
            format!("no built-in tool named \"{name}\" ({those})")
        });
    }
    grant_tool(
        door,
        footing,
        level,
        ToolKey::Builtin(name.to_owned()),
        name,
    )
}

/// Grants the directory `arg` (spelled as one: [`typed_path`]) at `level`: the notice line, or why not.
fn allow_dir(
    door: &Door,
    footing: &Footing,
    home: Option<&Path>,
    level: Level,
    arg: &str,
) -> Result<Done, String> {
    let Some(typed) = typed_path(footing, home, arg) else {
        return Err(EXPECTED.to_owned());
    };
    if !typed.is_dir() {
        return Err(format!("{arg} is not a directory"));
    }
    let real = fs::physical(&typed).ok_or_else(|| format!("cannot work out where {arg} is"))?;
    let shown = real.display();

    // The table beats every root: a protected directory is refused before "already writable" is asked.
    let root = &footing.root;
    let table = protected::table(root, &door.grants.roots(), home);
    if let Some(why) = protected::why_protected(&table, &real) {
        return Err(format!("{shown} is protected: {}", why.reason()));
    }
    if let Some(by) = writable_by(&door.grants, footing, level, &real) {
        return Err(format!("{shown} is already writable ({by})"));
    }

    let unsaved = kept(
        door.apply(level, |s| {
            s.add_dir(real.clone());
        }),
        "could not save the grant",
    )?;
    let mut line = match level {
        Level::Session => format!("✓ {shown} is writable for this session"),
        Level::Project => format!(
            "✓ {shown} is writable for this project ({})",
            shown_root(door)
        ),
    };
    if real != typed {
        let _ = write!(line, " ({arg} leads there)");
    }
    Ok(Done { line, unsaved })
}

/// The project root as a project grant's notice names it: its real location (`""` in a run with no project,
/// where no project grant is made).
fn shown_root(door: &Door) -> String {
    door.project.as_ref().map_or_else(String::new, |p| {
        fs::physical(&p.root)
            .unwrap_or_else(|| p.root.clone())
            .display()
            .to_string()
    })
}

/// `arg` as `<server>:<tool>` for a server of `names`, the longest one `arg` starts with, then `:` and a
/// non-empty tool. A `--mcp` server is named by its URL or its `argv[0]` (`parse_mcp_flag`), which may hold a
/// `:` and start with `/` or `.` — so the name is matched whole, BEFORE `arg` is read as a directory: a known
/// server's name wins over the directory spelling (`/allow ./srv.py:push` with `./srv.py` connected is its
/// tool), and a directory is a leading `/`, `~` or `.` that names no server.
fn named_server<'a>(arg: &'a str, names: &[String]) -> Option<(&'a str, &'a str)> {
    names
        .iter()
        .filter_map(|n| {
            let tool = arg.strip_prefix(n.as_str())?.strip_prefix(':')?;
            (!tool.is_empty()).then_some((arg.split_at(n.len()).0, tool))
        })
        .max_by_key(|(s, _)| s.len())
}

/// `<server>:<tool>` for a server no name of [`named_server`] matched, split at its LAST `:` (a URL's own `:`
/// stays in the server, so the refusal names what was typed), both halves non-empty; `None` for anything else.
fn server_tool(arg: &str) -> Option<(&str, &str)> {
    arg.rsplit_once(':')
        .filter(|(s, t)| !s.is_empty() && !t.is_empty())
}

/// Grants `server`'s `tool` (or, as `*`, every tool of it) at `level`: the notice line, or why not. The server
/// must be connected, under that name, exactly once — a name two `--mcp` servers share identifies neither —
/// and must not be `auto_run`; a named tool must be one it lists now.
fn allow_tool(
    door: &Door,
    footing: &Footing,
    servers: Option<&Manager>,
    level: Level,
    server: &str,
    tool: &str,
) -> Result<Done, String> {
    let found = servers
        .map(|m| m.connected_named(server))
        .unwrap_or_default();
    let target = match found.as_slice() {
        [] => {
            let names = servers.map(Manager::connected_names).unwrap_or_default();
            let connected = if names.is_empty() {
                String::new()
            } else {
                format!(" (connected: {})", names.join(", "))
            };
            return Err(format!(
                "no MCP server named \"{server}\" is connected{connected}"
            ));
        }
        [one] => one,
        many => {
            return Err(format!(
                "\"{server}\" names {} connected servers; add one to mcp_servers: under its own name to grant it",
                many.len()
            ));
        }
    };
    if target.auto_run {
        return Err(format!(
            "{server}'s tools already run without asking (auto_run)"
        ));
    }
    let tool = McpTool::parse(tool);
    if let McpTool::Named(t) = &tool
        && !target.tools.iter().any(|known| known.name == *t)
    {
        return Err(format!("server \"{server}\" has no tool \"{t}\""));
    }
    let key = ToolKey::Mcp {
        server: target.id.clone(),
        tool,
    };
    let what = match &key {
        ToolKey::Mcp {
            tool: McpTool::Any, ..
        } => format!("every tool of {server}"),
        _ => label(&key),
    };
    grant_tool(door, footing, level, key, &what)
}

/// Grants the tool `key` at `level`, the notice naming it `what`: the notice line, or why not — it is refused
/// when the static level already covers it — the config's `allow:` or `--allow` ([`Footing`] says which) —
/// when a project grant does, or (for a session grant) a session one.
fn grant_tool(
    door: &Door,
    footing: &Footing,
    level: Level,
    key: ToolKey,
    what: &str,
) -> Result<Done, String> {
    let shown = label(&key);
    if footing.config.allows(&key) {
        return Err(format!("{shown} already runs without asking (allow:)"));
    }
    if footing.flag.allows(&key) {
        return Err(format!("{shown} already runs without asking (--allow)"));
    }
    let held = |l: Level| door.grants.snapshot(l).allows(&key);
    if held(Level::Project) {
        return Err(format!(
            "{shown} already runs without asking (this project)"
        ));
    }
    if level == Level::Session && held(Level::Session) {
        return Err(format!(
            "{shown} already runs without asking (this session)"
        ));
    }
    let unsaved = kept(
        door.apply(level, |s| {
            s.add_tool(key);
        }),
        "could not save the grant",
    )?;
    let line = match level {
        Level::Session => format!("✓ {what} runs without asking in this session"),
        Level::Project => format!(
            "✓ {what} runs without asking in this project ({})",
            shown_root(door)
        ),
    };
    Ok(Done { line, unsaved })
}

/// What already makes the real directory `real` writable for a grant at `level`, if anything: the project
/// root, a live directory of the agent's `allow:` or of `--allow`, or a grant that
/// covers it — of this project at either
/// level, of this session only for another session grant (a project grant is what a session grant becomes when
/// it should last).
fn writable_by(
    grants: &Grants,
    footing: &Footing,
    level: Level,
    real: &Path,
) -> Option<&'static str> {
    let under = |root: &Path| fs::physical(root).is_some_and(|r| paths::within(&r, real));
    if under(&footing.root) {
        return Some("the project root");
    }
    if footing
        .config
        .dirs
        .iter()
        .any(|d| grants::is_live(d) && paths::within(d, real))
    {
        return Some("allow:");
    }
    if footing
        .flag
        .dirs
        .iter()
        .any(|d| grants::is_live(d) && paths::within(d, real))
    {
        return Some("--allow");
    }
    let covers = |l: Level| {
        grants
            .snapshot(l)
            .dirs
            .iter()
            .any(|d| grants::is_live(d) && paths::within(d, real))
    };
    if covers(Level::Project) {
        return Some("this project");
    }
    (level == Level::Session && covers(Level::Session)).then_some("this session")
}

/// The levels a revocation took a grant from, and what keeping the session level's left unsaved.
struct Revoked {
    from: Vec<&'static str>,
    unsaved: Option<SessionError>,
}

impl Revoked {
    /// `✓ revoked <what> (<levels>)`; `None` when no level held it.
    fn said(self, what: &str) -> Option<Done> {
        (!self.from.is_empty()).then(|| Done {
            line: format!("✓ revoked {what} ({})", self.from.join(", ")),
            unsaved: self.unsaved,
        })
    }
}

/// Applies `take` at each level `held` says holds the grant — the project's file first; one that cannot be
/// written keeps the grant, at both levels.
fn revoke_where(
    door: &Door,
    held: impl Fn(&grants::GrantSet) -> bool,
    take: impl Fn(&mut grants::GrantSet),
) -> Result<Revoked, String> {
    let mut out = Revoked {
        from: Vec::new(),
        unsaved: None,
    };
    if held(&door.grants.snapshot(Level::Project)) {
        kept(
            door.apply(Level::Project, &take),
            "could not save the revocation",
        )?;
        out.from.push("this project");
    }
    if held(&door.grants.snapshot(Level::Session)) {
        out.unsaved = kept(
            door.apply(Level::Session, &take),
            "could not save the revocation",
        )?;
        out.from.insert(0, "this session");
    }
    Ok(out)
}

/// One grant in effect, as the `Grants` tab lists it.
struct Granted {
    /// The level that holds it.
    level: Level,
    /// The directory, or the tool.
    what: Grant,
}

/// What a [`Granted`] row grants.
#[derive(PartialEq)]
enum Grant {
    Dir(PathBuf),
    Tool(ToolKey),
}

impl Granted {
    /// The row: its level in a column of one width, then the key — the directory or the tool — then its mark:
    /// `(stale)` for a directory that no longer leads anywhere, `(--mcp <spec>)` for a `--mcp` server's tool (the
    /// whole flag value it is keyed by). With `color` the level and the `--mcp` mark are faint and `(stale)` is
    /// yellow (a warning); the key is plain, so the surface's bold on the current row lands on it. Without
    /// `color` the row is the same text with no escape: search and copy read the plain text either way.
    fn row(&self, color: bool) -> String {
        let level = match self.level {
            Level::Session => "this session",
            Level::Project => "this project",
        };
        let (key, mark) = match &self.what {
            Grant::Dir(d) => {
                let mark = (!grants::is_live(d)).then(|| sgr_if(color, "33", "(stale)"));
                (d.display().to_string(), mark)
            }
            Grant::Tool(key) => {
                let mark = match key {
                    ToolKey::Mcp {
                        server: ServerId::Flag(spec),
                        ..
                    } => Some(sgr_if(color, "2", &format!("(--mcp {spec})"))),
                    _ => None,
                };
                (label(key), mark)
            }
        };
        let level = sgr_if(color, "2", &format!("{level:<LEVEL_W$}"));
        match mark {
            Some(mark) => format!("{level}  {key} {mark}"),
            None => format!("{level}  {key}"),
        }
    }

    /// What a revocation's notice names.
    fn shown(&self) -> String {
        match &self.what {
            Grant::Dir(d) => d.display().to_string(),
            Grant::Tool(key) => label(key),
        }
    }
}

/// Every grant in effect: this session's, then this project's, each level's directories before its tools.
fn granted(grants: &Grants) -> Vec<Granted> {
    let mut out = Vec::new();
    for level in [Level::Session, Level::Project] {
        let set = grants.snapshot(level);
        let dirs = set.dirs.into_iter().map(Grant::Dir);
        let tools = set.tools.into_iter().map(Grant::Tool);
        out.extend(dirs.chain(tools).map(|what| Granted { level, what }));
    }
    out
}

/// Takes back the grants of the `checked` rows of `held`, each from every level that holds it — a directory
/// checked once at either level is gone at both — with one notice line, or why not, per grant.
fn revoke_checked(door: &Door, held: &[Granted], checked: &[usize]) -> Vec<Result<Done, String>> {
    let mut picked: Vec<&Granted> = Vec::new();
    for g in checked.iter().filter_map(|&i| held.get(i)) {
        if !picked.iter().any(|p| p.what == g.what) {
            picked.push(g);
        }
    }
    picked
        .into_iter()
        .filter_map(|g| {
            let r = match &g.what {
                Grant::Dir(dir) => revoke_where(
                    door,
                    |s| s.dirs.contains(dir),
                    |s| s.dirs.retain(|d| d != dir),
                ),
                Grant::Tool(key) => revoke_where(
                    door,
                    |s| s.tools.contains(key),
                    |s| s.tools.retain(|k| k != key),
                ),
            };
            r.map(|from| from.said(&g.shown())).transpose()
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{
        Footing, Grants, Level, ToolKey, granted, named_server, revoke_checked, server_tool,
    };
    use crate::repl::grants::{Door, ProjectFile};
    use crate::session::SessionStore;
    use crate::tool::grants::{self, McpTool, ServerId};
    use crate::ui::facade::PanelBody;

    /// A temp dir holding `proj/` (the project, a repository) and `out/` beside it; their real locations.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(t.path().join("proj/.git/hooks")).expect("proj");
        std::fs::create_dir_all(t.path().join("out/sub")).expect("out");
        let root = crate::app::canonical(&t.path().join("proj")).expect("real proj");
        let out = crate::app::canonical(&t.path().join("out")).expect("real out");
        (t, root, out)
    }

    /// A new directory `name` in the temp dir, physical, for a test to grant and then make stale with
    /// [`make_stale`].
    fn stale_to_be(t: &tempfile::TempDir, name: &str) -> PathBuf {
        let dir = crate::app::canonical(t.path())
            .expect("real temp")
            .join(name);
        std::fs::create_dir(&dir).expect("the directory");
        dir
    }

    /// Makes the granted directory `dir` genuinely stale, as `code::tests::a_directory_replaced_by_a_link_is_stale`
    /// does: moves it away and puts a link to another directory of the temp dir in its place, so its physical
    /// location is no longer the path the grant names. A path that was never created is NOT stale — its missing
    /// tail is kept as it is — and only read as stale on macOS, where the temp dir is under `/var`, a link to
    /// `/private/var`; on Linux `/tmp` is physical and such a grant is live.
    fn make_stale(dir: &Path) {
        let elsewhere = dir.with_file_name("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        std::fs::rename(dir, dir.with_file_name("moved")).expect("move away");
        std::os::unix::fs::symlink(&elsewhere, dir).expect("link in its place");
    }

    /// A chat's grants over `root`, its project file in the temp dir, and no bundle.
    struct Chat {
        door: Door,
        footing: Footing,
        tools: crate::tool::Registry,
        /// The user's home, `None` unless a test gives one.
        home: Option<PathBuf>,
        /// Whether the agent has a workspace (`mode: agent`): [`Self::new`]'s does.
        workspace: bool,
        /// The run's MCP servers: none unless a test gives some ([`Self::with_servers`]).
        servers: Option<Arc<crate::mcp::Manager>>,
    }

    impl Chat {
        fn new(t: &tempfile::TempDir, root: &std::path::Path) -> Self {
            let project = ProjectFile {
                store: SessionStore::new(t.path().join("sessions")),
                root: root.to_path_buf(),
            };
            Self {
                door: Door {
                    grants: Arc::default(),
                    writer: Arc::default(),
                    project: Some(project.clone()),
                },
                footing: Footing {
                    root: root.to_path_buf(),
                    project: Some(project),
                    config: grants::GrantSet::default(),
                    flag: grants::GrantSet::default(),
                    agent: "coder".to_owned(),
                },
                tools: tools(),
                home: None,
                workspace: true,
                servers: None,
            }
        }

        /// The same chat with the agent's `allow:` resolved to `config`: the store's static level beside the
        /// footing's `--allow`, and the footing's.
        fn with_config(mut self, config: grants::GrantSet) -> Self {
            self.footing.config = config;
            self.fix()
        }

        /// The same chat with `--allow` resolved to `flag`: the static level after the footing's `allow:`, and
        /// the footing's.
        fn with_flag(mut self, flag: grants::GrantSet) -> Self {
            self.footing.flag = flag;
            self.fix()
        }

        /// The store's static level as the assembly builds it: the footing's `allow:`, then its `--allow`.
        fn fix(mut self) -> Self {
            let mut fixed = self.footing.config.clone();
            fixed.dirs.extend(self.footing.flag.dirs.iter().cloned());
            fixed.tools.extend(self.footing.flag.tools.iter().cloned());
            self.door.grants = Arc::new(Grants::with_static(fixed));
            self
        }

        /// The same chat with the MCP servers `servers`.
        fn with_servers(mut self, servers: Arc<crate::mcp::Manager>) -> Self {
            self.servers = Some(servers);
            self
        }

        /// The same chat with no project, as assembly leaves a run that has none: `workspace` false is a
        /// `mode: chat` agent, true an agent whose root (under the home `home`) no project stands on.
        fn without_project(mut self, workspace: bool, home: Option<PathBuf>) -> Self {
            self.door.project = None;
            self.footing.project = None;
            self.workspace = workspace;
            self.home = home;
            self
        }

        fn g(&self) -> &Grants {
            &self.door.grants
        }

        fn pf(&self) -> &ProjectFile {
            self.door.project.as_ref().expect("a project")
        }

        /// `/allow` in a run with the chat's MCP servers and the built-ins of [`tools`].
        fn allow(&self, arg: &str) -> Result<String, String> {
            super::allow(
                &self.door,
                &self.footing,
                self.home.as_deref(),
                self.workspace,
                self.servers.as_deref(),
                &self.tools,
                arg,
            )
            .map(|d| d.line)
        }

        /// The `Static` tab's title, prompt and rows; `None` when it is not shown.
        fn fixed(&self) -> Option<(String, String, Vec<String>)> {
            let p = super::static_tab(&self.footing)?;
            let PanelBody::View(v) = p.body else {
                panic!("the Static tab is not a view");
            };
            Some((p.title, p.prompt, v.lines))
        }

        /// The `Grants` tab's rows now.
        fn rows(&self) -> Vec<String> {
            granted(self.g()).iter().map(|g| g.row(false)).collect()
        }

        /// Commits the `Grants` tab with the rows `checked` (indices into [`Self::rows`]) checked.
        fn revoke(&self, checked: &[usize]) -> Vec<Result<String, String>> {
            let held = granted(self.g());
            revoke_checked(&self.door, &held, checked)
                .into_iter()
                .map(|r| r.map(|d| d.line))
                .collect()
        }
    }

    /// A built-in named `.0` whose argument-less probe asks when `.1`.
    struct Stub(&'static str, bool);

    impl crate::tool::Tool for Stub {
        fn def(&self) -> crate::provider::model::ToolDef {
            crate::provider::model::ToolDef {
                name: self.0.to_owned(),
                description: String::new(),
                input_schema: None,
                deferred: false,
            }
        }

        fn call<'a>(
            &'a self,
            _cx: &'a crate::tool::context::RunCtx,
            _args: &'a crate::provider::model::JsonObject,
        ) -> crate::BoxFuture<'a, crate::tool::ToolResult> {
            unreachable!("/allow never calls a tool")
        }

        fn requires_approval(&self, _args: Option<&crate::provider::model::JsonObject>) -> bool {
            self.1
        }
    }

    /// The built-ins: `shell` and `write_file` ask, `read_file` never does.
    fn tools() -> crate::tool::Registry {
        let mut reg = crate::tool::Registry::default();
        for (name, asks) in [("shell", true), ("read_file", false), ("write_file", true)] {
            reg.add(Arc::new(Stub(name, asks)));
        }
        reg
    }

    /// A relative argument is read against the project root; the protected table beats "already writable";
    /// what a root or a grant already covers is refused saying which; a granted directory reads back.
    #[test]
    fn allow_reads_relative_against_the_root_and_refuses_what_is_covered() {
        let (t, root, out) = fixture();
        let added = t.path().join("added");
        std::fs::create_dir(&added).unwrap();
        let added = crate::app::canonical(&added).unwrap();
        let c = Chat::new(&t, &root)
            .with_config(grants::GrantSet {
                dirs: vec![out.join("sub")],
                tools: Vec::new(),
            })
            .with_flag(grants::GrantSet {
                dirs: vec![added.clone()],
                tools: Vec::new(),
            });
        assert_eq!(
            c.allow("./.git/hooks").unwrap_err(),
            format!(
                "{} is protected: git executes the files in .git/hooks on the user's next git command",
                root.join(".git/hooks").display()
            )
        );
        assert_eq!(
            c.allow(".").unwrap_err(),
            format!("{} is already writable (the project root)", root.display())
        );
        let sub = out.join("sub");
        assert_eq!(
            c.allow(sub.to_str().unwrap()).unwrap_err(),
            format!("{} is already writable (allow:)", sub.display())
        );
        assert_eq!(
            c.allow(added.to_str().unwrap()).unwrap_err(),
            format!("{} is already writable (--allow)", added.display())
        );
        assert_eq!(
            c.allow("../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            c.allow("../out").unwrap_err(),
            format!("{} is already writable (this session)", out.display())
        );
        assert_eq!(c.g().live_dirs(), std::slice::from_ref(&out));
        assert_eq!(
            c.revoke(&[0]),
            [Ok(format!("✓ revoked {} (this session)", out.display()))]
        );
        assert!(c.g().live_dirs().is_empty());
    }

    /// `/allow … project` needs a project (docs/design/grants.md §4.2). A `mode: chat` run has none: the scope
    /// is refused whatever precedes it — a directory, a built-in tool, an MCP tool — saying why and what has one,
    /// nothing is written under the sessions root, and the session level works as ever. The same line in a
    /// `mode: agent` run is granted.
    #[test]
    fn allow_project_is_refused_in_chat_mode_and_granted_in_agent_mode() {
        const WHY: &str = "this run has no project (mode: chat); an agent with mode: agent has one";
        let (t, root, out) = fixture();
        let chat = Chat::new(&t, &root).without_project(false, None);
        for arg in ["../out project", "shell project", "srv:push project"] {
            assert_eq!(chat.allow(arg).unwrap_err(), WHY, "{arg}");
        }
        assert!(chat.g().snapshot(Level::Project).is_empty());
        assert!(!t.path().join("sessions").exists());
        assert_eq!(
            chat.allow("../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(chat.rows(), [format!("this session  {}", out.display())]);
        assert_eq!(
            chat.revoke(&[0]),
            [Ok(format!("✓ revoked {} (this session)", out.display()))]
        );

        let agent = Chat::new(&t, &root);
        assert_eq!(
            agent.allow("../out project").unwrap(),
            format!(
                "✓ {} is writable for this project ({})",
                out.display(),
                root.display()
            )
        );
        assert_eq!(
            agent.pf().store.project_grants(&root).unwrap().dirs,
            std::slice::from_ref(&out)
        );
    }

    /// An agent whose root is the home directory, or an ancestor of it, has no project either — the rule the
    /// dialog keeps for a directory (`repl::turn::tools::tests::the_dialog_never_offers_the_home_directory`),
    /// by the same predicate: `/allow … project` is refused naming the root, and nothing is written. Without a
    /// home there is nowhere to keep a project's grants, and the refusal says that instead.
    #[test]
    fn allow_project_is_refused_at_the_home_directory_or_above() {
        let (t, root, out) = fixture();
        for home in [root.clone(), root.join("users/me")] {
            let c = Chat::new(&t, &root).without_project(true, Some(home.clone()));
            for arg in ["../out project", "shell project"] {
                assert_eq!(
                    c.allow(arg).unwrap_err(),
                    format!(
                        "this run has no project: {} is /, the home directory or an ancestor of it, which is never a project",
                        root.display()
                    ),
                    "{arg} with the home at {}",
                    home.display()
                );
            }
            assert!(!t.path().join("sessions").exists());
            assert_eq!(
                c.allow("../out").unwrap(),
                format!("✓ {} is writable for this session", out.display())
            );
        }
        let c = Chat::new(&t, &root).without_project(true, None);
        assert_eq!(
            c.allow("../out project").unwrap_err(),
            "this run has no project (no home directory to keep its grants in)"
        );
    }

    /// `project` is the scope word: the grant goes to the project's file first, then memory. A project grant covers a
    /// session one asked for later, but a session grant does not stop a project one; committing the `Grants` tab
    /// takes a checked directory from every level that holds it, and the file with it.
    #[test]
    fn allow_project_reaches_the_file_and_revoke_every_level() {
        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root);
        let (g, pf) = (c.g(), c.pf());
        assert_eq!(
            c.allow("../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            c.allow("../out project").unwrap(),
            format!(
                "✓ {} is writable for this project ({})",
                out.display(),
                root.display()
            )
        );
        assert_eq!(
            pf.store.project_grants(&root).unwrap().dirs,
            std::slice::from_ref(&out)
        );
        assert_eq!(g.snapshot(Level::Project).dirs, std::slice::from_ref(&out));
        assert_eq!(
            c.allow("../out/sub project").unwrap_err(),
            format!(
                "{} is already writable (this project)",
                out.join("sub").display()
            )
        );
        assert_eq!(
            c.allow("../out/sub").unwrap_err(),
            format!(
                "{} is already writable (this project)",
                out.join("sub").display()
            )
        );
        assert_eq!(
            c.rows(),
            [
                format!("this session  {}", out.display()),
                format!("this project  {}", out.display()),
            ]
        );
        assert_eq!(
            c.revoke(&[1]),
            [Ok(format!(
                "✓ revoked {} (this session, this project)",
                out.display()
            ))]
        );
        assert!(g.live_dirs().is_empty());
        assert!(pf.store.project_grants(&root).unwrap().is_empty());
        assert!(c.rows().is_empty());

        // A file that cannot be read is never replaced: the grant is refused, saying why.
        let path = pf.store.grants_path(&root);
        std::fs::write(&path, "{broken").unwrap();
        let err = c.allow("../out project").unwrap_err();
        assert!(
            err.starts_with(&format!(
                "could not save the grant (cannot read {}",
                path.display()
            )),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
        assert!(g.live_dirs().is_empty());
    }

    /// A server's name is matched whole and FIRST: a URL or a path-like `--mcp` name keeps its `:` and wins over
    /// the directory spelling; the longest name wins; an unknown one splits at its last `:` (the refusal names
    /// what was typed).
    #[test]
    fn a_server_name_is_matched_whole_before_the_directory_test() {
        let names = [
            "https://h:8443/mcp".to_owned(),
            "./srv.py".to_owned(),
            "gh".to_owned(),
            "gh:x".to_owned(),
        ];
        let m = |arg| named_server(arg, &names);
        assert_eq!(
            m("https://h:8443/mcp:push"),
            Some(("https://h:8443/mcp", "push"))
        );
        assert_eq!(m("./srv.py:*"), Some(("./srv.py", "*")));
        assert_eq!(m("gh:x:y"), Some(("gh:x", "y")));
        assert_eq!(m("gh:echo"), Some(("gh", "echo")));
        assert_eq!(m("gh:"), None);
        assert_eq!(m("./srv.py"), None);
        assert_eq!(m("./out"), None);
        assert_eq!(
            server_tool("https://other/mcp:push"),
            Some(("https://other/mcp", "push"))
        );
        assert_eq!(server_tool("github"), None);
    }

    /// The `Grants` tab: every grant at both levels, each row naming its level, a stale directory marked and a
    /// `--mcp` grant naming its flag value; committing revokes exactly the checked rows — each at every level
    /// that holds it, a grant checked twice said once — and leaves the unchecked ones.
    #[test]
    fn the_grants_tab_revokes_exactly_the_checked_rows_at_both_levels() {
        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root);
        let (g, pf) = (c.g(), c.pf());
        assert!(c.rows().is_empty());
        let gone = stale_to_be(&t, "gone");
        let star = ToolKey::Mcp {
            server: ServerId::Config("github".to_owned()),
            tool: McpTool::Any,
        };
        let flag = ToolKey::Mcp {
            server: ServerId::Flag("npx -y @acme/mcp".to_owned()),
            tool: McpTool::Named("push".to_owned()),
        };
        g.update(Level::Session, |s| {
            s.add_dir(out.clone());
            s.add_dir(gone.clone());
            s.add_tool(star.clone());
        });
        make_stale(&gone);
        c.allow("../out/sub project").unwrap();
        c.door
            .apply(Level::Project, |s| {
                s.add_tool(star.clone());
                s.add_tool(flag.clone());
            })
            .unwrap();
        c.allow("shell project").unwrap();
        assert_eq!(
            c.rows(),
            [
                format!("this session  {}", out.display()),
                format!("this session  {} (stale)", gone.display()),
                "this session  github:*".to_owned(),
                format!("this project  {}", out.join("sub").display()),
                "this project  github:*".to_owned(),
                "this project  npx:push (--mcp npx -y @acme/mcp)".to_owned(),
                "this project  shell".to_owned(),
            ]
        );
        // The stale directory, `github:*` (checked at both of its rows), the project's directory and `shell`.
        assert_eq!(
            c.revoke(&[1, 2, 3, 4, 6]),
            [
                Ok(format!("✓ revoked {} (this session)", gone.display())),
                Ok("✓ revoked github:* (this session, this project)".to_owned()),
                Ok(format!(
                    "✓ revoked {} (this project)",
                    out.join("sub").display()
                )),
                Ok("✓ revoked shell (this project)".to_owned()),
            ]
        );
        assert_eq!(
            c.rows(),
            [
                format!("this session  {}", out.display()),
                "this project  npx:push (--mcp npx -y @acme/mcp)".to_owned(),
            ]
        );
        assert_eq!(
            pf.store.project_grants(&root).unwrap().tools,
            std::slice::from_ref(&flag)
        );
        assert!(c.revoke(&[]).is_empty(), "nothing checked revokes nothing");
    }

    /// The `Grants` tab's rows by style alone: the level faint in its column, the key plain, the `--mcp` mark
    /// faint and `(stale)` yellow. Stripped of its escapes a row is the plain row exactly, and the plain row (what
    /// `NO_COLOR` gets) carries no escape. On the surface the current row keeps `▸`, its tint and the bold over
    /// its label, and a checked row its `[x]`.
    #[test]
    fn the_grants_rows_set_the_level_the_key_and_the_mark_apart() {
        use crate::text::ansi::strip_sgr;
        use crate::text::width::str_width;
        use crate::ui::facade::Panel;
        use crate::ui::render::theme::{BOLD, CYAN, RESET};
        use crate::ui::testutil::{Surf, ch};

        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root);
        let gone = stale_to_be(&t, "gone");
        let flag = ToolKey::Mcp {
            server: ServerId::Flag("npx -y @acme/mcp".to_owned()),
            tool: McpTool::Named("push".to_owned()),
        };
        c.g().update(Level::Session, |s| {
            s.add_dir(out.clone());
            s.add_dir(gone.clone());
        });
        make_stale(&gone);
        c.door
            .apply(Level::Project, |s| {
                s.add_tool(flag.clone());
            })
            .unwrap();
        let held = granted(c.g());
        let styled: Vec<String> = held.iter().map(|g| g.row(true)).collect();
        let (dim, amber, off) = ("\x1b[2m", "\x1b[33m", "\x1b[0m");
        assert_eq!(
            styled,
            [
                format!("{dim}this session{off}  {}", out.display()),
                format!(
                    "{dim}this session{off}  {} {amber}(stale){off}",
                    gone.display()
                ),
                format!("{dim}this project{off}  npx:push {dim}(--mcp npx -y @acme/mcp){off}"),
            ]
        );
        let plain = c.rows();
        assert!(plain.iter().all(|r| !r.contains('\x1b')), "{plain:?}");
        assert_eq!(
            styled.iter().map(|r| strip_sgr(r)).collect::<Vec<_>>(),
            plain
        );

        // Wide enough that no temp path is cut.
        let mut s = Surf::sized(vec![Panel::multi("Grants", styled)], 200, 0);
        s.tap(ch(' '));
        let rows = s.rows();
        let (bg, b) = ("\x1b[48;5;237m", BOLD);
        let first = format!("this session  {}", out.display());
        assert_eq!(
            rows[1],
            format!(
                "{bg}{CYAN}{bg}▸\x1b[39m{bg} \x1b[1;32m{bg}[x]\x1b[22;39m{bg} {b}{bg}{dim}{bg}{b}{bg}this session\
                 {off}{bg}{b}{bg}  {}\x1b[22m{bg}{}{RESET}",
                out.display(),
                " ".repeat(200 - 6 - str_width(&first))
            ),
            "the current, checked row"
        );
        assert_eq!(
            rows[2],
            format!(
                "  \x1b[2m[ ]\x1b[22m {dim}this session{off}  {} {amber}(stale){off}",
                gone.display()
            )
        );
    }

    /// A bare word is a built-in tool: a last word `project` grants it in the project's file by its wire name, a session grant
    /// after it is refused as covered; without `project` it is the session's. An unknown name and a tool that
    /// never asks are refused, naming the tools that do; nothing is stored for them.
    #[test]
    fn allow_grants_a_builtin_tool_and_refuses_one_that_never_asks() {
        let (t, root, _out) = fixture();
        let c = Chat::new(&t, &root);
        let (g, pf) = (c.g(), c.pf());
        let shell = ToolKey::Builtin("shell".to_owned());
        assert_eq!(
            c.allow("shell project").unwrap(),
            format!(
                "✓ shell runs without asking in this project ({})",
                root.display()
            )
        );
        assert_eq!(
            pf.store.project_grants(&root).unwrap().tools,
            std::slice::from_ref(&shell)
        );
        assert_eq!(
            c.allow("shell").unwrap_err(),
            "shell already runs without asking (this project)"
        );
        assert_eq!(
            c.allow("write_file").unwrap(),
            "✓ write_file runs without asking in this session"
        );
        assert_eq!(
            g.snapshot(Level::Session).tools,
            [ToolKey::Builtin("write_file".to_owned())]
        );
        assert_eq!(
            c.allow("shel").unwrap_err(),
            "no built-in tool named \"shel\" (tools that ask: shell, write_file)"
        );
        assert_eq!(
            c.allow("read_file project").unwrap_err(),
            "read_file never asks (tools that ask: shell, write_file)"
        );
        assert_eq!(
            pf.store.project_grants(&root).unwrap().tools,
            std::slice::from_ref(&shell)
        );
        assert_eq!(g.snapshot(Level::Session).len(), 1);
    }

    /// The `Static` tab lists the agent's `allow:` then `--allow`, each with its source, the labels aligned;
    /// with neither, it has no rows and the surface keeps its two tabs.
    #[test]
    fn the_static_tab_lists_the_roots_by_source_and_is_absent_without_them() {
        let (t, root, out) = fixture();
        let add = PathBuf::from("/tmp/out");
        let c = Chat::new(&t, &root)
            .with_config(grants::GrantSet {
                dirs: vec![out.clone()],
                tools: Vec::new(),
            })
            .with_flag(grants::GrantSet {
                dirs: vec![add],
                tools: Vec::new(),
            });
        let (title, prompt, rows) = c.fixed().expect("the tab is shown");
        assert_eq!(title, "Static");
        assert_eq!(prompt, super::static_prompt("coder"));
        assert_eq!(
            rows,
            [
                format!("allow:   {}", out.display()),
                "--allow  /tmp/out".to_owned(),
            ]
        );
        assert!(Chat::new(&t, &root).fixed().is_none());
    }

    /// The `Static` tab lists the agent's `allow:` and `--allow` — each its
    /// directories, then its tools — each labelled with its source, and says where it is taken back: the file or
    /// the command line, not here; a tool `--allow` gives is refused naming the flag.
    #[test]
    fn the_static_tab_lists_tools_and_directories_by_source() {
        let (t, root, out) = fixture();
        let add = PathBuf::from("/tmp/out");
        let lib = out.join("sub");
        let gh = ToolKey::Mcp {
            server: ServerId::Config("gh".to_owned()),
            tool: McpTool::Any,
        };
        let c = Chat::new(&t, &root)
            .with_flag(grants::GrantSet {
                dirs: vec![add],
                tools: vec![ToolKey::Builtin("write_file".to_owned())],
            })
            .with_config(grants::GrantSet {
                dirs: vec![lib.clone()],
                tools: vec![ToolKey::Builtin("shell".to_owned()), gh],
            });
        let (title, prompt, rows) = c.fixed().expect("the tab is shown");
        assert_eq!(title, "Static");
        assert_eq!(
            prompt,
            "From the config and the command line: not grants made here, so none can be revoked here — edit agents.coder.allow, or drop the flag."
        );
        assert_eq!(
            rows,
            [
                format!("allow:   {}", lib.display()),
                "allow:   shell".to_owned(),
                "allow:   gh:*".to_owned(),
                "--allow  /tmp/out".to_owned(),
                "--allow  write_file".to_owned(),
            ]
        );
        assert!(c.rows().is_empty(), "the Grants tab holds none of it");
        assert_eq!(
            c.allow("write_file").unwrap_err(),
            "write_file already runs without asking (--allow)"
        );

        let only = Chat::new(&t, &root).with_config(grants::GrantSet {
            dirs: Vec::new(),
            tools: vec![ToolKey::Builtin("write_file".to_owned())],
        });
        assert_eq!(only.fixed().expect("shown").2, ["allow:   write_file"]);
    }

    /// What the config's `allow:` already gives, `/allow` refuses naming it — at either level, a directory under
    /// one of its directories, a built-in, an MCP tool its `*` covers — and the completion rows leave out: the
    /// built-in's rows and its scope row, every row of the server.
    #[test]
    fn allow_refuses_what_the_config_already_allows() {
        let (t, root, out) = fixture();
        let m = crate::mcp::testutil::serving(vec![
            (entry("gh", false), &[("search", "")]),
            (entry("lin", false), &[("list", "")]),
        ]);
        let c = Chat::new(&t, &root)
            .with_servers(m)
            .with_config(grants::GrantSet {
                dirs: vec![out.clone()],
                tools: vec![
                    ToolKey::Builtin("shell".to_owned()),
                    ToolKey::Mcp {
                        server: ServerId::Config("gh".to_owned()),
                        tool: McpTool::Any,
                    },
                ],
            });
        for line in ["shell", "shell project"] {
            assert_eq!(
                c.allow(line).unwrap_err(),
                "shell already runs without asking (allow:)"
            );
        }
        for line in ["gh:search", "gh:* project"] {
            let shown = line.trim_end_matches(" project");
            assert_eq!(
                c.allow(line).unwrap_err(),
                format!("{shown} already runs without asking (allow:)")
            );
        }
        for dir in [out.clone(), out.join("sub")] {
            let arg = dir.display().to_string();
            assert_eq!(
                c.allow(&arg).unwrap_err(),
                format!("{arg} is already writable (allow:)")
            );
            assert_eq!(
                c.allow(&format!("{arg} project")).unwrap_err(),
                format!("{arg} is already writable (allow:)")
            );
        }
        assert!(c.g().snapshot(Level::Session).is_empty());
        assert!(c.g().snapshot(Level::Project).is_empty());

        let values: Vec<String> = rows_of(&c).into_iter().map(|r| r.0).collect();
        assert_eq!(
            values,
            [
                "/allow write_file",
                "/allow write_file project",
                "/allow lin:",
                "/allow lin:*",
                "/allow lin:* project",
                "/allow lin:list",
                "/allow lin:list project",
            ]
        );
        assert!(
            c.allow("write_file").is_ok(),
            "what it does not allow, /allow grants"
        );
    }

    /// The scope is the LAST word: `project` there is the project level and the target is what precedes it,
    /// whatever the target is and however many spaces stand between; without it the grant is the session's.
    /// A last word that is not exactly `project` is part of the target.
    #[test]
    fn the_scope_is_the_last_word() {
        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root);
        let shown = root.display();
        assert_eq!(
            c.allow("shell").unwrap(),
            "✓ shell runs without asking in this session"
        );
        assert_eq!(
            c.allow("write_file   project").unwrap(),
            format!("✓ write_file runs without asking in this project ({shown})")
        );
        assert_eq!(
            c.allow("../out project").unwrap(),
            format!("✓ {} is writable for this project ({shown})", out.display())
        );
        assert_eq!(
            c.allow("shell projects").unwrap_err(),
            "no built-in tool named \"shell projects\" (tools that ask: shell, write_file)"
        );
        assert_eq!(
            c.g().snapshot(Level::Session).tools,
            [ToolKey::Builtin("shell".to_owned())]
        );
        let file = c.pf().store.project_grants(&root).unwrap();
        assert_eq!(file.tools, [ToolKey::Builtin("write_file".to_owned())]);
        assert_eq!(file.dirs, std::slice::from_ref(&out));
    }

    /// A first word `project` — alone, or before a target, the order the command once had — is refused naming
    /// the line to type, before anything else is read: in a run with a project and in one without, nothing is
    /// granted and nothing is written.
    #[test]
    fn project_first_is_refused_and_grants_nothing() {
        let (t, root, _out) = fixture();
        let with = Chat::new(&t, &root);
        let without = Chat::new(&t, &root).without_project(false, None);
        for c in [&with, &without] {
            for (arg, why) in [
                ("project", "the scope goes last: /allow <target> project"),
                ("project shell", "the scope goes last: /allow shell project"),
                (
                    "project  ../out",
                    "the scope goes last: /allow ../out project",
                ),
                ("project srv:*", "the scope goes last: /allow srv:* project"),
            ] {
                assert_eq!(c.allow(arg).unwrap_err(), why, "{arg}");
            }
            assert!(c.g().snapshot(Level::Session).is_empty());
            assert!(c.g().snapshot(Level::Project).is_empty());
        }
        assert!(!t.path().join("sessions").exists());
    }

    /// A directory whose name ends in ` project` is never guessed: beside `my`, `../my project` is refused naming
    /// both spellings and grants nothing; a trailing `/` keeps the name whole (a session grant), and `../my/
    /// project` is `my` for the project. A directory named `project` is `../project`, for the project `../project
    /// project`.
    #[test]
    fn a_directory_ending_in_project_is_never_guessed() {
        let (t, root, _out) = fixture();
        for d in ["my", "my project", "project"] {
            std::fs::create_dir(t.path().join(d)).unwrap();
        }
        let real = |d: &str| crate::app::canonical(&t.path().join(d)).unwrap();
        let c = Chat::new(&t, &root);
        let shown = root.display();
        assert_eq!(
            c.allow("../my project").unwrap_err(),
            "\"../my project\" is a directory too: write \"../my project/\" for it, or \"../my/ project\" for ../my in this project"
        );
        assert!(c.g().snapshot(Level::Session).is_empty());
        assert!(c.g().snapshot(Level::Project).is_empty());
        assert_eq!(
            c.allow("../my project/").unwrap(),
            format!(
                "✓ {} is writable for this session",
                real("my project").display()
            )
        );
        assert_eq!(
            c.allow("../my/ project").unwrap(),
            format!(
                "✓ {} is writable for this project ({shown})",
                real("my").display()
            )
        );
        assert_eq!(
            c.allow("../project").unwrap(),
            format!(
                "✓ {} is writable for this session",
                real("project").display()
            )
        );
        assert_eq!(
            c.allow("../project project").unwrap(),
            format!(
                "✓ {} is writable for this project ({shown})",
                real("project").display()
            )
        );
    }

    /// The rows as `(value, label, desc, after)`.
    fn rows_of(c: &Chat) -> Vec<(String, String, String, String)> {
        super::grantable(&c.door, &c.tools, c.servers.as_deref())
            .into_iter()
            .map(|r| (r.value, r.label, r.desc, r.after.unwrap_or_default()))
            .collect()
    }

    /// A built-in's row and its scope row, as [`super::grantable`] words them for the project at `root`.
    fn builtin_rows(
        name: &str,
        root: &std::path::Path,
        own: bool,
    ) -> Vec<(String, String, String, String)> {
        let mut rows = Vec::new();
        if own {
            rows.push((
                format!("/allow {name}"),
                name.to_owned(),
                "stop asking before it runs".to_owned(),
                "/allow ".to_owned(),
            ));
        }
        rows.push((
            format!("/allow {name} project"),
            "project".to_owned(),
            format!("for this project ({}), in every session", root.display()),
            format!("/allow {name} "),
        ));
        rows
    }

    /// `/allow `'s rows are the lines that would grant now: each built-in that asks, in the dispatcher's order,
    /// then its scope row behind `/allow <tool> ` — a tool that never asks has none. Every row, submitted, grants;
    /// a project grant takes both of the tool's rows away; a run with no project has no scope rows.
    #[test]
    fn the_rows_are_the_lines_that_would_grant() {
        let (t, root, _out) = fixture();
        let c = Chat::new(&t, &root);
        let all = [
            builtin_rows("shell", &root, true),
            builtin_rows("write_file", &root, true),
        ]
        .concat();
        assert_eq!(rows_of(&c), all);
        // Every row's value is a line `/allow` grants.
        for (value, ..) in &all {
            let arg = value.strip_prefix("/allow ").unwrap();
            assert!(c.allow(arg).is_ok(), "{value} did not grant");
        }
        assert!(rows_of(&c).is_empty(), "all granted, nothing left to offer");

        let (t, root, _out) = fixture();
        let c = Chat::new(&t, &root);
        c.allow("shell project").unwrap();
        assert_eq!(rows_of(&c), builtin_rows("write_file", &root, true));

        let (t, root, _out) = fixture();
        let c = Chat::new(&t, &root).without_project(false, None);
        let values: Vec<String> = rows_of(&c).into_iter().map(|r| r.0).collect();
        assert_eq!(values, ["/allow shell", "/allow write_file"]);
    }

    /// A tool granted for this session has no row of its own — that line would be refused — and keeps its
    /// `project` row, because `/allow shell project` still grants.
    #[test]
    fn a_session_grant_keeps_the_scope_row() {
        let (t, root, _out) = fixture();
        let c = Chat::new(&t, &root);
        c.allow("shell").unwrap();
        assert_eq!(
            rows_of(&c),
            [
                builtin_rows("shell", &root, false),
                builtin_rows("write_file", &root, true),
            ]
            .concat()
        );
        assert_eq!(
            c.allow("shell").unwrap_err(),
            "shell already runs without asking (this session)"
        );
        c.allow("shell project").unwrap();
        assert_eq!(rows_of(&c), builtin_rows("write_file", &root, true));
    }

    /// An `mcp_servers:` entry `name`, `auto_run` as given.
    fn entry(name: &str, auto_run: bool) -> crate::mcp::config::ServerConfig {
        crate::mcp::config::ServerConfig {
            name: name.to_owned(),
            command: format!("{name}-bin"),
            auto_run,
            ..crate::mcp::config::ServerConfig::default()
        }
    }

    /// The labels of the rows the list offers for `line`.
    fn offered(c: &Chat, line: &str) -> Vec<String> {
        let rows = super::grantable(&c.door, &c.tools, c.servers.as_deref());
        crate::ui::input::suggest::match_suggestions(&rows, line)
            .into_iter()
            .map(|r| r.label.clone())
            .collect()
    }

    /// A connected server is one row on level 1, after the built-ins, in config order — `gh:`, its value the
    /// prefix, its count its tool rows; behind `/allow gh:` wait `*` first, then its tools in the server's order,
    /// each with its own description, each followed by its scope row behind `/allow gh:<tool> `. Every row but
    /// the server's, submitted, grants; a session `*` takes every row of the server away but the scope rows,
    /// and with them the server's own row.
    #[test]
    fn a_servers_rows_wait_behind_its_prefix() {
        let chat = || {
            let (t, root, _out) = fixture();
            let m = crate::mcp::testutil::serving(vec![
                (
                    entry("gh", false),
                    &[("create_issue", "Opens an issue"), ("search", "Finds code")],
                ),
                (entry("lin", false), &[("list", "")]),
            ]);
            let c = Chat::new(&t, &root).with_servers(m);
            (t, root, c)
        };
        let (_t, root, c) = chat();
        let project = |line: &str| {
            (
                format!("{line} project"),
                "project".to_owned(),
                format!("for this project ({}), in every session", root.display()),
                format!("{line} "),
            )
        };
        let row = |value: &str, label: &str, desc: &str, after: &str| {
            (
                value.to_owned(),
                label.to_owned(),
                desc.to_owned(),
                after.to_owned(),
            )
        };
        let gh = [
            row("/allow gh:", "gh:", "MCP server · 2 tools", "/allow "),
            row(
                "/allow gh:*",
                "*",
                "every tool of gh, those it exposes later included",
                "/allow gh:",
            ),
            project("/allow gh:*"),
            row(
                "/allow gh:create_issue",
                "create_issue",
                "Opens an issue",
                "/allow gh:",
            ),
            project("/allow gh:create_issue"),
            row("/allow gh:search", "search", "Finds code", "/allow gh:"),
            project("/allow gh:search"),
        ];
        let lin = [
            row("/allow lin:", "lin:", "MCP server · 1 tool", "/allow "),
            row(
                "/allow lin:*",
                "*",
                "every tool of lin, those it exposes later included",
                "/allow lin:",
            ),
            project("/allow lin:*"),
            row("/allow lin:list", "list", "", "/allow lin:"),
            project("/allow lin:list"),
        ];
        let builtins = [
            builtin_rows("shell", &root, true),
            builtin_rows("write_file", &root, true),
        ]
        .concat();
        assert_eq!(rows_of(&c), [builtins, gh.to_vec(), lin.to_vec()].concat());

        // The levels, as the list filters them (docs/design/allow-completion.md §4.3).
        assert_eq!(
            offered(&c, "/allow "),
            ["shell", "write_file", "gh:", "lin:"]
        );
        assert_eq!(offered(&c, "/allow g"), ["gh:"]);
        assert_eq!(offered(&c, "/allow gh:"), ["*", "create_issue", "search"]);
        assert_eq!(offered(&c, "/allow gh:s"), ["search"]);
        assert_eq!(offered(&c, "/allow gh:search "), ["project"]);

        // Every row under a server is a line that grants now; the server's own is their prefix, not a line.
        for (value, ..) in gh.iter().skip(1).chain(&lin[1..]) {
            let (_t, _root, c) = chat();
            let arg = value.strip_prefix("/allow ").unwrap();
            assert!(c.allow(arg).is_ok(), "{value} did not grant");
        }
        assert!(c.allow("gh:").is_err(), "the prefix grants nothing");

        let (t, root, _out) = fixture();
        let m = crate::mcp::testutil::serving(vec![(entry("gh", false), &[("search", "")])]);
        let c = Chat::new(&t, &root).with_servers(m);
        c.allow("gh:*").unwrap();
        let values: Vec<String> = rows_of(&c).into_iter().map(|r| r.0).collect();
        assert_eq!(
            values,
            [
                "/allow shell",
                "/allow shell project",
                "/allow write_file",
                "/allow write_file project",
                "/allow gh:* project",
                "/allow gh:search project",
            ]
        );
    }

    /// The command's own refusals are the rows' absences: an `auto_run` server's tools never ask, and a name two
    /// connected servers share identifies neither — both have no row at all, while the server beside them keeps
    /// its own.
    #[test]
    fn an_auto_run_or_ambiguous_server_has_no_rows() {
        let (t, root, _out) = fixture();
        let flag = |spec: &str| crate::mcp::config::parse_mcp_flag(spec).expect("flag");
        let m = crate::mcp::testutil::serving(vec![
            (entry("auto", true), &[("push", "")]),
            (flag("npx -y @acme/mcp"), &[("push", "")]),
            (flag("npx -y @other/mcp"), &[("pull", "")]),
            (entry("gh", false), &[("search", "")]),
        ]);
        let c = Chat::new(&t, &root)
            .without_project(false, None)
            .with_servers(m);
        assert!(c.allow("auto:push").is_err());
        assert!(c.allow("npx:push").is_err());
        let values: Vec<String> = rows_of(&c).into_iter().map(|r| r.0).collect();
        assert_eq!(
            values,
            [
                "/allow shell",
                "/allow write_file",
                "/allow gh:",
                "/allow gh:*",
                "/allow gh:search",
            ]
        );
    }

    /// An `allow:` entry naming a tool its server does not expose is found once the server is connected; one it
    /// exposes, a `*`, and a server that is not connected say nothing.
    #[test]
    fn an_allowed_tool_the_server_lacks_is_found_when_it_connects() {
        let (t, root, _out) = fixture();
        let m = crate::mcp::testutil::serving(vec![(entry("gh", false), &[("search", "")])]);
        let key = |server: &str, tool: &str| ToolKey::Mcp {
            server: ServerId::Config(server.to_owned()),
            tool: McpTool::parse(tool),
        };
        let c = Chat::new(&t, &root)
            .with_servers(Arc::clone(&m))
            .with_config(grants::GrantSet {
                dirs: Vec::new(),
                tools: vec![
                    key("gh", "search"),
                    key("gh", "serch"),
                    key("gh", "*"),
                    key("lin", "list"),
                ],
            });
        assert_eq!(
            super::unexposed(c.g(), &m),
            [("gh".to_owned(), "serch".to_owned())]
        );
    }

    /// A tool's description is server-written: its ANSI sequences and other control characters never reach the
    /// list, its whitespace does (the list flattens it). A tool or a server whose NAME holds whitespace or a
    /// control character could not be typed back, so it has no row — the server's count says so too.
    #[test]
    fn a_servers_text_is_cleaned_before_it_is_a_row() {
        let (t, root, _out) = fixture();
        let m = crate::mcp::testutil::serving(vec![
            (
                entry("gh", false),
                &[
                    ("search", "\x1b[31mFinds\x1b[0m code\x07\nfast"),
                    ("two words", "x"),
                    ("bell\x07", "x"),
                ],
            ),
            (entry("my server", false), &[("push", "")]),
        ]);
        let c = Chat::new(&t, &root)
            .without_project(false, None)
            .with_servers(m);
        assert_eq!(
            rows_of(&c)
                .into_iter()
                .filter(|r| r.0.starts_with("/allow gh"))
                .map(|r| (r.1, r.2))
                .collect::<Vec<_>>(),
            [
                ("gh:".to_owned(), "MCP server · 1 tool".to_owned()),
                (
                    "*".to_owned(),
                    "every tool of gh, those it exposes later included".to_owned()
                ),
                ("search".to_owned(), "Finds code\nfast".to_owned()),
            ]
        );
        assert!(
            !rows_of(&c).iter().any(|r| r.0.contains("my server")),
            "a name with a space has no row"
        );
    }
}
