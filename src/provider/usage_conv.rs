//! Dialect usage → `Usage` converters (provider/usage.go).

use crate::provider::usage::Usage;

/// `input=input_tokens`, `output=output_tokens`, `total=total_tokens`, `cache_read=input_tokens_details.cached_tokens`;
/// `total==0` ⇒ `input+output`. Chat-completions and responses share the shape (`OpenAiUsage` decodes both namings).
///
/// The input figure ALREADY includes the cache hits, so `cache_read` is recorded for display only and the
/// authoritative total comes from the wire (compat servers often omit it); `output_tokens` already covers
/// reasoning tokens.
pub fn openai_usage(u: &crate::llm::chatcomp::OpenAiUsage) -> Usage {
    let mut out = Usage {
        input: u.input_tokens,
        output: u.output_tokens,
        total: u.total_tokens,
        ..Usage::default()
    };
    if let Some(d) = &u.input_tokens_details {
        out.cache_read = d.cached_tokens;
    }
    if out.total == 0 {
        out.total = out.input + out.output;
    }
    out
}

/// `input`, `output`, `cache_read=cache_read_input_tokens`, `cache_write=cache_creation_input_tokens`, `total` ALWAYS 0.
/// A field the wire never reported counts as 0.
///
/// Anthropic reports NO total and its `input_tokens` excludes both cache figures — they are additional context,
/// not a subset — so `total` is deliberately left at zero for `context_tokens` to sum the parts.
pub fn anthropic_usage(u: &crate::llm::anthropic::AnthropicUsage) -> Usage {
    Usage {
        input: u.input_tokens.unwrap_or(0),
        output: u.output_tokens.unwrap_or(0),
        cache_read: u.cache_read_input_tokens.unwrap_or(0),
        cache_write: u.cache_creation_input_tokens.unwrap_or(0),
        total: 0,
    }
}

/// `input=promptTokenCount`, `output=candidatesTokenCount+thoughtsTokenCount`, `cache_read=cachedContentTokenCount`, `total=totalTokenCount` or `input+output`.
///
/// candidatesTokenCount EXCLUDES thinking tokens while totalTokenCount includes them, which is why the total is
/// authoritative for context accounting; `output` keeps the thinking share so cumulative figures match billing.
pub fn google_usage(u: &crate::llm::google::GUsageMetadata) -> Usage {
    let mut out = Usage {
        input: u.prompt_token_count,
        output: u.candidates_token_count + u.thoughts_token_count,
        cache_read: u.cached_content_token_count,
        total: u.total_token_count,
        ..Usage::default()
    };
    if out.total == 0 {
        out.total = out.input + out.output;
    }
    out
}
