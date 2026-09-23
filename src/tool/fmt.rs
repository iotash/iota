//! Interactive tool-call display helpers (the D-12 lift). Rune-based, no width deps — a
//! header is truncated on rune boundaries, never display columns (a recorded divergence:
//! the header once measured display width; this module stays width-crate-free so the
//! TUI-free build carries no ruler).

use serde_json::Value;

use crate::provider::model::ToolCall;
use crate::tool::Dispatcher;

/// Max arguments shown in a call header before the `"… +N args"` tail.
pub(crate) const TOOL_HEADER_MAX_ARGS: usize = 3;

/// Max runes of one argument value in a call header.
pub(crate) const TOOL_HEADER_MAX_VALUE: usize = 15;

/// Max result lines shown inline under a call.
pub(crate) const TOOL_RESULT_MAX_LINES: usize = 3;

/// `"[name k:v k:v]"`: keys sorted, values one-lined + 15-rune cap + `"…"`, max 3 args then
/// `"… +N args"`; a `Some(summary)` from `header_summary` takes over completely (`Some("")` →
/// bare `"[name]"`).
pub fn tool_call_header(dispatch: &dyn Dispatcher, tc: &ToolCall) -> String {
    let name = display_tool_name(&tc.name);
    let detail = tool_call_detail(dispatch, tc);
    if detail.is_empty() {
        format!("[{name}]")
    } else {
        format!("[{name} {detail}]")
    }
}

/// Approval evidence: `header_summary` else the sorted-key digest.
///
/// A tool that writes its own summary takes over completely — an empty one renders as a
/// bare `"[name]"`, never as the argument digest (`edit_file`'s `new_string` must not
/// reach a header).
pub(crate) fn tool_call_detail(dispatch: &dyn Dispatcher, tc: &ToolCall) -> String {
    if let Some(summary) = dispatch.header_summary(&tc.name, &tc.arguments) {
        return summary;
    }
    let mut keys: Vec<&String> = tc.arguments.keys().collect();
    keys.sort();

    let shown = keys.len().min(TOOL_HEADER_MAX_ARGS);
    let mut parts: Vec<String> = Vec::with_capacity(shown + 1);
    for k in &keys[..shown] {
        let v = value_one_line(&tc.arguments[k.as_str()]);
        parts.push(format!("{k}:{}", truncate_runes(&v, TOOL_HEADER_MAX_VALUE)));
    }
    let extra = keys.len() - shown;
    if extra > 0 {
        parts.push(format!("… +{extra} args"));
    }
    parts.join(" ")
}

/// `"mcp__srv__tool"` → `"srv:tool"`; other names verbatim.
///
/// The server segment can never contain `"__"` (the MCP manager collapses underscore
/// runs), so the first `"__"` after the prefix is always the separator; a degenerate wire
/// name with an empty segment is returned unchanged.
pub(crate) fn display_tool_name(name: &str) -> String {
    let Some(rest) = name.strip_prefix("mcp__") else {
        return name.to_owned();
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return name.to_owned();
    };
    if server.is_empty() || tool.is_empty() {
        return name.to_owned();
    }
    format!("{server}:{tool}")
}

/// ≤3 lines full; >3 → 2 rows + `"    … +%d lines"`; 120-rune row cap; `"  ⎿ %s"` first /
/// `"    %s"` rest; `"(no output)"` for blank. Returns the rows (caller styles red on
/// error).
pub fn print_tool_result_lines(text: &str, _is_error: bool) -> Vec<String> {
    result_rows(text, "  ⎿ ")
}

/// The same fold with every row a continuation (`"    %s"`): the rows under a receipt that already
/// took the `⎿` — a yielded call's output so far, beneath its `still running` line.
pub fn continuation_rows(text: &str) -> Vec<String> {
    result_rows(text, "    ")
}

/// The fold behind both: `first` leads the first row, four blanks the rest.
fn result_rows(text: &str, first: &str) -> Vec<String> {
    let trimmed = text.trim_end_matches('\n');
    let body = if trimmed.trim().is_empty() {
        "(no output)"
    } else {
        trimmed
    };
    let lines: Vec<&str> = body.split('\n').collect();
    let (show, extra) = if lines.len() > TOOL_RESULT_MAX_LINES {
        // Leave the last row for the tail.
        let n = TOOL_RESULT_MAX_LINES - 1;
        (&lines[..n], lines.len() - n)
    } else {
        (&lines[..], 0)
    };
    let mut out = Vec::with_capacity(show.len() + 1);
    for (i, ln) in show.iter().enumerate() {
        let ln = truncate_runes(ln, 120);
        if i == 0 {
            out.push(format!("{first}{ln}"));
        } else {
            out.push(format!("    {ln}"));
        }
    }
    if extra > 0 {
        out.push(format!("    … +{extra} lines"));
    }
    out
}

/// A JSON argument value collapsed to one line: strings verbatim, everything else the
/// compact JSON text with its keys in sorted order (deterministic — DIVERGENCES T-33).
fn value_one_line(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    s.replace('\n', " ")
}

/// Truncates on rune boundaries + `'…'` so CJK text is never cut mid-rune.
fn truncate_runes(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        head + "…"
    } else {
        head
    }
}

// ---- the header-format ladder shared by the toolsets (additive to
// the frozen §4 set — both the code set and shell render paths, and one ladder is the
// point) ----

/// Display cap of a header path.
pub(crate) const HEADER_PATH_MAX: usize = 48;

/// The header's command rule — [`crate::text::header_command`], 64 runes of the first line, the one
/// rule for a command shown as a label (the header, the finished-job notice, the `/jobs` row, the
/// status row's job segment); it lives in `text` because `shell::jobs` sits below this module.
pub(crate) use crate::text::header_command;

/// Renders a model-supplied path for a call header: relative
/// verbatim; under `cwd` → cwd-relative; elsewhere under `root` → the `"../"` form; under
/// home → `"~/…"`; else absolute — then head-elided to [`HEADER_PATH_MAX`] runes.
pub(crate) fn header_path(raw: &str, cwd: &std::path::Path, root: &std::path::Path) -> String {
    let p = raw.trim();
    if p.is_empty() {
        return String::new();
    }
    elide_path_head(&header_path_full(p, cwd, root), HEADER_PATH_MAX)
}

fn header_path_full(p: &str, cwd: &std::path::Path, root: &std::path::Path) -> String {
    let path = std::path::Path::new(p);
    if !path.is_absolute() {
        // 1: the model's own spelling, cleaned.
        return crate::app::paths::to_slash(&crate::app::paths::clean(path));
    }
    let abs = crate::app::paths::clean(path);
    if !cwd.as_os_str().is_empty()
        && let Some(rel) = crate::app::paths::rel(cwd, &abs)
    {
        let rel_s = crate::app::paths::to_slash(&rel);
        if !rel_s.starts_with("..") {
            return rel_s; // 2: under cwd
        }
        if within(root, &abs) {
            return rel_s; // 3: elsewhere in the project
        }
    }
    if let Some(home) = std::env::home_dir()
        && !home.as_os_str().is_empty()
        && let Some(rel) = crate::app::paths::rel(&home, &abs)
    {
        let rel_s = crate::app::paths::to_slash(&rel);
        if !rel_s.starts_with("..") {
            return format!("~/{rel_s}"); // 4: under home
        }
    }
    crate::app::paths::to_slash(&abs) // 5: absolute
}

/// Whether `abs` sits inside `dir`.
fn within(dir: &std::path::Path, abs: &std::path::Path) -> bool {
    if dir.as_os_str().is_empty() {
        return false;
    }
    crate::app::paths::rel(dir, abs).is_some_and(|rel| {
        let s = crate::app::paths::to_slash(&rel);
        s != ".." && !s.starts_with("../")
    })
}

/// Trims a path from the FRONT to fit `max` runes, keeping whole segments where it can.
fn elide_path_head(p: &str, max: usize) -> String {
    if p.chars().count() <= max {
        return p.to_owned();
    }
    let segs: Vec<&str> = p.split('/').collect();
    for i in 1..segs.len() {
        let cand = format!(".../{}", segs[i..].join("/"));
        if cand.chars().count() <= max {
            return cand;
        }
    }
    // A single oversized segment (one very long file name): cut into it.
    let last = segs.last().copied().unwrap_or_default();
    format!("...{}", truncate_runes_front(last, max.saturating_sub(3)))
}

/// Keeps the LAST `n` runes of `s`.
fn truncate_runes_front(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_owned();
    }
    s.chars().skip(count - n).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        HEADER_PATH_MAX, display_tool_name, header_command, header_path, print_tool_result_lines,
    };

    #[test]
    fn test_display_tool_name() {
        assert_eq!(display_tool_name("shell"), "shell");
        assert_eq!(display_tool_name("mcp__srv__tool"), "srv:tool");
        assert_eq!(display_tool_name("mcp__srv__has__inner"), "srv:has__inner");
        assert_eq!(display_tool_name("mcp____tool"), "mcp____tool");
        assert_eq!(display_tool_name("mcp__srv__"), "mcp__srv__");
        assert_eq!(display_tool_name("mcp__loose"), "mcp__loose");
    }

    /// An absolute fixture root: `filepath.IsAbs`'s twin wants a drive letter on Windows, and the
    /// ladder's rungs are only reachable for a path that clears that bar.
    #[cfg(windows)]
    const FIXTURE_ROOT: &str = r"C:\proj";
    #[cfg(not(windows))]
    const FIXTURE_ROOT: &str = "/proj";

    /// An absolute path in neither the project nor home.
    #[cfg(windows)]
    const OUTSIDE_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts";
    #[cfg(not(windows))]
    const OUTSIDE_PATH: &str = "/etc/hosts";

    // The header path ladder: each rung exists for
    // a case the one above cannot serve, because a wrong rung silently degrades every file call's
    // header into something the user cannot type back. (Rune-count ruler — the display-width
    // divergence is noted in the module docs.)
    #[test]
    fn test_header_path() {
        // The ladder's first rung is "is this path absolute?", which on Windows means a drive
        // letter — a `/proj` fixture would be RELATIVE there and never reach the rungs under test.
        let root = std::path::PathBuf::from(FIXTURE_ROOT);
        let cwd = root.join("sub");
        let (cwd, root) = (cwd.as_path(), root.as_path());
        let abs = |parts: &[&str]| {
            parts
                .iter()
                .fold(root.to_path_buf(), |p, seg| p.join(seg))
                .to_string_lossy()
                .into_owned()
        };
        // relative stays verbatim
        assert_eq!(header_path("src/ui/model.rs", cwd, root), "src/ui/model.rs");
        // relative is cleaned but not rebased
        assert_eq!(header_path("./a/../b.rs", cwd, root), "b.rs");
        // under cwd
        assert_eq!(
            header_path(&abs(&["sub", "a", "b.rs"]), cwd, root),
            "a/b.rs"
        );
        // cwd itself
        assert_eq!(header_path(&abs(&["sub"]), cwd, root), ".");
        // elsewhere in the project walks up
        assert_eq!(
            header_path(&abs(&["other", "x.rs"]), cwd, root),
            "../other/x.rs"
        );
        // empty
        assert_eq!(header_path("", cwd, root), "");

        // under home outside the project (skipped when the home directory is unknown or contains
        // the fixture root)
        match std::env::home_dir() {
            Some(home) if !home.as_os_str().is_empty() && !root.starts_with(&home) => {
                let p = home.join("elsewhere").join("y.rs");
                assert_eq!(
                    header_path(&p.to_string_lossy(), cwd, root),
                    "~/elsewhere/y.rs"
                );
            }
            _ => eprintln!("SKIP: home directory unavailable or contains the fixture root"),
        }
    }

    // A path outside both the project and home has nowhere to be relative to.
    #[test]
    fn test_header_path_falls_back_to_absolute() {
        let root = std::path::PathBuf::from(FIXTURE_ROOT);
        let outside = std::path::PathBuf::from(OUTSIDE_PATH);
        assert_eq!(
            header_path(&outside.to_string_lossy(), &root.join("sub"), &root),
            crate::app::paths::to_slash(&outside)
        );
    }

    // Long paths lose their HEAD:
    // the basename identifies the file, so it is the part that must survive.
    #[test]
    fn test_header_path_elides_from_the_front() {
        let long = "a/very/deeply/nested/tree/of/directories/that/keeps/going/model.rs";
        let got = header_path(long, Path::new(""), Path::new(""));
        assert!(
            got.chars().count() <= HEADER_PATH_MAX,
            "not elided: {got:?} ({} cols)",
            got.chars().count()
        );
        assert!(got.ends_with("model.rs"), "basename lost: {got:?}");
        assert!(got.starts_with(".../"), "elision marker missing: {got:?}");

        // One oversized segment has no separator to cut at — the tail still wins.
        let huge = "x".repeat(200) + ".rs";
        let got = header_path(&huge, Path::new(""), Path::new(""));
        assert!(
            got.chars().count() <= HEADER_PATH_MAX,
            "oversized segment not cut: {} cols",
            got.chars().count()
        );
        assert!(
            std::path::Path::new(&got)
                .extension()
                .is_some_and(|e| e == "rs"),
            "extension lost: {got:?}"
        );
    }

    // A multi-line script
    // cannot be read on one row: keep the first line and say so, rather than flattening the whole
    // thing into a smear.
    #[test]
    fn test_header_command_first_line_and_width() {
        use crate::text::HEADER_CMD_MAX;
        assert_eq!(
            header_command("npm run build\nnpm test\nnpm publish"),
            "npm run build …"
        );

        let long = "for f in $(find . -name '*.rs'); do echo checking $f; rustfmt --check $f; done";
        let got = header_command(long);
        assert!(
            got.chars().count() <= HEADER_CMD_MAX,
            "not truncated: {} cols",
            got.chars().count()
        );
        assert!(
            got.ends_with('…') && got.starts_with("for f in"),
            "truncated from the wrong end: {got:?}"
        );

        // The rest of the ladder: trimming, tab flattening, the empty form and the exact cap.
        assert_eq!(header_command("git status"), "git status");
        assert_eq!(header_command("  git\tstatus  "), "git status");
        assert_eq!(header_command(""), "");
        let out = header_command(&"x".repeat(100));
        assert_eq!(out.chars().count(), HEADER_CMD_MAX);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn print_tool_result_rows_are_unstyled() {
        assert_eq!(
            print_tool_result_lines("a\nb", false),
            vec!["  ⎿ a", "    b"]
        );
        assert_eq!(print_tool_result_lines("", true), vec!["  ⎿ (no output)"]);
    }
}
