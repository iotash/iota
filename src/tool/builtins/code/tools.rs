//! The six code tools: `glob`, `grep`, `list_dir`, `read_file` (parallel-safe readers) and `edit_file`,
//! `write_file` (approval-gated writers). Descriptions and schemas are model-facing text: change them only
//! by decision.

use std::{
    collections::{BTreeSet, HashSet},
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{
    Artifact, ArtifactKind, Presentation, Tool, ToolOutput, ToolResult, post_artifact,
};
use crate::{BoxFuture, text};
use serde_json::{Value, json};

use super::{
    CODE_GLOB_COLLECT_CAP, CODE_GREP_MAX_FILE_BYTES, CODE_GREP_MAX_LINE_LEN, CODE_MAX_DIR_ENTRIES,
    CODE_MAX_FILE_BYTES, CODE_MAX_GLOB_RESULTS, CODE_MAX_GREP_CONTEXT, CODE_MAX_GREP_MATCHES,
    CODE_MAX_OUTPUT, CodeSet, byte_count, looks_binary, walk,
};
use crate::tool::args::{bool_arg, int_arg, read_file_limited, str_arg};

/// `glob`: newest-first file matches under the root.
pub(crate) struct Glob(pub(crate) Arc<CodeSet>);
/// `grep`: regex search with context.
pub(crate) struct Grep(pub(crate) Arc<CodeSet>);
/// `list_dir`: one directory level.
pub(crate) struct ListDir(pub(crate) Arc<CodeSet>);
/// `read_file`: numbered line window.
pub(crate) struct ReadFile(pub(crate) Arc<CodeSet>);
/// `edit_file`: exact-string replacement after a fresh read.
pub(crate) struct EditFile(pub(crate) Arc<CodeSet>);
/// `write_file`: whole-file write after a fresh read of an existing file.
pub(crate) struct WriteFile(pub(crate) Arc<CodeSet>);

/// A JSON object literal as a [`JsonObject`]; `None` is unreachable for the six schema literals below.
fn schema(v: Value) -> Option<JsonObject> {
    match v {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// The four readers hand their filesystem work to the blocking pool so a parallel round really overlaps
/// (POLICY, "blocking filesystem work runs under `spawn_blocking` where it matters"). A join failure can only
/// mean the runtime is going away, which the run reports as an interruption.
async fn blocking<F: FnOnce() -> ToolOutput + Send + 'static>(f: F) -> ToolResult {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ToolError::Cancelled)
}

impl Tool for Glob {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob".to_owned(),
            description: "Find files by name pattern under the project root. Patterns match root-relative paths \
                and support * ? and ** (a pattern without \"/\" matches at any depth, e.g. \"*.rs\"). Results \
                are newest-first. .git and root-.gitignore matches are excluded."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern, e.g. \"**/*.rs\" or \"src/*.rs\".",
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional directory to search, relative to the project root (default: the root).",
                    },
                },
                "required": ["pattern"],
            })),
            deferred: false,
        }
    }

    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        Box::pin(blocking(move || glob_call(&cs, &args)))
    }

    /// Readers are parallel-safe.
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        true
    }
}

impl Tool for Grep {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "grep".to_owned(),
            description: "Search file contents under the project root with a regular expression (Rust regex syntax: RE2-like, no backreferences or lookaround). \
                Output lines are \"path:line: text\" (context lines use \"-\" instead of \":\"). Binary files, \
                .git, and root-.gitignore matches are skipped."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression to search for (Rust regex syntax).",
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional directory to search, relative to the project root (default: the root).",
                    },
                    "include": {
                        "type": "string",
                        "description": "Optional filename glob filter, e.g. \"*.rs\" or \"src/**\".",
                    },
                    "context": {
                        "type": "integer",
                        "description": "Lines of context to show around each match (0-10, default 0).",
                    },
                },
                "required": ["pattern"],
            })),
            deferred: false,
        }
    }

    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        Box::pin(blocking(move || grep_call(&cs, &args)))
    }

    /// Readers are parallel-safe.
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        true
    }
}

impl Tool for ListDir {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_dir".to_owned(),
            description: "List one directory level under the project root: directories with a trailing \"/\", \
                files with their size. Defaults to the project root itself."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list, relative to the project root (default: the root).",
                    },
                },
            })),
            deferred: false,
        }
    }

    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        Box::pin(blocking(move || list_dir_call(&cs, &args)))
    }

    /// Readers are parallel-safe.
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        true
    }

    /// The path IS the call for the file tools (the D-12 lift).
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(self.0.header_arg(args))
    }
}

impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".to_owned(),
            description: "Read a text file inside the project root and return its content with line numbers, \
                windowed by the optional \"offset\" (1-based first line) and \"limit\" (line count). Paths are \
                relative to the project root. Reading a file is required before editing it."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the project root.",
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based line number to start reading from (default 1).",
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: all remaining lines).",
                    },
                },
                "required": ["path"],
            })),
            deferred: false,
        }
    }

    /// Reads through the numbered window, with POLICY fix F-06 for an oversized single line.
    fn call<'a>(&'a self, _cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        Box::pin(blocking(move || read_file_call(&cs, &args)))
    }

    /// Readers are parallel-safe.
    fn supports_parallel(&self, _args: Option<&JsonObject>) -> bool {
        true
    }

    /// The path IS the call for the file tools (the D-12 lift).
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(self.0.header_arg(args))
    }
}

impl Tool for EditFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit_file".to_owned(),
            description: "Replace an exact string in a file inside the project root. \"old_string\" must match \
                the file content exactly (including whitespace and indentation) and must be unique in the file \
                unless \"replace_all\" is set — extend it with surrounding lines to disambiguate. The file must \
                have been read with read_file first."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the project root.",
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to replace.",
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text.",
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence instead of requiring uniqueness (default false).",
                    },
                },
                "required": ["path", "old_string", "new_string"],
            })),
            deferred: false,
        }
    }

    /// Posts the T-35 diff artifact. Never parallel, so it runs in place rather than
    /// copying a whole file's `new_string` onto the blocking pool.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { Ok(edit_file_call(&self.0, cx, args)) })
    }

    /// `!auto_write`, whatever the call.
    fn requires_approval(&self, _args: Option<&JsonObject>) -> bool {
        !self.0.auto_write
    }

    /// Writers are expanded.
    fn presentation(&self) -> Presentation {
        Presentation::Expanded
    }

    /// The path IS the call for the file tools (the D-12 lift) — its
    /// `new_string` must never reach a header, so the empty summary stands.
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(self.0.header_arg(args))
    }
}

impl Tool for WriteFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".to_owned(),
            description: "Create or overwrite a whole file inside the project root (parent directories are \
                created). Overwriting an existing file requires reading it with read_file first — prefer \
                edit_file for changes inside an existing file."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the project root.",
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file content to write.",
                    },
                },
                "required": ["path", "content"],
            })),
            deferred: false,
        }
    }

    /// Posts the T-35 diff artifact. Never parallel, so it runs in place rather than
    /// copying a whole file's `content` onto the blocking pool.
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { Ok(write_file_call(&self.0, cx, args)) })
    }

    /// `!auto_write`, whatever the call.
    fn requires_approval(&self, _args: Option<&JsonObject>) -> bool {
        !self.0.auto_write
    }

    /// Writers are expanded.
    fn presentation(&self) -> Presentation {
        Presentation::Expanded
    }

    /// The path IS the call for the file tools (the D-12 lift) — its
    /// `content` must never reach a header, so the empty summary stands.
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(self.0.header_arg(args))
    }
}

// ---- glob ----

fn glob_call(cs: &CodeSet, args: &JsonObject) -> ToolOutput {
    let raw = str_arg(args, "pattern").trim();
    if raw.is_empty() {
        return ToolOutput::err("missing required argument: pattern");
    }
    // A pattern without a separator matches at any depth, root level included.
    let pattern = if raw.contains('/') {
        raw.to_owned()
    } else {
        format!("**/{raw}")
    };
    let Ok(compiled) = globset::GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
    else {
        return ToolOutput::err(format!("invalid glob pattern: {pattern}"));
    };
    let matcher = compiled.compile_matcher();
    let base = match code_base_dir(cs, args) {
        Ok(b) => b,
        Err(t) => return t,
    };

    let mut hits: Vec<(String, SystemTime)> = Vec::new();
    walk::walk_files(&cs.root, &base, &mut |_abs, rel, entry| {
        if matcher.is_match(rel) {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            hits.push((rel.to_owned(), mtime));
        }
        hits.len() < CODE_GLOB_COLLECT_CAP
    });

    if hits.is_empty() {
        return ToolOutput::ok(format!("no files match {pattern}"));
    }
    // Stable, so equal mtimes keep the walk's lexical order.
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));
    let shown = hits.len().min(CODE_MAX_GLOB_RESULTS);
    let mut out = String::new();
    for (rel, _) in &hits[..shown] {
        out.push_str(rel);
        out.push('\n');
    }
    if hits.len() > shown {
        let _ = write!(
            out,
            "[showing the {shown} newest of {} matches; narrow the pattern to see the rest]",
            hits.len()
        );
    }
    ToolOutput::ok(out.trim_end_matches('\n').to_owned())
}

// ---- grep ----

fn grep_call(cs: &CodeSet, args: &JsonObject) -> ToolOutput {
    // The pattern is NOT trimmed here: leading space is part of the regex.
    let pattern = str_arg(args, "pattern");
    if pattern.is_empty() {
        return ToolOutput::err("missing required argument: pattern");
    }
    let re = match regex::Regex::new(pattern) {
        Ok(re) => re,
        Err(e) => return ToolOutput::err(format!("invalid regular expression: {e}")),
    };
    let base = match code_base_dir(cs, args) {
        Ok(b) => b,
        Err(t) => return t,
    };
    let include = str_arg(args, "include").trim();
    let include_matcher = if include.is_empty() {
        None
    } else {
        match globset::GlobBuilder::new(include)
            .literal_separator(true)
            .build()
        {
            Ok(g) => Some(g.compile_matcher()),
            Err(_) => return ToolOutput::err(format!("invalid include pattern: {include}")),
        }
    };
    let ctx_lines = usize::try_from(int_arg(args, "context").clamp(0, CODE_MAX_GREP_CONTEXT))
        .unwrap_or_default();

    let mut buf = String::new();
    let mut total = 0usize;
    let mut capped = false;
    walk::walk_files(&cs.root, &base, &mut |abs, rel, entry| {
        if let Some(m) = &include_matcher
            && !match_include(m, include, rel)
        {
            return true;
        }
        let Ok(meta) = entry.metadata() else {
            return true;
        };
        if meta.len() > CODE_GREP_MAX_FILE_BYTES {
            return true;
        }
        let Ok((data, _)) = read_file_limited(abs, CODE_GREP_MAX_FILE_BYTES) else {
            return true;
        };
        if looks_binary(&data) {
            return true;
        }
        let content = String::from_utf8_lossy(&data);
        // strings.Split, not splitLines: a trailing newline yields a final empty (matchable) line and '\r' stays.
        let lines: Vec<&str> = content.split('\n').collect();
        let mut hits: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                hits.push(i);
                if total + hits.len() >= CODE_MAX_GREP_MATCHES {
                    break;
                }
            }
        }
        if hits.is_empty() {
            return true;
        }
        total += hits.len();
        emit_grep_file(&mut buf, rel, &lines, &hits, ctx_lines);
        if total >= CODE_MAX_GREP_MATCHES || buf.len() > CODE_MAX_OUTPUT {
            capped = true;
            return false;
        }
        true
    });

    if total == 0 {
        return ToolOutput::ok(format!("no matches for {pattern}"));
    }
    let mut out = buf.trim_end_matches('\n').to_owned();
    if capped {
        let _ = write!(
            out,
            "\n[stopped after {total} matches; refine the pattern or use include to narrow the search]"
        );
    }
    ToolOutput::ok(out)
}

/// A pattern without `/` matches the basename, otherwise the root-relative path.
fn match_include(matcher: &globset::GlobMatcher, pattern: &str, rel: &str) -> bool {
    let target = if pattern.contains('/') {
        rel
    } else {
        rel.rsplit_once('/').map_or(rel, |(_, base)| base)
    };
    matcher.is_match(target)
}

/// One file's hit and context rows, in ascending line order.
fn emit_grep_file(buf: &mut String, rel: &str, lines: &[&str], hits: &[usize], ctx_lines: usize) {
    let is_hit: HashSet<usize> = hits.iter().copied().collect();
    let mut show: BTreeSet<usize> = BTreeSet::new();
    for &h in hits {
        for i in h.saturating_sub(ctx_lines)..=h.saturating_add(ctx_lines) {
            if i < lines.len() {
                show.insert(i);
            }
        }
    }
    for i in show {
        let raw = lines[i];
        let line = if raw.len() > CODE_GREP_MAX_LINE_LEN {
            format!(
                "{}…",
                text::truncate_to_char_boundary(raw, CODE_GREP_MAX_LINE_LEN)
            )
        } else {
            raw.to_owned()
        };
        let sep = if is_hit.contains(&i) { ':' } else { '-' };
        let _ = writeln!(buf, "{rel}:{}{sep} {line}", i + 1);
    }
}

// ---- list_dir ----

fn list_dir_call(cs: &CodeSet, args: &JsonObject) -> ToolOutput {
    let base = match code_base_dir(cs, args) {
        Ok(b) => b,
        Err(t) => return t,
    };
    let read = match fs::read_dir(&base) {
        Ok(r) => r,
        Err(e) => return ToolOutput::err(format!("cannot list {}: {e}", cs.display(&base))),
    };
    // os.ReadDir sorts by filename; std::fs::read_dir does not.
    let mut entries: Vec<fs::DirEntry> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    if entries.is_empty() {
        return ToolOutput::ok("[directory is empty]");
    }
    let mut out = String::new();
    for (shown, e) in entries.iter().enumerate() {
        if shown == CODE_MAX_DIR_ENTRIES {
            let _ = write!(out, "[showing {shown} of {} entries]", entries.len());
            break;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        // Lstat semantics: a symlink shows its own size and is never listed as a directory.
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = writeln!(out, "{name}/");
        } else if let Ok(meta) = e.metadata() {
            let _ = writeln!(out, "{name} ({})", byte_count(meta.len()));
        } else {
            let _ = writeln!(out, "{name}");
        }
    }
    ToolOutput::ok(out.trim_end_matches('\n').to_owned())
}

// ---- read_file ----

fn read_file_call(cs: &CodeSet, args: &JsonObject) -> ToolOutput {
    let abs = match cs.resolve(str_arg(args, "path")) {
        Ok(p) => p,
        Err(t) => return t,
    };
    // These errors name the ABSOLUTE path, unlike every other message here.
    let (data, size) = match read_file_limited(&abs, CODE_MAX_FILE_BYTES) {
        Ok(v) => v,
        Err(t) => return t,
    };
    let display = cs.display(&abs);
    if looks_binary(&data) {
        return ToolOutput::err(format!(
            "{display} looks like a binary file ({}); read_file only serves text",
            byte_count(size)
        ));
    }
    // The ledger is stamped before the window checks, so an offset-past-end call still counts as a read.
    cs.note_read(&abs);
    if data.is_empty() {
        return ToolOutput::ok("[file is empty]");
    }
    let content = String::from_utf8_lossy(&data);
    let mut out = match numbered_window(&content, args, &display) {
        Ok(o) => o,
        Err(t) => return t,
    };
    if size > CODE_MAX_FILE_BYTES {
        let _ = write!(
            out,
            "\n[file is {}; only the first {} MB was read]",
            byte_count(size),
            CODE_MAX_FILE_BYTES / (1024 * 1024)
        );
    }
    ToolOutput::ok(out)
}

/// The numbered line window, with POLICY fix F-06: an oversized single row is cut to `CODE_MAX_OUTPUT - 4`
/// bytes so that the row plus its `…\n` still fits the window (cut to the full cap, the row was then
/// rejected — an empty window the model could not escape).
fn numbered_window(content: &str, args: &JsonObject, display: &str) -> Result<String, ToolOutput> {
    let lines = text::split_lines(content);
    let total = lines.len();
    let total_i = i64::try_from(total).unwrap_or(i64::MAX);
    let offset = int_arg(args, "offset").max(1);
    if offset > total_i {
        return Err(ToolOutput::err(format!(
            "offset {offset} is past the end of {display} ({total} lines)"
        )));
    }
    let start = usize::try_from(offset).unwrap_or(1);
    let mut end = total;
    let limit = int_arg(args, "limit");
    if limit > 0 {
        let stop = (start - 1).saturating_add(usize::try_from(limit).unwrap_or(usize::MAX));
        if stop < end {
            end = stop;
        }
    }

    let mut buf = String::new();
    let mut last = start - 1; // last line number actually emitted
    for (i, line) in lines.iter().enumerate().take(end).skip(start - 1) {
        let mut row = format!("{:>6}\t{line}\n", i + 1);
        if row.len() > CODE_MAX_OUTPUT {
            row = format!(
                "{}…\n",
                text::truncate_to_char_boundary(&row, CODE_MAX_OUTPUT - 4)
            );
        }
        if buf.len() + row.len() > CODE_MAX_OUTPUT {
            break;
        }
        buf.push_str(&row);
        last = i + 1;
    }
    let mut out = buf.trim_end_matches('\n').to_owned();
    if last < end {
        let _ = write!(
            out,
            "\n[output truncated — showing lines {start}-{last} of {total}; call read_file with offset={} to continue]",
            last + 1
        );
    } else if start > 1 || end < total {
        let _ = write!(out, "\n[showing lines {start}-{end} of {total}]");
    }
    Ok(out)
}

// ---- edit_file ----

/// Computes the unified diff between old and new content and posts it as the call's
/// display artifact (the D-19 lift, T-35). Hunks only: the `---`/`+++`
/// file header would duplicate the title, and the display is for the user's eyes — the
/// model-facing result text stays untouched (a full diff there costs tokens). An empty
/// diff posts nothing; headless runs inject no slot, so the post is a no-op there.
fn post_diff(cx: &RunCtx, display: &str, old: &str, new: &str) {
    let unified = super::udiff::unified(display, display, old, new);
    let mut lines: Vec<&str> = if unified.is_empty() {
        Vec::new()
    } else {
        unified.split('\n').collect()
    };
    if lines.last() == Some(&"") {
        lines.pop(); // the final row's trailing newline
    }
    while lines
        .first()
        .is_some_and(|l| l.starts_with("--- ") || l.starts_with("+++ "))
    {
        lines.remove(0);
    }
    if lines.is_empty() {
        return;
    }
    post_artifact(
        cx,
        Artifact {
            kind: ArtifactKind::Diff,
            title: display.to_owned(),
            lines: lines.into_iter().map(str::to_owned).collect(),
        },
    );
}

fn edit_file_call(cs: &CodeSet, cx: &RunCtx, args: &JsonObject) -> ToolOutput {
    let abs = match cs.resolve(str_arg(args, "path")) {
        Ok(p) => p,
        Err(t) => return t,
    };
    let old_string = str_arg(args, "old_string");
    let new_string = str_arg(args, "new_string");
    let replace_all = bool_arg(args, "replace_all", false);
    let display = cs.display(&abs);
    if old_string.is_empty() {
        return ToolOutput::err(
            "old_string must not be empty (use write_file to create or replace a whole file)",
        );
    }
    if old_string == new_string {
        return ToolOutput::err("old_string and new_string are identical");
    }
    if let Err(t) = cs.require_fresh_read(&abs) {
        return t;
    }
    let meta = match fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => return ToolOutput::err(format!("cannot access {display}: {e}")),
    };
    if meta.len() > CODE_MAX_FILE_BYTES {
        return ToolOutput::err(format!(
            "{display} is too large to edit ({})",
            byte_count(meta.len())
        ));
    }
    let (data, _) = match read_file_limited(&abs, CODE_MAX_FILE_BYTES) {
        Ok(v) => v,
        Err(t) => return t,
    };

    // Search and replace on BYTES, never on a decoded string. `String::from_utf8_lossy` used to stand here,
    // and its result was what got written back: every byte the file held that is not valid UTF-8 — a Latin-1
    // `é`, a Shift-JIS or GBK lead byte — came back as `EF BF BD`, ACROSS THE WHOLE FILE and not just the
    // edited span, and the diff built from the same buffer showed none of it. `old_string`/`new_string` are
    // JSON strings, so they are already valid UTF-8 and their bytes are all the needle we need.
    let old_bytes = old_string.as_bytes();
    let hits: Vec<usize> = memchr::memmem::find_iter(&data, old_bytes).collect();
    let count = hits.len();
    if count == 0 {
        return ToolOutput::err(format!(
            "old_string not found in {display} — it must match the file content exactly, whitespace included; read the file again if unsure"
        ));
    }
    if count > 1 && !replace_all {
        return ToolOutput::err(format!(
            "old_string appears {count} times in {display}; extend it with surrounding context to make it unique, or set replace_all"
        ));
    }

    let cut = if replace_all { &hits[..] } else { &hits[..1] };
    let updated = splice(&data, cut, old_bytes.len(), new_string.as_bytes());
    // Truncate-and-write, not atomic — the file exists, so its mode is untouched.
    if let Err(e) = write_bytes(&abs, &updated) {
        return ToolOutput::err(format!("cannot write {display}: {e}"));
    }
    cs.note_read(&abs);
    // The file is on disk in full; what is left is for human and model eyes, and a terminal renders TEXT.
    // Both sides come from the same lossy conversion, so an undecodable byte outside the edit reads as the
    // same `\u{FFFD}` in both and cancels out of the diff instead of inventing a hunk.
    let before = String::from_utf8_lossy(&data);
    let after = String::from_utf8_lossy(&updated);
    post_diff(cx, &display, &before, &after);

    let done = if replace_all { count } else { 1 };
    // The first replacement lands at old_string's original offset. Newlines survive any decoding, so the
    // line number counted on bytes is the one the lossy snippet below numbers by.
    let at = hits.first().copied().unwrap_or_default();
    let line = memchr::memchr_iter(b'\n', &data[..at]).count() + 1;
    ToolOutput::ok(format!(
        "{done} replacement(s) in {display}\n\n{}",
        edit_snippet(&after, line)
    ))
}

/// Replaces `needle_len` bytes at each of `at` (ascending, non-overlapping) with `replacement`, copying
/// every other byte through untouched.
fn splice(data: &[u8], at: &[usize], needle_len: usize, replacement: &[u8]) -> Vec<u8> {
    let grown = (data.len() + at.len() * replacement.len()).saturating_sub(at.len() * needle_len);
    let mut out = Vec::with_capacity(grown);
    let mut cursor = 0;
    for &start in at {
        out.extend_from_slice(&data[cursor..start]);
        out.extend_from_slice(replacement);
        cursor = start + needle_len;
    }
    out.extend_from_slice(&data[cursor..]);
    out
}

/// A few numbered lines around the first change.
fn edit_snippet(content: &str, line: usize) -> String {
    let lines = text::split_lines(content);
    let from = line.saturating_sub(3).max(1);
    let to = line.saturating_add(3).min(lines.len());
    let mut out = String::new();
    for i in from..=to {
        let _ = writeln!(out, "{:>6}\t{}", i, lines[i - 1]);
    }
    out.trim_end_matches('\n').to_owned()
}

// ---- write_file ----

fn write_file_call(cs: &CodeSet, cx: &RunCtx, args: &JsonObject) -> ToolOutput {
    let abs = match cs.resolve(str_arg(args, "path")) {
        Ok(p) => p,
        Err(t) => return t,
    };
    // An empty string is a valid content; only a missing or non-string value is not.
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return ToolOutput::err("missing required argument: content");
    };
    let display = cs.display(&abs);

    // Any stat failure (not only NotFound) means "new file": no read gate, mode 0644.
    let mut created = true;
    let mut old = String::new();
    let mut old_ok = true;
    if let Ok(meta) = fs::metadata(&abs) {
        if meta.is_dir() {
            return ToolOutput::err(format!("{display} is a directory"));
        }
        if !meta.is_file() {
            return ToolOutput::err(format!("{display} is not a regular file"));
        }
        if let Err(t) = cs.require_fresh_read(&abs) {
            return t;
        }
        created = false;
        // The previous content only feeds the display diff; a file too large to read
        // whole would produce a lying diff, so it produces none.
        if meta.len() <= CODE_MAX_FILE_BYTES {
            match read_file_limited(&abs, CODE_MAX_FILE_BYTES) {
                Ok((data, _)) => old = String::from_utf8_lossy(&data).into_owned(),
                Err(_) => old_ok = false,
            }
        } else {
            old_ok = false;
        }
    }

    if let Some(parent) = abs.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return ToolOutput::err(format!("cannot create parent directory for {display}: {e}"));
    }
    if let Err(e) = write_bytes(&abs, content.as_bytes()) {
        return ToolOutput::err(format!("cannot write {display}: {e}"));
    }
    cs.note_read(&abs);
    if old_ok {
        post_diff(cx, &display, &old, content);
    }

    let verb = if created { "created" } else { "overwritten" };
    ToolOutput::ok(format!(
        "wrote {} to {display} ({verb})",
        byte_count(u64::try_from(content.len()).unwrap_or(u64::MAX))
    ))
}

// ---- shared helpers ----

/// The optional `path` argument as a search base; empty → the root.
fn code_base_dir(cs: &CodeSet, args: &JsonObject) -> Result<PathBuf, ToolOutput> {
    let raw = str_arg(args, "path");
    if raw.trim().is_empty() {
        return Ok(cs.root.clone());
    }
    let abs = cs.resolve(raw)?;
    // A stat failure and a non-directory give the same text, and it names the RAW argument.
    match fs::metadata(&abs) {
        Ok(m) if m.is_dir() => Ok(abs),
        _ => Err(ToolOutput::err(format!("not a directory: {raw}"))),
    }
}

/// `os.WriteFile(path, data, 0o644)`: create-or-truncate; the mode applies only when the file is created.
fn write_bytes(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o644);
    }
    opts.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use crate::provider::model::JsonObject;
    use crate::tool::ToolOutput;
    use serde_json::json;

    use super::{
        CODE_MAX_OUTPUT, edit_snippet, emit_grep_file, match_include, numbered_window, splice,
    };

    fn args(v: serde_json::Value) -> JsonObject {
        match v {
            serde_json::Value::Object(m) => m,
            _ => JsonObject::new(),
        }
    }

    #[test]
    fn numbered_window_windows_and_marks() {
        let content = "l1\nl2\nl3\nl4\nl5\n";
        assert_eq!(
            numbered_window(content, &args(json!({})), "f.txt"),
            Ok("     1\tl1\n     2\tl2\n     3\tl3\n     4\tl4\n     5\tl5".to_owned())
        );
        assert_eq!(
            numbered_window(content, &args(json!({"offset": 2, "limit": 2})), "f.txt"),
            Ok("     2\tl2\n     3\tl3\n[showing lines 2-3 of 5]".to_owned())
        );
        // A zero/negative offset is 1, and a limit past the end is the whole file (no marker).
        assert_eq!(
            numbered_window(content, &args(json!({"offset": 0, "limit": 99})), "f.txt"),
            numbered_window(content, &args(json!({})), "f.txt")
        );
        assert_eq!(
            numbered_window(content, &args(json!({"offset": 9})), "f.txt"),
            Err(ToolOutput::err(
                "offset 9 is past the end of f.txt (5 lines)"
            ))
        );
    }

    // POLICY F-06: the oversized row is cut so it fits, instead of leaving an empty window.
    #[test]
    fn numbered_window_cuts_one_oversized_row_to_fit() {
        let content = format!("{}\n", "x".repeat(CODE_MAX_OUTPUT + 4096));
        let out = numbered_window(&content, &args(json!({})), "big.txt").expect("window");
        assert!(out.ends_with('…'), "the cut row keeps its ellipsis");
        assert_eq!(out.len(), CODE_MAX_OUTPUT - 1); // the trailing "\n" is trimmed
        assert!(!out.contains("[output truncated"));
    }

    #[test]
    fn edit_snippet_is_three_lines_either_side() {
        let content = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        assert_eq!(
            edit_snippet(content, 1),
            "     1\t1\n     2\t2\n     3\t3\n     4\t4"
        );
        assert_eq!(
            edit_snippet(content, 5),
            "     2\t2\n     3\t3\n     4\t4\n     5\t5\n     6\t6\n     7\t7\n     8\t8"
        );
        assert_eq!(edit_snippet("", 1), "");
    }

    // The byte splice `edit_file` replaces with: shorter, longer and equal replacements, and every byte
    // outside a match copied through whatever it is.
    #[test]
    fn splice_copies_every_other_byte_through() {
        let data = b"\xff a \xff a \xff";
        let hits: Vec<usize> = memchr::memmem::find_iter(data, b"a").collect();
        assert_eq!(hits, vec![2, 6]);
        assert_eq!(
            splice(data, &hits, 1, b"bb"),
            b"\xff bb \xff bb \xff".to_vec()
        );
        assert_eq!(
            splice(data, &hits[..1], 1, b"bb"),
            b"\xff bb \xff a \xff".to_vec()
        );
        assert_eq!(splice(data, &hits, 1, b""), b"\xff  \xff  \xff".to_vec());
        assert_eq!(splice(data, &[], 1, b"bb"), data.to_vec());
        // A match at either end has no bytes on that side to copy.
        assert_eq!(splice(b"ab", &[0], 1, b"Z"), b"Zb".to_vec());
        assert_eq!(splice(b"ab", &[1], 1, b"Z"), b"aZ".to_vec());
    }

    #[test]
    fn grep_rows_and_include_targets() {
        let lines = ["alpha", "beta", "gamma", "delta"];
        let mut buf = String::new();
        emit_grep_file(&mut buf, "a/b.rs", &lines, &[1], 1);
        assert_eq!(buf, "a/b.rs:1- alpha\na/b.rs:2: beta\na/b.rs:3- gamma\n");

        // A line over 500 bytes is cut on a char boundary and suffixed with U+2026.
        let long = format!("{}é", "y".repeat(499));
        let mut buf = String::new();
        emit_grep_file(&mut buf, "x", &[long.as_str()], &[0], 0);
        assert_eq!(buf, format!("x:1: {}…\n", "y".repeat(499)));

        let no_slash = globset::Glob::new("*.md").expect("glob").compile_matcher();
        assert!(match_include(&no_slash, "*.md", "docs/readme.md"));
        assert!(!match_include(&no_slash, "*.md", "docs/readme.rs"));
        let with_slash = globset::GlobBuilder::new("cmd/**")
            .literal_separator(true)
            .build()
            .expect("glob")
            .compile_matcher();
        assert!(match_include(&with_slash, "cmd/**", "cmd/main.rs"));
        assert!(!match_include(&with_slash, "cmd/**", "tool/main.rs"));
    }
}
