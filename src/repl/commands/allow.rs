//! `/allow` and `/revoke` — the chat's door to the grant store (docs/design/grants.md §5.2, DIVERGENCES X-111).
//!
//! The only door the shell has: a sandboxed command cannot be prompted mid-run, so a directory it must write
//! is made writable here, typed by the user. `/allow <dir>` grants the directory's REAL location and its
//! subtree for this session — the code jail and the shell sandbox read it at their next call, the harness's
//! `write roots:` line at the next turn (`refresh_harness`). `/revoke <dir>` takes it back; a bare `/allow`
//! lists what is in effect, by level.
//!
//! A message from the socket never reaches here: the loop's notice path runs no slash command
//! (`repl::inbound`), and the model has no way to type one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::app::{fs, paths, protected};
use crate::repl::render::styles::dim;
use crate::repl::run::Repl;
use crate::tool::grants::{self, GrantSet, Grants, Level, ToolKey};

/// What the user is told when the argument is not something `/allow` takes.
const EXPECTED: &str = "expected a directory (starting with /, ~ or .)";

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
    match allow(&repl.handles.grants, &b, arg) {
        Ok(line) => tr.notice(&line),
        Err(why) => tr.error(&format!("/allow: {why}")),
    }
}

/// `/revoke` with `arg`, the trimmed text after it.
pub(crate) fn cmd_revoke(repl: &Repl, arg: &str) {
    let tr = &repl.handles.tr;
    match revoke(&repl.handles.grants, &Boundary::of(repl), arg) {
        Ok(line) => tr.notice(&line),
        Err(why) => tr.error(&format!("/revoke: {why}")),
    }
}

/// Grants the directory `arg` for this session: the notice line, or why not.
fn allow(grants: &Grants, b: &Boundary<'_>, arg: &str) -> Result<String, String> {
    let typed = b.path(arg).ok_or_else(|| EXPECTED.to_owned())?;
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
    if let Some(by) = writable_by(grants, b, &real) {
        return Err(format!("{shown} is already writable ({by})"));
    }

    grants.grant_dir(Level::Session, real.clone());
    let mut line = format!("✓ {shown} is writable for this session");
    if real != typed {
        let _ = write!(line, " ({arg} leads there)");
    }
    Ok(line)
}

/// What already makes the real directory `real` writable, if anything: the project root, a `write_roots:`
/// entry, or a grant of this session that covers it.
fn writable_by(grants: &Grants, b: &Boundary<'_>, real: &Path) -> Option<&'static str> {
    let under = |root: &Path| fs::physical(root).is_some_and(|r| paths::within(&r, real));
    if under(&b.root) {
        return Some("the project root");
    }
    if b.static_roots.iter().any(|r| under(r)) {
        return Some("write_roots:");
    }
    let session = grants.snapshot(Level::Session);
    session
        .dirs
        .iter()
        .any(|d| grants::is_live(d) && paths::within(d, real))
        .then_some("this session")
}

/// Takes the directory `arg` back: the notice line, or why not. The grant is matched on the path as typed
/// (made absolute) and on its real location now, so a grant gone stale can still be named.
fn revoke(grants: &Grants, b: &Boundary<'_>, arg: &str) -> Result<String, String> {
    let typed = b.path(arg).ok_or_else(|| EXPECTED.to_owned())?;
    let real = fs::physical(&typed);
    for dir in [Some(typed), real].into_iter().flatten() {
        if grants.revoke_dir(Level::Session, &dir) {
            return Ok(format!("✓ revoked {} (this session)", dir.display()));
        }
    }
    Err(format!("nothing granted matches {arg}"))
}

/// The bare `/allow`: every grant in effect, by level, then the static roots — or, with none, how to make one.
fn listing(grants: &Grants, static_roots: &[PathBuf]) -> Vec<String> {
    let session = grants.snapshot(Level::Session);
    if session == GrantSet::default() && static_roots.is_empty() {
        return vec![dim(
            "No grants. /allow <dir> makes a directory writable for this session.",
        )];
    }
    let mut rows: Vec<(&str, String)> = Vec::new();
    for d in &session.dirs {
        let stale = if grants::is_live(d) { "" } else { " (stale)" };
        rows.push(("this session", format!("{}{stale}", d.display())));
    }
    for ToolKey::Builtin(name) in &session.tools {
        rows.push(("this session", name.clone()));
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

    use super::{Boundary, Grants, Level, ToolKey, allow, listing, revoke};
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
        let (_t, root, out) = fixture();
        let statics = [out.join("sub")];
        let b = boundary(root.clone(), &statics);
        let g = Grants::default();
        assert_eq!(
            allow(&g, &b, "./.git/hooks").unwrap_err(),
            format!(
                "{} is protected: git executes the files in .git/hooks on the user's next git command",
                root.join(".git/hooks").display()
            )
        );
        assert_eq!(
            allow(&g, &b, ".").unwrap_err(),
            format!("{} is already writable (the project root)", root.display())
        );
        let sub = out.join("sub");
        assert_eq!(
            allow(&g, &b, sub.to_str().unwrap()).unwrap_err(),
            format!("{} is already writable (write_roots:)", sub.display())
        );
        assert_eq!(
            allow(&g, &b, "../out").unwrap(),
            format!("✓ {} is writable for this session", out.display())
        );
        assert_eq!(
            allow(&g, &b, "../out").unwrap_err(),
            format!("{} is already writable (this session)", out.display())
        );
        assert_eq!(g.live_dirs(), std::slice::from_ref(&out));
        assert_eq!(
            revoke(&g, &b, "../out/").unwrap(),
            format!("✓ revoked {} (this session)", out.display())
        );
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
            ["No grants. /allow <dir> makes a directory writable for this session."]
        );
        let gone = t.path().join("gone");
        g.grant_dir(Level::Session, out.clone());
        g.grant_dir(Level::Session, gone.clone());
        g.grant_tool(Level::Session, ToolKey::Builtin("edit_file".to_owned()));
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
                "  static         /data/a (write_roots:)".to_owned(),
            ]
        );
    }
}
