//! `/mcp` — the servers of this run and their login state, and the OAuth round trip from inside the chat
//! (brain page `mcp-cli-and-oauth`): `/mcp` opens the live panel (the MCP tab of `/tools`, with each OAuth
//! server's login state), `/mcp login <name>` runs `iota mcp login`'s flow and reconnects the server, `/mcp
//! logout <name>` forgets the tokens and takes the server down.
//!
//! The flow itself is the manager's (`Manager::login`, `Manager::logout`); what is the loop's here is where
//! its lines land (the transcript), the busy row while the browser is out, and ESC as the way to give up
//! waiting — the same busy-plus-cancel-scope shape the `/model` listing uses.

use std::sync::Arc;

use crate::mcp::{ServerState, ServerStatus};
use crate::tool::Dispatcher;
use crate::ui::facade::{Panel, TabbedSpec};

use crate::repl::commands::tools::mcp_status_lines;
use crate::repl::render::styles::dim;
use crate::repl::run::Repl;

/// The refresh cadence of the panel (the one `/tools` uses).
const REFRESH_EVERY_MS: u64 = 500;

/// The panel's rows: `/tools`'s MCP tab, plus one `auth:` line per OAuth server.
pub(crate) fn mcp_panel_lines(dispatch: &dyn Dispatcher, servers: &[ServerStatus]) -> Vec<String> {
    let mut lines = mcp_status_lines(dispatch, servers);
    // The MCP tab prints its servers in order, each headed by a bold name row; the auth line goes under
    // that server's `endpoint:` row, which is the row right after the header.
    for srv in servers {
        let Some(login) = srv.login else { continue };
        let header = format!("{}  [", srv.name);
        if let Some(at) = lines
            .iter()
            .position(|l| crate::text::ansi::strip_sgr(l).starts_with(&header))
        {
            let insert_at = (at + 2).min(lines.len());
            lines.insert(
                insert_at,
                dim(&format!("  auth: oauth ({})", login.as_str())),
            );
        }
    }
    if servers.iter().any(|s| s.login.is_some()) {
        lines.push(dim("/mcp login <name> · /mcp logout <name>"));
    }
    lines
}

/// `/mcp` alone: the live panel.
async fn panel(repl: &Repl) {
    let hook = repl.handles.mcp.servers.as_ref().map(Arc::clone);
    let dispatch = Arc::clone(&repl.conv.dispatch);
    let rows = move || {
        let servers = hook.as_ref().map(|f| f()).unwrap_or_default();
        mcp_panel_lines(&*dispatch, &servers)
    };
    let spec = TabbedSpec {
        refresh_every_ms: REFRESH_EVERY_MS,
        panels: vec![
            Panel::view("MCP".to_owned(), rows())
                .with_wrap(true)
                .with_refresh(Box::new(rows)),
        ],
        ..TabbedSpec::default()
    };
    let _ = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await;
}

/// `/mcp [login <name> | logout <name>]`.
pub(crate) async fn cmd_mcp(repl: &mut Repl, arg: &str) {
    let mut words = arg.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (None, _, _) => panel(repl).await,
        (Some("login"), Some(name), None) => login(repl, name).await,
        (Some("logout"), Some(name), None) => logout(repl, name).await,
        _ => repl
            .handles
            .tr
            .notice("usage: /mcp, /mcp login <name>, /mcp logout <name>"),
    }
}

/// `/mcp login <name>`: the browser goes out, the busy row says so, ESC gives up; the server reconnects on
/// success and its tools join the live set.
async fn login(repl: &mut Repl, name: &str) {
    let Some(manager) = repl.handles.mcp.manager.as_ref().map(Arc::clone) else {
        repl.handles.tr.notice("no MCP servers in this run");
        return;
    };
    let tr = Arc::clone(&repl.handles.tr);
    let ui = Arc::clone(&repl.handles.ui);
    let child = repl.handles.cancel.child_token();
    let scope = ui.push_cancel_scope(child.clone());
    let busy = ui.busy(&format!("logging in to {name} in the browser"));
    // Boxed: the manager's flow (discovery, the browser round trip, the reconnect) is a large future,
    // and this one lives inside the loop's (`clippy::large_futures`).
    let result = Box::pin(manager.login(name, &mut |line| tr.notice(&line), &child)).await;
    busy.stop();
    scope.pop();
    match result {
        Ok(status) => match &status.state {
            ServerState::Connected { .. } => tr.notice(&format!(
                "MCP {name}: connected ({} tools)",
                status.tool_count
            )),
            ServerState::Failed(err) => tr.error(&format!(
                "⚠ MCP {name} failed: {}",
                err.split('\n').next().unwrap_or_default()
            )),
            ServerState::Connecting => tr.notice(&format!("MCP {name}: connecting…")),
        },
        Err(e) if child.is_cancelled() && !repl.handles.cancel.is_cancelled() => {
            tr.notice(&format!("login to {name} cancelled ({e})"));
        }
        Err(e) => tr.error(&format!("⚠ MCP {name} login failed: {e}")),
    }
}

/// `/mcp logout <name>`.
async fn logout(repl: &mut Repl, name: &str) {
    let Some(manager) = repl.handles.mcp.manager.as_ref().map(Arc::clone) else {
        repl.handles.tr.notice("no MCP servers in this run");
        return;
    };
    match Box::pin(manager.logout(name)).await {
        Ok(line) => repl.handles.tr.notice(&line),
        Err(e) => repl.handles.tr.error(&format!("⚠ MCP {e}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::mcp::auth::LoginState;
    use crate::mcp::config::AuthMode;
    use crate::mcp::{ServerState, ServerStatus};
    use crate::text::ansi::strip_sgr;
    use pretty_assertions::assert_eq;

    use super::mcp_panel_lines;
    use crate::repl::commands::tools::tests::DeferStatus;

    /// The panel is the MCP tab plus one `auth:` row under each OAuth server and the command hint; a plain
    /// server gets no auth row and a run without OAuth no hint.
    #[test]
    fn the_panel_adds_the_login_state_under_oauth_servers() {
        let d = DeferStatus::new(&[], &[]);
        let servers = vec![
            ServerStatus {
                name: "nb".to_owned(),
                endpoint: "https://nb.example/api/mcp".to_owned(),
                state: ServerState::Failed("not logged in: run iota mcp login nb".to_owned()),
                auth: AuthMode::Oauth,
                login: Some(LoginState::NotLoggedIn),
                ..ServerStatus::default()
            },
            ServerStatus {
                name: "fs".to_owned(),
                endpoint: "npx server-fs".to_owned(),
                state: ServerState::Connected {
                    segment: "fs".to_owned(),
                },
                tool_count: 1,
                tools: vec!["read".to_owned()],
                ..ServerStatus::default()
            },
        ];
        let plain: Vec<String> = mcp_panel_lines(&d, &servers)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(
            plain,
            vec![
                "2 server(s) · 1 tool(s)",
                "nb  [disconnected]",
                "  endpoint: https://nb.example/api/mcp",
                "  auth: oauth (not logged in)",
                "  tools: (none)",
                "  error: not logged in: run iota mcp login nb",
                "fs  [connected]",
                "  endpoint: npx server-fs",
                "  tools (1): read",
                "/mcp login <name> · /mcp logout <name>",
            ]
        );
        let without: Vec<String> = mcp_panel_lines(&d, &servers[1..])
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert!(
            !without
                .iter()
                .any(|l| l.contains("auth:") || l.contains("/mcp login"))
        );
    }
}
