use std::sync::Arc;

use serde_json::json;

use crate::agents::memory::{BotMemory, MEMORY_FILE};
use crate::provider::model::JsonObject;
use crate::tool::context::{ArtifactSlot, RunCtx};
use crate::tool::{ArtifactKind, Presentation, Registry, Tool, ToolEnv};

use super::{REMEMBER, new_memory_set};

fn args(v: serde_json::Value) -> JsonObject {
    match v {
        serde_json::Value::Object(m) => m,
        _ => panic!("object literal expected"),
    }
}

fn tool(dir: &std::path::Path) -> (Arc<dyn Tool>, BotMemory) {
    let memory = BotMemory::new("coder", dir.join("coder"));
    let env = ToolEnv {
        memory: Some(memory.clone()),
        ..ToolEnv::default()
    };
    let mut tools = new_memory_set(&env, None).expect("set");
    assert_eq!(tools.len(), 1);
    (tools.remove(0), memory)
}

async fn call(t: &dyn Tool, a: serde_json::Value) -> (crate::tool::ToolOutput, ArtifactSlot) {
    let slot = ArtifactSlot::default();
    let cx = RunCtx {
        artifact: Some(slot.clone()),
        ..RunCtx::default()
    };
    let out = t.call(&cx, &args(a)).await.expect("never a hard error");
    (out, slot)
}

/// Without a bot's memory the set contributes nothing, so no other session ever sees `remember`.
#[test]
fn the_set_needs_a_bots_memory() {
    assert!(
        new_memory_set(&ToolEnv::default(), None)
            .expect("set")
            .is_empty()
    );
}

/// No approval gate (the write is jailed to the bot's directory), and the call is shown expanded.
#[test]
fn remember_is_ungated_and_expanded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (t, _) = tool(dir.path());
    assert_eq!(t.def().name, REMEMBER);
    assert!(!t.requires_approval(None));
    assert!(!t.requires_approval(Some(&args(json!({"action": "add"})))));
    assert_eq!(t.presentation(), Presentation::Expanded);
    let schema = t.def().input_schema.expect("schema");
    let props: Vec<&String> = schema["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .collect();
    assert_eq!(props, ["action", "old", "section", "source", "text"]);
}

/// A write answers with the size and the section, and posts the changed lines for the transcript.
#[tokio::test]
async fn an_add_answers_with_its_section_and_shows_the_diff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (t, memory) = tool(dir.path());
    let (out, slot) = call(
        &*t,
        json!({"action": "add", "text": "回复用中文", "source": "user"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    let today = crate::agents::harness::today();
    assert_eq!(
        out.text,
        format!(
            "saved to MEMORY.md ## User (0.1 / 8 KiB)\n\n## User\n- [user] 回复用中文 ({today})"
        )
    );
    let art = slot.take().expect("a diff artifact");
    assert_eq!(art.kind, ArtifactKind::Diff);
    assert_eq!(art.title, MEMORY_FILE);
    assert!(
        art.lines
            .iter()
            .any(|l| l == &format!("+- [user] 回复用中文 ({today})")),
        "{:?}",
        art.lines
    );
    assert!(memory.path().exists());
    assert_eq!(memory.writes().take().len(), 1);

    // The other actions and the section argument reach the same file.
    let (out, _) = call(
        &*t,
        json!({"action": "add", "text": "发布见 RELEASING.md", "source": "inferred", "section": "Project: iota"}),
    )
    .await;
    assert!(
        out.text.starts_with("saved to MEMORY.md ## Project: iota"),
        "{}",
        out.text
    );
    let (out, _) = call(
        &*t,
        json!({"action": "replace", "old": "RELEASING", "text": "发布见 docs/RELEASING.md", "source": "user"}),
    )
    .await;
    assert!(
        out.text.contains("- [user] 发布见 docs/RELEASING.md"),
        "{}",
        out.text
    );
    let (out, _) = call(&*t, json!({"action": "remove", "old": "docs/RELEASING"})).await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(memory.writes().take().len(), 3);
}

/// Argument refusals are model-facing errors, and nothing is written for them.
#[tokio::test]
async fn bad_arguments_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (t, memory) = tool(dir.path());
    for (a, want) in [
        (json!({}), "missing required argument: action"),
        (
            json!({"action": "forget"}),
            "action must be \"add\", \"replace\" or \"remove\", got \"forget\"",
        ),
        (
            json!({"action": "add", "text": "x"}),
            "source is required: \"user\" or \"inferred\"",
        ),
        (
            json!({"action": "replace", "old": "x", "text": "y", "source": "me"}),
            "source must be \"user\" or \"inferred\", got \"me\"",
        ),
        (
            json!({"action": "add", "text": "x", "source": "user", "section": "Misc"}),
            "section must be \"User\", \"Project: <name>\" or \"Open threads\", got \"Misc\"",
        ),
        (
            json!({"action": "add", "text": "Bearer abc", "source": "user"}),
            crate::agents::memory::SECRET_REFUSAL,
        ),
    ] {
        let (out, slot) = call(&*t, a).await;
        assert!(out.is_error);
        assert_eq!(out.text, want);
        assert!(slot.take().is_none());
    }
    assert!(!memory.path().exists());
    assert!(memory.writes().take().is_empty());
}

/// The registry reports the tool's own answers (`build_dispatcher` enables the set this way).
#[test]
fn the_registry_carries_the_presentation() {
    use crate::tool::Dispatcher as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let env = ToolEnv {
        memory: Some(BotMemory::new("coder", dir.path().join("coder"))),
        ..ToolEnv::default()
    };
    let mut r = Registry::default();
    r.enable_set(&env, crate::tool::sets::MEMORY_SET, &mut |w| {
        panic!("unexpected warning {w}")
    });
    assert_eq!(r.presentation(REMEMBER), Presentation::Expanded);
    assert!(!r.requires_approval(REMEMBER, None));
}
