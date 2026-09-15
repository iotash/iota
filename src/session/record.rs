//! One `messages.jsonl` line (chat/session.go:67-130).
//!
//! Records are serialised COMPACT with a trailing `'\n'` and are NEVER rewritten. Unknown keys are ignored
//! on read (`serde` default) and Rust emits ONLY Go's key set. Two "always present even when zero" traps,
//! both verified against the Go source: `usage.in` / `usage.out`, and `tool_calls[].arguments`.

use serde::{Deserialize, Deserializer};

use crate::session::meta::{is_false, is_zero_i64, is_zero_u64};

/// The append-only message log inside a bundle.
pub const LOG_FILE: &str = "messages.jsonl";
/// The content-addressed attachment store inside a bundle.
pub const ATTACHMENTS_DIR: &str = "attachments";
/// Where images generated inside a session are written.
pub(crate) const IMAGES_DIR: &str = "images";
/// The pseudo-role of a compaction marker (chat/session.go:594) — not a `crate::provider::model::Role`.
pub(crate) const ROLE_COMPACTION: &str = "compaction";
/// `data_ref` prefix of an attachment reference (chat/session.go:502): `sha256:<64 lowercase hex>`.
pub(crate) const DATA_REF_PREFIX: &str = "sha256:";

/// One record of `messages.jsonl` (chat/session.go:67-88). Key emission order is the declaration order
/// below, which is Go's struct order.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionRecord {
    /// `"system" | "user" | "assistant" | "tool" | "compaction"` — or anything a future build writes. A
    /// plain `String` (Go's own shape): `crate::provider::model::Role` cannot hold `"compaction"`.
    pub role: String,
    /// Visible text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content: String,
    /// Thinking text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// References into the bundle's attachment store.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SessionAttachment>,
    /// Tool calls this assistant message requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<SessionToolCall>,
    /// Tool-result records: which call this answers.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool_call_id: String,
    /// Tool-result records: the function name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool_call_name: String,
    /// Tool-result records: whether the call failed.
    #[serde(skip_serializing_if = "is_false")]
    pub is_error: bool,
    /// Assistant messages cut short by the user.
    #[serde(skip_serializing_if = "is_false")]
    pub interrupted: bool,
    /// `role == "user"` only: the text is a host notice (a finished background job), not something the
    /// user typed. Absent in every session written before background jobs existed, which reads back as
    /// `false` — an old log still replays exactly as it did.
    #[serde(skip_serializing_if = "is_false")]
    pub notice: bool,
    /// The dialect's opaque replay payload, tagged with the provider type that produced it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<SessionRaw>,
    /// `role == "compaction"` only: how many leading conversation messages the summary supersedes. 0 omits
    /// the key.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub compacted_through: i64,
    /// What the API call behind this record cost; the log's sum is the session's cumulative usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<SessionUsage>,
}

/// One requested tool call (chat/session.go:115-119). `arguments` has NO `omitempty`: it is always
/// emitted.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionToolCall {
    /// Call id assigned by the provider.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Go writes `null` for a nil map and `{}` for an empty one, and reads both. Rust ALWAYS writes `{}`
    /// (D-49); a literal `null` or an absent key deserialises to the empty map.
    #[serde(deserialize_with = "null_as_empty_object")]
    pub arguments: crate::provider::model::JsonObject,
}

/// A reference into the bundle's content-addressed attachment store (chat/session.go:121-125). All three
/// keys are always present.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionAttachment {
    /// Basename of the original file.
    pub filename: String,
    /// MIME type.
    #[serde(rename = "mime")]
    pub mime_type: String,
    /// `"sha256:" + 64 lowercase hex` — the file under `attachments/`.
    pub data_ref: String,
}

/// The dialect's replay payload as stored (chat/session.go:127-130). Both keys are always present; `blob`
/// is carried verbatim and is never parsed by this crate (D-51a).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionRaw {
    /// The provider type that produced the blob (`ProviderKind::as_str`).
    pub provider: String,
    /// The dialect's opaque JSON value.
    pub blob: crate::provider::model::Raw,
}

/// Token accounting of one record (chat/session.go:90-99). `in`/`out` are ALWAYS emitted (even 0); the
/// rest are omitted when zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionUsage {
    /// Input (prompt) tokens.
    #[serde(rename = "in")]
    pub input: u64,
    /// Output (completion) tokens.
    #[serde(rename = "out")]
    pub output: u64,
    /// Prompt-cache read tokens.
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_read: u64,
    /// Prompt-cache write tokens.
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_write: u64,
    /// The provider's own total for the call.
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub total: u64,
}

impl From<crate::provider::usage::Usage> for SessionUsage {
    fn from(u: crate::provider::usage::Usage) -> Self {
        Self {
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            total: u.total,
        }
    }
}

impl From<SessionUsage> for crate::provider::usage::Usage {
    fn from(u: SessionUsage) -> Self {
        Self {
            input: u.input,
            output: u.output,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            total: u.total,
        }
    }
}

/// `null`, `{}` and an absent key all yield the empty map (D-49; Go's loader accepts a nil map too).
fn null_as_empty_object<'de, D>(d: D) -> Result<crate::provider::model::JsonObject, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<crate::provider::model::JsonObject>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{SessionRecord, SessionUsage};

    /// Usage converts both ways without loss.
    #[test]
    fn usage_conversions_round_trip() {
        let core = crate::provider::usage::Usage {
            input: 1000,
            output: 200,
            cache_read: 5,
            cache_write: 7,
            total: 1200,
        };
        let stored: SessionUsage = core.into();
        assert_eq!(stored.input, 1000);
        assert_eq!(stored.output, 200);
        assert_eq!(crate::provider::usage::Usage::from(stored), core);
    }

    /// An unknown key on a record is ignored, not an error (Go's plain `json.Unmarshal`).
    #[test]
    fn unknown_record_keys_are_ignored() {
        let rec: SessionRecord =
            serde_json::from_str(r#"{"role":"user","content":"hi","future_key":{"a":1}}"#).unwrap();
        assert_eq!(rec.role, "user");
        assert_eq!(rec.content, "hi");
    }
}
