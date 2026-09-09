//! The T-35 artifact producers: `edit_file`/`write_file` post their unified diff and the
//! delegate tool posts its accounting note through the `RunCtx` artifact slot
//! (`tool/code_test.go:320`, `tool/delegate_test.go:68,95`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{collections::HashMap, fs, path::Path, sync::Arc, time::Duration};

use iota::BoxFuture;
use iota::chat::turns::{ArtifactSlot, RunCtx};
use iota::provider::model::JsonObject;
use iota::provider::usage::Usage;
use iota::tool::{
    AgentInfo, ArtifactKind, DelegateOutcome, DelegateResult, DelegateSpec, Delegator, Env, Tool,
    ToolOutput,
};
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

/// A delegator whose one child ends with the scripted rounds/duration and failure.
struct FakeDelegator {
    names: Vec<String>,
    agents: HashMap<String, AgentInfo>,
    rounds: u32,
    duration: Duration,
    error_text: Option<String>,
}

impl Delegator for FakeDelegator {
    fn agent_names(&self) -> &[String] {
        &self.names
    }

    fn agent(&self, name: &str) -> Option<&AgentInfo> {
        self.agents.get(name)
    }

    fn run<'a>(&'a self, _cx: &'a RunCtx, _spec: DelegateSpec) -> BoxFuture<'a, DelegateOutcome> {
        Box::pin(async move {
            DelegateOutcome {
                result: DelegateResult {
                    reply: String::new(),
                    rounds: self.rounds,
                    usage: Usage::default(),
                    duration: self.duration,
                },
                error: self.error_text.clone().map(Into::into),
            }
        })
    }
}

fn delegate_tool(fake: FakeDelegator) -> Arc<dyn Tool> {
    let tools = iota::tool::delegate::new_delegate_set(
        &Env {
            delegate: Some(Arc::new(fake)),
            ..Env::default()
        },
        None,
    )
    .expect("delegate set");
    assert_eq!(tools.len(), 1);
    Arc::clone(&tools[0])
}

// Go: tool/delegate_test.go:68 TestDelegateReportsCostOfAFailedChild — a failed child is
// billed for the rounds it completed, and it is the run worth investigating: the
// accounting must not go missing exactly when the cost is surprising.
#[tokio::test]
async fn test_delegate_reports_cost_of_a_failed_child() {
    let tool = delegate_tool(FakeDelegator {
        names: vec!["a".to_owned()],
        agents: HashMap::from([("a".to_owned(), AgentInfo::default())]),
        rounds: 3,
        duration: Duration::from_secs(2),
        error_text: Some("upstream exploded".to_owned()),
    });

    let slot = ArtifactSlot::default();
    let cx = RunCtx {
        artifact: Some(slot.clone()),
        ..RunCtx::default()
    };
    let args: JsonObject = serde_json::json!({"agent": "a", "task": "t"})
        .as_object()
        .cloned()
        .unwrap();
    let out = tool
        .call(&cx, &args)
        .await
        .expect("a child's failure must be the parent's result");
    assert!(
        out.is_error && out.text.contains("upstream exploded"),
        "want the failure as an error result: {:?}",
        out.text
    );
    let art = slot
        .take()
        .expect("a failed delegation posted no accounting");
    assert_eq!(art.kind, ArtifactKind::Note);
    // The exact note rows (tool/delegate.go:169-180): rounds, tokens, elapsed.
    assert_eq!(art.lines, vec!["3 rounds", "0 tokens", "2s"]);
}

// Go: tool/delegate_test.go:95 TestDelegateSkipsAccountingWhenNothingRan — a child that
// never reached the provider has nothing to account for, and a "0 rounds · 0 tokens" row
// would be noise dressed as information.
#[tokio::test]
async fn test_delegate_skips_accounting_when_nothing_ran() {
    let tool = delegate_tool(FakeDelegator {
        names: vec!["a".to_owned()],
        agents: HashMap::from([("a".to_owned(), AgentInfo::default())]),
        rounds: 0,
        duration: Duration::ZERO,
        error_text: Some("no api key".to_owned()),
    });

    let slot = ArtifactSlot::default();
    let cx = RunCtx {
        artifact: Some(slot.clone()),
        ..RunCtx::default()
    };
    let args: JsonObject = serde_json::json!({"agent": "a", "task": "t"})
        .as_object()
        .cloned()
        .unwrap();
    let out = tool.call(&cx, &args).await.expect("tool result");
    assert!(out.is_error, "a build failure must be an error result");
    assert!(
        slot.take().is_none(),
        "posted accounting for a child that never ran"
    );
}
