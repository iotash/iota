//! Pins the user-visible `Display` texts of iota-core's error types byte-for-byte against the Go sources
//! (provider.go:329, openai.go:64,148,154,182, tool.go:521, POLICY F-02).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::error::Error;

use iota::llm::LlmError;
use iota::mcp::config::McpFlagError;
use iota::provider::ProviderKind;
use iota::provider::error::{
    InvalidEffort, PermanentError, ProviderError, UnknownProviderType, WireOp,
};
use iota::tool::error::ToolError;

#[test]
fn unknown_provider_type_display() {
    // provider/provider.go:329
    assert_eq!(
        UnknownProviderType("opnai".to_owned()).to_string(),
        "unknown provider type: opnai (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)"
    );
    assert_eq!(
        "x".parse::<ProviderKind>().unwrap_err().to_string(),
        format!(
            "unknown provider type: x (supported: {})",
            ProviderKind::SUPPORTED_LIST
        )
    );
    assert_eq!(
        ProviderKind::SUPPORTED_LIST,
        ProviderKind::ALL
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn provider_error_display() {
    let wire = |op: WireOp, e: LlmError| ProviderError::wire(op, e);
    assert_eq!(
        wire(WireOp::Chat, LlmError::NoEvents).to_string(),
        "chat error: stream ended without any SSE events (server did not stream?)"
    );
    assert_eq!(
        wire(WireOp::Stream, LlmError::InBand("x".to_owned())).to_string(),
        "stream error: received error while streaming: x"
    );
    assert_eq!(
        wire(
            WireOp::ListModels,
            LlmError::InvalidModelName("bad?".to_owned())
        )
        .to_string(),
        r#"failed to list models: llm: invalid model name "bad?""#
    );
    // Image dialects pass the wire error through with no prefix at all.
    assert_eq!(
        wire(WireOp::Raw, LlmError::NoEvents).to_string(),
        "stream ended without any SSE events (server did not stream?)"
    );
    // A cancelled call is never wrapped.
    assert!(matches!(
        wire(WireOp::Stream, LlmError::Cancelled),
        ProviderError::Cancelled
    ));
    assert_eq!(ProviderError::NoChoices.to_string(), "no response choices");
    assert_eq!(
        ProviderError::permanent_msg("imagen: empty prompt").to_string(),
        "imagen: empty prompt"
    );
    assert_eq!(ProviderError::Cancelled.to_string(), "interrupted");
    assert_eq!(
        ProviderError::other("imagen: raw wire failure passes through").to_string(),
        "imagen: raw wire failure passes through"
    );
    // The wire failure stays reachable, typed and through `source()`.
    let e = wire(WireOp::Stream, LlmError::NoEvents);
    assert!(matches!(e.llm(), Some(LlmError::NoEvents)));
    assert_eq!(
        e.source().map(ToString::to_string),
        Some("stream ended without any SSE events (server did not stream?)".to_owned())
    );
    // `-l` double-prefixes, exactly like Go's provider + cmd wraps.
    let inner = wire(WireOp::ListModels, LlmError::NoEvents);
    assert!(
        format!("failed to list models: {inner}")
            .starts_with("failed to list models: failed to list models: ")
    );
}

#[test]
fn permanent_error_display_and_source() {
    let p = PermanentError::msg("images: response contained no images");
    assert_eq!(p.to_string(), "images: response contained no images");
    assert_eq!(
        p.source().map(ToString::to_string),
        Some("images: response contained no images".to_owned())
    );
    let wrapped = ProviderError::Permanent(p);
    assert_eq!(wrapped.to_string(), "images: response contained no images");
    assert!(matches!(wrapped, ProviderError::Permanent(_)));
}

#[test]
fn tool_error_display() {
    assert_eq!(
        ToolError::UnknownTool("mcp__x__y".to_owned()).to_string(),
        "unknown tool: mcp__x__y"
    );
    assert_eq!(ToolError::Cancelled.to_string(), "interrupted");
    let t = ToolError::Transport("connection closed".into());
    assert_eq!(t.to_string(), "connection closed");
    assert_eq!(
        t.source().map(ToString::to_string),
        Some("connection closed".to_owned())
    );
    assert_eq!(
        format!(
            "Error calling tool: {}",
            ToolError::UnknownTool("z".to_owned())
        ),
        "Error calling tool: unknown tool: z"
    );
}

#[test]
fn mcp_flag_and_effort_display() {
    assert_eq!(
        McpFlagError::EmptyFlag.to_string(),
        "--mcp: empty server specification"
    );
    assert_eq!(
        iota::mcp::config::parse_mcp_flag("   ").unwrap_err(),
        McpFlagError::EmptyFlag
    );
    // InvalidEffort displays the raw value; callers compose the Go sentence around it.
    let e = InvalidEffort("HIGH".to_owned());
    assert_eq!(e.to_string(), "HIGH");
    assert_eq!(
        format!("config effort {:?}: want low|medium|high|xhigh|max", e.0),
        "config effort \"HIGH\": want low|medium|high|xhigh|max"
    );
}
