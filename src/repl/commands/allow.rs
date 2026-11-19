//! `/allow` and `/revoke` — the chat's door to the grant store (docs/design/grants.md §5.2, DIVERGENCES X-111).
//!
//! The only door the shell has: a sandboxed command cannot be prompted mid-run, so a directory it must write
//! is made writable here, typed by the user. `/allow <dir>` grants the directory's REAL location and its
//! subtree for this session — the code jail and the shell sandbox read it at their next call, the harness's
//! `write roots:` line at the next turn (`refresh_harness`). `/allow project <dir>` grants it for the project,
//! in every session and every mode. `/allow [project] <server>:<tool>` stops the question for one MCP tool,
//! `<server>:*` for every tool of the server, those it exposes later included: `<server>` is resolved against
//! the CONNECTED servers and `<tool>` against its current tool list, so a typo is refused instead of stored, and
//! the grant is keyed by the server's identity (its configured name, or a `--mcp` server's whole flag value),
//! never the wire segment. `/revoke <dir | server:tool | server:*>` takes it back from every level that holds
//! it; a bare `/allow` lists what is in effect, by level. Every change of the session level is written into the bundle's
//! meta ([`save_session`]) — the dialog's grants too — so a resume restores it; an ephemeral chat keeps it in
//! memory until `/save` stamps it. A change of the project level is written to the project's `grants.json`
//! FIRST ([`ProjectFile::update`]) and only then takes effect — `--no-save` included: it is not conversation.
//!
//! A message from the socket never reaches here: the loop's notice path runs no slash command
//! (`repl::inbound`), and the model has no way to type one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::app::{fs, paths, protected};
use crate::mcp::Manager;
use crate::mcp::config::parse_mcp_flag;
use crate::repl::render::styles::dim;
use crate::repl::run::Repl;
use crate::repl::title::WriterSlot;
use crate::session::{SessionError, SessionStore};
use crate::sync::lock;
use crate::tool::grants::{self, GrantSet, Grants, Level, McpTool, ServerId, ToolKey};

/// What the user is told when the argument is not something `/allow` takes.
const EXPECTED: &str = "expected a directory (starting with /, ~ or .) or <server>:<tool>";

/// Where the project level is kept: the store's `grants.json` for the project root the tools read paths
/// against (`ToolEnv::root`), in every mode.
#[derive(Clone, Debug)]
pub(crate) struct ProjectFile {
    /// The sessions root the project's bucket is in.
    pub(crate) store: SessionStore,
    /// The project root.
    pub(crate) root: PathBuf,
}

impl ProjectFile {
    /// The chat's project file.
    pub(crate) fn of(repl: &Repl) -> Self {
        Self {
            store: repl.session.store.clone(),
            root: Boundary::of(repl).root,
        }
    }

    /// Replaces the project level of `grants` with what the file holds now — a start, `/new`, a resume. A file
    /// that cannot be read grants nothing: the warning to show is returned.
    pub(crate) fn load(&self, grants: &Grants) -> Option<String> {
        load_project(&self.store, &self.root, grants)
    }

    /// Changes the project level, the file FIRST (read–modify–write) and then memory, which takes what the file
    /// now holds — a grant another process made meanwhile included. Nothing changes when the file cannot be
    /// read or written.
    pub(crate) fn update(
        &self,
        grants: &Grants,
        change: impl FnOnce(&mut GrantSet),
    ) -> Result<GrantSet, SessionError> {
        let set = self.store.update_project_grants(&self.root, change)?;
        grants.replace_project(set.clone());
        Ok(set)
    }
}

/// [`ProjectFile::load`] for a caller with the parts: the project level of `grants` replaced by `root`'s entry
/// of its bucket's file; on a file that cannot be read, emptied, and the warning returned (without its `⚠`).
pub(crate) fn load_project(store: &SessionStore, root: &Path, grants: &Grants) -> Option<String> {
    match store.project_grants(root) {
        Ok(set) => {
            grants.replace_project(set);
            None
        }
        Err(e) => {
            grants.replace_project(GrantSet::default());
            Some(format!("grants: {e}; no project grants apply"))
        }
    }
}

/// Where the chat's boundary is now: what a typed directory is read against and compared with.
struct Boundary<'a> {
    /// The project root, as the tools read a relative path against it.
    root: PathBuf,
    /// The top-level `write_roots:` as the tools hold them.
    static_roots: &'a [PathBuf],
    /// The user's home directory: `~` and the protected table's home rows.
    home: Option<&'a Path>,
}

impl<'a> Boundary<'a> {
    fn of(repl: &'a Repl) -> Self {
        let env = &repl.conv.harness_inputs.env;
        // `ToolEnv::root`'s order: the project root, else the working directory.
        let root = Some(env.project_root.clone())
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| repl.conv.agent.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        Self {
            root,
            static_roots: &env.write_roots,
            home: repl.conv.agent.home.as_deref(),
        }
    }

    /// The typed `arg` as an absolute, cleaned path: `~` is home, a relative one hangs off the project root.
    /// `None` when `arg` is not spelled as a directory.
    fn path(&self, arg: &str) -> Option<PathBuf> {
        if !arg.starts_with(['/', '~', '.']) {
            return None;
        }
        let p = crate::tool::expand_home(arg, self.home);
        Some(paths::clean(&self.root.join(p)))
    }
}

/// `/allow` with `arg`, the trimmed text after it.
pub(crate) fn cmd_allow(repl: &Repl, arg: &str) {
    let tr = &repl.handles.tr;
    let b = Boundary::of(repl);
    if arg.is_empty() {
        tr.notice_lines(&listing(&repl.handles.grants, b.static_roots));
        return;
    }
    let servers = repl.handles.mcp.manager.as_deref();
    match allow(
        &repl.handles.grants,
        &ProjectFile::of(repl),
        &b,
        servers,
        arg,
    ) {
        Ok(line) => {
            tr.notice(&line);
            saved(repl);
        }
        Err(why) => tr.error(&format!("/allow: {why}")),
    }
}

/// `/revoke` with `arg`, the trimmed text after it.
pub(crate) fn cmd_revoke(repl: &Repl, arg: &str) {
    let tr = &repl.handles.tr;
    match revoke(
        &repl.handles.grants,
        &ProjectFile::of(repl),
        &Boundary::of(repl),
        arg,
    ) {
        Ok(line) => {
            tr.notice(&line);
            saved(repl);
        }
        Err(why) => tr.error(&format!("/revoke: {why}")),
    }
}

/// [`save_session`] for a command, a failure said as every other failed meta write is.
fn saved(repl: &Repl) {
    if let Err(e) = save_session(&repl.session.writer, &repl.handles.grants) {
        repl.handles
            .tr
            .error(&format!("Warning: failed to save session: {e}"));
    }
}

/// Writes the session level of `grants` into the meta of the bundle in `writer`, under its single-writer lock
/// (`SessionWriter::update_meta`; a pending bundle keeps it for its first write). An ephemeral chat has no
/// bundle: the grants stay in memory, and `/save` stamps them into the meta it mints.
pub(crate) fn save_session(writer: &WriterSlot, grants: &Grants) -> Result<(), SessionError> {
    let set = grants.snapshot(Level::Session);
    lock(writer)
        .as_mut()
        .map_or(Ok(()), |w| w.update_meta(|m| m.grants = set))
}

/// How many grants each level holds, as `/status` and the start's line say it — `2 for this session, 3 for this
/// project`, a level with none left out: `None` when neither holds any.
pub(crate) fn counted(session: usize, project: usize) -> Option<String> {
    let parts: Vec<String> = [(session, "this session"), (project, "this project")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, level)| format!("{n} for {level}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// [`counted`] over the store.
pub(crate) fn summary(grants: &Grants) -> Option<String> {
    counted(
        grants.snapshot(Level::Session).len(),
        grants.snapshot(Level::Project).len(),
    )
}

/// The line a start or a resume prints when it loaded grants — the bundle's, the project's: `None` when it
/// loaded none.
pub(crate) fn loaded(grants: &Grants) -> Option<String> {
    summary(grants).map(|s| format!("grants: {s}"))
}

/// [`loaded`] as a chat says it: with where to look.
pub(crate) fn loaded_in_chat(grants: &Grants) -> Option<String> {
    loaded(grants).map(|l| format!("{l} (/allow lists them)"))
}

/// Grants the directory or the MCP tool `arg` for this session — or, after the keyword `project`, for this
/// project: the notice line, or why not. `servers` is the run's MCP manager, `None` in a run with no server.
fn allow(
    grants: &Grants,
    project: &ProjectFile,
    b: &Boundary<'_>,
    servers: Option<&Manager>,
    arg: &str,
) -> Result<String, String> {
    let (level, arg) = match arg.strip_prefix("project") {
        Some(rest) if rest.starts_with(char::is_whitespace) => (Level::Project, rest.trim_start()),
        _ => (Level::Session, arg),
    };
    let Some(typed) = b.path(arg) else {
        let (server, tool) = server_tool(arg).ok_or_else(|| EXPECTED.to_owned())?;
        return allow_tool(grants, project, servers, level, server, tool);
    };
    if !typed.is_dir() {
        return Err(format!("{arg} is not a directory"));
    }
    let real = fs::physical(&typed).ok_or_else(|| format!("cannot work out where {arg} is"))?;
    let shown = real.display();

    // The table beats every root: a protected directory is refused before "already writable" is asked.
    let mut extra = b.static_roots.to_vec();
    extra.extend(grants.live_dirs());
    let table = protected::table(&b.root, &extra, b.home);
    if let Some(why) = protected::why_protected(&table, &real) {
        return Err(format!("{shown} is protected: {}", why.reason()));
    }
    if let Some(by) = writable_by(grants, b, level, &real) {
        return Err(format!("{shown} is already writable ({by})"));
    }

    let mut line = match level {
        Level::Session => {
            grants.grant_dir(Level::Session, real.clone());
            format!("✓ {shown} is writable for this session")
        }
        Level::Project => {
            project
                .update(grants, |s| {
                    if !s.dirs.contains(&real) {
                        s.dirs.push(real.clone());
                    }
                })
                .map_err(|e| format!("could not save the grant ({e})"))?;
            let root = fs::physical(&project.root).unwrap_or_else(|| project.root.clone());
            format!(
                "✓ {shown} is writable for this project ({})",
                root.display()
            )
        }
    };
    if real != typed {
        let _ = write!(line, " ({arg} leads there)");
    }
    Ok(line)
}

/// `<server>:<tool>` split at its first `:`, both halves non-empty; `None` for anything else.
fn server_tool(arg: &str) -> Option<(&str, &str)> {
    arg.split_once(':')
        .filter(|(s, t)| !s.is_empty() && !t.is_empty())
}

/// Grants `server`'s `tool` (or, as `*`, every tool of it) at `level`: the notice line, or why not. The server
/// must be connected, under that name, exactly once — a name two `--mcp` servers share identifies neither —
/// and must not be `auto_run`; a named tool must be one it lists now.
fn allow_tool(
    grants: &Grants,
    project: &ProjectFile,
    servers: Option<&Manager>,
    level: Level,
    server: &str,
    tool: &str,
) -> Result<String, String> {
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
    let held = |l: Level| covered(&grants.snapshot(l), &key);
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
    match level {
        Level::Session => {
            grants.grant_tool(Level::Session, key);
            Ok(format!("✓ {what} runs without asking in this session"))
        }
        Level::Project => {
            project
                .update(grants, |s| {
                    if !s.tools.contains(&key) {
                        s.tools.push(key);
                    }
                })
                .map_err(|e| format!("could not save the grant ({e})"))?;
            let root = fs::physical(&project.root).unwrap_or_else(|| project.root.clone());
            Ok(format!(
                "✓ {what} runs without asking in this project ({})",
                root.display()
            ))
        }
    }
}

/// Whether `set` answers the tool `key`: by name, or — an MCP tool — by its server's `*`.
fn covered(set: &GrantSet, key: &ToolKey) -> bool {
    let any = match key {
        ToolKey::Mcp { server, .. } => ToolKey::Mcp {
            server: server.clone(),
            tool: McpTool::Any,
        },
        ToolKey::Builtin(_) => return set.tools.contains(key),
    };
    set.tools.contains(key) || set.tools.contains(&any)
}

/// The name a server's identity is typed as: an entry's name; a `--mcp` server's `argv[0]` (or URL), as it is
/// named in `/tools`.
fn server_name(id: &ServerId) -> String {
    match id {
        ServerId::Config(name) => name.clone(),
        ServerId::Flag(spec) => parse_mcp_flag(spec).map_or_else(|_| spec.clone(), |c| c.name),
    }
}

/// A tool grant as the dialog and the notices name it: a built-in by its name, an MCP tool as
/// `<server>:<tool>` (`<server>:*` for every tool).
pub(crate) fn label(key: &ToolKey) -> String {
    match key {
        ToolKey::Builtin(name) => name.clone(),
        ToolKey::Mcp { server, tool } => format!("{}:{}", server_name(server), tool.as_str()),
    }
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
/// root, a `write_roots:` entry, or a grant that covers it — of this project at either level, of this session
/// only for another session grant (a project grant is what a session grant becomes when it should last).
fn writable_by(
    grants: &Grants,
    b: &Boundary<'_>,
    level: Level,
    real: &Path,
) -> Option<&'static str> {
    let under = |root: &Path| fs::physical(root).is_some_and(|r| paths::within(&r, real));
    if under(&b.root) {
        return Some("the project root");
    }
    if b.static_roots.iter().any(|r| under(r)) {
        return Some("write_roots:");
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

/// Takes the directory `arg` back from every level that holds it: the notice line, or why not. The grant is
/// matched on the path as typed (made absolute) and on its real location now, so a grant gone stale can still
/// be named. The project level goes through its file first; one that cannot be written keeps the grant.
fn revoke(
    grants: &Grants,
    project: &ProjectFile,
    b: &Boundary<'_>,
    arg: &str,
) -> Result<String, String> {
    let Some(typed) = b.path(arg) else {
        let (server, tool) = server_tool(arg).ok_or_else(|| EXPECTED.to_owned())?;
        return revoke_tool(grants, project, server, tool, arg);
    };
    let real = fs::physical(&typed);
    for dir in [Some(typed), real].into_iter().flatten() {
        let mut from = Vec::new();
        if grants.snapshot(Level::Project).dirs.contains(&dir) {
            project
                .update(grants, |s| s.dirs.retain(|d| *d != dir))
                .map_err(|e| format!("could not save the revocation ({e})"))?;
            from.push("this project");
        }
        if grants.revoke_dir(Level::Session, &dir) {
            from.insert(0, "this session");
        }
        if !from.is_empty() {
            return Ok(format!("✓ revoked {} ({})", dir.display(), from.join(", ")));
        }
    }
    Err(format!("nothing granted matches {arg}"))
}

/// Takes back every grant of `server`'s `tool` (`*` as itself, never as a pattern) from every level that holds
/// it: the server need not be connected, and is matched by the name it is typed as ([`server_name`]) — so one
/// `--mcp` name revokes the grants of every flag value it starts. The project level goes through its file first.
fn revoke_tool(
    grants: &Grants,
    project: &ProjectFile,
    server: &str,
    tool: &str,
    arg: &str,
) -> Result<String, String> {
    let matches = |k: &ToolKey| match k {
        ToolKey::Mcp {
            server: id,
            tool: t,
        } => t.as_str() == tool && server_name(id) == server,
        ToolKey::Builtin(_) => false,
    };
    let mut from = Vec::new();
    if grants.snapshot(Level::Project).tools.iter().any(matches) {
        project
            .update(grants, |s| s.tools.retain(|k| !matches(k)))
            .map_err(|e| format!("could not save the revocation ({e})"))?;
        from.push("this project");
    }
    let session: Vec<ToolKey> = grants
        .snapshot(Level::Session)
        .tools
        .into_iter()
        .filter(matches)
        .collect();
    if !session.is_empty() {
        for k in &session {
            grants.revoke_tool(Level::Session, k);
        }
        from.insert(0, "this session");
    }
    if from.is_empty() {
        return Err(format!("nothing granted matches {arg}"));
    }
    Ok(format!("✓ revoked {server}:{tool} ({})", from.join(", ")))
}

/// The bare `/allow`: every grant in effect, by level, then the static roots — or, with none, how to make one.
fn listing(grants: &Grants, static_roots: &[PathBuf]) -> Vec<String> {
    let session = grants.snapshot(Level::Session);
    let project = grants.snapshot(Level::Project);
    if session.is_empty() && project.is_empty() && static_roots.is_empty() {
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
    for r in static_roots {
        rows.push(("static", format!("{} (write_roots:)", r.display())));
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

    use super::{Boundary, Grants, Level, ProjectFile, ToolKey, listing, revoke};
    use crate::tool::grants::{McpTool, ServerId};

    /// `/allow` in a run with no MCP server.
    fn allow(g: &Grants, pf: &ProjectFile, b: &Boundary<'_>, arg: &str) -> Result<String, String> {
        super::allow(g, pf, b, None, arg)
    }
    use crate::session::SessionStore;
    use crate::text::ansi::strip_sgr;

    /// A temp dir holding `proj/` (the project, a repository) and `out/` beside it; their real locations.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(t.path().join("proj/.git/hooks")).expect("proj");
        std::fs::create_dir_all(t.path().join("out/sub")).expect("out");
        let root = std::fs::canonicalize(t.path().join("proj")).expect("real proj");
        let out = std::fs::canonicalize(t.path().join("out")).expect("real out");
        (t, root, out)
    }

    fn project(t: &tempfile::TempDir, root: &std::path::Path) -> ProjectFile {
        ProjectFile {
            store: SessionStore::new(t.path().join("sessions")),
            root: root.to_path_buf(),
        }
    }

    fn boundary(root: PathBuf, statics: &[PathBuf]) -> Boundary<'_> {
        Boundary {
            root,
            static_roots: statics,
            home: None,
        }
    }

    /// A relative argument is read against the project root; the protected table beats "already writable";
    /// what a root or a grant already covers is refused saying which; a granted directory reads back.
    #[test]
    fn allow_reads_relative_against_the_root_and_refuses_what_is_covered() {
        let (t, root, out) = fixture();
        let statics = [out.join("sub")];
        let b = boundary(root.clone(), &statics);
        let g = Grants::default();
        let pf = project(&t, &root);
        assert_eq!(
            allow(&g, &pf, &b, "./.git/hooks").unwrap_err(),
            format!(
                "{} is protected: git executes the files in .git/hooks on the user's next git command",
                root.join(".git/hooks").display()
            )
        );
        assert_eq!(
            allow(&g, &pf, &b, ".").unwrap_err(),
            format!("{} is already writable (the project root)", root.display())
        );
        let sub = out.join("sub");
        assert_eq!(
            allow(&g, &pf, &b, sub.to_str().unwrap()).unwrap_err(),
            format!("{} is already writable (write_roots:)", sub.display())
        );
        assert_eq!(
            allow(&g, &pf, &b, "../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            allow(&g, &pf, &b, "../out").unwrap_err(),
            format!("{} is already writable (this session)", out.display())
        );
        assert_eq!(g.live_dirs(), std::slice::from_ref(&out));
        assert_eq!(
            revoke(&g, &pf, &b, "../out/").unwrap(),
            format!("✓ revoked {} (this session)", out.display())
        );
        assert!(g.live_dirs().is_empty());
    }

    /// `project` is a keyword: the grant goes to the project's file first, then memory. A project grant covers a
    /// session one asked for later, but a session grant does not stop a project one; a revoke takes the
    /// directory from every level that holds it, and the file with it.
    #[test]
    fn allow_project_reaches_the_file_and_revoke_every_level() {
        let (t, root, out) = fixture();
        let b = boundary(root.clone(), &[]);
        let g = Grants::default();
        let pf = project(&t, &root);
        assert_eq!(
            allow(&g, &pf, &b, "../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            allow(&g, &pf, &b, "project ../out").unwrap(),
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
            allow(&g, &pf, &b, "project ../out/sub").unwrap_err(),
            format!(
                "{} is already writable (this project)",
                out.join("sub").display()
            )
        );
        assert_eq!(
            allow(&g, &pf, &b, "../out/sub").unwrap_err(),
            format!(
                "{} is already writable (this project)",
                out.join("sub").display()
            )
        );
        assert_eq!(
            revoke(&g, &pf, &b, "../out").unwrap(),
            format!("✓ revoked {} (this session, this project)", out.display())
        );
        assert!(g.live_dirs().is_empty());
        assert!(pf.store.project_grants(&root).unwrap().is_empty());
        assert_eq!(
            revoke(&g, &pf, &b, "../out").unwrap_err(),
            "nothing granted matches ../out"
        );

        // A file that cannot be read is never replaced: the grant is refused, saying why.
        let path = pf.store.grants_path(&root);
        std::fs::write(&path, "{broken").unwrap();
        let err = allow(&g, &pf, &b, "project ../out").unwrap_err();
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

    /// The listing: each level once in the label column, a stale grant marked, the static roots last.
    #[test]
    fn the_listing_names_each_level_once() {
        let (t, _root, out) = fixture();
        let g = Grants::default();
        assert_eq!(
            listing(&g, &[])
                .iter()
                .map(|l| strip_sgr(l))
                .collect::<Vec<_>>(),
            [
                "No grants. /allow <dir> makes a directory writable for this session, /allow project <dir> for this project."
            ]
        );
        let gone = t.path().join("gone");
        g.grant_dir(Level::Session, out.clone());
        g.grant_dir(Level::Session, gone.clone());
        g.grant_tool(Level::Session, ToolKey::Builtin("edit_file".to_owned()));
        g.grant_dir(Level::Project, out.join("sub"));
        g.grant_tool(
            Level::Project,
            ToolKey::Mcp {
                server: ServerId::Config("github".to_owned()),
                tool: McpTool::Any,
            },
        );
        g.grant_tool(
            Level::Project,
            ToolKey::Mcp {
                server: ServerId::Flag("npx -y @acme/mcp".to_owned()),
                tool: McpTool::Named("push".to_owned()),
            },
        );
        let lines: Vec<String> = listing(&g, &[PathBuf::from("/data/a")])
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
            ]
        );
    }
}
