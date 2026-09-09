//! `<think>` tag splitter tests (`provider/thinktag_test.go`) plus the `ReasoningGate` adapter.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::sink::ReasoningGate;
use iota::provider::think::{StreamThinkSplitter, ThinkOut, ThinkTagSplitter, split_inline_think};
use iota::testing::{RecordingSink, SinkEvent};

#[derive(Default)]
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

/// thinktag_test.go:8-19.
fn feed_splitter(deltas: &[&str]) -> (String, String) {
    let mut sp = ThinkTagSplitter::default();
    let mut out = Collect::default();
    for d in deltas {
        sp.write(d, &mut out);
    }
    sp.flush(&mut out);
    (out.content, out.think)
}

// Go: provider/thinktag_test.go:21
#[test]
fn test_think_tag_splitter() {
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("no tags", &["hello ", "world"], "hello world", ""),
        (
            "single delta",
            &["<think>reason</think>answer"],
            "answer",
            "reason",
        ),
        (
            "tags split across deltas",
            &["<th", "ink>rea", "son</th", "ink>ans", "wer"],
            "answer",
            "reason",
        ),
        (
            "leading whitespace before tag",
            &["\n <think>r</think>c"],
            "c",
            "r",
        ),
        (
            "unclosed block is all reasoning",
            &["<think>only thoughts"],
            "",
            "only thoughts",
        ),
        (
            "dangling close prefix stays think",
            &["<think>x</thi"],
            "",
            "x</thi",
        ),
        (
            "template padding after close trimmed",
            &["<think>r</think>", "\n\n", "answer"],
            "answer",
            "r",
        ),
        (
            "literal tag mid-content passes through",
            &["see the <think> tag"],
            "see the <think> tag",
            "",
        ),
        (
            "close without open passes through",
            &["plain</think>text"],
            "plain</think>text",
            "",
        ),
        (
            "open prefix that mismatches flushes raw",
            &["<th", "ing else"],
            "<thing else",
            "",
        ),
        (
            "second block after close passes through",
            &["<think>r</think>a<think>b</think>"],
            "a<think>b</think>",
            "r",
        ),
        ("whitespace-only stream", &["  "], "  ", ""),
        ("empty stream", &[], "", ""),
        ("partial open at EOF flushes raw", &["<thi"], "<thi", ""),
    ];
    assert_eq!(cases.len(), 14);
    for (name, deltas, want_content, want_think) in cases {
        let (content, think) = feed_splitter(deltas);
        assert_eq!(
            (content.as_str(), think.as_str()),
            (*want_content, *want_think),
            "{name}: content={content:?} think={think:?}, want {want_content:?} / {want_think:?}"
        );
    }
}

// Go: provider/thinktag_test.go:54
#[test]
fn test_think_tag_splitter_byte_at_a_time() {
    let input = "\n<think>deep\nthought</think>\n\nThe <think> tag explained.";
    let deltas: Vec<&str> = (0..input.len()).map(|i| &input[i..=i]).collect();
    let (content, think) = feed_splitter(&deltas);
    assert_eq!(think, "deep\nthought");
    assert_eq!(content, "The <think> tag explained.");

    // Whole-string feeding resolves identically.
    assert_eq!(feed_splitter(&[input]), (content, think));
}

// Go: provider/thinktag_test.go:69
#[test]
fn test_split_inline_think() {
    let (c, th) = split_inline_think("<think>pondering</think>done");
    assert_eq!((c.as_str(), th.as_str()), ("done", "pondering"));
    let (c, th) = split_inline_think("no tags here");
    assert_eq!((c.as_str(), th.as_str()), ("no tags here", ""));
}

/// thinktag.go:163-175: think text reaches the reasoning channel, the first visible write closes reasoning
/// BEFORE the content, held-back bytes never close it prematurely, and both sides accumulate.
#[test]
fn stream_think_splitter_closes_reasoning_before_content() {
    let mut sink = RecordingSink::default();
    let mut sp = StreamThinkSplitter::new();
    {
        let mut gate = ReasoningGate::new(&mut sink);
        for d in ["<th", "ink>pond", "ering</thi", "nk>", "\n\n", "hel", "lo"] {
            sp.write(d, &mut gate);
        }
        assert!(gate.is_closed(), "the first content write closes reasoning");
        sp.flush(&mut gate);
    }
    assert_eq!(
        sink.events,
        vec![
            SinkEvent::Reasoning("pond".into()),
            SinkEvent::Reasoning("ering".into()),
            SinkEvent::ReasoningDone,
            SinkEvent::Content("hel".into()),
            SinkEvent::Content("lo".into()),
        ]
    );
    assert_eq!(sp.content, "hello");
    assert_eq!(sp.think, "pondering");

    // An unclosed block never emits content, so the gate closes only on drop.
    let mut sink = RecordingSink::default();
    let mut sp = StreamThinkSplitter::new();
    {
        let mut gate = ReasoningGate::new(&mut sink);
        sp.write("<think>need a tool</thi", &mut gate);
        assert!(!gate.is_closed());
        sp.flush(&mut gate);
        assert!(!gate.is_closed());
    }
    assert_eq!(
        sink.events,
        vec![
            SinkEvent::Reasoning("need a tool".into()),
            SinkEvent::Reasoning("</thi".into()),
            SinkEvent::ReasoningDone,
        ]
    );
    assert_eq!(sp.content, "");
    assert_eq!(sp.think, "need a tool</thi");
}
