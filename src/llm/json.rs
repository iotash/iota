//! Wire-level JSON building blocks shared by every dialect: the comparable verbatim payload `Raw` and the
//! sorted-key object alias `JsonObject`.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// A JSON object. `serde_json::Map` is `BTreeMap`-backed (no `preserve_order`), so keys iterate sorted — Go's
/// `map[string]any` marshal order.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;

/// A verbatim JSON payload (Go `json.RawMessage`). Wraps `Box<RawValue>` so it can be COMPARED: `RawValue` has no
/// `PartialEq` (serde_json-1.0.151 raw.rs). Equality is byte-equality of the JSON text (`get()`), which is exactly
/// what "replayed verbatim" means; tests compare `Message`/`RoundResult` by `==` and history-shape assertions
/// therefore compare `raw_content` textually.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Raw(pub Box<RawValue>);

impl Raw {
    /// `RawValue::from_string` — validates the text; the ONLY way to record a payload from a string.
    pub fn from_string(json: String) -> Result<Raw, serde_json::Error> {
        RawValue::from_string(json).map(Raw)
    }

    /// `serde_json::value::to_raw_value` — compact serialisation (sorted keys for maps = Go marshal order).
    pub fn from_value<T: Serialize + ?Sized>(v: &T) -> Result<Raw, serde_json::Error> {
        serde_json::value::to_raw_value(v).map(Raw)
    }

    /// The JSON text.
    pub fn get(&self) -> &str {
        self.0.get()
    }
}

impl PartialEq for Raw {
    fn eq(&self, o: &Self) -> bool {
        self.0.get() == o.0.get()
    }
}

impl Eq for Raw {}

impl From<Box<RawValue>> for Raw {
    fn from(v: Box<RawValue>) -> Self {
        Self(v)
    }
}

impl AsRef<RawValue> for Raw {
    fn as_ref(&self) -> &RawValue {
        &self.0
    }
}
