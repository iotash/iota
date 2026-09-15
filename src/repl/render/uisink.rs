//! `uiMDSink` (chat/uisink.go): the `crate::markdown::sink::Sink` implementation over the
//! facade — batches ALL complete lines of one write into ONE commit (the anti-crawl law),
//! materializes the transcript's latched separator before a preview opens (`preOpen`),
//! and hands the facade's `PreviewHandle` straight through to the renderer (the same trait,
//! `crate::markdown::preview`) — plus `lineCommitter` (the SGR-reset glue rule).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::repl::render::styles::dim;

/// Opens the facade's metered one-row preview for a label (the `StreamSink::block_preview`
/// verb, pre-bound by the turn).
pub(crate) type PreviewFn =
    Box<dyn FnMut(&str) -> Box<dyn crate::ui::facade::PreviewHandle> + Send>;

/// Commits one batch of rendered lines (the transcript's content-block committer).
pub(crate) type CommitFn = Box<dyn FnMut(Vec<String>) + Send>;

struct MdSinkInner {
    preview: PreviewFn,
    commit: CommitFn,
    width: Box<dyn Fn() -> usize + Send>,
    pre_open: Option<Box<dyn Fn() + Send>>,
    buf: String,
}

/// The markdown sink over the facade (chat/uisink.go `uiMDSink`). Cloneable handle: one
/// clone goes into the `markdown::Writer`, the caller keeps another to
/// [`UiMdSink::flush`] the trailing partial line after `Writer::flush`.
#[derive(Clone)]
pub(crate) struct UiMdSink(Arc<Mutex<MdSinkInner>>);

impl UiMdSink {
    /// A sink committing through `commit`, metering previews through `preview` (labels
    /// are dimmed HERE — the chat layer pre-styles markdown's plain `"rendering…"`
    /// labels, uisink.go:82), reading the live width through `width`, and materializing
    /// latched blanks through `pre_open` (nil-safe; = `Transcript::flush_pending`).
    pub(crate) fn new(
        preview: PreviewFn,
        commit: CommitFn,
        width: Box<dyn Fn() -> usize + Send>,
        pre_open: Option<Box<dyn Fn() + Send>>,
    ) -> Self {
        Self(Arc::new(Mutex::new(MdSinkInner {
            preview,
            commit,
            width,
            pre_open,
            buf: String::new(),
        })))
    }

    /// Commits a trailing partial line left in the buffer (call after
    /// `markdown::Writer::flush`).
    pub(crate) fn flush(&self) {
        let mut g = self.lock();
        if !g.buf.is_empty() {
            let tail = std::mem::take(&mut g.buf);
            (g.commit)(vec![tail]);
        }
    }

    fn lock(&self) -> MutexGuard<'_, MdSinkInner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl crate::markdown::sink::Sink for UiMdSink {
    /// Commits every complete line in the chunk as ONE batch — a single multi-line
    /// insert. Per-line commits would make a flushed block (a rendered table following
    /// its preview's frame shrink) crawl back row by row; one batched insert makes
    /// shrink+push land back to back, within a render frame.
    fn write(&mut self, rendered: &str) {
        let mut g = self.lock();
        g.buf.push_str(rendered);
        let mut lines = Vec::new();
        while let Some(i) = g.buf.find('\n') {
            lines.push(g.buf[..i].to_owned());
            g.buf.drain(..=i);
        }
        if !lines.is_empty() {
            (g.commit)(lines);
        }
    }

    fn width(&self) -> usize {
        let g = self.lock();
        (g.width)()
    }

    /// The block's separator may sit in the transcript's blank latch (interior blanks
    /// defer until more content follows) — the preview about to occupy the next row IS
    /// that content, so materialize it first: the preview must be spaced exactly like the
    /// block it morphs into.
    fn block_preview(&mut self, label: &str) -> Option<Box<dyn crate::ui::facade::PreviewHandle>> {
        let mut g = self.lock();
        if let Some(pre) = &g.pre_open {
            pre();
        }
        Some((g.preview)(&dim(label)))
    }
}

/// Buffers formatted output and commits it as lines (chat/uisink.go `lineCommitter`) —
/// the v2 shape for helpers that print multi-line output into a writer. `flush` strips
/// one trailing newline, splits, and glues any escape-only line (fatih color's
/// reset-around-newline artifact) onto its predecessor so every committed line stays
/// self-contained.
#[derive(Default)]
pub(crate) struct LineCommitter {
    buf: String,
}

impl LineCommitter {
    /// Buffers one formatted chunk.
    pub(crate) fn write(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    /// Splits everything buffered (one trailing newline dropped) into one batch, gluing
    /// escape-only lines onto their predecessor.
    pub(crate) fn flush(&mut self) -> Vec<String> {
        let s = std::mem::take(&mut self.buf);
        let s = s.strip_suffix('\n').unwrap_or(&s);
        if s.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = Vec::new();
        for ln in s.split('\n') {
            if !ln.is_empty()
                && crate::text::ansi::strip_sgr(ln).is_empty()
                && let Some(last) = out.last_mut()
            {
                last.push_str(ln);
                continue;
            }
            out.push(ln.to_owned());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::markdown::sink::Sink as _;
    use crate::markdown::{CodeTheme, RenderOptions, Writer};

    use crate::repl::render::transcript::Transcript;
    use crate::testing::{ScriptedUi, UiEvent};
    use crate::ui::facade::Ui as _;
    use tokio_util::sync::CancellationToken;

    use super::{LineCommitter, UiMdSink};

    /// A no-op facade preview handle.
    struct NopPreview;

    impl crate::ui::facade::PreviewHandle for NopPreview {
        fn write_raw_line(&mut self, _line: &str) {}

        fn close(&mut self) {}
    }

    // Go: chat/uisink_test.go:38 TestUIMDSinkBatchesLines — all complete lines of one
    // write land as ONE commit batch; a partial line buffers until its newline arrives;
    // flush commits the tail.
    #[test]
    fn test_ui_md_sink_batches_lines() {
        let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let sink_batches = Arc::clone(&batches);
        let mut s = UiMdSink::new(
            Box::new(|_label| Box::new(NopPreview)),
            Box::new(move |lines| sink_batches.lock().expect("batches").push(lines)),
            Box::new(|| 80),
            None,
        );

        // A rendered 4-line block arriving in one write (a flushed table).
        s.write("r1\nr2\nr3\nr4\n");
        {
            let got = batches.lock().expect("batches");
            assert_eq!(got.as_slice(), &[vec!["r1", "r2", "r3", "r4"]]);
        }

        // Streaming: a partial line buffers until its newline arrives.
        batches.lock().expect("batches").clear();
        s.write("hel");
        assert!(
            batches.lock().expect("batches").is_empty(),
            "partial line committed early"
        );
        s.write("lo\nwor");
        {
            let got = batches.lock().expect("batches");
            assert_eq!(got.len(), 1);
            assert_eq!(got[0][0], "hello", "line not assembled across writes");
        }
        s.flush();
        {
            let got = batches.lock().expect("batches");
            assert_eq!(got.len(), 2);
            assert_eq!(got[1][0], "wor", "flush did not commit the tail");
        }
    }

    // Go: chat/compose_test.go:559 TestLineCommitterGluesTrailingReset — fatih color's
    // Fprintf places the SGR reset AFTER a trailing newline, so a styled "…\n" write ends
    // with a reset-only line; the committer glues it back so no spurious blank row
    // reaches the transcript.
    #[test]
    fn test_line_committer_glues_trailing_reset() {
        let mut lc = LineCommitter::default();
        lc.write("\x1b[2m  ⎿ /Users/joyqi\n\x1b[0m"); // color.Fprintf's exact shape
        let got = lc.flush();
        assert_eq!(got, vec!["\x1b[2m  ⎿ /Users/joyqi\x1b[0m".to_owned()]);
    }

    /// Records the transcript's facade calls as the `kind:payload` lines the assertions compare
    /// (a `ScriptedUi` at 80×30 plus the rendering of its event log).
    #[derive(Clone)]
    struct Rec(Arc<ScriptedUi>);

    impl Default for Rec {
        fn default() -> Self {
            let ui = ScriptedUi::new(Vec::new());
            ui.set_size(80, 30);
            Self(ui)
        }
    }

    impl Rec {
        /// The facade the transcript under test writes to.
        fn ui(&self) -> Arc<ScriptedUi> {
            Arc::clone(&self.0)
        }
    }

    // Go: chat/uisink_test.go:117 TestPreviewSpacingMatchesSettle — the separator a block
    // visually needs must be ON SCREEN while its preview shows (flushPending via
    // preOpen), not latched until the settle reveals it.
    #[test]
    fn test_preview_spacing_matches_settle() {
        let rec = Rec::default();
        let tr = Arc::new(Transcript::new(rec.ui(), None));
        // The preview opens through the facade's own stream sink, so it lands in the same
        // ordered log as the transcript's prints.
        let stream = rec.ui().start_stream(CancellationToken::new());
        let pre = Arc::clone(&tr);
        let mut committer = tr.content_block();
        let sink = UiMdSink::new(
            Box::new(move |label| stream.block_preview(label)),
            Box::new(move |lines| committer.push(&lines)),
            Box::new(|| 80),
            Some(Box::new(move || pre.flush_pending())),
        );
        let mut mdw = Writer::new(
            Box::new(sink.clone()),
            RenderOptions {
                color: true,
                code_theme: CodeTheme::Monokai,
            },
        );

        mdw.write(b"intro paragraph\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        mdw.flush();
        sink.flush();

        let events: Vec<String> = rec
            .ui()
            .events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Print(lines) => Some(format!("print:{}", lines.join("|"))),
                UiEvent::Preview(label) => {
                    Some(format!("preview:{}", crate::text::ansi::strip_sgr(&label)))
                }
                _ => None,
            })
            .collect();
        let pi = events
            .iter()
            .position(|e| e.starts_with("preview:"))
            .unwrap_or_else(|| panic!("no preview opened:\n{events:?}"));
        // The blank separator must precede the preview open, not trail it.
        assert!(pi > 0, "preview opened first:\n{events:?}");
        assert_eq!(
            events[pi - 1],
            "print:",
            "the block separator is not on screen before the preview:\n{events:?}"
        );
    }
}
