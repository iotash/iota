#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Ctrl+G where the terminal cannot be lent yet (off unix): [`runs_here`]'s two answers, and the flow
//! built on the "no" — one line, no list, no draft file, no `run_external`. Not `cfg(unix)`: the
//! platform is a parameter, so both answers run on every host.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{EditorSetup, NOT_YET, runs_here, serve_on};
use crate::repl::render::transcript::Transcript;
use crate::testing::{ScriptedUi, UiEvent};

#[test]
fn the_editor_runs_on_unix_only_for_now() {
    assert!(runs_here(true));
    assert!(!runs_here(false));
}

/// One Ctrl+G of `draft` through [`serve_on`] with `unix`: every event recorded once it answered.
async fn ctrl_g_on(unix: bool, setup: EditorSetup, draft: &str) -> Vec<UiEvent> {
    let ui = ScriptedUi::new(Vec::new());
    let tr = Arc::new(Transcript::new(ui.clone(), None));
    let stop = CancellationToken::new();
    let task = tokio::spawn(serve_on(ui.clone(), tr, setup, stop.clone(), unix));
    ui.ctrl_g(draft);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ui
            .events()
            .iter()
            .any(|e| matches!(e, UiEvent::EditDone { .. }))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the flow never answered");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the flow never stopped")
        .unwrap();
    ui.events()
}

fn printed(events: &[UiEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Print(rows) => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Off unix, with an editor configured or not, the waiter is served the draft untouched and the
/// conversation gets [`NOT_YET`] — the list never opens, no draft file is written, the terminal is
/// never asked for.
#[tokio::test]
async fn off_unix_ctrl_g_answers_not_yet_and_runs_nothing() {
    for command in [Some(vec!["vi".to_owned()]), None] {
        let dir = tempfile::tempdir().unwrap();
        let setup = EditorSetup {
            command: command.clone(),
            env: Some(("EDITOR", "vi".to_owned())),
            dir: dir.path().join("drafts"),
            ..EditorSetup::default()
        };
        let events = ctrl_g_on(false, setup, "seed").await;
        let done: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                UiEvent::EditDone { text, notice } => Some((text.clone(), notice.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(done, vec![(None, None)], "{command:?}");
        let printed = printed(&events);
        assert_eq!(
            printed.iter().filter(|r| r.contains(NOT_YET)).count(),
            1,
            "{command:?}: {printed:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, UiEvent::Exec(_) | UiEvent::Tabbed(_))),
            "{command:?}: the terminal or the list was asked for: {events:?}"
        );
        assert!(!dir.path().join("drafts").exists(), "{command:?}");
    }
}

/// On unix the same Ctrl+G goes on to the editor: here a missing one, found out by its probe — the
/// flow's own line, not [`NOT_YET`].
#[tokio::test]
async fn on_unix_ctrl_g_goes_on_to_the_editor() {
    let dir = tempfile::tempdir().unwrap();
    let setup = EditorSetup {
        command: Some(vec!["iota-no-such-editor".to_owned()]),
        dir: dir.path().join("drafts"),
        ..EditorSetup::default()
    };
    let printed = printed(&ctrl_g_on(true, setup, "seed").await);
    assert!(
        printed
            .iter()
            .any(|r| r.contains("⚠ editor iota-no-such-editor: command not found")),
        "{printed:?}"
    );
    assert!(!printed.iter().any(|r| r.contains(NOT_YET)), "{printed:?}");
}
