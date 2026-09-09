//! The `/debug` request log (chat/reqlog.go): a 30-entry ring of the most recent provider
//! requests with their bodies capped at 256 KiB, filled by `Client::send_payload` while recording
//! is ON (`/debug on`) and browsed by the `/debug` inspector. Go's log never echoes to stderr
//! (the cmd/root.go comment is stale — T3 policy); "verbose" = record + transcript no-collapse +
//! the status row's `debug` segment.
//!
//! Go records inside an `http.RoundTripper` installed on the provider's `*http.Client`
//! (reqlog.go:77-137). Rust records at the ONE execute point instead (`Client::send_payload` and
//! `Client::fetch_bare`), which is what keeps MCP traffic out of the ring: the run's
//! `reqwest::Client` is shared with the MCP transports, the recorder is not (T3 policy;
//! `T3_DESIGN` §4.4). Headers are recorded by neither language — method, URL, bodies, the status
//! line, the error text, the timestamp and the duration are the whole entry.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;

/// The ring holds this many entries, newest first (reqlog.go:27).
pub const REQ_LOG_MAX_ENTRIES: usize = 30;
/// Request and response bodies are cut at this many bytes (reqlog.go:28).
pub const REQ_LOG_MAX_BODY: usize = 256 * 1024;

/// One recorded request: the immutable request half plus the response half, which the transport
/// fills in as the round-trip proceeds (reqlog.go:31-45).
pub struct RequestEntry {
    /// When the request was captured.
    pub time: jiff::Zoned,
    /// The HTTP method.
    pub method: String,
    /// The absolute URL.
    pub url: String,
    /// The request body, capped at [`REQ_LOG_MAX_BODY`].
    pub req_body: Vec<u8>,
    /// Pre-computed `last_user_text` of `req_body` with whitespace collapsed — the `/debug`
    /// rows read this, never the body (`T3_DESIGN` §10 item 7: the list refreshes every 500 ms
    /// and would otherwise re-parse up to 30 × 256 KiB of JSON per tick).
    pub summary: String,
    resp: Mutex<ResponseHalf>,
}

/// The response half of an entry (reqlog.go:38-44), snapshotted by [`RequestEntry::response`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResponseHalf {
    /// `"200 OK"`-style status line; empty while pending.
    pub status: String,
    /// The transport error text; empty on success.
    pub err: Option<String>,
    /// The response body, capped at [`REQ_LOG_MAX_BODY`].
    pub resp_body: Vec<u8>,
    /// Round-trip duration; zero while pending.
    pub duration: Duration,
}

impl RequestEntry {
    /// A pending entry stamped now; the body is copied through `cap_bytes` and its summary
    /// computed once, from the capped copy exactly as Go's `summaryCol` reads `e.ReqBody`.
    pub fn new(method: &str, url: &str, req_body: &[u8]) -> Self {
        let req_body = cap_bytes(req_body);
        let summary = collapse_whitespace(&last_user_text(&req_body));
        Self {
            time: jiff::Zoned::now(),
            method: method.to_owned(),
            url: url.to_owned(),
            req_body,
            summary,
            resp: Mutex::new(ResponseHalf::default()),
        }
    }

    /// A snapshot of the response half.
    pub fn response(&self) -> ResponseHalf {
        self.resp
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records the status line.
    pub(crate) fn set_status(&self, s: String) {
        self.resp
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .status = s;
    }

    /// Records a transport failure and the duration.
    pub(crate) fn set_err(&self, e: String, d: Duration) {
        let mut r = self.resp.lock().unwrap_or_else(PoisonError::into_inner);
        r.err = Some(e);
        r.duration = d;
    }

    /// Appends response bytes, capped at [`REQ_LOG_MAX_BODY`].
    pub(crate) fn append_body(&self, chunk: &[u8]) {
        let mut r = self.resp.lock().unwrap_or_else(PoisonError::into_inner);
        let room = REQ_LOG_MAX_BODY.saturating_sub(r.resp_body.len());
        r.resp_body
            .extend_from_slice(&chunk[..chunk.len().min(room)]);
    }

    /// Records the duration.
    pub(crate) fn set_duration(&self, d: Duration) {
        self.resp
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .duration = d;
    }
}

/// Test seam (feature `testing`): an entry whose response half is already filled, so a suite can
/// stock the ring with finished round-trips without driving a transport. The recording setters
/// stay `pub(crate)` — only `Client::send_payload` and `RecordingStream` may move a LIVE entry.
#[cfg(feature = "testing")]
impl RequestEntry {
    /// A finished entry: `status` is the status line (`"200 OK"`), `err` the transport failure
    /// text (empty on success), `resp_body` the captured response and `duration` its round trip.
    pub fn completed(method: &str, url: &str, req_body: &[u8], response: ResponseHalf) -> Self {
        let e = Self::new(method, url, req_body);
        *e.resp.lock().unwrap_or_else(PoisonError::into_inner) = response;
        e
    }
}

impl std::fmt::Debug for RequestEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestEntry")
            .field("time", &self.time)
            .field("method", &self.method)
            .field("url", &self.url)
            .field("req_body_len", &self.req_body.len())
            .field("summary", &self.summary)
            .field("resp", &self.response())
            .finish()
    }
}

/// The ring (reqlog.go:47-104): newest first, capped at [`REQ_LOG_MAX_ENTRIES`], recording OFF
/// until `set_verbose(true)`.
pub struct RequestLog {
    entries: Mutex<VecDeque<Arc<RequestEntry>>>,
    verbose: AtomicBool,
}

impl Default for RequestLog {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestLog {
    /// An empty log with recording OFF.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(REQ_LOG_MAX_ENTRIES)),
            verbose: AtomicBool::new(false),
        }
    }

    /// Whether recording is on.
    pub fn verbose(&self) -> bool {
        self.verbose.load(Ordering::Relaxed)
    }

    /// Switches recording on or off. Switching it off leaves the captured entries in place —
    /// they stay browsable, only new round-trips stop being captured (reqlog.go:49-51).
    pub fn set_verbose(&self, v: bool) {
        self.verbose.store(v, Ordering::Relaxed);
    }

    /// The entries, NEWEST first. The `Arc`s are shared with the transport, so a row rendered
    /// from this snapshot sees the status, body and duration fill in as the round-trip proceeds
    /// (Go returns the same pointers).
    pub fn entries(&self) -> Vec<Arc<RequestEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Prepends `e`, evicting the oldest past [`REQ_LOG_MAX_ENTRIES`].
    pub fn add(&self, e: Arc<RequestEntry>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.push_front(e);
        entries.truncate(REQ_LOG_MAX_ENTRIES);
    }
}

/// A copy of `b` cut at [`REQ_LOG_MAX_BODY`] (reqlog.go:158-166).
pub(crate) fn cap_bytes(b: &[u8]) -> Vec<u8> {
    b[..b.len().min(REQ_LOG_MAX_BODY)].to_vec()
}

/// Go `strings.Join(strings.Fields(s), " ")` (debug.go:38).
pub(crate) fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The most recent user-authored text inside a request body, across the provider shapes
/// (debug.go:127-181): OpenAI/Anthropic `messages[]`, Responses `input`, Gemini
/// `contents[].parts[]`, the `OpenAI` Images `prompt` and Imagen's `instances[].prompt`. `""` for
/// a bodyless, non-JSON or unrecognised request (a model listing, say).
///
/// It lives here rather than in `repl::commands::debug` (which re-exports it under the frozen
/// name) because [`RequestEntry::new`] pre-computes the summary and `llm` may not reach up into
/// `repl` — DEVIATIONS3 `[WP66]`.
pub(crate) fn last_user_text(body: &[u8]) -> String {
    if body.is_empty() {
        return String::new();
    }
    let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };

    // 1. `messages[]`, scanned from the END: a set role that is not `user` skips the entry (a
    //    missing or non-string role counts as user, exactly as Go's zero-value type assertion).
    if let Some(msgs) = m.get("messages").and_then(Value::as_array) {
        for item in msgs.iter().rev() {
            let Some(mm) = item.as_object() else { continue };
            let role = mm.get("role").and_then(Value::as_str).unwrap_or_default();
            if !role.is_empty() && role != "user" {
                continue;
            }
            let text = content_text(mm.get("content").unwrap_or(&Value::Null));
            if !text.is_empty() {
                return text;
            }
        }
    }
    // 2/3. Responses `input`: a bare string is the whole prompt, an array is scanned from the end.
    match m.get("input") {
        Some(Value::String(s)) => return s.clone(),
        Some(Value::Array(arr)) => {
            for item in arr.iter().rev() {
                if let Some(mm) = item.as_object() {
                    let text = content_text(mm.get("content").unwrap_or(&Value::Null));
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
        }
        _ => {}
    }
    // 4. Gemini `contents[].parts[]`.
    if let Some(contents) = m.get("contents").and_then(Value::as_array) {
        for item in contents.iter().rev() {
            if let Some(mm) = item.as_object() {
                let text = content_text(mm.get("parts").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    // 5/6. The image dialects: OpenAI Images' top-level `prompt`, Imagen's `instances[]` envelope.
    if let Some(Value::String(s)) = m.get("prompt") {
        return s.clone();
    }
    if let Some(instances) = m.get("instances").and_then(Value::as_array) {
        for item in instances.iter().rev() {
            if let Some(Value::String(s)) = item.as_object().and_then(|mm| mm.get("prompt"))
                && !s.is_empty()
            {
                return s.clone();
            }
        }
    }
    String::new()
}

/// The text of a message `content` (or Gemini `parts`): a plain string, or the first array part
/// carrying a non-empty `text` string (debug.go:185-199).
pub(crate) fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            for p in parts {
                if let Some(Value::String(t)) = p.as_object().and_then(|pm| pm.get("text"))
                    && !t.is_empty()
                {
                    return t.clone();
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// The streaming tee over a response body (reqlog.go:139-175): every chunk is appended (capped)
/// to the entry, the duration is set at the end or on error, and `Drop` is the `Close` backstop
/// for consumers that stop before the end of the stream (an SSE reader at its terminal event).
pub(crate) struct RecordingStream<S> {
    inner: S,
    entry: Arc<RequestEntry>,
    start: Instant,
    done: bool,
}

impl<S> futures::Stream for RecordingStream<S>
where
    S: futures::Stream<Item = reqwest::Result<Bytes>> + Unpin,
{
    type Item = reqwest::Result<Bytes>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.entry.append_body(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            // Go's `recordingBody.Read` stamps the duration on ANY error, EOF included.
            Poll::Ready(other) => {
                this.done = true;
                this.entry.set_duration(this.start.elapsed());
                Poll::Ready(other)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for RecordingStream<S> {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            self.entry.set_duration(self.start.elapsed());
        }
    }
}

/// Records `resp`'s status on `entry` and rebuilds it with its body routed through a
/// [`RecordingStream`] onto the same entry (`http::Response` → `reqwest::Response`,
/// reqwest-0.13.4 response.rs:459).
///
/// The rebuild loses exactly two things reqwest carries beside the HTTP response: `url()`
/// (which becomes the placeholder `http://no.url.provided.local/`) and the response extensions.
/// Nothing in this crate reads either — status, version, headers and the body stream all
/// survive — and `rebuilt_response_keeps_everything_but_the_url` pins that (DIVERGENCES T-45).
pub(crate) fn record_response(
    entry: Arc<RequestEntry>,
    resp: reqwest::Response,
    start: Instant,
) -> reqwest::Response {
    entry.set_status(status_line(resp.status()));
    let (status, version) = (resp.status(), resp.version());
    let headers = resp.headers().clone();
    let body = reqwest::Body::wrap_stream(RecordingStream {
        inner: resp.bytes_stream(),
        entry,
        start,
        done: false,
    });
    // `Response::new` + the part setters instead of `Response::builder()`: the builder's
    // `body()` returns a `Result` that only a rejected status/version could ever populate, and
    // both came out of the response being rebuilt — this way there is no infallible error to
    // swallow.
    let mut rebuilt = http::Response::new(body);
    *rebuilt.status_mut() = status;
    *rebuilt.version_mut() = version;
    *rebuilt.headers_mut() = headers;
    reqwest::Response::from(rebuilt)
}

/// `"{code} {canonical_reason}"`, or `"{code}"` when the code has no reason.
///
/// Go keeps the server's status LINE verbatim (`resp.Status`), which reqwest does not expose:
/// only the code and its canonical reason survive, so a relay sending a custom reason phrase
/// renders its canonical one here. Rows show the code alone, so the difference is visible only
/// in `↓ Response` (DIVERGENCES T-44).
pub(crate) fn status_line(s: reqwest::StatusCode) -> String {
    match s.canonical_reason() {
        Some(reason) => format!("{} {reason}", s.as_u16()),
        None => s.as_u16().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REQ_LOG_MAX_BODY, REQ_LOG_MAX_ENTRIES, RequestEntry, RequestLog, cap_bytes,
        collapse_whitespace, content_text, last_user_text, record_response, status_line,
    };
    use bytes::Bytes;
    use futures::StreamExt as _;
    use std::sync::Arc;
    use std::time::Instant;

    /// A `reqwest::Response` over `chunks`, delivered one chunk per poll — the multi-frame body a
    /// wiremock server cannot be made to produce.
    fn chunked_response(chunks: &[&'static [u8]]) -> reqwest::Response {
        let items: Vec<Result<Bytes, std::io::Error>> =
            chunks.iter().map(|c| Ok(Bytes::from_static(c))).collect();
        let body = reqwest::Body::wrap_stream(futures::stream::iter(items));
        let mut resp = http::Response::new(body);
        *resp.status_mut() = reqwest::StatusCode::CREATED;
        resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        reqwest::Response::from(resp)
    }

    /// Go: `chat/reqlog_test.go:80` `TestRequestLogRingNewestFirst` — the ring caps at 30 and
    /// `entries()` leads with the newest.
    #[test]
    fn ring_keeps_the_newest_thirty() {
        let log = RequestLog::new();
        for i in 0..REQ_LOG_MAX_ENTRIES + 5 {
            log.add(Arc::new(RequestEntry::new("GET", &format!("/r/{i}"), b"")));
        }
        let entries = log.entries();
        assert_eq!(entries.len(), REQ_LOG_MAX_ENTRIES);
        assert_eq!(entries[0].url, "/r/34");
        assert_eq!(entries[entries.len() - 1].url, "/r/5");
    }

    /// Go: `chat/reqlog_test.go:102` `TestRequestLogVerboseToggle` — the switch the `/debug`
    /// Verbose tab flips, off by default, and switching it off keeps the captured entries.
    #[test]
    fn verbose_defaults_off_and_toggles() {
        let log = RequestLog::new();
        assert!(!log.verbose());
        log.set_verbose(true);
        assert!(log.verbose());
        log.add(Arc::new(RequestEntry::new("GET", "/x", b"")));
        log.set_verbose(false);
        assert!(!log.verbose());
        assert_eq!(
            log.entries().len(),
            1,
            "turning recording off keeps the ring"
        );
    }

    /// Go: `chat/reqlog_test.go:118` `TestCapBytes` — a small body is untouched, an oversized one
    /// is cut to exactly the cap, on both halves of an entry.
    #[test]
    fn cap_bytes_cuts_at_the_body_cap() {
        assert_eq!(cap_bytes(b"hi"), b"hi");
        let big = vec![7u8; REQ_LOG_MAX_BODY + 1000];
        assert_eq!(cap_bytes(&big).len(), REQ_LOG_MAX_BODY);
        let e = RequestEntry::new("GET", "/x", &big);
        assert_eq!(e.req_body.len(), REQ_LOG_MAX_BODY);
        e.append_body(&big);
        e.append_body(b"more");
        assert_eq!(e.response().resp_body.len(), REQ_LOG_MAX_BODY);
    }

    /// The summary is computed ONCE, at capture, with whitespace collapsed — the rows never
    /// re-parse the body (`T3_DESIGN` §10 item 7).
    #[test]
    fn the_summary_is_precomputed_and_whitespace_collapsed() {
        let e = RequestEntry::new(
            "POST",
            "https://api.anthropic.com/v1/messages",
            br#"{"messages":[{"role":"user","content":"a  long\n\tprompt"}]}"#,
        );
        assert_eq!(e.summary, "a long prompt");
        assert_eq!(collapse_whitespace("  a \t b\n"), "a b");
        // A model listing carries no user text at all.
        assert_eq!(RequestEntry::new("GET", "/v1/models", b"").summary, "");
    }

    /// Go: `chat/debug_test.go:37` `TestLastUserText` — the most recent user message across every
    /// provider body shape, and `""` for a bodyless or unrecognised request.
    ///
    /// The function lives here rather than in `repl::commands::debug` because
    /// [`RequestEntry::new`] pre-computes each entry's summary from it (DEVIATIONS3 `[WP66]`).
    #[test]
    fn last_user_text_digs_every_dialect() {
        for (name, body, want) in [
            (
                "anthropic",
                r#"{"messages":[{"role":"user","content":"你好"}]}"#,
                "你好",
            ),
            (
                "openai last user",
                r#"{"messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"}]}"#,
                "hello",
            ),
            (
                "content parts",
                r#"{"messages":[{"role":"user","content":[{"type":"text","text":"pic"}]}]}"#,
                "pic",
            ),
            (
                "gemini",
                r#"{"contents":[{"role":"user","parts":[{"text":"explain"}]}]}"#,
                "explain",
            ),
            ("responses input", r#"{"input":"do it"}"#, "do it"),
            (
                "images prompt",
                r#"{"model":"gpt-image-1","prompt":"a red fox"}"#,
                "a red fox",
            ),
            (
                "imagen instances",
                r#"{"instances":[{"prompt":"a blue whale"}],"parameters":{"sampleCount":1}}"#,
                "a blue whale",
            ),
            (
                "last user wins",
                r#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"a"},{"role":"user","content":"second"}]}"#,
                "second",
            ),
            ("no body", "", ""),
            ("model listing", "{}", ""),
        ] {
            assert_eq!(last_user_text(body.as_bytes()), want, "{name}");
        }
    }

    /// The shapes Go's table does not spell out but its code pins: a missing role counts as user,
    /// a Responses `input` ARRAY is scanned from the end, and a non-object body is `""`.
    #[test]
    fn last_user_text_edge_shapes() {
        assert_eq!(
            last_user_text(br#"{"messages":[{"content":"roleless"}]}"#),
            "roleless"
        );
        assert_eq!(
            last_user_text(br#"{"input":[{"content":"a"},{"content":"b"}]}"#),
            "b"
        );
        // An assistant-only conversation falls through to the later shapes and ends empty.
        assert_eq!(
            last_user_text(br#"{"messages":[{"role":"assistant","content":"hi"}]}"#),
            ""
        );
        // A truncated (capped) body is no longer valid JSON: Go returns "" and so do we.
        assert_eq!(last_user_text(br#"{"messages":[{"role":"user","#), "");
        assert_eq!(last_user_text(b"[1,2]"), "", "a non-object body");
        assert_eq!(last_user_text(b"not json"), "");
        // `contentText`: a string wins, an array yields the first non-empty `text` part.
        assert_eq!(content_text(&serde_json::json!("plain")), "plain");
        assert_eq!(
            content_text(&serde_json::json!([{"type":"image"},{"text":""},{"text":"t"}])),
            "t"
        );
        assert_eq!(content_text(&serde_json::json!({"text":"nope"})), "");
        assert_eq!(content_text(&serde_json::Value::Null), "");
    }

    /// The tee fills the entry chunk by chunk and stamps the duration at the end of the stream.
    #[tokio::test]
    async fn the_tee_captures_each_chunk_as_it_is_read() {
        let entry = Arc::new(RequestEntry::new("POST", "/v1/messages", b""));
        let rebuilt = record_response(
            Arc::clone(&entry),
            chunked_response(&[b"data: one\n\n", b"data: two\n\n"]),
            Instant::now(),
        );
        assert_eq!(
            entry.response().status,
            "201 Created",
            "status lands at once"
        );
        assert!(entry.response().resp_body.is_empty(), "nothing read yet");

        let mut stream = rebuilt.bytes_stream();
        let first = stream.next().await.expect("chunk").expect("ok");
        assert_eq!(&first[..], b"data: one\n\n");
        assert_eq!(entry.response().resp_body, b"data: one\n\n");
        assert!(entry.response().duration.is_zero());

        let second = stream.next().await.expect("chunk").expect("ok");
        assert_eq!(&second[..], b"data: two\n\n");
        assert!(stream.next().await.is_none(), "end of body");
        assert_eq!(entry.response().resp_body, b"data: one\n\ndata: two\n\n");
        assert!(!entry.response().duration.is_zero(), "stamped at EOF");
    }

    /// `Drop` backstops the duration for a consumer that stops before EOF — an SSE reader that
    /// stops at its terminal event (reqlog.go:165-175 `recordingBody.Close`).
    #[tokio::test]
    async fn dropping_the_stream_early_still_stamps_the_duration() {
        let entry = Arc::new(RequestEntry::new("POST", "/v1/messages", b""));
        let rebuilt = record_response(
            Arc::clone(&entry),
            chunked_response(&[b"first", b"never read"]),
            Instant::now(),
        );
        let mut stream = rebuilt.bytes_stream();
        stream.next().await.expect("chunk").expect("ok");
        assert!(entry.response().duration.is_zero());
        drop(stream);
        assert!(!entry.response().duration.is_zero(), "Drop is the backstop");
        assert_eq!(
            entry.response().resp_body,
            b"first",
            "only what was actually read is captured"
        );
    }

    /// What the `http::Response` → `reqwest::Response` rebuild costs (`T3_DESIGN` §4.4, R1;
    /// DIVERGENCES T-45): status, version, headers and the body all survive; `url()` becomes
    /// reqwest's placeholder and the response extensions are dropped. Nothing in this crate reads
    /// either — `Sse`, `resp.bytes()` and `consume_images` take the body, `StatusError` is built
    /// from the request's own URL — so the loss is invisible, and this test is what would notice
    /// if a future caller started depending on it.
    #[tokio::test]
    async fn rebuilt_response_keeps_everything_but_the_url() {
        let entry = Arc::new(RequestEntry::new(
            "GET",
            "https://api.example/v1/models",
            b"",
        ));
        let original = chunked_response(&[b"body"]);
        assert_eq!(
            original.url().as_str(),
            "http://no.url.provided.local/",
            "a hand-built response has no url either — the placeholder is reqwest's"
        );
        let rebuilt = record_response(Arc::clone(&entry), original, Instant::now());
        assert_eq!(rebuilt.status(), reqwest::StatusCode::CREATED);
        assert_eq!(rebuilt.version(), http::Version::HTTP_11);
        assert_eq!(
            rebuilt
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            rebuilt.url().as_str(),
            "http://no.url.provided.local/",
            "the rebuild cannot carry the URL forward (reqwest response.rs:459)"
        );
        assert_eq!(&rebuilt.bytes().await.expect("body")[..], b"body");
    }

    /// The status line reqwest can reconstruct: code plus canonical reason, code alone when the
    /// registry has no reason for it.
    #[test]
    fn status_line_shapes() {
        assert_eq!(status_line(reqwest::StatusCode::OK), "200 OK");
        assert_eq!(status_line(reqwest::StatusCode::CREATED), "201 Created");
        let unknown = reqwest::StatusCode::from_u16(599);
        assert_eq!(
            unknown.map(status_line).ok(),
            Some("599".to_owned()),
            "an unregistered code renders bare"
        );
    }
}
