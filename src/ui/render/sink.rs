//! The turn's stream sink — a port of internal/ui/sink.go's `streamSink`: the
//! [`crate::ui::facade::UiStreamSink`] implementation. A buffering markdown block no longer has a
//! preview of its own (tui-plan S7b): it is a live block in the conversation that grows line by
//! line, so all this sink still does is end the turn — drop a call preview it leaked and pop the
//! turn's cancel scope.

use std::sync::{Arc, Mutex};

use crate::sync::lock;
use crate::ui::facade::UiStreamSink;

use super::region::Region;

/// The [`UiStreamSink`] implementation (sink.go `streamSink`).
pub(crate) struct StreamSink {
    region: Arc<Mutex<Region>>,
    /// Pops the turn cancel scope (Go `Send(scopePopMsg{})`, sink.go:26).
    on_done: Box<dyn Fn() + Send + Sync>,
}

impl StreamSink {
    /// A sink over the shared region; `on_done` runs once per [`UiStreamSink::done`]
    /// after the leaked/deferred preview is dropped.
    pub(crate) fn new(
        region: Arc<Mutex<Region>>,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            region,
            on_done: Box::new(on_done),
        }
    }
}

impl UiStreamSink for StreamSink {
    fn done(&self) {
        lock(&self.region).drop_preview(); // a leaked/deferred preview dies with the turn
        (self.on_done)();
    }
}
