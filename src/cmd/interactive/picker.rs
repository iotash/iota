//! The `iota resume` session picker (chat/session.go:1033-1040, 1091-1094): the one-panel spec the surface
//! shows and the `[project]` hint a scoped listing's rows share.

use std::path::Path;

use crate::repl::session_label;
use crate::session::SessionInfo;
use crate::ui::facade::{Panel, TabbedSpec};

/// chat/session.go:1091 — the `iota resume` picker's only panel.
const PICK_SESSION_TITLE: &str = "Select a session to resume";

/// chat/session.go:1092 — the picker shows 15 rows (`PANEL_HEIGHT` View default).
const PICK_SESSION_HEIGHT: usize = 15;

/// The `iota resume` picker's spec (chat/session.go:1091-1094): ONE searchable list of `session_label` rows.
pub(super) fn picker_spec(rows: &[SessionInfo], project: Option<&str>) -> TabbedSpec {
    TabbedSpec {
        panels: vec![
            Panel::list(
                PICK_SESSION_TITLE.to_owned(),
                rows.iter().map(|s| session_label(s, project)).collect(),
            )
            .with_search(true)
            .with_height(PICK_SESSION_HEIGHT),
        ],
        ..TabbedSpec::default()
    }
}

/// The `[project]` hint every row of a SCOPED listing shares (chat/session.go:1033-1040 `projectHint`; the
/// bucket IS the project — DEVIATIONS3 `[WP50]`).
pub(super) fn project_hint(scope: Option<&Path>) -> Option<String> {
    scope.map(|root| {
        root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    })
}

/// A listing row for the tests here and in the parent's `open_ui` tests.
#[cfg(test)]
pub(super) fn info(id: &str, title: &str) -> SessionInfo {
    SessionInfo {
        id: id.to_owned(),
        title: title.to_owned(),
        model: "gpt-4o".to_owned(),
        provider: "openai".to_owned(),
        updated_at: None,
        message_count: 4,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{PICK_SESSION_HEIGHT, PICK_SESSION_TITLE, info, picker_spec, project_hint};
    use crate::ui::facade::PanelKind;

    /// The `iota resume` picker is `chat.PickSession` byte for byte: ONE searchable list panel titled
    /// "Select a session to resume", 15 rows high, whose items are the `session_label` rows of the LISTING
    /// (chat/session.go:1091-1094).
    #[test]
    fn picker_spec_matches_pick_session() {
        let rows = [info("aaa", "first chat"), info("bbb", "")];
        let spec = picker_spec(&rows, Some("proj"));
        assert_eq!(spec.panels.len(), 1);
        assert!(!spec.enter_advances);
        assert_eq!(spec.refresh_every_ms, 0);
        let panel = &spec.panels[0];
        assert_eq!(panel.title, PICK_SESSION_TITLE);
        assert_eq!(panel.title, "Select a session to resume");
        assert_eq!(panel.kind(), PanelKind::List);
        assert_eq!(panel.height, PICK_SESSION_HEIGHT);
        assert!(panel.search);
        assert_eq!(
            panel.items(),
            vec![
                "first chat · gpt-4o · unknown · 4 msgs [proj]".to_owned(),
                "(untitled) · gpt-4o · unknown · 4 msgs [proj]".to_owned(),
            ]
        );
        // An unscoped listing carries no hint at all.
        assert_eq!(
            picker_spec(&rows, None).panels[0].items()[0],
            "first chat · gpt-4o · unknown · 4 msgs"
        );
    }

    #[test]
    fn project_hint_is_the_bucket_name() {
        assert_eq!(
            project_hint(Some(std::path::Path::new("/work/my-app"))),
            Some("my-app".to_owned())
        );
        assert_eq!(project_hint(None), None);
    }
}
