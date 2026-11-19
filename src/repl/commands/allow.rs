//! `/allow` and `/revoke` — the chat's door to the grant store (docs/design/grants.md §5.2, DIVERGENCES X-111).
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
//! never the wire segment. `/revoke <dir | server:tool | server:*>` takes it back from every level that holds
//! it; a bare `/allow` lists what is in effect, by level, then the static roots by where they were declared
//! (`write_roots:`, `--add-dir`). This file parses and words; every change is kept by the grants service,
//! [`Door::apply`] (`repl::grants`), as the dialog's and `/save`'s are.
//!
//! A message from the socket never reaches here: the loop's notice path runs no slash command
//! (`repl::inbound`), and the model has no way to type one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::app::{fs, paths, protected};
use crate::mcp::Manager;
use crate::repl::grants::{Door, Footing, Unsaved, label, server_name, unsaved_session};
use crate::repl::render::styles::dim;
use crate::repl::run::Repl;
use crate::session::SessionError;
use crate::tool::grants::{self, Grants, Level, McpTool, ServerId, ToolKey};

/// What the user is told when the argument is not something `/allow` takes.
const EXPECTED: &str = "expected a directory (starting with /, ~ or .) or <server>:<tool>";

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

/// `/allow` with `arg`, the trimmed text after it.
pub(crate) fn cmd_allow(repl: &Repl, arg: &str) {
    let tr = &repl.handles.tr;
    let footing = &repl.handles.footing;
    if arg.is_empty() {
        tr.notice_lines(&listing(&repl.handles.grants, footing));
        return;
    }
    let servers = repl.handles.mcp.manager.as_deref();
    match allow(&repl.door(), footing, home(repl), servers, arg) {
        Ok(done) => say(repl, done),
        Err(why) => tr.error(&format!("/allow: {why}")),
    }
}

/// `/revoke` with `arg`, the trimmed text after it.
pub(crate) fn cmd_revoke(repl: &Repl, arg: &str) {
    match revoke(&repl.door(), &repl.handles.footing, home(repl), arg) {
        Ok(done) => say(repl, done),
        Err(why) => repl.handles.tr.error(&format!("/revoke: {why}")),
    }
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
    let Some(typed) = typed_path(footing, home, arg) else {
        let (server, tool) = server_tool(arg).ok_or_else(|| EXPECTED.to_owned())?;
        return allow_tool(door, servers, level, server, tool);
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

/// Takes the directory or the MCP tool `arg` back from every level that holds it: the notice line, or why not.
/// A directory grant is matched on the path as typed (made absolute) and on its real location now, so a grant
/// gone stale can still be named. The project level goes through its file first; one that cannot be written keeps the grant.
fn revoke(door: &Door, footing: &Footing, home: Option<&Path>, arg: &str) -> Result<Done, String> {
    // The stored grants' server names, resolved first as `/allow` resolves the connected ones ([`named_server`]):
    // a grant is taken back with no server connected.
    let names: Vec<String> = [Level::Session, Level::Project]
        .into_iter()
        .flat_map(|l| door.grants.snapshot(l).tools)
        .filter_map(|k| match k {
            ToolKey::Mcp { server, .. } => Some(server_name(&server)),
            ToolKey::Builtin(_) => None,
        })
        .collect();
    if let Some((server, tool)) = named_server(arg, &names) {
        return revoke_tool(door, server, tool, arg);
    }
    let Some(typed) = typed_path(footing, home, arg) else {
        let (server, tool) = server_tool(arg).ok_or_else(|| EXPECTED.to_owned())?;
        return revoke_tool(door, server, tool, arg);
    };
    let real = fs::physical(&typed);
    for dir in [Some(typed), real].into_iter().flatten() {
        let from = revoke_where(
            door,
            |s| s.dirs.contains(&dir),
            |s| {
                s.dirs.retain(|d| *d != dir);
            },
        )?;
        if let Some(done) = from.said(&dir.display().to_string()) {
            return Ok(done);
        }
    }
    Err(format!("nothing granted matches {arg}"))
}

/// Takes back every grant of `server`'s `tool` (`*` as itself, never as a pattern) from every level that holds
/// it: the server need not be connected, and is matched by the name it is typed as ([`server_name`]) — so one
/// `--mcp` name revokes the grants of every flag value it starts. The project level goes through its file first.
fn revoke_tool(door: &Door, server: &str, tool: &str, arg: &str) -> Result<Done, String> {
    let matches = |k: &ToolKey| match k {
        ToolKey::Mcp {
            server: id,
            tool: t,
        } => t.as_str() == tool && server_name(id) == server,
        ToolKey::Builtin(_) => false,
    };
    revoke_where(
        door,
        |s| s.tools.iter().any(matches),
        |s| {
            s.tools.retain(|k| !matches(k));
        },
    )?
    .said(&format!("{server}:{tool}"))
    .ok_or_else(|| format!("nothing granted matches {arg}"))
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

/// The bare `/allow`: every grant in effect, by level, then the static roots — each saying where it was
/// declared — or, with none, how to make one.
fn listing(grants: &Grants, footing: &Footing) -> Vec<String> {
    let session = grants.snapshot(Level::Session);
    let project = grants.snapshot(Level::Project);
    if session.is_empty()
        && project.is_empty()
        && footing.write_roots.is_empty()
        && footing.added_dirs.is_empty()
    {
        return vec![dim(
            "No grants. /allow <dir> makes a directory writable for this session, /allow project <dir> for this project.",
        )];
    }
    let mut rows: Vec<(&str, String)> = Vec::new();
    for (label, set) in [("this session", &session), ("this project", &project)] {
        for d in &set.dirs {
            let stale = if grants::is_live(d) { "" } else { " (stale)" };
            rows.push((label, format!("{}{stale}", d.display())));
        }
        for key in &set.tools {
            rows.push((label, listed(key)));
        }
    }
    for (roots, by) in [
        (&footing.write_roots, "write_roots:"),
        (&footing.added_dirs, "--add-dir"),
    ] {
        for r in roots {
            rows.push(("static", format!("{} ({by})", r.display())));
        }
    }
    let width = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let mut out = vec!["Grants".to_owned()];
    let mut last = "";
    for (level, value) in rows {
        let label = if level == last { "" } else { level };
        last = level;
        out.push(dim(&format!("  {label:<width$}   {value}")));
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{Footing, Grants, Level, ToolKey, listing, named_server, server_tool};
    use crate::repl::grants::{Door, ProjectFile};
    use crate::session::SessionStore;
    use crate::text::ansi::strip_sgr;
    use crate::tool::grants::{McpTool, ServerId};

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

        fn revoke(&self, arg: &str) -> Result<String, String> {
            super::revoke(&self.door, &self.footing, None, arg).map(|d| d.line)
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
            c.revoke("../out/").unwrap(),
            format!("✓ revoked {} (this session)", out.display())
        );
        assert!(c.g().live_dirs().is_empty());
    }

    /// `project` is a keyword: the grant goes to the project's file first, then memory. A project grant covers a
    /// session one asked for later, but a session grant does not stop a project one; a revoke takes the
    /// directory from every level that holds it, and the file with it.
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
            c.revoke("../out").unwrap(),
            format!("✓ revoked {} (this session, this project)", out.display())
        );
        assert!(g.live_dirs().is_empty());
        assert!(pf.store.project_grants(&root).unwrap().is_empty());
        assert_eq!(
            c.revoke("../out").unwrap_err(),
            "nothing granted matches ../out"
        );

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

    /// The listing: each level once in the label column, a stale grant marked, the static roots last.
    #[test]
    fn the_listing_names_each_level_once() {
        let (t, root, out) = fixture();
        let c = Chat::new(&t, &root, &[], &[]);
        let g = c.g();
        assert_eq!(
            listing(g, &c.footing)
                .iter()
                .map(|l| strip_sgr(l))
                .collect::<Vec<_>>(),
            [
                "No grants. /allow <dir> makes a directory writable for this session, /allow project <dir> for this project."
            ]
        );
        let gone = t.path().join("gone");
        g.update(Level::Session, |s| {
            s.add_dir(out.clone());
            s.add_dir(gone.clone());
            s.add_tool(ToolKey::Builtin("edit_file".to_owned()));
        });
        g.update(Level::Project, |s| {
            s.add_dir(out.join("sub"));
            s.add_tool(ToolKey::Mcp {
                server: ServerId::Config("github".to_owned()),
                tool: McpTool::Any,
            });
            s.add_tool(ToolKey::Mcp {
                server: ServerId::Flag("npx -y @acme/mcp".to_owned()),
                tool: McpTool::Named("push".to_owned()),
            });
        });
        let c = Chat::new(
            &t,
            &root,
            &[PathBuf::from("/data/a")],
            &[PathBuf::from("/data/b")],
        );
        let lines: Vec<String> = listing(g, &c.footing)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(
            lines,
            [
                "Grants".to_owned(),
                format!("  this session   {}", out.display()),
                format!("                 {} (stale)", gone.display()),
                "                 edit_file".to_owned(),
                format!("  this project   {}", out.join("sub").display()),
                "                 github:*".to_owned(),
                "                 npx:push (--mcp npx -y @acme/mcp)".to_owned(),
                "  static         /data/a (write_roots:)".to_owned(),
                "                 /data/b (--add-dir)".to_owned(),
            ]
        );
    }
}
