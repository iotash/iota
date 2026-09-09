//! Terminal ownership — warts W1 `RECREATE_ON_HEIGHT_CHANGE`, W2 `SELF_TRACKED_TOP`,
//! W3 `CLEAR_AFTER_RECREATE`, W6 `LINE_COUNT_SELF_CONSISTENCY`, W9 `PORTABLE_FALLBACK`,
//! plus the DSR resync (`TUI_DESIGN` §4).
//!
//! - **W1**: `Viewport::Inline(h)` is frozen at `Terminal` construction, so any
//!   frame-height change recreates the `Terminal` — `MoveTo(0, top)`, drop, a fresh
//!   `Terminal::with_options(.., Inline(new_h))` (it re-anchors at the cursor row,
//!   scrolling history up when a grow does not fit). Recreation is cheap (a writer
//!   handle); the loop coalesces it to one per iteration.
//! - **W2**: `CompletedFrame.area` lies (`y` is always 0), so the viewport top is
//!   self-tracked: recreation pins `top = min(top, screen_h − new_h)`, every insert
//!   pushes `top = min(top + rows, screen_h − view_height)`, and after a resize ONE cursor DSR
//!   right after a draw that placed the cursor at a known offset recovers
//!   `top = abs_y − rel_y` (skipped while a surface hides the cursor; resynced on the
//!   next composer draw).
//! - **W3**: a recreated `Terminal` starts with empty buffers while the screen still
//!   shows the old frame; `Terminal::clear()` right after recreation (Inline clears
//!   viewport-top→screen-end, wiping shrink-freed rows — below-frame is dead space)
//!   resets the back buffer so the next draw repaints every cell.
//! - **W6**: `insert_before` height comes from `Paragraph::line_count(width)` of the
//!   SAME `Paragraph` rendered into the buffer (feature `unstable-rendered-line-info`).
//!   The region pre-wraps every entry to ≤ width−1 and guarantees one-entry-one-row, so
//!   `line_count == rows.len()` — asserted as a debug invariant, never a second source
//!   of truth. This self-consistency is what kills Go's `sanitizeOverflow` (T-01).
//! - **W9**: ratatui's `scrolling-regions` is always on (one binary; `Cargo.toml`), so
//!   `insert_before` scrolls the partial region 0..top with DECSTBM. An emulator that
//!   discards region-scrolled lines is a bug to fix, not a build to switch to
//!   (`docs/TUI-VERIFY.md` §2).
//!
//! [`Geometry`] is the headless seam: the L2b/vt100 layer injects synthetic
//! size/cursor answers so the REAL writer stack runs without a tty; live construction
//! passes `None` and every query reaches crossterm.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellWidth};
use ratatui::layout::{Position, Size};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::facade::ProgressState;
use super::frame::FrameView;
use super::osc;
use super::spans::ansi_to_spans;

/// Synthetic terminal geometry for headless tests: the backend answers size and
/// cursor-position queries from here instead of the real tty. Cloned handles share
/// state, so a test mutates what the "terminal" reports while [`Term`] keeps its
/// bookkeeping mirrored.
#[derive(Clone)]
pub(crate) struct Geometry {
    size: Arc<Mutex<Size>>,
    cursor: Arc<Mutex<Position>>,
}

/// Rides over lock poisoning: geometry is plain display state and every access
/// re-establishes nothing — a panicked test thread must not poison the loop.
fn ride<T: Copy>(m: &Mutex<T>) -> T {
    match m.lock() {
        Ok(g) => *g,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn store<T>(m: &Mutex<T>, v: T) {
    match m.lock() {
        Ok(mut g) => *g = v,
        Err(poisoned) => *poisoned.into_inner() = v,
    }
}

impl Geometry {
    /// A synthetic terminal of `width`×`height` cells, cursor at the origin.
    #[cfg(test)]
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self {
            size: Arc::new(Mutex::new(Size { width, height })),
            cursor: Arc::new(Mutex::new(Position::ORIGIN)),
        }
    }

    /// Changes the reported terminal size (pair with a scripted `Event::Resize`).
    #[cfg(test)]
    pub(crate) fn set_size(&self, width: u16, height: u16) {
        store(&self.size, Size { width, height });
    }

    /// The reported size.
    pub(crate) fn size(&self) -> Size {
        ride(&self.size)
    }

    /// The reported cursor position (the synthetic DSR answer).
    pub(crate) fn cursor(&self) -> Position {
        ride(&self.cursor)
    }

    fn set_cursor(&self, pos: Position) {
        store(&self.cursor, pos);
    }
}

/// The loop's backend: a [`CrosstermBackend`] whose geometry queries can be answered
/// synthetically ([`Geometry`]) so the byte-emitting stack runs headless. All drawing
/// and scrolling delegates to the inner backend — the emitted bytes are the real ones.
pub(crate) struct LoopBackend<W: Write> {
    inner: CrosstermBackend<W>,
    geo: Option<Geometry>,
}

impl<W: Write> Backend for LoopBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // W9 corollary: a wide grapheme OWNS the cells to its right, and a covered cell
        // reads back as a space. Ratatui's buffer diff (the normal draw path and the
        // scrolling-regions insert) drops those cells, but `Terminal::insert_before` can
        // still hand the WHOLE buffer to the backend (its `draw_lines` path), and an
        // unfiltered backend then prints `中 文 一 行` for a padded CJK row. Drop the
        // covered cells here — the same rule ratatui's own diff and `TestBackend` apply —
        // so the backend emits the same bytes for the same buffer on every path.
        let mut covered: Option<(u16, u16)> = None; // (row, first column past the grapheme)
        self.inner.draw(content.filter(move |(x, y, cell)| {
            if let Some((row, end)) = covered
                && row == *y
                && *x < end
            {
                return false;
            }
            covered = Some((*y, x.saturating_add(cell.cell_width().max(1))));
            true
        }))
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        match &self.geo {
            Some(g) => Ok(g.cursor()),
            None => self.inner.get_cursor_position(),
        }
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let pos = position.into();
        if let Some(g) = &self.geo {
            g.set_cursor(pos); // the synthetic DSR mirrors what the terminal would say
        }
        self.inner.set_cursor_position(pos)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        match &self.geo {
            Some(g) => Ok(g.size()),
            None => self.inner.size(),
        }
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        match &self.geo {
            Some(g) => Ok(WindowSize {
                columns_rows: g.size(),
                pixels: Size::default(),
            }),
            None => self.inner.window_size(),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }

    fn scroll_region_up(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_down(region, line_count)
    }
}

/// Produces a fresh writer for each `Terminal` incarnation (W1 recreation) plus the
/// out-of-band control writer. For the live build this is `|| io::stdout()` (handles
/// share the one stream); tests hand out clones of a shared byte buffer.
pub(crate) type MakeWriter<W> = Box<dyn FnMut() -> W + Send>;

/// Owns the inline `Terminal` and every wart the spike proved: recreation on height
/// change (W1+W3), the self-tracked viewport top with its DSR resync (W2), and
/// `line_count`-sized inserts (W6).
pub(crate) struct Term<W: Write> {
    terminal: Terminal<LoopBackend<W>>,
    make_writer: MakeWriter<W>,
    /// Out-of-band control writer (same underlying stream): `MoveTo` ahead of a
    /// recreation, title OSC, the final cursor park.
    ctrl: W,
    geo: Option<Geometry>,
    /// The self-tracked viewport top row (W2).
    pub(crate) top: u16,
    /// The current inline viewport height.
    pub(crate) view_height: u16,
    /// Set on resize; cleared by the first draw that can DSR-resync `top`.
    top_dirty: bool,
    /// Last emitted window title (emit-on-change).
    last_title: Option<String>,
    /// Last emitted OSC 9;4 state (emit-on-change; `None` = never emitted).
    last_progress: Option<ProgressState>,
    /// Whether focus reporting (mode 1004) was turned on — [`Drop`] turns it back off.
    focus_on: bool,
}

impl<W: Write> Term<W> {
    /// Builds the inline terminal at `height` rows, anchored at `start_top` (the
    /// caller's pre-raw-mode cursor row). `geo` = `None` live, synthetic in tests.
    pub(crate) fn new(
        mut make_writer: MakeWriter<W>,
        height: u16,
        start_top: u16,
        geo: Option<Geometry>,
    ) -> io::Result<Self> {
        let mut ctrl = make_writer();
        anchor(&mut ctrl, geo.as_ref(), start_top)?;
        let backend = LoopBackend {
            inner: CrosstermBackend::new(make_writer()),
            geo: geo.clone(),
        };
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        let screen_h = terminal.size()?.height;
        let top = start_top.min(screen_h.saturating_sub(height));
        Ok(Self {
            terminal,
            make_writer,
            ctrl,
            geo,
            top,
            view_height: height,
            top_dirty: false,
            last_title: None,
            last_progress: None,
            focus_on: false,
        })
    }

    /// The terminal size (synthetic under [`Geometry`]).
    pub(crate) fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    /// W1 `RECREATE_ON_HEIGHT_CHANGE` + W3 `CLEAR_AFTER_RECREATE`: recreates the
    /// `Terminal` at `new_h` anchored at the tracked top, pins the top (W2), and
    /// clears so the next draw repaints every cell. Returns whether a recreation
    /// happened (the loop coalesces to one per iteration by calling this once).
    pub(crate) fn ensure_height(&mut self, new_h: u16) -> io::Result<bool> {
        if new_h == self.view_height {
            return Ok(false);
        }
        anchor(&mut self.ctrl, self.geo.as_ref(), self.top)?;
        let backend = LoopBackend {
            inner: CrosstermBackend::new((self.make_writer)()),
            geo: self.geo.clone(),
        };
        self.terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(new_h),
            },
        )?;
        let screen_h = self.terminal.size()?.height;
        self.view_height = new_h;
        self.top = self.top.min(screen_h.saturating_sub(new_h)); // W2
        self.terminal.clear()?; // W3
        Ok(true)
    }

    /// Commits pre-wrapped rows into native scrollback via `insert_before`, sized by
    /// W6 `LINE_COUNT_SELF_CONSISTENCY`, then updates the tracked top (W2:
    /// push-down-then-pin — ratatui's own internal algorithm). Returns the row count.
    pub(crate) fn insert_lines(&mut self, rows: &[String]) -> io::Result<u16> {
        if rows.is_empty() {
            return Ok(0);
        }
        let size = self.terminal.size()?;
        let width = size.width.max(1);
        let lines: Vec<Line<'static>> = rows.iter().map(|r| ansi_to_spans(r)).collect();
        let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let count = para.line_count(width);
        // W6: the region pre-wraps to ≤ width−1 and splits embedded newlines, so the
        // measured height must equal the entry count — one entry, one row.
        debug_assert_eq!(
            count,
            rows.len(),
            "W6 LINE_COUNT_SELF_CONSISTENCY: an insert entry wrapped or split"
        );
        let n = u16::try_from(count).unwrap_or(u16::MAX);
        self.terminal.insert_before(n, |buf| {
            let area = buf.area;
            para.render(area, buf);
        })?;
        self.top = self
            .top
            .saturating_add(n)
            .min(size.height.saturating_sub(self.view_height));
        Ok(n)
    }

    /// Full clear from the viewport top to screen end + back-buffer reset (the W3
    /// primitive; also the T-32 `tea.ClearScreen` twin for surface Tab switches).
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.terminal.clear()
    }

    /// Syncs ratatui to the (possibly changed) terminal size — the W5 resize pass
    /// calls this before clearing and redrawing.
    pub(crate) fn autoresize(&mut self) -> io::Result<()> {
        self.terminal.autoresize()
    }

    /// Marks the tracked top stale (resize); the next cursor-visible draw resyncs it
    /// via one DSR query (W2).
    pub(crate) fn mark_top_dirty(&mut self) {
        self.top_dirty = true;
    }

    /// W4 `DRAW_WITH_INSERTS` end step: renders the frame rows top-down into the
    /// inline viewport and places the REAL cursor (`None` = hidden, surface open).
    /// When the top is dirty and the cursor was just placed at a known offset, one
    /// DSR query resyncs the tracked top (W2).
    pub(crate) fn draw_frame(&mut self, view: &FrameView) -> io::Result<()> {
        let lines: Vec<Line<'static>> = view.rows.iter().map(|r| ansi_to_spans(r)).collect();
        let cursor = view.cursor;
        self.terminal.draw(|f| {
            let area = f.area();
            f.render_widget(Paragraph::new(Text::from(lines)), area);
            if let Some((x, y)) = cursor {
                let x = x.min(area.width.saturating_sub(1));
                let y = (area.y.saturating_add(y)).min(area.bottom().saturating_sub(1));
                f.set_cursor_position(Position::new(x, y));
            }
        })?;
        if self.top_dirty
            && let Some((_, rel_y)) = cursor
        {
            let abs = if let Some(g) = &self.geo {
                g.cursor()
            } else {
                let (x, y) = crossterm::cursor::position()?;
                Position::new(x, y)
            };
            self.top = abs.y.saturating_sub(rel_y);
            self.top_dirty = false;
        }
        Ok(())
    }

    /// Emits the window title OSC — on change only (the facade already sanitized it).
    /// The never-set + empty combination stays silent.
    pub(crate) fn set_title(&mut self, title: &str) -> io::Result<()> {
        if self.last_title.as_deref() == Some(title)
            || (title.is_empty() && self.last_title.is_none())
        {
            return Ok(());
        }
        osc::emit_title(&mut self.ctrl, title)?;
        self.ctrl.flush()?;
        self.last_title = Some(title.to_owned());
        Ok(())
    }

    /// Emits the OSC 9;4 progress sequence — on change only, exactly like the title, so
    /// the dirty branch can call it unconditionally. A `Term` that never showed a bar
    /// stays silent for `None` (nothing to clear).
    pub(crate) fn set_progress(&mut self, s: ProgressState) -> io::Result<()> {
        if self.last_progress == Some(s)
            || (s == ProgressState::None && self.last_progress.is_none())
        {
            return Ok(());
        }
        self.ctrl.write_all(osc::progress_seq(s).as_bytes())?;
        self.ctrl.flush()?;
        self.last_progress = Some(s);
        Ok(())
    }

    /// Writes one attention ping (OSC 9 + a ringing BEL) out of band. The caller decides
    /// WHETHER to ping (the loop gates on focus) and has already sanitized and defused the
    /// text; both sequences are cursor-neutral, so this is safe between draws.
    pub(crate) fn notify(&mut self, text: &str) -> io::Result<()> {
        self.ctrl.write_all(osc::notify_seq(text).as_bytes())?;
        self.ctrl.flush()
    }

    /// Turns terminal focus reporting on (mode 1004) so the loop learns when the window
    /// blurs — the ONE gate on attention pings. Called by `run_loop` before its first
    /// iteration, NOT by [`Term::new`]: the one-shot picker and the test harnesses build a
    /// `Term` too and must not leave the mode set (T3 design D17).
    pub(crate) fn enable_focus_reporting(&mut self) -> io::Result<()> {
        self.ctrl.write_all(osc::CSI_FOCUS_ON.as_bytes())?;
        self.ctrl.flush()?;
        self.focus_on = true;
        Ok(())
    }

    /// Parks the cursor on the frame's bottom row and opens a fresh shell line —
    /// the loop's exit path (the spike's park sequence).
    pub(crate) fn park_cursor(&mut self) -> io::Result<()> {
        let bottom = self.top.saturating_add(self.view_height);
        queue!(self.ctrl, MoveTo(0, bottom.saturating_sub(1)))?;
        self.ctrl.write_all(b"\r\n")?;
        self.ctrl.flush()
    }
}

/// The renderer's exit clean-up (bubbletea `cursed_renderer.go:178-180,212-214`): focus
/// reporting goes back off and a bar that is still showing is reset. `Term` is passed by
/// value into `run_loop` and therefore dropped on BOTH its clean and its error path, which
/// is what makes this the guarantee — `TermGuard` (raw mode, bracketed paste) unwinds later,
/// on the handle's `close()`. Errors are unreportable here and dropped: the process is
/// leaving the terminal either way.
impl<W: Write> Drop for Term<W> {
    fn drop(&mut self) {
        if self.focus_on {
            let _ = self.ctrl.write_all(osc::CSI_FOCUS_OFF.as_bytes());
        }
        if !matches!(self.last_progress, None | Some(ProgressState::None)) {
            let _ = self.ctrl.write_all(osc::OSC_PROGRESS_RESET.as_bytes());
        }
        let _ = self.ctrl.flush();
    }
}

/// Moves the physical cursor to `(0, top)` ahead of an inline (re)construction —
/// `with_options` anchors the viewport at the cursor row. Mirrors the move into the
/// synthetic geometry so a headless recreation anchors identically.
fn anchor<W: Write>(ctrl: &mut W, geo: Option<&Geometry>, top: u16) -> io::Result<()> {
    queue!(ctrl, MoveTo(0, top))?;
    ctrl.flush()?;
    if let Some(g) = geo {
        g.set_cursor(Position::new(0, top));
    }
    Ok(())
}
