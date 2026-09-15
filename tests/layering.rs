//! The layering gate (docs/ARCHITECTURE.md §1.2): the module graph of `src/` only ever points DOWN
//! the declared layer order, the three crate-naming seams the former crate boundaries carried
//! (ratatui/crossterm, `image`, the process environment) hold, and the process's stderr has one
//! writer. One test binary in place of greps in `ci.sh`: a violation names the file and line, and
//! the exceptions the tree still carries are a table that only ever shrinks.
//!
//! Edges are the `crate::<module>` paths a file names, `#[cfg(test)]` modules blanked out (a test
//! may reach up; the product graph may not), doc and line comments cut, `src/testing/` (the test
//! fakes, which reach everywhere by design) not scanned.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;

/// The declared order, bottom row first (the phase-5 plan §3, `config` above `tool`/`session` as
/// §2 acknowledged, `shell`/`agents` below `tool` as the tree has always had them — `tool/shell.rs`
/// is the policy over the `shell/` mechanism and `tool/agent.rs` consumes the `agents/` overlay).
/// A module may name every module in a LOWER row and none in its own row or a higher one; the
/// modules of one row are independent siblings.
const LAYERS: &[&[&str]] = &[
    &["app"],
    &["text", "vars", "paths", "sync", "imgterm"],
    &["color", "diag"],
    &["llm"],
    &["provider"],
    &["shell", "agents"],
    &["tool"],
    &["mcp", "session", "mathtext"],
    &["config", "markdown", "headless"],
    &["ui"],
    &["host"],
    &["repl"],
    &["cmd"],
];

/// The upward edges the tree still carries, one row per (file, module it must not name), each with
/// the phase-5 PR that retires it. The gate fails on a NEW upward edge and on a row here whose edge
/// is gone — delete the row together with the edge.
const KNOWN_UPWARD: &[(&str, &str, &str)] = &[
    (
        "diag.rs",
        "cmd",
        "reads `cmd::VERSION` — PR-3 moves VERSION into app/",
    ),
    (
        "repl/params.rs",
        "cmd",
        "calls `cmd::window::parse_window_size` — PR-7 moves window.rs into config/",
    ),
    (
        "repl/catalog.rs",
        "cmd",
        "calls `cmd::resolve::resolve_key_from_env_or_config` — no PR yet (the key resolver wants a home below repl)",
    ),
];

/// The module the fakes live in: not a layer, and product code never names it.
const FAKES: &str = "testing";

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `dir`, sorted.
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// The path relative to `src/`, slash-separated.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("under src/")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The top-level module a file belongs to: its first directory under `src/`, or its own stem.
fn module_of(rel: &str) -> String {
    let head = rel.split('/').next().expect("non-empty path");
    head.strip_suffix(".rs").unwrap_or(head).to_owned()
}

/// The top-level modules `lib.rs` declares.
fn declared_modules(lib: &str) -> BTreeSet<String> {
    Regex::new(r"(?m)^(?:pub(?:\(crate\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*);")
        .unwrap()
        .captures_iter(lib)
        .map(|c| c[1].to_owned())
        .collect()
}

/// The index just past the `}` that closes the block opening at `src[start]` (a `{`). Braces
/// inside line and block comments, string, raw-string, byte-string and char literals do not count.
fn skip_block(src: &[u8], start: usize) -> usize {
    assert_eq!(src[start], b'{');
    let is_ident = |i: usize| i > 0 && (src[i - 1].is_ascii_alphanumeric() || src[i - 1] == b'_');
    let mut depth = 0usize;
    let mut i = start;
    while i < src.len() {
        let rest = &src[i..];
        if rest.starts_with(b"//") {
            i += rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        } else if rest.starts_with(b"/*") {
            i += rest
                .windows(2)
                .position(|w| w == b"*/")
                .expect("block comment closes")
                + 2;
        } else if rest[0] == b'"' || (rest.starts_with(b"b\"") && !is_ident(i)) {
            i = skip_string(src, if rest[0] == b'"' { i } else { i + 1 });
        } else if rest[0] == b'r' && !is_ident(i) {
            match skip_raw_string(src, i) {
                Some(end) => i = end,
                None => i += 1,
            }
        } else if rest[0] == b'\'' {
            i = skip_char_or_lifetime(src, i);
        } else {
            match rest[0] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    panic!("unbalanced braces from byte {start}");
}

/// Past the closing quote of the `"…"` literal opening at `src[start]`.
fn skip_string(src: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

/// Past the `r#*"…"#*` literal opening at `src[start]` (an `r`), or `None` when it is not one.
fn skip_raw_string(src: &[u8], start: usize) -> Option<usize> {
    let mut j = start + 1;
    let mut hashes = 0;
    while j < src.len() && src[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j >= src.len() || src[j] != b'"' {
        return None;
    }
    let close: Vec<u8> = std::iter::once(b'"')
        .chain(std::iter::repeat_n(b'#', hashes))
        .collect();
    let body = &src[j + 1..];
    let at = body
        .windows(close.len())
        .position(|w| w == close.as_slice())
        .expect("raw string closes");
    Some(j + 1 + at + close.len())
}

/// Past the char literal opening at `src[start]` (a `'`), or past the quote when it starts a
/// lifetime or a loop label.
fn skip_char_or_lifetime(src: &[u8], start: usize) -> usize {
    let at = |k: usize| src.get(k).copied();
    if at(start + 1) == Some(b'\\') {
        if at(start + 2) == Some(b'u') {
            // '\u{…}'
            let close = src[start..]
                .iter()
                .position(|&b| b == b'}')
                .expect("unicode escape closes");
            return start + close + 2;
        }
        return start + 4; // '\n', '\'', '\\'
    }
    if at(start + 2) == Some(b'\'') {
        return start + 3; // 'x'
    }
    // A lifetime or label: the quote alone.
    start + 1
}

/// The product half of a file: every `#[cfg(test)] mod x { … }` blanked (newlines kept, so line
/// numbers hold), every `#[cfg(test)] mod x;` child recorded in `skip` so it is not scanned.
fn strip_test_mods(
    path: &Path,
    src: &str,
    known: &BTreeSet<PathBuf>,
    skip: &mut BTreeSet<PathBuf>,
) -> String {
    let header = Regex::new(
        r"(?m)^[ \t]*#\[cfg\(test\)\][ \t]*\n(?:[ \t]*#\[[^\n]*\][ \t]*\n)*[ \t]*(?:pub(?:\([a-z]+\))?[ \t]+)?mod[ \t]+([A-Za-z_][A-Za-z0-9_]*)[ \t]*([;{])",
    )
    .unwrap();
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    for m in header.captures_iter(src) {
        let whole = m.get(0).unwrap();
        if whole.start() < pos {
            continue; // a header inside a block already blanked
        }
        if &m[2] == ";" {
            let dir = path.parent().unwrap();
            let stem = path.file_stem().unwrap().to_string_lossy();
            let base = if matches!(stem.as_ref(), "mod" | "lib" | "main") {
                dir.to_path_buf()
            } else {
                dir.join(stem.as_ref())
            };
            for cand in [
                base.join(format!("{}.rs", &m[1])),
                base.join(&m[1]).join("mod.rs"),
            ] {
                if known.contains(&cand) {
                    skip.insert(cand);
                }
            }
            continue;
        }
        let end = skip_block(src.as_bytes(), whole.end() - 1);
        out.push_str(&src[pos..whole.start()]);
        out.extend(std::iter::repeat_n(
            '\n',
            src[whole.start()..end].matches('\n').count(),
        ));
        pos = end;
    }
    out.push_str(&src[pos..]);
    out
}

/// One `crate::<module>` reference in product code.
#[derive(Debug)]
struct Edge {
    file: String,
    line: usize,
    to: String,
}

/// Every cross-module edge of the product graph, keyed by the source module.
fn product_edges(root: &Path, modules: &BTreeSet<String>) -> BTreeMap<String, Vec<Edge>> {
    let files = rs_files(root);
    let known: BTreeSet<PathBuf> = files.iter().cloned().collect();
    let mut skip = BTreeSet::new();
    let mut stripped = Vec::new();
    for path in &files {
        let rel = rel(root, path);
        let module = module_of(&rel);
        if !modules.contains(&module) || module == FAKES {
            continue; // main.rs, lib.rs, the fakes
        }
        let src = fs::read_to_string(path).expect("read source");
        stripped.push((
            path.clone(),
            rel,
            module,
            strip_test_mods(path, &src, &known, &mut skip),
        ));
    }
    let path_re = Regex::new(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut edges: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    for (path, rel, module, text) in stripped {
        if skip.contains(&path) {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for c in path_re.captures_iter(code) {
                let to = c[1].to_owned();
                // `crate::BoxFuture`, `crate::BoxError`: items of the root, not modules.
                if to != module && modules.contains(&to) {
                    edges.entry(module.clone()).or_default().push(Edge {
                        file: rel.clone(),
                        line: i + 1,
                        to,
                    });
                }
            }
        }
    }
    edges
}

#[test]
fn the_module_graph_points_down() {
    let root = src_root();
    let modules = declared_modules(&fs::read_to_string(root.join("lib.rs")).unwrap());
    let rank: BTreeMap<&str, usize> = LAYERS
        .iter()
        .enumerate()
        .flat_map(|(i, row)| row.iter().map(move |m| (*m, i)))
        .collect();
    // The table and lib.rs name the same modules (a rename or a new module must be placed).
    let ranked: BTreeSet<String> = rank.keys().map(|m| (*m).to_owned()).collect();
    let mut declared = modules.clone();
    declared.remove(FAKES);
    assert_eq!(
        ranked, declared,
        "LAYERS (left) and the modules lib.rs declares (right) disagree"
    );

    let edges = product_edges(&root, &modules);
    let mut unexpected = Vec::new();
    let mut used = BTreeSet::new();
    for (from, list) in &edges {
        for e in list {
            let up = match rank.get(e.to.as_str()) {
                Some(to) => *to >= rank[from.as_str()],
                None => true, // the fakes
            };
            if !up {
                continue;
            }
            match KNOWN_UPWARD
                .iter()
                .position(|(file, to, _)| *file == e.file && *to == e.to)
            {
                Some(i) => {
                    used.insert(i);
                }
                None => unexpected.push(format!(
                    "  {}:{} names crate::{} ({} is not below {})",
                    e.file, e.line, e.to, e.to, from
                )),
            }
        }
    }
    let stale: Vec<String> = KNOWN_UPWARD
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, (file, to, why))| format!("  {file} → {to} ({why})"))
        .collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "layering (tests/layering.rs):\n{}{}",
        if unexpected.is_empty() {
            String::new()
        } else {
            format!(
                "new upward edges — move the code down or the caller up:\n{}\n",
                unexpected.join("\n")
            )
        },
        if stale.is_empty() {
            String::new()
        } else {
            format!(
                "KNOWN_UPWARD rows whose edge is gone — delete them:\n{}\n",
                stale.join("\n")
            )
        }
    );
}

/// The three seams `ci.sh` used to grep for, over the raw text of every file under `src/` (tests
/// and the fakes included, comments included — a name in a comment is a name).
#[test]
fn crate_names_stay_behind_their_seams() {
    let root = src_root();
    let terminal = Regex::new(r"ratatui|crossterm").unwrap();
    let image = Regex::new(r"\bimage::").unwrap();
    let mut bad = Vec::new();
    for path in rs_files(&root) {
        let rel = rel(&root, &path);
        let text = fs::read_to_string(&path).unwrap();
        // 1. only src/ui/** may name ratatui/crossterm — the loop, the renderer and the command never see a terminal crate.
        if !rel.starts_with("ui/") && terminal.is_match(&text) {
            bad.push(format!(
                "  {rel}: names ratatui/crossterm (only src/ui/ may)"
            ));
        }
        // 2. the session store never reads the process environment — it takes its root from HostDirs (injected).
        if rel.starts_with("session/") && text.contains("std::env::var") {
            bad.push(format!(
                "  {rel}: reads the process environment (src/session must not)"
            ));
        }
        // 3. only src/imgterm.rs may name the `image` crate — every other module sees `imgterm::Frame`.
        if rel != "imgterm.rs" && image.is_match(&text) {
            bad.push(format!(
                "  {rel}: names the image crate (only src/imgterm.rs may)"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "crate-naming seams (tests/layering.rs):\n{}",
        bad.join("\n")
    );
}

/// `Streams` (src/cmd/io.rs) is the process's one stderr writer: every warning reaches the user
/// through `Streams::warning`/`caution` (or a UI line), never through a stray `eprintln!`.
#[test]
fn stderr_has_one_writer() {
    let root = src_root();
    let writer = Regex::new(r"\beprintln!|\beprint!|\bstderr\(\)").unwrap();
    let files = rs_files(&root);
    let known: BTreeSet<PathBuf> = files.iter().cloned().collect();
    let mut skip = BTreeSet::new();
    let mut texts = Vec::new();
    for path in &files {
        let rel = rel(&root, path);
        if module_of(&rel) == FAKES {
            continue;
        }
        let src = fs::read_to_string(path).unwrap();
        texts.push((
            path.clone(),
            rel,
            strip_test_mods(path, &src, &known, &mut skip),
        ));
    }
    let mut bad = Vec::new();
    for (path, rel, text) in texts {
        if skip.contains(&path) || rel == "cmd/io.rs" {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if writer.is_match(code) {
                bad.push(format!("  {rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "stderr writers outside src/cmd/io.rs (tests/layering.rs) — go through Streams:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_block_skipper_reads_rust() {
    let src = br##"mod tests { let s = "}"; let r = r#"}"#; let c = '}'; let e = '\''; let u = '\u{7d}'; // }
        /* } */ let b = b"}"; let lt: &'a str = x; 'outer: loop { break 'outer; } } fn after() {}"##;
    let end = skip_block(src, 10);
    assert_eq!(&src[end..], b" fn after() {}");
}
