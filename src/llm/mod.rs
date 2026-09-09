//! Wire layer (internal/llm): the hand-rolled HTTP/SSE client, the error taxonomy, the SSE reader, the shared
//! OpenAI-shaped `GET /models` listing and one module per dialect. The `Provider` adapters over it live in
//! `crate::provider`.

pub mod client;
pub(crate) mod error;
pub mod json;
pub(crate) mod multipart;
pub(crate) mod progress;
pub mod reqlog;
pub(crate) mod sse;

// The frozen contract names the error taxonomy at `crate::llm::{LlmError, StatusError}`
// (TUI_CONTRACTS §1.4/§7); the concrete types live one module down.
pub use client::{Client, Jitter, RandJitter, StatusError, default_http_client};
pub use error::{LlmError, RespFailure};
pub use sse::{Event, Sse};

pub mod models;

pub mod anthropic;
pub mod chatcomp;
pub mod google;
pub(crate) mod images;
pub mod responses;

/// `deserialize_with` helper for wire strings a server may send EMPTY where it means absent
/// (a Gemini call without an id, a Responses item without a `call_id`): `""` and a missing key
/// both read as `None`.
pub(crate) fn none_if_empty<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    let s = <Option<String> as serde::Deserialize>::deserialize(d)?;
    Ok(s.filter(|s| !s.is_empty()))
}
