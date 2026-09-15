//! The offline token counter and the token-accounting constants (`chat/tokens.go`).
//!
//! **Offline by construction.** Go paired `tiktoken-go` with `tiktoken-go-loader`'s
//! offline loader so a token count never becomes a network call; `tiktoken` embeds the same
//! `o200k_base` rank table in the binary (zstd-compressed, decompressed in-process), which
//! is the identical property arrived at from the other side. The encoder is built ONCE,
//! lazily, behind a [`std::sync::OnceLock`]: a chat that never needs a figure never pays the
//! table's decompression, and a chat that does pays it once.
//!
//! **The fallback is a byte count, not a character count.** Go's `len(text) / 4` counts
//! BYTES, so [`TokenCounter::count`] does too (`str::len`). Keeping the byte semantics is
//! what makes the degraded figure the same degraded figure on both sides — a `chars().count()`
//! twin would silently disagree by a factor of three on CJK.
//!
//! The counter is an estimator: the figures it produces are compared against a threshold
//! and shown behind a `≈`, never billed. That is the licence for the one place this port
//! cannot be exact — Go renders tool-call argument VALUES with `fmt %v`, which has no
//! total Rust twin for composite JSON (see [`go_value`]).

use crate::provider::model::JsonObject;
use crate::provider::model::Message;
use serde_json::Value;
use tiktoken::CoreBpe;

/// The assumed model context size when none is configured (`chat/tokens.go`
/// `defaultContextWindow`).
pub(crate) const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// What ONE attachment is assumed to cost (`chat/tokens.go` `attachmentTokens`).
///
/// Images and PDFs are not text-tokenizable, so the estimator needs a stand-in. 1200 is
/// the ballpark a full-size image actually bills at across providers; the 256 Go used
/// before was low by roughly 5×, which let an image-heavy history slip past the
/// compaction threshold unnoticed.
pub(crate) const ATTACHMENT_TOKENS: u64 = 1_200;

/// Compact when projected tokens reach this share of the window (`chat/compact.go`
/// `compactThresholdPercent`).
pub(crate) const COMPACT_THRESHOLD_PERCENT: u64 = 80;

/// The room the next exchange needs — one reply plus the message that asks for it
/// (`chat/compact.go` `compactReserveTokens`).
pub(crate) const COMPACT_RESERVE_TOKENS: u64 = 16_000;

/// How much of the window usage must grow after a declined auto-compaction offer before it
/// is offered again (`chat/compact.go` `compactSnoozePercent`).
pub(crate) const COMPACT_SNOOZE_PERCENT: u64 = 5;

/// Per-message framing overhead charged by `countMessages` (`chat/tokens.go:70`).
const MESSAGE_FRAMING_TOKENS: u64 = 4;

/// The process-wide `o200k_base` encoder, or `None` when it failed to build.
///
/// `None` is Go's `enc == nil`: the counter degrades to the byte heuristic rather than
/// failing, because a chat that cannot tokenize must still run. The crate hands out a
/// `&'static CoreBpe` from its own lazily-built table, so what the `OnceLock` memoizes is
/// the lookup, not the encoder.
static ENCODER: std::sync::OnceLock<Option<&'static CoreBpe>> = std::sync::OnceLock::new();

fn encoder() -> Option<&'static CoreBpe> {
    *ENCODER.get_or_init(|| tiktoken::get_encoding("o200k_base"))
}

/// The local fallback tokenizer (`o200k_base`, embedded) used for providers that do not
/// report usage and for sizing not-yet-sent content (`chat/tokens.go` `tokenCounter`).
///
/// Approximate for non-OpenAI models but fine for window accounting. A zero-sized handle:
/// the rank table itself is process-wide, so a counter can be copied into a closure, a
/// budget and a transcript without a refcount.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenCounter;

// The counter is a zero-sized HANDLE: the rank table is process-wide, so `self` carries no
// state. Keeping the methods on it (rather than as free functions) is deliberate — it is
// Go's `c.count(...)` seam, and the day a build wants a second tokenizer the shape is
// already right.
#[allow(clippy::unused_self)]
impl TokenCounter {
    /// The counter (`chat/tokens.go` `newTokenCounter`). Building the encoder is deferred to
    /// the first count.
    pub fn new() -> Self {
        Self
    }

    /// Whether the real tokenizer loaded — `false` means every count is the byte heuristic. Only the
    /// tests ask; the counter itself never branches on it.
    #[cfg(test)]
    pub fn has_encoder(self) -> bool {
        encoder().is_some()
    }

    /// Tokens in `text` (`chat/tokens.go` `count`): the `o200k_base` encoding with NO
    /// special tokens allowed — the exact shape of Go's `Encode(text, nil, nil)`, whose
    /// empty allowed/disallowed sets reduce to the ordinary encoder.
    pub fn count(self, text: &str) -> u64 {
        match encoder() {
            Some(bpe) => u64::try_from(bpe.encode(text).len()).unwrap_or(u64::MAX),
            None => u64::try_from(text.len() / 4).unwrap_or(u64::MAX),
        }
    }

    /// Tokens a history occupies (`chat/tokens.go` `countMessages`): content + reasoning +
    /// a flat framing charge per message, every tool call's name and argument pairs, and
    /// [`ATTACHMENT_TOKENS`] per attachment.
    pub fn count_messages(self, msgs: &[Message]) -> u64 {
        let mut total: u64 = 0;
        for m in msgs {
            total += self.count(&m.content) + self.count(m.reasoning()) + MESSAGE_FRAMING_TOKENS;
            for tc in m.tool_calls() {
                total += self.count(&tc.name);
                for (k, v) in &tc.arguments {
                    total += self.count(k) + self.count(&go_value(v));
                }
            }
            total += u64::try_from(m.attachments.len()).unwrap_or(0) * ATTACHMENT_TOKENS;
        }
        total
    }
}

/// One JSON value as Go's `fmt %v` would print it.
///
/// Exact for the scalars tool arguments are made of — a string prints bare, a number
/// prints in its shortest form, a bool prints `true`/`false`, and JSON `null` is Go's
/// `<nil>` rather than serde's `null`. Composite values (arrays, objects) print as COMPACT
/// JSON where Go would print `[1 2]` / `map[k:v]`: the two disagree by a handful of
/// punctuation tokens on a value nobody bills, and inventing a Go-formatter twin for
/// arbitrary nesting would be a second, wronger, source of truth (spec `chat-commands`
/// §Rust-mapping explicitly leaves the rendering to the port).
pub(crate) fn go_value(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A whole argument map as Go's `fmt %v` prints one: `map[k1:v1 k2:v2]`, keys ASCENDING.
///
/// Go has sorted map keys on print since 1.12 and `serde_json::Map` is a `BTreeMap`
/// without `preserve_order`, so the two orders agree — which matters, because this text is
/// what the compaction summary shows the model about the tools a turn ran.
pub(crate) fn go_map(args: &JsonObject) -> String {
    let mut out = String::from("map[");
    for (i, (k, v)) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(k);
        out.push(':');
        out.push_str(&go_value(v));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::{TokenCounter, go_map, go_value};
    use crate::provider::model::JsonObject;
    use serde_json::{Value, json};

    /// Go: `chat/tokens_test.go:37` `TestTokenCounter` — the encoder must load OFFLINE, and
    /// the counts must be the `tiktoken-go` counts, not merely plausible ones.
    ///
    /// The goldens below were produced by running `o200k_base` through
    /// `pkoukk/tiktoken-go` v0.1.8 + `tiktoken-go-loader` v0.0.2 (`Encode(s, nil, nil)`),
    /// i.e. the counter this port replaces. They are the parity gate `TUI_WPS` asks for: an
    /// encoder that silently fell back to the byte heuristic, or a rank table that drifted,
    /// changes every one of them.
    #[test]
    fn test_token_counter() {
        let c = TokenCounter::new();
        assert!(
            c.has_encoder(),
            "the bundled o200k_base ranks failed to load"
        );
        assert_eq!(c.count(""), 0);
        let short = c.count("hello world, this is a test");
        assert!(short > 0 && short <= 20, "short sentence = {short}");

        // Go parity goldens (tiktoken-go v0.1.8, o200k_base).
        for (text, want) in [
            ("", 0),
            ("hello world, this is a test", 7),
            ("The quick brown fox jumps over the lazy dog.", 10),
            ("你好，世界！这是一个测试。", 8),
            ("def main():\n    print(\"hello\")\n", 8),
            ("🙂🚀 emoji + ünïcödé", 9),
            (
                "[Earlier conversation summary]\nsummary text\n\n———\n\n",
                10,
            ),
            ("Summarize older context to reclaim the window", 9),
            ("a\tb\tc", 3),
            ("1234567890", 4),
        ] {
            assert_eq!(c.count(text), want, "tiktoken-go parity for {text:?}");
        }
    }

    /// `countMessages` is a COMPOSITION of `count`: content + reasoning + 4 framing per
    /// message, every tool-call name and argument pair, and a flat 1200 per attachment.
    /// Pinning it against `count` rather than against a number keeps the test honest if
    /// the rank table ever moves.
    #[test]
    fn count_messages_composes_content_calls_and_attachments() {
        let c = TokenCounter::new();
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), json!("/tmp/x"));
        // Reasoning and tool calls are the assistant's; the attachment rides the same message.
        let msgs = vec![
            crate::provider::model::Message::assistant("hello world, this is a test")
                .with_reasoning("some thinking"),
            crate::provider::model::Message {
                attachments: vec![crate::provider::model::Attachment::default()],
                ..crate::provider::model::Message::assistant_with_calls(
                    "",
                    vec![crate::provider::model::ToolCall {
                        id: "1".to_owned(),
                        name: "read_file".to_owned(),
                        arguments: args,
                    }],
                    None,
                )
            },
        ];
        let want = c.count("hello world, this is a test")
            + c.count("some thinking")
            + 4
            + c.count("")
            + c.count("")
            + 4
            + c.count("read_file")
            + c.count("path")
            + c.count("/tmp/x")
            + super::ATTACHMENT_TOKENS;
        assert_eq!(c.count_messages(&msgs), want);
        assert_eq!(c.count_messages(&[]), 0);
    }

    /// The `fmt %v` twin: scalars exactly, `null` as Go's `<nil>`, maps in Go's
    /// sorted-key `map[k:v]` shape.
    #[test]
    fn go_value_matches_gos_verb_for_scalars() {
        assert_eq!(go_value(&json!("plain")), "plain");
        assert_eq!(go_value(&json!(3)), "3");
        assert_eq!(go_value(&json!(3.5)), "3.5");
        assert_eq!(go_value(&json!(true)), "true");
        assert_eq!(go_value(&Value::Null), "<nil>");

        let mut args = JsonObject::new();
        args.insert("zeta".to_owned(), json!(1));
        args.insert("alpha".to_owned(), json!("x"));
        assert_eq!(go_map(&args), "map[alpha:x zeta:1]");
        assert_eq!(go_map(&JsonObject::new()), "map[]");
    }
}
