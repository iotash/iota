//! `/allow` — the chat's door to the grant store (docs/design/grants.md §5.2, DIVERGENCES X-111).
//!
//! The only door the shell has: a sandboxed command cannot be prompted mid-run, so a directory it must write
//! is made writable here, typed by the user. `/allow <dir>` grants the directory's REAL location and its
//! subtree for this session — the code jail and the shell sandbox read it at their next call, the harness's
//! `write roots:` line at the next turn (`refresh_harness`). `/allow project <dir>` grants it for the project,
//! in every session and every mode. `/allow [project] <server>:<tool>` stops the question for one MCP tool,
//! `<server>:*` for every tool of the server, those it exposes later included: `<server>` is resolved against
//! the CONNECTED servers' names before the argument is read as a directory (a `--mcp` server is named by its
//! URL or `argv[0]`: [`named_server`]), and `<tool>` against its current tool list, so a typo is refused instead
//! of stored, and the grant is keyed by the server's identity (its configured name, or a `--mcp` server's whole flag value),
//! never the wire segment.
//!
//! The BARE `/allow` opens a tabbed surface, the `/file` way — the focused tab IS the verb. `Add` is a
//! directory browser ([`crate::ui::facade::Pick::Dir`]: Enter descends, Space chooses, `./` is the directory shown) whose choice
//! is granted for this session exactly as `/allow <dir>` grants it; `Grants` is a multi-select of every grant
//! in effect, by level, and committing it takes each checked grant back from every level that holds it; `Static`,
//! shown only when there is one, is a read-only view of the roots that are not grants (`write_roots:`, `--add-dir`). This
//! file parses and words; every change is kept by the grants service, [`Door::apply`] (`repl::grants`), as the
//! dialog's and `/save`'s are.
//!
//! A message from the socket never reaches here: the loop's notice path runs no slash command
//! (`repl::inbound`), and the model has no way to type one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::app::{fs, paths, protected};
use crate::mcp::Manager;
use crate::repl::grants::{Door, Footing, Unsaved, label, unsaved_session};
use crate::repl::run::Repl;
use crate::session::SessionError;
use crate::tool::grants::{self, Grants, Level, McpTool, ServerId, ToolKey};
use crate::ui::facade::{Panel, TabbedSpec};

/// What the user is told when the argument is not something `/allow` takes.
const EXPECTED: &str = "expected a directory (starting with /, ~ or .) or <server>:<tool>";

/// The `Grants` tab's prompt when nothing is granted.
const NONE: &str = "No grants. Add makes a directory writable for this session; /allow project <dir> for this project.";

/// The `Static` tab's prompt: what its rows are, and why none can be taken back here.
const STATIC: &str =
    "Writable from the config and the command line: not grants, so none can be revoked here.";

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
        servers,
        arg,
    ) {
        Ok(done) => say(repl, done),
        Err(why) => repl.handles.tr.error(&format!("/allow: {why}")),
    }
}

/// The bare `/allow`: `Add` (a directory browser opened on the project root) and `Grants` (every grant in
/// effect, checked rows meaning "take back"), then `Static` when there is a static root ([`static_tab`]). A cancel, or a commit with nothing chosen or checked, changes
/// nothing.
async fn panel(repl: &Repl) {
    let door = repl.door();
    let footing = &repl.handles.footing;
    let held = granted(&door.grants);
    let rows = held.iter().map(Granted::row).collect();
    let mut grants_tab = Panel::multi("Grants", rows).with_search(true);
    if held.is_empty() {
        grants_tab = grants_tab.with_prompt(NONE);
    }
    let mut panels = vec![
        Panel::dir_browser("Add", footing.project.root.clone()).with_search(true),
        grants_tab,
    ];
    panels.extend(static_tab(footing));
    let spec = TabbedSpec {
        panels,
        ..TabbedSpec::default()
    };
    // A facade failure is a cancel, as everywhere: a closed facade ends the loop at the next read.
    let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await else {
        return;
    };
    if r.cancelled {
        return;
    }
    match r.focused {
        0 => {
            let path = r.panels.first().map(|p| p.path.clone()).unwrap_or_default();
            if path.is_empty() {
                return;
            }
            match allow_dir(&door, footing, home(repl), Level::Session, &path) {
                Ok(done) => say(repl, done),
                Err(why) => repl.handles.tr.error(&format!("/allow: {why}")),
            }
        }
        1 => {
            let checked = r
                .panels
                .get(1)
                .map(|p| p.checked.clone())
                .unwrap_or_default();
            for done in revoke_checked(&door, &held, &checked) {
                match done {
                    Ok(done) => say(repl, done),
                    Err(why) => repl.handles.tr.error(&format!("/allow: {why}")),
                }
            }
        }
        // `Static` is read-only: Enter there commits the surface, and nothing is done.
        _ => {}
    }
}

/// The `Static` tab: a read-only view, one row per writable root that is not a grant, `write_roots:` first,
/// each labelled with its source (the labels padded to one width). `None` when there is no such root: the
/// surface then keeps its two tabs.
fn static_tab(footing: &Footing) -> Option<Panel> {
    let w = footing.write_roots.iter().map(|r| ("write_roots:", r));
    let a = footing.added_dirs.iter().map(|r| ("--add-dir", r));
    let rows: Vec<String> = w
        .chain(a)
        .map(|(from, r)| format!("{from:<12}  {}", r.display()))
        .collect();
    (!rows.is_empty()).then(|| Panel::view("Static", rows).with_prompt(STATIC))
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
    Some(paths::clean(&footing.project.root.join(p)))
}

/// Grants the directory or the MCP tool `arg` for this session — or, after the keyword `project`, for this
/// project: the notice line, or why not. `servers` is the run's MCP manager, `None` in a run with no server.
fn allow(
    door: &Door,
    footing: &Footing,
    home: Option<&Path>,
    servers: Option<&Manager>,
    arg: &str,
) -> Result<Done, String> {
    let (level, arg) = match arg.strip_prefix("project") {
        Some(rest) if rest.starts_with(char::is_whitespace) => (Level::Project, rest.trim_start()),
        _ => (Level::Session, arg),
    };
    let names = servers.map(Manager::connected_names).unwrap_or_default();
    if let Some((server, tool)) = named_server(arg, &names) {
        return allow_tool(door, servers, level, server, tool);
    }
    if typed_path(footing, home, arg).is_none() {
        let (server, tool) = server_tool(arg).ok_or_else(|| EXPECTED.to_owned())?;
        return allow_tool(door, servers, level, server, tool);
    }
    allow_dir(door, footing, home, level, arg)
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
    let root = &footing.project.root;
    let table = protected::table(root, &door.grants.roots(&footing.static_roots()), home);
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

/// The project root as a project grant's notice names it: its real location.
fn shown_root(door: &Door) -> String {
    let root = &door.project.root;
    fs::physical(root)
        .unwrap_or_else(|| root.clone())
        .display()
        .to_string()
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
        && !target.tools.contains(t)
    {
        return Err(format!("server \"{server}\" has no tool \"{t}\""));
    }
    let key = ToolKey::Mcp {
        server: target.id.clone(),
        tool,
    };
    let shown = label(&key);
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
    let what = match &key {
        ToolKey::Mcp {
            tool: McpTool::Any, ..
        } => format!("every tool of {server}"),
        _ => shown,
    };
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

/// [`label`] as the listing shows it: a `--mcp` server's grant also names the whole flag value it is keyed by.
fn listed(key: &ToolKey) -> String {
    match key {
        ToolKey::Mcp {
            server: ServerId::Flag(spec),
            ..
        } => format!("{} (--mcp {spec})", label(key)),
        _ => label(key),
    }
}

/// What already makes the real directory `real` writable for a grant at `level`, if anything: the project
/// root, a `write_roots:` entry, an `--add-dir` value, or a grant that covers it — of this project at either
/// level, of this session only for another session grant (a project grant is what a session grant becomes when
/// it should last).
fn writable_by(
    grants: &Grants,
    footing: &Footing,
    level: Level,
    real: &Path,
) -> Option<&'static str> {
    let under = |root: &Path| fs::physical(root).is_some_and(|r| paths::within(&r, real));
    if under(&footing.project.root) {
        return Some("the project root");
    }
    if footing.write_roots.iter().any(|r| under(r)) {
        return Some("write_roots:");
    }
    if footing.added_dirs.iter().any(|r| under(r)) {
        return Some("--add-dir");
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
    /// The row: its level, then the directory (`(stale)` when it no longer leads anywhere) or the tool, a
    /// `--mcp` server's also naming the whole flag value it is keyed by.
    fn row(&self) -> String {
        let level = match self.level {
            Level::Session => "this session",
            Level::Project => "this project",
        };
        let what = match &self.what {
            Grant::Dir(d) => {
                let stale = if grants::is_live(d) { "" } else { " (stale)" };
                format!("{}{stale}", d.display())
            }
            Grant::Tool(key) => listed(key),
        };
        format!("{level}  {what}")
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
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{
        Footing, Grants, Level, ToolKey, granted, named_server, revoke_checked, server_tool,
    };
    use crate::repl::grants::{Door, ProjectFile};
    use crate::session::SessionStore;
    use crate::tool::grants::{McpTool, ServerId};
    use crate::ui::facade::PanelBody;

    /// A temp dir holding `proj/` (the project, a repository) and `out/` beside it; their real locations.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(t.path().join("proj/.git/hooks")).expect("proj");
        std::fs::create_dir_all(t.path().join("out/sub")).expect("out");
        let root = std::fs::canonicalize(t.path().join("proj")).expect("real proj");
        let out = std::fs::canonicalize(t.path().join("out")).expect("real out");
        (t, root, out)
    }

    /// A chat's grants over `root`, its project file in the temp dir, `write_roots:` and `--add-dir` as given,
    /// and no bundle.
    struct Chat {
        door: Door,
        footing: Footing,
    }

    impl Chat {
        fn new(
            t: &tempfile::TempDir,
            root: &std::path::Path,
            write_roots: &[PathBuf],
            added_dirs: &[PathBuf],
        ) -> Self {
            let project = ProjectFile {
                store: SessionStore::new(t.path().join("sessions")),
                root: root.to_path_buf(),
            };
            Self {
                door: Door {
                    grants: Arc::default(),
                    writer: Arc::default(),
                    project: project.clone(),
                },
                footing: Footing {
                    project,
                    write_roots: write_roots.to_vec(),
                    added_dirs: added_dirs.to_vec(),
                },
            }
        }

        fn g(&self) -> &Grants {
            &self.door.grants
        }

        fn pf(&self) -> &ProjectFile {
            &self.door.project
        }

        /// `/allow` in a run with no MCP server.
        fn allow(&self, arg: &str) -> Result<String, String> {
            super::allow(&self.door, &self.footing, None, None, arg).map(|d| d.line)
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
            granted(self.g()).iter().map(super::Granted::row).collect()
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

    /// A relative argument is read against the project root; the protected table beats "already writable";
    /// what a root or a grant already covers is refused saying which; a granted directory reads back.
    #[test]
    fn allow_reads_relative_against_the_root_and_refuses_what_is_covered() {
        let (t, root, out) = fixture();
        let added = t.path().join("added");
        std::fs::create_dir(&added).unwrap();
        let added = std::fs::canonicalize(added).unwrap();
        let c = Chat::new(&t, &root, &[out.join("sub")], std::slice::from_ref(&added));
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
            format!("{} is already writable (write_roots:)", sub.display())
        );
        assert_eq!(
            c.allow(added.to_str().unwrap()).unwrap_err(),
            format!("{} is already writable (--add-dir)", added.display())
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

    /// `project` is a keyword: the grant goes to the project's file first, then memory. A project grant covers a
    /// session one asked for later, but a session grant does not stop a project one; committing the `Grants` tab
    /// takes a checked directory from every level that holds it, and the file with it.
    #[test]
    fn allow_project_reaches_the_file_and_revoke_every_level() {
        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root, &[], &[]);
        let (g, pf) = (c.g(), c.pf());
        assert_eq!(
            c.allow("../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            c.allow("project ../out").unwrap(),
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
            c.allow("project ../out/sub").unwrap_err(),
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
        let err = c.allow("project ../out").unwrap_err();
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
        let c = Chat::new(&t, &root, &[], &[]);
        let (g, pf) = (c.g(), c.pf());
        assert!(c.rows().is_empty());
        let gone = t.path().join("gone");
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
        c.allow("project ../out/sub").unwrap();
        c.door
            .apply(Level::Project, |s| {
                s.add_tool(star.clone());
                s.add_tool(flag.clone());
            })
            .unwrap();
        assert_eq!(
            c.rows(),
            [
                format!("this session  {}", out.display()),
                format!("this session  {} (stale)", gone.display()),
                "this session  github:*".to_owned(),
                format!("this project  {}", out.join("sub").display()),
                "this project  github:*".to_owned(),
                "this project  npx:push (--mcp npx -y @acme/mcp)".to_owned(),
            ]
        );
        // The stale directory, `github:*` (checked at both of its rows) and the project's directory.
        assert_eq!(
            c.revoke(&[1, 2, 3, 4]),
            [
                Ok(format!("✓ revoked {} (this session)", gone.display())),
                Ok("✓ revoked github:* (this session, this project)".to_owned()),
                Ok(format!(
                    "✓ revoked {} (this project)",
                    out.join("sub").display()
                )),
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

    /// The `Static` tab lists `write_roots:` then `--add-dir`, each with its source, the labels aligned; with
    /// neither, it has no rows and the surface keeps its two tabs.
    #[test]
    fn the_static_tab_lists_the_roots_by_source_and_is_absent_without_them() {
        let (t, root, out) = fixture();
        let add = PathBuf::from("/tmp/out");
        let c = Chat::new(
            &t,
            &root,
            std::slice::from_ref(&out),
            std::slice::from_ref(&add),
        );
        let (title, prompt, rows) = c.fixed().expect("the tab is shown");
        assert_eq!(title, "Static");
        assert_eq!(prompt, super::STATIC);
        assert_eq!(
            rows,
            [
                format!("write_roots:  {}", out.display()),
                "--add-dir     /tmp/out".to_owned(),
            ]
        );
        assert!(Chat::new(&t, &root, &[], &[]).fixed().is_none());
    }
}
