//! The JSON run report (chat/output.go): wire structs in Go key order, the per-run `RunRecorder`, and the
//! delegated-cost section derived from the `DelegationLedger`.

use std::time::Instant;

use crate::chat::turns::DelegationLedger;
use crate::provider::ProviderKind;
use crate::provider::model::ToolCall;
use crate::provider::usage::Usage;
use serde::Serialize;

/// Token usage on the wire (every field always present, Go key order).
#[allow(clippy::struct_field_names)] // the `_tokens` suffixes ARE the frozen JSON keys (chat/output.go)
#[derive(Serialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// `input_tokens`.
    pub input_tokens: u64,
    /// `output_tokens`.
    pub output_tokens: u64,
    /// `cache_read_tokens`.
    pub cache_read_tokens: u64,
    /// `cache_write_tokens`.
    pub cache_write_tokens: u64,
    /// `total_tokens`.
    pub total_tokens: u64,
}

impl From<Usage> for TokenUsage {
    /// `tokenUsage(u)` (output.go:72-80).
    fn from(u: Usage) -> Self {
        Self {
            input_tokens: u.input,
            output_tokens: u.output,
            cache_read_tokens: u.cache_read,
            cache_write_tokens: u.cache_write,
            total_tokens: u.total,
        }
    }
}

impl TokenUsage {
    /// Field-wise sum (INCLUDING the total, so an absent total stays absent) — output.go:82-88.
    pub fn add(&mut self, u: Usage) {
        self.input_tokens += u.input;
        self.output_tokens += u.output;
        self.cache_read_tokens += u.cache_read;
        self.cache_write_tokens += u.cache_write;
        self.total_tokens += u.total;
    }

    /// Back to the core accounting struct (output.go:160-168).
    pub fn to_usage(self) -> Usage {
        Usage {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_read: self.cache_read_tokens,
            cache_write: self.cache_write_tokens,
            total: self.total_tokens,
        }
    }
}

/// One API round of the run.
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// 1-based round number.
    pub round: u32,
    /// Tool names the round requested (omitted when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Usage of the round (zero when the provider reported nothing).
    pub usage: TokenUsage,
}

/// What everything the run delegated cost, in aggregate.
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegatedReport {
    /// Rounds run by delegated children.
    pub rounds: u32,
    /// Their aggregate usage.
    pub usage: TokenUsage,
}

/// The `--output-format json` document (chat/output.go, key order preserved).
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct RunReport {
    /// Always `"result"`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Provider type string.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// The final reply (`""` on failure).
    pub reply: String,
    /// The error text (omitted when empty).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Rounds this run made itself.
    pub rounds: u32,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Aggregate usage of this run's own rounds.
    pub usage: TokenUsage,
    /// Delegated cost (omitted when nothing was delegated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegated: Option<DelegatedReport>,
    /// Per-round usage (omitted when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub round_usage: Vec<RoundReport>,
    /// Saved image paths (omitted when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Image save failures (omitted when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub image_errors: Vec<String>,
}

/// `serde_json::to_writer_pretty` (2-space indent, no HTML escaping — Go's `SetIndent("", "  ")` +
/// `SetEscapeHTML(false)`) followed by the trailing `"\n"` `Encoder.Encode` appends (output.go:197-202).
pub fn write_report(w: &mut dyn std::io::Write, r: &RunReport) -> std::io::Result<()> {
    serde_json::to_writer_pretty(&mut *w, r)?;
    w.write_all(b"\n")
}

/// Per-run accounting: start time, one `RoundReport` per observed round, and the running total.
pub struct RunRecorder {
    started: Instant,
    rounds: Vec<RoundReport>,
    total: TokenUsage,
}

impl Default for RunRecorder {
    fn default() -> Self {
        Self::start()
    }
}

impl RunRecorder {
    /// Starts the clock with no rounds (`newRunRecorder`, output.go:140).
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            rounds: Vec::new(),
            total: TokenUsage::default(),
        }
    }

    /// `RoundReport { round: len+1, tools, usage: usage.map_or(zero) }`; total += usage (output.go:147-156). A
    /// call that reported no usage still contributes a zero-usage round so the round COUNT stays truthful.
    pub fn observe(&mut self, usage: Option<Usage>, tools: Vec<String>) {
        let round = u32::try_from(self.rounds.len() + 1).unwrap_or(u32::MAX);
        let mut rr = RoundReport {
            round,
            tools,
            usage: TokenUsage::default(),
        };
        if let Some(u) = usage {
            rr.usage = TokenUsage::from(u);
            self.total.add(u);
        }
        self.rounds.push(rr);
    }

    /// The rounds observed so far.
    pub fn rounds(&self) -> &[RoundReport] {
        &self.rounds
    }

    /// Number of rounds observed.
    pub fn round_count(&self) -> u32 {
        u32::try_from(self.rounds.len()).unwrap_or(u32::MAX)
    }

    /// The running total as core `Usage` (output.go:160-168).
    pub fn usage(&self) -> Usage {
        self.total.to_usage()
    }

    /// Assembles the report: `kind` "result", duration since `start`, the rounds, and `err`'s Display (or "")
    /// (output.go:173-191).
    #[allow(clippy::too_many_arguments)] // frozen contract signature (CONTRACTS §6.5)
    pub fn report(
        &self,
        kind: ProviderKind,
        model: &str,
        reply: &str,
        images: Vec<String>,
        image_errors: Vec<String>,
        delegated: Option<DelegatedReport>,
        err: Option<&dyn std::fmt::Display>,
    ) -> RunReport {
        RunReport {
            kind: "result",
            provider: kind.as_str().to_owned(),
            model: model.to_owned(),
            reply: reply.to_owned(),
            error: err.map(ToString::to_string).unwrap_or_default(),
            rounds: self.round_count(),
            duration_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            usage: self.total,
            delegated,
            round_usage: self.rounds.clone(),
            images,
            image_errors,
        }
    }
}

/// The names of the calls, in call order and with repeats kept (output.go:206-215).
pub(crate) fn tool_names(calls: &[ToolCall]) -> Vec<String> {
    calls.iter().map(|tc| tc.name.clone()).collect()
}

/// `ledger.snapshot()` mapped to the report section; `None` when nothing was delegated (turns.go:117-127).
pub fn delegated_report(ledger: &DelegationLedger) -> Option<DelegatedReport> {
    ledger.snapshot().map(|(rounds, usage)| DelegatedReport {
        rounds,
        usage: TokenUsage::from(usage),
    })
}

#[cfg(test)]
mod tests {
    use crate::chat::turns::DelegationLedger;
    use crate::provider::ProviderKind;
    use crate::provider::model::ToolCall;
    use crate::provider::usage::Usage;

    use super::{RunRecorder, TokenUsage, delegated_report, tool_names, write_report};

    #[test]
    fn recorder_counts_usage_less_rounds() {
        let mut rec = RunRecorder::start();
        rec.observe(None, vec!["a".to_owned(), "a".to_owned()]);
        rec.observe(
            Some(Usage {
                input: 5,
                output: 1,
                total: 6,
                ..Usage::default()
            }),
            vec![],
        );
        assert_eq!(rec.round_count(), 2);
        assert_eq!(rec.rounds()[0].round, 1);
        assert_eq!(rec.rounds()[0].tools, ["a", "a"]);
        assert_eq!(rec.rounds()[0].usage, TokenUsage::default());
        assert_eq!(rec.rounds()[1].round, 2);
        assert_eq!(rec.rounds()[1].usage.total_tokens, 6);
        assert_eq!(rec.usage().input, 5);
        let rep = rec.report(
            ProviderKind::Anthropic,
            "m",
            "r",
            vec!["/p".to_owned()],
            vec![],
            None,
            Some(&"bad"),
        );
        assert_eq!(rep.kind, "result");
        assert_eq!(rep.provider, "anthropic");
        assert_eq!(rep.model, "m");
        assert_eq!(rep.error, "bad");
        assert_eq!(rep.rounds, 2);
        assert_eq!(rep.images, ["/p"]);
        assert!(rep.delegated.is_none());
    }

    #[test]
    fn report_json_shape_and_order() {
        let mut rec = RunRecorder::start();
        rec.observe(
            Some(Usage {
                input: 1,
                ..Usage::default()
            }),
            vec!["noop".to_owned()],
        );
        let ledger = DelegationLedger::default();
        ledger.add(
            2,
            Usage {
                input: 3,
                ..Usage::default()
            },
        );
        let rep = rec.report(
            ProviderKind::OpenAi,
            "gpt",
            "<b>&",
            vec![],
            vec!["saving image failed: x".to_owned()],
            delegated_report(&ledger),
            None,
        );
        let mut out = Vec::new();
        write_report(&mut out, &rep).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(text.starts_with("{\n  \"type\": \"result\",\n  \"provider\": \"openai\","));
        // HTML is not escaped; keys follow Go struct order.
        assert!(text.contains("\"reply\": \"<b>&\""));
        let keys: Vec<usize> = [
            "\"type\"",
            "\"provider\"",
            "\"model\"",
            "\"reply\"",
            "\"rounds\"",
            "\"duration_ms\"",
            "\"usage\"",
            "\"delegated\"",
            "\"round_usage\"",
            "\"image_errors\"",
        ]
        .iter()
        .map(|k| text.find(k).unwrap())
        .collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "key order: {text}");
        assert!(!text.contains("\"error\""));
        assert!(!text.contains("\"images\""));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["delegated"]["rounds"], 2);
        assert_eq!(v["delegated"]["usage"]["input_tokens"], 3);
        assert_eq!(v["round_usage"][0]["tools"][0], "noop");
    }

    #[test]
    fn tool_names_keep_order_and_repeats() {
        let call = |n: &str| ToolCall {
            name: n.to_owned(),
            ..ToolCall::default()
        };
        assert_eq!(
            tool_names(&[call("b"), call("a"), call("b")]),
            ["b", "a", "b"]
        );
        assert!(tool_names(&[]).is_empty());
        assert!(delegated_report(&DelegationLedger::default()).is_none());
    }
}
