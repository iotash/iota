//! `/new` (alias `/clear`, never listed) — a new session in the same run (docs/design/new-session-command.md).
//!
//! What it is: `iota run` again, inside this process, without re-reading the config or reconnecting MCP.
//! The session being left is persisted, its title pass given up and its writer dropped — which lets go of
//! its lock — and it stays on disk for `/session` or `iota resume`. The new one gets a new id in the same
//! bucket and takes over what the chat runs under: the endpoint, the model, the five layered parameters
//! with their sources, the system prompt, the tools, the cwd, and whether it is saving at all. What starts
//! from nothing: the history (bar the system prompt), the attachments, the meter, the title, the
//! compaction snooze and the deferred tools a search loaded. What carries on untouched: background jobs
//! (a finished one's notice goes to the NEW session), the approval grant, `/debug`.
//!
//! The screen is not cleared (§3.4): one dim divider in the transcript, the `/session` swap's shape, and
//! the conversation goes on below it.

use std::fmt::Write as _;

use crate::config::AgentMode;
use crate::provider::model::{Message, Role};
use crate::provider::usage::Usage;
use crate::session::{NewSession, SessionError, SessionWriter};
use crate::sync::lock;

use crate::repl::run::Repl;

/// What a bot answers to `/new` (§3.3; bot-mode.md §6 #9).
pub(crate) const BOT_REFUSAL: &str =
    "A bot's session never ends: to start over, run another agent or rename the bot.";

/// `/new` (and `/clear`) with `arg` — the text after the command, which v1 takes none of (§3.7 #9).
pub(crate) async fn cmd_new(repl: &mut Repl, arg: &str) {
    if repl.conv.mode.is_bot() {
        repl.handles.tr.notice(BOT_REFUSAL);
        return;
    }
    if !arg.is_empty() {
        repl.handles.tr.notice("Usage: /new");
        return;
    }
    // A session nobody has said anything in IS a new session: a second `/new` mints no id (§3.5).
    if !started(&repl.conv.history) {
        repl.handles.tr.notice("Already a new session.");
        return;
    }
    let saving = lock(&repl.session.writer).is_some();
    // An unsaved chat has no copy anywhere once it is left: one a model has replied in is not dropped
    // unasked (§3.7 #3). A failed ask is a Keep.
    if !saving && repl.replied() {
        let discard = repl
            .handles
            .ui
            .confirm(
                &repl.handles.cancel,
                "Discard this unsaved chat and start a new one?",
                "Discard",
                "Keep",
            )
            .await
            .unwrap_or(false);
        if !discard {
            return;
        }
    }
    // What the log does not have yet goes first; while it does not land, the session is not left — the
    // backlog would be lost with the history (`/compact`'s refusal).
    if !repl.persist_turn() {
        repl.handles
            .tr
            .error("New session failed: the conversation is not saved yet");
        return;
    }
    let next = if saving {
        match mint(repl) {
            Ok(w) => Some(w),
            Err(e) => {
                repl.handles.tr.error(&format!("Error: {e}"));
                return;
            }
        }
    } else {
        None
    };

    // ---- the switch: the old session lets go, the new one takes over ----
    // A landed name stays in the old meta; an unfinished pass is given up (the `/model` move's way).
    repl.session.abort_title();
    // Installing the new writer drops the old one: its handle closes, its lock goes.
    repl.session.titler.restart(next);
    if saving {
        // Where the chat runs, into the bundle this run just created — what a fresh start writes.
        let moved = crate::repl::liveparams::moved_now(repl);
        if let Some(w) = lock(&repl.session.writer).as_mut() {
            let _ = w.move_to(&moved);
        }
    }
    repl.report_session();
    let system = repl
        .conv
        .history
        .first()
        .filter(|m| m.role() == Role::System)
        .cloned();
    repl.conv.history = system.into_iter().collect();
    repl.session.persisted = 0;
    let dropped = repl.conv.pending.len();
    repl.conv.pending.clear();
    repl.conv.ctxm.reset();
    repl.conv.budget.reseed(&repl.conv.history);
    repl.conv.ctxm.seed_totals(Usage::default());
    repl.conv.compact_declined = 0;
    repl.conv.dispatch.reset_loads();

    let id = lock(&repl.session.writer)
        .as_ref()
        .map(|w| w.id().to_owned());
    repl.handles.tr.notice(&divider(
        id.as_deref(),
        dropped,
        repl.handles.jobs.running(),
    ));
    repl.push_status();
}

/// Whether anything but the system prompt is in the history — something said, by the user, a model or the
/// loop.
fn started(history: &[Message]) -> bool {
    history.iter().any(|m| m.role() != Role::System)
}

/// A bundle for the new session, made as the run made its own at startup: the endpoint and the model it is
/// on NOW, the agent, and the cwd that picks the bucket. Touches no disk until the first append.
fn mint(repl: &Repl) -> Result<SessionWriter, SessionError> {
    let cwd = repl
        .session
        .scope
        .as_deref()
        .or(repl.conv.agent.cwd.as_deref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    repl.session.store.create(NewSession {
        provider: repl.conv.catalog.session_provider().to_owned(),
        cwd,
        project: repl.conv.mode == AgentMode::Agent,
        agent: repl.conv.catalog.agent().to_owned(),
        ..NewSession::new(repl.conv.provider.kind(), repl.conv.provider.model())
    })
}

/// The transcript's one line between the two sessions (§3.4): the new id — or, unsaved, how to keep it —
/// then what did not come along and what keeps running.
fn divider(id: Option<&str>, dropped: usize, jobs: usize) -> String {
    let mut line = id.map_or_else(
        || "New session · not saved · /save keeps it".to_owned(),
        |id| format!("New session {id}"),
    );
    if dropped > 0 {
        let s = if dropped == 1 { "" } else { "s" };
        let _ = write!(line, " ({dropped} attachment{s} dropped)");
    }
    if jobs > 0 {
        let (s, verb) = if jobs == 1 {
            ("", "keeps")
        } else {
            ("s", "keep")
        };
        let _ = write!(line, " · {jobs} background job{s} {verb} running");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::divider;

    #[test]
    fn the_divider_names_what_started_and_what_carries_on() {
        assert_eq!(
            divider(Some("k7qz3xv9m2ht"), 0, 0),
            "New session k7qz3xv9m2ht"
        );
        assert_eq!(
            divider(None, 0, 0),
            "New session · not saved · /save keeps it"
        );
        assert_eq!(
            divider(Some("k7qz3xv9m2ht"), 2, 0),
            "New session k7qz3xv9m2ht (2 attachments dropped)"
        );
        assert_eq!(
            divider(Some("k7qz3xv9m2ht"), 0, 1),
            "New session k7qz3xv9m2ht · 1 background job keeps running"
        );
        assert_eq!(
            divider(None, 1, 3),
            "New session · not saved · /save keeps it (1 attachment dropped) · 3 background jobs keep running"
        );
    }
}
