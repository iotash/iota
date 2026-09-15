//! Public-API tests of the manager (mcp/manager_test.go:426-454 plus the close / reserved-header cases): everything
//! here goes through `Manager::new` → `connect_all` → `Dispatcher` → `close` and spawns only `sh`.

use std::{collections::BTreeMap, time::Duration};

use iota::app::env::Env;
use iota::mcp::config::ServerConfig;
use iota::mcp::{Manager, ManagerOptions};
#[cfg(unix)]
use iota::provider::model::JsonObject;
use iota::tool::Dispatcher;
#[cfg(unix)]
use iota::tool::context::RunCtx;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

/// Options with an empty map resolver (no process environment) and the default 30 s deadline.
fn options() -> ManagerOptions {
    ManagerOptions::new(reqwest::Client::new(), Env::default())
}

/// Only the `sh`-spawning tests build one of these, and those are all `cfg(unix)`.
#[cfg(unix)]
fn stdio(name: &str, command: &str, args: &[&str]) -> ServerConfig {
    ServerConfig {
        name: name.to_owned(),
        command: command.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        ..ServerConfig::default()
    }
}

/// Whether a process with `pid` still exists (zombies included — rmcp's drop-kill also reaps).
#[cfg(unix)]
fn process_exists(pid: &str) -> bool {
    std::process::Command::new("ps")
        .args(["-p", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_never_answers_fails_at_the_connect_timeout() {
    // The subprocess records its own pid so the test can prove it was reaped, then never speaks MCP.
    let pid_file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("iota-mcp-hang-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pid_file);
    let script = format!("echo $$ > '{}'; cat >/dev/null", pid_file.display());
    let opts = ManagerOptions {
        connect_timeout: Duration::from_millis(300),
        ..options()
    };
    let m = Manager::new(vec![stdio("hang", "sh", &["-c", &script])], opts);

    let start = std::time::Instant::now();
    let statuses = m.connect_all(&CancellationToken::new()).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(15),
        "connect_all took {elapsed:?} — timeout did not fire (would be ~300ms)"
    );

    assert_eq!(statuses.len(), 1, "want 1 server");
    assert_eq!(statuses, m.servers());
    let s = &statuses[0];
    assert_eq!(s.name, "hang");
    assert_eq!(s.endpoint, format!("sh -c {script}"));
    assert!(
        s.error().unwrap_or_default().contains("timed out"),
        "hung server should be a resolved failure with a timeout error, got {:?}",
        s.state
    );
    assert_eq!(s.error(), Some("connection timed out after 300ms"));
    assert_eq!(s.segment(), "");
    assert!(s.tools.is_empty() && s.tool_count == 0);
    assert!(
        m.tools().is_empty(),
        "timed-out server should contribute no tools"
    );
    assert_eq!(m.prefix_of()("hang"), "");

    // Close, then the subprocess must be gone (killed when the timed-out connect was dropped, reaped by rmcp).
    m.close().await;
    let mut pid = String::new();
    for _ in 0..50 {
        pid = std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !pid.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!pid.is_empty(), "the subprocess never recorded its pid");
    let mut alive = true;
    for _ in 0..100 {
        alive = process_exists(&pid);
        if !alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = std::fs::remove_file(&pid_file);
    assert!(
        !alive,
        "subprocess {pid} survived the timed-out connect and close()"
    );
}

// DIVERGENCES D-01: rmcp rejects a custom header that collides with a default one; that server fails, nothing
// aborts, and the failure is a per-server `connect failed: …` text.
#[tokio::test]
async fn reserved_header_is_a_connect_failure() {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_owned(), "text/plain".to_owned());
    let m = Manager::new(
        vec![
            ServerConfig {
                name: "hdr".to_owned(),
                url: "http://127.0.0.1:9/mcp".to_owned(),
                headers,
                ..ServerConfig::default()
            },
            // A non-http scheme never opens a transport.
            ServerConfig {
                name: "ftp".to_owned(),
                url: "ftp://example.invalid/mcp".to_owned(),
                ..ServerConfig::default()
            },
            // Neither command nor url.
            ServerConfig {
                name: "empty".to_owned(),
                ..ServerConfig::default()
            },
        ],
        ManagerOptions {
            connect_timeout: Duration::from_secs(5),
            ..options()
        },
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert_eq!(statuses.len(), 3);
    for s in &statuses {
        assert!(s.error().is_some(), "{}: {s:?}", s.name);
        assert_eq!(s.segment(), "");
    }
    let hdr = &statuses[0];
    assert!(
        hdr.error()
            .unwrap_or_default()
            .starts_with("connect failed: "),
        "{:?}",
        hdr.state
    );
    assert!(
        hdr.error()
            .unwrap_or_default()
            .contains("Header name 'accept' is reserved and conflicts with default headers"),
        "{:?}",
        hdr.state
    );
    assert_eq!(hdr.endpoint, "http://127.0.0.1:9/mcp");
    assert_eq!(
        statuses[1].error(),
        Some("unsupported URL scheme: ftp://example.invalid/mcp")
    );
    assert_eq!(
        statuses[2].error(),
        Some("server config must have either command or url")
    );
    assert!(m.tools().is_empty());
    m.close().await;
}

/// A minimal MCP server in POSIX `sh`: newline-delimited JSON-RPC over stdin/stdout answering `initialize`,
/// `tools/list` (one tool `echo`) and `tools/call` (`"pong"`), exiting on stdin EOF.
#[cfg(unix)]
const SH_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"says pong","inputSchema":{"type":"object"}}]}}' ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"pong"}],"isError":false}}' ;;
  esac
done
"#;

// DIVERGENCES D-05: `close()` drops the sessions AND the tool index, so a later call is `unknown tool` (never a
// panic) and `tools()` is empty; a second `close()` is a no-op.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_is_idempotent_and_clears_tools() {
    let m = Manager::new(
        vec![
            stdio("fake", "sh", &["-c", SH_SERVER]),
            // A command that exits immediately without speaking MCP: `connect failed: …` with its stderr appended.
            stdio("broken", "sh", &["-c", "echo boom >&2; exit 3"]),
        ],
        ManagerOptions {
            connect_timeout: Duration::from_secs(10),
            ..options()
        },
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert_eq!(statuses.len(), 2);
    let fake = &statuses[0];
    assert!(fake.connected(), "{fake:?}");
    assert_eq!(fake.segment(), "fake");
    assert_eq!(fake.tools, vec!["echo"]);
    assert_eq!(fake.tool_count, 1);
    assert_eq!(fake.error(), None);
    assert_eq!(fake.wire_prefix(), "mcp__fake__");
    assert_eq!(m.prefix_of()("fake"), "mcp__fake__");
    let broken = &statuses[1];
    assert!(
        broken
            .error()
            .unwrap_or_default()
            .starts_with("connect failed: "),
        "{broken:?}"
    );
    assert!(
        broken
            .error()
            .unwrap_or_default()
            .ends_with("\n  subprocess stderr:\nboom"),
        "stderr appendix missing: {:?}",
        broken.state
    );

    let defs = m.tools();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "mcp__fake__echo");
    assert_eq!(defs[0].description, "says pong");
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), serde_json::Value::from("object"));
    assert_eq!(defs[0].input_schema, Some(schema));
    assert!(!defs[0].deferred);

    let cx = RunCtx::new(CancellationToken::new());
    let out = m
        .call_tool(&cx, "mcp__fake__echo", JsonObject::new())
        .await
        .expect("call before close");
    assert_eq!(out.text, "pong");
    assert!(!out.is_error);
    // Raw names are never dispatchable.
    let err = m
        .call_tool(&cx, "echo", JsonObject::new())
        .await
        .expect_err("raw name");
    assert_eq!(err.to_string(), "unknown tool: echo");

    let start = std::time::Instant::now();
    m.close().await;
    assert!(m.tools().is_empty(), "tools() must be empty after close");
    let err = m
        .call_tool(&cx, "mcp__fake__echo", JsonObject::new())
        .await
        .expect_err("call after close");
    assert_eq!(err.to_string(), "unknown tool: mcp__fake__echo");
    // Statuses are untouched by close (the host may still print them).
    assert_eq!(m.servers()[0].segment(), "fake");
    assert!(m.servers()[0].connected());
    m.close().await;
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "close should return well within the 10 s bound"
    );
}

/// `SH_SERVER` with no tools, writing the `initialize` request it receives to the file named by `$1` before
/// answering it: what the client SAID in the handshake, verbatim off the wire.
#[cfg(unix)]
const SH_HANDSHAKE_RECORDER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" > "$1"
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[]}}' ;;
  esac
done
"#;

// The handshake's `clientInfo` is `iota/<CARGO_PKG_VERSION>` — the version `--version` prints (tests/cmd/cli.rs),
// asserted off the wire rather than off the constant. Until 2026-09-15 every server was told `1.0.0`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_client_info_carries_the_crate_version() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("initialize.json");
    let m = Manager::new(
        vec![stdio(
            "fake",
            "sh",
            &["-c", SH_HANDSHAKE_RECORDER, "sh", &record.to_string_lossy()],
        )],
        ManagerOptions {
            connect_timeout: Duration::from_secs(10),
            ..options()
        },
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses[0].connected(), "{:?}", statuses[0]);
    m.close().await;

    let request: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(request["method"], "initialize");
    assert_eq!(request["params"]["clientInfo"]["name"], "iota");
    assert_eq!(
        request["params"]["clientInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

// A cancelled run token resolves every pending connect as a failure instead of waiting for the deadline.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_token_aborts_connect() {
    let m = Manager::new(
        vec![stdio("hang", "sh", &["-c", "cat >/dev/null"])],
        ManagerOptions {
            connect_timeout: Duration::from_secs(30),
            ..options()
        },
    );
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });
    let start = std::time::Instant::now();
    let statuses = m.connect_all(&cancel).await;
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(statuses[0].error(), Some("connect failed: interrupted"));
    assert!(!statuses[0].connected());
    m.close().await;
}
