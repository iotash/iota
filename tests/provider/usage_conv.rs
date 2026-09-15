//! Dialect usage converter tests (`provider/usage_test.go` `TestDialectUsageConversion`), one sub-case per dialect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// Go: provider/usage_test.go:48
#[test]
fn test_dialect_usage_conversion() {
    // chatcomp keeps cached inside input
    {
        use iota::llm::chatcomp::{OpenAiTokenDetails, OpenAiUsage};
        use iota::provider::usage_conv::openai_usage;
        let u = openai_usage(&OpenAiUsage {
            input_tokens: 1000,
            output_tokens: 200,
            total_tokens: 1200,
            input_tokens_details: Some(OpenAiTokenDetails { cached_tokens: 768 }),
        });
        assert_eq!((u.input, u.cache_read), (1000, 768), "usage = {u:?}");
        assert_eq!(u.context_tokens(), 1200, "cached must not be added on top");

        // chatcomp without a total falls back to the sum (compat servers often omit total_tokens).
        let u = openai_usage(&OpenAiUsage {
            input_tokens: 30,
            output_tokens: 12,
            ..OpenAiUsage::default()
        });
        assert_eq!(u.context_tokens(), 42);
        assert_eq!(u.total, 42);
        assert_eq!(u.cache_read, 0);
    }

    // anthropic adds cache beside input
    {
        use iota::llm::anthropic::AnthropicUsage;
        use iota::provider::usage_conv::anthropic_usage;
        let u = anthropic_usage(&AnthropicUsage {
            input_tokens: Some(120),
            output_tokens: Some(80),
            cache_read_input_tokens: Some(9000),
            cache_creation_input_tokens: Some(1000),
        });
        assert_eq!(u.total, 0, "anthropic must not invent a total: {u:?}");
        assert_eq!(u.context_tokens(), 10200, "cache is additional context");
        assert_eq!((u.cache_read, u.cache_write), (9000, 1000));
    }

    // google counts thinking tokens (candidatesTokenCount EXCLUDES thoughts; totalTokenCount includes them).
    {
        use iota::llm::google::GUsageMetadata;
        use iota::provider::usage_conv::google_usage;
        let u = google_usage(&GUsageMetadata {
            prompt_token_count: 5000,
            candidates_token_count: 300,
            thoughts_token_count: 2000,
            total_token_count: 7300,
            ..GUsageMetadata::default()
        });
        assert_eq!(u.output, 2300, "candidates + thoughts");
        assert_eq!(u.context_tokens(), 7300, "the reported total wins");

        let u = google_usage(&GUsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            cached_content_token_count: 4,
            ..GUsageMetadata::default()
        });
        assert_eq!((u.total, u.cache_read), (15, 4));
    }

    // responses keeps cached inside input
    {
        use iota::llm::chatcomp::{OpenAiTokenDetails, OpenAiUsage};
        use iota::provider::usage_conv::openai_usage;
        let u = openai_usage(&OpenAiUsage {
            input_tokens: 900,
            output_tokens: 100,
            total_tokens: 1000,
            input_tokens_details: Some(OpenAiTokenDetails { cached_tokens: 640 }),
        });
        assert_eq!(u.context_tokens(), 1000);
        assert_eq!(u.cache_read, 640);

        let u = openai_usage(&OpenAiUsage {
            input_tokens: 9,
            output_tokens: 1,
            ..OpenAiUsage::default()
        });
        assert_eq!(u.total, 10);
    }
}
