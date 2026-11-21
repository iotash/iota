//! `agents.<name>.allow` (docs/design/config-allow.md §3): what an agent may do without being asked, in
//! `/allow`'s vocabulary without the scope word — a directory, a built-in tool that asks, `<server>:<tool>` or
//! `<server>:*`. The entries fill the grant store's static level at assembly (`cmd::assemble_tools`,
//! [`crate::tool::grants::Grants::with_static`]); this module owns their grammar.
//!
//! An entry is read in `/allow`'s order (docs/design/grants.md §5.2), with the agent's own `mcp_servers:` names
//! in place of the connected ones:
//!
//! 1. a server's whole name — the longest that matches — then `:` and the tool;
//! 2. a directory: it starts with `~` or `.`, or has a root ([`Path::has_root`]: `/x` on Unix; `/x`, `\x`,
//!    `C:\x`, `C:/x` and `\\server\share` on Windows — so a drive's `:` is never a server's, and a rooted
//!    spelling without a drive is a path the OS resolves against the current drive; `C:x`, with no root, is not
//!    a directory);
//! 3. anything else holding a `:` — `<server>:<tool>`, split at its LAST `:`;
//! 4. a bare word — a built-in tool, by its wire name.
//!
//! There is no scope word: in a file the level is the file's, so a last word `project` is part of the target.

use std::path::Path;

use crate::tool::builtins::{ASKING, NAMES};
use crate::tool::grants::McpTool;

/// One `allow:` entry, classified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A directory, as written (trimmed): `~` is the home, a relative one hangs off the project root. Resolved
    /// to its real location at assembly.
    Dir(String),
    /// A built-in tool that asks ([`ASKING`]), by its wire name.
    Builtin(String),
    /// One tool of a server, or every tool of it.
    Mcp {
        /// The server, as written: an `mcp_servers:` entry's name.
        server: String,
        /// Which of its tools.
        tool: McpTool,
    },
}

/// What an entry that is no target of `allow:` is told.
pub const EXPECTED: &str =
    "expected a directory (starting with /, ~ or .), a built-in tool or <server>:<tool>";

/// Reads one entry against `servers`, the `mcp_servers:` names the agent loads (§3.2). The refusal is the
/// message after the entry's coordinate. Whether a server is one the agent loads, and whether a built-in is one
/// of its tools, is the load's to check (`Config::validate`): this is the grammar and the names that hold for
/// every agent.
pub fn parse(entry: &str, servers: &[String]) -> Result<Entry, String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err("is blank".to_owned());
    }
    let named = servers
        .iter()
        .filter_map(|s| Some((s, entry.strip_prefix(s.as_str())?.strip_prefix(':')?)))
        .max_by_key(|(s, _)| s.len());
    if let Some((server, tool)) = named {
        return mcp(server, tool);
    }
    if spells_dir(entry) {
        return Ok(Entry::Dir(entry.to_owned()));
    }
    if let Some((server, tool)) = entry.rsplit_once(':') {
        if server.is_empty() {
            return Err(EXPECTED.to_owned());
        }
        return mcp(server, tool);
    }
    let those = format!("tools that ask: {}", ASKING.join(", "));
    if ASKING.contains(&entry) {
        Ok(Entry::Builtin(entry.to_owned()))
    } else if NAMES.contains(&entry) {
        Err(format!("{entry} never asks ({those})"))
    } else {
        Err(format!("no built-in tool named \"{entry}\" ({those})"))
    }
}

/// Whether `target` is spelled as a directory (rule 2 of the module's order): it starts with `~` or `.`, or has
/// a root ([`Path::has_root`]). Not [`Path::is_absolute`]: on Windows `/x` and `\x` have a root but no drive, so
/// they are not absolute, yet the OS resolves them against the current drive — they ARE paths. `/allow`'s typed
/// target reads the same rule, so the chat, `allow:` and `--allow` agree on every platform.
pub fn spells_dir(target: &str) -> bool {
    target.starts_with(['~', '.']) || Path::new(target).has_root()
}

/// `<server>:<tool>`, the tool being a name or `*`; an empty one names nothing.
fn mcp(server: &str, tool: &str) -> Result<Entry, String> {
    if tool.is_empty() {
        return Err(format!(
            "\"{server}:\" names no tool: write {server}:<tool> or {server}:*"
        ));
    }
    Ok(Entry::Mcp {
        server: server.to_owned(),
        tool: McpTool::parse(tool),
    })
}

#[cfg(test)]
mod tests {
    use super::{EXPECTED, Entry, parse};
    use crate::tool::grants::McpTool;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| (*s).to_owned()).collect()
    }

    fn mcp(server: &str, tool: &str) -> Entry {
        Entry::Mcp {
            server: server.to_owned(),
            tool: McpTool::parse(tool),
        }
    }

    /// `/allow`'s order: a known server first, then a directory, then any `:` (split at the last), then a bare
    /// word — a built-in that asks, refused naming those that do when it never asks or is no tool at all.
    #[test]
    fn an_entry_is_read_in_allows_order() {
        let s = names(&["github"]);
        assert_eq!(
            parse("github:search_issues", &s),
            Ok(mcp("github", "search_issues"))
        );
        assert_eq!(parse("github:*", &s), Ok(mcp("github", "*")));
        assert_eq!(
            parse(" ~/scratch ", &s),
            Ok(Entry::Dir("~/scratch".to_owned()))
        );
        assert_eq!(
            parse("../shared-lib", &s),
            Ok(Entry::Dir("../shared-lib".to_owned()))
        );
        assert_eq!(parse("/tmp/out", &s), Ok(Entry::Dir("/tmp/out".to_owned())));
        // A server the agent does not load is still `<server>:<tool>`: the load says whose it is.
        assert_eq!(parse("gitlab:push", &s), Ok(mcp("gitlab", "push")));
        assert_eq!(parse("a:b:c", &s), Ok(mcp("a:b", "c")));
        for t in ["shell", "edit_file", "write_file"] {
            assert_eq!(parse(t, &s), Ok(Entry::Builtin(t.to_owned())));
        }
        let those = "(tools that ask: shell, edit_file, write_file)";
        assert_eq!(
            parse("read_file", &s),
            Err(format!("read_file never asks {those}"))
        );
        assert_eq!(
            parse("shel", &s),
            Err(format!("no built-in tool named \"shel\" {those}"))
        );
        assert_eq!(parse("  ", &s), Err("is blank".to_owned()));
        assert_eq!(
            parse("github:", &s),
            Err("\"github:\" names no tool: write github:<tool> or github:*".to_owned())
        );
        assert_eq!(parse(":push", &s), Err(EXPECTED.to_owned()));
    }

    /// A server whose name is spelled like a directory, or holds a `:`, is matched whole and first — the longest
    /// name that matches — so its tools are never read as a path.
    #[test]
    fn a_server_name_wins_over_the_directory_spelling() {
        let s = names(&["./srv.py", "a", "a:b"]);
        assert_eq!(parse("./srv.py:push", &s), Ok(mcp("./srv.py", "push")));
        assert_eq!(parse("./srv.py:*", &s), Ok(mcp("./srv.py", "*")));
        assert_eq!(parse("a:b:c", &s), Ok(mcp("a:b", "c")));
        assert_eq!(parse("a:c", &s), Ok(mcp("a", "c")));
        // Without the server, the same spelling is a directory.
        assert_eq!(
            parse("./srv.py:push", &[]),
            Ok(Entry::Dir("./srv.py:push".to_owned()))
        );
    }

    /// A file has no scope word: `project` at the end is part of the target, never a level.
    #[test]
    fn project_is_not_a_scope_word_in_a_file() {
        assert_eq!(
            parse("~/my project", &[]),
            Ok(Entry::Dir("~/my project".to_owned()))
        );
        assert_eq!(
            parse("shell project", &[]),
            Err(
                "no built-in tool named \"shell project\" (tools that ask: shell, edit_file, write_file)"
                    .to_owned()
            )
        );
        assert_eq!(
            parse("github:push project", &names(&["github"])),
            Ok(mcp("github", "push project"))
        );
    }

    /// On every platform a rooted spelling is a directory, and one with no root is not: `x` is a built-in's
    /// name, `C:x` (relative to a drive's current directory on Windows) is `<server>:<tool>`.
    #[test]
    fn a_rooted_spelling_is_a_directory_on_every_platform() {
        let s = names(&["github"]);
        assert_eq!(parse("/tmp/out", &s), Ok(Entry::Dir("/tmp/out".to_owned())));
        assert_eq!(
            parse("x", &s),
            Err(
                "no built-in tool named \"x\" (tools that ask: shell, edit_file, write_file)"
                    .to_owned()
            )
        );
        assert_eq!(parse("C:x", &s), Ok(mcp("C", "x")));
    }

    /// On Windows a drive path, a UNC path and a rooted path without a drive have a root, so they are a
    /// directory before any `:` rule reads the drive's colon as a server's. A platform-specific expectation, not
    /// a skipped test: off Windows a drive spelling has no root (pinned below).
    #[cfg(windows)]
    #[test]
    fn a_windows_rooted_path_is_a_directory() {
        let s = names(&["github"]);
        for d in [r"C:\x", "C:/x", r"\\server\share\x", r"\x", "/x"] {
            assert_eq!(parse(d, &s), Ok(Entry::Dir(d.to_owned())), "{d}");
        }
        // Relative to the drive's current directory, with no root: still `<server>:<tool>`.
        assert_eq!(parse("C:x", &s), Ok(mcp("C", "x")));
    }

    /// Off Windows a drive spelling has no root: it is `<server>:<tool>`, which the load refuses when no
    /// `mcp_servers:` entry has that name.
    #[cfg(not(windows))]
    #[test]
    fn a_drive_spelling_is_no_directory_off_windows() {
        assert_eq!(parse(r"C:\x", &[]), Ok(mcp("C", r"\x")));
    }
}
