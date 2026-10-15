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
    // X-77: what did not happen, within what deadline, and what to check — not `connection timed out after`.
    assert_eq!(
        s.error(),
        Some(
            "\"sh\" did not finish the MCP handshake within 300ms — check that it is an MCP server (one that speaks MCP on stdin/stdout)"
        ),
        "hung server should be a resolved failure with a timeout error, got {:?}",
        s.state
    );
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
    // X-77: the header named, and the fix — not rmcp's `Header name 'accept' is reserved …` behind
    // `connect failed: Send message error Transport [rmcp::…]`.
    assert_eq!(
        hdr.error(),
        Some(
            "its config sets the header \"accept\", which iota sends itself — remove it from the headers"
        ),
        "{:?}",
        hdr.state
    );
    assert_eq!(hdr.endpoint, "http://127.0.0.1:9/mcp");
    assert_eq!(
        statuses[1].error(),
        Some("the url must start with http:// or https:// (it is ftp://example.invalid/mcp)")
    );
    assert_eq!(
        statuses[2].error(),
        Some("the entry has neither a command nor a url — give it one")
    );
    assert!(m.tools().is_empty());
    m.close().await;
}

/// A minimal MCP server in POSIX `sh`: newline-delimited JSON-RPC over stdin/stdout answering `initialize`,
/// `tools/list` (one tool `echo`) and `tools/call` (`"pong"`), exiting on stdin EOF.
#[cfg(unix)]
pub(crate) const SH_SERVER: &str = r#"
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
            // A command that exits immediately without speaking MCP: its last stderr line is the reason (X-77).
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
    // The summary names the command and what it said; the detail keeps the chain and the whole stderr.
    assert_eq!(
        broken.error(),
        Some(
            "\"sh\" exited during startup: boom\nconnection closed: initialize response\nstderr:\n  boom"
        ),
        "{broken:?}"
    );
    assert_eq!(
        broken.failure(),
        Some((
            "\"sh\" exited during startup: boom",
            vec![
                "connection closed: initialize response",
                "stderr:",
                "  boom"
            ]
        ))
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
    assert_eq!(statuses[0].error(), Some("interrupted while connecting"));
    assert!(!statuses[0].connected());
    m.close().await;
}

/// DIVERGENCES X-77: an HTTP server that does not connect is ONE line that says what happened and what to do,
/// the cause chain kept as detail lines — never rmcp's `connect failed: Send message error Transport
/// [rmcp::transport::worker::WorkerTransport<…>] error: …, when send initialize request`. One server per kind:
/// a port nobody listens on (the line the owner quoted, 2026-10-15), a 404, a 5xx whose body is an HTML page
/// (a proxy's error page), and a web page where the MCP endpoint should be.
#[tokio::test]
async fn http_failures_say_what_happened_and_what_to_do() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

    let server = MockServer::start().await;
    Mock::given(matchers::path("/404"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":"nope"}"#))
        .mount(&server)
        .await;
    Mock::given(matchers::path("/503"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(
            "<!doctype html>\n<html><head><title>Unable to connect</title></head><body>…</body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;
    Mock::given(matchers::path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html>hi</html>", "text/html"))
        .mount(&server)
        .await;
    // A port that was free a moment ago: connecting to it is refused.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let http = |name: &str, url: String| ServerConfig {
        name: name.to_owned(),
        url,
        auth: iota::mcp::config::AuthMode::None,
        ..ServerConfig::default()
    };
    let refused = format!("http://127.0.0.1:{closed}/api/mcp");
    let m = Manager::new(
        vec![
            http("nb", refused.clone()),
            http("missing", format!("{}/404", server.uri())),
            http("proxied", format!("{}/503", server.uri())),
            http("page", format!("{}/page", server.uri())),
        ],
        // The bare client: a developer machine's system proxy would answer for the closed port itself.
        ManagerOptions::new(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("client"),
            Env::default(),
        ),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    let summaries: Vec<&str> = statuses
        .iter()
        .map(|s| s.failure().expect("failed").0)
        .collect();
    assert_eq!(
        summaries,
        vec![
            format!(
                "can't reach {refused}: connection refused — check the url, and that the server is running"
            ),
            format!(
                "{}/404 answered 404 Not Found — check that the url is the server's MCP endpoint",
                server.uri()
            ),
            format!(
                "{}/503 answered 503 Service Unavailable (\"Unable to connect\") — the server, or a proxy on the way to it, is failing; try again later",
                server.uri()
            ),
            format!(
                "{}/page answered with text/html, not MCP — check that the url is the server's MCP endpoint",
                server.uri()
            ),
        ]
    );
    // The detail keeps what the summary words: the OS's own error at the bottom of the chain, the body cut to
    // a line, an HTML page by its size.
    let (_, detail) = statuses[0].failure().expect("failed");
    assert!(
        detail
            .last()
            .is_some_and(|l| l.starts_with("Connection refused")),
        "{detail:?}"
    );
    assert_eq!(
        statuses[1].failure().expect("failed").1,
        vec![r#"response body: {"error":"nope"}"#]
    );
    let (_, detail) = statuses[2].failure().expect("failed");
    assert!(
        detail.len() == 1 && detail[0].starts_with("response body: an HTML page ("),
        "{detail:?}"
    );
    for s in &statuses {
        let text = s.error().expect("failed");
        for leak in [
            "rmcp::",
            "WorkerTransport",
            "when send initialize request",
            "connect failed",
            "<html",
        ] {
            assert!(!text.contains(leak), "{}: {leak:?} in {text:?}", s.name);
        }
    }
    m.close().await;
}

/// DIVERGENCES X-77/X-78: a stdio server that exits mid-session, after a last word on stderr. The first call
/// that finds it gone marks it down — the status carries that last line, and the tail of its stderr as
/// detail — but its tools stay listed (the owner's decision 2026-10-15: not taken out); that call and every
/// one after it tell the model WHOSE server is gone and what brings it back — not rmcp's `Transport closed`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_to_a_server_that_exited_marks_it_down_and_names_it() {
    // SH_SERVER, except that it says why and exits right after answering `tools/list`.
    let script = SH_SERVER.replace(
        r#"'{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"says pong","inputSchema":{"type":"object"}}]}}' ;;"#,
        r#"'{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"says pong","inputSchema":{"type":"object"}}]}}'; echo 'fatal: out of memory' >&2; exit 1 ;;"#,
    );
    assert_ne!(script, SH_SERVER, "the exit was spliced in");
    let m = Manager::new(vec![stdio("gone", "sh", &["-c", &script])], options());
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses[0].connected(), "{:?}", statuses[0]);
    // The child has exited; give the transport a moment to see the end of its stdout.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let want = "Error calling tool: MCP server \"gone\": it has exited — its tools can't be used again in this \
                session; restarting iota starts it again";
    let cx = RunCtx::new(CancellationToken::new());
    for _ in 0..2 {
        let (text, is_error) =
            iota::tool::model_text(m.call_tool(&cx, "mcp__gone__echo", JsonObject::new()).await);
        assert!(is_error);
        assert_eq!(text, want);
    }

    let status = &m.servers()[0];
    assert!(!status.connected());
    assert_eq!(
        status.failure(),
        Some((
            "exited (its last stderr line: \"fatal: out of memory\") — its tools will fail until iota is restarted",
            vec!["stderr:", "  fatal: out of memory"]
        ))
    );
    // Kept: the model is still offered the tool, and it is still the server's.
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__gone__echo"]
    );
    assert_eq!(status.wire_names(), vec!["mcp__gone__echo"]);
    m.close().await;
}

/// DIVERGENCES X-78: a server that never answers a call — the shape of a wrapper still holding the pipe after
/// the real server died — no longer holds the turn forever: the call is given up at the deadline, the entry's
/// `tool_timeout_sec` over the manager's default, with a sentence that says so and how to allow longer. The
/// server is NOT marked down: it may only be slow.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_nobody_answers_ends_at_the_deadline() {
    // SH_SERVER without its `tools/call` answer: the request is read and ignored.
    let script = SH_SERVER.replace(
        r#"      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"pong"}],"isError":false}}' ;;"#,
        "      : ;;",
    );
    assert_ne!(script, SH_SERVER, "the answer was cut");
    let mut slow = stdio("slow", "sh", &["-c", &script]);
    slow.tool_timeout_sec = Some(1);
    let m = Manager::new(
        vec![stdio("deaf", "sh", &["-c", &script]), slow],
        ManagerOptions {
            call_timeout: Duration::from_millis(300),
            ..options()
        },
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses.iter().all(|s| s.error().is_none()), "{statuses:?}");

    let cx = RunCtx::new(CancellationToken::new());
    for (wire, after, name) in [
        ("mcp__deaf__echo", "300ms", "deaf"),
        ("mcp__slow__echo", "1s", "slow"),
    ] {
        let start = std::time::Instant::now();
        let (text, is_error) =
            iota::tool::model_text(m.call_tool(&cx, wire, JsonObject::new()).await);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{wire} was not given up"
        );
        assert!(is_error);
        assert_eq!(
            text,
            format!(
                "Error calling tool: MCP server \"{name}\": no answer within {after}, so the call was given up — \
                 if the tool is meant to run that long, raise tool_timeout_sec for this server in the config"
            )
        );
    }
    assert!(
        m.servers().iter().all(|s| s.error().is_none()),
        "a timeout is not down"
    );
    m.close().await;
}
