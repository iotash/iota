//! `uiMDSink` (chat/uisink.go): the `crate::markdown::sink::Sink` implementation over the
//! facade — batches ALL complete lines of one write into ONE commit (the anti-crawl law), and
//! commits a buffering block as ONE live block kept as its source (`markdown::Fragment`, laid out
//! at every width the screen takes — tui-plan S7a): committed again under the same id each time
//! it grows and when it closes, so the screen shows it line by line (tui-plan S7b) — plus
//! `lineCommitter` (the SGR-reset glue rule).

use std::sync::{Arc, Mutex, MutexGuard};

use crate::sync::lock;
use crate::ui::facade::{Block, LiveId};

/// Commits one batch of blocks (the transcript's content-block committer).
pub(crate) type CommitFn = Box<dyn FnMut(Vec<Block>) + Send>;

struct MdSinkInner {
    commit: CommitFn,
    /// The open block's id, once it has shown.
    live: Option<LiveId>,
    buf: String,
}

/// The markdown sink over the facade (chat/uisink.go `uiMDSink`). Cloneable handle: one
/// clone goes into the `markdown::Writer`, the caller keeps another to
/// [`UiMdSink::flush`] the trailing partial line after `Writer::flush`.
#[derive(Clone)]
pub(crate) struct UiMdSink(Arc<Mutex<MdSinkInner>>);

impl UiMdSink {
    /// A sink committing through `commit`.
    pub(crate) fn new(commit: CommitFn) -> Self {
        Self(Arc::new(Mutex::new(MdSinkInner {
            commit,
            live: None,
            buf: String::new(),
        })))
    }

    /// Commits a trailing partial line left in the buffer (call after
    /// `markdown::Writer::flush`).
    pub(crate) fn flush(&self) {
        let mut g = self.lock();
        if !g.buf.is_empty() {
            let tail = std::mem::take(&mut g.buf);
            (g.commit)(vec![Block::Row(tail)]);
        }
    }

    fn lock(&self) -> MutexGuard<'_, MdSinkInner> {
        lock(&self.0)
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
            lines.push(Block::Row(g.buf[..i].to_owned()));
            g.buf.drain(..=i);
        }
        if !lines.is_empty() {
            (g.commit)(lines);
        }
    }

    /// The closed block as its source — ONE laid-out block, re-rendered at every width the screen
    /// takes; when it showed while open, its last commit, replacing the live one.
    fn write_block(&mut self, block: crate::markdown::Fragment) {
        let mut g = self.lock();
        let b = match g.live.take() {
            Some(id) => Block::live(id, block, false),
            None => Block::laid(block),
        };
        (g.commit)(vec![b]);
    }

    /// Every open block shows as it grows.
    fn shows_open_blocks(&self) -> bool {
        true
    }

    /// The open block as it stands, replacing what it showed before. Its first showing is
    /// content to the transcript: a separator latched above it is released first, so the open
    /// block is spaced exactly like the closed one.
    fn grow_block(&mut self, block: crate::markdown::Fragment) {
        let mut g = self.lock();
        let id = *g.live.get_or_insert_with(LiveId::next);
        (g.commit)(vec![Block::live(id, block, true)]);
    }

    /// Never asked: every closed block is kept as its source ([`Self::write_block`]) and laid out
    /// by the screen at the width of each draw.
    fn width(&self) -> usize {
        0
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

    use super::{LineCommitter, UiMdSink};

    // Go: chat/uisink_test.go:38 TestUIMDSinkBatchesLines — all complete lines of one
    // write land as ONE commit batch; a partial line buffers until its newline arrives;
    // flush commits the tail.
    #[test]
    fn test_ui_md_sink_batches_lines() {
        let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let sink_batches = Arc::clone(&batches);
        let mut s = UiMdSink::new(Box::new(move |lines: Vec<crate::ui::facade::Block>| {
            let lines = lines.iter().flat_map(|b| b.render(80)).collect();
            sink_batches.lock().expect("batches").push(lines);
        }));

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

    // Go: chat/uisink_test.go:117 TestPreviewSpacingMatchesSettle, for the block that grows in
    // place of the preview (tui-plan S7b): the separator the block needs is on screen before its
    // first showing, every row it takes shows it again under the same id, and the commit that
    // closes it is the one print of the block.
    #[test]
    fn an_open_block_is_spaced_like_the_closed_one_and_grows_a_row_at_a_time() {
        let rec = Rec::default();
        let tr = Arc::new(Transcript::new(rec.ui(), None));
        let mut committer = tr.content_block();
        let sink = UiMdSink::new(Box::new(move |lines| committer.push(&lines)));
        let mut mdw = Writer::new(
            Box::new(sink.clone()),
            RenderOptions {
                color: false,
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
                UiEvent::Print(lines) => Some(format!("print:{}", lines.len())),
                UiEvent::Grow(lines) => Some(format!("grow:{}", lines.len())),
                _ => None,
            })
            .collect();
        // intro, its separator, the table at one row (3 lines boxed), the separator row (still
        // 3), the data row (5 lines with the rule between), then the close: one print of 5.
        assert_eq!(
            events,
            [
                "print:1", "print:1", "grow:3", "grow:3", "grow:5", "print:5"
            ],
            "{events:?}"
        );
    }
}
