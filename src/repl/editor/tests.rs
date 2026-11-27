#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The Ctrl+G flow over the scripted facade, with stub editors written as shell scripts: the outcome
//! table of the module doc, row by row, and the draft file — where, what it holds, its mode, gone after.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{EditorSetup, NO_EDITOR, serve};
use crate::repl::render::transcript::Transcript;
use crate::testing::{ScriptedUi, UiEvent};

/// A scratch directory with a stub editor `ed` whose body is `body` (`$1` is the draft's path,
/// `$DIR` this directory).
struct Stub {
    dir: tempfile::TempDir,
}

impl Stub {
    fn new(body: &str) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let ed = dir.path().join("ed");
        std::fs::write(
            &ed,
            format!("#!/bin/sh\nDIR='{}'\n{body}\n", dir.path().display()),
        )
        .unwrap();
        std::fs::set_permissions(&ed, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn setup(&self) -> EditorSetup {
        EditorSetup {
            command: Some(vec![self.path("ed").display().to_string()]),
            dir: self.path("drafts"),
        }
    }
}

/// Runs one Ctrl+G of `draft` through [`serve`]: what the flow answered (the new draft, the notice) and
/// the rows it printed.
async fn ctrl_g(setup: EditorSetup, draft: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let ui = ScriptedUi::new(Vec::new());
    let tr = Arc::new(Transcript::new(ui.clone(), None));
    let stop = CancellationToken::new();
    let task = tokio::spawn(serve(ui.clone(), tr, setup, stop.clone()));
    ui.ctrl_g(draft);
    let answer = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let found = ui.events().into_iter().find_map(|e| match e {
                UiEvent::EditDone { text, notice } => Some((text, notice)),
                _ => None,
            });
            if let Some(found) = found {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the flow never answered");
    stop.cancel();
    task.await.unwrap();
    let printed = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Print(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    (answer.0, answer.1, printed)
}

fn drafts_left(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

/// Exit 0 with the text as it was handed out: the draft is left alone. The file the editor saw is
/// `draft-<pid>.md` under the configured directory, mode 0600, holding the draft and one newline,
/// and it is gone afterwards.
#[tokio::test]
async fn an_unchanged_file_leaves_the_draft() {
    let stub = Stub::new(
        r#"cp "$1" "$DIR/seen"; basename "$1" > "$DIR/name"; find "$1" -perm 600 > "$DIR/perm""#,
    );
    let (text, notice, printed) = ctrl_g(stub.setup(), "line one\nline two").await;
    assert_eq!((text, notice.as_deref()), (None, Some("draft unchanged")));
    assert!(printed.is_empty(), "{printed:?}");
    assert_eq!(
        std::fs::read_to_string(stub.path("seen")).unwrap(),
        "line one\nline two\n"
    );
    assert_eq!(
        std::fs::read_to_string(stub.path("name")).unwrap().trim(),
        format!("draft-{}.md", std::process::id())
    );
    assert!(
        !std::fs::read_to_string(stub.path("perm"))
            .unwrap()
            .is_empty(),
        "not 0600"
    );
    assert_eq!(
        drafts_left(&stub.path("drafts")),
        0,
        "the draft file was left behind"
    );
}

/// Exit 0 with the text changed: the new draft, CRLF read as LF and the trailing newlines dropped.
#[tokio::test]
async fn a_changed_file_replaces_the_draft() {
    let stub = Stub::new(r#"printf 'more\r\n\n\n' >> "$1""#);
    let (text, notice, _) = ctrl_g(stub.setup(), "seed").await;
    assert_eq!(text.as_deref(), Some("seed\nmore"));
    assert_eq!(notice.as_deref(), Some("draft updated"));
}

/// Exit 0 with the file emptied: the draft is cleared.
#[tokio::test]
async fn an_emptied_file_clears_the_draft() {
    let stub = Stub::new(r#": > "$1""#);
    let (text, notice, _) = ctrl_g(stub.setup(), "seed").await;
    assert_eq!(text.as_deref(), Some(""));
    assert_eq!(notice.as_deref(), Some("draft cleared"));
}

/// A non-zero exit (vim's `:cq`) leaves the draft, whatever the file holds, and says the status.
#[tokio::test]
async fn a_failed_editor_leaves_the_draft() {
    let stub = Stub::new(r#"echo changed > "$1"; exit 3"#);
    let (text, notice, printed) = ctrl_g(stub.setup(), "seed").await;
    assert_eq!(text, None);
    assert_eq!(
        notice.as_deref(),
        Some("editor exited with status 3 — draft unchanged")
    );
    assert!(printed.is_empty(), "{printed:?}");
}

/// A file the editor removed, or left as bytes that are not UTF-8: the draft untouched, a `⚠` line.
#[tokio::test]
async fn a_file_gone_or_not_utf8_is_an_error_line() {
    let stub = Stub::new(r#"rm "$1""#);
    let (text, notice, printed) = ctrl_g(stub.setup(), "seed").await;
    assert_eq!((text, notice), (None, None));
    assert!(
        printed.iter().any(|r| r.contains("⚠ editor: cannot read")),
        "{printed:?}"
    );
    let stub = Stub::new(r#"printf '\377\376' > "$1""#);
    let (text, notice, printed) = ctrl_g(stub.setup(), "seed").await;
    assert_eq!((text, notice), (None, None));
    assert!(
        printed.iter().any(|r| r.contains("is not UTF-8")),
        "{printed:?}"
    );
}

/// A command that is not there is found out before the terminal is lent: no program is run, no file
/// written, one `⚠` line names it.
#[tokio::test]
async fn a_missing_command_never_lends_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let setup = EditorSetup {
        command: Some(vec!["iota-no-such-editor".to_owned(), "--wait".to_owned()]),
        dir: dir.path().join("drafts"),
    };
    let ui = ScriptedUi::new(Vec::new());
    let tr = Arc::new(Transcript::new(ui.clone(), None));
    let (text, notice) = super::edit(ui.as_ref(), &tr, &setup, "seed").await;
    assert_eq!((text, notice), (None, None));
    assert!(
        !ui.events().iter().any(|e| matches!(e, UiEvent::Exec(_))),
        "the terminal was lent"
    );
    assert!(!dir.path().join("drafts").exists());
    let printed: Vec<String> = ui
        .events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Print(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        printed
            .iter()
            .any(|r| r.contains("⚠ editor iota-no-such-editor: command not found")),
        "{printed:?}"
    );
}

/// No `editor:` configured: one line naming the key, nothing run.
#[tokio::test]
async fn no_editor_configured_says_which_key_to_set() {
    let (text, notice, printed) = ctrl_g(EditorSetup::default(), "seed").await;
    assert_eq!((text, notice), (None, None));
    assert_eq!(printed.len(), 1, "{printed:?}");
    assert!(printed[0].contains(NO_EDITOR), "{printed:?}");
    assert!(NO_EDITOR.contains("editor:"));
}

/// The configured words come first and the draft's path last; a leftover of this pid's is replaced,
/// not appended to.
#[tokio::test]
async fn the_argv_ends_with_the_draft_and_a_leftover_is_replaced() {
    let stub = Stub::new(r#"printf '%s\n' "$@" > "$DIR/args"; cp "$2" "$DIR/seen""#);
    let mut setup = stub.setup();
    setup.command.as_mut().unwrap().push("--wait".to_owned());
    std::fs::create_dir_all(&setup.dir).unwrap();
    let leftover = setup.dir.join(format!("draft-{}.md", std::process::id()));
    std::fs::write(&leftover, "stale\n").unwrap();
    let (_, notice, _) = ctrl_g(setup, "fresh").await;
    assert_eq!(notice.as_deref(), Some("draft unchanged"));
    assert_eq!(
        std::fs::read_to_string(stub.path("args")).unwrap(),
        format!("--wait\n{}\n", leftover.display())
    );
    assert_eq!(
        std::fs::read_to_string(stub.path("seen")).unwrap(),
        "fresh\n"
    );
    assert!(!leftover.exists());
}
