//! Terminal ownership — warts W5 `RESIZE_PASS_FIRST`, W6 `LINE_COUNT_SELF_CONSISTENCY`
//! (`TUI_DESIGN` §4) over the inline terminal iota owns ([`InlineTerminal`]).
//!
//! W1 `RECREATE_ON_HEIGHT_CHANGE`, W2 `SELF_TRACKED_TOP`, W3 `CLEAR_AFTER_RECREATE`, the
//! acknowledged size and W9's scrolling regions are gone (2026-09-25, `inline-resize-owned`
//! option B, DIVERGENCES X-54): each existed only because ratatui's `Terminal` froze the
//! inline viewport at construction and addressed the screen absolutely. The viewport is a
//! field now, a height change is a field write and a repaint of what changed, and every byte
//! is written from the cursor ([`InlineTerminal`]'s module docs), which the emulator carries
//! with its cell through any resize.
//!
//! - **W5**: [`Term::resize`] re-anchors the frame clearing only rows the old frame provably
//!   owned, and scrolls or inserts nothing: ONE DSR before writing a byte — for the
//!   bookkeeping of the floor, never for where the erase lands; the erase goes up from the
//!   cursor by the frame's own rows above it (every draw leaves the cursor on a known frame
//!   row, and a resize carries it with its cell) plus the rows a reflow grew above them once
//!   the emulator has shown that it reflows (a lower bound of our own rows' growth —
//!   [`Term::rewrite_rows`] keeps each row's line exactly as long as what it shows); a frame
//!   flush with the bottom kept flush on the new floor, the rows between left as a blank band
//!   the next output fills ([`Term::pad`]); and staged rows the emulator itself pushed into
//!   the history handed back to be committed where they stand. The frame is laid out at the
//!   terminal's FULL width at rest and a column (or a few) short while a drag lasts
//!   (`Model::frame_width`, the burst layout — the owner's, X-52): only a drag's first step
//!   rewraps the frame's full-width rows, and that growth is ours to claim, never to predict
//!   for transcript rows. The size a pass takes is the one the terminal has NOW, and the
//!   cursor wins over any size read: a cursor below the last row read means the screen grew
//!   again in between. A band row is filled by the next output in place, the frame not moving
//!   ([`Term::insert_lines`]). An emulator that does not reflow never has a row above the
//!   cursor claimed (DIVERGENCES X-52).
//! - **Why no row is lost** (the verifier's three repros, 2026-09-25): an erase starts at a row
//!   counted UP from the cursor, so a resize the emulator applies between iota's check and its
//!   write — tmux pulling history rows back on a grow — moves the rows the erase lands on
//!   together with the cursor (the check→execute window names no row); writes made before
//!   iota has read a resize (a stream inserting and drawing under a new size) are cursor-
//!   relative too, so the cursor stays on the frame row the bookkeeping says (no stale
//!   anchor); and a DSR that fails costs the floor, not the anchor (no fallback row).
//! - **W6**: `insert_before` height comes from `Paragraph::line_count(width)` of the
//!   SAME `Paragraph` rendered into the buffer (feature `unstable-rendered-line-info`).
//!   The region pre-wraps every entry to ≤ width−1 and guarantees one-entry-one-row, so
//!   `line_count == rows.len()` — asserted as a debug invariant, never a second source
//!   of truth. This self-consistency is what kills Go's `sanitizeOverflow` (T-01).
//!
//! [`Geometry`] is the headless seam: the L2b/vt100 layer injects synthetic
//! size/cursor answers so the REAL writer stack runs without a tty; live construction
//! passes `None` and every query reaches crossterm.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ratatui::buffer::{Cell, CellWidth};
use ratatui::layout::{Position, Size};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::inline_term::{InlineTerminal, is_blank};
use super::osc;
use crate::ui::facade::ProgressState;
use crate::ui::render::frame::FrameView;
use crate::ui::render::spans::ansi_to_spans;

/// Synthetic terminal geometry for headless tests: the backend answers size and
/// cursor-position queries from here instead of the real tty. Cloned handles share
/// state, so a test mutates what the "terminal" reports while [`Term`] keeps its
/// bookkeeping mirrored.
#[derive(Clone)]
pub(crate) struct Geometry {
    size: Arc<Mutex<Size>>,
    cursor: Arc<Mutex<Position>>,
    /// Test seam: the cursor query fails (a terminal that never answers the DSR — crossterm
    /// gives up after ~2 s with an error).
    dsr_fails: Arc<std::sync::atomic::AtomicBool>,
    /// Test seam: the rows the screen REALLY has when the reported size lags it (tmux applies
    /// the next resize before the tty size or the event says so); the cursor moves within it.
    real_rows: Arc<Mutex<Option<u16>>>,
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
            dsr_fails: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            real_rows: Arc::new(Mutex::new(None)),
        }
    }

    /// The screen really has `rows` rows while the reported size stays what it was.
    #[cfg(test)]
    pub(crate) fn set_real_rows(&self, rows: u16) {
        store(&self.real_rows, Some(rows));
    }

    /// The last row the cursor can reach: the real screen's, or the reported one's.
    fn last_row(&self) -> u16 {
        ride(&self.real_rows)
            .unwrap_or_else(|| self.size().height)
            .saturating_sub(1)
    }

    /// Makes every cursor query fail from now on (or answer again).
    #[cfg(test)]
    pub(crate) fn set_dsr_fails(&self, fails: bool) {
        self.dsr_fails
            .store(fails, std::sync::atomic::Ordering::Relaxed);
    }

    /// The synthetic DSR: the cursor, or the error a terminal that never answers yields.
    pub(super) fn query_cursor(&self) -> io::Result<Position> {
        if self.dsr_fails.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the terminal did not answer the cursor position query",
            ));
        }
        Ok(self.cursor())
    }

    /// Changes the reported terminal size (pair with a scripted `Event::Resize`).
    #[cfg(test)]
    pub(crate) fn set_size(&self, width: u16, height: u16) {
        store(&self.size, Size { width, height });
    }

    /// Moves the synthetic cursor by `rows` (negative = up) — what an emulator does to
    /// the cursor when a resize reflows or drops the rows above it (pair with
    /// [`Geometry::set_size`] ahead of the scripted `Event::Resize`).
    #[cfg(test)]
    pub(crate) fn shift_cursor(&self, rows: i32) {
        let pos = self.cursor();
        let y = (i32::from(pos.y) + rows).clamp(0, i32::from(u16::MAX));
        self.set_cursor(Position::new(pos.x, u16::try_from(y).unwrap_or(0)));
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

    /// The cursor is put on `pos` (an absolute move).
    pub(super) fn put_cursor(&self, pos: Position) {
        self.set_cursor(pos);
    }

    /// The cursor moves to column `x` of its row.
    pub(super) fn set_col(&self, x: u16) {
        let pos = self.cursor();
        self.set_cursor(Position::new(x, pos.y));
    }

    /// `CUU n`: up, stopping on the top row.
    pub(super) fn up(&self, n: u16) {
        let pos = self.cursor();
        self.set_cursor(Position::new(pos.x, pos.y.saturating_sub(n)));
    }

    /// `n` × `LF`: down, the screen scrolling under a cursor that stays on the last row.
    pub(super) fn lf(&self, n: u16) {
        let pos = self.cursor();
        let last = self.last_row();
        self.set_cursor(Position::new(pos.x, pos.y.saturating_add(n).min(last)));
    }
}

/// Produces the writers: one for the inline terminal, one out-of-band control writer. For
/// the live build this is `|| io::stdout()` (handles share the one stream); tests hand out
/// clones of a shared byte buffer.
pub(crate) type MakeWriter<W> = Box<dyn FnMut() -> W + Send>;

/// Owns the inline terminal and the frame's life on it: the resize it re-anchors with one
/// DSR (W5), the band a resize leaves, and `line_count`-sized inserts (W6).
pub(crate) struct Term<W: Write> {
    t: InlineTerminal<W>,
    /// Out-of-band control writer (same underlying stream): title OSC, progress, pings.
    ctrl: W,
    geo: Option<Geometry>,
    /// Blank rows directly above the frame that a resize left there (W5 step 3): ours to
    /// fill with the next output, so a band never has to scroll into the history.
    pad: u16,
    /// Whether the emulator rewraps its lines on a narrowing — learned from the first
    /// resize that shows it ([`Term::learn_reflow`]); `None` counts as "no".
    reflows: Option<bool>,
    /// An upper bound of each frame row's line length in the emulator (relative to the
    /// top; missing = 0). An erase empties every line; a draw can only lengthen one to
    /// what it showed or shows; [`Term::rewrite_rows`] brings one back to what it shows.
    /// Inserts move the frame's rows as a block, so the bound survives them.
    line_len: Vec<u16>,
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
        let ctrl = make_writer();
        let size = if let Some(g) = &geo {
            g.size()
        } else {
            let (width, height) = crossterm::terminal::size()?;
            Size { width, height }
        };
        let t = InlineTerminal::new(make_writer(), geo.clone(), size, height, start_top)?;
        Ok(Self {
            t,
            ctrl,
            geo,
            pad: 0,
            reflows: None,
            line_len: Vec::new(),
            last_title: None,
            last_progress: None,
            focus_on: false,
        })
    }

    /// The frame's first row (bookkeeping, absolute).
    pub(crate) fn top(&self) -> u16 {
        self.t.viewport().y
    }

    /// The frame's height.
    pub(crate) fn view_height(&self) -> u16 {
        self.t.viewport().height
    }

    /// A frame-height change: a field write and a repaint of what changed (no recreation,
    /// no clear). A taller frame grows into the blank band a resize left above it (W5)
    /// before it takes rows below it — scrolling the transcript into the history where the
    /// screen ends. Returns whether the height changed.
    pub(crate) fn ensure_height(&mut self, new_h: u16) -> io::Result<bool> {
        let h = self.view_height();
        if new_h == h {
            return Ok(false);
        }
        let grow = new_h.saturating_sub(h).min(self.pad).min(self.top());
        if grow > 0 {
            self.pad -= grow;
            self.t.grow_up(grow);
            let mut len = vec![0; usize::from(grow)];
            len.append(&mut self.line_len);
            self.line_len = len;
        }
        self.t.set_height(new_h)?;
        self.line_len.resize(usize::from(self.view_height()), 0);
        Ok(true)
    }

    /// W5 `RESIZE_PASS_FIRST`: takes the new `size` and re-anchors the frame (now `new_h`
    /// rows) where the emulator left it. Every row this clears is one the old frame provably
    /// owned; a transcript row is never touched, and nothing is inserted or scrolled.
    ///
    /// 1. **The anchor.** The last write left the cursor on frame row `c` — the composer's,
    ///    or the frame's top-left while a surface hides it — and a resize carries it with its
    ///    cell, so the `c` rows above it and everything below it are the old frame's. The
    ///    erase starts `c` rows UP from the cursor (`CUU`, which can only stop short, at the
    ///    top of the screen): no row number enters it.
    /// 2. **The overhang.** What a reflow grew ABOVE the cursor lies above that row: the top
    ///    separator's second piece, a staged row that wrapped. It is ours only on an emulator
    ///    that reflows, so it is claimed only once the emulator has SHOWN that it does
    ///    ([`Term::learn_reflow`]) and then by a lower bound of our own rows' growth
    ///    ([`Drawn::growth_above`]). Unknown or not reflowing, nothing above the frame's
    ///    first row is touched; short of the truth, a row is duplicated, never lost. (A
    ///    hidden cursor sits on the frame's first row: nothing of the frame is above it.)
    /// 3. **The floor.** The ONE DSR, before any byte is written, says where that row is:
    ///    `start = cursor − above`. A frame that was flush with the bottom stays flush:
    ///    `top = S' − new_h`. An emulator keeps its last row on the bottom through a reflow
    ///    — the cursor estimate alone lags whatever grew BELOW the cursor — and a row grow
    ///    pulls history in above it or adds blank rows below it. The rows between `start`
    ///    and the floor are old-frame rows: they are erased and left as a blank BAND between
    ///    the transcript and the frame ([`Term::pad`]), which the next output fills before
    ///    anything scrolls. The move there is `LF`s from `start` — a DSR that is stale by
    ///    then only makes the band a row taller or shorter. tmux eats the rows below the
    ///    cursor on a row shrink: the cursor then lands on the bottom row, the floor lies
    ///    above `start`, and the frame stays at `start`, its rows made by `LF`. Without an
    ///    answer (a terminal that does not answer the DSR), there is no floor: the frame is
    ///    laid out where it was.
    /// 4. **Rows the emulator already archived.** A frame near the top that grows in a
    ///    reflow — a drastic narrowing right after startup — has its first rows pushed
    ///    off the screen into the history. Whole rows counted by the same lower bound
    ///    ([`Drawn::rows_pushed`]), up to `droppable` (the staged rows the frame opens
    ///    with), are returned: the caller drops them from the frame and the staging window,
    ///    since drawing them again would show them twice. `height(dropped)` is the frame
    ///    height without them.
    /// 5. **The recheck.** A second DSR once the frame is laid out: the bookkeeping follows
    ///    the cursor (the frame's first row) — it only ever moves the model, never a byte.
    pub(crate) fn resize(
        &mut self,
        size: Size,
        droppable: u16,
        height: impl FnOnce(u16) -> u16,
    ) -> io::Result<u16> {
        // The size the terminal has NOW: in a fast drag the event's size can already be stale
        // (the next resize applied, its event not yet read).
        let mut size = self
            .real_size()
            .filter(|s| s.width > 0 && s.height > 0)
            .unwrap_or(size);
        let old = self.t.size();
        let view = self.t.viewport();
        let flush = view.bottom() >= old.height;
        let at = self.t.frame_cursor();
        let d = Drawn {
            top: view.y,
            widths: visible_widths(self.t.shown(), view),
            cursor_row: at.y,
            cursor_x: at.x,
        };
        let mut above = d.cursor_row;
        let mut start = None;
        let mut dropped = 0;
        // A DSR that fails (crossterm's ~2 s timeout, a terminal that never answers) is not
        // a reason to unwind the loop: the anchor is the cursor either way.
        if let Ok(pos) = self.cursor_position() {
            // The cursor is the one current fact: a cursor below the last row of the size just
            // read means the terminal grew again in between — it has at least that many rows.
            if pos.y >= size.height {
                size.height = pos.y.saturating_add(1);
            }
            if size.width < old.width {
                self.learn_reflow(&d, old, size, pos.y, flush);
            }
            let cols = (size.width < old.width && self.reflows == Some(true)).then_some(size.width);
            above = d
                .cursor_row
                .saturating_add(cols.map_or(0, |c| d.growth_above(c)));
            start = Some(pos.y.saturating_sub(above));
            dropped = d
                .rows_pushed(above.saturating_sub(pos.y), cols)
                .min(droppable);
        }
        self.t.set_size(size);
        let new_h = height(dropped).clamp(1, size.height.max(1));
        self.t.up_from_cursor(above)?;
        if let Some(s) = start {
            self.t.resync(s);
        }
        self.t.erase_here_down()?;
        self.line_len.clear();
        let here = self.t.cursor().y;
        let floor = size.height.saturating_sub(new_h);
        if flush && start.is_some() && floor > here {
            self.t.down(floor - here)?;
            self.pad = self.pad.saturating_add(floor - here);
        }
        self.t.place(new_h)?;
        if let Ok(pos) = self.cursor_position() {
            self.t.resync(pos.y);
        }
        self.pad = self.pad.min(self.top());
        Ok(dropped)
    }

    /// Learns from a narrowing whether the emulator reflows — a fixed property of the
    /// terminal, so one conclusive resize settles it for the session. With the height
    /// unchanged, a reflowing emulator moves the cursor when rows grew on the side it
    /// anchors against (below it for one that keeps the bottom row, above it for one that
    /// keeps the top); a flush frame whose rows below the cursor multiplied has reflowed
    /// whatever the height did. "No reflow" needs a still cursor with rows that would
    /// have grown on BOTH sides. A row change alone proves nothing (tmux, for one, eats
    /// the rows below the cursor first), so it leaves the answer where it was.
    fn learn_reflow(&mut self, d: &Drawn, old: Size, new: Size, cur: u16, flush: bool) {
        let row = usize::from(d.cursor_row);
        let wraps = |w: &u16| *w > new.width;
        let above = d.widths.iter().take(row).any(wraps) || d.growth_above(new.width) > 0;
        let below = d.widths.iter().skip(row + 1).any(wraps)
            || d.widths
                .get(row)
                .is_some_and(|&w| w.div_ceil(new.width.max(1)) > 1 + d.cursor_x / new.width.max(1));
        if !above && !below {
            return;
        }
        let rows_below = new.height.saturating_sub(1).saturating_sub(cur);
        let plain_below = self
            .view_height()
            .saturating_sub(1)
            .saturating_sub(d.cursor_row);
        if flush && new.height <= old.height && rows_below > plain_below {
            self.reflows = Some(true);
        } else if new.height == old.height {
            if cur != d.top.saturating_add(d.cursor_row) {
                self.reflows = Some(true);
            } else if above && below {
                self.reflows = Some(false);
            }
        }
    }

    /// The terminal's size NOW — the synthetic one under [`Geometry`], the tty's live.
    fn real_size(&self) -> Option<Size> {
        match &self.geo {
            Some(g) => Some(g.size()),
            None => crossterm::terminal::size()
                .ok()
                .map(|(width, height)| Size { width, height }),
        }
    }

    /// The physical cursor: one DSR live, the synthetic answer under [`Geometry`].
    fn cursor_position(&self) -> io::Result<Position> {
        if let Some(g) = &self.geo {
            return g.query_cursor();
        }
        let (x, y) = crossterm::cursor::position()?;
        Ok(Position::new(x, y))
    }

    /// Closes the band a resize left above the frame (W5 step 3) when the drag is over: the
    /// band's rows are deleted and the screen scrolled back down over them, so the transcript
    /// sits on the frame again, the frame where it was, and the band's rows go to the top of
    /// the screen — never a hole the next output starts in. Once per drag, never inside a
    /// resize pass (a scroll during the drag would leave blank rows at the top for the drag's
    /// next reflow to push into the history).
    pub(crate) fn close_band(&mut self) -> io::Result<()> {
        let pad = std::mem::take(&mut self.pad).min(self.top());
        self.t.close_band(pad)
    }

    /// Commits pre-wrapped rows into native scrollback right above the frame, sized by W6
    /// `LINE_COUNT_SELF_CONSISTENCY`. Returns the row count.
    pub(crate) fn insert_lines(&mut self, rows: &[String]) -> io::Result<u16> {
        if rows.is_empty() {
            return Ok(0);
        }
        let width = self.t.size().width.max(1);
        let lines: Vec<Line<'static>> = rows.iter().map(|r| ansi_to_spans(r)).collect();
        // W6: the region pre-wraps to ≤ width−1 and splits embedded newlines, so the
        // measured height must equal the entry count — one entry, one row.
        debug_assert_eq!(
            Paragraph::new(Text::from(lines.clone()))
                .wrap(Wrap { trim: false })
                .line_count(width),
            rows.len(),
            "W6 LINE_COUNT_SELF_CONSISTENCY: an insert entry wrapped or split"
        );
        let total = u16::try_from(rows.len()).unwrap_or(u16::MAX);
        let mut lines = lines;
        // The band a resize left above the frame (W5) takes the first rows: they are written
        // straight into it, right under the transcript, and the frame does not move — only
        // what the band cannot hold is inserted (and scrolls anything).
        let pad = self.pad.min(self.top());
        if pad > 0 {
            let k = pad.min(total);
            let rest = lines.split_off(usize::from(k));
            let para = Paragraph::new(Text::from(lines));
            self.t.fill_above(pad, k, |buf| {
                let area = buf.area;
                para.render(area, buf);
            })?;
            self.pad = pad - k;
            lines = rest;
        }
        if !lines.is_empty() {
            let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
            self.t.insert_before(n, |buf| {
                let area = buf.area;
                para.render(area, buf);
            })?;
        }
        Ok(total)
    }

    /// Erases every frame row and forgets what they showed — the T-32 `tea.ClearScreen`
    /// twin for surface Tab switches: the next draw writes every cell.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.line_len.clear();
        self.t.clear()
    }

    /// The size the frame is laid out against (what the last resize pass took).
    pub(crate) fn size(&self) -> Size {
        self.t.size()
    }

    /// W4 `DRAW_WITH_INSERTS` end step: renders the frame rows top-down into the
    /// inline viewport and places the REAL cursor (`None` = hidden, surface open). Either
    /// way the physical cursor ends on a known frame row — the cursor's, or the frame's
    /// first row while it is hidden — which is what W5's resize anchor counts from.
    pub(crate) fn draw_frame(&mut self, view: &FrameView) -> io::Result<()> {
        let lines: Vec<Line<'static>> = view.rows.iter().map(|r| ansi_to_spans(r)).collect();
        let cursor = view.cursor;
        let line_len = std::mem::take(&mut self.line_len);
        let area = self.t.viewport();
        let mut placed = None;
        self.t.draw(|buf| {
            Paragraph::new(Text::from(lines)).render(area, buf);
            let widths = visible_widths(buf, area);
            // A row whose line may run past what it now shows: the diff writes a SPACE into
            // each cell that went blank, and an emulator counts written cells as line length
            // — even after an `EL` (tmux does). Its cells go along to be written again over
            // an erased line.
            let redraw: Vec<(u16, Vec<Cell>)> = widths
                .iter()
                .enumerate()
                .filter(|&(i, &w)| line_len.get(i).copied().unwrap_or(0) > w)
                .map(|(i, _)| {
                    let y = area.y + u16::try_from(i).unwrap_or(u16::MAX);
                    let cells = (area.left()..area.right())
                        .map(|x| buf.cell((x, y)).cloned().unwrap_or_default())
                        .collect();
                    (y, cells)
                })
                .collect();
            let bottom = area.bottom().saturating_sub(1);
            let pos = cursor.map(|(x, y)| {
                Position::new(
                    x.min(area.width.saturating_sub(1)),
                    area.y.saturating_add(y).min(bottom),
                )
            });
            placed = Some((pos, widths, redraw));
        })?;
        let Some((pos, widths, redraw)) = placed else {
            return Ok(());
        };
        // What the diff may have written: nothing past the longer of the old line and the
        // new content.
        self.line_len = widths
            .iter()
            .enumerate()
            .map(|(i, &w)| w.max(line_len.get(i).copied().unwrap_or(0)))
            .collect();
        self.rewrite_rows(&redraw, &widths, area.y)?;
        // Hidden: parked on the frame's top-left, where the next resize finds no frame row
        // above it to rewrap. Cursor-invisible, so the move costs nothing on screen.
        self.t.place_cursor(pos)
    }

    /// Keeps each frame row's line in the emulator exactly as long as what it shows, which
    /// W5's growth bound ([`Drawn::growth_above`]) takes for granted: a row whose line may
    /// be longer (see [`Term::draw_frame`]) is erased WHOLE (`EL 2` — the one erase that
    /// resets a tmux line) and its cells written again.
    fn rewrite_rows(
        &mut self,
        rows: &[(u16, Vec<Cell>)],
        widths: &[u16],
        top: u16,
    ) -> io::Result<()> {
        for (y, cells) in rows {
            self.t.rewrite_row(*y, cells)?;
            let i = usize::from(y.saturating_sub(top));
            if let (Some(len), Some(&w)) = (self.line_len.get_mut(i), widths.get(i)) {
                *len = w;
            }
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

    /// Parks the cursor on the frame's bottom row, opens a fresh shell line and clears
    /// from there down — the loop's exit path (the spike's park sequence). The clear is
    /// for what a frame taller than the last one may have left below it.
    pub(crate) fn park_cursor(&mut self) -> io::Result<()> {
        self.t.park()
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

/// What the last [`Term::draw_frame`] left on screen: each row's visible width — which,
/// with [`Term::rewrite_rows`], is the emulator's line length — and where it left the
/// physical cursor: the W5 resize anchor.
struct Drawn {
    /// The viewport top it was drawn at.
    top: u16,
    /// Each viewport row's visible width: through its last cell that shows anything.
    widths: Vec<u16>,
    cursor_row: u16,
    cursor_x: u16,
}

impl Drawn {
    /// A LOWER bound of the rows a reflow to `cols` columns adds above the cursor: each
    /// row above it splits into `⌈width / cols⌉` pieces, and the cursor's own row puts the
    /// pieces before the cursor's cell above it. The widths are the emulator's line
    /// lengths (`Term::rewrite_rows` keeps them so); an emulator that
    /// wraps a wide glyph early only makes the truth larger — a duplicated row, never a
    /// cleared transcript row (`Term::resize` step 3).
    fn growth_above(&self, cols: u16) -> u16 {
        let cols = cols.max(1);
        let above: u16 = self
            .widths
            .iter()
            .take(usize::from(self.cursor_row))
            .map(|&w| w.div_ceil(cols).saturating_sub(1))
            .fold(0, u16::saturating_add);
        let own = self
            .widths
            .get(usize::from(self.cursor_row))
            .map_or(0, |&w| self.cursor_x.min(w.saturating_sub(1)) / cols);
        above.saturating_add(own)
    }
}

/// Each row's visible width in `area`: through the RIGHT edge of its last cell that shows
/// anything — a glyph, or a blank with a background or a modifier (a highlight bar is
/// content). A wide grapheme owns the cell to its right, which ratatui keeps as a covered
/// `" "`; the walk back skips that cell like any blank and lands on the grapheme's own
/// cell, so the width is that cell's column plus the grapheme's cell width — never its
/// column + 1, which cut a row ending in `中` one short, and a shrinking row was then
/// erased from the middle of its last glyph.
fn visible_widths(buf: &ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> Vec<u16> {
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .rev()
                .find_map(|x| {
                    buf.cell((x, y))
                        .filter(|c| !is_blank(c))
                        .map(|c| x - area.left() + c.cell_width().max(1))
                })
                .unwrap_or(0)
                .min(area.width)
        })
        .collect()
}

impl Drawn {
    /// How many of the frame's first rows lie WHOLE among the `pieces` rows a resize put
    /// above the screen's top — at `cols` columns if the emulator reflows, one row each
    /// otherwise. Lower-bound pieces never count a row that is still partly on screen:
    /// the true pieces above can only exceed the counted ones by as much as the rows
    /// before it grew.
    fn rows_pushed(&self, pieces: u16, cols: Option<u16>) -> u16 {
        let mut left = pieces;
        let mut rows = 0;
        for &w in self.widths.iter().take(usize::from(self.cursor_row)) {
            let p = cols.map_or(1, |c| w.div_ceil(c.max(1)).max(1));
            if p > left {
                break;
            }
            left -= p;
            rows += 1;
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use ratatui::buffer::Buffer;
    use ratatui::layout::{Rect, Size};
    use ratatui::text::Line;
    use ratatui::widgets::Widget;

    use super::visible_widths;

    use super::{Geometry, Term};
    use crate::ui::render::frame::FrameView;
    use crate::ui::testutil::SharedBuf;

    fn term(w: u16, h: u16, view_h: u16, top: u16) -> (Term<SharedBuf>, SharedBuf, Geometry) {
        let geo = Geometry::new(w, h);
        let buf = SharedBuf::default();
        let wtr = buf.clone();
        let t = Term::new(
            Box::new(move || wtr.clone()),
            view_h,
            top,
            Some(geo.clone()),
        )
        .unwrap();
        (t, buf, geo)
    }

    fn frame() -> FrameView {
        FrameView {
            rows: vec![
                "┄".repeat(60),
                "❯ ".to_owned(),
                "┄".repeat(60),
                "status".to_owned(),
            ],
            cursor: Some((2, 1)),
        }
    }

    fn tall() -> FrameView {
        FrameView {
            rows: (0..9).map(|i| format!("row-{i}")).collect(),
            cursor: Some((2, 6)),
        }
    }

    /// Every row a user can reach: the history (oldest first), then the screen.
    fn reachable(p: &mut vt100::Parser) -> Vec<String> {
        let cols = p.screen().size().1;
        let screen: Vec<String> = p.screen().rows(0, cols).collect();
        p.screen_mut().set_scrollback(usize::MAX);
        let depth = p.screen().scrollback();
        let mut all = Vec::with_capacity(depth + screen.len());
        for off in (1..=depth).rev() {
            p.screen_mut().set_scrollback(off);
            all.push(p.screen().rows(0, cols).next().unwrap_or_default());
        }
        p.screen_mut().set_scrollback(0);
        all.extend(screen);
        all
    }

    /// Replays `bytes` through a `rows`×`cols` emulator that, at byte `mark`, grows by
    /// `pulled` rows the way tmux does: history rows come back onto the top of the screen
    /// (here: `PULLED-k` rows) and everything — the cursor with it — moves down.
    fn replay_pulled(bytes: &[u8], mark: usize, rows: u16, cols: u16, pulled: u16) -> Vec<String> {
        let mut p = vt100::Parser::new(rows, cols, 1000);
        p.process(&bytes[..mark]);
        let (r, c) = p.screen().cursor_position();
        p.screen_mut().set_size(rows + pulled, cols);
        let mut seq = format!("\u{1b}[1;1H\u{1b}[{pulled}L");
        for k in 0..pulled {
            let _ = write!(seq, "\u{1b}[{};1HPULLED-{k}", k + 1);
        }
        let _ = write!(seq, "\u{1b}[{};{}H", r + pulled + 1, c + 1);
        p.process(seq.as_bytes());
        p.process(&bytes[mark..]);
        reachable(&mut p)
    }

    /// A term with 18 transcript rows `t-00..t-17` above a 9-row frame flush with the bottom
    /// of a 27-row screen.
    fn with_transcript() -> (Term<SharedBuf>, SharedBuf, Geometry) {
        let (mut t, buf, geo) = term(120, 27, 9, 18);
        let rows: Vec<String> = (0..18).map(|i| format!("t-{i:02}")).collect();
        t.insert_lines(&rows).unwrap();
        t.draw_frame(&tall()).unwrap();
        (t, buf, geo)
    }

    fn assert_all_once(all: &[String], names: &[String]) {
        for name in names {
            let n = all.iter().filter(|r| r.trim_end() == name).count();
            assert_eq!(n, 1, "{name} reachable {n} times:\n{}", all.join("\n"));
        }
    }

    fn transcript(extra: &[&str], pulled: u16) -> Vec<String> {
        let mut names: Vec<String> = (0..18).map(|i| format!("t-{i:02}")).collect();
        names.extend(extra.iter().map(|s| (*s).to_owned()));
        names.extend((0..pulled).map(|k| format!("PULLED-{k}")));
        names
    }

    /// The acknowledged size is gone (it existed so ratatui's autoresize would not run its
    /// inline resize): nothing but the resize pass reads the size, so a draw after the tty
    /// changed still lays out against the size the last pass took — and never clears.
    #[test]
    fn a_draw_lays_out_against_the_size_the_resize_pass_took() {
        let (mut t, buf, geo) = term(80, 24, 4, 20);
        t.draw_frame(&frame()).unwrap();
        geo.set_size(60, 24);
        let mark = buf.bytes().len();
        t.draw_frame(&frame()).unwrap();
        let drawn = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert!(!drawn.contains("\u{1b}[2J"), "a clear on a draw: {drawn:?}");
        assert!(!drawn.contains("\u{1b}[J"), "an erase on a draw: {drawn:?}");
        assert_eq!(t.size().width, 80);
        t.resize(Size::new(60, 24), 0, |_| 4).unwrap();
        assert_eq!(t.size().width, 60, "taken by `Term::resize`");
    }

    /// The verifier's P0 (2026-09-24), at the seam: in a fast drag tmux has already grown the
    /// screen to 28 rows — pulling a history row back, so the frame and the cursor moved down
    /// one — while both the event and the tty still say 27. The erase is counted up from the
    /// cursor, so the pulled row and every transcript row survive, and the bookkeeping
    /// follows the cursor.
    #[test]
    fn a_resize_on_a_stale_size_never_erases_above_the_frame() {
        let (mut t, buf, geo) = with_transcript();
        let mark = buf.bytes().len();
        geo.set_real_rows(28);
        geo.shift_cursor(1);
        t.resize(Size::new(120, 27), 0, |_| 9).unwrap();
        t.draw_frame(&tall()).unwrap();
        assert_eq!(t.top(), 19, "the frame is where the terminal put it");
        let all = replay_pulled(&buf.bytes(), mark, 27, 120, 1);
        assert_all_once(&all, &transcript(&[], 1));
    }

    /// The cursor answers from a screen that grew AGAIN after the size was read (reported 26,
    /// the cursor on row 27): the cursor wins — the screen has at least 28 rows — and the
    /// anchor is the cursor's frame top.
    #[test]
    fn a_cursor_beyond_the_reported_size_wins_over_it() {
        let (mut t, buf, geo) = with_transcript();
        let mark = buf.bytes().len();
        geo.set_real_rows(31);
        geo.shift_cursor(4); // tmux grew twice, pulling history rows back: the frame moved down 4
        t.resize(Size::new(120, 27), 0, |_| 9).unwrap();
        t.draw_frame(&tall()).unwrap();
        assert_eq!(t.top(), 22, "the anchor is the cursor's frame top");
        let all = replay_pulled(&buf.bytes(), mark, 27, 120, 4);
        assert_all_once(&all, &transcript(&[], 4));
    }

    /// Loss (c) of the 2026-09-25 re-verification — the check→execute window: the DSR answers
    /// from the screen as it was, and tmux applies the next grow (pulling history rows back)
    /// before the pass's bytes arrive. The erase is counted from the cursor, which moved with
    /// the frame: nothing above it is touched, whatever the DSR said.
    #[test]
    fn a_grow_between_the_dsr_and_the_erase_loses_no_row() {
        let (mut t, buf, _geo) = with_transcript();
        let mark = buf.bytes().len();
        // The DSR (geo) still answers the old row; the emulator pulls 3 rows at `mark`.
        t.resize(Size::new(120, 27), 0, |_| 9).unwrap();
        t.draw_frame(&tall()).unwrap();
        let all = replay_pulled(&buf.bytes(), mark, 27, 120, 3);
        assert_all_once(&all, &transcript(&[], 3));
    }

    /// Loss (a) of the 2026-09-25 re-verification — streaming through a height drag: tmux has
    /// grown the screen (history rows pulled back, the frame and the cursor down) and the loop,
    /// which has not read the resize yet, inserts a row and draws. Cursor-relative, the insert
    /// lands right above the frame and the draw on it, so the cursor stays on the frame row the
    /// bookkeeping says, and the pass that follows erases the frame only.
    #[test]
    fn writes_before_the_resize_is_read_keep_the_anchor() {
        let (mut t, buf, geo) = with_transcript();
        let mark = buf.bytes().len();
        geo.set_real_rows(29);
        geo.shift_cursor(2);
        t.insert_lines(&["new-0".to_owned(), "new-1".to_owned()])
            .unwrap();
        t.draw_frame(&tall()).unwrap();
        geo.set_size(120, 29);
        t.resize(Size::new(120, 29), 0, |_| 9).unwrap();
        t.draw_frame(&tall()).unwrap();
        let all = replay_pulled(&buf.bytes(), mark, 27, 120, 2);
        assert_all_once(&all, &transcript(&["new-0", "new-1"], 2));
        let rows: Vec<&String> = all.iter().filter(|r| r.starts_with("row-")).collect();
        assert_eq!(rows.len(), 9, "one frame on screen:\n{}", all.join("\n"));
    }

    /// Loss (b) of the 2026-09-25 re-verification — a terminal that does not answer the DSR,
    /// through a grow that pulled history rows back: the old fallback anchor (the recorded
    /// top) was a row above the frame by then and its erase took a committed row. There is
    /// no fallback anchor now — the erase is counted from the cursor.
    #[test]
    fn a_resize_whose_dsr_fails_loses_no_row_on_a_grow() {
        let (mut t, buf, geo) = with_transcript();
        let mark = buf.bytes().len();
        geo.set_dsr_fails(true);
        geo.set_size(120, 28);
        geo.shift_cursor(1);
        t.resize(Size::new(120, 28), 0, |_| 9).unwrap();
        t.draw_frame(&tall()).unwrap();
        let all = replay_pulled(&buf.bytes(), mark, 27, 120, 1);
        assert_all_once(&all, &transcript(&[], 1));
    }

    /// A terminal that never answers the cursor query (crossterm times out with an error) must
    /// not take the loop down on a resize: the tracked top stays the anchor and the frame is
    /// drawn.
    #[test]
    fn a_resize_whose_dsr_fails_keeps_the_tracked_top_and_does_not_unwind() {
        let (mut t, _buf, geo) = term(80, 24, 4, 20);
        t.draw_frame(&frame()).unwrap();
        geo.set_dsr_fails(true);
        geo.set_size(70, 24);
        t.resize(Size::new(70, 24), 0, |_| 4)
            .expect("a failed DSR must not unwind the resize");
        assert_eq!(t.top(), 20, "the tracked top is the anchor");
        t.draw_frame(&frame()).expect("the frame draws after it");
        assert!(
            t.ensure_height(5).is_ok(),
            "a later height change survives it too"
        );
    }

    fn widths_of(rows: &[&str]) -> Vec<u16> {
        let area = Rect::new(0, 0, 20, u16::try_from(rows.len()).unwrap_or(u16::MAX));
        let mut buf = Buffer::empty(area);
        for (y, row) in rows.iter().enumerate() {
            Line::from(*row).render(Rect::new(0, u16::try_from(y).unwrap_or(0), 20, 1), &mut buf);
        }
        visible_widths(&buf, area)
    }

    /// A wide grapheme owns the cell to its right (ratatui's covered cell reads `" "`), so a
    /// row that ends in one is as wide as that grapheme's RIGHT edge: `❯ 中文` is 6, not 5 —
    /// measured one short, a shrinking row was erased from the middle of its last glyph.
    #[test]
    fn a_row_ending_in_a_wide_grapheme_is_as_wide_as_its_right_edge() {
        assert_eq!(widths_of(&["❯ 中文"]), vec![6]);
        assert_eq!(
            widths_of(&["a👍🏽"]),
            vec![3],
            "a skin-tone emoji is one 2-cell grapheme"
        );
        assert_eq!(widths_of(&["x❤️"]), vec![3], "VS16 makes the heart 2 cells");
        assert_eq!(widths_of(&["ab"]), vec![2]);
        assert_eq!(widths_of(&[""]), vec![0]);
    }
}
