//! Go's `RawContentProvider` round trip, dialect-side (chat/session.go:529-538, :796-804).
//!
//! There is no provider hook: the mapping is a pure function of the provider
//! kind. The blob is carried verbatim as [`Raw`] and is NEVER parsed or validated — only its shape (a JSON
//! array versus a single value) is checked, because that is the only thing the variant choice depends on
//! (D-51a). A semantically wrong blob reaches the dialect, which already falls back at replay.

use crate::provider::ProviderKind;
use crate::provider::model::{Raw, RawContent};

use crate::session::record::SessionRaw;

/// Whether a blob's JSON text is an array (the shape the block-list variants need).
fn is_array(blob: &Raw) -> bool {
    blob.get().trim_start().starts_with('[')
}

/// The blob as an item list. A literal `null` — what a Go NIL slice would have marshalled to — is the
/// empty list, exactly as Go's `json.Unmarshal` into a slice reads it (D-50); anything that is not an
/// array is a shape mismatch and yields `None`.
fn blocks(raw: &SessionRaw) -> Option<Vec<Raw>> {
    serde_json::from_str::<Option<Vec<Raw>>>(raw.blob.get())
        .ok()
        .map(Option::unwrap_or_default)
}

/// WRITE (chat/session.go:529-538). `None` means "omit the `raw` key" — Go's silently-swallowed marshal
/// error, and its `(nil, nil)` empty-blocks rule.
///
/// | `RawContent` | kind | blob |
/// |---|---|---|
/// | `OpenAi(r)` | `OpenAi` | `r` verbatim (the assistant message object) |
/// | `Anthropic(v)` | `Anthropic` | a JSON array of `v`; an EMPTY `v` → `None` (D-50) |
/// | `OpenResponses(v)` | `OpenResponses` | a JSON array of `v`; an EMPTY `v` → `None` (D-50) |
/// | `Google(r)` | `Gemini` or `VertexAi` | `r` verbatim (the `GContent` JSON) |
/// | anything else | — | `None` |
pub fn raw_to_blob(kind: ProviderKind, rc: &RawContent) -> Option<Raw> {
    match (rc, kind) {
        (RawContent::OpenAi(r), ProviderKind::OpenAi)
        | (RawContent::Google(r), ProviderKind::Gemini | ProviderKind::VertexAi) => Some(r.clone()),
        (RawContent::Anthropic(v), ProviderKind::Anthropic)
        | (RawContent::OpenResponses(v), ProviderKind::OpenResponses) => {
            if v.is_empty() {
                return None;
            }
            Raw::from_value(v).ok()
        }
        _ => None,
    }
}

/// LOAD (chat/session.go:796-804). The payload is restored ONLY when `raw.provider` equals
/// `kind.as_str()` EXACTLY — a `"gemini"`-tagged blob does NOT restore under `"vertexai"` and vice versa,
/// although both are the Google dialect. A blob whose shape does not fit the variant (an array where a
/// value is wanted, or vice versa) yields `None`, reproducing Go's swallowed unmarshal error; the message
/// then degrades to content + tool calls.
pub fn blob_to_raw(kind: ProviderKind, raw: &SessionRaw) -> Option<RawContent> {
    if raw.provider != kind.as_str() {
        return None;
    }
    match kind {
        ProviderKind::OpenAi => {
            (!is_array(&raw.blob)).then(|| RawContent::OpenAi(raw.blob.clone()))
        }
        ProviderKind::Gemini | ProviderKind::VertexAi => {
            (!is_array(&raw.blob)).then(|| RawContent::Google(raw.blob.clone()))
        }
        ProviderKind::Anthropic => blocks(raw).map(RawContent::Anthropic),
        ProviderKind::OpenResponses => blocks(raw).map(RawContent::OpenResponses),
        ProviderKind::Imagen | ProviderKind::Images => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::provider::ProviderKind;
    use crate::provider::model::{Raw, RawContent};

    use super::{blob_to_raw, raw_to_blob};
    use crate::session::record::SessionRaw;

    fn raw(json: &str) -> Raw {
        Raw::from_string(json.to_owned()).expect("valid json")
    }

    /// A blob tagged with a different provider type never restores, and a mismatched pairing never writes.
    #[test]
    fn tag_must_match_exactly() {
        let stored = SessionRaw {
            provider: "gemini".to_owned(),
            blob: raw(r#"{"parts":[]}"#),
        };
        assert!(blob_to_raw(ProviderKind::Gemini, &stored).is_some());
        assert_eq!(blob_to_raw(ProviderKind::VertexAi, &stored), None);
        assert_eq!(blob_to_raw(ProviderKind::OpenAi, &stored), None);
        // Writing under a kind that cannot produce the variant omits the key.
        let google = RawContent::Google(raw(r#"{"parts":[]}"#));
        assert_eq!(raw_to_blob(ProviderKind::OpenAi, &google), None);
        assert!(raw_to_blob(ProviderKind::VertexAi, &google).is_some());
    }

    /// Imagen/images sessions never carry a replay payload.
    #[test]
    fn image_kinds_never_restore() {
        let stored = SessionRaw {
            provider: "imagen".to_owned(),
            blob: raw("{}"),
        };
        assert_eq!(blob_to_raw(ProviderKind::Imagen, &stored), None);
        assert_eq!(
            raw_to_blob(ProviderKind::Images, &RawContent::OpenAi(raw("{}"))),
            None
        );
    }
}
