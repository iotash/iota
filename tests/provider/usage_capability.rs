//! The `Provider::reports_usage` capability pin (T-38; Go `UsageReporter` probe, run.go:53).
//! The four text dialects report usage; the dedicated image providers MUST NOT — the chat
//! layer's capability gates (compaction, ctx meter, token status segments) key off this.

use iota::provider::Provider;

/// A fresh client for the capability probes (no request is ever sent).
fn http() -> reqwest::Client {
    reqwest::Client::new()
}

// Go: provider/openai.go:18 (`var _ UsageReporter = (*OpenAIProvider)(nil)`)
#[test]
fn openai_reports_usage() {
    let p = iota::provider::openai::OpenAiProvider::new("k", "", "m", None, http());
    assert!(p.reports_usage(), "openai must report usage");
}

// Go: provider/anthropic.go:18 (`var _ UsageReporter = (*AnthropicProvider)(nil)`)
#[test]
fn anthropic_reports_usage() {
    let p = iota::provider::anthropic::AnthropicProvider::new("k", "", "m", None, http());
    assert!(p.reports_usage(), "anthropic must report usage");
}

// Go: provider/google.go:16 (`var _ UsageReporter = (*GoogleProvider)(nil)`)
#[test]
fn google_reports_usage() {
    let gemini = iota::provider::google::GoogleProvider::gemini("k", "", "m", None, http());
    assert!(gemini.reports_usage(), "gemini must report usage");
    let vertex = iota::provider::google::GoogleProvider::vertex_ai("k", "", "m", None, http());
    assert!(vertex.reports_usage(), "vertexai must report usage");
}

// Go: provider/openresponses.go:17 (`var _ UsageReporter = (*OpenResponsesProvider)(nil)`)
#[test]
fn openresponses_reports_usage() {
    let p = iota::provider::openresponses::OpenResponsesProvider::new("k", "", "m", None, http());
    assert!(p.reports_usage(), "openresponses must report usage");
}

// Go: provider/imagen_test.go:200 ("imagen must not be a UsageReporter")
#[test]
fn test_imagen_capability_surface_usage() {
    let p = iota::provider::imagen::ImagenProvider::new("k", "", "m", http());
    assert!(!p.reports_usage(), "imagen must not be a UsageReporter");
}

// Go: provider/images_test.go:174 ("images must not be a UsageReporter")
#[test]
fn test_images_capability_surface_usage() {
    let p = iota::provider::images::ImagesProvider::new("k", "", "m", http());
    assert!(!p.reports_usage(), "images must not be a UsageReporter");
}
