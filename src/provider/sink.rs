//! Streaming sink contract (provider.go:136-137): `StreamSink`, the headless `NullSink`, and `ReasoningGate`,
//! which enforces the close-before-first-content rule.

/// Receives streamed deltas from a provider dialect.
pub trait StreamSink: Send {
    /// A visible-content delta.
    fn content(&mut self, delta: &str);
    /// A reasoning (thinking) delta.
    fn reasoning(&mut self, delta: &str);
    /// Reasoning is finished. Must be idempotent for callers that bypass `ReasoningGate`.
    fn reasoning_done(&mut self);
    /// Tool-argument delta observed while composing. `name` None = anonymous (never raise a
    /// widget on it — the zombie-spinner rule). The openai, anthropic and openresponses
    /// dialects emit it as tool arguments stream in; google does not.
    fn tool_delta(&mut self, _name: Option<&str>, _delta: &str) {}
    /// A progressive image frame (decoded bytes) from a streaming image generation
    /// (openresponses `partial_image`; chat/images.go:187 `watchImagePartials`). Default no-op.
    fn image_partial(&mut self, _frame: &[u8]) {}
}

/// Headless sink: discards everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl StreamSink for NullSink {
    fn content(&mut self, _delta: &str) {}

    fn reasoning(&mut self, _delta: &str) {}

    fn reasoning_done(&mut self) {}
}

/// Wraps a sink so `reasoning_done` fires at most once; `content()` closes first; `Drop` closes (Go's
/// `defer closeReasoning()`).
pub struct ReasoningGate<'a> {
    sink: &'a mut dyn StreamSink,
    closed: bool,
}

impl<'a> ReasoningGate<'a> {
    /// Wraps `sink` with reasoning open.
    pub fn new(sink: &'a mut dyn StreamSink) -> Self {
        Self {
            sink,
            closed: false,
        }
    }

    /// Forwards a reasoning delta.
    pub fn reasoning(&mut self, s: &str) {
        self.sink.reasoning(s);
    }

    /// `close()` then forward the content delta.
    pub fn content(&mut self, s: &str) {
        self.close();
        self.sink.content(s);
    }

    /// Forwards a tool-argument delta (`StreamSink::tool_delta`) without disturbing the
    /// reasoning state. A dialect emitting composing deltas holds the gate for the whole
    /// stream loop, so this forwarder is how it reaches the sink at all — the same shape
    /// as [`Self::reasoning`] (DEVIATIONS3 `[WP55]`).
    pub fn tool_delta(&mut self, name: Option<&str>, delta: &str) {
        self.sink.tool_delta(name, delta);
    }

    /// Forwards a progressive image frame (`StreamSink::image_partial`) without touching the
    /// reasoning state — the same shape as [`Self::tool_delta`].
    pub fn image_partial(&mut self, frame: &[u8]) {
        self.sink.image_partial(frame);
    }

    /// Fires `reasoning_done` the first time only (idempotent).
    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.sink.reasoning_done();
        }
    }

    /// Whether `close()` has fired.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

impl Drop for ReasoningGate<'_> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::{NullSink, ReasoningGate, StreamSink};

    /// A minimal event log (the feature-gated `testing::RecordingSink` is not available to every test build).
    #[derive(Default)]
    struct Log(Vec<String>);

    impl StreamSink for Log {
        fn content(&mut self, delta: &str) {
            self.0.push(format!("c:{delta}"));
        }

        fn reasoning(&mut self, delta: &str) {
            self.0.push(format!("r:{delta}"));
        }

        fn reasoning_done(&mut self) {
            self.0.push("done".to_owned());
        }
    }

    #[test]
    fn reasoning_gate_closes_once_and_on_drop() {
        let mut log = Log::default();
        {
            let mut gate = ReasoningGate::new(&mut log);
            assert!(!gate.is_closed());
            gate.reasoning("a");
            gate.close();
            assert!(gate.is_closed());
            gate.close();
            gate.reasoning("b"); // late reasoning still forwards; the gate stays closed
            assert!(gate.is_closed());
        }
        assert_eq!(log.0, ["r:a", "done", "r:b"]);

        // Never closed explicitly: Drop closes exactly once (Go's `defer closeReasoning()`).
        let mut log = Log::default();
        {
            let mut gate = ReasoningGate::new(&mut log);
            gate.reasoning("x");
        }
        assert_eq!(log.0, ["r:x", "done"]);

        // No traffic at all still closes on drop.
        let mut log = Log::default();
        drop(ReasoningGate::new(&mut log));
        assert_eq!(log.0, ["done"]);

        // The headless sink accepts everything silently.
        let mut null = NullSink;
        let mut gate = ReasoningGate::new(&mut null);
        gate.reasoning("r");
        gate.content("c");
        gate.close();
    }

    #[test]
    fn reasoning_gate_content_closes_first() {
        let mut log = Log::default();
        {
            let mut gate = ReasoningGate::new(&mut log);
            gate.reasoning("think");
            gate.content("hello");
            assert!(gate.is_closed());
            gate.content(" world");
        }
        assert_eq!(log.0, ["r:think", "done", "c:hello", "c: world"]);
    }
}
