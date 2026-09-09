//! The OpenAI-shaped `GET /models` listing (internal/llm/chatcomp.go Models) shared by the chat-completions and
//! responses dialects.

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{client::Client, error::LlmError};

/// The models endpoint path.
pub(crate) const PATH_MODELS: &str = "/models";

/// `GET /models` body.
#[derive(Deserialize, Default)]
pub(crate) struct ModelsResponse {
    /// The listed models.
    #[serde(default)]
    pub(crate) data: Vec<ModelEntry>,
}

/// One listed model.
#[derive(Deserialize, Default)]
pub(crate) struct ModelEntry {
    /// Model id.
    #[serde(default)]
    pub(crate) id: String,
}

/// GET /models → ids sorted bytewise. Used verbatim by `ChatComp::models` and `Responses::models` (openai.go:59-70, openresponses.go models path).
pub async fn openai_model_ids(
    client: &Client,
    cancel: &CancellationToken,
) -> Result<Vec<String>, LlmError> {
    let out: ModelsResponse = client.get_json(cancel, PATH_MODELS).await?;
    let mut models: Vec<String> = out.data.into_iter().map(|m| m.id).collect();
    models.sort();
    Ok(models)
}

/// Go `url.QueryEscape` (net/url): `A-Z a-z 0-9 - _ . ~` pass through, a space becomes `+`, every other byte is
/// percent-encoded with upper-case hex. Both paginated listings (anthropic `after_id`, google `pageToken`) escape
/// their cursor with it.
pub(crate) fn query_escape(s: &str) -> String {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 0x0f)]));
            }
        }
    }
    out
}

#[cfg(test)]
mod query_escape_tests {
    use super::query_escape;

    // Go: net/url QueryEscape — the anthropic (google.go:256) and google pins, merged.
    #[test]
    fn query_escape_matches_go() {
        assert_eq!(query_escape("claude-a"), "claude-a");
        assert_eq!(query_escape("a.b_c-d~e"), "a.b_c-d~e");
        assert_eq!(query_escape("a b"), "a+b");
        assert_eq!(query_escape("a/b?c&d=e"), "a%2Fb%3Fc%26d%3De");
        assert_eq!(query_escape("a/b:c?d&e=f"), "a%2Fb%3Ac%3Fd%26e%3Df");
        assert_eq!(query_escape("ä"), "%C3%A4");
        assert_eq!(query_escape("é"), "%C3%A9");
        assert_eq!(query_escape(""), "");
    }
}
