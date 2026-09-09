//! Shared provider state (provider/base.go): `ProviderCore`, `HasCore` with the blanket `Tunable`/`TopPTunable`
//! impls over it, the attachment data-URL / base64 helpers and `make_client`.

use crate::provider::{Effort, HttpTransport, ProviderKind, TopPTunable, Tunable};
use base64::Engine;

use crate::llm::client::{Client, default_http_client};
use reqwest::header::HeaderValue;

/// Go baseProvider state (base.go:11-22) minus the per-call usage/observer fields.
#[derive(Clone, Debug)]
pub(crate) struct ProviderCore {
    /// The provider type.
    pub(crate) kind: ProviderKind,
    /// Current model id.
    pub(crate) model: String,
    /// Sampling temperature; `None` = omit.
    pub(crate) temperature: Option<f64>,
    /// Nucleus sampling; `None` = omit.
    pub(crate) top_p: Option<f64>,
    /// Reasoning effort; `None` = omit.
    pub(crate) effort: Option<Effort>,
}

/// Access to the shared core of a text provider.
pub(crate) trait HasCore {
    /// The core, read-only.
    fn core(&self) -> &ProviderCore;
    /// The core, mutable.
    fn core_mut(&mut self) -> &mut ProviderCore;
}

/// Every text provider tunes through its `ProviderCore`: one blanket impl, legal now that the traits and the
/// providers share a crate (the per-provider macro the crate boundary once forced is gone with the merge).
impl<T: HasCore + Send + Sync> Tunable for T {
    fn set_temperature(&mut self, t: Option<f64>) {
        self.core_mut().temperature = t;
    }

    fn temperature(&self) -> Option<f64> {
        self.core().temperature
    }

    fn set_effort(&mut self, e: Option<Effort>) {
        self.core_mut().effort = e;
    }

    fn effort(&self) -> Option<Effort> {
        self.core().effort
    }
}

/// The `top_p` twin of the blanket `Tunable` impl.
impl<T: HasCore + Send + Sync> TopPTunable for T {
    fn set_top_p(&mut self, p: Option<f64>) {
        self.core_mut().top_p = p;
    }

    fn top_p(&self) -> Option<f64> {
        self.core().top_p
    }
}

/// Default base URL of the OpenAI-shaped dialects (`https://api.openai.com/v1`).
pub(crate) const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// `"data:<mime>;base64,<std padded b64>"`.
pub(crate) fn data_url(mime: &str, data: &[u8]) -> String {
    format!("data:{mime};base64,{}", b64(data))
}

/// Standard padded base64 (Go `StdEncoding`).
pub(crate) fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Standard padded base64 decode.
pub(crate) fn b64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(s)
}

/// `Client::new(base_url_or_default, http.client)` with the transport's `/debug` recorder installed
/// when present; `None` ⇒ `default_http_client()` and no recorder.
pub(crate) fn make_client(base_url: &str, default: &str, http: Option<HttpTransport>) -> Client {
    let base = if base_url.is_empty() {
        default
    } else {
        base_url
    };
    let HttpTransport { client, recorder } =
        http.unwrap_or_else(|| HttpTransport::from(default_http_client()));
    let mut c = Client::new(base, client);
    if let Some(log) = recorder {
        c = c.with_recorder(log);
    }
    c
}

/// A credential header value (`Authorization`, `x-api-key`, `x-goog-api-key`), marked sensitive so any
/// `Debug` rendering (`wire::client::Client`'s included) prints `Sensitive` instead of the key bytes.
///
/// A key that cannot be spelled as an HTTP header value (control bytes) is sent as an EMPTY header — a
/// guaranteed, attributable 401 — rather than panicking or omitting the header: Go's `Header.Set` stores
/// any string and net/http fails the request when writing it. Every provider builds its credential
/// header here, so the convention cannot drift per dialect again.
pub(crate) fn credential_header(value: &str) -> HeaderValue {
    let mut v = HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""));
    v.set_sensitive(true);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_standard_padded() {
        assert_eq!(b64(&[1]), "AQ==");
        assert_eq!(b64(&[2]), "Ag==");
        assert_eq!(b64(b"hello"), "aGVsbG8=");
        assert_eq!(b64_decode("aGVsbG8=").unwrap(), b"hello");
        assert!(b64_decode("aGVsbG8").is_err(), "padding is mandatory");
        assert_eq!(data_url("image/png", &[1]), "data:image/png;base64,AQ==");
    }

    /// The shared invalid-key convention (idiom-8): an unrepresentable credential becomes the EMPTY
    /// header value (a guaranteed, attributable 401), and every credential value is marked sensitive.
    #[test]
    fn credential_header_pins_the_empty_header_convention() {
        let ok = credential_header("Bearer sk-secret");
        assert!(ok.is_sensitive());
        assert_eq!(ok.to_str().unwrap(), "Bearer sk-secret");

        let bad = credential_header("Bearer bad\nkey");
        assert!(bad.is_sensitive());
        assert_eq!(bad, "");
    }

    /// security-4: a `Client` holding credential headers never leaks them through `Debug`.
    #[test]
    fn client_debug_never_prints_credentials() {
        let client = make_client("http://h", "http://h", None).with_header(
            reqwest::header::AUTHORIZATION,
            credential_header("Bearer sk-secret"),
        );
        let dump = format!("{client:?}");
        assert!(dump.contains("authorization"), "{dump}");
        assert!(dump.contains("Sensitive"), "{dump}");
        assert!(!dump.contains("sk-secret"), "{dump}");
    }
}
