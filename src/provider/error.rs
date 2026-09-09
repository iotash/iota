//! Provider error taxonomy (provider.go:207-214): `PermanentError`, `ProviderError`,
//! `UnknownProviderType` and `InvalidEffort`. Every Display text is byte-equal to Go.

use crate::BoxError;
use crate::llm::LlmError;

/// Failure retrying cannot fix (image dialects). Display delegates; `source()` is the inner error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PermanentError(#[source] pub BoxError);

impl PermanentError {
    /// Wraps a plain string error.
    pub fn msg(s: impl Into<String>) -> Self {
        Self(s.into().into())
    }
}

/// The call a wire failure came from — Go's `fmt.Errorf("chat error: %w")` prefixes (provider.go:207-214).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireOp {
    /// Unary chat.
    Chat,
    /// A streaming round.
    Stream,
    /// Model listing.
    ListModels,
    /// Image dialects pass the wire error through raw.
    Raw,
}

impl WireOp {
    /// Go's prefix for the call; `""` for `Raw`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Chat => "chat error: ",
            Self::Stream => "stream error: ",
            Self::ListModels => "failed to list models: ",
            Self::Raw => "",
        }
    }
}

/// A provider (LLM) failure as the chat layer sees it.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The wire layer failed. `op` names the call (its Go prefix leads the text) and the failure keeps its
    /// type, so the chat layer classifies it by matching, never by downcasting. Boxed: `LlmError` carries
    /// whole status bodies, and every `Result` up the chat layer would otherwise grow with it.
    #[error("{}{source}", .op.prefix())]
    Wire {
        /// The call that failed.
        op: WireOp,
        /// The wire failure.
        #[source]
        source: Box<LlmError>,
    },
    /// The response carried no choices.
    #[error("no response choices")]
    NoChoices,
    /// A failure retrying cannot fix.
    #[error(transparent)]
    Permanent(PermanentError),
    /// The run was cancelled.
    #[error("interrupted")]
    Cancelled,
    /// A failure with no wire type behind it, passed through raw (an image result fetch).
    #[error("{0}")]
    Other(#[source] BoxError),
}

impl ProviderError {
    /// `Wire { op, source }` — except that a cancelled call is `Cancelled`, never wrapped.
    pub fn wire(op: WireOp, e: LlmError) -> Self {
        match e {
            LlmError::Cancelled => Self::Cancelled,
            source => Self::Wire {
                op,
                source: Box::new(source),
            },
        }
    }

    /// `Other` wrapping `e`.
    pub fn other(e: impl Into<BoxError>) -> Self {
        Self::Other(e.into())
    }

    /// `Permanent` wrapping a plain string error.
    pub fn permanent_msg(s: impl Into<String>) -> Self {
        Self::Permanent(PermanentError::msg(s))
    }

    /// The wire failure behind a `Wire`.
    pub fn llm(&self) -> Option<&LlmError> {
        match self {
            Self::Wire { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// An unrecognised provider type string (provider.go:329).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown provider type: {0} (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)"
)]
pub struct UnknownProviderType(pub String);

/// Display is the raw value; callers compose the Go sentence around it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidEffort(pub String);
