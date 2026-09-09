//! Provider contracts (provider/provider.go:132-331, base.go): `ProviderKind`, `Effort`, the `Provider` and
//! `ToolProvider` traits, the optional capability traits, the per-call result structs — and, below them, the
//! seven `Provider` adapters (one module per dialect, all always compiled in, like Go), the think-tag
//! splitter, the usage converters and the `new_provider` factory (provider/provider.go:310-331). The wire
//! layer they ride on is `crate::llm`.

pub use tokio_util::sync::CancellationToken;

pub mod error;
pub mod model;
pub mod sink;
pub mod usage;

pub(crate) mod common;
pub mod image_util;
pub mod think;
pub mod usage_conv;

pub mod anthropic;
pub mod google;
pub mod imagen;
pub mod images;
pub mod openai;
pub mod openresponses;

use std::sync::Arc;

use crate::BoxFuture;
use crate::llm::default_http_client;
use crate::llm::reqlog::RequestLog;
use crate::provider::error::{InvalidEffort, ProviderError, UnknownProviderType};
use crate::provider::model::{Attachment, Message, RawContent, ToolCall, ToolDef};
use crate::provider::sink::StreamSink;
use crate::provider::usage::Usage;

/// The seven built-in provider types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// `openai` (chat-completions dialect).
    OpenAi,
    /// `anthropic`.
    Anthropic,
    /// `gemini` (Google dialect, Gemini API).
    Gemini,
    /// `vertexai` (Google dialect, Vertex AI).
    VertexAi,
    /// `openresponses` (Responses dialect).
    OpenResponses,
    /// `imagen` (Google image generation).
    Imagen,
    /// `images` (`OpenAI`-shaped image generation).
    Images,
}

impl ProviderKind {
    /// Go `knownTypes` order.
    pub const ALL: [ProviderKind; 7] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::VertexAi,
        Self::OpenResponses,
        Self::Imagen,
        Self::Images,
    ];

    /// The supported-types list as printed in `UnknownProviderType`.
    pub const SUPPORTED_LIST: &'static str =
        "openai, anthropic, gemini, vertexai, openresponses, imagen, images";

    /// The Go type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::VertexAi => "vertexai",
            Self::OpenResponses => "openresponses",
            Self::Imagen => "imagen",
            Self::Images => "images",
        }
    }

    /// Environment variable holding the API key (cmd/root.go:551-559 table).
    pub const fn env_key(self) -> &'static str {
        match self {
            Self::OpenAi | Self::OpenResponses | Self::Images => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Gemini | Self::VertexAi | Self::Imagen => "GOOGLE_API_KEY",
        }
    }

    /// Whether `s` names a built-in provider type (exact match).
    pub fn is_known(s: &str) -> bool {
        Self::ALL.iter().any(|k| k.as_str() == s)
    }
}

impl std::str::FromStr for ProviderKind {
    type Err = UnknownProviderType;

    /// Exact strings only.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownProviderType(s.to_owned()))
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Env key for a raw type string: known kind → its key, anything else → `"API_KEY"`.
pub(crate) fn provider_env_key(raw_type: &str) -> &'static str {
    raw_type
        .parse::<ProviderKind>()
        .map_or("API_KEY", ProviderKind::env_key)
}

/// Reasoning effort level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effort {
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    XHigh,
    /// `max`.
    Max,
}

impl Effort {
    /// The accepted spellings, in order.
    pub const NAMES: [&'static str; 5] = ["low", "medium", "high", "xhigh", "max"];

    /// The Go effort string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// One of `NAMES`, exactly; anything else — the empty string included — is
    /// `Err(InvalidEffort(raw.to_owned()))`.
    pub fn parse(s: &str) -> Result<Effort, InvalidEffort> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(InvalidEffort(other.to_owned())),
        }
    }

    /// A level read at a boundary where `""` means "unset": `config.yaml`, session meta, a tool
    /// argument, a picker's "default" row.
    pub fn optional(s: &str) -> Result<Option<Effort>, InvalidEffort> {
        if s.is_empty() {
            Ok(None)
        } else {
            Self::parse(s).map(Some)
        }
    }
}

/// Result of a unary `Provider::chat` call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatResult {
    /// The visible reply text.
    pub text: String,
    /// Token usage of the call; `None` = the provider reported nothing.
    pub usage: Option<Usage>,
    /// Images the call generated.
    pub images: Vec<Attachment>,
}

/// Result of one tool-loop round (`ToolProvider::stream_chat_with_tools`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoundResult {
    /// Visible content of the round.
    pub content: String,
    /// Wire-level reasoning first, then tag-extracted think text.
    pub reasoning: String,
    /// Tool calls the model requested.
    pub tool_calls: Vec<ToolCall>,
    /// None = the call reported no usage (Go ok=false).
    pub usage: Option<Usage>,
    /// The dialect's replay payload for the assistant message.
    pub raw_content: Option<RawContent>,
    /// Images the round generated.
    pub images: Vec<Attachment>,
}

/// An LLM provider. Calls take `&self`; every per-call product is returned in `ChatResult`/`RoundResult`.
pub trait Provider: Send + Sync {
    /// The provider type.
    fn kind(&self) -> ProviderKind;
    /// The current model.
    fn model(&self) -> &str;
    /// Switches the model.
    fn set_model(&mut self, model: String);
    /// Lists the available model ids.
    fn list_models<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>>;
    /// Unary send without tools. Text providers return the visible half of `split_inline_think`.
    fn chat<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>>;

    // Optional capabilities — the accessor IS the capability check.

    /// Tool-calling capability.
    fn as_tool_provider(&self) -> Option<&dyn ToolProvider> {
        None
    }
    /// Temperature/effort tuning capability.
    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        None
    }
    /// Nucleus-sampling capability.
    fn as_top_p_tunable(&mut self) -> Option<&mut dyn TopPTunable> {
        None
    }
    /// Image-output opt-in capability.
    fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
        None
    }
    /// Image-generation parameter capability.
    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn ImageGenTunable> {
        None
    }
    /// JSON-edits switch capability.
    fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
        None
    }
    /// Tool-search host capability.
    fn as_tool_search_host(&mut self) -> Option<&mut dyn ToolSearchHost> {
        None
    }

    /// True when the dialect records last-call usage (Go `UsageReporter` probe, run.go:53;
    /// `LastUsageFull` parity). This is the `tokenAware` source: WP53 gates /compact
    /// registration, the auto-offer, the Context tab and the ctx/token status segments on
    /// it; T1 lays the inert call sites (T-38). Default false; the four text dialects
    /// return true, imagen/images MUST stay false (`imagen_test.go:200` — pinned by a
    /// capability test).
    fn reports_usage(&self) -> bool {
        false
    }

    /// Progressive-frame capability (the `images` dialect; `None` for imagen — no streaming
    /// form). The accessor IS the capability check (T3 design D4).
    fn as_image_partial_provider(&self) -> Option<&dyn ImagePartialProvider> {
        None
    }
}

/// The unary image dialects' progressive-frame seam (provider/images.go:61
/// `SetImagePartialObserver`, as a call-scoped observer — the T-11 precedent).
pub trait ImagePartialProvider: Send + Sync {
    /// `chat` with an observer: the request carries `stream:true, partial_images:1`, every
    /// `.partial_image` frame reaches `on_partial` (decoded bytes, non-empty), the `.completed`
    /// frame is the result. A backend answering plain JSON never calls `on_partial`.
    fn chat_observed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        on_partial: &'a mut (dyn FnMut(&[u8]) + Send),
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>>;
}

/// One HTTP transport: the shared `reqwest::Client` plus the optional `/debug` recorder
/// (cmd/root.go:128 `reqLog.HTTPClient()`). `From<reqwest::Client>` = no recorder.
#[derive(Clone, Default)]
pub struct HttpTransport {
    /// The shared HTTP client.
    pub client: reqwest::Client,
    /// The `/debug` request log, when this transport records.
    pub recorder: Option<Arc<RequestLog>>,
}

impl From<reqwest::Client> for HttpTransport {
    fn from(client: reqwest::Client) -> Self {
        Self {
            client,
            recorder: None,
        }
    }
}

/// Providers that support tool calling.
pub trait ToolProvider: Send + Sync {
    /// One tool-loop round. MUST call `sink.reasoning_done()` before the first `content()`,
    /// before the first tool-argument delta, and on every exit path (use `ReasoningGate`).
    fn stream_chat_with_tools<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        messages: &'a [Message],
        tools: &'a [ToolDef],
        sink: &'a mut dyn StreamSink,
    ) -> BoxFuture<'a, Result<RoundResult, ProviderError>>;
}

/// Sampling/reasoning parameters adjustable after construction. `None` = omit the parameter.
pub trait Tunable: Send + Sync {
    /// Sets the temperature (`None` = provider default).
    fn set_temperature(&mut self, t: Option<f64>);
    /// The configured temperature.
    fn temperature(&self) -> Option<f64>;
    /// Sets the reasoning effort (`None` = provider default).
    fn set_effort(&mut self, e: Option<Effort>);
    /// The configured effort.
    fn effort(&self) -> Option<Effort>;
}

/// Nucleus sampling (config-only `top_p`).
pub trait TopPTunable: Send + Sync {
    /// Sets `top_p` (`None` = provider default).
    fn set_top_p(&mut self, p: Option<f64>);
    /// The configured `top_p`.
    fn top_p(&self) -> Option<f64>;
}

/// Providers whose image generation needs an explicit request-side opt-in.
pub trait ImageTunable: Send + Sync {
    /// Switches image output on or off.
    fn set_image_output(&mut self, on: bool);
    /// Whether image output is requested.
    fn image_output(&self) -> bool;
}

/// Generation knobs of dedicated image providers (imagen / images); `None` = omit, server default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageGenParams {
    /// e.g. `1:1`, `3:2`, `16:9`.
    pub aspect_ratio: Option<String>,
    /// e.g. `1K`, `2K` (imagen) — dialect-specific.
    pub image_size: Option<String>,
    /// Negative prompt.
    pub negative_prompt: Option<String>,
}

impl ImageGenParams {
    /// The knobs as `config.yaml` and session meta carry them: verbatim strings, `""` = unset.
    pub fn from_raw(aspect_ratio: &str, image_size: &str, negative_prompt: &str) -> Self {
        let set = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        Self {
            aspect_ratio: set(aspect_ratio),
            image_size: set(image_size),
            negative_prompt: set(negative_prompt),
        }
    }

    /// Whether every knob is unset.
    pub fn is_empty(&self) -> bool {
        self.aspect_ratio.is_none() && self.image_size.is_none() && self.negative_prompt.is_none()
    }
}

/// The choice lists a dedicated image provider offers for its generation parameters (per-dialect unions).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageGenOptions {
    /// Accepted aspect ratios.
    pub aspect_ratios: Vec<&'static str>,
    /// Accepted image sizes.
    pub image_sizes: Vec<&'static str>,
    /// Whether the dialect carries a negative prompt at all.
    pub negative_prompt: bool,
}

/// Dedicated image providers expose their generation parameters.
pub trait ImageGenTunable: Send + Sync {
    /// Sets the generation parameters.
    fn set_image_gen_params(&mut self, p: ImageGenParams);
    /// The current generation parameters.
    fn image_gen_params(&self) -> &ImageGenParams;
    /// The choice lists this dialect offers.
    fn image_gen_options(&self) -> ImageGenOptions;
}

/// Image providers whose edit endpoint comes in two wire flavours expose the JSON switch.
pub trait ImageEditJsonTunable: Send + Sync {
    /// Selects the JSON body encoding for edits.
    fn set_json_edits(&mut self, on: bool);
    /// Whether JSON edits are selected.
    fn json_edits(&self) -> bool;
}

/// Installed by the run loop; returns at most 5 ranked deferred tools (empty when the dispatcher lacks the capability).
pub type ToolSearcher = std::sync::Arc<dyn Fn(&str) -> Vec<ToolDef> + Send + Sync>;

/// Providers that host a tool-search protocol leg (responses).
pub trait ToolSearchHost: Send + Sync {
    /// Installs (or clears) the searcher the run loop provides.
    fn set_tool_searcher(&mut self, f: Option<ToolSearcher>);
}

#[cfg(test)]
mod tests {
    use super::{Effort, ImageGenParams, ProviderKind, provider_env_key};
    use crate::provider::error::{InvalidEffort, UnknownProviderType};

    #[test]
    fn provider_kind_from_str_error_text() {
        let table = [
            ("openai", ProviderKind::OpenAi),
            ("anthropic", ProviderKind::Anthropic),
            ("gemini", ProviderKind::Gemini),
            ("vertexai", ProviderKind::VertexAi),
            ("openresponses", ProviderKind::OpenResponses),
            ("imagen", ProviderKind::Imagen),
            ("images", ProviderKind::Images),
        ];
        for (s, kind) in table {
            assert_eq!(s.parse::<ProviderKind>(), Ok(kind));
            assert_eq!(kind.as_str(), s);
            assert_eq!(kind.to_string(), s);
            assert!(ProviderKind::is_known(s));
        }
        // Go knownTypes order.
        assert_eq!(
            ProviderKind::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>(),
            [
                "openai",
                "anthropic",
                "gemini",
                "vertexai",
                "openresponses",
                "imagen",
                "images"
            ]
        );
        // Exact strings only; the error carries the raw value and the supported list (provider.go:329).
        for bad in ["OpenAI", " openai", "opnai", ""] {
            assert_eq!(
                bad.parse::<ProviderKind>(),
                Err(UnknownProviderType(bad.to_owned()))
            );
            assert!(!ProviderKind::is_known(bad));
        }
        assert_eq!(
            "opnai".parse::<ProviderKind>().unwrap_err().to_string(),
            "unknown provider type: opnai (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)"
        );
    }

    #[test]
    fn effort_parse_table() {
        assert_eq!(Effort::optional(""), Ok(None));
        assert_eq!(Effort::parse(""), Err(InvalidEffort(String::new())));
        for (s, e) in [
            ("low", Effort::Low),
            ("medium", Effort::Medium),
            ("high", Effort::High),
            ("xhigh", Effort::XHigh),
            ("max", Effort::Max),
        ] {
            assert_eq!(Effort::parse(s), Ok(e));
            assert_eq!(Effort::optional(s), Ok(Some(e)));
            assert_eq!(e.as_str(), s);
        }
        assert_eq!(Effort::NAMES, ["low", "medium", "high", "xhigh", "max"]);
        for bad in ["HIGH", "ultra", " low", "none"] {
            let err = Effort::parse(bad).unwrap_err();
            assert_eq!(err, InvalidEffort(bad.to_owned()));
            assert_eq!(err.to_string(), bad);
        }
        assert!(ImageGenParams::default().is_empty());
        assert!(
            !ImageGenParams {
                aspect_ratio: Some("1:1".to_owned()),
                ..ImageGenParams::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn env_key_table() {
        // cmd/root.go:551-559
        let table = [
            (ProviderKind::OpenAi, "OPENAI_API_KEY"),
            (ProviderKind::Anthropic, "ANTHROPIC_API_KEY"),
            (ProviderKind::Gemini, "GOOGLE_API_KEY"),
            (ProviderKind::VertexAi, "GOOGLE_API_KEY"),
            (ProviderKind::OpenResponses, "OPENAI_API_KEY"),
            (ProviderKind::Imagen, "GOOGLE_API_KEY"),
            (ProviderKind::Images, "OPENAI_API_KEY"),
        ];
        for (kind, key) in table {
            assert_eq!(kind.env_key(), key);
            assert_eq!(provider_env_key(kind.as_str()), key);
        }
        assert_eq!(provider_env_key("custom"), "API_KEY");
        assert_eq!(provider_env_key(""), "API_KEY");
    }
}

/// Construction parameters shared by every provider (provider.go:310-331).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderParams<'a> {
    /// API key (already resolved: flag > env > config).
    pub api_key: &'a str,
    /// Base URL; `""` selects the dialect default.
    pub base_url: &'a str,
    /// Model id.
    pub model: &'a str,
    /// Sampling temperature; `None` = provider default (dropped for image kinds).
    pub temperature: Option<f64>,
}

/// Dispatch on `kind`. `http` None ⇒ `default_http_client()` with no recorder. Infallible today: every kind has its constructor
/// compiled in; the `Result` is kept for the callers' error ladder (unknown type STRINGS are rejected earlier by
/// `ProviderKind::from_str` with the Go text).
///
/// Constructor mapping (temperature dropped for image kinds): `OpenAi → OpenAiProvider::new`,
/// `Anthropic → AnthropicProvider::new`, `Gemini → GoogleProvider::gemini`, `VertexAi → GoogleProvider::vertex_ai`,
/// `OpenResponses → OpenResponsesProvider::new`, `Imagen → ImagenProvider::new`, `Images → ImagesProvider::new`.
// CONTRACTS §3.0 fixes `p: ProviderParams<'_>` by value; its fields are all `Copy`, so clippy sees no consumption.
#[allow(clippy::needless_pass_by_value)]
pub fn new_provider(
    kind: ProviderKind,
    p: ProviderParams<'_>,
    http: Option<HttpTransport>,
) -> Result<Box<dyn Provider>, ProviderError> {
    let ProviderParams {
        api_key,
        base_url,
        model,
        temperature,
    } = p;
    let http = http.unwrap_or_else(|| HttpTransport::from(default_http_client()));
    match kind {
        ProviderKind::OpenAi => Ok(Box::new(openai::OpenAiProvider::new(
            api_key,
            base_url,
            model,
            temperature,
            http,
        ))),

        ProviderKind::Anthropic => Ok(Box::new(anthropic::AnthropicProvider::new(
            api_key,
            base_url,
            model,
            temperature,
            http,
        ))),

        ProviderKind::Gemini => Ok(Box::new(google::GoogleProvider::gemini(
            api_key,
            base_url,
            model,
            temperature,
            http,
        ))),

        ProviderKind::VertexAi => Ok(Box::new(google::GoogleProvider::vertex_ai(
            api_key,
            base_url,
            model,
            temperature,
            http,
        ))),

        ProviderKind::OpenResponses => Ok(Box::new(openresponses::OpenResponsesProvider::new(
            api_key,
            base_url,
            model,
            temperature,
            http,
        ))),

        ProviderKind::Imagen => Ok(Box::new(imagen::ImagenProvider::new(
            api_key, base_url, model, http,
        ))),

        ProviderKind::Images => Ok(Box::new(images::ImagesProvider::new(
            api_key, base_url, model, http,
        ))),
    }
}
