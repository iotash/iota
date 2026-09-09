//! HTTP client (internal/llm/client.go): one provider endpoint with static headers, an optional per-attempt auth
//! hook, the byte-for-byte retry policy, the uniform response-header timeout (POLICY I-02) and `StatusError`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::BoxError;
use crate::llm::json::Raw;
use crate::text::truncate_to_char_boundary;
use bytes::Bytes;
use futures::StreamExt;
use rand::Rng;
use reqwest::{
    Method, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::progress::{self, ProgressBody};
use super::reqlog::{RequestEntry, RequestLog, record_response};
use super::{error::LlmError, sse::Sse};

/// Extra attempts after the first (client.go).
pub(crate) const DEFAULT_RETRIES: u32 = 2;
/// Uniform response-header timeout on EVERY client (POLICY I-02); never a whole-request timeout.
pub(crate) const HEADER_TIMEOUT: Duration = Duration::from_secs(120);
/// Backoff base: `min(500ms << attempt, 8s) - jitter(d/4)`.
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);
/// Backoff cap.
pub const BACKOFF_CAP: Duration = Duration::from_secs(8);
/// At most this many bytes of an error body are read.
pub(crate) const ERROR_BODY_READ_CAP: usize = 1 << 20;
/// The error detail is cut at this many bytes (then `…` appended).
pub const ERROR_DETAIL_CAP: usize = 2048;

/// Uniform jitter source for the retry backoff (injectable so tests pin delays).
pub trait Jitter: Send + Sync {
    /// A duration in `0..=max`.
    fn jitter(&self, max: Duration) -> Duration;
}

/// `rand::rng().random_range(0..=max)`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RandJitter;

impl Jitter for RandJitter {
    fn jitter(&self, max: Duration) -> Duration {
        let max_ns = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(rand::rng().random_range(0..=max_ns))
    }
}

/// Always 0 (tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoJitter;

impl Jitter for NoJitter {
    fn jitter(&self, _max: Duration) -> Duration {
        Duration::ZERO
    }
}

/// Per-attempt request mutation (authorisation); a failure is `LlmError::Authorize` and is never retried.
pub(crate) type AuthHook = Arc<dyn Fn(&mut reqwest::Request) -> Result<(), BoxError> + Send + Sync>;

/// One provider endpoint: base URL (all trailing '/' trimmed), static headers, optional per-attempt auth hook, retry policy.
#[derive(Clone)]
pub struct Client {
    base_url: String,
    http: reqwest::Client,
    headers: Vec<(HeaderName, HeaderValue)>,
    auth: Option<AuthHook>,
    retries: u32,
    jitter: Arc<dyn Jitter>,
    header_timeout: Duration,
    /// The `/debug` request log; `None` = nothing is ever recorded (every test client, MCP).
    recorder: Option<Arc<RequestLog>>,
}

/// One request body (T3 design §4.4): the JSON encoding every dialect sends, or the multipart
/// form of `/images/edits` with its boundary-carrying content type.
#[derive(Clone, Debug)]
pub(crate) enum Payload {
    /// `application/json`.
    Json(Bytes),
    /// `multipart/form-data; boundary=…`.
    Multipart {
        /// The full `Content-Type` value.
        content_type: String,
        /// The encoded form.
        body: Bytes,
    },
}

impl Payload {
    /// The body bytes.
    pub(crate) fn bytes(&self) -> &Bytes {
        match self {
            Self::Json(b) | Self::Multipart { body: b, .. } => b,
        }
    }

    /// The `Content-Type` header value.
    pub(crate) fn content_type(&self) -> HeaderValue {
        match self {
            Self::Json(_) => HeaderValue::from_static("application/json"),
            Self::Multipart { content_type, .. } => HeaderValue::from_str(content_type)
                .unwrap_or_else(|_| HeaderValue::from_static("multipart/form-data")),
        }
    }
}

// `headers` renders names AND values: credential values are installed via `common::credential_header`,
// which marks them sensitive, so they print as `Sensitive`, never the key bytes (security-4).
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("headers", &self.headers)
            .field("auth", &self.auth.is_some())
            .field("retries", &self.retries)
            .field("header_timeout", &self.header_timeout)
            .field("recorder", &self.recorder.is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `retries = DEFAULT_RETRIES`, `jitter = RandJitter`, `header_timeout = HEADER_TIMEOUT`; trailing `/` trimmed from `base_url`.
    pub fn new(base_url: &str, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
            headers: Vec::new(),
            auth: None,
            retries: DEFAULT_RETRIES,
            jitter: Arc::new(RandJitter),
            header_timeout: HEADER_TIMEOUT,
            recorder: None,
        }
    }

    /// Installs the `/debug` request log: every attempt is captured before its round-trip and
    /// its response recorded while `log.verbose()` (cmd/root.go:128 `reqLog.HTTPClient()`).
    /// Without one — every test client, and the MCP transports' bare `reqwest::Client` — the
    /// seam is inert and the response is never rebuilt.
    #[must_use]
    pub fn with_recorder(mut self, log: Arc<RequestLog>) -> Self {
        self.recorder = Some(log);
        self
    }

    /// The installed `/debug` request log, if any.
    pub fn recorder(&self) -> Option<&Arc<RequestLog>> {
        self.recorder.as_ref()
    }

    /// Appended to every request (`Header.Add`).
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    /// Number of extra attempts after the first.
    #[must_use]
    pub fn with_retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }

    /// Per-attempt auth hook.
    #[must_use]
    pub fn with_auth(mut self, hook: AuthHook) -> Self {
        self.auth = Some(hook);
        self
    }

    /// Jitter source for the backoff.
    #[must_use]
    pub fn with_jitter(mut self, j: Arc<dyn Jitter>) -> Self {
        self.jitter = j;
        self
    }

    /// Response-header timeout (default `HEADER_TIMEOUT`); a test seam like `Jitter` so `header_timeout_is_transport_error` pins ~200 ms, not 2 min.
    #[must_use]
    pub fn with_header_timeout(mut self, d: Duration) -> Self {
        self.header_timeout = d;
        self
    }

    /// The base URL (trailing slashes trimmed).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The underlying reqwest client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// JSON request, JSON response (2xx body decoded; non-JSON 2xx ⇒ `LlmError::Decode`).
    pub async fn do_json<B: serde::Serialize + Sync, T: serde::de::DeserializeOwned>(
        &self,
        cancel: &CancellationToken,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, LlmError> {
        let payload = encode(body)?;
        let resp = self.send(cancel, method, path, payload).await?;
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(LlmError::Cancelled),
            r = resp.bytes() => r.map_err(LlmError::Transport)?,
        };
        serde_json::from_slice(&bytes).map_err(LlmError::Decode)
    }

    /// GET without body.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        cancel: &CancellationToken,
        path: &str,
    ) -> Result<T, LlmError> {
        self.do_json::<(), T>(cancel, Method::GET, path, None).await
    }

    /// JSON request whose 2xx body is an SSE stream. Retries only until the first successful response head.
    pub async fn stream<B: serde::Serialize + Sync>(
        &self,
        cancel: &CancellationToken,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Sse, LlmError> {
        let payload = encode(body)?;
        let resp = self.send(cancel, method, path, payload).await?;
        Ok(Sse::new(resp.bytes_stream(), cancel.clone()))
    }

    /// The retry loop over a JSON body (client.go:114-168); returns the successful (status < 400)
    /// response. `= send_payload(Payload::Json)`.
    pub async fn send(
        &self,
        cancel: &CancellationToken,
        method: Method,
        path: &str,
        payload: Option<Bytes>,
    ) -> Result<reqwest::Response, LlmError> {
        self.send_payload(cancel, method, path, payload.map(Payload::Json))
            .await
    }

    /// The ONE execute path (T3 design §4.4): per attempt, capture → the upload-progress body
    /// wrap when a turn reporter is in scope → execute under the header timeout / cancel select
    /// → `sent()` on EVERY arm → record. Retries per `should_retry`/`retry_delay`, and because
    /// the loop sits ABOVE the capture, EVERY attempt is its own `/debug` entry (Go's retry loop
    /// sits above its recording transport the same way, client.go:105-113).
    pub(crate) async fn send_payload(
        &self,
        cancel: &CancellationToken,
        method: Method,
        path: &str,
        payload: Option<Payload>,
    ) -> Result<reqwest::Response, LlmError> {
        let url = format!("{}{path}", self.base_url);
        let mut attempt: u32 = 0;
        loop {
            let progress = progress::current();
            // Step 1 — capture BEFORE the round-trip, so a pending row (status `…`) appears in
            // the inspector the moment the request goes out (reqlog.go:95-104).
            let entry = self.capture(
                method.as_str(),
                &url,
                payload.as_ref().map_or(&[][..], |p| p.bytes()),
            );
            // Step 2 — the progress body wraps the payload AFTER the capture: what the capture
            // reads in-process must never count as uploaded bytes (reqlog.go:106-112).
            let req = self.build_request(&method, &url, payload.clone(), progress.as_ref())?;

            let start = Instant::now();
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    if let Some(tp) = &progress {
                        tp.sent();
                    }
                    // Go's `RoundTrip` returns the context error and records it; the row must
                    // not be left pending forever.
                    if let Some(e) = &entry {
                        e.set_err(LlmError::Cancelled.to_string(), start.elapsed());
                    }
                    return Err(LlmError::Cancelled);
                }
                r = tokio::time::timeout(self.header_timeout, self.http.execute(req)) => match r {
                    Ok(Ok(resp)) => Ok(resp),
                    Ok(Err(e)) => Err(LlmError::Transport(e)),
                    Err(_) => Err(LlmError::HeaderTimeout(self.header_timeout)),
                },
            };
            // Go fires `sent` whenever `RoundTrip` returns, whatever it returned.
            if let Some(tp) = &progress {
                tp.sent();
            }
            // Step 4 — record: the status line now, the body as the caller streams it.
            let outcome = record_attempt(entry, outcome, start);

            let (final_err, headers) = match outcome {
                Ok(resp) if resp.status().as_u16() < 400 => return Ok(resp),
                Ok(resp) => {
                    let headers = resp.headers().clone();
                    let status = resp.status();
                    let body = read_error_body(resp, cancel).await;
                    (
                        LlmError::Status(StatusError::new(status, method.as_str(), &url, &body)),
                        Some(headers),
                    )
                }
                Err(e) => (e, None),
            };

            if attempt >= self.retries
                || cancel.is_cancelled()
                || !should_retry(&final_err, headers.as_ref())
            {
                return Err(final_err);
            }
            let delay = retry_delay(headers.as_ref(), attempt, self.jitter.as_ref());
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(LlmError::Cancelled),
                () = tokio::time::sleep(delay) => {}
            }
            attempt += 1;
        }
    }

    /// Absolute-URL GET with NO static headers and NO auth hook, one attempt, the header timeout
    /// — the relay image fetch of the image dialects (images.go:192-207). It goes through the
    /// recording client in Go, so it is recorded here too: its row's action is the URL's last
    /// path segment (or `Request`), never `Chat`/`Image`.
    pub(crate) async fn fetch_bare(
        &self,
        cancel: &CancellationToken,
        url: &str,
    ) -> Result<reqwest::Response, LlmError> {
        let entry = self.capture(Method::GET.as_str(), url, &[]);
        let req = self.http.get(url).build().map_err(LlmError::Transport)?;
        let start = Instant::now();
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(LlmError::Cancelled),
            r = tokio::time::timeout(self.header_timeout, self.http.execute(req)) => match r {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(LlmError::Transport(e)),
                Err(_) => Err(LlmError::HeaderTimeout(self.header_timeout)),
            },
        };
        record_attempt(entry, outcome, start)
    }

    /// Step 1 of the seam: a pending entry in the ring, or `None` when there is no recorder or
    /// recording is off (reqlog.go:94-104) — the OFF path allocates nothing and rebuilds nothing.
    fn capture(&self, method: &str, url: &str, body: &[u8]) -> Option<Arc<RequestEntry>> {
        let log = self.recorder.as_ref().filter(|l| l.verbose())?;
        let entry = Arc::new(RequestEntry::new(method, url, body));
        log.add(Arc::clone(&entry));
        Some(entry)
    }

    /// One attempt's request: the payload's `Content-Type` only with a body, every static header
    /// appended, then the auth hook (its failure is `Authorize`, never retried). Under a turn
    /// reporter the body streams through a [`ProgressBody`] with an EXPLICIT `Content-Length`
    /// (hyper honours it over the stream's missing size hint — T3 design §4.4 step 2).
    fn build_request(
        &self,
        method: &Method,
        url: &str,
        payload: Option<Payload>,
        progress: Option<&Arc<progress::TurnProgress>>,
    ) -> Result<reqwest::Request, LlmError> {
        let mut builder = self.http.request(method.clone(), url);
        if let Some(payload) = payload {
            let content_type = payload.content_type();
            let bytes = payload.bytes().clone();
            builder = builder.header(CONTENT_TYPE, content_type);
            builder =
                match progress {
                    Some(tp) if !bytes.is_empty() => {
                        builder.header(CONTENT_LENGTH, bytes.len()).body(
                            reqwest::Body::wrap_stream(ProgressBody::new(bytes, Arc::clone(tp))),
                        )
                    }
                    _ => builder.body(bytes),
                };
        }
        let mut req = builder.build().map_err(LlmError::Transport)?;
        for (name, value) in &self.headers {
            req.headers_mut().append(name.clone(), value.clone());
        }
        if let Some(auth) = &self.auth {
            auth(&mut req).map_err(LlmError::Authorize)?;
        }
        Ok(req)
    }
}

/// Encodes the request body once (`llm: encode request: …`).
fn encode<B: serde::Serialize>(body: Option<&B>) -> Result<Option<Bytes>, LlmError> {
    body.map(|b| {
        serde_json::to_vec(b)
            .map(Bytes::from)
            .map_err(LlmError::Encode)
    })
    .transpose()
}

/// Step 4 of the recording seam (reqlog.go:117-136): a captured attempt gets the transport error
/// text and its duration, or its status line plus a response whose body tees into the entry as
/// the caller reads it. Without a captured entry the outcome passes through untouched — no
/// rebuild, so every non-recording client (every test, MCP) keeps reqwest's own response.
fn record_attempt(
    entry: Option<Arc<RequestEntry>>,
    outcome: Result<reqwest::Response, LlmError>,
    start: Instant,
) -> Result<reqwest::Response, LlmError> {
    let Some(entry) = entry else { return outcome };
    match outcome {
        Ok(resp) => Ok(record_response(entry, resp, start)),
        Err(e) => {
            entry.set_err(e.to_string(), start.elapsed());
            Err(e)
        }
    }
}

/// Reads at most `ERROR_BODY_READ_CAP` bytes of a failed response's body; read errors and cancellation keep
/// whatever arrived (Go: `io.ReadAll(io.LimitReader(..))` with the error ignored).
async fn read_error_body(resp: reqwest::Response, cancel: &CancellationToken) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut body = resp.bytes_stream();
    while raw.len() < ERROR_BODY_READ_CAP {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            c = body.next() => c,
        };
        match chunk {
            Some(Ok(bytes)) => {
                let room = ERROR_BODY_READ_CAP - raw.len();
                raw.extend_from_slice(&bytes[..bytes.len().min(room)]);
            }
            Some(Err(_)) | None => break,
        }
    }
    raw
}

/// client.go:171-186. A transport-level failure (including the header timeout) retries; a status failure retries
/// when `x-should-retry` says `true`, never when it says `false`, else on 408/409/429 or any 5xx.
pub fn should_retry(err: &LlmError, headers: Option<&HeaderMap>) -> bool {
    match err {
        LlmError::Status(se) => {
            match headers
                .and_then(|h| h.get("x-should-retry"))
                .and_then(|v| v.to_str().ok())
            {
                Some("true") => return true,
                Some("false") => return false,
                _ => {}
            }
            matches!(se.status, 408 | 409 | 429) || se.status >= 500
        }
        LlmError::Transport(_) | LlmError::HeaderTimeout(_) => true,
        _ => false,
    }
}

/// client.go:188-212. `retry-after-ms` (integer > 0) → ms; `retry-after` (float seconds > 0) → seconds;
/// `retry-after` (HTTP-date with positive remaining time) → that; else `min(500ms << attempt, 8s) - jitter(d/4)`.
/// `headers` is `None` for transport failures (always backoff).
pub fn retry_delay(headers: Option<&HeaderMap>, attempt: u32, jitter: &dyn Jitter) -> Duration {
    if let Some(h) = headers {
        if let Some(ms) = header_str(h, "retry-after-ms")
            && let Ok(n) = ms.parse::<i64>()
            && n > 0
        {
            return Duration::from_millis(n.unsigned_abs());
        }
        if let Some(ra) = header_str(h, "retry-after") {
            if let Ok(secs) = ra.parse::<f64>()
                && secs > 0.0
                && let Ok(d) = Duration::try_from_secs_f64(secs)
            {
                return d;
            }
            if let Ok(t) = httpdate::parse_http_date(ra)
                && let Ok(d) = t.duration_since(std::time::SystemTime::now())
                && d > Duration::ZERO
            {
                return d;
            }
        }
    }
    let d = if attempt >= 4 {
        BACKOFF_CAP
    } else {
        (BACKOFF_BASE * (1u32 << attempt)).min(BACKOFF_CAP)
    };
    // Uniform jitter: sleep in [0.75d, d].
    d.saturating_sub(jitter.jitter(d / 4))
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// reqwest client: rustls, HTTP/2 allowed, system proxy env honoured, NO whole-request timeout, NO read timeout.
#[allow(clippy::expect_used)] // TLS backend init failure is unrecoverable here
pub fn default_http_client() -> reqwest::Client {
    // The builder with no options set is exactly `Client::new()`; `build()` only fails when the TLS backend
    // cannot initialise, and `Client::new()` PANICS on that same failure — so there is no fallback to
    // offer, only an honest message instead of reqwest's internal one.
    reqwest::Client::builder()
        .build()
        .expect("TLS backend failed to initialize")
}

/// A non-2xx/3xx response (client.go:32-47): `POST "http://…": 400 Bad Request {"message":…}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{method} {url:?}: {status} {status_text} {body}")]
pub struct StatusError {
    /// HTTP status code.
    pub status: u16,
    /// `StatusCode::canonical_reason()` or `""`.
    pub status_text: String,
    /// Request method.
    pub method: String,
    /// Request URL.
    pub url: String,
    /// Error detail: the trimmed body, or the raw text of a non-null top-level `error` value; cut at
    /// `ERROR_DETAIL_CAP` bytes plus `…`.
    pub body: String,
}

impl StatusError {
    /// client.go:217-237 over an already-read (≤ 1 MiB) body.
    pub fn new(status: StatusCode, method: &str, url: &str, raw: &[u8]) -> Self {
        Self {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_owned(),
            method: method.to_owned(),
            url: url.to_owned(),
            body: error_detail(raw),
        }
    }
}

/// The top-level `error` JSON value of an error body when present and not `null`, else the trimmed body; cut
/// at `ERROR_DETAIL_CAP` bytes (`…` appended).
fn error_detail(raw: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        error: Option<Raw>,
    }
    let text = String::from_utf8_lossy(raw);
    let mut detail = text.trim().to_owned();
    if let Ok(Envelope { error: Some(e) }) = serde_json::from_str::<Envelope>(&text) {
        e.get().clone_into(&mut detail);
    }
    if detail.len() > ERROR_DETAIL_CAP {
        let mut cut = truncate_to_char_boundary(&detail, ERROR_DETAIL_CAP).to_owned();
        cut.push('…');
        detail = cut;
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_detail_prefers_the_envelope_and_treats_null_as_absent() {
        assert_eq!(
            error_detail(br#"{"error":{"message":"bad"}}"#),
            r#"{"message":"bad"}"#
        );
        assert_eq!(error_detail(br#" {"error":"x"} "#), r#""x""#);
        assert_eq!(error_detail(br#"{"error":null}"#), r#"{"error":null}"#);
        assert_eq!(error_detail(b"  plain text \n"), "plain text");
        assert_eq!(error_detail(b"[1,2]"), "[1,2]");
        let long = "a".repeat(ERROR_DETAIL_CAP + 10);
        let cut = error_detail(long.as_bytes());
        assert_eq!(cut.len(), ERROR_DETAIL_CAP + "…".len());
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn status_error_display_matches_go() {
        let se = StatusError::new(
            StatusCode::BAD_REQUEST,
            "POST",
            "http://h/x",
            br#"{"error":{"message":"bad","type":"invalid_request_error"}}"#,
        );
        assert_eq!(
            se.to_string(),
            r#"POST "http://h/x": 400 Bad Request {"message":"bad","type":"invalid_request_error"}"#
        );
        let unknown =
            StatusError::new(StatusCode::from_u16(599).unwrap(), "GET", "http://h/y", b"");
        assert_eq!(unknown.to_string(), r#"GET "http://h/y": 599  "#);
    }
}
