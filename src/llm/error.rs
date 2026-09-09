//! Wire error taxonomy (client.go:32-47 plus the per-dialect texts): `LlmError` and the responses `RespFailure`.

use std::time::Duration;

use crate::BoxError;
use crate::text::go_duration;

use super::client::StatusError;

/// Every failure the wire layer reports. Provider adapters map `Cancelled → ProviderError::Cancelled` and
/// everything else to `ProviderError::{chat, stream, list_models}` (text providers) or `ProviderError::other`
/// (image providers, Go returns those raw).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// A non-2xx/3xx response.
    #[error(transparent)]
    Status(StatusError),
    /// The SSE body ended without a single event or `[DONE]`.
    #[error("stream ended without any SSE events (server did not stream?)")]
    NoEvents,
    /// A non-null top-level `error` inside a stream payload; the raw JSON of the error value.
    #[error("received error while streaming: {0}")]
    InBand(String),
    /// A chat-completions / google chunk that is not valid JSON.
    #[error("llm: malformed stream chunk: {0}")]
    MalformedChunk(#[source] serde_json::Error),
    /// A responses / anthropic event that is not valid JSON.
    #[error("llm: malformed stream event: {0}")]
    MalformedEvent(#[source] serde_json::Error),
    /// An images response that is not valid JSON.
    #[error("llm: malformed images response: {0}")]
    MalformedImages(#[source] serde_json::Error),
    /// An images SSE stream that ended without a `.completed` frame.
    #[error("llm: image stream ended without a completed image")]
    ImageStreamIncomplete,
    /// A terminal responses event (`error`, `response.failed`, `response.incomplete`).
    #[error(transparent)]
    Failure(RespFailure),
    /// The request body could not be serialised.
    #[error("llm: encode request: {0}")]
    Encode(#[source] serde_json::Error),
    /// The auth hook failed (never retried).
    #[error("llm: authorize request: {0}")]
    Authorize(#[source] BoxError),
    /// A transport-level reqwest failure (retried).
    #[error("{0}")]
    Transport(#[source] reqwest::Error),
    /// The 2-minute response-header timeout (POLICY I-02) expired; retried like `Transport`.
    #[error("response headers not received within {}", go_duration(*.0))]
    HeaderTimeout(Duration),
    /// A 2xx body that is not the expected JSON.
    #[error("{0}")]
    Decode(#[source] serde_json::Error),
    /// A google model name containing `?`, `&` or `..`.
    #[error("llm: invalid model name {0:?}")]
    InvalidModelName(String),
    /// The cancellation token fired.
    #[error("interrupted")]
    Cancelled,
}

impl LlmError {
    /// google.go:235-242: Status 404|405, or Decode/Malformed* whose serde error `classify() == Category::Syntax`.
    pub fn is_list_fallback(&self) -> bool {
        match self {
            Self::Status(se) => se.status == 404 || se.status == 405,
            Self::Decode(e)
            | Self::MalformedChunk(e)
            | Self::MalformedEvent(e)
            | Self::MalformedImages(e) => e.classify() == serde_json::error::Category::Syntax,
            _ => false,
        }
    }

    /// The `StatusError` when this is a `Status`.
    pub fn status(&self) -> Option<&StatusError> {
        match self {
            Self::Status(se) => Some(se),
            _ => None,
        }
    }
}

/// responses.go:226-243. Display: detail = message; if message == "" detail = code; else if code != "" detail += " (" + code + ")"; if detail == "" detail = "no detail provided"; `{event}: {detail}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct RespFailure {
    /// The terminal event type (`error`, `response.failed`, `response.incomplete`).
    pub event: String,
    /// Error code (or the incomplete reason).
    pub code: String,
    /// Error message.
    pub message: String,
}

impl std::fmt::Display for RespFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match (self.message.as_str(), self.code.as_str()) {
            ("", "") => "no detail provided".to_owned(),
            ("", code) => code.to_owned(),
            (message, "") => message.to_owned(),
            (message, code) => format!("{message} ({code})"),
        };
        write!(f, "{}: {detail}", self.event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(event: &str, code: &str, message: &str) -> String {
        RespFailure {
            event: event.to_owned(),
            code: code.to_owned(),
            message: message.to_owned(),
        }
        .to_string()
    }

    #[test]
    fn resp_failure_display_matches_go() {
        assert_eq!(
            failure("response.failed", "rate_limit", "slow down"),
            "response.failed: slow down (rate_limit)"
        );
        assert_eq!(
            failure("response.incomplete", "max_output_tokens", ""),
            "response.incomplete: max_output_tokens"
        );
        assert_eq!(failure("error", "", "boom"), "error: boom");
        assert_eq!(failure("error", "", ""), "error: no detail provided");
    }

    #[test]
    fn is_list_fallback_table() {
        let status = |code: u16| {
            LlmError::Status(StatusError {
                status: code,
                status_text: String::new(),
                method: "GET".into(),
                url: "http://h".into(),
                body: String::new(),
            })
        };
        assert!(status(404).is_list_fallback());
        assert!(status(405).is_list_fallback());
        assert!(!status(500).is_list_fallback());
        assert!(!status(400).is_list_fallback());
        let syntax = serde_json::from_str::<serde_json::Value>("<!DOCTYPE html>").unwrap_err();
        assert!(LlmError::Decode(syntax).is_list_fallback());
        let syntax = serde_json::from_str::<serde_json::Value>("<html>").unwrap_err();
        assert!(LlmError::MalformedChunk(syntax).is_list_fallback());
        let eof = serde_json::from_str::<serde_json::Value>("").unwrap_err();
        assert!(!LlmError::Decode(eof).is_list_fallback());
        let data = serde_json::from_str::<u32>("\"x\"").unwrap_err();
        assert!(!LlmError::Decode(data).is_list_fallback());
        assert!(!LlmError::NoEvents.is_list_fallback());
        assert!(!LlmError::HeaderTimeout(Duration::from_secs(1)).is_list_fallback());
        assert!(status(404).status().is_some());
        assert!(LlmError::Cancelled.status().is_none());
    }

    #[test]
    fn display_texts_match_go() {
        assert_eq!(
            LlmError::NoEvents.to_string(),
            "stream ended without any SSE events (server did not stream?)"
        );
        assert_eq!(
            LlmError::InBand(r#"{"message":"boom"}"#.into()).to_string(),
            r#"received error while streaming: {"message":"boom"}"#
        );
        assert_eq!(
            LlmError::HeaderTimeout(Duration::from_secs(120)).to_string(),
            "response headers not received within 2m0s"
        );
        assert_eq!(
            LlmError::InvalidModelName("bad?x".into()).to_string(),
            r#"llm: invalid model name "bad?x""#
        );
        assert_eq!(LlmError::Cancelled.to_string(), "interrupted");
    }
}
