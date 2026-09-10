//! `/session` — the mode-isolated picker, the Delete tab, and the resume swap
//! (chat/run.go:717-795; chat/session.go:1046-1091).
//!
//! The listing is MODE-ISOLATED: agent mode lists the project bucket, normal mode the flat
//! root, and the two never merge (only resume-id resolution widens). The Delete tab
//! excludes the CURRENT session — deleting the bundle being written to is not a thing a
//! picker should let happen.
//!
//! The swap ordering is the load-bearing part and is reproduced exactly: close the old
//! writer → install the new one → window title → history/watermark → budget → session
//! totals → clear attachments → adopt the resumed name → conditional model replay →
//! tuning replay → the notice → the echo → status.

use std::path::Path;
use std::sync::{Arc, PoisonError};

use crate::session::SessionInfo;
use crate::ui::facade::{Panel, TabbedSpec};

use crate::repl::replay::{RESUME_ECHO_ROUNDS, echo_rounds, last_rounds};
use crate::repl::run::Repl;
use crate::repl::title::window_title;

/// A session's one-line picker row (chat/session.go:1046-1063 `sessionLabel`).
///
/// `project` is the bucket hint the LISTING carries — Rust's `SessionInfo` deliberately
/// lacks Go's picker-only `Project` field, so the scope supplies it (`TUI_CONTRACTS` §7).
/// Deliberately unstyled: picker rows go through width-based truncation that does not skip
/// ANSI escapes.
pub fn session_label(info: &crate::session::SessionInfo, project: Option<&str>) -> String {
    // Flattened on READ too: bundles written before titles were funnelled through
    // `title_from` may still hold a newline, and one row per session is what the picker's
    // cursor arithmetic assumes.
    let mut title = crate::repl::title::flatten_line(&info.title);
    if title.is_empty() {
        "(untitled)".clone_into(&mut title);
    }
    let label = format!(
        "{title} · {} · {} · {} msgs",
        info.model,
        humanize_time(info.updated_at),
        info.message_count
    );
    match project.filter(|p| !p.is_empty()) {
        Some(p) => format!("{label} [{p}]"),
        None => label,
    }
}

/// A timestamp as a session picker shows it (chat/session.go:1100-1114 `humanizeTime`):
/// `"just now"` under a minute, `"%dm ago"` under an hour, `"%dh ago"` under a day, the
/// absolute `"2006-01-02 15:04"` beyond that, and `"unknown"` for a bundle whose stamp
/// could not be parsed (Go's zero time).
pub(crate) fn humanize_time(t: Option<jiff::Timestamp>) -> String {
    let Some(t) = t else {
        return "unknown".to_owned();
    };
    let secs = (jiff::Timestamp::now().as_second() - t.as_second()).max(0);
    match secs {
        s if s < 60 => "just now".to_owned(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        _ => t
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
    }
}

/// The `[project]` hint for rows of a SCOPED listing (chat/session.go:1033-1040
/// `projectHint`).
///
/// Go read each bundle's own recorded cwd; the Rust `SessionInfo` does not carry it, so
/// the hint comes from the scope every row in that bucket shares — the bucket IS the
/// project (DEVIATIONS3 `[WP50]`).
pub(crate) fn project_hint(scope: Option<&Path>) -> Option<String> {
    scope.map(|root| {
        root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    })
}

/// `/session` — pick a session to resume, or check off sessions to delete. A facade
/// failure is a cancel (see [`super::model::cmd_model`]).
pub(crate) async fn cmd_session(repl: &mut Repl) {
    let infos = match repl.store.list(repl.scope.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            repl.tr.error(&format!("Error: {e}"));
            return;
        }
    };
    if infos.is_empty() {
        repl.tr.notice("No sessions yet.");
        return;
    }
    let hint = project_hint(repl.scope.as_deref());
    let current = repl.session_id();
    let resume_rows: Vec<String> = infos
        .iter()
        .map(|s| session_label(s, hint.as_deref()))
        .collect();
    // The session being written to is not offered for deletion.
    let deletable: Vec<&SessionInfo> = infos.iter().filter(|s| s.id != current).collect();
    let delete_rows: Vec<String> = deletable
        .iter()
        .map(|s| session_label(s, hint.as_deref()))
        .collect();

    let Ok(r) = repl
        .ui
        .tabbed(
            &repl.cancel,
            TabbedSpec {
                panels: vec![
                    Panel::list("Resume".to_owned(), resume_rows).with_search(true),
                    Panel::multi("Delete".to_owned(), delete_rows).with_search(true),
                ],
                ..TabbedSpec::default()
            },
        )
        .await
    else {
        return;
    };
    if r.cancelled {
        return;
    }
    if r.focused == 1 {
        let checked = r
            .panels
            .get(1)
            .map(|p| p.checked.clone())
            .unwrap_or_default();
        let mut deleted = 0;
        for i in checked {
            let Some(s) = deletable.get(i) else { continue };
            match repl.store.delete(&s.id) {
                Ok(()) => deleted += 1,
                Err(e) => repl.tr.error(&format!("Failed to delete {}: {e}", s.id)),
            }
        }
        if deleted > 0 {
            repl.tr.notice(&format!("Deleted {deleted} session(s)."));
        }
        return;
    }
    let cursor = r.panels.first().map_or(0, |p| p.cursor);
    let Some(info) = infos.get(cursor) else {
        return;
    };
    let id = info.id.clone();
    if id == current {
        repl.tr.notice("Already in this session.");
        return;
    }
    let kind = repl.provider.kind();
    let (writer, resumed) = match repl.store.resume(&id, kind) {
        Ok(v) => v,
        Err(e) => {
            repl.tr.error(&format!("Error: {e}"));
            return;
        }
    };

    // ---- the swap, in the ONE order that leaves nothing stale (run.go:762-793) ----
    // Installing the new writer drops the old one, which closes its handle (Go's
    // sw.Close()); the title state resolves the slot per call, so it follows.
    let usage = writer.usage();
    {
        let mut slot = repl.writer.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(writer);
    }
    repl.ui.set_title(&window_title(&repl.session_title()));
    repl.history = resumed.messages;
    repl.persisted = repl.history.len();
    repl.budget.reseed(&repl.history);
    repl.ctxm.seed_totals(usage); // the switched-to session brings its own totals
    repl.pending.clear();
    repl.titler.adopt(); // the resumed bundle brings its own name
    // A live switch takes the bundle's model and tuning whole: no flag is in play any more.
    let warn_tr = Arc::clone(&repl.tr);
    let window = crate::session::replay_session_settings(
        &resumed.meta,
        &mut *repl.provider,
        kind,
        &crate::session::Overrides::default(),
        &mut |w| warn_tr.notice(&w),
    );
    if let Some(n) = window {
        repl.budget.set_window(n);
    }
    repl.tr.notice(&format!(
        "Resumed session {id} ({} messages)",
        repl.history.len()
    ));
    let msgs = last_rounds(&repl.history, RESUME_ECHO_ROUNDS);
    if !msgs.is_empty() {
        let dispatch = Arc::clone(&repl.dispatch);
        let img_dir = repl.with_writer_path(crate::session::SessionWriter::images_path);
        let lines = echo_rounds(
            msgs,
            |n| dispatch.presentation(n) == crate::tool::Presentation::Surface,
            usize::from(repl.ui.width()),
            img_dir.as_deref(),
        );
        repl.tr.echo(&lines);
    }
    repl.push_status();
}

#[cfg(test)]
mod tests {
    use crate::session::SessionInfo;

    use super::{humanize_time, project_hint, session_label};

    fn info(title: &str) -> SessionInfo {
        SessionInfo {
            id: "k7qz3xv9m2ht".to_owned(),
            title: title.to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            updated_at: None,
            message_count: 4,
        }
    }

    // Go: chat/session_test.go TestSessionLabelFlattensStoredTitle — a label never spans
    // rows: a legacy stored newline is flattened on read, because one row per session is
    // what the picker's cursor arithmetic assumes.
    #[test]
    fn test_session_label_flattens_stored_title() {
        assert_eq!(
            session_label(&info("first line\nsecond line"), None),
            "first line second line · gpt-4o · unknown · 4 msgs"
        );
        assert_eq!(
            session_label(&info(""), None),
            "(untitled) · gpt-4o · unknown · 4 msgs"
        );
        // The bucket hint rides at the end, unstyled.
        assert_eq!(
            session_label(&info("a chat"), Some("proj")),
            "a chat · gpt-4o · unknown · 4 msgs [proj]"
        );
        assert_eq!(
            session_label(&info("a chat"), Some("")),
            "a chat · gpt-4o · unknown · 4 msgs",
            "an empty hint adds nothing"
        );
    }

    #[test]
    fn humanize_time_buckets() {
        let now = jiff::Timestamp::now();
        let ago = |secs: i64| Some(now - jiff::SignedDuration::from_secs(secs));
        assert_eq!(humanize_time(None), "unknown");
        assert_eq!(humanize_time(ago(5)), "just now");
        assert_eq!(humanize_time(ago(120)), "2m ago");
        assert_eq!(humanize_time(ago(7_200)), "2h ago");
        // Beyond a day: the absolute stamp, which at least carries the date.
        let old = humanize_time(ago(86_400 * 3));
        assert_eq!(old.len(), 16, "absolute form is `YYYY-MM-DD HH:MM`: {old}");
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
