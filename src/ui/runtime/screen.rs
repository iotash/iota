//! The full-screen terminal (`docs`-less migration scaffold: `IOTA_FULLSCREEN=1`, tui-plan S1).
//!
//! iota owns the alternate screen (`ESC[?1049h` on entry, `l` on exit) and draws every frame
//! with ratatui's own [`Terminal`] in [`Viewport::Fullscreen`]: X-54's reason for a hand-rolled
//! terminal — absolute rows going stale across a resize on the main screen — does not exist
//! here (the alternate screen has no history and no reflow), so every byte may name a row.
//!
//! The screen is two rectangles, top to bottom: the conversation viewport and the frame (the
//! loop's [`FrameView`] rows, unchanged, pinned to the bottom). The conversation is a plain
//! `Vec<Row>` of already-rendered ANSI strings (T-05): what used to be inserted into the native
//! history is appended here, and only the rows a draw shows go through `ansi_to_spans`. A row
//! wider than the screen is hard-wrapped by `wrap_ansi` at draw time — nothing is cached yet.
//!
//! - **A frame** is one `Terminal::draw`: autoresize, layout, `Buffer::diff`, the cursor. The
//!   bytes collect in [`SyncOut`] and leave in one write per loop iteration, under X-55's rule
//!   (a write longer than `SYNC_MIN` is one DEC 2026 synchronized update).
//! - **A resize** is a size handed to the backend; the next draw's `autoresize` clears the
//!   screen and repaints every cell. No cursor query is ever made ([`Out`] answers ratatui's
//!   own `get_cursor_position` from the last position it set).
//! - **Exit** ([`Screen::leave`]) leaves the alternate screen and writes the whole conversation
//!   into the main screen at the width it has then, so it survives in the shell's history.
//!   [`ALT_ACTIVE`] lets `restore_terminal` (a panic hook, a second SIGTERM) leave the
//!   alternate screen too when the loop never got to.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossterm::queue;
use crossterm::terminal::{Clear, ClearType as CtClear, EnterAlternateScreen};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::inline_term::{SYNC_BEGIN, SYNC_END, SYNC_MIN};
use crate::ui::render::frame::FrameView;
use crate::ui::render::spans::ansi_to_spans;
use crate::ui::render::theme::{FAINT, RESET, REV_ON};

/// Whether a live [`Screen`] holds the alternate screen now — set by the live constructor only
/// (`Term::fullscreen`), cleared by [`Screen::leave`] and by `restore_terminal`, which reads it.
pub(crate) static ALT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The bytes that hand the main screen back: the cursor shown, the alternate screen left.
pub(crate) const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// A request from the full screen's view keys (tui-plan D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewOp {
    /// `PgUp`: half a screen toward the start.
    HalfUp,
    /// `PgDn`: half a screen toward the tail (following it again at the bottom).
    HalfDown,
    /// `Ctrl+Home`: the session's first line.
    Top,
    /// `Ctrl+End`: the tail, followed again.
    Bottom,
    /// `Ctrl+O`: the conversation written into the terminal's own scrollback ([`Screen::dump`]).
    Dump,
    /// The key after a dump: back to the full screen.
    Return,
    /// The conversation search (`Ctrl+R`, or `/` scrolled back — [`search`]).
    Search(SearchOp),
}

/// The search line's query as the model's field shows it, and the field's cursor column while the
/// query is being typed (`None` once it is applied).
pub(crate) struct Prompt {
    pub(crate) text: String,
    pub(crate) cursor: Option<usize>,
}

/// The line written above the conversation in the main screen, by the first dump or the exit:
/// where iota's part of the shell's history starts.
pub(crate) const TRANSCRIPT_MARK: &str = "┈┈┈┈ iota transcript ┈┈┈┈";

/// The full-screen terminal: ratatui's `Terminal` over [`Out`] and the conversation buffer.
pub(crate) struct Screen<W: Write> {
    term: Terminal<Out<W>>,
    conv: Conversation,
    /// The viewport's height at the last draw (what half a screen is).
    view_h: u16,
    /// The open batches' depth, shared with the [`SyncOut`] under the backend.
    batch: Arc<AtomicUsize>,
    /// The last frame drawn, while it is what the screen shows (a clear or a resize forgets it).
    last: Option<Shown>,
    /// While the conversation is written out on the main screen ([`Screen::dump`]): whether the
    /// alternate screen was marked held for `restore_terminal` ([`ALT_ACTIVE`]) when it was left.
    dumped: Option<bool>,
    /// Rows the dumps and the exit have written into the main screen so far (each is written
    /// once).
    written: u64,
    /// The search under way, if one is.
    search: Option<Search>,
}

/// A drawn frame and the layout it was drawn with — what a scroll of the viewport starts from.
struct Shown {
    buf: Buffer,
    view_h: u16,
    following: bool,
}

impl<W: Write> Screen<W> {
    /// Enters the alternate screen on `out` (`size` is the terminal's) and clears it.
    pub(crate) fn new(out: W, size: Size) -> io::Result<Self> {
        let batch = Arc::new(AtomicUsize::new(0));
        let mut out = SyncOut {
            out,
            pending: Vec::new(),
            batch: Arc::clone(&batch),
        };
        queue!(out, EnterAlternateScreen, Clear(CtClear::All))?;
        out.flush()?;
        let backend = Out {
            inner: CrosstermBackend::new(out),
            size,
            cursor: Position::ORIGIN,
            mute: false,
        };
        let term = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )?;
        Ok(Self {
            term,
            conv: Conversation::new(),
            view_h: size.height,
            batch,
            last: None,
            dumped: None,
            search: None,
            written: 0,
        })
    }

    /// The size the screen is laid out at (the last one [`Screen::resize`] took).
    pub(crate) fn size(&self) -> Size {
        self.term.backend().size
    }

    /// Takes the terminal's new size; the next draw's `autoresize` clears the screen and
    /// repaints every cell — the whole resize protocol.
    pub(crate) fn resize(&mut self, size: Size) {
        self.term.backend_mut().size = size;
    }

    /// Appends committed rows to the conversation (what the inline terminal inserted above
    /// its frame). Nothing is written: the next draw shows them.
    pub(crate) fn push(&mut self, rows: &[String]) {
        self.conv.push(rows);
    }

    /// Whether the view is scrolled back from the tail.
    pub(crate) fn paused(&self) -> bool {
        !self.conv.following()
    }

    /// Applies a view key: moves the viewport (the next draw shows it), writes the conversation
    /// out, or comes back from that.
    pub(crate) fn view(&mut self, op: ViewOp) -> io::Result<()> {
        let width = usize::from(self.size().width.max(1));
        let h = usize::from(self.view_h);
        let half = (h / 2).max(1);
        match op {
            ViewOp::HalfUp => self.conv.scroll_up(half, width, h),
            ViewOp::HalfDown => self.conv.scroll_down(half, width, h),
            ViewOp::Top => self.conv.go_top(width, h),
            ViewOp::Bottom => self.conv.go_bottom(),
            ViewOp::Dump => self.dump()?,
            ViewOp::Return => self.back()?,
            ViewOp::Search(op) => self.search_op(op, width, h),
        }
        Ok(())
    }

    /// `Ctrl+O` (Claude Code's `[`, without a transcript mode — tui-plan D5): leaves the
    /// alternate screen and writes the conversation into the main screen, so the terminal's own
    /// search, copy mode and selection reach all of it. The screen stays there — nothing is drawn,
    /// output keeps collecting in the buffer — until the next key ([`Screen::back`]). Each row is
    /// written once: a dump writes what no dump before it did, so the main screen's history holds
    /// the conversation once, in order, the first part under a [`TRANSCRIPT_MARK`] line.
    fn dump(&mut self) -> io::Result<()> {
        if self.dumped.is_some() {
            return Ok(());
        }
        self.dumped = Some(ALT_ACTIVE.swap(false, Ordering::Relaxed));
        self.write_out(true)
    }

    /// One step of the search: a jump to the hit it is on, or the view back where it was.
    fn search_op(&mut self, op: SearchOp, width: usize, h: usize) {
        match op {
            SearchOp::Open => {
                if self.search.is_none() {
                    let above = self.conv.last_shown(width, h);
                    self.search = Some(Search::new(&self.conv, above));
                }
            }
            SearchOp::Query(q) => {
                if let Some(s) = &mut self.search {
                    s.set_query(&self.conv, &q);
                    match s.current() {
                        Some((n, off)) => self.conv.jump_to(n, off, width, h),
                        None => self.conv.restore(s.origin(), width, h),
                    }
                }
            }
            SearchOp::Step(dir) => {
                if let Some(s) = &mut self.search {
                    s.step(dir);
                    if let Some((n, off)) = s.current() {
                        self.conv.jump_to(n, off, width, h);
                    }
                }
            }
            SearchOp::Accept => self.search = None,
            SearchOp::Cancel => {
                if let Some(s) = self.search.take() {
                    self.conv.restore(s.origin(), width, h);
                }
            }
        }
    }

    /// Back from a dump: the alternate screen again, cleared and repainted whole by the next draw.
    fn back(&mut self) -> io::Result<()> {
        let Some(held) = self.dumped.take() else {
            return Ok(());
        };
        ALT_ACTIVE.store(held, Ordering::Relaxed);
        queue!(self.out(), EnterAlternateScreen)?;
        self.clear()
    }

    /// Opens a batch: what is written until [`Screen::end_batch`] is one write.
    pub(crate) fn begin_batch(&mut self) {
        self.batch.fetch_add(1, Ordering::Relaxed);
    }

    /// Closes the batch and writes it (X-55's rule).
    pub(crate) fn end_batch(&mut self) -> io::Result<()> {
        let depth = self.batch.load(Ordering::Relaxed);
        self.batch.store(depth.saturating_sub(1), Ordering::Relaxed);
        Write::flush(self.out())
    }

    /// Writes cursor-neutral bytes (an OSC) in order with the batch around them.
    pub(crate) fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        let out = self.out();
        out.write_all(bytes)?;
        Write::flush(out)
    }

    /// Clears the screen and forgets what it showed (T-32): the next draw writes every cell.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.last = None;
        self.term.clear()
    }

    /// One frame: the conversation's visible rows over the frame's rows, the real cursor where
    /// the frame puts it (hidden when it puts none).
    pub(crate) fn draw_frame(
        &mut self,
        view: &FrameView,
        prompt: Option<&Prompt>,
    ) -> io::Result<()> {
        if self.dumped.is_some() {
            return Ok(());
        }
        self.term.autoresize()?;
        let Size { width, height } = self.size();
        let frame_h = u16::try_from(view.rows.len())
            .unwrap_or(u16::MAX)
            .min(height);
        let view_h = height - frame_h;
        self.view_h = view_h;
        if let Some(s) = &mut self.search {
            s.catch_up(&self.conv);
        }
        let lines = self.view_lines(usize::from(width), usize::from(view_h), prompt);
        let fresh = self.conv.take_fresh(usize::from(width));
        if let Some(n) = self.scrolled_by(fresh, view_h) {
            self.scroll_region(n, view_h)?;
        }
        let shown = u16::try_from(lines.len()).unwrap_or(view_h).min(view_h);
        // The conversation sits on the frame: a short one leaves the blank rows at the top.
        let conv = Rect::new(0, view_h - shown, width, shown);
        let frame = Rect::new(0, view_h, width, frame_h);
        let frame_lines: Vec<Line<'static>> = view.rows.iter().map(|r| ansi_to_spans(r)).collect();
        // While a query is typed the real cursor is on the search line (an IME's candidate window
        // follows it), not in the composer.
        let typing = self
            .search
            .as_ref()
            .and(prompt)
            .and_then(|p| p.cursor)
            .filter(|_| view_h > 0);
        let cursor = match typing {
            Some(col) => Some(Position::new(
                u16::try_from(col + 1)
                    .unwrap_or(u16::MAX)
                    .min(width.saturating_sub(1)),
                view_h - 1,
            )),
            None => view.cursor.map(|(x, y)| {
                Position::new(
                    x.min(width.saturating_sub(1)),
                    view_h.saturating_add(y.min(frame_h.saturating_sub(1))),
                )
            }),
        };
        let done = self.term.draw(|f| {
            f.render_widget(Paragraph::new(Text::from(lines)), conv);
            f.render_widget(Paragraph::new(Text::from(frame_lines)), frame);
            if let Some(pos) = cursor {
                f.set_cursor_position(pos);
            }
        })?;
        self.last = Some(Shown {
            buf: done.buffer.clone(),
            view_h,
            following: self.conv.following(),
        });
        Ok(())
    }

    /// How far the viewport's content moved up since the last frame, when that is one scroll:
    /// the tail followed then and now, the screen and the viewport the same size, and fewer new
    /// lines than the viewport holds (more is a whole new viewport anyway).
    fn scrolled_by(&self, fresh: usize, view_h: u16) -> Option<u16> {
        let last = self.last.as_ref()?;
        let n = u16::try_from(fresh).ok()?;
        (last.following
            && self.conv.following()
            && last.buf.area.as_size() == self.size()
            && last.view_h == view_h
            && n > 0
            && n < view_h)
            .then_some(n)
    }

    /// Scrolls the viewport's rows up by `n` with ONE operation — `DECSTBM` over the viewport,
    /// `SU n`, the margins reset — where a cell diff would rewrite every row that moved (tui-plan
    /// S2; S1 streamed 3.4× inline's bytes for that). ratatui keeps its previous frame to itself,
    /// so it is handed the same scroll first: the last frame shifted up by `n`, applied with the
    /// backend muted, becomes the previous frame the next draw diffs against — that draw then
    /// writes the `n` new lines and whatever else changed.
    fn scroll_region(&mut self, n: u16, view_h: u16) -> io::Result<()> {
        let Some(Shown { mut buf, .. }) = self.last.take() else {
            return Ok(());
        };
        let width = usize::from(buf.area.width);
        let region = usize::from(view_h) * width;
        let moved = usize::from(n) * width;
        buf.content[..region].rotate_left(moved);
        for cell in &mut buf.content[region - moved..region] {
            cell.reset();
        }
        *self.term.current_buffer_mut() = buf;
        self.term.backend_mut().mute = true;
        let applied = self.term.apply_buffer().map(|_| ());
        self.term.backend_mut().mute = false;
        applied?;
        write!(self.out(), "\x1b[1;{view_h}r\x1b[{n}S\x1b[r")
    }

    /// The viewport's lines as spans, the search's matches highlighted. The last line is the
    /// search line while a search is on — the viewport padded to its full height for it — and,
    /// otherwise, the way back to the tail when the view is anchored.
    fn view_lines(
        &mut self,
        width: usize,
        h: usize,
        prompt: Option<&Prompt>,
    ) -> Vec<Line<'static>> {
        let hl = self.search.as_ref().and_then(Search::highlight);
        let mut lines = self.conv.view_lines(width, h, hl.as_ref());
        if let (Some(s), Some(p)) = (&self.search, prompt)
            && h > 0
        {
            let hint = if p.cursor.is_some() {
                "Enter walks the hits · Esc goes back"
            } else {
                "n/p walk · Enter stays · Esc goes back"
            };
            let status = s.status();
            let sep = if status.is_empty() { "" } else { " · " };
            let line = format!("/{}  {FAINT}{status}{sep}{hint}{RESET}", p.text);
            while lines.len() < h {
                lines.insert(0, String::new());
            }
            if let Some(last) = lines.last_mut() {
                *last = line;
            }
        } else if !self.conv.following()
            && let Some(last) = lines.last_mut()
        {
            *last = match self.conv.unseen() {
                0 => format!("{REV_ON} ↓ Ctrl+End follows the output again {RESET}"),
                1 => format!("{REV_ON} ↓ 1 new line · Ctrl+End {RESET}"),
                n => format!("{REV_ON} ↓ {n} new lines · Ctrl+End {RESET}"),
            };
        }
        lines.iter().map(|r| ansi_to_spans(r)).collect()
    }

    /// The exit: leaves the alternate screen (unless a dump already did) and writes what no dump
    /// has into the main screen — the conversation survives in the shell's history, whole.
    pub(crate) fn leave(&mut self) -> io::Result<()> {
        let leave = self.dumped.take().is_none();
        self.write_out(leave)?;
        ALT_ACTIVE.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Writes the rows not written yet into the main screen at the width the terminal has now —
    /// leaving the alternate screen first when `leave`, under a [`TRANSCRIPT_MARK`] line the first
    /// time — each row closed with an SGR reset and `CRLF` (raw mode is on), then clears what was
    /// below: the next line is a fresh one.
    fn write_out(&mut self, leave: bool) -> io::Result<()> {
        let width = usize::from(self.size().width.max(1));
        let mut dump = Vec::new();
        if leave {
            dump.extend_from_slice(LEAVE);
        }
        if self.written == 0 {
            dump.extend_from_slice(format!("{FAINT}{TRANSCRIPT_MARK}{RESET}\r\n").as_bytes());
        }
        let (lines, written) = self.conv.lines_after(self.written, width);
        self.written = written;
        for line in &lines {
            dump.extend_from_slice(line.as_bytes());
            dump.extend_from_slice(b"\x1b[0m\r\n");
        }
        dump.extend_from_slice(b"\x1b[J");
        let out = self.out();
        out.write_all(&dump)?;
        Write::flush(out)
    }

    /// The backend as a byte sink: what is written here joins the frame's bytes in [`SyncOut`].
    fn out(&mut self) -> &mut CrosstermBackend<SyncOut<W>> {
        &mut self.term.backend_mut().inner
    }
}

/// The byte sink under the backend: everything a frame writes collects here, and leaves in one
/// write at the end of the outermost batch (or at a flush outside any) — a DEC 2026 synchronized
/// update when it is longer than [`SYNC_MIN`], bare otherwise (X-55, the inline terminal's rule).
struct SyncOut<W: Write> {
    out: W,
    pending: Vec<u8>,
    batch: Arc<AtomicUsize>,
}

impl<W: Write> SyncOut<W> {
    fn commit(&mut self) -> io::Result<()> {
        if self.batch.load(Ordering::Relaxed) > 0 {
            return Ok(());
        }
        if self.pending.len() > SYNC_MIN {
            let mut block = Vec::with_capacity(self.pending.len() + 16);
            block.extend_from_slice(SYNC_BEGIN);
            block.append(&mut self.pending);
            block.extend_from_slice(SYNC_END);
            self.out.write_all(&block)?;
        } else if !self.pending.is_empty() {
            self.out.write_all(&self.pending)?;
            self.pending.clear();
        }
        self.out.flush()
    }
}

impl<W: Write> Write for SyncOut<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit()
    }
}

/// ratatui's crossterm backend with two answers of its own: the size is the one the loop handed
/// over (read from the resize event — no query, and a headless test needs no tty), and the
/// cursor position is the last one set (ratatui's `clear` asks for it; a DSR is never sent).
struct Out<W: Write> {
    inner: CrosstermBackend<SyncOut<W>>,
    size: Size,
    cursor: Position,
    /// Whether ratatui's writes are dropped ([`Screen::scroll_region`] hands it a frame the
    /// screen already shows).
    mute: bool,
}

impl<W: Write> Backend for Out<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if self.mute {
            return Ok(());
        }
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        if self.mute {
            return Ok(());
        }
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.cursor = position.into();
        self.inner.set_cursor_position(self.cursor)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

mod conversation;
pub(crate) mod search;

use conversation::Conversation;
use search::{Search, SearchOp};

#[cfg(test)]
mod tests;
