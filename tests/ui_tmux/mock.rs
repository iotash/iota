//! The L4 mock provider: a dependency-free chat-completions server on loopback.
//!
//! `wiremock` cannot express what these scenarios need — a response that DRIPS over
//! seconds so ESC, Ctrl+C, a surface open and a SIGWINCH all land mid-stream — and
//! the terminal layer keeps no serde/tokio dev-dependency for it, so this is one
//! `TcpListener`, one thread per connection, and hand-written SSE.
//!
//! **No environment, no network.** It binds `127.0.0.1:0` and is reached only through the
//! flags the scenario passes (`-u http://127.0.0.1:<port> -k test -M fake`).
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
//! | anything else | `echo: <the message>`                                        |
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
//! | `POST /images/generations`     | SSE: one `image_generation.partial_image`, then `.completed` |
//! | `POST /images/edits`           | the same (both the multipart and the JSON edit form)      |
//! | anything else (`POST`)         | the chat-completions dialect above                        |
//!
//! Both image frames carry the checked-in 2×2 PNG (`tests/fixtures/images/rb-2x2.png`),
//! base64-encoded here rather than pulled in as a dependency — the partial frame is held
//! for [`PARTIAL_GAP`] so the generation widget is observable before the picture lands.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
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

/// The image both image frames carry — the checked-in 2×2 fixture the L1 imgterm tests use.
const PNG_2X2: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");

/// The markdown document the `think` and `md` scripts return: one of every block the
/// renderer treats differently, small enough to fit a 24-row pane.
const DOC: &str = "# Heading\n\nsome *text* here\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n\ndone.\n";

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
        return models(&mut sock);
    }
    if path.ends_with("/images/generations") || path.ends_with("/images/edits") {
        return images_reply(&mut sock);
    }
    let prompt = last_content(&body).unwrap_or_default();
    if body.contains("\"stream\":true") {
        stream_reply(&mut sock, &prompt)
    } else {
        unary_reply(&mut sock, &prompt)
    }
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
    let body = format!(
        "{{\"choices\":[{{\"message\":{{\"content\":{}}}}}],\"usage\":{{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}}}",
        json_string(&format!("echo: {prompt}"))
    );
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    sock.flush()
}

/// The streaming chat shape: chunked SSE, the transcript picked by `prompt`.
fn stream_reply(sock: &mut TcpStream, prompt: &str) -> std::io::Result<()> {
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;

    if let Some(rest) = prompt.strip_prefix("stream") {
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
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
    )?;
    chunk(sock, "data: [DONE]\n\n")?;
    sock.write_all(b"0\r\n\r\n")?;
    sock.flush()
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

/// The LAST `"content":"…"` value in a request body — the message that just arrived.
///
/// A hand parser rather than a serde dependency: the bodies are compact machine JSON, the
/// tail message is always the user's (this harness advertises no tools, so no tool result
/// can follow it), and only string escapes need undoing.
fn last_content(body: &str) -> Option<String> {
    const KEY: &str = "\"content\":\"";
    let mut found = None;
    let mut from = 0usize;
    while let Some(at) = body[from..].find(KEY) {
        let start = from + at + KEY.len();
        let mut out = String::new();
        let mut chars = body[start..].chars();
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
        found = Some(out);
        from = start;
    }
    found
}
