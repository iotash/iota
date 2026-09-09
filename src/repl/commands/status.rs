//! `/status` — the capability-gated readout (chat/status.go:42-135, chat/run.go:938-955).
//!
//! The rows are assembled by CAPABILITY, never as a fixed list of maybe-blank fields: a
//! provider without tuning shows no Temperature row rather than a row saying "default"
//! about a knob it does not have. The name column is padded to the widest label ACTUALLY
//! PRESENT, by display width — the labels vary by provider, and a fixed width breaks
//! alignment the moment one of them outgrows it (and CJK would break a byte-width one).
//!
//! The Context / Token count / Last turn / Session input+output+cache rows exist only
//! while token accounting is live — the same single gate `/compact`'s registration reads
//! (Go asserts `provider.UsageReporter`). A provider without usage reporting therefore
//! renders Go's token-less-provider shape (T-10).

use std::fmt::Write as _;

use crate::provider::usage::Usage;
use crate::text;
use crate::text::width::str_width;
use crate::ui::facade::ViewSpec;

use crate::repl::run::Repl;
use crate::repl::styles::{bold, truncate_runes};

/// One labelled row: `name` is the bold left column, `value` the right one.
pub(crate) struct StatusItem {
    pub(crate) name: String,
    pub(crate) value: String,
}

fn item(name: &str, value: impl Into<String>) -> StatusItem {
    StatusItem {
        name: name.to_owned(),
        value: value.into(),
    }
}

/// An image provider's generation defaults (chat/status.go:14-29 `imageGenLabel`).
///
/// `pub(crate)`: `/model`'s image-params notice renders the SAME label `/status` shows,
/// and a second copy of the join rule would be a second source of truth (WP54).
pub(crate) fn image_gen_label(g: &crate::provider::ImageGenParams) -> String {
    let mut parts = Vec::new();
    if let Some(aspect) = &g.aspect_ratio {
        parts.push(format!("aspect {aspect}"));
    }
    if let Some(size) = &g.image_size {
        parts.push(format!("size {size}"));
    }
    if let Some(negative) = &g.negative_prompt {
        parts.push(format!("negative {}", truncate_runes(negative, 24)));
    }
    if parts.is_empty() {
        return "default".to_owned();
    }
    parts.join(" · ")
}

/// The token half of the readout, present only while accounting is live (Go's
/// `provider.UsageReporter` assertion).
pub(crate) struct TokenStatus {
    /// Tokens the next request is projected to carry.
    pub(crate) used: u64,
    /// The context window.
    pub(crate) window: u64,
    /// Whether `used` is the provider's figure rather than a local estimate.
    pub(crate) have_usage: bool,
    /// Cumulative session usage.
    pub(crate) totals: Usage,
    /// The last API call's usage (Go `LastUsageFull`); `None` before the first one.
    pub(crate) last: Option<Usage>,
}

/// The `/status` rows for the live state (chat/status.go `statusLines`).
///
/// Plain arguments rather than the loop's state, so the capability gating is unit-testable
/// against a bare provider: `mcp` is `Some((connected, configured))` only where the binary
/// wired the hook, `tokens` is `Some` only while accounting is live, and `session_id` is
/// empty while the chat is ephemeral.
pub(crate) fn status_lines(
    provider: &mut dyn crate::provider::Provider,
    messages: usize,
    pending: usize,
    tools: usize,
    mcp: Option<(usize, usize)>,
    tokens: Option<&TokenStatus>,
    session_id: &str,
) -> Vec<StatusItem> {
    let provider_type = provider.kind().as_str().to_owned();
    let model = provider.model().to_owned();
    let mcp = match mcp {
        Some((connected, total)) if total > 0 => format!("{connected}/{total} servers connected"),
        _ => "none configured".to_owned(),
    };

    let mut items = vec![
        item("Provider", provider_type),
        item(
            "Model",
            if model.is_empty() {
                "(not selected)".to_owned()
            } else {
                model
            },
        ),
    ];
    // Tuning knobs when the provider exposes them; "default" means the parameter is
    // omitted from requests entirely.
    if let Some(t) = provider.as_tunable() {
        // Go formats with `strconv.FormatFloat(v,'f',-1,64)`; `go_float` is the 'g' twin,
        // which agrees on every value a 0.1-step slider can produce.
        let temperature = t
            .temperature()
            .map_or_else(|| "default".to_owned(), crate::text::go_float);
        let effort = t
            .effort()
            .map_or("default", crate::provider::Effort::as_str)
            .to_owned();
        items.push(item("Temperature", temperature));
        items.push(item("Effort", effort));
    }
    // Token rows only for a provider that accounts tokens — the same idiom as the tuning
    // knobs above: the assertion IS the capability (status.go:100-119).
    if let Some(t) = tokens {
        let pct = (t.used * 100).checked_div(t.window).unwrap_or(0);
        items.push(item(
            "Context",
            format!(
                "{} / {} tokens ({pct}%)",
                text::tokens(t.used),
                text::tokens(t.window)
            ),
        ));
        items.push(item(
            "Token count",
            if t.have_usage {
                "provider-reported"
            } else {
                "estimated (local tokenizer)"
            },
        ));
        items.push(item("Last turn", last_turn_label(t.last)));
        items.push(item(
            "Session input",
            format!("{} tokens", text::tokens(t.totals.input)),
        ));
        items.push(item(
            "Session output",
            format!("{} tokens", text::tokens(t.totals.output)),
        ));
        // Cache rows only where caching actually happened: a provider that reports none
        // shows nothing rather than a row of zeros.
        if t.totals.cached() {
            items.push(item(
                "Session cache",
                format!(
                    "{} read, {} written ({:.1}% of input)",
                    text::tokens(t.totals.cache_read),
                    text::tokens(t.totals.cache_write),
                    t.totals.cache_hit_rate()
                ),
            ));
        }
    }
    if let Some(g) = provider.as_image_gen_tunable() {
        let label = image_gen_label(g.image_gen_params());
        items.push(item("Image params", label));
    }
    items.push(item("Messages", format!("{messages} in context")));
    if pending > 0 {
        items.push(item("Attachments", format!("{pending} pending")));
    }
    items.push(item(
        "Tools",
        if tools == 0 {
            "none".to_owned()
        } else {
            format!("{tools} available")
        },
    ));
    items.push(item("MCP", mcp));
    items.push(item(
        "Session",
        if session_id.is_empty() {
            "not saved (ephemeral)"
        } else {
            session_id
        },
    ));
    items
}

/// The "Last turn" value (chat/status.go:56-64): the last call's input/output, plus the
/// cache figures when that call cached anything. `"(none yet)"` before the first call.
fn last_turn_label(last: Option<Usage>) -> String {
    let Some(u) = last else {
        return "(none yet)".to_owned();
    };
    let mut s = format!(
        "input {}, output {}",
        text::tokens(u.input),
        text::tokens(u.output)
    );
    if u.cached() {
        let _ = write!(
            s,
            ", cache {} ({:.1}%)",
            text::tokens(u.cache_read),
            u.cache_hit_rate()
        );
    }
    s
}

/// Renders the rows into the `"Status"` viewer (chat/run.go:938-955).
pub(crate) fn status_rows(items: &[StatusItem]) -> Vec<String> {
    let width = items.iter().map(|i| str_width(&i.name)).max().unwrap_or(0);
    items
        .iter()
        .map(|i| {
            let pad = " ".repeat(width.saturating_sub(str_width(&i.name)));
            format!("{}{pad}  {}", bold(&i.name), i.value)
        })
        .collect()
}

/// `/status`: a read-only viewer, so it neither waits for the title pass nor changes
/// anything. A facade failure is a cancel (see [`super::model::cmd_model`]).
pub(crate) async fn cmd_status(repl: &mut Repl) {
    let tools = repl.dispatch.tools().len();
    let mcp = repl.mcp.servers.as_ref().map(|f| {
        let servers = f();
        (
            servers.iter().filter(|s| s.connected).count(),
            servers.len(),
        )
    });
    let messages = repl.history.len();
    let pending = repl.pending.len();
    let session_id = repl.session_id();
    // Go asserts `provider.UsageReporter`; the Rust twin is the meter's own gate, which is
    // built from exactly that capability (`provider.reports_usage()`).
    let tokens = repl.ctxm.is_enabled().then(|| TokenStatus {
        used: repl.budget.used(),
        window: repl.budget.window(),
        have_usage: repl.budget.have_usage(),
        totals: repl.ctxm.totals(),
        last: repl.budget.last_usage(),
    });
    let items = status_lines(
        &mut *repl.provider,
        messages,
        pending,
        tools,
        mcp,
        tokens.as_ref(),
        &session_id,
    );
    let lines = status_rows(&items);
    let _ = repl
        .ui
        .view(
            &repl.cancel,
            ViewSpec {
                title: "Status".to_owned(),
                lines,
                height: 0,
            },
        )
        .await;
}

#[cfg(test)]
mod tests {
    use crate::testing::FakeToolProvider;
    use pretty_assertions::assert_eq;

    use super::{StatusItem, TokenStatus, Usage, status_lines, status_rows};

    fn names(items: &[StatusItem]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    fn value<'a>(items: &'a [StatusItem], name: &str) -> &'a str {
        items
            .iter()
            .find(|i| i.name == name)
            .map_or("", |i| i.value.as_str())
    }

    // Go: chat/status.go:42-135 — rows exist by CAPABILITY, never as a row of zeros: a
    // provider with no tuning shows no Temperature row, an ephemeral chat says so, an
    // empty attachment set contributes no row at all, and a provider that accounts no
    // tokens shows the token-LESS shape (T-10).
    #[test]
    fn status_rows_are_capability_gated() {
        let mut p = FakeToolProvider::looping(0, 1);
        let items = status_lines(&mut p, 4, 0, 2, None, None, "");
        assert_eq!(
            names(&items),
            ["Provider", "Model", "Messages", "Tools", "MCP", "Session"]
        );
        assert_eq!(value(&items, "Provider"), "openai");
        assert_eq!(value(&items, "Model"), "gpt-test");
        assert_eq!(value(&items, "Messages"), "4 in context");
        assert_eq!(value(&items, "Tools"), "2 available");
        assert_eq!(value(&items, "MCP"), "none configured");
        assert_eq!(value(&items, "Session"), "not saved (ephemeral)");

        // Pending attachments add a row; no tools and no servers degrade to words.
        let items = status_lines(&mut p, 0, 3, 0, Some((1, 2)), None, "k7qz3xv9m2ht");
        assert_eq!(
            names(&items),
            [
                "Provider",
                "Model",
                "Messages",
                "Attachments",
                "Tools",
                "MCP",
                "Session"
            ]
        );
        assert_eq!(value(&items, "Attachments"), "3 pending");
        assert_eq!(value(&items, "Tools"), "none");
        assert_eq!(value(&items, "MCP"), "1/2 servers connected");
        assert_eq!(value(&items, "Session"), "k7qz3xv9m2ht");
        // A configured-but-empty server set is still "none configured".
        let items = status_lines(&mut p, 0, 0, 0, Some((0, 0)), None, "");
        assert_eq!(value(&items, "MCP"), "none configured");
    }

    /// A token-accounting provider gains the whole token block, in Go's order and byte
    /// shape: Context with its percentage, the estimate/measured source, the last call's
    /// figures, and the session totals. The cache rows appear only where caching actually
    /// happened — a provider that reports none shows no row of zeros.
    // Go: chat/status.go:100-119 (the `provider.UsageReporter` branch)
    #[test]
    fn token_rows_render_go_shapes_and_gate_the_cache_row() {
        let mut p = FakeToolProvider::looping(0, 1);
        let bare = TokenStatus {
            used: 64_000,
            window: 128_000,
            have_usage: true,
            totals: Usage {
                input: 12_345,
                output: 678,
                ..Usage::default()
            },
            last: None,
        };
        let items = status_lines(&mut p, 2, 0, 1, None, Some(&bare), "");
        assert_eq!(
            names(&items),
            [
                "Provider",
                "Model",
                "Context",
                "Token count",
                "Last turn",
                "Session input",
                "Session output",
                "Messages",
                "Tools",
                "MCP",
                "Session"
            ]
        );
        assert_eq!(value(&items, "Context"), "64k / 128k tokens (50%)");
        assert_eq!(value(&items, "Token count"), "provider-reported");
        assert_eq!(value(&items, "Last turn"), "(none yet)");
        assert_eq!(value(&items, "Session input"), "12.3k tokens");
        assert_eq!(value(&items, "Session output"), "678 tokens");

        // A local estimate says so, and a cached session gains exactly one more row.
        let cached = TokenStatus {
            used: 1_000,
            window: 0,
            have_usage: false,
            totals: Usage {
                input: 1_000,
                output: 100,
                cache_read: 400,
                cache_write: 200,
                ..Usage::default()
            },
            last: Some(Usage {
                input: 300,
                output: 50,
                cache_read: 150,
                ..Usage::default()
            }),
        };
        let items = status_lines(&mut p, 2, 0, 1, None, Some(&cached), "");
        assert_eq!(value(&items, "Context"), "1k / 0 tokens (0%)");
        assert_eq!(value(&items, "Token count"), "estimated (local tokenizer)");
        assert_eq!(
            value(&items, "Last turn"),
            "input 300, output 50, cache 150 (33.3%)"
        );
        assert_eq!(
            value(&items, "Session cache"),
            "400 read, 200 written (25.0% of input)"
        );
    }

    /// The name column pads by DISPLAY width, so a CJK label (which no Go provider has, but
    /// a translated one would) does not shear the value column (chat/run.go:940-952).
    #[test]
    fn status_rows_pad_by_display_width() {
        let items = vec![
            StatusItem {
                name: "模型".to_owned(), // 4 columns, 6 bytes
                value: "gpt-4o".to_owned(),
            },
            StatusItem {
                name: "Tools".to_owned(), // 5 columns
                value: "none".to_owned(),
            },
        ];
        let rows = status_rows(&items);
        let plain: Vec<String> = rows
            .iter()
            .map(|r| crate::text::ansi::strip_sgr(r))
            .collect();
        // "模型" is 4 columns wide in 6 bytes: a byte-width pad would shear the value
        // column by two.
        assert_eq!(plain, ["模型   gpt-4o", "Tools  none"]);
        for (row, value) in plain.iter().zip(["gpt-4o", "none"]) {
            let at = row.rfind(value).expect("the value column");
            assert_eq!(
                crate::text::width::str_width(&row[..at]),
                7,
                "every value column starts at the same display column: {row:?}"
            );
        }
    }
}
