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

use crate::mcp::auth::not_logged_in;
use crate::repl::render::transcript::Transcript;
use crate::repl::run::McpEvent;

/// Drains `events` into `tr` until the channel closes or `done` fires (the UI loop thread
/// exited). Only the error's summary — its first line, which says what happened and what to do
/// (`crate::mcp::explain`) — is shown: a stack of transport detail in the chat area buries the
/// conversation. When there IS detail, the line says where it is: the server's page in `/tools`.
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
                let (summary, detail) = crate::mcp::explain::split(&err);
                // An OAuth server with no token is not broken, it is waiting for a login: the line says so
                // and names the way in — the CLI, the one sentence every outlet uses.
                if summary.starts_with("not logged in") {
                    tr.error(&format!("⚠ MCP {} {}", ev.name, not_logged_in(&ev.name)));
                    continue;
                }
                tr.error(&notice_line(&ev.name, summary, !detail.is_empty()));
            }
            () = done.cancelled() => return,
        }
    }
}

/// `⚠ MCP <name>: <summary>`, and ` · details in /tools` when the failure has detail lines to read there.
pub(crate) fn notice_line(name: &str, summary: &str, has_detail: bool) -> String {
    let pointer = if has_detail {
        " · details in /tools"
    } else {
        ""
    };
    format!("⚠ MCP {name}: {summary}{pointer}")
}

#[cfg(test)]
mod tests {
    use super::notice_line;

    // X-77: `⚠ MCP <name>: <summary>`, and the pointer only when there is something to find there.
    #[test]
    fn the_notice_points_at_the_detail_only_when_there_is_some() {
        assert_eq!(
            notice_line(
                "nb",
                "can't reach https://x/mcp: connection refused — check the url",
                true
            ),
            "⚠ MCP nb: can't reach https://x/mcp: connection refused — check the url · details in /tools"
        );
        assert_eq!(
            notice_line(
                "bad",
                "the url must start with http:// or https:// (it is ftp://x)",
                false
            ),
            "⚠ MCP bad: the url must start with http:// or https:// (it is ftp://x)"
        );
    }
}
