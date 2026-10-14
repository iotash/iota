//! The L4 mock provider: a dependency-free chat-completions (and Anthropic Messages) server on loopback.
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
//! | `stream N [MS]` | `N` lines `l#00 line` … , 60 ms (or `MS`) apart (the interruptible one) |
//! | `think`       | ~1.5 s of reasoning deltas, then a small markdown document   |
//! | `md`          | that markdown document, streamed in 7-byte chunks            |
//! | `exact`       | `EXACTSTART`, a line of exactly 80 `x`, `EXACTEND`            |
//! | `straddle`    | `WIDESTART`, `y` + 40 × `中` (81 columns), `WIDEEND`          |
//! | `blocks`      | [`blocks_doc`]: a 40-line document with a code block, a table and a list, one line per delta |
//! | `emoji`       | [`EMOJI`]: a table whose cells carry emoji, flag sequences and VS16 characters |
//! | `run:<cmd>`   | ONE `shell` tool call, `{"command": <cmd>, "background": true}`, `finish_reason: tool_calls` |
//! | `runfg:<cmd>` | the same call in the foreground (no `background` key)          |
//! | `runtail:<cmd>` | the foreground call, then [`TRAILING`] as content AFTER it — a model that keeps talking past its call |
//! | `runanon:<cmd>` | the same, but the arguments arrive BEFORE the call's name (an anonymous composing delta) |
//! | `hang:<word>` | the `200` head, then not one byte until the client walks away   |
//! | `openend:<word>` | the full `echo:` answer through `[DONE]`, then the body held open (no last chunk) |
//! | anything else | `echo: <the message>`                                        |
//!
//! Two directives ride ANY message rather than selecting a script:
//!
//! * `fail:<status>:<n>` anywhere in the conversation — the first `n` streaming requests carrying
//!   that message get `<status>` with a JSON error envelope, the rest stream normally. The counter
//!   is keyed by the whole message text, so two scenarios never share one (`FAILS`).
//! * `title:<word>` in the session-title pass (the unary request embeds the first user message):
//!   the pass answers `<word>` alone, so a scenario can choose the window title it then reads back.
//! * `hang:` in the session-title pass: the pass gets no answer at all — not even a head — so it
//!   sits on `TITLE_TIMEOUT` like a title request to a provider that stopped answering.
//! * `slowtitle:` hangs the session-title pass the same way and ONLY it: the turn itself streams its
//!   ordinary `echo:` answer at once.
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
//! | `POST /v1/messages`            | the Anthropic Messages dialect below (a `type: anthropic` provider) |
//! | `GET  /v1/messages/log`        | every `/v1/messages` body received so far, one per line   |
//! | anything else (`POST`)         | the chat-completions dialect above                        |
//!
//! Both image frames carry the checked-in 2×2 PNG (`tests/fixtures/images/rb-2x2.png`),
//! base64-encoded here rather than pulled in as a dependency — the partial frame is held
//! for [`PARTIAL_GAP`] so the generation widget is observable before the picture lands.
//!
//! **The Anthropic dialect (F3).** `POST /v1/messages` answers in typed SSE events (`message_start`,
//! `content_block_start`/`_delta`/`_stop` per block, `message_delta` with the `stop_reason`,
//! `message_stop`), the script picked by the text of the request's LAST message — the user's turn,
//! or the `tool_result` that closes a call:
//!
//! | last message    | reply                                                                     |
//! |-----------------|---------------------------------------------------------------------------|
//! | `thinkcut`      | a thinking block (30 `halfthought #` deltas, [`THINK_GAP`] apart, then its `signature_delta`), `stop_reason: max_tokens` — NO text block |
//! | `toolcut:<cmd>` | a `shell` `tool_use` whose ONE `input_json_delta` is `{"command":"<cmd>` (string and object left open), held [`COMPOSE_HOLD`], the block stopped, `stop_reason: max_tokens` |
//! | `run:<cmd>`     | the same call, whole (`{"command":<cmd>}`), `stop_reason: tool_use` — the control that DOES run |
//! | a `tool_result` | `ran: <the result's first line>`, `end_turn`                              |
//! | anything else   | a sealed `considering #` thought, then `echo: <the message>`, `end_turn`  |
//!
//! A request that advertises no `tools` is the session-title pass (the client streams it too,
//! X-71): it gets the text `Mock title`, whatever its embedded message asks for. Every body is
//! kept, in order, and `GET /v1/messages/log` hands them back — the only way a scenario can see
//! what the client SENT after a cut (the half-thought, the dropped call, the rolled-back message).

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

/// How long the `toolcut:` script holds its half-composed call before the cut: long enough that a
/// scenario can see the composing widget `[shell …]` up, so its absence afterwards means something.
const COMPOSE_HOLD: Duration = Duration::from_millis(1500);

/// Gap between the partial image frame and the finished picture: long enough that a
/// scenario can observe the generation widget, short enough not to pad the suite.
const PARTIAL_GAP: Duration = Duration::from_millis(600);

/// How long `GET /slow/models` holds its answer: long enough for a scenario to press ESC while
/// `Fetching available models…` is up, short enough that the abandoned fetch is over before the
/// scenario's next step could be confused by it.
const SLOW_LIST: Duration = Duration::from_millis(2000);

/// The image both image frames carry — the checked-in 2×2 fixture the L1 imgterm tests use.
const PNG_2X2: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");

/// The text `runtail:`/`runanon:` stream after their call (docs/design/busy-span.md §2.7).
const TRAILING: &str = "TRAILTEXT after the call\n";

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
        if path.ends_with("/v1/messages/log") {
            return messages_log(&mut sock);
        }
        if path.contains("/slow/") {
            thread::sleep(SLOW_LIST);
        }
        return models(&mut sock);
    }
    if path.ends_with("/images/generations") || path.ends_with("/images/edits") {
        return images_reply(&mut sock);
    }
    if path.ends_with("/v1/messages") {
        return anthropic_reply(&mut sock, &body);
    }
    let prompt = last_content(&body).unwrap_or_default();
    if prompt.contains("hang:") {
        return hang(&mut sock, &mut reader, body.contains("\"stream\":true"));
    }
    if prompt.contains("slowtitle:") && !body.contains("\"stream\":true") {
        return hang(&mut sock, &mut reader, false);
    }
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

/// `hang:` — a provider that stopped answering: a streaming request gets its `200` head (the
/// connection is up, the model is silent), the unary title pass not even that. Either way the
/// connection is held until the client drops it (ESC, the idle bound, the title timeout).
fn hang(
    sock: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    head: bool,
) -> std::io::Result<()> {
    if head {
        write!(
            sock,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )?;
        sock.flush()?;
    }
    let mut buf = [0u8; 1024];
    while reader.read(&mut buf)? > 0 {}
    Ok(())
}

/// `fail:<status>:<n>` — the status this request must answer with: that of the FIRST directive
/// in the conversation whose budget is not yet spent, or none.
///
/// Every USER message of the request is read, not only the last: a retried round re-issues the
/// same conversation, a steer message queued during the backoff lands behind the message that
/// carries the directive, and a later turn of the same session still carries the earlier, spent
/// one — and the model's own echo of the directive is not a directive.
fn fail_now(body: &str) -> Option<u16> {
    let mut fails = FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for c in user_contents(body) {
        let Some(at) = c.find("fail:") else { continue };
        let mut parts = c[at + "fail:".len()..].splitn(3, ':');
        let Some(status) = parts.next().and_then(|s| s.parse::<u16>().ok()) else {
            continue;
        };
        let digits: String = parts
            .next()
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let Ok(n) = digits.parse::<u32>() else {
            continue;
        };
        let seen = fails.entry(c).or_insert(0);
        if *seen < n {
            *seen += 1;
            return Some(status);
        }
    }
    None
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
    } else if let Some(cmd) = prompt.strip_prefix("runtail:") {
        tool_call(sock, cmd.trim(), false)?;
        content(sock, TRAILING)?;
        finish = "tool_calls";
    } else if let Some(cmd) = prompt.strip_prefix("runanon:") {
        anonymous_tool_call(sock, cmd.trim())?;
        content(sock, TRAILING)?;
        finish = "tool_calls";
    } else if prompt.starts_with("blocks") {
        for line in blocks_doc().lines() {
            content(sock, &format!("{line}\n"))?;
            thread::sleep(LINE_GAP);
        }
    } else if prompt.starts_with("emoji") {
        content(sock, EMOJI)?;
    } else if let Some(rest) = prompt.strip_prefix("stream") {
        let mut args = rest.split_whitespace();
        let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(10);
        let gap = args
            .next()
            .and_then(|a| a.parse().ok())
            .map_or(LINE_GAP, Duration::from_millis);
        for i in 0..n {
            content(sock, &format!("l#{i:02} line\n"))?;
            thread::sleep(gap);
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
    if prompt.starts_with("openend") {
        // The answer is complete, the body is not: a relay that never sends the last chunk.
        sock.flush()?;
        let mut buf = [0u8; 1024];
        while sock.read(&mut buf)? > 0 {}
        return Ok(());
    }
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

/// One foreground `shell` call whose argument object arrives BEFORE its name: the first delta carries the id
/// and the whole arguments with no name, the second names the call — the composing observer sees an anonymous
/// delta and nothing after it (the zombie-spinner rule raises no composing widget for it).
fn anonymous_tool_call(sock: &mut TcpStream, cmd: &str) -> std::io::Result<()> {
    let id = format!("call_l4_{}", CALLS.fetch_add(1, Ordering::Relaxed) + 1);
    let args = format!("{{\"command\":{}}}", json_string(cmd));
    chunk(
        sock,
        &format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"type\":\"function\",\"function\":{{\"arguments\":{}}}}}]}},\"finish_reason\":null}}]}}\n\n",
            json_string(&args)
        ),
    )?;
    chunk(
        sock,
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"shell\"}}]},\"finish_reason\":null}]}\n\n",
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
    string_values_after(body, "\"content\":\"")
}

/// The USER messages' texts only — what the scenario typed (and the job notices, which ride
/// the same role). The dialect writes `role` before `content`, so the pair is one substring;
/// an assistant reply that echoes a directive back must never trip it.
fn user_contents(body: &str) -> Vec<String> {
    string_values_after(body, "\"role\":\"user\",\"content\":\"")
}

/// Every JSON string that follows `key` in `body`, escapes undone, in order.
fn string_values_after(body: &str, key: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(at) = body[from..].find(key) {
        let start = from + at + key.len();
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

/// Every `POST /v1/messages` body the mock has served, in arrival order — what
/// `GET /v1/messages/log` hands back, so a scenario can read what the client SENT after a cut.
static MESSAGES_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// `GET /v1/messages/log` — every recorded Anthropic request body, one per line (the client writes
/// compact JSON, so a body never holds a raw newline).
fn messages_log(sock: &mut TcpStream) -> std::io::Result<()> {
    let body = {
        let log = MESSAGES_LOG
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut b = log.join("\n");
        if !b.is_empty() {
            b.push('\n');
        }
        b
    };
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    sock.flush()
}

/// The Anthropic Messages dialect (`POST /v1/messages`, `llm/anthropic.rs`): typed SSE events —
/// `message_start`, a `content_block_start`/`_delta`/`_stop` run per block, `message_delta` with the
/// `stop_reason` and `message_stop` — the script picked by the request's LAST message (see the
/// module table).
///
/// The client streams every request on this dialect, the session-title pass included (X-71); that
/// pass is the one request that advertises no `tools`, and it gets a plain title so a scenario's
/// keyword never truncates the title too.
fn anthropic_reply(sock: &mut TcpStream, body: &str) -> std::io::Result<()> {
    MESSAGES_LOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(body.to_owned());
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;
    event(
        sock,
        "message_start",
        r#"{"type":"message_start","message":{"id":"msg_l4","type":"message","role":"assistant","content":[],"usage":{"input_tokens":11,"output_tokens":1}}}"#,
    )?;

    let last = last_message(body);
    let texts = string_values_after(last, "\"text\":\"");
    let prompt = texts.last().cloned().unwrap_or_default();
    let stop = if !body.contains("\"tools\":[") {
        text_block(sock, 0, "Mock title")?;
        "end_turn"
    } else if last.contains("\"type\":\"tool_result\"") {
        let first = texts
            .first()
            .and_then(|t| t.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or("")
            .to_owned();
        text_block(sock, 0, &format!("ran: {first}"))?;
        "end_turn"
    } else if prompt.contains("thinkcut") {
        // Cut mid-THOUGHT: a sealed thinking block and nothing after it — no text block at all.
        thinking_block(sock, 0, "halfthought")?;
        "max_tokens"
    } else if let Some(at) = prompt.find("toolcut:") {
        // Cut mid-ARGUMENTS: the call is named, half its input arrives, the block closes, the
        // ceiling is the stop reason. The pause keeps the composing widget observable.
        let cmd = prompt[at + "toolcut:".len()..].trim();
        let id = format!("toolu_l4_{}", CALLS.fetch_add(1, Ordering::Relaxed) + 1);
        event(
            sock,
            "content_block_start",
            &format!(
                r#"{{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"shell","input":{{}}}}}}"#
            ),
        )?;
        // `{"command":"<cmd>` — the string never closes, the object never ends.
        let half = format!("{{\"command\":{}", json_string(cmd));
        let half = &half[..half.len() - 1];
        event(
            sock,
            "content_block_delta",
            &format!(
                r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
                json_string(half)
            ),
        )?;
        thread::sleep(COMPOSE_HOLD);
        block_stop(sock, 0)?;
        "max_tokens"
    } else if let Some(at) = prompt.find("run:") {
        // The control: the same call, whole, closed by `tool_use` — this one DOES run.
        let cmd = prompt[at + "run:".len()..].trim();
        let id = format!("toolu_l4_{}", CALLS.fetch_add(1, Ordering::Relaxed) + 1);
        event(
            sock,
            "content_block_start",
            &format!(
                r#"{{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"shell","input":{{}}}}}}"#
            ),
        )?;
        let args = format!("{{\"command\":{}}}", json_string(cmd));
        event(
            sock,
            "content_block_delta",
            &format!(
                r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
                json_string(&args)
            ),
        )?;
        block_stop(sock, 0)?;
        "tool_use"
    } else {
        // The ordinary turn: a short sealed thought, then the answer — a turn that ENDS.
        thinking_block(sock, 0, "considering")?;
        text_block(sock, 1, &format!("echo: {prompt}"))?;
        "end_turn"
    };

    event(
        sock,
        "message_delta",
        &format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"{stop}","stop_sequence":null}},"usage":{{"output_tokens":7}}}}"#
        ),
    )?;
    event(sock, "message_stop", r#"{"type":"message_stop"}"#)?;
    sock.write_all(b"0\r\n\r\n")?;
    sock.flush()
}

/// A thinking block at `index`: [`THINK_GAP`]-spaced `thinking_delta`s of `<word> #`, then the
/// `signature_delta` that seals it, then its stop.
fn thinking_block(sock: &mut TcpStream, index: u32, word: &str) -> std::io::Result<()> {
    event(
        sock,
        "content_block_start",
        &format!(
            r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"thinking","thinking":"","signature":""}}}}"#
        ),
    )?;
    for i in 0..30 {
        event(
            sock,
            "content_block_delta",
            &format!(
                r#"{{"type":"content_block_delta","index":{index},"delta":{{"type":"thinking_delta","thinking":{}}}}}"#,
                json_string(&format!("{word} {i} "))
            ),
        )?;
        thread::sleep(THINK_GAP);
    }
    event(
        sock,
        "content_block_delta",
        &format!(
            r#"{{"type":"content_block_delta","index":{index},"delta":{{"type":"signature_delta","signature":"c2lnLWw0"}}}}"#
        ),
    )?;
    block_stop(sock, index)
}

/// A text block at `index` carrying `text` in one `text_delta`.
fn text_block(sock: &mut TcpStream, index: u32, text: &str) -> std::io::Result<()> {
    event(
        sock,
        "content_block_start",
        &format!(
            r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"text","text":""}}}}"#
        ),
    )?;
    event(
        sock,
        "content_block_delta",
        &format!(
            r#"{{"type":"content_block_delta","index":{index},"delta":{{"type":"text_delta","text":{}}}}}"#,
            json_string(text)
        ),
    )?;
    block_stop(sock, index)
}

/// `content_block_stop` for `index`.
fn block_stop(sock: &mut TcpStream, index: u32) -> std::io::Result<()> {
    event(
        sock,
        "content_block_stop",
        &format!(r#"{{"type":"content_block_stop","index":{index}}}"#),
    )
}

/// One named SSE event (`event: <kind>` + `data: <json>`), as the Messages API frames them.
fn event(sock: &mut TcpStream, kind: &str, data: &str) -> std::io::Result<()> {
    chunk(sock, &format!("event: {kind}\ndata: {data}\n\n"))
}

/// The request's LAST message: from its `"role":"` up to where the message list ends (the
/// `system` and `tools` arrays follow `messages` in the body, and the system prompt has `text`
/// values of its own).
fn last_message(body: &str) -> &str {
    let from = body.rfind("{\"role\":\"").unwrap_or(0);
    let rest = &body[from..];
    let end = ["],\"system\":[", "],\"tools\":[", "],\"stream\":"]
        .iter()
        .filter_map(|k| rest.find(k))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}
