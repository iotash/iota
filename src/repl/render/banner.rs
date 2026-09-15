//! The startup banner (chat/run.go:86-105): what the chat is, what it can be told, where
//! it is being saved, and what agent mode loaded.
//!
//! The command line reads the SAME table the completion row and the dispatch chain read,
//! so a conditional command can never be advertised without existing (or exist without
//! being advertised) — the one-table law (`TUI_DESIGN` §9).
//!
//! Go printed these to plain stdout before the Program claimed the terminal. Here they go
//! through the facade like everything else: the frame engine inserts them above the
//! composer, which is the same scrollback in the same order, and the "nothing writes to
//! the terminal except through `ui`" invariant survives.

use crate::agents::Overlay;

use crate::repl::styles::dim;

/// The banner's rows, in Go's order (chat/run.go:86-105).
///
/// `session_id` is empty while the chat is not persisting; `ephemeral` marks a chat that
/// STARTED without a bundle (a `/save` factory exists), which is what turns the session
/// row into the `/save` hint. `overlay` is `Some` only in agent mode.
pub(crate) fn banner_lines(
    commands: &[String],
    session_id: &str,
    ephemeral: bool,
    overlay: Option<&Overlay>,
) -> Vec<String> {
    let mut lines = vec![
        dim("Chat started. Press Ctrl+C to exit."),
        dim(&format!("Commands: {}", commands.join(", "))),
    ];
    if !session_id.is_empty() {
        lines.push(dim(&format!("Session: {session_id}")));
    } else if ephemeral {
        lines.push(dim("Session: not saved — /save [title] keeps this chat"));
    }
    if let Some(o) = overlay {
        if o.file_count() > 0 {
            #[allow(clippy::cast_precision_loss)] // a chain is capped at 32 KiB
            let kb = o.chain_size() as f64 / 1024.0;
            lines.push(dim(&format!(
                "Agent mode: AGENTS.md loaded ({} files, {kb:.1} KB)",
                o.file_count()
            )));
        }
        if o.skill_count() > 0 {
            lines.push(dim(&format!(
                "Agent mode: {} skill(s) available",
                o.skill_count()
            )));
        }
        for warn in o.warnings() {
            lines.push(dim(&format!("⚠ {warn}")));
        }
    }
    lines
}
