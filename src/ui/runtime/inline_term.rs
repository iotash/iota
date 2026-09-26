//! The inline terminal iota owns (`inline-resize-owned`, option B of the 2026-09-24 UI
//! evaluation): two ratatui pieces — [`Buffer`] and [`Buffer::diff`] — and nothing of
//! ratatui's `Terminal`. The viewport is a plain field, a height change is a field write, and
//! nothing here ever clears the screen.
//!
//! **Every byte is addressed from the cursor.** The frame lives at the bottom of a screen
//! other programs share with it (the shell's scrollback above, the emulator's own reflow and
//! history pulls under it), and a resize can land between any two bytes iota writes: tmux
//! applies the next size — pulling history rows back onto the top of the screen on a grow,
//! eating the rows below the cursor on a shrink — before iota has read the event, and an
//! absolute row computed a moment earlier then names somebody else's row. ratatui's
//! `Terminal` can only address absolutely (`MoveTo`, DECSTBM), which is why the tracked top,
//! the recreation on a height change, the post-recreation erase and the acknowledged size
//! existed (W1/W2/W3/W5). Here the physical cursor is the origin of every write — the
//! emulator carries it with its cell through any resize — and the moves are the four that
//! stay exact in CONTENT coordinates whatever the screen did:
//!
//! - `CUU n` up (it can only stop short at the top of the screen: an error DOWNWARD, into the
//!   frame, never onto a transcript row above it);
//! - `LF` down (on the last row it scrolls the screen into the native history, so it moves
//!   exactly one content row whatever the height is);
//! - `CR` + `CUF n` within a row;
//! - `IL`/`DL`/`SD` on rows the frame owns (they push only the frame's rows, or blank ones,
//!   off the screen).
//!
//! `CUD` is never used (it stops at the last row and the way back up would overshoot), nor is
//! any absolute position after the first one, [`InlineTerminal::new`]'s anchor on the row a
//! DSR has just reported. The model below — [`InlineTerminal::viewport`] and the cursor in
//! absolute rows — is bookkeeping for the resize pass's floor (`Term::resize`), resynced from
//! a DSR there; a stale model misplaces nothing, because nothing is placed by it.
//!
//! [`Geometry`] (the headless seam) mirrors each move the way a terminal would carry the
//! cursor, so the synthetic DSR answers what a real one would.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::style::{
    Attribute, Color as CColor, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
    SetUnderlineColor,
};
use ratatui::backend::IntoCrossterm;
use ratatui::buffer::{Buffer, Cell, CellWidth};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier};

use super::term::Geometry;

/// The SGR state of the pen while a run of cells is written.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    ul: Color,
    modifier: Modifier,
}

impl Pen {
    const RESET: Self = Self {
        fg: Color::Reset,
        bg: Color::Reset,
        ul: Color::Reset,
        modifier: Modifier::empty(),
    };

    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            ul: cell.underline_color,
            modifier: cell.modifier,
        }
    }
}

/// The modifiers and the SGR attribute that sets each.
const ATTRIBUTES: [(Modifier, Attribute); 9] = [
    (Modifier::BOLD, Attribute::Bold),
    (Modifier::DIM, Attribute::Dim),
    (Modifier::ITALIC, Attribute::Italic),
    (Modifier::UNDERLINED, Attribute::Underlined),
    (Modifier::SLOW_BLINK, Attribute::SlowBlink),
    (Modifier::RAPID_BLINK, Attribute::RapidBlink),
    (Modifier::REVERSED, Attribute::Reverse),
    (Modifier::HIDDEN, Attribute::Hidden),
    (Modifier::CROSSED_OUT, Attribute::CrossedOut),
];

/// The inline viewport, its diff base, and a cursor-relative writer (module docs).
pub(crate) struct InlineTerminal<W: Write> {
    out: W,
    /// The bytes of the operation in progress: every public operation writes them in one
    /// `write_all` + flush, so an emulator gets it in as few reads as the pipe allows.
    pending: Vec<u8>,
    geo: Option<Geometry>,
    /// The terminal size the frame is laid out against (the resize pass sets it).
    size: Size,
    /// Where the frame is, in absolute rows (bookkeeping — see the module docs).
    viewport: Rect,
    /// What the frame's rows show on screen: the diff base of the next draw.
    shown: Buffer,
    /// Where the physical cursor is, in the same absolute rows. Between operations it is on
    /// a frame row — the composer's, or the frame's first while a surface hides it.
    cursor: Position,
    pen: Pen,
    /// Whether commits are wrapped in DEC 2026 ([`InlineTerminal::set_sync`]).
    sync: bool,
    /// Open batches ([`InlineTerminal::begin_batch`]): while any is open, an operation's
    /// bytes wait in `pending` and the batch goes out as one synchronized update.
    batch: u32,
}

impl<W: Write> InlineTerminal<W> {
    /// Reserves `height` rows for the frame from row `at` down — the row a DSR has just
    /// reported the cursor on, before anything was drawn: the one absolute move this type
    /// makes. Rows the screen does not have are made by scrolling (`LF`), as a shell does.
    pub(crate) fn new(
        out: W,
        geo: Option<Geometry>,
        size: Size,
        height: u16,
        at: u16,
    ) -> io::Result<Self> {
        let at = at.min(size.height.saturating_sub(1));
        let viewport = Rect::new(0, at, size.width, 1);
        let mut t = Self {
            out,
            pending: Vec::new(),
            geo,
            size,
            viewport,
            shown: Buffer::empty(viewport),
            cursor: Position::new(0, at),
            pen: Pen::RESET,
            batch: 0,
            sync: true,
        };
        queue!(t.pending, MoveTo(0, at))?;
        if let Some(g) = &t.geo {
            g.put_cursor(Position::new(0, at));
        }
        t.reserve(height);
        t.commit()?;
        Ok(t)
    }

    /// The terminal size the frame is laid out against.
    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// Takes a new size (the resize pass): bookkeeping only.
    pub(crate) fn set_size(&mut self, size: Size) {
        self.size = size;
        self.viewport.width = size.width;
        self.shown.resize(self.viewport);
    }

    /// Where the frame is (absolute rows, bookkeeping).
    pub(crate) fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The cursor within the frame: its column and its row counted from the frame's top.
    pub(crate) fn frame_cursor(&self) -> Position {
        Position::new(self.cursor.x, self.cursor.y.saturating_sub(self.viewport.y))
    }

    /// The cursor, in absolute rows (bookkeeping).
    pub(crate) fn cursor(&self) -> Position {
        self.cursor
    }

    /// Renders the next frame into a fresh buffer and writes what differs from what the
    /// screen shows. The cursor ends wherever the last cell left it — the caller places it.
    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Buffer)) -> io::Result<()> {
        let mut next = Buffer::empty(self.viewport);
        render(&mut next);
        {
            let updates = self.shown.diff(&next);
            for (x, y, cell) in updates {
                self.put(x, y, cell)?;
            }
        }
        self.pen_reset()?;
        self.shown = next;
        self.commit()
    }

    /// Puts the cursor on `pos` (a frame cell) and shows it, or hides it on the frame's
    /// first row (`None`) — where a resize finds no frame row above it to rewrap.
    pub(crate) fn place_cursor(&mut self, pos: Option<Position>) -> io::Result<()> {
        if let Some(p) = pos {
            self.goto(p)?;
            queue!(self.pending, Show)?;
        } else {
            queue!(self.pending, Hide)?;
            self.goto(Position::new(0, self.viewport.y))?;
        }
        self.commit()
    }

    /// Inserts `n` rows rendered by `draw_fn` right above the frame, which moves down under
    /// them — onto blank rows below it, or with the screen scrolling the transcript into the
    /// history. The frame's rows are not rewritten.
    ///
    /// From the cursor: `LF` to the frame's last row and `n` rows past it (the screen scrolls
    /// as far as it has to), `CUU` back to the frame's first row, `IL n` there — the frame
    /// moves down onto the rows the `LF`s made, and what `IL` pushes off the bottom of the
    /// screen are those blank rows — and the new rows go into the gap. No absolute row, no
    /// scroll region: whatever the emulator did to the screen since the last write, every row
    /// this touches is the frame's or a blank one.
    pub(crate) fn insert_before(
        &mut self,
        n: u16,
        draw_fn: impl FnOnce(&mut Buffer),
    ) -> io::Result<()> {
        if n == 0 {
            return Ok(());
        }
        let at = self.frame_cursor();
        let h = self.viewport.height;
        let mut rows = Buffer::empty(Rect::new(0, 0, self.size.width, n));
        draw_fn(&mut rows);
        if h.saturating_add(n) <= self.size.height {
            self.goto_row(self.viewport.bottom().saturating_sub(1))?;
            self.lf(n);
            self.up(h - 1 + n)?;
            write!(self.pending, "\x1b[{n}L\r")?;
            self.cursor.x = 0;
            let top = self.cursor.y;
            self.put_rows(&mut rows, top)?;
            self.viewport.y = top + n;
        } else {
            // The frame and the rows do not fit on the screen together: the frame goes (from
            // its first row down), the rows are written and scroll up into the history, and
            // the frame is laid out again under them — a full repaint, the one case that
            // takes one (ratatui's non-scrolling-region path does the same).
            self.goto_row(self.viewport.y)?;
            self.erase_below()?;
            for i in 0..n {
                let y = self.cursor.y;
                let mut row = Buffer::empty(Rect::new(0, 0, self.size.width, 1));
                for x in 0..self.size.width {
                    if let (Some(src), Some(dst)) = (rows.cell((x, i)), row.cell_mut((x, 0))) {
                        *dst = src.clone();
                    }
                }
                self.put_rows(&mut row, y)?;
                self.cr();
                self.lf(1);
            }
            self.reserve(h);
        }
        self.shown.area.y = self.viewport.y;
        self.back_to(at)?;
        self.commit()
    }

    /// Writes `k` rows rendered by `draw_fn` from `from` rows above the frame's first row
    /// down — rows the caller knows are blank and its own (a resize's band): the frame does
    /// not move.
    pub(crate) fn fill_above(
        &mut self,
        from: u16,
        k: u16,
        draw_fn: impl FnOnce(&mut Buffer),
    ) -> io::Result<()> {
        if k == 0 {
            return Ok(());
        }
        let at = self.frame_cursor();
        let mut rows = Buffer::empty(Rect::new(0, 0, self.size.width, k));
        draw_fn(&mut rows);
        let top = self.viewport.y.saturating_sub(from);
        self.put_rows(&mut rows, top)?;
        self.back_to(at)?;
        self.commit()
    }

    /// The frame's first row moves up `k` rows onto blank rows it owns (a resize's band):
    /// bookkeeping only — nothing on screen changes.
    pub(crate) fn grow_up(&mut self, k: u16) {
        let k = k.min(self.viewport.y);
        if k == 0 {
            return;
        }
        let area = Rect::new(
            0,
            self.viewport.y - k,
            self.viewport.width,
            self.viewport.height + k,
        );
        self.remap(area, k);
    }

    /// B0's `resize_viewport`: the frame keeps its first row and becomes `h` rows tall. A
    /// shorter frame erases the rows it gives up (and the dead space below them); a taller
    /// one takes the rows below it — made by `LF` where the screen ends, the transcript
    /// scrolling into the history — and erases them. The rows it keeps are not touched: the
    /// next draw writes only what changed.
    pub(crate) fn set_height(&mut self, h: u16) -> io::Result<()> {
        let h = h.clamp(1, self.size.height.max(1));
        let old = self.viewport;
        if h == old.height {
            return Ok(());
        }
        let at = self.frame_cursor();
        if h < old.height {
            self.goto_row(old.y + h)?;
            self.erase_below()?;
            self.remap(Rect { height: h, ..old }, 0);
        } else {
            let g = h - old.height;
            self.goto_row(old.bottom().saturating_sub(1))?;
            self.lf(g);
            let top = self.cursor.y.saturating_sub(h - 1);
            self.up(g - 1)?;
            self.erase_below()?;
            self.viewport.y = top;
            self.shown.area.y = top;
            self.remap(Rect::new(0, top, old.width, h), 0);
        }
        self.back_to(Position::new(at.x, at.y.min(h - 1)))?;
        self.commit()
    }

    /// Moves the cursor `rows` up from where it is, to the first row of what the caller
    /// erases next: never more than the frame's own rows above the cursor (plus, for a
    /// resize, the rows its reflow provably grew above them).
    pub(crate) fn up_from_cursor(&mut self, rows: u16) -> io::Result<()> {
        self.up(rows)?;
        self.commit()
    }

    /// Erases from the cursor's row to the end of the screen; what the frame showed is gone.
    pub(crate) fn erase_here_down(&mut self) -> io::Result<()> {
        self.erase_below()?;
        self.shown = Buffer::empty(self.viewport);
        self.commit()
    }

    /// Moves the cursor down `rows` rows (`LF`) — onto rows a resize has just erased.
    pub(crate) fn down(&mut self, rows: u16) -> io::Result<()> {
        self.lf(rows);
        self.commit()
    }

    /// Lays a `height`-row frame out from the cursor's row down (scrolling the screen where
    /// it ends there): the frame shows nothing yet.
    pub(crate) fn place(&mut self, height: u16) -> io::Result<()> {
        self.cr();
        self.reserve(height);
        self.commit()
    }

    /// Bookkeeping from a DSR: the cursor is REALLY on `y`. The frame is not touched — for
    /// a cursor the caller has just moved off its frame row (the resize pass, before it lays
    /// the frame out again). No byte is written.
    pub(crate) fn resync_cursor(&mut self, y: u16) {
        self.cursor.y = y;
    }

    /// Bookkeeping from a DSR: the cursor is REALLY on `y` — the model follows (the frame
    /// with it). No byte is written.
    pub(crate) fn resync(&mut self, y: u16) {
        let rel = self.cursor.y.saturating_sub(self.viewport.y);
        self.cursor.y = y;
        self.viewport.y = y.saturating_sub(rel);
        self.shown.area.y = self.viewport.y;
        if self.viewport.bottom() > self.size.height {
            self.size.height = self.viewport.bottom();
        }
    }

    /// Closes a band of `pad` blank rows right above the frame: `DL` takes them out (the
    /// frame moves up onto the transcript, blank rows appear at the bottom), `SD` moves the
    /// whole screen back down (those blank rows go off the bottom, blank rows come in at the
    /// top) — the transcript sits on the frame again and the frame is where it was. Both are
    /// counted from the cursor; `SD` does not move it.
    pub(crate) fn close_band(&mut self, pad: u16) -> io::Result<()> {
        let pad = pad.min(self.viewport.y);
        if pad == 0 {
            return Ok(());
        }
        let at = self.frame_cursor();
        self.goto_row(self.viewport.y - pad)?;
        write!(self.pending, "\x1b[{pad}M\x1b[{pad}T\r")?;
        self.cursor.x = 0;
        self.lf(pad);
        self.back_to(at)?;
        self.commit()
    }

    /// The exit: the cursor to the frame's last row, a fresh line below it, and everything
    /// from there down erased (what a taller frame left below the last one).
    pub(crate) fn park(&mut self) -> io::Result<()> {
        self.goto_row(self.viewport.bottom().saturating_sub(1))?;
        self.pending.extend_from_slice(b"\r\n\x1b[J");
        self.lf_model(1);
        self.cursor.x = 0;
        self.commit()
    }

    /// Erases every frame row (the T-32 force-redraw): the next draw writes every cell.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        let at = self.frame_cursor();
        self.goto_row(self.viewport.y)?;
        self.erase_below()?;
        self.shown = Buffer::empty(self.viewport);
        self.back_to(at)?;
        self.commit()
    }

    // --- the writer ------------------------------------------------------------------

    /// Reserves `h` rows from the cursor's row down and makes them the (blank) frame.
    fn reserve(&mut self, h: u16) {
        let h = h.clamp(1, self.size.height.max(1));
        self.lf(h - 1);
        // `up` only fails on a write error into a Vec, which cannot happen.
        let _ = self.up(h - 1);
        self.viewport = Rect::new(0, self.cursor.y, self.size.width, h);
        self.shown = Buffer::empty(self.viewport);
    }

    /// Re-bases the diff base on `area`: old row `j` becomes new row `j + shift`; rows the
    /// old frame did not cover are blank (the caller erased them or knows them blank).
    fn remap(&mut self, area: Rect, shift: u16) {
        let mut next = Buffer::empty(area);
        let old = &self.shown;
        for j in 0..old.area.height {
            let i = j + shift;
            if i >= area.height {
                break;
            }
            for x in 0..area.width.min(old.area.width) {
                if let (Some(src), Some(dst)) = (
                    old.cell((x, old.area.y + j)),
                    next.cell_mut((x, area.y + i)),
                ) {
                    *dst = src.clone();
                }
            }
        }
        self.viewport = area;
        self.shown = next;
    }

    /// Writes every non-blank cell of `rows` with its first row on `top`.
    fn put_rows(&mut self, rows: &mut Buffer, top: u16) -> io::Result<()> {
        rows.area.y = top;
        let blank = Buffer::empty(rows.area);
        for (x, y, cell) in blank.diff(rows) {
            self.put(x, y, cell)?;
        }
        self.pen_reset()
    }

    fn put(&mut self, x: u16, y: u16, cell: &Cell) -> io::Result<()> {
        if self.cursor != Position::new(x, y) {
            self.goto(Position::new(x, y))?;
        }
        let pen = Pen::of(cell);
        if pen != self.pen {
            self.pen_set(pen)?;
        }
        queue!(self.pending, Print(cell.symbol()))?;
        let w = cell.cell_width().max(1);
        self.cursor.x = self.cursor.x.saturating_add(w).min(self.size.width);
        if let Some(g) = &self.geo {
            g.set_col(self.cursor.x.min(self.size.width.saturating_sub(1)));
        }
        Ok(())
    }

    fn pen_set(&mut self, pen: Pen) -> io::Result<()> {
        if pen.modifier != self.pen.modifier {
            queue!(self.pending, SetAttribute(Attribute::Reset))?;
            for (m, a) in ATTRIBUTES {
                if pen.modifier.contains(m) {
                    queue!(self.pending, SetAttribute(a))?;
                }
            }
            self.pen = Pen::RESET;
            self.pen.modifier = pen.modifier;
        }
        if pen.fg != self.pen.fg {
            queue!(self.pending, SetForegroundColor(pen.fg.into_crossterm()))?;
        }
        if pen.bg != self.pen.bg {
            queue!(self.pending, SetBackgroundColor(pen.bg.into_crossterm()))?;
        }
        if pen.ul != self.pen.ul {
            queue!(self.pending, SetUnderlineColor(pen.ul.into_crossterm()))?;
        }
        self.pen = pen;
        Ok(())
    }

    /// Back to the default pen: an erase fills with the current background.
    fn pen_reset(&mut self) -> io::Result<()> {
        if self.pen != Pen::RESET {
            queue!(
                self.pending,
                SetForegroundColor(CColor::Reset),
                SetBackgroundColor(CColor::Reset),
                SetUnderlineColor(CColor::Reset),
                SetAttribute(Attribute::Reset)
            )?;
            self.pen = Pen::RESET;
        }
        Ok(())
    }

    /// Erases the cursor's row and everything below it, never as an erase-below from the
    /// home position — tmux (`scroll-on-clear`, its default) takes that for a clear screen and
    /// files the whole screen into the history: `EL 2` on the row, then an erase-below from
    /// its SECOND column, wherever the row is.
    fn erase_below(&mut self) -> io::Result<()> {
        self.pen_reset()?;
        self.pending.extend_from_slice(b"\r\x1b[2K");
        if self.size.width > 1 {
            self.pending.extend_from_slice(b"\x1b[C\x1b[J\r");
        }
        self.cursor.x = 0;
        Ok(())
    }

    /// Back to the cursor's frame position `at` (after the frame may have moved).
    fn back_to(&mut self, at: Position) -> io::Result<()> {
        self.goto(Position::new(at.x, self.viewport.y.saturating_add(at.y)))
    }

    fn goto_row(&mut self, y: u16) -> io::Result<()> {
        self.goto(Position::new(0, y))
    }

    fn goto(&mut self, p: Position) -> io::Result<()> {
        if p.y < self.cursor.y {
            self.up(self.cursor.y - p.y)?;
        } else if p.y > self.cursor.y {
            self.lf(p.y - self.cursor.y);
        }
        if p.x != self.cursor.x {
            self.cr();
            if p.x > 0 {
                write!(self.pending, "\x1b[{}C", p.x)?;
                self.cursor.x = p.x;
                if let Some(g) = &self.geo {
                    g.set_col(p.x);
                }
            }
        }
        Ok(())
    }

    fn up(&mut self, n: u16) -> io::Result<()> {
        if n == 0 {
            return Ok(());
        }
        write!(self.pending, "\x1b[{n}A")?;
        self.cursor.y = self.cursor.y.saturating_sub(n);
        if let Some(g) = &self.geo {
            g.up(n);
        }
        Ok(())
    }

    fn lf(&mut self, n: u16) {
        for _ in 0..n {
            self.pending.push(b'\n');
        }
        self.lf_model(n);
    }

    fn lf_model(&mut self, n: u16) {
        self.cursor.y = self
            .cursor
            .y
            .saturating_add(n)
            .min(self.size.height.saturating_sub(1));
        if let Some(g) = &self.geo {
            g.lf(n);
        }
    }

    fn cr(&mut self) {
        self.pending.push(b'\r');
        self.cursor.x = 0;
        if let Some(g) = &self.geo {
            g.set_col(0);
        }
    }

    /// Turns the synchronized updates off (or on): the one switch, never decided from a
    /// terminal's name or version (X-55).
    pub(crate) fn set_sync(&mut self, on: bool) {
        self.sync = on;
    }

    /// Opens a batch: every operation until the matching [`InlineTerminal::end_batch`] goes
    /// out as ONE synchronized update. Batches nest. A cursor query must never be made inside
    /// one — `WezTerm` and Alacritty hold the input (the answer, or the query itself) until the
    /// block ends.
    pub(crate) fn begin_batch(&mut self) {
        self.batch += 1;
    }

    /// Closes a batch; the outermost one writes everything it gathered.
    pub(crate) fn end_batch(&mut self) -> io::Result<()> {
        self.batch = self.batch.saturating_sub(1);
        self.commit()
    }

    /// Whether a batch is open (a cursor query now would sit inside a synchronized update).
    pub(crate) fn batching(&self) -> bool {
        self.batch > 0
    }

    /// Writes bytes that move no cursor (an OSC: the title, the progress bar, a ping) in
    /// the order of the operations around them — inside the open batch, if there is one.
    /// Outside a batch they go straight out: nothing on screen changes, nothing to sync.
    pub(crate) fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.batch > 0 {
            self.pending.extend_from_slice(bytes);
            return Ok(());
        }
        self.out.write_all(bytes)?;
        self.out.flush()
    }

    /// Hands the operation's bytes to the terminal in one write, as one DEC 2026 synchronized
    /// update (`ESC[?2026h … ESC[?2026l`): an emulator that knows the mode shows the screen
    /// before the block or after it, never in between — an insert's `LF`s before its `IL`
    /// (the frame a row up for a moment), a height change's erase before its redraw. Sent
    /// blind, like crossterm's `BeginSynchronizedUpdate`: an emulator that does not know the
    /// mode ignores it. Inside a batch the bytes wait for its end.
    fn commit(&mut self) -> io::Result<()> {
        if self.batch > 0 {
            return Ok(());
        }
        if !self.pending.is_empty() && !self.sync {
            self.out.write_all(&self.pending)?;
            self.pending.clear();
        }
        if !self.pending.is_empty() {
            let mut block = Vec::with_capacity(self.pending.len() + 16);
            block.extend_from_slice(SYNC_BEGIN);
            block.append(&mut self.pending);
            block.extend_from_slice(SYNC_END);
            self.out.write_all(&block)?;
        }
        self.out.flush()
    }
}

/// DEC private mode 2026, synchronized output: begin and end of one update.
pub(crate) const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// See [`SYNC_BEGIN`].
pub(crate) const SYNC_END: &[u8] = b"\x1b[?2026l";
