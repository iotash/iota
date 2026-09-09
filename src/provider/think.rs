//! `<think>` tag splitter (provider/thinktag.go, byte-exact): the streaming state machine, the unary helper and
//! the `ReasoningGate` adapter.

use crate::provider::sink::ReasoningGate;

/// The opening tag.
pub(crate) const THINK_OPEN: &str = "<think>";
/// The closing tag.
pub(crate) const THINK_CLOSE: &str = "</think>";
/// Whitespace trimmed while sniffing and after the closing tag.
pub(crate) const WS_CUTSET: &[char] = &[' ', '\t', '\r', '\n'];

/// Receiver of the split output.
pub trait ThinkOut {
    /// A think-text chunk.
    fn think(&mut self, s: &str);
    /// A visible-content chunk.
    fn content(&mut self, s: &str);
}

/// State machine exactly thinktag.go:44-134 (sniff → `feed_think` / `feed_content`; `flush` mandatory at end of stream).
///
/// Only a block that opens the stream (after optional whitespace) counts as reasoning — once the opening bytes
/// mismatch, everything passes through verbatim for the rest of the round. An unclosed block is implicitly all
/// reasoning. Tags may be split across deltas, so partial matches are held back until resolved. Byte slicing is
/// safe because every boundary lies inside the ASCII tag.
#[derive(Debug, Default)]
pub struct ThinkTagSplitter {
    /// Left the sniffing state: the opening-tag question is settled.
    started: bool,
    /// Inside the think block, routing to think until `</think>`.
    in_think: bool,
    /// Just closed the block: trim the template's padding once.
    after_tag: bool,
    /// Sniff: raw prefix still matching `<think>`; think: suffix that prefixes `</think>`.
    held: String,
}

impl ThinkTagSplitter {
    /// `""` is a no-op; `!started` → sniff; `in_think` → `feed_think`; else `feed_content`.
    pub fn write(&mut self, s: &str, out: &mut impl ThinkOut) {
        if s.is_empty() {
            return;
        }
        if !self.started {
            self.sniff(s, out);
        } else if self.in_think {
            self.feed_think(s, out);
        } else {
            self.feed_content(s, out);
        }
    }

    /// Mandatory at end of stream: held text in think → `think`; else `started = true`, held → `content`.
    pub fn flush(&mut self, out: &mut impl ThinkOut) {
        let held = std::mem::take(&mut self.held);
        if held.is_empty() {
            return;
        }
        if self.in_think {
            // A dangling prefix of </think> that never completed is think text.
            out.think(&held);
            return;
        }
        // Stream ended while it still looked like an opening tag: it wasn't one.
        self.started = true;
        out.content(&held);
    }

    /// Decides whether the stream opens with `<think>`, holding bytes until the question is settled either way.
    fn sniff(&mut self, s: &str, out: &mut impl ThinkOut) {
        self.held.push_str(s);
        let trimmed = self.held.trim_start_matches(WS_CUTSET);
        if trimmed.is_empty() {
            return; // pure whitespace so far, keep waiting
        }
        if let Some(rest) = trimmed.strip_prefix(THINK_OPEN) {
            let rest = rest.to_owned();
            self.held = String::new();
            self.started = true;
            self.in_think = true;
            if !rest.is_empty() {
                self.feed_think(&rest, out);
            }
            return;
        }
        if trimmed.len() < THINK_OPEN.len() && THINK_OPEN.starts_with(trimmed) {
            return; // still a viable tag prefix, keep holding
        }
        let held = std::mem::take(&mut self.held);
        self.started = true;
        out.content(&held);
    }

    /// Routes text to the think channel until `</think>` completes.
    fn feed_think(&mut self, delta: &str, out: &mut impl ThinkOut) {
        let mut s = std::mem::take(&mut self.held);
        s.push_str(delta);
        if let Some(i) = s.find(THINK_CLOSE) {
            if i > 0 {
                out.think(&s[..i]);
            }
            self.in_think = false;
            self.after_tag = true;
            let rest = &s[i + THINK_CLOSE.len()..];
            if !rest.is_empty() {
                self.feed_content(rest, out);
            }
            return;
        }
        let k = longest_suffix_prefix(&s, THINK_CLOSE);
        let (emit, hold) = s.split_at(s.len() - k);
        hold.clone_into(&mut self.held);
        if !emit.is_empty() {
            out.think(emit);
        }
    }

    /// Visible text; swallows the template padding right after `</think>` (possibly across several deltas).
    fn feed_content(&mut self, s: &str, out: &mut impl ThinkOut) {
        let mut s = s;
        if self.after_tag {
            s = s.trim_start_matches(WS_CUTSET);
            if s.is_empty() {
                return; // templates pad </think> with blank lines; swallow them
            }
            self.after_tag = false;
        }
        out.content(s);
    }
}

/// `max = min(tag.len()-1, s.len())`; k from max down to 1; byte compare of `s`'s suffix with `tag`'s prefix.
pub(crate) fn longest_suffix_prefix(s: &str, tag: &str) -> usize {
    let (s, tag) = (s.as_bytes(), tag.as_bytes());
    let max = tag.len().saturating_sub(1).min(s.len());
    (1..=max)
        .rev()
        .find(|&k| s[s.len() - k..] == tag[..k])
        .unwrap_or(0)
}

/// Collects both channels (the unary helper's receiver).
#[derive(Debug, Default)]
struct Collect {
    content: String,
    think: String,
}

impl ThinkOut for Collect {
    fn think(&mut self, s: &str) {
        self.think.push_str(s);
    }

    fn content(&mut self, s: &str) {
        self.content.push_str(s);
    }
}

/// Unary path helper: (content, think).
pub fn split_inline_think(s: &str) -> (String, String) {
    let mut sp = ThinkTagSplitter::default();
    let mut out = Collect::default();
    sp.write(s, &mut out);
    sp.flush(&mut out);
    (out.content, out.think)
}

/// Stream adapter: think → `gate.reasoning()`; content → `gate.content()` (closes reasoning first); accumulates both.
#[derive(Debug, Default)]
pub struct StreamThinkSplitter {
    sp: ThinkTagSplitter,
    /// Every content chunk, concatenated.
    pub content: String,
    /// Every think chunk, concatenated.
    pub think: String,
}

/// The `ThinkOut` over a gate plus the two accumulators (thinktag.go:163-175).
struct GateOut<'a, 'g> {
    gate: &'a mut ReasoningGate<'g>,
    content: &'a mut String,
    think: &'a mut String,
}

impl ThinkOut for GateOut<'_, '_> {
    fn think(&mut self, s: &str) {
        self.gate.reasoning(s);
        self.think.push_str(s);
    }

    fn content(&mut self, s: &str) {
        self.gate.content(s);
        self.content.push_str(s);
    }
}

impl StreamThinkSplitter {
    /// A fresh splitter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one delta.
    pub fn write(&mut self, delta: &str, gate: &mut ReasoningGate<'_>) {
        let Self { sp, content, think } = self;
        let mut out = GateOut {
            gate,
            content,
            think,
        };
        sp.write(delta, &mut out);
    }

    /// Flushes held text at end of stream.
    pub fn flush(&mut self, gate: &mut ReasoningGate<'_>) {
        let Self { sp, content, think } = self;
        let mut out = GateOut {
            gate,
            content,
            think,
        };
        sp.flush(&mut out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_suffix_prefix_table() {
        let cases: &[(&str, &str, usize)] = &[
            ("", THINK_CLOSE, 0),
            ("abc", THINK_CLOSE, 0),
            ("abc<", THINK_CLOSE, 1),
            ("x</thi", THINK_CLOSE, 5),
            ("</think", THINK_CLOSE, 7),
            // A full tag is never a PROPER prefix: max is tag.len()-1.
            ("</think>", THINK_CLOSE, 0),
            ("<</", THINK_CLOSE, 2),
            ("<", THINK_CLOSE, 1),
            ("/", THINK_CLOSE, 0),
            ("abc<t", THINK_OPEN, 2),
            ("<th", THINK_OPEN, 3),
            ("</", THINK_OPEN, 0),
        ];
        for &(s, tag, want) in cases {
            assert_eq!(longest_suffix_prefix(s, tag), want, "{s:?} / {tag:?}");
        }
    }
}
