//! Token accounting of ONE API call (provider/provider.go:61-130).

/// Token accounting of ONE API call (provider/provider.go:61-130). Fields a dialect does not report stay zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input (prompt) tokens.
    pub input: u64,
    /// Output (completion) tokens.
    pub output: u64,
    /// Prompt-cache read tokens (inside or beside `input`, dialect-specific — see `context_tokens`).
    pub cache_read: u64,
    /// Prompt-cache write tokens.
    pub cache_write: u64,
    /// The provider's own total for the call, when it reports one (authoritative).
    pub total: u64,
}

impl Usage {
    /// Whether this dialect files its cache counts BESIDE `input` rather than inside it (the absent total is the tell).
    fn cache_beside_input(self) -> bool {
        self.total == 0
    }

    /// `total` when non-zero, else `input + output + cache_read + cache_write`.
    pub fn context_tokens(self) -> u64 {
        if self.cache_beside_input() {
            self.input + self.output + self.cache_read + self.cache_write
        } else {
            self.total
        }
    }

    /// `input` when `total` non-zero, else `input + cache_read + cache_write`.
    pub fn prompt_tokens(self) -> u64 {
        if self.cache_beside_input() {
            self.input + self.cache_read + self.cache_write
        } else {
            self.input
        }
    }

    /// 0 when `prompt_tokens() == 0 || cache_read == 0`; else `cache_read / prompt * 100` (f64).
    #[allow(clippy::cast_precision_loss)] // token counts never approach 2^53
    pub fn cache_hit_rate(self) -> f64 {
        let prompt = self.prompt_tokens();
        if prompt == 0 || self.cache_read == 0 {
            return 0.0;
        }
        self.cache_read as f64 / prompt as f64 * 100.0
    }

    /// Whether any cache activity was accounted for at all.
    pub fn cached(self) -> bool {
        self.cache_read > 0 || self.cache_write > 0
    }
}

impl core::ops::AddAssign for Usage {
    /// Field-wise, INCLUDING total.
    fn add_assign(&mut self, o: Self) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.total += o.total;
    }
}

#[cfg(test)]
mod tests {
    use super::Usage;

    // Go: provider/usage_test.go:12
    #[test]
    fn test_usage_context_tokens() {
        let cases = [
            (
                "total wins over the itemized parts",
                Usage {
                    input: 1000,
                    output: 200,
                    cache_read: 800,
                    total: 1200,
                    ..Usage::default()
                },
                1200,
            ),
            (
                "no total sums every part",
                Usage {
                    input: 100,
                    output: 200,
                    cache_read: 3000,
                    cache_write: 500,
                    total: 0,
                },
                3800,
            ),
            (
                "plain in/out",
                Usage {
                    input: 10,
                    output: 5,
                    ..Usage::default()
                },
                15,
            ),
            ("zero", Usage::default(), 0),
        ];
        for (name, u, want) in cases {
            assert_eq!(u.context_tokens(), want, "{name}");
        }
    }

    // Go: provider/usage_test.go:116
    #[test]
    fn test_usage_prompt_tokens_and_hit_rate() {
        // cache inside input: 13056 of a 17000-token prompt came from cache.
        let u = Usage {
            input: 17000,
            output: 500,
            cache_read: 13056,
            total: 17500,
            ..Usage::default()
        };
        assert_eq!(u.prompt_tokens(), 17000, "cache already inside");
        let rate = u.cache_hit_rate();
        assert!(
            (76.7..=76.9).contains(&rate),
            "hit rate = {rate}, want ~76.8"
        );
        assert!(u.cached());

        // cache beside input: 200 fresh tokens on top of a 9800-token cached prefix.
        let u = Usage {
            input: 200,
            output: 300,
            cache_read: 9800,
            ..Usage::default()
        };
        assert_eq!(u.prompt_tokens(), 10000, "cache is additional");
        assert!((u.cache_hit_rate() - 98.0).abs() < 1e-9);

        // no cache reports nothing.
        let u = Usage {
            input: 1000,
            output: 100,
            total: 1100,
            ..Usage::default()
        };
        assert!(!u.cached());
        assert!(u.cache_hit_rate().abs() < f64::EPSILON);

        // empty usage.
        let u = Usage::default();
        assert!(!u.cached());
        assert!(u.cache_hit_rate().abs() < f64::EPSILON);
        assert_eq!(u.prompt_tokens(), 0);
    }

    #[test]
    fn add_assign_sums_every_field_including_total() {
        let mut u = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total: 5,
        };
        u += Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            total: 50,
        };
        assert_eq!(
            u,
            Usage {
                input: 11,
                output: 22,
                cache_read: 33,
                cache_write: 44,
                total: 55
            }
        );
        // An anthropic-style run keeps the "no total" tell while the parts accumulate.
        let mut a = Usage::default();
        a += Usage {
            input: 5,
            ..Usage::default()
        };
        assert_eq!(a.total, 0);
        assert_eq!(a.context_tokens(), 5);
    }
}
