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
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::inline_term::{SYNC_BEGIN, SYNC_END, SYNC_MIN};
use crate::text::ansi::{ansi_width, wrap_ansi};
use crate::ui::render::frame::FrameView;
use crate::ui::render::spans::ansi_to_spans;

/// Whether a live [`Screen`] holds the alternate screen now — set by the live constructor only
/// (`Term::fullscreen`), cleared by [`Screen::leave`] and by `restore_terminal`, which reads it.
pub(crate) static ALT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The bytes that hand the main screen back: the cursor shown, the alternate screen left.
pub(crate) const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// One conversation row: the rendered ANSI string and its display width, measured once when it
/// was committed (a draw wraps it only when the screen is narrower than that).
struct Row {
    text: String,
    cols: usize,
}

/// The full-screen terminal: ratatui's `Terminal` over [`Out`], the conversation buffer, and
/// how far the viewport is scrolled up from the tail.
pub(crate) struct Screen<W: Write> {
    term: Terminal<Out<W>>,
    rows: Vec<Row>,
    /// Display rows between the viewport's last row and the conversation's tail (0 = following
    /// the tail). Clamped by the draw that finds the top.
    scroll: usize,
    /// The viewport's height at the last draw (what half a screen is).
    view_h: u16,
    /// The open batches' depth, shared with the [`SyncOut`] under the backend.
    batch: Arc<AtomicUsize>,
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
        };
        let term = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )?;
        Ok(Self {
            term,
            rows: Vec::new(),
            scroll: 0,
            view_h: size.height,
            batch,
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
        self.rows.extend(rows.iter().map(|text| Row {
            cols: ansi_width(text),
            text: text.clone(),
        }));
    }

    /// Scrolls the viewport by half its height: up (`true`, toward the start of the session) or
    /// down (toward the tail, which it follows again once it gets there).
    pub(crate) fn scroll_half(&mut self, up: bool) {
        let half = usize::from((self.view_h / 2).max(1));
        self.scroll = if up {
            self.scroll.saturating_add(half)
        } else {
            self.scroll.saturating_sub(half)
        };
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
        self.term.clear()
    }

    /// One frame: the conversation's visible rows over the frame's rows, the real cursor where
    /// the frame puts it (hidden when it puts none).
    pub(crate) fn draw_frame(&mut self, view: &FrameView) -> io::Result<()> {
        self.term.autoresize()?;
        let Size { width, height } = self.size();
        let frame_h = u16::try_from(view.rows.len())
            .unwrap_or(u16::MAX)
            .min(height);
        let view_h = height - frame_h;
        self.view_h = view_h;
        let lines = self.visible(usize::from(width), usize::from(view_h));
        let shown = u16::try_from(lines.len()).unwrap_or(view_h).min(view_h);
        // The conversation sits on the frame: a short one leaves the blank rows at the top.
        let conv = Rect::new(0, view_h - shown, width, shown);
        let frame = Rect::new(0, view_h, width, frame_h);
        let frame_lines: Vec<Line<'static>> = view.rows.iter().map(|r| ansi_to_spans(r)).collect();
        let cursor = view.cursor.map(|(x, y)| {
            Position::new(
                x.min(width.saturating_sub(1)),
                view_h.saturating_add(y.min(frame_h.saturating_sub(1))),
            )
        });
        self.term.draw(|f| {
            f.render_widget(Paragraph::new(Text::from(lines)), conv);
            f.render_widget(Paragraph::new(Text::from(frame_lines)), frame);
            if let Some(pos) = cursor {
                f.set_cursor_position(pos);
            }
        })?;
        Ok(())
    }

    /// The viewport's rows, oldest first: the last `h` display rows ending `scroll` rows above
    /// the tail. Rows are wrapped from the tail backwards only as far as the viewport reaches,
    /// and a `scroll` past the session's first row is clamped to it here.
    fn visible(&mut self, width: usize, h: usize) -> Vec<Line<'static>> {
        let need = h.saturating_add(self.scroll);
        let mut back: Vec<String> = Vec::new();
        for row in self.rows.iter().rev() {
            if row.cols > width {
                back.extend(wrap_ansi(&row.text, width).into_iter().rev());
            } else {
                back.push(row.text.clone());
            }
            if back.len() >= need {
                break;
            }
        }
        self.scroll = self.scroll.min(back.len().saturating_sub(h));
        let end = (self.scroll + h).min(back.len());
        back[self.scroll..end]
            .iter()
            .rev()
            .map(|r| ansi_to_spans(r))
            .collect()
    }

    /// The exit: leaves the alternate screen and writes the whole conversation into the main
    /// screen at the width the terminal has now, each row closed with an SGR reset and `CRLF`
    /// (raw mode is still on), then clears whatever was below — the shell's next prompt starts on
    /// a fresh line under the conversation.
    pub(crate) fn leave(&mut self) -> io::Result<()> {
        let width = usize::from(self.size().width.max(1));
        let mut dump = Vec::with_capacity(self.rows.len() * 64);
        dump.extend_from_slice(LEAVE);
        for row in &self.rows {
            let parts = if row.cols > width {
                wrap_ansi(&row.text, width)
            } else {
                vec![row.text.clone()]
            };
            for part in parts {
                dump.extend_from_slice(part.as_bytes());
                dump.extend_from_slice(b"\x1b[0m\r\n");
            }
        }
        dump.extend_from_slice(b"\x1b[J");
        let out = self.out();
        out.write_all(&dump)?;
        Write::flush(out)?;
        ALT_ACTIVE.store(false, Ordering::Relaxed);
        Ok(())
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
}

impl<W: Write> Backend for Out<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
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

#[cfg(test)]
mod tests;
