#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The Ctrl+G flow over the scripted facade, with stub editors written as shell scripts: the outcome
//! table of the module doc, row by row, and the draft file — where, what it holds, its mode, gone after.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{Choice, EditorSetup, choices, serve};
use crate::repl::render::transcript::Transcript;
use crate::testing::{Reply, ScriptedUi, UiEvent};
use crate::ui::facade::{PanelResult, TabbedResult};

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
            ..EditorSetup::default()
        }
    }
}

/// Runs one Ctrl+G of `draft` through [`serve`]: what the flow answered (the new draft, the notice) and
/// the rows it printed.
async fn ctrl_g(setup: EditorSetup, draft: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let (text, notice, events) = ctrl_g_with(setup, draft, Vec::new()).await;
    (text, notice, printed(&events))
}

/// [`ctrl_g`] over a facade answering the list with `script`: the answer and every event recorded.
async fn ctrl_g_with(
    setup: EditorSetup,
    draft: &str,
    script: Vec<Reply>,
) -> (Option<String>, Option<String>, Vec<UiEvent>) {
    let ui = ScriptedUi::new(script);
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
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the flow never stopped")
        .unwrap();
    (answer.0, answer.1, ui.events())
}

/// The rows printed among `events`.
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
        ..EditorSetup::default()
    };
    let (text, notice, events) = ctrl_g_with(setup, "seed", Vec::new()).await;
    assert_eq!((text, notice), (None, None));
    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::Exec(_))),
        "the terminal was lent"
    );
    assert!(!dir.path().join("drafts").exists());
    let printed = printed(&events);
    assert!(
        printed
            .iter()
            .any(|r| r.contains("⚠ editor iota-no-such-editor: command not found")),
        "{printed:?}"
    );
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

/// A list commit choosing row `cursor` (past the rows: `Other…` with `custom` typed).
fn chose(cursor: usize, custom: &str) -> Reply {
    Reply::Tabbed(TabbedResult {
        panels: vec![PanelResult {
            cursor,
            custom: custom.to_owned(),
            ..PanelResult::default()
        }],
        ..TabbedResult::default()
    })
}

/// The `Editor` lists opened among `events`: each one's rows and cursor.
fn lists(events: &[UiEvent]) -> Vec<(Vec<String>, usize, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Tabbed(t) => t
                .panels
                .first()
                .filter(|p| p.title == "Editor")
                .map(|p| (p.items.clone(), p.cursor, p.custom)),
            _ => None,
        })
        .collect()
}

/// The rows: the environment's editor first, labelled with its variable and kept as typed; then the
/// known editors this machine has, in their order, each as it is stored; the environment's command not
/// listed twice; nothing found, nothing listed.
#[test]
fn the_rows_are_the_environment_then_what_is_installed() {
    let installed = |name: &str| ["vim", "nano", "code", "emacs"].contains(&name);
    let env = ("EDITOR", " vim ".to_owned());
    let rows = choices(Some(&env), installed);
    let commands: Vec<&str> = rows.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(commands, ["vim", "emacs -nw", "nano", "code --wait"]);
    assert_eq!(
        rows[0],
        Choice {
            command: "vim".into(),
            label: "vim · from $EDITOR".into()
        }
    );
    assert_eq!(rows[1].label, "emacs -nw");
    let env = ("VISUAL", "code --wait -n".to_owned());
    let rows = choices(Some(&env), |_| false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "code --wait -n · from $VISUAL");
    // An environment value naming no program is no row.
    assert!(choices(Some(&("EDITOR", "'unbalanced".to_owned())), |_| false).is_empty());
    assert!(choices(None, |_| false).is_empty());
}

/// No `editor:`: the list, below the composer, with the environment's editor first and preselected
/// and `Other…` last; choosing it edits the draft, makes it the editor from then on — the second
/// Ctrl+G opens no list — and writes it as `editor:` into the user config.
#[tokio::test]
async fn the_list_chooses_runs_and_writes_the_editor() {
    let stub = Stub::new(r#"echo more >> "$1""#);
    let ed = stub.path("ed").display().to_string();
    let config = stub.path("home.yaml");
    std::fs::write(&config, "# mine\nmouse: false\n").unwrap();
    let setup = EditorSetup {
        command: None,
        env: Some(("EDITOR", ed.clone())),
        config: Some(config.clone()),
        dir: stub.path("drafts"),
    };
    let ui = ScriptedUi::new(vec![chose(0, "")]);
    let tr = Arc::new(Transcript::new(ui.clone(), None));
    let stop = CancellationToken::new();
    let task = tokio::spawn(serve(ui.clone(), tr, setup, stop.clone()));
    for draft in ["one", "two"] {
        ui.ctrl_g(draft);
        tokio::time::timeout(Duration::from_secs(10), async {
            while ui
                .events()
                .iter()
                .filter(|e| matches!(e, UiEvent::EditDone { .. }))
                .count()
                < if draft == "one" { 1 } else { 2 }
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the flow never answered");
    }
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the flow never stopped")
        .unwrap();
    let events = ui.events();
    let opened = lists(&events);
    assert_eq!(opened.len(), 1, "the list is asked once: {opened:?}");
    let (rows, cursor, custom) = &opened[0];
    assert_eq!(rows[0], format!("{ed} · from $EDITOR"));
    assert_eq!((*cursor, *custom), (0, true));
    let done: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::EditDone { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        done,
        [Some("one\nmore".to_owned()), Some("two\nmore".to_owned())]
    );
    let written = std::fs::read_to_string(&config).unwrap();
    assert_eq!(written, format!("# mine\nmouse: false\neditor: {ed}\n"));
    let shown = printed(&events);
    assert!(
        shown
            .iter()
            .any(|r| r.contains(&format!("saved to {}", config.display()))),
        "{shown:?}"
    );
}

/// A choice that fails — a typed command that is not there, then one that exits non-zero — prints
/// its `⚠` line and opens the list again on that row, writing nothing; the first exit 0 is written.
#[tokio::test]
async fn a_failed_choice_opens_the_list_again_on_its_row() {
    let stub = Stub::new(
        r#"[ -e "$DIR/once" ] && { echo ok >> "$1"; exit 0; }; touch "$DIR/once"; exit 4"#,
    );
    let ed = stub.path("ed").display().to_string();
    let config = stub.path("home.yaml");
    let setup = EditorSetup {
        command: None,
        env: Some(("VISUAL", ed.clone())),
        config: Some(config.clone()),
        dir: stub.path("drafts"),
    };
    let script = vec![
        chose(usize::MAX, "iota-no-such-editor --wait"),
        chose(0, ""),
        chose(0, ""),
    ];
    let (text, notice, events) = ctrl_g_with(setup, "seed", script).await;
    assert_eq!(text.as_deref(), Some("seed\nok"));
    assert_eq!(notice.as_deref(), Some("draft updated"));
    let opened = lists(&events);
    let cursors: Vec<usize> = opened.iter().map(|(_, c, _)| *c).collect();
    // The first list on the environment's row, the second on `Other…` (after the rows), the third on
    // the environment's row again.
    let other = opened[0].0.len();
    assert_eq!(cursors, [0, other, 0]);
    let shown = printed(&events);
    assert!(
        shown
            .iter()
            .any(|r| r.contains("⚠ editor iota-no-such-editor: command not found")),
        "{shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|r| r.contains("exited with status 4 — draft unchanged")),
        "{shown:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        format!("editor: {ed}\n")
    );
}

/// Closing the list leaves the draft and writes nothing; a write that fails keeps the choice for the
/// session and says why.
#[tokio::test]
async fn a_closed_list_or_a_failed_write() {
    let stub = Stub::new(r#"echo more >> "$1""#);
    let ed = stub.path("ed").display().to_string();
    let setup = |config: Option<PathBuf>| EditorSetup {
        command: None,
        env: Some(("EDITOR", ed.clone())),
        config,
        dir: stub.path("drafts"),
    };
    let closed = Reply::Tabbed(TabbedResult {
        cancelled: true,
        ..TabbedResult::default()
    });
    let config = stub.path("home.yaml");
    let (text, notice, _) = ctrl_g_with(setup(Some(config.clone())), "seed", vec![closed]).await;
    assert_eq!((text, notice), (None, None));
    assert!(!config.exists());
    // A file the next load would refuse is not written to.
    std::fs::write(&config, "no_such_key: 1\n").unwrap();
    let (text, _, events) =
        ctrl_g_with(setup(Some(config.clone())), "seed", vec![chose(0, "")]).await;
    assert_eq!(text.as_deref(), Some("seed\nmore"));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "no_such_key: 1\n"
    );
    let shown = printed(&events);
    assert!(
        shown.iter().any(|r| r.contains(&format!(
            "editor: {ed} for this session — not saved: config"
        ))),
        "{shown:?}"
    );
    let (_, _, events) = ctrl_g_with(setup(None), "seed", vec![chose(0, "")]).await;
    assert!(
        printed(&events)
            .iter()
            .any(|r| r.contains("not saved: no home directory")),
    );
}
