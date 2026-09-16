//! The L4 mock provider: a dependency-free chat-completions server on loopback.
//!
//! `wiremock` cannot express what these scenarios need — a response that DRIPS over
//! seconds so ESC, Ctrl+C, a surface open and a SIGWINCH all land mid-stream — and
//! the terminal layer keeps no serde/tokio dev-dependency for it, so this is one
//! `TcpListener`, one thread per connection, and hand-written SSE.
//!
//! **No environment, no network.** It binds `127.0.0.1:0` and is reached only through the
//! `providers.mock.url` the scenario writes into the pane's config (`http://127.0.0.1:<port>`).
//!
//! **The script is chosen by the prompt.** The scenario types a word into the composer and
//! that word selects the transcript, so one server serves every scenario:
//!
//! | user message  | reply                                                        |
//! |---------------|--------------------------------------------------------------|
//! | `stream N`    | `N` lines `l#00 line` … , 60 ms apart (the interruptible one) |
//! | `think`       | ~1.5 s of reasoning deltas, then a small markdown document   |
//! | `md`          | that markdown document, streamed in 7-byte chunks            |
//! | `exact`       | `EXACTSTART`, a line of exactly 80 `x`, `EXACTEND`            |
//! | `straddle`    | `WIDESTART`, `y` + 40 × `中` (81 columns), `WIDEEND`          |
//! | `blocks`      | [`blocks_doc`]: a 40-line document with a code block, a table and a list, one line per delta |
//! | `emoji`       | [`EMOJI`]: a table whose cells carry emoji, flag sequences and VS16 characters |
//! | `run:<cmd>`   | ONE `shell` tool call, `{"command": <cmd>, "background": true}`, `finish_reason: tool_calls` |
//! | `runfg:<cmd>` | the same call in the foreground (no `background` key)          |
//! | anything else | `echo: <the message>`                                        |
//!
//! Two directives ride ANY message rather than selecting a script:
//!
//! * `fail:<status>:<n>` anywhere in the conversation — the first `n` streaming requests carrying
//!   that message get `<status>` with a JSON error envelope, the rest stream normally. The counter
//!   is keyed by the whole message text, so two scenarios never share one (`FAILS`).
//! * `title:<word>` in the session-title pass (the unary request embeds the first user message):
//!   the pass answers `<word>` alone, so a scenario can choose the window title it then reads back.
//!
//! A request whose LAST message is a tool result (the follow-up of a `run:` call) streams
//! `ran: <the result's first line>` — the text turn that closes a tool round.
//!
//! A non-streaming request (`"stream"` absent or false — the async session-title pass)
//! gets the unary JSON shape instead, so the title provider never sees an SSE body.
//!
//! **Path routing (T3).** The request line's path picks the dialect, so one server also
//! serves the `images` provider that scenario 13 drives:
//!
//! | request                        | reply                                                     |
//! |--------------------------------|-----------------------------------------------------------|
//! | `GET  /models`                 | the three model ids `/model`'s picker lists               |
//! | `GET  /slow/models`            | the same listing after [`SLOW_LIST`] — a fetch ESC can land in |
//! | `POST /images/generations`     | SSE: one `image_generation.partial_image`, then `.completed` |
//! | `POST /images/edits`           | the same (both the multipart and the JSON edit form)      |
//! | anything else (`POST`)         | the chat-completions dialect above                        |
//!
//! Both image frames carry the checked-in 2×2 PNG (`tests/fixtures/images/rb-2x2.png`),
//! base64-encoded here rather than pulled in as a dependency — the partial frame is held
//! for [`PARTIAL_GAP`] so the generation widget is observable before the picture lands.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Gap between streamed lines: slow enough that a scenario can act mid-stream, fast
/// enough that a 60-line transcript still finishes inside four seconds.
const LINE_GAP: Duration = Duration::from_millis(60);

/// Gap between reasoning deltas (30 of them ≈ 1.5 s — past the 1 s the `◇ thought for 1s`
/// marker needs to be more than `<1s`).
const THINK_GAP: Duration = Duration::from_millis(50);

/// Gap between the partial image frame and the finished picture: long enough that a
/// scenario can observe the generation widget, short enough not to pad the suite.
const PARTIAL_GAP: Duration = Duration::from_millis(600);

/// How long `GET /slow/models` holds its answer: long enough for a scenario to press ESC while
/// `Fetching available models…` is up, short enough that the abandoned fetch is over before the
/// scenario's next step could be confused by it.
const SLOW_LIST: Duration = Duration::from_millis(2000);

/// The image both image frames carry — the checked-in 2×2 fixture the L1 imgterm tests use.
const PNG_2X2: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");

/// `fail:<status>:<n>` bookkeeping: how many times each carrying message has been refused.
static FAILS: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());

/// Tool-call ids, unique for the life of the process (a replayed round carries the old ones).
static CALLS: AtomicU64 = AtomicU64::new(0);

/// The markdown document the `think` and `md` scripts return: one of every block the
/// renderer treats differently, small enough to fit a 24-row pane.
const DOC: &str = "# Heading\n\nsome *text* here\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n\ndone.\n";

/// The `blocks` document: 40 source lines, one per delta, [`LINE_GAP`] apart — a 16-line fenced
/// block and an 8-row table (both buffered behind a metered preview row while they stream), a
/// list, and a `BLOCKSEND` marker so a scenario can wait for the whole thing.
fn blocks_doc() -> String {
    let mut d = String::from("# Blocks\n\nBLOCKSSTART\n\n```rust\n");
    for i in 1..=16 {
        let _ = writeln!(d, "fn code_line_{i:02}() {{}}");
    }
    d.push_str("```\n\n| n | name |\n|---|------|\n");
    for i in 1..=8 {
        let _ = writeln!(d, "| {i:02} | row_{i:02} |");
    }
    d.push('\n');
    for i in 1..=8 {
        let _ = writeln!(d, "- item_{i:02}");
    }
    d.push_str("\nBLOCKSEND\n");
    d
}

/// The `emoji` document: a table whose cells carry a plain emoji, a flag sequence (two regional
/// indicators), a VS16 character (a text-presentation base plus U+FE0F) and a skin-tone modifier —
/// every place the width ruler and a terminal's cell accounting can disagree, framed by markers.
const EMOJI: &str = "EMOJISTART\n\n| id | glyph | flag | vs16 | note |\n|---|---|---|---|---|\n| 1 | 😀 | 🇯🇵 | ❤️ | smile |\n| 2 | 🚀 | 🇩🇪 | ☕️ | rocket |\n| 3 | 👍🏽 | 🇧🇷 | ✔️ | thumbs |\n| 4 | 中文 | 🇺🇸 | ⚠️ | cjk |\n\nEMOJIEND\n";

/// Starts the server on an ephemeral loopback port and returns it. The listener thread is
/// detached: it lives as long as the test process.
pub fn start() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            thread::spawn(move || {
                // A client that walks away mid-stream (ESC, Ctrl+C, exit) is the POINT of
                // several scenarios; the resulting broken pipe is not an error here.
                let _ = serve(stream);
            });
        }
    });
    Ok(port)
}

/// Serves exactly one request, then closes (`Connection: close` — keep-alive would buy
/// nothing and cost a state machine).
fn serve(mut sock: TcpStream) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let is_get = line.starts_with("GET ");
    // The request line is `METHOD SP path SP HTTP/1.1`; the path picks the dialect.
    let path = line.split(' ').nth(1).unwrap_or("").to_owned();
    let mut len = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    if is_get {
        if path.contains("/slow/") {
            thread::sleep(SLOW_LIST);
        }
        return models(&mut sock);
    }
    if path.ends_with("/images/generations") || path.ends_with("/images/edits") {
        return images_reply(&mut sock);
    }
    let prompt = last_content(&body).unwrap_or_default();
    if body.contains("\"stream\":true") {
        if let Some(status) = fail_now(&body) {
            return status_reply(&mut sock, status);
        }
        let after_tool = last_role(&body).as_deref() == Some("tool");
        stream_reply(&mut sock, &prompt, after_tool)
    } else {
        unary_reply(&mut sock, &prompt)
    }
}

/// `fail:<status>:<n>` — the status this request must answer with, if its budget is not spent.
///
/// The directive is looked for in EVERY message of the request, not only the last: a retried
/// round re-issues the same conversation, and a steer message queued during the backoff lands
/// behind the message that carries it.
fn fail_now(body: &str) -> Option<u16> {
    let (key, status, n) = contents(body).into_iter().find_map(|c| {
        let at = c.find("fail:")?;
        let mut parts = c[at + "fail:".len()..].splitn(3, ':');
        let status: u16 = parts.next()?.parse().ok()?;
        let digits: String = parts
            .next()?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let n: u32 = digits.parse().ok()?;
        Some((c.clone(), status, n))
    })?;
    let mut fails = FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let seen = fails.entry(key).or_insert(0);
    if *seen >= n {
        return None;
    }
    *seen += 1;
    Some(status)
}

/// A refused request: the status with the JSON error envelope a chat-completions server sends.
fn status_reply(sock: &mut TcpStream, status: u16) -> std::io::Result<()> {
    let text = match status {
        503 => "Service Unavailable",
        500 => "Internal Server Error",
        429 => "Too Many Requests",
        400 => "Bad Request",
        _ => "Error",
    };
    let body = format!("{{\"error\":{{\"message\":\"scripted {status}\",\"type\":\"mock\"}}}}");
    write!(
        sock,
        "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    sock.flush()
}

/// `GET /models` — the three ids `/model`'s picker lists.
fn models(sock: &mut TcpStream) -> std::io::Result<()> {
    let body = r#"{"data":[{"id":"fake"},{"id":"gemini-pro"},{"id":"gpt-4o"}]}"#;
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    sock.flush()
}

/// The unary chat shape (used by the async title pass, which never streams).
fn unary_reply(sock: &mut TcpStream, prompt: &str) -> std::io::Result<()> {
    // `title:<word>` — the session-title pass embeds the first user message in its prompt, so a
    // scenario that typed `title:<word> …` gets exactly `<word>` as the session's name.
    let text = prompt
        .find("title:")
        .map(|at| &prompt[at + "title:".len()..])
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| format!("echo: {prompt}"));
    let body = format!(
        "{{\"choices\":[{{\"message\":{{\"content\":{}}}}}],\"usage\":{{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}}}",
        json_string(&text)
    );
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    sock.flush()
}

/// The streaming chat shape: chunked SSE, the transcript picked by `prompt` — or, when the
/// request's last message is a tool result (`after_tool`), the text that closes the tool round.
fn stream_reply(sock: &mut TcpStream, prompt: &str, after_tool: bool) -> std::io::Result<()> {
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;

    let mut finish = "stop";
    if after_tool {
        let first = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        content(sock, &format!("ran: {first}"))?;
    } else if let Some(cmd) = prompt.strip_prefix("run:") {
        tool_call(sock, cmd.trim(), true)?;
        finish = "tool_calls";
    } else if let Some(cmd) = prompt.strip_prefix("runfg:") {
        tool_call(sock, cmd.trim(), false)?;
        finish = "tool_calls";
    } else if prompt.starts_with("blocks") {
        for line in blocks_doc().lines() {
            content(sock, &format!("{line}\n"))?;
            thread::sleep(LINE_GAP);
        }
    } else if prompt.starts_with("emoji") {
        content(sock, EMOJI)?;
    } else if let Some(rest) = prompt.strip_prefix("stream") {
        let n: usize = rest.trim().parse().unwrap_or(10);
        for i in 0..n {
            content(sock, &format!("l#{i:02} line\n"))?;
            thread::sleep(LINE_GAP);
        }
    } else if prompt.starts_with("think") {
        for i in 0..30 {
            delta(sock, "reasoning", &format!("pondering {i} "))?;
            thread::sleep(THINK_GAP);
        }
        for chunk in chunks(DOC, 7) {
            content(sock, &chunk)?;
            thread::sleep(Duration::from_millis(20));
        }
    } else if prompt.starts_with("md") {
        for chunk in chunks(DOC, 7) {
            content(sock, &chunk)?;
            thread::sleep(Duration::from_millis(20));
        }
    } else if prompt.starts_with("exact") {
        content(sock, &format!("EXACTSTART\n{}\nEXACTEND\n", "x".repeat(80)))?;
    } else if prompt.starts_with("straddle") {
        content(sock, &format!("WIDESTART\ny{}\nWIDEEND\n", "中".repeat(40)))?;
    } else {
        content(sock, &format!("echo: {prompt}"))?;
    }

    chunk(
        sock,
        &format!(
            "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{finish}\"}}],\"usage\":{{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}}}\n\n"
        ),
    )?;
    chunk(sock, "data: [DONE]\n\n")?;
    sock.write_all(b"0\r\n\r\n")?;
    sock.flush()
}

/// One `shell` tool call as the `OpenAI` wire spells it: the first delta names the call (id, name,
/// empty arguments), the second carries the whole argument object — two deltas rather than one
/// because that is the shape a real server streams, and the accumulator has to join them.
fn tool_call(sock: &mut TcpStream, cmd: &str, background: bool) -> std::io::Result<()> {
    let id = format!("call_l4_{}", CALLS.fetch_add(1, Ordering::Relaxed) + 1);
    chunk(
        sock,
        &format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"type\":\"function\",\"function\":{{\"name\":\"shell\",\"arguments\":\"\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
        ),
    )?;
    let args = if background {
        format!("{{\"command\":{},\"background\":true}}", json_string(cmd))
    } else {
        format!("{{\"command\":{}}}", json_string(cmd))
    };
    chunk(
        sock,
        &format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":{}}}}}]}},\"finish_reason\":null}}]}}\n\n",
            json_string(&args)
        ),
    )
}

/// The images dialect (`llm/images.rs::consume_images`): a `text/event-stream` body whose
/// frames are JSON objects with a `type` matched by SUFFIX.
///
/// One `image_generation.partial_image` (which the transcript paints into the generation
/// widget), a pause, then `image_generation.completed` — the only frame that becomes the
/// finished picture. Both carry [`PNG_2X2`]; a stream that ended without a `.completed`
/// frame is an error to the client, so the pair is the minimum honest reply.
fn images_reply(sock: &mut TcpStream) -> std::io::Result<()> {
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;
    let b64 = base64(PNG_2X2);
    chunk(
        sock,
        &format!(
            "data: {{\"type\":\"image_generation.partial_image\",\"b64_json\":\"{b64}\",\"output_format\":\"png\"}}\n\n"
        ),
    )?;
    thread::sleep(PARTIAL_GAP);
    chunk(
        sock,
        &format!(
            "data: {{\"type\":\"image_generation.completed\",\"b64_json\":\"{b64}\",\"output_format\":\"png\"}}\n\n"
        ),
    )?;
    chunk(sock, "data: [DONE]\n\n")?;
    sock.write_all(b"0\r\n\r\n")?;
    sock.flush()
}

/// Standard padded base64 (RFC 4648) — the encoder the images dialect decodes with.
///
/// Written out rather than pulled in: this file's whole point is that the L4 mock has no
/// dependencies of its own.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= group.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One `delta.content` event.
fn content(sock: &mut TcpStream, text: &str) -> std::io::Result<()> {
    delta(sock, "content", text)
}

/// One `delta.<field>` event (`content` or the wire-level `reasoning`).
fn delta(sock: &mut TcpStream, field: &str, text: &str) -> std::io::Result<()> {
    chunk(
        sock,
        &format!(
            "data: {{\"choices\":[{{\"delta\":{{\"{field}\":{}}}}}]}}\n\n",
            json_string(text)
        ),
    )
}

/// Writes one HTTP chunked-transfer frame and flushes it — the flush is what makes the
/// drip visible to the client.
fn chunk(sock: &mut TcpStream, payload: &str) -> std::io::Result<()> {
    write!(sock, "{:x}\r\n", payload.len())?;
    sock.write_all(payload.as_bytes())?;
    sock.write_all(b"\r\n")?;
    sock.flush()
}

/// Splits `s` into pieces of at most `n` bytes, never inside a UTF-8 sequence.
fn chunks(s: &str, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if cur.len() + ch.len_utf8() > n && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Serialises `s` as a JSON string literal (the only serde this file needs).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The LAST `"content":"…"` value in a request body — the message that just arrived: the
/// user's, or the tool result that closes a `run:` round (see [`last_role`]).
fn last_content(body: &str) -> Option<String> {
    contents(body).pop()
}

/// Every `"content":"…"` string value in a request body, in order.
///
/// A hand parser rather than a serde dependency: the bodies are compact machine JSON, the
/// key only ever names a message's text (the advertised tools carry `description`s, never
/// `content`), and only string escapes need undoing.
fn contents(body: &str) -> Vec<String> {
    const KEY: &str = "\"content\":\"";
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(at) = body[from..].find(KEY) {
        let start = from + at + KEY.len();
        found.push(unescape_until_quote(&body[start..]));
        from = start;
    }
    found
}

/// The `role` of the request's LAST message — `tool` when a tool result closes the conversation,
/// which is the only time the mock must NOT read the tail as a prompt.
fn last_role(body: &str) -> Option<String> {
    const KEY: &str = "\"role\":\"";
    let at = body.rfind(KEY)?;
    Some(unescape_until_quote(&body[at + KEY.len()..]))
}

/// A JSON string body up to its closing quote, escapes undone.
fn unescape_until_quote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(v)
                    {
                        out.push(c);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}
