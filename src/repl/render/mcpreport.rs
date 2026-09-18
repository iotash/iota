//! The MCP background reporter (chat/run.go:1159-1177 `reportMCPFailures`).
//!
//! Servers connect in the background, so a failure has no turn to belong to: it is
//! relayed into the scrollback once, as a transcript error, by a task that selects the
//! event channel against the facade's shutdown token. Successful connects say nothing —
//! they show up in `/tools` — unless the merge had something to warn about (a skipped
//! duplicate wire name, DIVERGENCES X-29), which lands as one dim notice per line.

use std::sync::Arc;

use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::repl::render::transcript::Transcript;
use crate::repl::run::McpEvent;

/// Drains `events` into `tr` until the channel closes or `done` fires (the UI loop thread
/// exited). Only the FIRST line of an error is shown — a stack of transport detail in the
/// chat area buries the conversation.
pub(crate) async fn report_mcp_failures(
    mut events: Receiver<McpEvent>,
    tr: Arc<Transcript>,
    done: CancellationToken,
) {
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return };
                for warning in &ev.warnings {
                    tr.notice(&format!("⚠ MCP {}: {warning}", ev.name));
                }
                let Some(err) = ev.error else { continue }; // connected: nothing more to report
                let first = err.split('\n').next().unwrap_or_default();
                // An OAuth server with no token is not broken, it is waiting for a login — and in the
                // chat the way in is the slash command, not the CLI the text names.
                if first.starts_with("not logged in") {
                    tr.error(&format!("⚠ MCP {} not logged in: /mcp login {}", ev.name, ev.name));
                    continue;
                }
                tr.error(&format!("⚠ MCP {} failed: {first}", ev.name));
            }
            () = done.cancelled() => return,
        }
    }
}
