//! `describe_error` — the byte-exact headline/hint table (chat/errors.go:34-79). A wire
//! failure keeps its type all the way up (`ProviderError::Wire`), so classification is a
//! match on `LlmError`, not a walk of the source chain.

use crate::chat::ChatError;
use crate::llm::{LlmError, StatusError};
use crate::provider::error::ProviderError;
use serde_json::Value;

/// One classified error for the transcript (chat/errors.go `errorReport`): a short
/// classification headline, the provider's human-readable message (or the raw error text
/// as a fallback — no information is ever dropped), and an optional actionable hint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorReport {
    /// The red `✗` headline.
    pub headline: String,
    /// Detail rows (wrapped at width−5, floor 20, by the errorBlock).
    pub detail: Vec<String>,
    /// The dim hint row (`""` = none).
    pub hint: String,
}

impl ErrorReport {
    /// The generic row: `Request failed` over the error's own text, no hint.
    pub fn request_failed(e: &dyn std::fmt::Display) -> Self {
        Self {
            headline: "Request failed".to_owned(),
            detail: vec![e.to_string()],
            hint: String::new(),
        }
    }

    /// Flattens the report into the errorBlock detail rows — hint last (errors.go:23-28).
    pub fn lines(&self) -> Vec<String> {
        let mut out = self.detail.clone();
        if !self.hint.is_empty() {
            out.push(self.hint.clone());
        }
        out
    }
}

/// Byte-exact classification table (errors.go:34-79). Wire errors (`LlmError::Status`)
/// get a status-class headline plus their envelope's message field; a missing SSE stream
/// and transport failures get their own headlines; everything else keeps its error text
/// as detail under a generic headline.
pub fn describe_error(e: &ChatError) -> ErrorReport {
    match e {
        ChatError::Provider(ProviderError::Wire { source, .. }) => describe_llm(source, e),
        other => ErrorReport::request_failed(other),
    }
}

/// The wire rows of the table; `whole` is the full error text the detail row shows.
fn describe_llm(llm: &LlmError, whole: &dyn std::fmt::Display) -> ErrorReport {
    match llm {
        LlmError::Status(se) => describe_status(se),
        LlmError::NoEvents => ErrorReport {
            headline: "Provider did not stream".to_owned(),
            detail: vec![whole.to_string()],
            hint: String::new(),
        },
        LlmError::Transport(_) | LlmError::HeaderTimeout(_) => ErrorReport {
            headline: "Network error".to_owned(),
            detail: vec![whole.to_string()],
            hint: String::new(),
        },
        _ => ErrorReport::request_failed(whole),
    }
}

/// The status table (errors.go:51-79).
fn describe_status(se: &StatusError) -> ErrorReport {
    let detail = envelope_message(&se.body).unwrap_or_else(|| se.body.clone());
    let (headline, hint) = match se.status {
        401 | 403 => (
            format!("Authentication failed ({})", se.status),
            "Check the API key for this provider",
        ),
        402 => ("Billing issue (402)".to_owned(), ""),
        404 => (
            "Not found (404)".to_owned(),
            "Check the model name (/model) and base URL",
        ),
        408 => ("Request timed out (408)".to_owned(), ""),
        413 => ("Request too large (413)".to_owned(), ""),
        429 => ("Rate limited (429)".to_owned(), ""),
        s if s >= 500 => (format!("Provider server error ({s})"), ""),
        s => (format!("Request rejected ({s} {})", se.status_text), ""),
    };
    let mut r = ErrorReport {
        headline,
        detail: non_empty_lines(&detail),
        hint: hint.to_owned(),
    };
    if context_overflow(&detail) {
        r.headline = format!("Context window exceeded ({})", se.status);
        "Try /compact to shrink the conversation".clone_into(&mut r.hint);
    }
    r
}

/// Extracts the human-readable message from a provider error envelope
/// (errors.go:87-110): the dialect shapes all carry `"message"`; a bare JSON string (an
/// `"error"` value that wasn't an object) and one extra `{"error":{"message":…}}` nesting
/// (proxies passing the whole envelope through) are also accepted. Non-JSON → `None`
/// (the raw body passes verbatim).
fn envelope_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    match v {
        Value::String(s) => (!s.is_empty()).then_some(s),
        Value::Object(m) => {
            if let Some(Value::String(msg)) = m.get("message")
                && !msg.is_empty()
            {
                return Some(msg.clone());
            }
            if let Some(Value::Object(e)) = m.get("error")
                && let Some(Value::String(msg)) = e.get("message")
                && !msg.is_empty()
            {
                return Some(msg.clone());
            }
            None
        }
        _ => None,
    }
}

/// Whether a provider message describes the request exceeding the model's context window
/// — the one 4xx with a specific in-chat remedy, /compact (errors.go:115-129).
fn context_overflow(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    [
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "too many tokens",
        "input token count",
        "prompt is too long",
    ]
    .iter()
    .any(|pat| lower.contains(pat))
}

/// Splits `s` into trimmed-right rows, dropping blank ones (errors.go:132-138).
fn non_empty_lines(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    s.split('\n')
        .map(|ln| ln.trim_end_matches('\r'))
        .filter(|ln| !ln.trim().is_empty())
        .map(str::to_owned)
        .collect()
}
