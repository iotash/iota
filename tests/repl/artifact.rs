//! The T-35 artifact producers: `edit_file`/`write_file` post their unified diff through the `RunCtx`
//! artifact slot (`tool/code_test.go:320`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use iota::chat::turns::{ArtifactSlot, RunCtx};
use iota::provider::model::JsonObject;
use iota::tool::{ArtifactKind, Env, Tool, ToolOutput};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

type Tools = HashMap<String, Arc<dyn Tool>>;

/// A code set over `<temp>/proj` (Go's `newCodeProject`).
fn code_project(files: &[(&str, &str)]) -> (TempDir, Tools) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("proj");
    fs::create_dir_all(&root).expect("project root");
    for (rel, contents) in files {
        write_project_file(&root, rel, contents);
    }
    let tools = iota::tool::code::new_code_set(
        &Env {
            project_root: Some(root),
            ..Env::default()
        },
        None,
    )
    .expect("code set")
    .into_iter()
    .map(|t| (t.def().name, t))
    .collect();
    (dir, tools)
}

fn write_project_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("parents");
    fs::write(path, content).expect("write");
}

/// A call under a FRESH artifact slot; returns the output and whatever the call posted.
async fn call_collecting(
    tools: &Tools,
    name: &str,
    args: serde_json::Value,
) -> (ToolOutput, Option<iota::tool::Artifact>) {
    let slot = ArtifactSlot::default();
    let cx = RunCtx {
        artifact: Some(slot.clone()),
        ..RunCtx::default()
    };
    let args: JsonObject = args.as_object().cloned().unwrap_or_default();
    let out = tools[name].call(&cx, &args).await.expect("no hard error");
    (out, slot.take())
}

/// A slotless call (the fresh-read prerequisite).
async fn call(tools: &Tools, name: &str, args: serde_json::Value) -> ToolOutput {
    let args: JsonObject = args.as_object().cloned().unwrap_or_default();
    tools[name]
        .call(&RunCtx::default(), &args)
        .await
        .expect("no hard error")
}

// Go: tool/code_test.go:320 TestMutationsPostDiffArtifact — edit_file and write_file post
// their unified diff through the artifact side channel: display-only, never part of the
// model-facing result text.
#[tokio::test]
async fn test_mutations_post_diff_artifact() {
    let (_dir, tools) = code_project(&[("a.txt", "one\ntwo\nthree\n")]);

    call(&tools, "read_file", serde_json::json!({"path": "a.txt"})).await; // edit needs a fresh read
    let (out, art) = call_collecting(
        &tools,
        "edit_file",
        serde_json::json!({"path": "a.txt", "old_string": "two", "new_string": "2"}),
    )
    .await;
    assert!(!out.is_error, "edit failed: {:?}", out.text);
    let art = art.expect("edit_file must post a diff artifact");
    assert_eq!(art.kind, ArtifactKind::Diff);
    assert_eq!(art.title, "a.txt");
    let joined = art.lines.join("\n");
    assert!(
        joined.contains("-two") && joined.contains("+2"),
        "diff content wrong:\n{joined}"
    );
    assert!(
        !joined.contains("--- ") && !joined.contains("+++ "),
        "file header rows must be stripped:\n{joined}"
    );
    assert!(
        !out.text.contains("@@"),
        "the diff must not leak into the model-facing result:\n{}",
        out.text
    );

    // Creating a new file diffs against empty content: all additions.
    let (out, art) = call_collecting(
        &tools,
        "write_file",
        serde_json::json!({"path": "new.txt", "content": "alpha\nbeta\n"}),
    )
    .await;
    assert!(!out.is_error, "write failed: {:?}", out.text);
    let art = art.expect("write_file must post a diff artifact for a new file");
    let joined = art.lines.join("\n");
    assert!(
        joined.contains("+alpha") && joined.contains("+beta"),
        "creation diff must be all additions:\n{joined}"
    );

    // Overwriting: the diff runs old → new.
    call(&tools, "read_file", serde_json::json!({"path": "new.txt"})).await;
    let (out, art) = call_collecting(
        &tools,
        "write_file",
        serde_json::json!({"path": "new.txt", "content": "alpha\ngamma\n"}),
    )
    .await;
    assert!(!out.is_error, "overwrite failed: {:?}", out.text);
    let joined = art.expect("overwrite artifact").lines.join("\n");
    assert!(
        joined.contains("-beta") && joined.contains("+gamma"),
        "overwrite diff wrong:\n{joined}"
    );
}
