//! `code` toolset integration tests (`tool/code_test.go`, `tool/parallel_test.go:15-55`).

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use iota::provider::model::JsonObject;
use iota::tool::code::{CODE_MAX_OUTPUT, new_code_set};
use iota::tool::context::RunCtx;
use iota::tool::sets::{RawNode, ToolsConfig};
use iota::tool::{Dispatcher, Presentation, Tool, ToolEnv, ToolOutput};
use iota::tool::{Registry, merge};
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

/// The set's tools by advertised name (Go's `index` in `newCodeProject`).
type Tools = HashMap<String, Arc<dyn Tool>>;

/// A code set over `<temp>/proj` (Go's `newCodeProject`). The project sits one level below the temp root so a
/// sibling `outside.txt` is reachable as `../outside.txt` without writing outside the fixture.
fn code_project(files: &[(&str, &str)], cfg: &str) -> (TempDir, PathBuf, Tools) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("proj");
    fs::create_dir_all(&root).expect("project root");
    for (rel, contents) in files {
        write_project_file(&root, rel, contents);
    }
    let tools = tools_at(&root, cfg);
    (dir, root, tools)
}

/// `newCodeSet(Env{ProjectRoot: root}, node)` indexed by tool name.
fn tools_at(root: &Path, cfg: &str) -> Tools {
    new_code_set(&env_at(root), node(cfg).as_ref())
        .expect("code set")
        .into_iter()
        .map(|t| (t.def().name, t))
        .collect()
}

fn env_at(root: &Path) -> ToolEnv {
    ToolEnv {
        project_root: Some(root.to_path_buf()),
        ..ToolEnv::default()
    }
}

/// `yaml.Unmarshal` of the set's own mapping; `""` is Go's zero node (defaults).
fn node(cfg: &str) -> Option<RawNode> {
    if cfg.is_empty() {
        None
    } else {
        Some(serde_norway::from_str(cfg).expect("set config"))
    }
}

/// Go's `call`: a model-facing result, never a hard error.
async fn call(tools: &Tools, name: &str, args: serde_json::Value) -> ToolOutput {
    let args: JsonObject = args.as_object().cloned().unwrap_or_default();
    tools[name]
        .call(&RunCtx::default(), &args)
        .await
        .expect("no hard error")
}

/// Go's `writeProjectFile`.
fn write_project_file(root: &Path, rel: &str, content: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("parents");
    fs::write(&path, content).expect("write project file");
    path
}

/// `write_project_file` for content a `&str` cannot hold: the fixtures below are files in Latin-1,
/// Shift-JIS and GBK, which is the whole point of them.
fn write_project_bytes(root: &Path, rel: &str, content: &[u8]) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("parents");
    fs::write(&path, content).expect("write project file");
    path
}

/// An `edit_file` over a byte fixture, through the read gate: write the bytes, `read_file` to stamp the
/// ledger, then edit. Returns the edit's output and the file as it stands on disk afterwards.
async fn edit_bytes(
    root: &Path,
    tools: &Tools,
    rel: &str,
    before: &[u8],
    args: serde_json::Value,
) -> (ToolOutput, Vec<u8>) {
    let path = write_project_bytes(root, rel, before);
    let read = call(tools, "read_file", json!({ "path": rel })).await;
    assert!(!read.is_error, "read {rel} = {read:?}");
    let out = call(tools, "edit_file", args).await;
    let after = fs::read(&path).expect("read back");
    (out, after)
}

/// `os.Chtimes(path, t, t)` for the mtime half.
fn set_mtime(path: &Path, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open for utimes")
        .set_modified(when)
        .expect("set mtime");
}
#[tokio::test]
async fn code_tools_refuse_paths_outside_the_project_root() {
    let (dir, root, tools) = code_project(&[("a.txt", "hello\n")], "");
    let outside = dir.path().join("outside.txt");
    fs::write(&outside, "secret").expect("write outside");

    for path in [
        outside.to_string_lossy().into_owned(),
        "../outside.txt".to_owned(),
        "a/../../outside.txt".to_owned(),
    ] {
        let out = call(&tools, "read_file", json!({ "path": path })).await;
        assert!(
            out.is_error && out.text.contains("outside the project root"),
            "path {path:?} should be jailed, got {out:?}"
        );
    }

    // Relative and absolute-inside paths both resolve.
    let out = call(&tools, "read_file", json!({ "path": "a.txt" })).await;
    assert!(
        !out.is_error && out.text.contains("hello"),
        "relative read failed: {out:?}"
    );
    let inside = root.join("a.txt").to_string_lossy().into_owned();
    let out = call(&tools, "read_file", json!({ "path": inside })).await;
    assert!(
        !out.is_error && out.text.contains("hello"),
        "absolute-inside read failed: {out:?}"
    );
}
#[tokio::test]
async fn glob_lists_matching_files_relative_to_the_root() {
    let (_dir, root, tools) = code_project(
        &[
            ("pkg/old.go", "package pkg\n"),
            ("pkg/new.go", "package pkg\n"),
            ("docs/readme.md", "hi\n"),
            ("vendor/dep.go", "package dep\n"),
            (".gitignore", "vendor/\n"),
        ],
        "",
    );
    let past = SystemTime::now() - Duration::from_secs(3600);
    set_mtime(&root.join("pkg/old.go"), past);

    // Bare pattern matches at any depth; gitignored vendor/ is excluded; newest file first.
    let out = call(&tools, "glob", json!({ "pattern": "*.go" })).await;
    assert!(!out.is_error, "glob error: {}", out.text);
    assert!(
        !out.text.contains("vendor/dep.go"),
        "gitignored file leaked into glob:\n{}",
        out.text
    );
    assert_eq!(
        out.text.split('\n').collect::<Vec<_>>(),
        ["pkg/new.go", "pkg/old.go"]
    );

    // Path-anchored pattern.
    let out = call(&tools, "glob", json!({ "pattern": "docs/*.md" })).await;
    assert_eq!(out.text, "docs/readme.md");

    let out = call(&tools, "glob", json!({ "pattern": "*.rs" })).await;
    assert!(
        !out.is_error && out.text.contains("no files match"),
        "no-match glob = {out:?}"
    );
}
#[tokio::test]
async fn grep_searches_with_context_and_respects_the_ignore_files() {
    let (_dir, root, tools) = code_project(
        &[
            (
                "main.go",
                "package main\n\nfunc main() {\n\tprintln(\"hi\")\n}\n",
            ),
            ("notes.md", "the main idea\n"),
        ],
        "",
    );
    fs::write(root.join("blob.bin"), b"ma\x00in").expect("write binary");

    let out = call(&tools, "grep", json!({ "pattern": "func main" })).await;
    assert!(
        !out.is_error && out.text.contains("main.go:3: func main() {"),
        "grep basic = {out:?}"
    );

    // include filter narrows by basename glob; context adds - lines.
    let out = call(
        &tools,
        "grep",
        json!({ "pattern": "main", "include": "*.md" }),
    )
    .await;
    assert!(
        !out.text.contains("main.go"),
        "include filter failed:\n{}",
        out.text
    );
    assert!(
        out.text.contains("notes.md:1: the main idea"),
        "include filter failed:\n{}",
        out.text
    );

    let out = call(
        &tools,
        "grep",
        json!({ "pattern": "println", "context": 1 }),
    )
    .await;
    assert!(
        out.text.contains("main.go:3- func main() {"),
        "context lines missing:\n{}",
        out.text
    );
    assert!(
        out.text.contains("main.go:4: \tprintln"),
        "context lines missing:\n{}",
        out.text
    );

    // Binary files are skipped silently; a bad regex is a model-facing error.
    let out = call(&tools, "grep", json!({ "pattern": "ma.?in" })).await;
    assert!(
        !out.text.contains("blob.bin"),
        "binary file leaked into grep:\n{}",
        out.text
    );
    let out = call(&tools, "grep", json!({ "pattern": "(" })).await;
    assert!(
        out.is_error && out.text.contains("invalid regular expression"),
        "bad regex = {out:?}"
    );
}
#[tokio::test]
async fn list_dir_lists_entries_with_a_directory_marker() {
    let (_dir, _root, tools) = code_project(&[("pkg/a.go", "x"), ("top.txt", "12345")], "");

    let out = call(&tools, "list_dir", json!({})).await;
    assert!(!out.is_error, "list_dir root = {out:?}");
    assert!(out.text.contains("pkg/"), "list_dir root:\n{}", out.text);
    assert!(
        out.text.contains("top.txt (5 B)"),
        "list_dir root:\n{}",
        out.text
    );

    let out = call(&tools, "list_dir", json!({ "path": "nope" })).await;
    assert!(
        out.is_error && out.text.contains("not a directory"),
        "missing dir = {out:?}"
    );
}
#[tokio::test]
async fn read_file_returns_a_line_window() {
    let (_dir, root, tools) = code_project(&[("f.txt", "l1\nl2\nl3\nl4\nl5\n")], "");

    let out = call(&tools, "read_file", json!({ "path": "f.txt" })).await;
    assert!(!out.is_error, "numbered read = {out:?}");
    assert!(out.text.contains("     1\tl1"), "{}", out.text);
    assert!(out.text.contains("     5\tl5"), "{}", out.text);

    let out = call(
        &tools,
        "read_file",
        json!({ "path": "f.txt", "offset": 2, "limit": 2 }),
    )
    .await;
    assert_eq!(out.text, "     2\tl2\n     3\tl3\n[showing lines 2-3 of 5]");

    let out = call(&tools, "read_file", json!({ "path": "f.txt", "offset": 9 })).await;
    assert!(
        out.is_error && out.text.contains("past the end"),
        "offset past end = {out:?}"
    );

    write_project_file(&root, "empty.txt", "");
    let out = call(&tools, "read_file", json!({ "path": "empty.txt" })).await;
    assert!(!out.is_error, "empty read = {out:?}");
    assert_eq!(out.text, "[file is empty]");

    fs::write(root.join("bin.dat"), b"a\x00b").expect("write binary");
    let out = call(&tools, "read_file", json!({ "path": "bin.dat" })).await;
    assert!(
        out.is_error && out.text.contains("binary"),
        "binary read = {out:?}"
    );
}
#[tokio::test]
async fn edit_file_replaces_a_unique_occurrence_and_refuses_ambiguity() {
    let (_dir, root, tools) = code_project(&[("f.go", "aaa\nbbb\naaa\n")], "");

    // Read-before-edit: an unread file is rejected.
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "bbb", "new_string": "BBB" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("read it with read_file"),
        "unread edit = {out:?}"
    );
    call(&tools, "read_file", json!({ "path": "f.go" })).await;

    // Ambiguous old_string needs replace_all or more context.
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "aaa", "new_string": "AAA" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("appears 2 times"),
        "ambiguous edit = {out:?}"
    );

    // A unique replacement succeeds, reports a numbered snippet, stays fresh.
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "bbb", "new_string": "BBB" }),
    )
    .await;
    assert!(!out.is_error, "edit = {out:?}");
    assert_eq!(
        out.text,
        "1 replacement(s) in f.go\n\n     1\taaa\n     2\tBBB\n     3\taaa"
    );
    assert_eq!(
        fs::read_to_string(root.join("f.go")).expect("read back"),
        "aaa\nBBB\naaa\n"
    );

    // replace_all handles both occurrences without a re-read (the edit itself refreshed the ledger). A
    // string-typed "true" (model serialization quirk) coerces instead of silently editing one occurrence.
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "aaa", "new_string": "xxx", "replace_all": "true" }),
    )
    .await;
    assert!(!out.is_error, "replace_all = {out:?}");
    assert!(
        out.text.contains("2 replacement(s)"),
        "replace_all = {out:?}"
    );

    // Not-found and no-op edits are model-facing errors.
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "zzz", "new_string": "y" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("not found"),
        "not-found edit = {out:?}"
    );
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "xxx", "new_string": "xxx" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("identical"),
        "no-op edit = {out:?}"
    );

    // External modification after the read invalidates the ledger.
    set_mtime(
        &root.join("f.go"),
        SystemTime::now() + Duration::from_secs(3600),
    );
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "f.go", "old_string": "xxx", "new_string": "y" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("changed on disk"),
        "stale edit = {out:?}"
    );
}

// New: `edit_file` is a BYTE operation. Until this landed it decoded the whole file with
// `String::from_utf8_lossy` and wrote the decoded result back, so one edit anywhere in a Latin-1 file
// turned every byte in it that is not valid UTF-8 into `EF BF BD` — outside the edited span as much as
// inside it — and the diff, built from the same lossy buffer, showed nothing. Go never had the bug
// (`tool/code.go:740-755`: `string(data)` holds arbitrary bytes, `strings.Count`/`Replace` count them,
// `[]byte(updated)` hands the same bytes back), so these five tests pin a Go behaviour the port lost.
#[tokio::test]
async fn edit_file_preserves_latin1_bytes() {
    let (_dir, root, tools) = code_project(&[], "");
    // 0xE9 = é, 0xEF = ï in Latin-1; neither is a valid UTF-8 sequence on its own.
    let before = b"// caf\xe9 counter\nlet x = 1;\n// na\xefve\n";
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "latin1.rs",
        before,
        json!({ "path": "latin1.rs", "old_string": "let x = 1;", "new_string": "let x = 2;" }),
    )
    .await;

    assert!(!out.is_error, "latin-1 edit = {out:?}");
    assert_eq!(
        after,
        b"// caf\xe9 counter\nlet x = 2;\n// na\xefve\n".to_vec(),
        "every byte outside the edited span must survive the write"
    );
}

// New: not a single-byte-encoding accident — a legacy multi-byte encoding's trail bytes are ASCII
// punctuation (Shift-JIS `ソ` = 83 5C, where 5C is `\`; `本` = 96 7B, where 7B is `{`), which is exactly
// the shape a decode-and-rewrite mangles and a byte splice cannot.
#[tokio::test]
async fn edit_file_preserves_shift_jis_and_gbk_bytes() {
    let (_dir, root, tools) = code_project(&[], "");

    // Shift-JIS: 日本語 = 93 FA 96 7B 8C EA, ソ = 83 5C.
    let before = b"// \x93\xfa\x96\x7b\x8c\xea \x83\x5c\nconst N = 1;\n";
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "sjis.go",
        before,
        json!({ "path": "sjis.go", "old_string": "const N = 1;", "new_string": "const N = 42;" }),
    )
    .await;
    assert!(!out.is_error, "shift-jis edit = {out:?}");
    assert_eq!(
        after,
        b"// \x93\xfa\x96\x7b\x8c\xea \x83\x5c\nconst N = 42;\n".to_vec()
    );

    // GBK: 中文 = D6 D0 CE C4, 配置 = C5 E4 D6 C3.
    let before = b"# \xd6\xd0\xce\xc4\xc5\xe4\xd6\xc3\nport = 80\n";
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "gbk.ini",
        before,
        json!({ "path": "gbk.ini", "old_string": "port = 80", "new_string": "port = 8080" }),
    )
    .await;
    assert!(!out.is_error, "gbk edit = {out:?}");
    assert_eq!(
        after,
        b"# \xd6\xd0\xce\xc4\xc5\xe4\xd6\xc3\nport = 8080\n".to_vec()
    );
}

// New: fidelity cuts both ways — a file that really does contain U+FFFD keeps its three bytes, and an
// undecodable byte beside it stays undecodable. The old path collapsed the two into one another: it read
// both as U+FFFD and wrote `EF BF BD` for each, so the file came back with a replacement character it
// never had and no way to tell which was which.
#[tokio::test]
async fn edit_file_keeps_real_replacement_characters_apart() {
    let (_dir, root, tools) = code_project(&[], "");
    let before = ["genuine: \u{fffd}\n".as_bytes(), b"edit me\nraw: \xff\n"].concat();
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "mixed.txt",
        &before,
        json!({ "path": "mixed.txt", "old_string": "edit me", "new_string": "edited" }),
    )
    .await;

    assert!(!out.is_error, "mixed edit = {out:?}");
    assert_eq!(
        after,
        b"genuine: \xef\xbf\xbd\nedited\nraw: \xff\n".to_vec()
    );
    // The sharp end of it: the old path decoded BOTH as U+FFFD and wrote `EF BF BD` for each, so this
    // count was 2 and the lone `0xFF` was gone.
    assert_eq!(
        after.windows(3).filter(|w| *w == b"\xef\xbf\xbd").count(),
        1,
        "the file had exactly one real U+FFFD and must still have exactly one"
    );
}

// New: the needle is a JSON string, so it may be multi-byte UTF-8 — searching bytes must find it and
// `replace_all` must replace every occurrence, in a file whose OTHER bytes are not UTF-8 at all.
#[tokio::test]
async fn edit_file_finds_a_multibyte_needle_in_a_non_utf8_file() {
    let (_dir, root, tools) = code_project(&[], "");
    let before = [
        "标题: 配置\n说明: 配置文件\n".as_bytes(),
        b"legacy: \xb1\xea\xcc\xe2\n",
    ]
    .concat();
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "mixed.yaml",
        &before,
        json!({
            "path": "mixed.yaml",
            "old_string": "配置",
            "new_string": "設定",
            "replace_all": true
        }),
    )
    .await;

    assert!(!out.is_error, "multibyte edit = {out:?}");
    assert!(
        out.text.contains("2 replacement(s)"),
        "multibyte edit = {out:?}"
    );
    let expected = [
        "标题: 設定\n说明: 設定文件\n".as_bytes(),
        b"legacy: \xb1\xea\xcc\xe2\n",
    ]
    .concat();
    assert_eq!(after, expected);
}

// New: the binary policy, which `edit_file` does not own and never did (Go's does not sniff either).
// The gate is `read_file`'s NUL sniff plus the read ledger: a file it refuses is a file that was never
// read, and an unread file cannot be edited. Past the 8000-byte sniff window the sniff says nothing, so
// such a file IS editable — and there byte fidelity is the whole answer: NULs and undecodable bytes
// come back exactly as they went in.
#[tokio::test]
async fn edit_file_treats_a_binary_file_as_bytes() {
    let (_dir, root, tools) = code_project(&[], "");

    // A NUL inside the sniff window: read_file refuses, so edit_file never gets a fresh read.
    write_project_bytes(&root, "early.bin", b"\x00\x01edit me\x00");
    let out = call(&tools, "read_file", json!({ "path": "early.bin" })).await;
    assert!(
        out.is_error && out.text.contains("binary"),
        "binary read = {out:?}"
    );
    let out = call(
        &tools,
        "edit_file",
        json!({ "path": "early.bin", "old_string": "edit me", "new_string": "edited" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("read it with read_file"),
        "unread binary edit = {out:?}"
    );
    assert_eq!(
        fs::read(root.join("early.bin")).expect("read back"),
        b"\x00\x01edit me\x00".to_vec(),
        "a refused edit writes nothing"
    );

    // A NUL past it: the sniff never sees it, the edit runs, and every byte around it survives.
    let tail = b"\nmarker\n\x00\xfe tail\n";
    let before = [&b"x".repeat(8000)[..], tail].concat();
    let (out, after) = edit_bytes(
        &root,
        &tools,
        "late.bin",
        &before,
        json!({ "path": "late.bin", "old_string": "marker", "new_string": "MARKER" }),
    )
    .await;
    assert!(!out.is_error, "late-NUL edit = {out:?}");
    let expected = [&b"x".repeat(8000)[..], b"\nMARKER\n\x00\xfe tail\n"].concat();
    assert_eq!(after, expected);
}
#[tokio::test]
async fn write_file_creates_parents_and_overwrites() {
    let (_dir, root, tools) = code_project(&[], "");

    // New file: no prior read needed, parents created.
    let out = call(
        &tools,
        "write_file",
        json!({ "path": "new/dir/f.txt", "content": "hello" }),
    )
    .await;
    assert!(!out.is_error, "create = {out:?}");
    assert_eq!(out.text, "wrote 5 B to new/dir/f.txt (created)");
    assert_eq!(
        fs::read_to_string(root.join("new/dir/f.txt")).expect("read back"),
        "hello"
    );

    // Overwriting without reading first is rejected; the write above counts as the read.
    write_project_file(&root, "other.txt", "x");
    let out = call(
        &tools,
        "write_file",
        json!({ "path": "other.txt", "content": "y" }),
    )
    .await;
    assert!(
        out.is_error && out.text.contains("read it with read_file"),
        "unread overwrite = {out:?}"
    );
    let out = call(
        &tools,
        "write_file",
        json!({ "path": "new/dir/f.txt", "content": "hello2" }),
    )
    .await;
    assert!(!out.is_error, "tracked overwrite = {out:?}");
    assert_eq!(out.text, "wrote 6 B to new/dir/f.txt (overwritten)");

    let out = call(&tools, "write_file", json!({ "path": "z.txt" })).await;
    assert!(
        out.is_error && out.text == "missing required argument: content",
        "missing content = {out:?}"
    );
}
#[tokio::test]
async fn mutating_code_tools_need_approval_unless_auto_write() {
    let (_dir, root, _tools) = code_project(&[], "");
    let reg = registry(&root, "code:\n");
    for (name, want) in [
        ("edit_file", true),
        ("write_file", true),
        ("read_file", false),
        ("glob", false),
        ("grep", false),
        ("list_dir", false),
    ] {
        assert_eq!(
            reg.requires_approval(name),
            want,
            "requires_approval({name})"
        );
    }

    let auto = registry(&root, "code:\n  auto_write: true\n");
    assert!(
        !auto.requires_approval("write_file") && !auto.requires_approval("edit_file"),
        "auto_write should waive approval"
    );

    // The capability routes through Merge, and unknown tools never require it.
    let merged = merge(vec![Arc::new(reg) as Arc<dyn Dispatcher>]);
    assert!(merged.requires_approval("write_file"));
    assert!(!merged.requires_approval("read_file"));
    assert!(!merged.requires_approval("nope"));
}

/// `tool.Build(Env{ProjectRoot: root}, rawTools(t, yaml), nil)` with no warnings expected.
fn registry(root: &Path, yaml: &str) -> Registry {
    let raw: ToolsConfig = serde_norway::from_str(yaml).expect("tools yaml");
    let mut warnings: Vec<String> = Vec::new();
    let reg = Registry::build(&env_at(root), &raw, &mut |w| warnings.push(w));
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    reg
}
#[tokio::test]
async fn a_read_only_code_set_offers_no_mutating_tool() {
    let (_dir, root, _tools) = code_project(&[], "");
    let tools = tools_at(&root, "read_only: true\n");

    let mut got: Vec<String> = tools.keys().cloned().collect();
    got.sort();
    assert_eq!(got, ["glob", "grep", "list_dir", "read_file"]);
    for (name, tool) in &tools {
        assert!(
            tool.supports_parallel(None),
            "{name} survives read_only but is not parallel-safe"
        );
    }
}
#[test]
fn read_only_and_auto_write_contradict_each_other() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = node("read_only: true\nauto_write: true\n");
    let Err(err) = new_code_set(&env_at(dir.path()), cfg.as_ref()) else {
        panic!("read_only + auto_write must be rejected");
    };
    assert_eq!(
        err.to_string(),
        "read_only and auto_write contradict each other: auto_write approves writes the set does not offer"
    );
}

// WP09).
#[test]
fn only_the_read_only_code_tools_opt_into_parallel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = tools_at(dir.path(), "");
    let want_parallel: HashSet<&str> = ["glob", "grep", "list_dir", "read_file"]
        .into_iter()
        .collect();

    assert_eq!(tools.len(), 6);
    for (name, tool) in &tools {
        // These answer the same for every call, so nil arguments are the honest thing to pass.
        let got = tool.supports_parallel(None);
        assert_eq!(got, want_parallel.contains(name.as_str()), "{name}");
        if !got {
            continue;
        }
        // Whatever opts in must also be harmless in the ways that matter.
        assert!(
            !tool.requires_approval(),
            "{name} runs in parallel but gates on approval: two prompts, one screen"
        );
        assert_eq!(
            tool.presentation(),
            Presentation::Group,
            "{name} runs in parallel but opens a surface or expands a diff"
        );
    }
}

// New (POLICY F-06): Go cut the row to the full 64 KiB cap and then rejected it, leaving an empty window plus a
// continuation marker pointing at the same line — a loop the model could not escape.
#[tokio::test]
async fn read_file_oversized_line_is_cut_to_fit() {
    let big = format!("{}\n", "x".repeat(CODE_MAX_OUTPUT + 4096));
    let (_dir, _root, tools) = code_project(&[("big.txt", big.as_str())], "");

    let out = call(&tools, "read_file", json!({ "path": "big.txt" })).await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.starts_with("     1\txxx"), "the row is served");
    assert!(out.text.ends_with('…'), "the cut row keeps its ellipsis");
    assert_eq!(out.text.len(), CODE_MAX_OUTPUT - 1);
    assert!(
        !out.text.contains("[output truncated"),
        "the whole (single) line was served, so there is nothing to continue"
    );
}

// New: the advertised names, descriptions and schemas are code.go's, byte for byte.
#[test]
fn descriptions_and_schemas_match_go() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = tools_at(dir.path(), "");
    let def = |name: &str| tools[name].def();

    let glob = def("glob");
    assert_eq!(
        glob.description,
        "Find files by name pattern under the project root. Patterns match root-relative paths and support * ? \
         and ** (a pattern without \"/\" matches at any depth, e.g. \"*.go\"). Results are newest-first. .git \
         and root-.gitignore matches are excluded."
    );
    assert_eq!(
        serde_json::Value::Object(glob.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"**/*.go\" or \"cmd/*.go\"." },
                "path": { "type": "string", "description": "Optional directory to search, relative to the project root (default: the root)." },
            },
            "required": ["pattern"],
        })
    );

    let grep = def("grep");
    assert_eq!(
        grep.description,
        "Search file contents under the project root with a Go regular expression (RE2). Output lines are \
         \"path:line: text\" (context lines use \"-\" instead of \":\"). Binary files, .git, and \
         root-.gitignore matches are skipped."
    );
    assert_eq!(
        serde_json::Value::Object(grep.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for (Go/RE2 syntax)." },
                "path": { "type": "string", "description": "Optional directory to search, relative to the project root (default: the root)." },
                "include": { "type": "string", "description": "Optional filename glob filter, e.g. \"*.go\" or \"cmd/**\"." },
                "context": { "type": "integer", "description": "Lines of context to show around each match (0-10, default 0)." },
            },
            "required": ["pattern"],
        })
    );

    let list_dir = def("list_dir");
    assert_eq!(
        list_dir.description,
        "List one directory level under the project root: directories with a trailing \"/\", files with their \
         size. Defaults to the project root itself."
    );
    // No "required" key at all (code.go:478-486).
    assert_eq!(
        serde_json::Value::Object(list_dir.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list, relative to the project root (default: the root)." },
            },
        })
    );

    let read_file = def("read_file");
    assert_eq!(
        read_file.description,
        "Read a text file inside the project root and return its content with line numbers, windowed by the \
         optional \"offset\" (1-based first line) and \"limit\" (line count). Paths are relative to the project \
         root. Reading a file is required before editing it."
    );
    assert_eq!(
        serde_json::Value::Object(read_file.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root." },
                "offset": { "type": "integer", "description": "1-based line number to start reading from (default 1)." },
                "limit": { "type": "integer", "description": "Maximum number of lines to return (default: all remaining lines)." },
            },
            "required": ["path"],
        })
    );

    let edit_file = def("edit_file");
    assert_eq!(
        edit_file.description,
        "Replace an exact string in a file inside the project root. \"old_string\" must match the file content \
         exactly (including whitespace and indentation) and must be unique in the file unless \"replace_all\" \
         is set — extend it with surrounding lines to disambiguate. The file must have been read with read_file \
         first."
    );
    assert_eq!(
        serde_json::Value::Object(edit_file.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring uniqueness (default false)." },
            },
            "required": ["path", "old_string", "new_string"],
        })
    );

    let write_file = def("write_file");
    assert_eq!(
        write_file.description,
        "Create or overwrite a whole file inside the project root (parent directories are created). Overwriting \
         an existing file requires reading it with read_file first — prefer edit_file for changes inside an \
         existing file."
    );
    assert_eq!(
        serde_json::Value::Object(write_file.input_schema.expect("schema")),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the project root." },
                "content": { "type": "string", "description": "Full file content to write." },
            },
            "required": ["path", "content"],
        })
    );
}

// The capability is what switches the
// digest off, and an absent path yields a bare name rather than a digest of the remaining
// arguments: `edit_file`'s `new_string` must never reach a header.
#[test]
fn the_file_tools_header_is_the_path_or_a_bare_name() {
    let (_dir, _root, tools) = code_project(&[], "");
    let edit = &tools["edit_file"];

    let args: JsonObject = json!({
        "path": "internal/ui/model.go",
        "old_string": "before",
        "new_string": "code\n".repeat(500),
    })
    .as_object()
    .cloned()
    .expect("object");
    assert_eq!(
        edit.header_summary(&args).as_deref(),
        Some("internal/ui/model.go"),
        "summary wants the path alone"
    );

    let no_path: JsonObject = json!({ "new_string": "x" })
        .as_object()
        .cloned()
        .expect("object");
    assert_eq!(
        edit.header_summary(&no_path).as_deref(),
        Some(""),
        "summary without a path wants the empty string, which renders as a bare `[edit_file]`"
    );

    // Every file tool answers the same way — the path IS the call (code.go:638-645).
    for name in ["read_file", "list_dir", "write_file"] {
        assert_eq!(
            tools[name].header_summary(&args).as_deref(),
            Some("internal/ui/model.go"),
            "{name} must summarise by path"
        );
    }
}
