//! `describe_error` — the byte-exact headline/hint table (chat/errors.go:34-79). A wire
//! failure keeps its type all the way up (`ProviderError::Wire`), so classification is a
//! match on `LlmError`, not a walk of the source chain.

use crate::headless::ChatError;
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

    /// Names the layer the ceiling came from in a truncation's hint (see [`truncated_hint`]); any
    /// other report, and a cut whose request carried no ceiling, is returned as it was.
    #[must_use]
    pub fn naming_ceiling(mut self, e: &ChatError, layer: CeilingLayer) -> Self {
        if let ChatError::Truncated(t) = e
            && t.limit.is_some()
        {
            self.hint = truncated_hint(Some(layer));
        }
        self
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
        ChatError::Truncated(t) => ErrorReport {
            headline: "Output truncated".to_owned(),
            detail: vec![t.to_string()],
            // A ceiling the request carried is the user's knob; without one the endpoint's own limit cut it.
            hint: if t.limit.is_some() {
                truncated_hint(None)
            } else {
                TRUNCATED_UNSENT_HINT.to_owned()
            },
        },
        other => ErrorReport::request_failed(other),
    }
}

/// Which layer set the ceiling a cut hit: the session's `max_tokens` source, with a config one split by
/// whether the `agents:` entry declares it (it overrides the model's, so editing the model's would not help).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeilingLayer {
    /// The `agents:` entry's `max_tokens:`.
    Agent,
    /// The current model's `models:` entry.
    Model,
    /// A value the session carries as its own (`ParamSource::User`), not the config's.
    Session,
    /// Nothing set one: the dialect's default.
    Builtin,
}

impl CeilingLayer {
    /// The layer behind a ceiling tagged `source`; `agent_declares` says whether the `agents:` entry sets
    /// `max_tokens:`.
    pub fn of(source: crate::session::ParamSource, agent_declares: bool) -> Self {
        use crate::session::ParamSource;
        match source {
            ParamSource::Config if agent_declares => Self::Agent,
            ParamSource::Config => Self::Model,
            ParamSource::User => Self::Session,
            ParamSource::Builtin => Self::Builtin,
        }
    }
}

/// What a user can do about an output the ceiling cut off — raise the ceiling where it is actually set, or
/// think less — and how an edit reaches a running session: the config is read at startup, a resume restores
/// the ceiling the session recorded, and only a model switch re-evaluates it (brain `model-param-layering`).
/// `None` (no layer known) names both config layers in their precedence order.
pub(crate) fn truncated_hint(layer: Option<CeilingLayer>) -> String {
    let head = match layer {
        Some(CeilingLayer::Agent) => {
            "This ceiling is max_tokens: on the agent's `agents:` entry, which overrides the model's — raise it there (thinking counts toward it), or lower the effort in /model."
        }
        Some(CeilingLayer::Model) => {
            "This ceiling is max_tokens: on this model's `models:` entry — raise it there (thinking counts toward it), or lower the effort in /model."
        }
        Some(CeilingLayer::Session) => {
            "This ceiling is the one this session recorded, not the config's — set max_tokens: on the agent's `agents:` or this model's `models:` entry (thinking counts toward it), or lower the effort in /model."
        }
        Some(CeilingLayer::Builtin) => {
            "This ceiling is the built-in default — set max_tokens: on this model's `models:` entry or the agent's `agents:` entry (thinking counts toward it), or lower the effort in /model."
        }
        None => {
            "Raise max_tokens: on the agent's `agents:` entry when it sets one, else on this model's `models:` entry (thinking counts toward it), or lower the effort in /model."
        }
    };
    format!("{head} {TRUNCATED_APPLY}")
}

/// The second sentence of every ceiling hint: how a config edit reaches this session.
const TRUNCATED_APPLY: &str = "A config edit is read at startup: a new session takes it, while this one — resumed or not — keeps its ceiling until /model switches its model.";

/// The same cut where the request carried no ceiling (the responses dialect, X-72): the endpoint's own limit
/// cannot be raised from here, so the hint names only what can be lowered.
const TRUNCATED_UNSENT_HINT: &str = "The endpoint's own output limit cut it: lower the model's effort in /model, or ask for a shorter reply";

/// What a user can do about a stalled stream. It names no setting on purpose: the idle bound is a
/// constant (`client::STREAM_IDLE_TIMEOUT`), not a knob (decided 2026-10-01).
const STALL_HINT: &str = "Send the message again, or check the provider's base URL and model (/model); ESC stops a silent wait sooner";

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
        LlmError::StreamIdle(_) => ErrorReport {
            headline: "Response stalled".to_owned(),
            detail: vec![whole.to_string()],
            hint: STALL_HINT.to_owned(),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! `describe_error` classification (formerly `tests/repl/errors.rs`, reached through a hidden `pub use`
    //! re-export; moved in-file 2026-09-15).

    use super::{ErrorReport, describe_error};
    use crate::headless::ChatError;
    use crate::llm::{LlmError, StatusError};
    use crate::provider::error::{ProviderError, WireOp};
    use pretty_assertions::assert_eq;

    fn status_err(status: u16, body: &str) -> LlmError {
        LlmError::Status(StatusError {
            status,
            status_text: "Status Text".to_owned(),
            method: "POST".to_owned(),
            url: "https://api.example.com/v1/x".to_owned(),
            body: body.to_owned(),
        })
    }

    /// A wire failure as the chat layer sees it.
    fn wire(e: LlmError) -> ChatError {
        ChatError::Provider(ProviderError::wire(WireOp::Stream, e))
    }

    /// Byte-exact headlines/details/hints; wire errors get a status-class headline plus the envelope's
    /// message, never the raw URL/JSON dump.
    #[test]
    fn describe_error_classifies_each_failure_into_headline_detail_and_hint() {
        struct Case {
            name: &'static str,
            err: ChatError,
            headline: &'static str,
            detail: String,
            hint: &'static str,
        }
        let cases = [
            Case {
                name: "rate limit with openai envelope",
                err: wire(status_err(
                    429,
                    r#"{"message":"Rate limit reached for gpt-4o","type":"tokens","code":"rate_limit_exceeded"}"#,
                )),
                headline: "Rate limited (429)",
                detail: "Rate limit reached for gpt-4o".to_owned(),
                hint: "",
            },
            Case {
                name: "auth failure hints at the key",
                err: wire(status_err(
                    401,
                    r#"{"message":"Incorrect API key provided"}"#,
                )),
                headline: "Authentication failed (401)",
                detail: "Incorrect API key provided".to_owned(),
                hint: "Check the API key for this provider",
            },
            Case {
                name: "context overflow reroutes to /compact",
                err: wire(status_err(
                    400,
                    r#"{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}"#,
                )),
                headline: "Context window exceeded (400)",
                detail: "This model's maximum context length is 8192 tokens".to_owned(),
                hint: "Try /compact to shrink the conversation",
            },
            Case {
                name: "anthropic prompt-too-long phrasing",
                err: wire(status_err(
                    400,
                    r#"{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}"#,
                )),
                headline: "Context window exceeded (400)",
                detail: "prompt is too long: 210000 tokens > 200000 maximum".to_owned(),
                hint: "Try /compact to shrink the conversation",
            },
            Case {
                name: "non-JSON body falls back verbatim",
                err: wire(status_err(502, "upstream connect error")),
                headline: "Provider server error (502)",
                detail: "upstream connect error".to_owned(),
                hint: "",
            },
            Case {
                name: "bare string error value",
                err: wire(status_err(400, r#""invalid request""#)),
                headline: "Request rejected (400 Status Text)",
                detail: "invalid request".to_owned(),
                hint: "",
            },
            Case {
                name: "nested envelope from a proxy",
                err: wire(status_err(
                    404,
                    r#"{"error":{"message":"model x does not exist"}}"#,
                )),
                headline: "Not found (404)",
                detail: "model x does not exist".to_owned(),
                hint: "Check the model name (/model) and base URL",
            },
            Case {
                name: "permanent failure keeps its text",
                err: ChatError::Provider(ProviderError::permanent_msg("quota")),
                headline: "Request failed",
                detail: "quota".to_owned(),
                hint: "",
            },
            Case {
                name: "no SSE events",
                err: wire(LlmError::NoEvents),
                headline: "Provider did not stream",
                detail: format!("stream error: {}", LlmError::NoEvents),
                hint: "",
            },
            Case {
                name: "a stall says what to do and names no knob",
                err: wire(LlmError::StreamIdle(std::time::Duration::from_secs(300))),
                headline: "Response stalled",
                detail: "stream error: no data from the provider for 5m0s (stream idle timeout); the request was not retried".to_owned(),
                hint: "Send the message again, or check the provider's base URL and model (/model); ESC stops a silent wait sooner",
            },
            Case {
                name: "a truncated output names the ceiling and the knob",
                err: ChatError::Truncated(crate::provider::Truncation {
                    reason: "stop_reason: max_tokens".to_owned(),
                    limit: Some(32_000),
                    dropped_tools: Vec::new(),
                }),
                headline: "Output truncated",
                detail: "stop_reason: max_tokens: the output hit its 32000-token limit".to_owned(),
                hint: "Raise max_tokens: on the agent's `agents:` entry when it sets one, else on this model's `models:` entry (thinking counts toward it), or lower the effort in /model. A config edit is read at startup: a new session takes it, while this one — resumed or not — keeps its ceiling until /model switches its model.",
            },
            Case {
                name: "a cut with no ceiling sent names only what can be lowered",
                err: ChatError::Truncated(crate::provider::Truncation {
                    reason: "response.incomplete: max_output_tokens".to_owned(),
                    limit: None,
                    dropped_tools: Vec::new(),
                }),
                headline: "Output truncated",
                detail: "response.incomplete: max_output_tokens: the output hit its token limit"
                    .to_owned(),
                hint: "The endpoint's own output limit cut it: lower the model's effort in /model, or ask for a shorter reply",
            },
            Case {
                name: "plain error",
                err: ChatError::Io(std::io::Error::other("tool rounds exceeded")),
                headline: "Request failed",
                detail: "tool rounds exceeded".to_owned(),
                hint: "",
            },
        ];
        for c in cases {
            let r = describe_error(&c.err);
            assert_eq!(r.headline, c.headline, "{}: headline", c.name);
            assert_eq!(r.detail.join("\n"), c.detail, "{}: detail", c.name);
            assert_eq!(r.hint, c.hint, "{}: hint", c.name);
        }
    }

    /// F6: a truncation's hint names the layer its ceiling actually came from — an `agents:` value
    /// overrides the model's, so pointing at the model entry would send the user to a key that changes
    /// nothing — and every variant says how an edit reaches the running (or resumed) session. Any other
    /// report, and a cut with no ceiling sent, keeps its hint.
    #[test]
    fn a_truncation_hint_names_the_layer_that_set_the_ceiling() {
        use super::CeilingLayer;
        use crate::session::ParamSource;
        let cut = |limit| {
            ChatError::Truncated(crate::provider::Truncation {
                reason: "stop_reason: max_tokens".to_owned(),
                limit,
                dropped_tools: Vec::new(),
            })
        };
        let cases = [
            (
                ParamSource::Config,
                true,
                CeilingLayer::Agent,
                "on the agent's `agents:` entry, which overrides the model's",
            ),
            (
                ParamSource::Config,
                false,
                CeilingLayer::Model,
                "on this model's `models:` entry — raise it there",
            ),
            (
                ParamSource::User,
                true,
                CeilingLayer::Session,
                "the one this session recorded, not the config's",
            ),
            (
                ParamSource::Builtin,
                false,
                CeilingLayer::Builtin,
                "the built-in default",
            ),
        ];
        for (source, agent, layer, names) in cases {
            assert_eq!(CeilingLayer::of(source, agent), layer, "{source:?}/{agent}");
            let e = cut(Some(32_000));
            let hint = describe_error(&e).naming_ceiling(&e, layer).hint;
            assert!(hint.contains(names), "{layer:?}: {hint}");
            assert!(
                hint.ends_with("keeps its ceiling until /model switches its model."),
                "{layer:?}: {hint}"
            );
        }
        let unsent = cut(None);
        assert_eq!(
            describe_error(&unsent)
                .naming_ceiling(&unsent, CeilingLayer::Agent)
                .hint,
            describe_error(&unsent).hint,
            "no ceiling was sent: nothing to name"
        );
        let other = ChatError::Io(std::io::Error::other("x"));
        assert_eq!(
            describe_error(&other).naming_ceiling(&other, CeilingLayer::Agent),
            describe_error(&other)
        );
    }

    /// `lines()` appends the hint after the detail rows.
    #[test]
    fn the_report_lines_put_the_hint_after_the_detail_rows() {
        let mut r = ErrorReport {
            headline: String::new(),
            detail: vec!["a".to_owned(), "b".to_owned()],
            hint: "h".to_owned(),
        };
        assert_eq!(r.lines().join("|"), "a|b|h");
        r.hint = String::new();
        assert_eq!(r.lines().join("|"), "a|b");
    }
}
