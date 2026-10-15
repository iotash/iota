//! What an MCP failure says to the person who has to act on it (DIVERGENCES X-77).
//!
//! rmcp's errors are written for whoever debugs rmcp: `Send message error Transport
//! [rmcp::transport::worker::WorkerTransport<…>] error: Client error: error sending request for url (…), when
//! send initialize request` names a generic type and an internal step, and says nothing of why — the
//! `Connection refused` is three `source()`s further down, where `Display` never looks. This module reads the
//! TYPED error (never its text, except the `HTTP <status>: <body>` that rmcp only hands over as text) and
//! words it as ONE line: what happened, to which endpoint, and what to do about it.
//!
//! The text contract every outlet already followed is now the deliberate one: **line 1 is the summary**, the
//! only line the startup notice and `iota mcp list --probe` print; **the lines after it are the detail** — the
//! cause chain, a response body cut to a line, the server's stderr — which the MCP tab of `/tools` and the
//! headless stderr show under the summary, and `iota mcp list --json` carries whole. rmcp's own text, type
//! names included, goes to the developer's channel (`IOTA_LOG`) and nowhere else.

use std::error::Error;
use std::fmt::Write as _;

use rmcp::service::{ClientInitializeError, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpError;

use crate::mcp::config::{AuthMode, ServerConfig};

/// The HTTP transport's error, as both of iota's HTTP clients (bare `reqwest` and the OAuth `AuthClient`
/// over it) produce it.
type HttpError = StreamableHttpError<reqwest::Error>;

/// How much of an HTTP response body the detail keeps (one line, whitespace collapsed).
const BODY_DETAIL_CHARS: usize = 200;

/// How many trailing stderr lines the detail keeps; the capture itself is capped at 64 KiB.
const STDERR_DETAIL_LINES: usize = 40;

/// One failure, worded: the line a user sees, and the detail kept beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Explained {
    /// What happened and what to do, in one line, without the server's name (every outlet puts it in front).
    pub(crate) summary: String,
    /// The cause chain and whatever else explains it, one entry per line; may be empty.
    pub(crate) detail: Vec<String>,
}

impl Explained {
    fn new(summary: impl Into<String>, detail: Vec<String>) -> Self {
        Self {
            summary: summary.into(),
            detail,
        }
    }

    /// The summary, then one line per detail entry — the text `McpError::Connect` carries.
    pub(crate) fn into_text(self) -> String {
        let mut text = self.summary;
        for line in self.detail {
            text.push('\n');
            text.push_str(&line);
        }
        text
    }
}

/// Line 1 of a failure text and the lines after it (the detail), as the outlets print them.
pub(crate) fn split(text: &str) -> (&str, Vec<&str>) {
    let mut lines = text.split('\n');
    let summary = lines.next().unwrap_or_default();
    (summary, lines.filter(|l| !l.trim().is_empty()).collect())
}

/// What a summary calls the server's endpoint: the URL, or the command in quotes.
fn subject(cfg: &ServerConfig) -> String {
    if cfg.url.is_empty() {
        format!("\"{}\"", cfg.command)
    } else {
        cfg.url.clone()
    }
}

/// Every level of `err`'s cause chain, one line each, a level that only repeats the one above it skipped.
fn chain(err: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut next = Some(err);
    while let Some(e) = next {
        let text = one_line(&e.to_string());
        if !text.is_empty() && lines.last().is_none_or(|prev| !prev.contains(&text)) {
            lines.push(text);
        }
        next = e.source();
    }
    lines
}

/// The cause chain of an HTTP transport error. rmcp's `Client(E)` does not name its client error as the
/// `source()`, so the chain is walked from the client error itself — that is where the `Connection refused`
/// lives.
fn http_chain(err: &HttpError) -> Vec<String> {
    match err {
        HttpError::Client(e) => chain(e),
        _ => chain(err),
    }
}

/// `text` on one line: every run of whitespace (newlines included) collapsed to one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` on one line, cut to `max` characters with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    let flat = one_line(text);
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

// ---------------------------------------------------------------- connecting

/// A `serve` (the `initialize` handshake) that failed. `stderr` is what a stdio server wrote before it did —
/// empty for an HTTP server.
pub(crate) fn handshake(
    cfg: &ServerConfig,
    err: &ClientInitializeError,
    stderr: &[u8],
) -> Explained {
    tracing::debug!(server = %cfg.name, error = %err, "mcp handshake failed");
    if cfg.url.is_empty() {
        stdio_handshake(cfg, err, stderr)
    } else {
        http_handshake(cfg, err)
    }
}

/// The HTTP half of [`handshake`].
fn http_handshake(cfg: &ServerConfig, err: &ClientInitializeError) -> Explained {
    let url = &cfg.url;
    match err {
        ClientInitializeError::TransportError { error, .. } => {
            match error.error.downcast_ref::<HttpError>() {
                Some(http) => http_failure(cfg, http),
                None => Explained::new(
                    format!("the MCP handshake with {url} failed — check the url"),
                    chain(error.error.as_ref()),
                ),
            }
        }
        // Discovery and the legacy `initialize` both failed: the legacy attempt is the one a plain server answers.
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            http_handshake(cfg, fallback)
        }
        ClientInitializeError::JsonRpcError(data) => Explained::new(
            format!(
                "{url} refused the MCP handshake: {} (code {})",
                data.message, data.code.0
            ),
            Vec::new(),
        ),
        ClientInitializeError::ConnectionClosed(_) => Explained::new(
            format!(
                "{url} closed the connection during the MCP handshake — check that the url is its MCP endpoint"
            ),
            chain(err),
        ),
        _ => Explained::new(
            format!(
                "{url} did not complete the MCP handshake — check that the url is an MCP endpoint"
            ),
            chain(err),
        ),
    }
}

/// An HTTP transport failure at connect time, worded against the entry (its URL, how it authenticates).
fn http_failure(cfg: &ServerConfig, err: &HttpError) -> Explained {
    let url = &cfg.url;
    match err {
        HttpError::Client(e) => {
            if let Some(status) = e.status() {
                status_failure(cfg, status.as_u16(), &status.to_string(), "")
            } else {
                let (reason, hint) = unreachable(e);
                Explained::new(
                    format!("can't reach {url}: {reason} — {hint}"),
                    http_chain(err),
                )
            }
        }
        HttpError::UnexpectedServerResponse(text) => match parse_status(text) {
            Some((code, status, body)) => status_failure(cfg, code, status, body),
            None => Explained::new(
                format!("{url} answered unexpectedly — check that the url is its MCP endpoint"),
                vec![clip(text, BODY_DETAIL_CHARS)],
            ),
        },
        HttpError::UnexpectedContentType(content_type) => Explained::new(
            format!(
                "{url} answered with {}, not MCP — check that the url is the server's MCP endpoint",
                content_type.as_deref().unwrap_or("no content type")
            ),
            Vec::new(),
        ),
        HttpError::Deserialize(_) => Explained::new(
            format!(
                "{url} answered with something that is not MCP — check that the url is its MCP endpoint"
            ),
            http_chain(err),
        ),
        HttpError::AuthRequired(_) | HttpError::InsufficientScope(_) if has_authorization(cfg) => {
            Explained::new(
                format!("{url} refused the Authorization header in its config — check it"),
                http_chain(err),
            )
        }
        HttpError::AuthRequired(_) | HttpError::InsufficientScope(_) => Explained::new(
            format!("{url} asks for a login — {}", credentials_hint(cfg)),
            http_chain(err),
        ),
        HttpError::ReservedHeaderConflict(name) => Explained::new(
            format!(
                "its config sets the header {name:?}, which iota sends itself — remove it from the headers"
            ),
            Vec::new(),
        ),
        HttpError::Auth(auth) => Explained::new(
            format!(
                "the login failed: {} — run iota mcp login {}",
                one_line(&auth.to_string()),
                cfg.name
            ),
            http_chain(err),
        ),
        _ => Explained::new(
            format!(
                "the MCP handshake with {url} failed: {} — check the url",
                one_line(&err.to_string())
            ),
            http_chain(err),
        ),
    }
}

/// `HTTP 404 Not Found: <body>` (rmcp's text for a non-2xx answer) → `(404, "404 Not Found", body)`.
fn parse_status(text: &str) -> Option<(u16, &str, &str)> {
    let rest = text.strip_prefix("HTTP ")?;
    let (status, body) = rest.split_once(": ").unwrap_or((rest, ""));
    let code = status.split(' ').next()?.parse().ok()?;
    Some((code, status, body))
}

/// A server that answered with an HTTP error status: the status named, the action it implies, and the body
/// kept as one detail line — an HTML page (a proxy's, a web server's) by its title, never whole.
fn status_failure(cfg: &ServerConfig, code: u16, status: &str, body: &str) -> Explained {
    let url = &cfg.url;
    let title = html_title(body);
    let named = match &title {
        Some(title) => format!("{status} (\"{title}\")"),
        None => status.to_owned(),
    };
    let hint = match code {
        401 | 403 => credentials_hint(cfg),
        404 | 405 | 410 => "check that the url is the server's MCP endpoint".to_owned(),
        500..=599 => {
            "the server, or a proxy on the way to it, is failing; try again later".to_owned()
        }
        _ => "check the url".to_owned(),
    };
    let detail = if body.trim().is_empty() {
        Vec::new()
    } else if title.is_some() || body.trim_start().starts_with('<') {
        vec![format!(
            "response body: an HTML page ({} bytes)",
            body.len()
        )]
    } else {
        vec![format!("response body: {}", clip(body, BODY_DETAIL_CHARS))]
    };
    Explained::new(format!("{url} answered {named} — {hint}"), detail)
}

/// Whether the entry sends an `Authorization` header of its own.
fn has_authorization(cfg: &ServerConfig) -> bool {
    cfg.headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("authorization"))
}

/// What to do about a server that wants credentials it was not given, by how the entry authenticates.
fn credentials_hint(cfg: &ServerConfig) -> String {
    if has_authorization(cfg) {
        "check the Authorization header in its config".to_owned()
    } else if cfg.auth == AuthMode::None {
        "the entry says auth: none; add credentials to its headers, or remove auth: none to log in"
            .to_owned()
    } else {
        format!("run iota mcp login {}", cfg.name)
    }
}

/// The `<title>` of an HTML body, when it has one.
fn html_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let len = lower[start..].find("</title>")?;
    let title = one_line(body.get(start..start + len)?);
    (!title.is_empty()).then_some(title)
}

/// Why a request got no HTTP answer at all, in words, and what to check: the I/O error kind at the bottom of
/// the chain when there is one, else the resolver's or TLS's own words.
fn unreachable(err: &reqwest::Error) -> (String, &'static str) {
    const RUNNING: &str = "check the url, and that the server is running";
    if err.is_timeout() {
        return ("timed out".to_owned(), RUNNING);
    }
    let mut next: Option<&(dyn Error + 'static)> = Some(err);
    let mut leaf = String::new();
    while let Some(e) = next {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    return ("connection refused".to_owned(), RUNNING);
                }
                std::io::ErrorKind::ConnectionReset => {
                    return ("connection reset".to_owned(), RUNNING);
                }
                std::io::ErrorKind::TimedOut => return ("timed out".to_owned(), RUNNING),
                _ => {}
            }
        }
        leaf = e.to_string();
        next = e.source();
    }
    let lower = leaf.to_ascii_lowercase();
    if lower.contains("dns error")
        || lower.contains("failed to lookup address")
        || lower.contains("nodename nor servname")
    {
        (
            "host not found".to_owned(),
            "check the host name in the url",
        )
    } else if lower.contains("certificate") || lower.contains("tls") || lower.contains("handshake")
    {
        (
            "the TLS handshake failed".to_owned(),
            "check the url's scheme and the server's certificate",
        )
    } else {
        (one_line(&leaf), RUNNING)
    }
}

/// The stdio half of [`handshake`]: the child exited (or closed its output) before it finished — its last
/// stderr line is usually the reason — or it answered something that is not MCP.
fn stdio_handshake(cfg: &ServerConfig, err: &ClientInitializeError, stderr: &[u8]) -> Explained {
    let cmd = subject(cfg);
    let stderr = String::from_utf8_lossy(stderr);
    let stderr_lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut detail = chain(err);
    if !stderr_lines.is_empty() {
        let skipped = stderr_lines.len().saturating_sub(STDERR_DETAIL_LINES);
        let mut head = String::from("stderr:");
        if skipped > 0 {
            let _ = write!(
                head,
                " (last {STDERR_DETAIL_LINES} of {} lines)",
                stderr_lines.len()
            );
        }
        detail.push(head);
        detail.extend(stderr_lines[skipped..].iter().map(|l| format!("  {l}")));
    }
    let summary = match err {
        ClientInitializeError::ConnectionClosed(_)
        | ClientInitializeError::TransportError { .. } => match stderr_lines.last() {
            Some(last) => format!("{cmd} exited during startup: {}", one_line(last)),
            None => format!(
                "{cmd} exited during startup without saying why — run it by hand to see the error"
            ),
        },
        ClientInitializeError::JsonRpcError(data) => {
            format!(
                "{cmd} refused the MCP handshake: {} (code {})",
                data.message, data.code.0
            )
        }
        _ => format!("{cmd} did not complete the MCP handshake — check that it is an MCP server"),
    };
    Explained::new(summary, detail)
}

/// A stdio command that could not be started at all.
pub(crate) fn spawn(cfg: &ServerConfig, err: &std::io::Error) -> Explained {
    tracing::debug!(server = %cfg.name, error = %err, "mcp spawn failed");
    let cmd = subject(cfg);
    let summary = match err.kind() {
        std::io::ErrorKind::NotFound => {
            format!("can't start {cmd}: command not found — check that it is installed and on PATH")
        }
        std::io::ErrorKind::PermissionDenied => {
            format!("can't start {cmd}: permission denied — check that it is executable")
        }
        _ => format!("can't start {cmd}: {}", one_line(&err.to_string())),
    };
    Explained::new(summary, chain(err))
}

/// The handshake did not finish within the connect deadline.
pub(crate) fn timeout(cfg: &ServerConfig, after: &str) -> String {
    let hint = if cfg.url.is_empty() {
        "check that it is an MCP server (one that speaks MCP on stdin/stdout)"
    } else {
        "check that the server is up and that the url is its MCP endpoint"
    };
    format!(
        "{} did not finish the MCP handshake within {after} — {hint}",
        subject(cfg)
    )
}

/// `tools/list` failed after the handshake succeeded.
pub(crate) fn list_tools(cfg: &ServerConfig, err: &ServiceError) -> Explained {
    tracing::debug!(server = %cfg.name, error = %err, "mcp tools/list failed");
    let (reason, _) = service_reason(err);
    Explained::new(
        format!("connected, but listing its tools failed: {reason}"),
        chain(err),
    )
}

// ---------------------------------------------------------------- calling

/// A `tools/call` that failed under the transport, as the sentence after `MCP server "<name>": ` — what the
/// model reads as the tool result, and the transcript shows under the call.
pub(crate) fn call(err: &ServiceError) -> String {
    tracing::debug!(error = %err, "mcp tools/call failed");
    match service_reason(err) {
        (reason, Some(hint)) => format!("{reason} — {hint}"),
        (reason, None) => reason,
    }
}

/// What a `ServiceError` means in words, and the hint that goes with it (none when the reason is the
/// server's own message).
fn service_reason(err: &ServiceError) -> (String, Option<&'static str>) {
    const RECONNECT: &str = "restart iota to reconnect it";
    match err {
        ServiceError::TransportClosed => (
            "disconnected (it exited or closed the connection)".to_owned(),
            Some(RECONNECT),
        ),
        ServiceError::TransportSend(send) => match send.error.downcast_ref::<HttpError>() {
            Some(HttpError::Client(e)) => {
                if let Some(status) = e.status() {
                    (format!("it answered {status}"), None)
                } else {
                    let (reason, _) = unreachable(e);
                    (
                        format!("can't reach it: {reason}"),
                        Some("check that it is still running"),
                    )
                }
            }
            Some(HttpError::UnexpectedServerResponse(text)) => match parse_status(text) {
                Some((_, status, _)) => (format!("it answered {status}"), None),
                None => ("it answered unexpectedly".to_owned(), None),
            },
            Some(HttpError::SessionExpired) => {
                ("the server dropped the session".to_owned(), Some(RECONNECT))
            }
            Some(
                HttpError::Auth(_) | HttpError::AuthRequired(_) | HttpError::InsufficientScope(_),
            ) => (
                "the server wants a new login".to_owned(),
                Some("run iota mcp login, then restart iota"),
            ),
            // A stdio child that died: the write to its stdin fails before any read sees the EOF.
            _ if is_broken_pipe(send.error.as_ref()) => (
                "disconnected (it exited or closed the connection)".to_owned(),
                Some(RECONNECT),
            ),
            _ => (
                format!(
                    "the request could not be sent: {}",
                    chain(send.error.as_ref()).pop().unwrap_or_default()
                ),
                None,
            ),
        },
        ServiceError::McpError(data) => (
            format!(
                "it returned an error: {} (code {})",
                data.message, data.code.0
            ),
            None,
        ),
        ServiceError::Timeout { .. } => (format!("no answer: {err}"), None),
        ServiceError::UnexpectedResponse => (
            "it answered with something that is not a result".to_owned(),
            None,
        ),
        _ => (one_line(&err.to_string()), None),
    }
}

/// Whether an I/O `BrokenPipe` is anywhere in the chain.
fn is_broken_pipe(err: &(dyn Error + 'static)) -> bool {
    let mut next = Some(err);
    while let Some(e) = next {
        if e.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        {
            return true;
        }
        next = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use rmcp::model::{ErrorCode, ErrorData};
    use rmcp::service::ServiceError;

    use super::{Explained, call, clip, html_title, parse_status, spawn, split};
    use crate::mcp::config::ServerConfig;

    fn stdio(command: &str) -> ServerConfig {
        ServerConfig {
            name: "s".to_owned(),
            command: command.to_owned(),
            ..ServerConfig::default()
        }
    }

    #[test]
    fn the_text_is_the_summary_then_the_detail() {
        let text = Explained::new(
            "what happened — what to do",
            vec!["cause".to_owned(), "  deeper".to_owned()],
        )
        .into_text();
        assert_eq!(text, "what happened — what to do\ncause\n  deeper");
        assert_eq!(
            split(&text),
            ("what happened — what to do", vec!["cause", "  deeper"])
        );
        assert_eq!(split("alone"), ("alone", Vec::<&str>::new()));
    }

    // Until 2026-10-15 these were `connect failed: No such file or directory (os error 2)` and
    // `Transport closed` / `Mcp error: -32602: …` — the OS's or rmcp's words with no subject and no way out.
    #[test]
    fn a_command_that_is_not_there_says_so() {
        let err = std::io::Error::from(std::io::ErrorKind::NotFound);
        let explained = spawn(&stdio("npx"), &err);
        assert_eq!(
            explained.summary,
            "can't start \"npx\": command not found — check that it is installed and on PATH"
        );
        assert_eq!(explained.detail, vec![err.to_string()]);
    }

    #[test]
    fn a_failed_call_says_what_happened() {
        assert_eq!(
            call(&ServiceError::TransportClosed),
            "disconnected (it exited or closed the connection) — restart iota to reconnect it"
        );
        assert_eq!(
            call(&ServiceError::McpError(ErrorData::new(
                ErrorCode(-32602),
                "Invalid params: rpcerror",
                None
            ))),
            "it returned an error: Invalid params: rpcerror (code -32602)"
        );
    }

    #[test]
    fn an_http_status_and_its_body_are_read_not_dumped() {
        assert_eq!(
            parse_status("HTTP 503 Service Unavailable: <html>…"),
            Some((503, "503 Service Unavailable", "<html>…"))
        );
        assert_eq!(
            parse_status("HTTP 401 Unauthorized: "),
            Some((401, "401 Unauthorized", ""))
        );
        assert_eq!(parse_status("Unexpected content type"), None);
        assert_eq!(
            html_title("<!doctype html><HTML><head>\n<TITLE> Unable to\n connect </TITLE>"),
            Some("Unable to connect".to_owned())
        );
        assert_eq!(html_title("{\"error\":\"nope\"}"), None);
        assert_eq!(clip("a\n  b   c", 10), "a b c");
        assert_eq!(clip("abcdefghijkl", 5), "abcde…");
    }
}
