//! The six code tools: `glob`, `grep`, `list_dir`, `read_file` (parallel-safe readers) and `edit_file`,
//! `write_file` (approval-gated writers). Descriptions and schemas are model-facing text: change them only
//! by decision.

use std::{
    collections::{BTreeSet, HashSet},
    fmt::Write as _,
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use crate::provider::model::{JsonObject, ToolDef};
use crate::tool::context::RunCtx;
use crate::tool::error::ToolError;
use crate::tool::{
    ApprovalSubject, Artifact, ArtifactKind, Presentation, Tool, ToolOutput, ToolResult,
    post_artifact,
};
use crate::{BoxFuture, text};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    CODE_BINARY_SNIFF_LEN, CODE_GLOB_COLLECT_CAP, CODE_GREP_MAX_FILE_BYTES, CODE_GREP_MAX_LINE_LEN,
    CODE_GREP_MAX_TOTAL_BYTES, CODE_MAX_DIR_ENTRIES, CODE_MAX_FILE_BYTES, CODE_MAX_GLOB_RESULTS,
    CODE_MAX_GREP_CONTEXT, CODE_MAX_GREP_MATCHES, CODE_MAX_OUTPUT, CODE_WALK_MAX_ENTRIES, CodeSet,
    byte_count, looks_binary, walk,
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

/// The four readers hand their filesystem work to the blocking pool (`spawn_blocking`) so a parallel round
/// really overlaps. A join failure can only mean the runtime is going away, which the run reports as an
/// interruption.
async fn blocking<F: FnOnce() -> ToolOutput + Send + 'static>(f: F) -> ToolResult {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ToolError::Cancelled)
}

impl Tool for Glob {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob".to_owned(),
            description: "Find files by name pattern, under the project root unless \"path\" names another \
                directory (reads are not restricted). Patterns match paths relative to the project root or \
                write root holding the searched directory, else to that directory, and support * ? and ** (a \
                pattern without \"/\" matches at any depth, e.g. \"*.rs\"). Results are newest-first. .git and \
                root-.gitignore matches are excluded."
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
                        "description": "Optional directory to search, relative to the project root or absolute (default: the root).",
                    },
                },
                "required": ["pattern"],
            })),
            deferred: false,
        }
    }

    /// The walk reads `cx.cancel`: nothing races the call's await against ESC, so the token is the
    /// only thing that stops the walk (and with it the call).
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        let cancel = cx.cancel.clone();
        Box::pin(blocking(move || glob_call(&cs, &args, &cancel)))
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
            description: "Search file contents with a regular expression (Rust regex syntax: RE2-like, no backreferences or lookaround), \
                under the project root unless \"path\" names another directory (reads are not restricted). Output lines are \"path:line: text\" (context lines use \"-\" instead of \":\"). Binary files, \
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
                        "description": "Optional directory to search, relative to the project root or absolute (default: the root).",
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

    /// The walk reads `cx.cancel`: nothing races the call's await against ESC, so the token is the
    /// only thing that stops the walk (and with it the call).
    fn call<'a>(&'a self, cx: &'a RunCtx, args: &'a JsonObject) -> BoxFuture<'a, ToolResult> {
        let cs = Arc::clone(&self.0);
        let args = args.clone();
        let cancel = cx.cancel.clone();
        Box::pin(blocking(move || grep_call(&cs, &args, &cancel)))
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
            description: "List one directory level: directories with a trailing \"/\", files with their size. \
                Defaults to the project root itself; any other directory may be listed (reads are not restricted)."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list, relative to the project root or absolute (default: the root).",
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
            description: "Read a text file and return its content with line numbers, windowed by the optional \
                \"offset\" (1-based first line) and \"limit\" (line count). Paths are relative to the project \
                root or absolute; reads are not restricted. Reading a file is required before editing it."
                .to_owned(),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the project root or absolute.",
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

    /// Reads through the numbered window, with the DIVERGENCES F-06 fix for an oversized single line.
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
            description: format!(
                "Replace an exact string in a file inside {}. \"old_string\" must match the file content \
                 exactly (including whitespace and indentation) and must be unique in the file unless \
                 \"replace_all\" is set — extend it with surrounding lines to disambiguate. The file must have \
                 been read with read_file first.{}",
                self.0.write_scope(),
                self.0.outside_clause()
            ),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the project root or absolute.",
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

    /// Inside the roots `!auto_write`; outside them always under `outside_writes: ask` (see
    /// [`CodeSet::write_asks`]).
    fn requires_approval(&self, args: Option<&JsonObject>) -> bool {
        self.0.write_asks(args)
    }

    /// `auto_write` — it waives the approval of writes inside the roots only ([`CodeSet::waives`]).
    fn waives_approval(&self) -> bool {
        self.0.waives()
    }

    /// Writers are expanded.
    fn presentation(&self) -> Presentation {
        Presentation::Expanded
    }

    /// An outside write is bound to its real location ([`writer_subject`]).
    fn approval_subject(&self, args: &JsonObject) -> Option<ApprovalSubject> {
        writer_subject(&self.0, args)
    }

    /// The path IS the call for the file tools (the D-12 lift) — its
    /// `new_string` must never reach a header, so the empty summary stands. An outside write
    /// shows its real location, marked ([`writer_summary`]).
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(writer_summary(&self.0, args))
    }
}

impl Tool for WriteFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".to_owned(),
            description: format!(
                "Create or overwrite a whole file inside {} (parent directories are created). Overwriting an \
                 existing file requires reading it with read_file first — prefer edit_file for changes inside an \
                 existing file.{}",
                self.0.write_scope(),
                self.0.outside_clause()
            ),
            input_schema: schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the project root or absolute.",
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

    /// Inside the roots `!auto_write`; outside them always under `outside_writes: ask` (see
    /// [`CodeSet::write_asks`]).
    fn requires_approval(&self, args: Option<&JsonObject>) -> bool {
        self.0.write_asks(args)
    }

    /// `auto_write` — it waives the approval of writes inside the roots only ([`CodeSet::waives`]).
    fn waives_approval(&self) -> bool {
        self.0.waives()
    }

    /// Writers are expanded.
    fn presentation(&self) -> Presentation {
        Presentation::Expanded
    }

    /// An outside write is bound to its real location ([`writer_subject`]).
    fn approval_subject(&self, args: &JsonObject) -> Option<ApprovalSubject> {
        writer_subject(&self.0, args)
    }

    /// The path IS the call for the file tools (the D-12 lift) — its
    /// `content` must never reach a header, so the empty summary stands. An outside write
    /// shows its real location, marked ([`writer_summary`]).
    fn header_summary(&self, args: &JsonObject) -> Option<String> {
        Some(writer_summary(&self.0, args))
    }
}

/// A writer's header, and so its approval prompt: the path as the readers show it, except a write the user
/// would be asked about outside every root, which shows the RESOLVED real location, absolute, marked
/// `(outside the project root)` — what the user approves is where the bytes land, not the argument as typed.
fn writer_summary(cs: &CodeSet, args: &JsonObject) -> String {
    match cs.approvable(str_arg(args, "path")) {
        Some(real) => super::outside_summary(&real),
        None => cs.header_arg(args),
    }
}

/// An outside write's approval subject, from ONE resolution of its real location: the prompt names it
/// (marked), "allow once" admits exactly it, "allow for this session" grants its directory
/// ([`super::grant_dir`]). `None` inside the roots (the approval is about the tool, as it always was).
fn writer_subject(cs: &CodeSet, args: &JsonObject) -> Option<ApprovalSubject> {
    let real = cs.approvable(str_arg(args, "path"))?;
    Some(ApprovalSubject {
        detail: super::outside_summary(&real),
        scope: super::grant_dir(&real).to_path_buf(),
        target: real,
    })
}

// ---- glob ----

/// The line a walk that did not run to the end closes its result with, so the model never takes a
/// partial answer for the whole one: `[search cancelled]` when the run's token stopped it (ESC
/// cancels the call, not the turn, so the model reads this result), `[stopped after reading
/// {CODE_GREP_MAX_TOTAL_BYTES} MB of files; …]` when `grep` spent its read budget, `[stopped after
/// visiting {CODE_WALK_MAX_ENTRIES} entries; …]` for a capped walk; `None` for a walk that finished
/// (or that the visitor stopped, which says so itself).
fn walk_note(walked: &walk::Walked, spent: bool) -> Option<String> {
    let what = match walked {
        walk::Walked::Cancelled => return Some("[search cancelled]".to_owned()),
        _ if spent => format!("reading {} MB of files", CODE_GREP_MAX_TOTAL_BYTES >> 20),
        walk::Walked::Capped => format!("visiting {CODE_WALK_MAX_ENTRIES} entries"),
        walk::Walked::Done | walk::Walked::Stopped => return None,
    };
    Some(format!(
        "[stopped after {what}; narrow \"path\" to search further]"
    ))
}

fn glob_call(cs: &CodeSet, args: &JsonObject, cancel: &CancellationToken) -> ToolOutput {
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
    let walked = walk::walk_files(cs.anchor(&base), &base, cancel, &mut |abs, rel, entry| {
        if matcher.is_match(rel) {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            hits.push((cs.display(abs), mtime));
        }
        hits.len() < CODE_GLOB_COLLECT_CAP
    });

    let note = walk_note(&walked, false);
    if hits.is_empty() {
        let empty = format!("no files match {pattern}");
        return ToolOutput::ok(match note {
            Some(note) => format!(
                "{empty}
{note}"
            ),
            None => empty,
        });
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
    let mut out = out.trim_end_matches('\n').to_owned();
    if let Some(note) = note {
        let _ = write!(out, "\n{note}");
    }
    ToolOutput::ok(out)
}

// ---- grep ----

fn grep_call(cs: &CodeSet, args: &JsonObject, cancel: &CancellationToken) -> ToolOutput {
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
    // Bytes read so far: a grep's cost is what it reads, so the walk's entry cap alone would still let
    // one over `/` read gigabytes.
    let mut read = 0u64;
    let mut spent = false;
    let walked = walk::walk_files(cs.anchor(&base), &base, cancel, &mut |abs, rel, _| {
        if read >= CODE_GREP_MAX_TOTAL_BYTES {
            spent = true;
            return false;
        }
        if let Some(m) = &include_matcher
            && !match_include(m, include, rel)
        {
            return true;
        }
        let Some(data) = grep_text(abs, &mut read) else {
            return true;
        };
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
        emit_grep_file(&mut buf, &cs.display(abs), &lines, &hits, ctx_lines);
        if total >= CODE_MAX_GREP_MATCHES || buf.len() > CODE_MAX_OUTPUT {
            capped = true;
            return false;
        }
        true
    });

    let note = walk_note(&walked, spent);
    if total == 0 {
        let empty = format!("no matches for {pattern}");
        return ToolOutput::ok(match note {
            Some(note) => format!(
                "{empty}
{note}"
            ),
            None => empty,
        });
    }
    let mut out = buf.trim_end_matches('\n').to_owned();
    if capped {
        let _ = write!(
            out,
            "\n[stopped after {total} matches; refine the pattern or use include to narrow the search]"
        );
    }
    if let Some(note) = note {
        let _ = write!(out, "\n{note}");
    }
    ToolOutput::ok(out)
}

/// The bytes `grep` searches in `abs`, or `None` for anything but a readable regular text file of at
/// most [`CODE_GREP_MAX_FILE_BYTES`] (a link is judged by its target); every byte read, a binary
/// file's sniff included, is added to `read`, the call's budget. The first
/// [`CODE_BINARY_SNIFF_LEN`] bytes are sniffed before the rest is read, so a binary file costs one
/// small read, not up to 10 MB — ripgrep's order, and what keeps a walk over `/` affordable.
fn grep_text(abs: &Path, read: &mut u64) -> Option<Vec<u8>> {
    let meta = fs::metadata(abs).ok()?;
    if !meta.is_file() || meta.len() > CODE_GREP_MAX_FILE_BYTES {
        return None;
    }
    let mut file = fs::File::open(abs).ok()?;
    let mut data = Vec::new();
    let sniff = u64::try_from(CODE_BINARY_SNIFF_LEN).unwrap_or(u64::MAX);
    let sniffed =
        u64::try_from((&mut file).take(sniff).read_to_end(&mut data).ok()?).unwrap_or(u64::MAX);
    // Charged before the verdict: a binary file's sniff is read too.
    *read = read.saturating_add(sniffed);
    if looks_binary(&data) {
        return None;
    }
    let rest = file
        .take(CODE_GREP_MAX_FILE_BYTES.saturating_sub(sniffed))
        .read_to_end(&mut data)
        .ok()?;
    *read = read.saturating_add(u64::try_from(rest).unwrap_or(u64::MAX));
    Some(data)
}

/// A pattern without `/` matches the basename, otherwise the path relative to the walk's root.
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
    let abs = match cs.read_path(str_arg(args, "path")) {
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

/// The numbered line window, with the DIVERGENCES F-06 fix: an oversized single row is cut to `CODE_MAX_OUTPUT - 4`
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
pub(crate) fn post_diff(cx: &RunCtx, display: &str, old: &str, new: &str) {
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

/// `path` keys the read ledger, `abs` is opened: the same lexical path inside the roots, the approved real
/// location outside them ([`CodeSet::write_target`], which re-checks the location now).
fn edit_file_call(cs: &CodeSet, cx: &RunCtx, args: &JsonObject) -> ToolOutput {
    let (path, abs) = match cs.write_target(cx, str_arg(args, "path")) {
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
    if let Err(t) = cs.require_fresh_read(&path) {
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
    cs.note_read(&path);
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

/// `path` keys the read ledger, `abs` is opened — as in [`edit_file_call`].
fn write_file_call(cs: &CodeSet, cx: &RunCtx, args: &JsonObject) -> ToolOutput {
    let (path, abs) = match cs.write_target(cx, str_arg(args, "path")) {
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
        if let Err(t) = cs.require_fresh_read(&path) {
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
    cs.note_read(&path);
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

/// The optional `path` argument as a search base (anywhere: reads are open), at its real location
/// ([`CodeSet::walk_base`]); empty → the root.
fn code_base_dir(cs: &CodeSet, args: &JsonObject) -> Result<PathBuf, ToolOutput> {
    let raw = str_arg(args, "path");
    if raw.trim().is_empty() {
        return Ok(cs.root().to_path_buf());
    }
    let abs = cs.read_path(raw)?;
    // A stat failure and a non-directory give the same text, and it names the RAW argument.
    match fs::metadata(&abs) {
        Ok(m) if m.is_dir() => Ok(cs.walk_base(abs)),
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

    // DIVERGENCES F-06: the oversized row is cut so it fits, instead of leaving an empty window.
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

    // Every byte `grep` reads is charged to the call's budget — a binary file's sniff too, though
    // nothing of it is searched.
    #[test]
    fn grep_text_charges_every_byte_it_reads() {
        use super::{CODE_BINARY_SNIFF_LEN, grep_text};
        let t = tempfile::tempdir().expect("tempdir");
        let mut binary = vec![0u8; 3 * CODE_BINARY_SNIFF_LEN];
        binary[10] = 1;
        std::fs::write(t.path().join("bin"), &binary).expect("bin");
        let text = "y".repeat(3 * CODE_BINARY_SNIFF_LEN);
        std::fs::write(t.path().join("txt"), &text).expect("txt");

        let mut read = 0u64;
        assert_eq!(grep_text(&t.path().join("bin"), &mut read), None);
        assert_eq!(read, CODE_BINARY_SNIFF_LEN as u64);
        let got = grep_text(&t.path().join("txt"), &mut read).expect("text");
        assert_eq!(got, text.as_bytes());
        assert_eq!(read, (4 * CODE_BINARY_SNIFF_LEN) as u64);
        assert_eq!(grep_text(&t.path().join("missing"), &mut read), None);
        assert_eq!(read, (4 * CODE_BINARY_SNIFF_LEN) as u64);
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

    // Under `refuse`, the writers go through `CodeSet::write_target` and refuse a symlink out of the root — nothing
    // outside is created or changed — while `read_file` reads through the same link (reads are open).
    #[cfg(unix)]
    #[test]
    fn writers_refuse_a_symlink_escape_that_read_file_follows() {
        use super::super::CodeSet;
        use super::super::tests::jail_fixture;
        use super::{edit_file_call, read_file_call, write_file_call};
        use crate::tool::context::RunCtx;
        use std::os::unix::fs::symlink;

        let (_t, root, out) = jail_fixture();
        symlink(out.join("secret.txt"), root.join("escape")).expect("link");
        symlink(&out, root.join("outdir")).expect("link");
        let cs = CodeSet::new(
            root.clone(),
            Vec::new(),
            true,
            super::super::Outside::Refuse,
        )
        .expect("set");
        let cx = RunCtx::default();
        let refused = |arg: &str| {
            ToolOutput::err(format!(
                "path is outside the project root ({}): {arg}",
                root.display()
            ))
        };
        // The overlong links: Linux resolves them and finds them outside, macOS cannot resolve them.
        let unresolved = |arg: &str| {
            if cfg!(target_os = "macos") {
                ToolOutput::err(format!(
                    "cannot verify {arg} inside the project root ({})",
                    root.display()
                ))
            } else {
                refused(arg)
            }
        };

        assert_eq!(
            read_file_call(&cs, &args(json!({"path": "escape"}))),
            ToolOutput::ok("     1\ts")
        );
        assert_eq!(
            write_file_call(&cs, &cx, &args(json!({"path": "escape", "content": "x"}))),
            refused("escape")
        );
        assert_eq!(
            write_file_call(
                &cs,
                &cx,
                &args(json!({"path": "outdir/new.txt", "content": "x"}))
            ),
            refused("outdir/new.txt")
        );
        assert_eq!(
            edit_file_call(
                &cs,
                &cx,
                &args(json!({"path": "escape", "old_string": "s", "new_string": "x"}))
            ),
            refused("escape")
        );
        assert_eq!(
            std::fs::read_to_string(out.join("secret.txt")).expect("secret"),
            "s"
        );
        assert!(!out.join("new.txt").exists());

        // B1: the planted overlong links are refused for a write; a read goes where the kernel goes.
        super::super::tests::plant_overlong(&root, &out);
        for path in ["L/S/secret.txt", "L2"] {
            assert_eq!(
                read_file_call(&cs, &args(json!({"path": path}))),
                ToolOutput::ok("     1\ts")
            );
        }
        assert_eq!(
            write_file_call(
                &cs,
                &cx,
                &args(json!({"path": "L/S/planted.txt", "content": "x"}))
            ),
            unresolved("L/S/planted.txt")
        );
        assert!(!out.join("planted.txt").exists());

        // A write through a link that stays inside still lands.
        symlink("dir", root.join("subdir")).expect("link");
        let wrote = write_file_call(
            &cs,
            &cx,
            &args(json!({"path": "subdir/new.txt", "content": "x"})),
        );
        assert!(!wrote.is_error, "{wrote:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("dir/new.txt")).expect("new"),
            "x"
        );
    }

    // A walk meets symlinks as entries and never descends into one; with reads open, `grep` reads
    // through a file link wherever it leads and `glob` lists every link, in the root or out of it.
    #[cfg(unix)]
    #[test]
    fn walks_report_symlinks_that_leave_the_root() {
        use super::super::CodeSet;
        use super::super::tests::{jail_fixture, plant_overlong};
        use super::{glob_call, grep_call};
        use std::os::unix::fs::symlink;
        use tokio_util::sync::CancellationToken;

        let (_t, root, out) = jail_fixture();
        std::fs::write(out.join("secret.txt"), "TOPSECRET").expect("secret");
        std::fs::write(root.join("dir/kept.txt"), "TOPSECRET inside").expect("kept");
        symlink(out.join("secret.txt"), root.join("escape")).expect("link");
        symlink(&out, root.join("outdir")).expect("link");
        symlink("dir/kept.txt", root.join("alias")).expect("link");
        plant_overlong(&root, &out);
        let cs = CodeSet::new(
            root.clone(),
            Vec::new(),
            false,
            super::super::Outside::Refuse,
        )
        .expect("set");

        let grep = grep_call(
            &cs,
            &args(json!({"pattern": "TOPSECRET"})),
            &CancellationToken::new(),
        );
        assert_eq!(
            grep,
            ToolOutput::ok(
                "L2:1: TOPSECRET\nalias:1: TOPSECRET inside\ndir/kept.txt:1: TOPSECRET inside\nescape:1: TOPSECRET"
            )
        );
        let glob = glob_call(
            &cs,
            &args(json!({"pattern": "*"})),
            &CancellationToken::new(),
        );
        assert!(!glob.is_error, "{glob:?}");
        let names: Vec<&str> = glob.text.lines().collect();
        for link in ["alias", "escape", "outdir", "L", "L2"] {
            assert!(names.contains(&link), "{link} not in {names:?}");
        }
    }

    /// A set under `outside_writes: ask` with a user (the interactive case), over the jail fixture with
    /// `root/outdir -> out` and `out2/` beside it.
    #[cfg(unix)]
    fn asking_set(
        auto_write: bool,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::sync::Arc<super::super::CodeSet>,
    ) {
        use super::super::tests::jail_fixture;
        let (t, root, out) = jail_fixture();
        std::os::unix::fs::symlink(&out, root.join("outdir")).expect("link");
        let cs = super::super::CodeSet::new(
            root.clone(),
            Vec::new(),
            auto_write,
            super::super::Outside::Ask,
        )
        .expect("set");
        (t, root, out, cs)
    }

    /// What the walk carries for a call the gate allowed: "allow once" binds the subject's target, a session
    /// grant its scope — both from ONE resolution of the call.
    #[cfg(unix)]
    fn allowed(
        cs: &super::super::CodeSet,
        a: &JsonObject,
        session: bool,
    ) -> crate::tool::context::RunCtx {
        use crate::tool::context::{Approved, RunCtx};
        let s = super::writer_subject(cs, a).expect("an outside write");
        RunCtx {
            approved: Some(if session {
                Approved::Dir(s.scope)
            } else {
                Approved::Once(s.target)
            }),
            ..RunCtx::default()
        }
    }

    #[cfg(unix)]
    fn relink(link: &std::path::Path, to: &std::path::Path) {
        std::fs::remove_file(link).expect("unlink");
        std::os::unix::fs::symlink(to, link).expect("relink");
    }

    /// THE INVARIANT: an outside write needs THIS call's approval, carried in `RunCtx::approved`, and that
    /// approval must cover the real location as it is at call time. A call no gate approved — what a parallel
    /// batch, a headless loop or any future caller that skips the gate hands the tool — never writes, even
    /// with `auto_write`. (Under the rejected design, "`*_call` admits OUTSIDE because every caller gates",
    /// the first assertion fails: the file would be created.)
    #[cfg(unix)]
    #[test]
    fn an_outside_write_needs_this_calls_approval() {
        use super::{edit_file_call, read_file_call, write_file_call, writer_subject};
        use crate::tool::ApprovalSubject;
        use crate::tool::context::{Approved, RunCtx};

        let (t, root, out, cs) = asking_set(true);
        let real_out = std::fs::canonicalize(&out).expect("real out");
        let w = args(json!({"path": "outdir/new.txt", "content": "x"}));
        assert_eq!(
            writer_subject(&cs, &w),
            Some(ApprovalSubject {
                detail: format!(
                    "{} (outside the project root)",
                    real_out.join("new.txt").display()
                ),
                target: real_out.join("new.txt"),
                scope: real_out.clone(),
            }),
            "the prompt names the RESOLVED path; the grant is its directory"
        );

        // No approval: refused, nothing created.
        let got = write_file_call(&cs, &RunCtx::default(), &w);
        assert!(got.is_error, "{got:?}");
        assert!(
            got.text.starts_with(&format!("path is outside the project root ({}): outdir/new.txt — writing there needs the user's approval", root.display())),
            "{}",
            got.text
        );
        assert!(
            !out.join("new.txt").exists(),
            "an unapproved call wrote outside"
        );

        // An approval for another target, or a grant for another directory, does not carry over.
        for other in [
            Approved::Once(real_out.join("else.txt")),
            Approved::Dir(real_out.join("deep")),
        ] {
            let cx = RunCtx {
                approved: Some(other),
                ..RunCtx::default()
            };
            assert!(write_file_call(&cs, &cx, &w).is_error);
            assert!(!out.join("new.txt").exists());
        }

        // Approved: the write lands where the prompt said — the real path behind the symlinked parent.
        let got = write_file_call(&cs, &allowed(&cs, &w, false), &w);
        assert_eq!(
            got,
            ToolOutput::ok(format!(
                "wrote 1 B to {} (created)",
                real_out.join("new.txt").display()
            ))
        );
        assert_eq!(
            std::fs::read_to_string(out.join("new.txt")).expect("landed"),
            "x"
        );
        // A directory grant admits another file in that directory, and only directly in it.
        let w2 = args(json!({"path": "outdir/two.txt", "content": "2"}));
        assert!(!write_file_call(&cs, &allowed(&cs, &w, true), &w2).is_error);
        let deeper = args(json!({"path": "outdir/deep/three.txt", "content": "3"}));
        assert!(write_file_call(&cs, &allowed(&cs, &w, true), &deeper).is_error);
        assert!(!out.join("deep/three.txt").exists());

        // edit_file the same way: read through the link, then edit with the approval.
        assert!(!read_file_call(&cs, &args(json!({"path": "outdir/secret.txt"}))).is_error);
        let e = args(json!({"path": "outdir/secret.txt", "old_string": "s", "new_string": "t"}));
        assert!(edit_file_call(&cs, &RunCtx::default(), &e).is_error);
        assert_eq!(
            std::fs::read_to_string(out.join("secret.txt")).expect("kept"),
            "s"
        );
        assert!(!edit_file_call(&cs, &allowed(&cs, &e, false), &e).is_error);
        assert_eq!(
            std::fs::read_to_string(out.join("secret.txt")).expect("edited"),
            "t"
        );

        // The link is swapped between the prompt and the call: the approval named `out`, the link now
        // points at `out2` — the location is re-checked and the call refused, nothing lands in `out2`.
        let out2 = t.path().join("out2");
        std::fs::create_dir(&out2).expect("out2");
        let w = args(json!({"path": "outdir/swapped.txt", "content": "x"}));
        for session in [false, true] {
            let cx = allowed(&cs, &w, session);
            relink(&root.join("outdir"), &out2);
            assert!(write_file_call(&cs, &cx, &w).is_error);
            assert!(!out2.join("swapped.txt").exists() && !out.join("swapped.txt").exists());
            relink(&root.join("outdir"), &out);
        }
    }

    /// Review B1, the reviewer's repro exactly: the link is swapped between the prompt's detail and the grant
    /// scope, then back, and the call is attempted with what the gate would have carried. Before round 2 the
    /// detail and the scope were two resolutions, and the call was admitted by the detail's text: with the
    /// scope (`out2`) granted, `out/x.txt` was written though nobody approved `out`. Now the call carries the
    /// GRANTED scope, which must be the directory of where it lands — nothing is written anywhere outside.
    #[cfg(unix)]
    #[test]
    fn a_link_swapped_between_detail_and_scope_writes_nothing() {
        use super::{write_file_call, writer_subject, writer_summary};
        use crate::tool::context::{Approved, RunCtx};

        let (t, root, out, cs) = asking_set(true);
        let out2 = t.path().join("out2");
        std::fs::create_dir(&out2).expect("out2");
        let w = args(json!({"path": "outdir/x.txt", "content": "x"}));
        // 1. root/outdir -> out; take the detail.
        let detail = writer_summary(&cs, &w);
        // 2. Relink outdir -> out2; the grant scope is now out2 (granted for the session).
        relink(&root.join("outdir"), &out2);
        let scope = writer_subject(&cs, &w).expect("subject").scope;
        assert_eq!(scope, std::fs::canonicalize(&out2).expect("real out2"));
        // 3. Relink outdir -> out again.
        relink(&root.join("outdir"), &out);
        // 4. The call, with the grant the gate keyed on.
        let cx = RunCtx {
            approved: Some(Approved::Dir(scope)),
            ..RunCtx::default()
        };
        assert!(write_file_call(&cs, &cx, &w).is_error);
        assert!(
            !out.join("x.txt").exists() && !out2.join("x.txt").exists(),
            "a write landed in a directory nobody granted"
        );
        // And one resolution can never disagree with itself: detail, target and scope name one place.
        let s = writer_subject(&cs, &w).expect("subject");
        assert_eq!(s.detail, detail);
        assert_eq!(s.detail, super::super::outside_summary(&s.target));
        assert_eq!(Some(s.scope.as_path()), s.target.parent());
    }

    /// Review S1: a target with no parent (`/`) is granted as itself, never as the tool-wide key an inside
    /// grant uses.
    #[cfg(unix)]
    #[test]
    fn a_target_without_a_parent_is_its_own_grant() {
        let (_t, _root, _out, cs) = asking_set(true);
        let s = super::writer_subject(&cs, &args(json!({"path": "/"}))).expect("subject");
        assert_eq!(s.scope, std::path::PathBuf::from("/"));
        assert_eq!(s.target, std::path::PathBuf::from("/"));
    }

    /// The batches that skip the gate (headless `run_batch`, the interactive parallel walk) build their
    /// `RunCtx` without an approval; even if a writer were batched, an outside write would not land. And
    /// the writers are not parallel, so neither batch takes them at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_batch_never_writes_outside() {
        use crate::provider::model::ToolCall;
        use crate::tool::{Dispatcher, Registry};
        use std::sync::Arc;

        let (_t, _root, out, cs) = asking_set(true);
        let mut reg = Registry::default();
        reg.add(Arc::new(super::WriteFile(Arc::clone(&cs))));
        reg.add(Arc::new(super::EditFile(cs)));
        let calls: Vec<ToolCall> = ["a.txt", "b.txt"]
            .iter()
            .map(|f| ToolCall {
                name: "write_file".to_owned(),
                arguments: args(json!({"path": format!("outdir/{f}"), "content": "x"})),
                ..ToolCall::default()
            })
            .collect();
        assert!(!reg.supports_parallel("write_file", Some(&calls[0].arguments)));
        assert!(!reg.supports_parallel("edit_file", None));
        let outcomes = crate::headless::batch::run_batch(
            &crate::tool::context::RunCtx::default(),
            &reg,
            &calls,
        )
        .await;
        assert!(outcomes.iter().all(|o| o.is_error), "{outcomes:?}");
        assert!(!out.join("a.txt").exists() && !out.join("b.txt").exists());
    }

    /// Who asks, per call: inside the roots `!auto_write`; outside them always under `ask` with a user,
    /// never otherwise (refused at the call); an unverifiable path never (refused at the call). The session
    /// grant of an outside write is its real directory.
    #[cfg(unix)]
    #[test]
    fn the_writers_ask_matrix() {
        use super::super::tests::jail_fixture;
        use super::super::{CodeSet, Outside};
        use crate::tool::Tool;

        let (_t, root, out) = jail_fixture();
        std::os::unix::fs::symlink("lb", root.join("la")).expect("la");
        std::os::unix::fs::symlink("la", root.join("lb")).expect("lb");
        let real_out = std::fs::canonicalize(&out).expect("real out");
        let inside = args(json!({"path": "dir/x.txt"}));
        let outside = args(json!({"path": format!("{}/deep/x.txt", out.display())}));
        let looped = args(json!({"path": "la/x.txt"}));
        for auto_write in [false, true] {
            for policy in [Outside::Ask, Outside::Unattended, Outside::Refuse] {
                let cs = CodeSet::new(root.clone(), Vec::new(), auto_write, policy).expect("set");
                let at = format!("auto_write={auto_write} {policy:?}");
                for tool in [
                    std::sync::Arc::new(super::WriteFile(std::sync::Arc::clone(&cs)))
                        as std::sync::Arc<dyn Tool>,
                    std::sync::Arc::new(super::EditFile(std::sync::Arc::clone(&cs))),
                ] {
                    assert_eq!(
                        tool.requires_approval(Some(&inside)),
                        !auto_write,
                        "{at} inside"
                    );
                    assert_eq!(
                        tool.requires_approval(Some(&outside)),
                        policy == Outside::Ask,
                        "{at} outside"
                    );
                    assert!(!tool.requires_approval(Some(&looped)), "{at} unverifiable");
                    assert_eq!(tool.waives_approval(), auto_write, "{at} waives");
                    assert_eq!(tool.approval_subject(&inside), None, "{at}");
                    assert_eq!(
                        tool.approval_subject(&outside).map(|s| s.scope),
                        (policy == Outside::Ask).then(|| real_out.join("deep")),
                        "{at}"
                    );
                    // The header names the real location only when it is what a prompt is about.
                    let header = tool.header_summary(&outside).expect("summary");
                    assert_eq!(
                        header.ends_with(" (outside the project root)"),
                        policy == Outside::Ask,
                        "{at} {header}"
                    );
                }
            }
        }
    }

    /// The writers' descriptions are model-facing: under `outside_writes: ask` with a user they say that a
    /// path outside the roots asks the user first, so a model does not take a refusal there as final; with
    /// nobody to ask, or under `refuse`, they say nothing of it (the pinned texts).
    #[test]
    fn the_writers_say_an_outside_write_asks_only_when_it_does() {
        use super::super::{CodeSet, Outside};
        use crate::tool::Tool;
        let clause = " Outside that, the user is asked to approve a write there first.";
        for policy in [Outside::Ask, Outside::Unattended, Outside::Refuse] {
            let cs = CodeSet::new(std::path::PathBuf::from("/proj"), Vec::new(), false, policy)
                .expect("set");
            for d in [
                super::WriteFile(std::sync::Arc::clone(&cs)).def(),
                super::EditFile(std::sync::Arc::clone(&cs)).def(),
            ] {
                assert_eq!(
                    d.description.ends_with(clause),
                    policy == Outside::Ask,
                    "{policy:?} {}",
                    d.description
                );
                assert!(
                    d.description.contains("inside the project root"),
                    "{}",
                    d.description
                );
            }
        }
    }
}
