//! Terminal ownership — warts W1 `RECREATE_ON_HEIGHT_CHANGE`, W2 `SELF_TRACKED_TOP`,
//! W3 `CLEAR_AFTER_RECREATE`, W5 `RESIZE_PASS_FIRST`, W6 `LINE_COUNT_SELF_CONSISTENCY`,
//! W9 `PORTABLE_FALLBACK` (`TUI_DESIGN` §4).
//!
//! - **W1**: `Viewport::Inline(h)` is frozen at `Terminal` construction, so any
//!   frame-height change recreates the `Terminal` — `MoveTo(0, top)`, drop, a fresh
//!   `Terminal::with_options(.., Inline(new_h))` (it re-anchors at the cursor row,
//!   scrolling history up when a grow does not fit). Recreation is cheap (a writer
//!   handle); the loop coalesces it to one per iteration.
//! - **W2**: `CompletedFrame.area` lies (`y` is always 0), so the viewport top is
//!   self-tracked: recreation pins `top = min(top, screen_h − new_h)` and every insert
//!   pushes `top = min(top + rows, screen_h − view_height)`. A resize re-derives it
//!   from the one DSR of W5.
//! - **W5**: ratatui's own inline resize is never allowed to run. On a width SHRINK it
//!   pins the viewport to row 0 and sends `ESC[2J` (ratatui-core 0.1.2
//!   `terminal/resize.rs:42-44`): the composer jumps to the top and every transcript row
//!   still on screen is erased. `Terminal::draw` autoresizes by itself, so not calling
//!   `autoresize` is not enough — [`LoopBackend::size`] answers the ACKNOWLEDGED size,
//!   which only [`Term::resize`] changes, and ratatui's `last_known_area` therefore only
//!   moves when this module recreates the `Terminal`. [`Term::resize`] re-anchors the frame
//!   clearing only rows the old frame provably owned, and scrolls or inserts nothing: ONE
//!   DSR before writing a byte (every draw leaves the cursor on a known frame row, and a
//!   resize carries it with its cell); the rows a reflow grew above the cursor once the
//!   emulator has shown that it reflows (a lower bound of our own rows' growth —
//!   [`Term::rewrite_rows`] keeps each row's line exactly as long as what it shows); a
//!   frame flush with the bottom kept flush on the new floor, the rows between left as a
//!   blank band the next output fills ([`Term::pad`]); and staged rows the emulator itself
//!   pushed into the history handed back to be committed where they stand. The frame is
//!   laid out at the terminal's FULL width at rest and a column (or a few) short while a
//!   drag lasts (`Model::frame_width`, the burst layout — the owner's, X-52): only a drag's
//!   first step rewraps the frame's full-width rows, and that growth is ours to claim,
//!   never to predict for transcript rows. Nothing is erased on a stale picture: a pass
//!   takes the size the terminal has NOW (a fast drag's event can be one size behind), and
//!   a recreation checks with a cursor query that the frame's bottom row is where it placed
//!   it before its W3 erase — tmux, grown again mid-drag, pulls history rows back, and an
//!   erase from a top computed on the older size wiped a committed row (P0, 2026-09-24). A band row is filled by the next
//!   output in place, the frame not moving ([`Term::insert_lines`]). An emulator that does
//!   not reflow never has a row above the cursor claimed (DIVERGENCES X-52).
//! - **W3**: a recreated `Terminal` starts with empty buffers while the screen still
//!   shows the old frame; right after recreation the rows from the new top to the screen's
//!   end are erased (wiping shrink-freed rows — below-frame is dead space) so the next draw
//!   repaints every cell. The erase is RELATIVE to the cursor the recreation's line feeds
//!   left on the frame's last row, never an absolute row, and the frame's first row is
//!   checked again after it (stopgap, 2026-09-25). Every move and erase of a resize pass is
//!   relative to the cursor for the same reason: tmux can resize again between a cursor
//!   answer and the next byte, and its grow pulls history rows onto the rows an absolute
//!   move names. Without a cursor answer nothing above the cursor is erased.
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

use crossterm::cursor::{MoveDown, MoveTo, MoveUp};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType as CrosstermClear};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellWidth};
use ratatui::layout::{Position, Size};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

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
    /// Test seam: what the emulator does right AFTER it answers a cursor query — a tmux
    /// resize landing between the answer and the next byte the pass writes. Fires once, on
    /// the query whose index (from 0) it is armed with.
    #[cfg(test)]
    after_query: Arc<Mutex<AfterQuery>>,
}

#[cfg(test)]
type AfterQuery = (usize, Option<(usize, Box<dyn FnOnce() + Send>)>);

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
            after_query: Arc::new(Mutex::new((0, None))),
        }
    }

    /// Runs `f` right after the `nth` cursor query from now (0 = the next one) is answered.
    #[cfg(test)]
    pub(crate) fn after_query(&self, nth: usize, f: impl FnOnce() + Send + 'static) {
        let mut g = crate::sync::lock(&self.after_query);
        let seen = g.0;
        g.1 = Some((seen + nth, Box::new(f)));
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
    fn query_cursor(&self) -> io::Result<Position> {
        if self.dsr_fails.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the terminal did not answer the cursor position query",
            ));
        }
        let answer = self.cursor();
        #[cfg(test)]
        {
            let hook = {
                let mut g = crate::sync::lock(&self.after_query);
                let n = g.0;
                g.0 += 1;
                match g.1.take() {
                    Some((at, f)) if at == n => Some(f),
                    other => {
                        g.1 = other;
                        None
                    }
                }
            };
            if let Some(f) = hook {
                f();
            }
        }
        Ok(answer)
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

    /// Mirrors a RELATIVE move of `rows` rows (negative = up) to column 0: a terminal
    /// clamps it to the screen (a line feed on the last row scrolls, the cursor stays).
    fn step(&self, rows: i32) {
        let y = (i32::from(self.cursor().y) + rows).clamp(0, i32::from(self.last_row()));
        self.set_cursor(Position::new(0, u16::try_from(y).unwrap_or(0)));
    }
}

/// The loop's backend: a [`CrosstermBackend`] whose geometry queries can be answered
/// synthetically ([`Geometry`]) so the byte-emitting stack runs headless. All drawing
/// and scrolling delegates to the inner backend — the emitted bytes are the real ones.
pub(crate) struct LoopBackend<W: Write> {
    inner: CrosstermBackend<W>,
    geo: Option<Geometry>,
    /// The acknowledged terminal size (W5): what [`Term`] last took from a resize event,
    /// never a live query — ratatui's autoresize must not see a change iota has not handled.
    size: Size,
    /// Where the last `set_cursor_position` put the cursor, until a draw moves it (the
    /// home-position erase below needs to know).
    at: Option<Position>,
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
        self.at = None;
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
        if let Some(g) = &self.geo {
            // The synthetic cursor moves like a terminal's: down, clamped to the last row.
            let pos = g.cursor();
            let last = g.last_row();
            g.set_cursor(Position::new(pos.x, pos.y.saturating_add(n).min(last)));
        }
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let answer = match &self.geo {
            Some(g) => g.query_cursor(),
            None => self.inner.get_cursor_position(),
        };
        // ratatui asks inside `with_options` and `clear` (a recreation), each right after we
        // placed the cursor ourselves. A terminal that does not answer (crossterm times out
        // after ~2 s) must not unwind the loop: the position we set is the answer it would give.
        match (answer, self.at) {
            (Err(_), Some(at)) => Ok(at),
            (answer, _) => answer,
        }
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let pos = position.into();
        self.at = Some(pos);
        if let Some(g) = &self.geo {
            g.set_cursor(pos); // the synthetic DSR mirrors what the terminal would say
        }
        self.inner.set_cursor_position(pos)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        // An erase-below FROM THE HOME POSITION is a clear screen to tmux, which (with its
        // default `scroll-on-clear`) files the whole screen into the history first — a
        // frame re-anchored on row 0 (a narrowing that grew the box past the top) left its
        // old copy and the banner above it there. Erasing row 0 by itself and then from row
        // 1 down is the same erase, and not that one.
        if matches!(clear_type, ClearType::AfterCursor) && self.at == Some(Position::ORIGIN) {
            self.inner.clear_region(ClearType::CurrentLine)?;
            self.inner.set_cursor_position(Position::new(0, 1))?;
            self.inner.clear_region(ClearType::AfterCursor)?;
            return self.inner.set_cursor_position(Position::ORIGIN);
        }
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
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
/// change (W1+W3), the self-tracked viewport top (W2), the resize it re-anchors with
/// one DSR (W5), and `line_count`-sized inserts (W6).
pub(crate) struct Term<W: Write> {
    terminal: Terminal<LoopBackend<W>>,
    make_writer: MakeWriter<W>,
    /// Out-of-band control writer (same underlying stream): `MoveTo` ahead of a
    /// recreation, title OSC, the final cursor park.
    ctrl: W,
    geo: Option<Geometry>,
    /// The acknowledged terminal size every backend incarnation answers (W5).
    size: Size,
    /// The self-tracked viewport top row (W2).
    pub(crate) top: u16,
    /// The current inline viewport height.
    pub(crate) view_height: u16,
    /// What the last draw left on screen (W5's anchor); `None` until a draw has placed
    /// the cursor, and again after anything that moves it elsewhere.
    drawn: Option<Drawn>,
    /// Blank rows directly above the frame that a resize left there (W5 step 3): ours to
    /// fill with the next output, so a band never has to scroll into the history.
    pad: u16,
    /// Whether the emulator rewraps its lines on a narrowing — learned from the first
    /// resize that shows it ([`Term::learn_reflow`]); `None` counts as "no".
    reflows: Option<bool>,
    /// An upper bound of each frame row's line length in the emulator (relative to the
    /// top; missing = 0). The W3 clear empties every line; a draw can only lengthen one to
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
        let mut ctrl = make_writer();
        let size = if let Some(g) = &geo {
            g.size()
        } else {
            let (width, height) = crossterm::terminal::size()?;
            Size { width, height }
        };
        anchor(&mut ctrl, geo.as_ref(), start_top)?;
        let backend = LoopBackend {
            inner: CrosstermBackend::new(make_writer()),
            geo: geo.clone(),
            size,
            at: None,
        };
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        let top = start_top.min(size.height.saturating_sub(height));
        Ok(Self {
            terminal,
            make_writer,
            ctrl,
            geo,
            size,
            top,
            view_height: height,
            drawn: None,
            line_len: Vec::new(),
            reflows: None,
            pad: 0,
            last_title: None,
            last_progress: None,
            focus_on: false,
        })
    }

    /// W1 `RECREATE_ON_HEIGHT_CHANGE` + W3 `CLEAR_AFTER_RECREATE`: recreates the
    /// `Terminal` at `new_h` anchored at the tracked top. Returns whether a recreation
    /// happened (the loop coalesces to one per iteration by calling this once).
    pub(crate) fn ensure_height(&mut self, new_h: u16) -> io::Result<bool> {
        if new_h == self.view_height {
            return Ok(false);
        }
        // A taller frame grows into the blank band a resize left above it (W5) before it
        // scrolls anything into the history.
        let grow = new_h.saturating_sub(self.view_height).min(self.pad);
        self.pad -= grow;
        self.recreate(new_h, self.top - grow, false)?;
        Ok(true)
    }

    /// W5 `RESIZE_PASS_FIRST`: takes the new `size` as the acknowledged one and
    /// re-anchors the frame (now `new_h` rows) where the emulator left it. Every row this
    /// clears is one the old frame provably owned; a transcript row is never touched, and
    /// nothing is inserted or scrolled.
    ///
    /// 1. **The cursor estimate.** The ONE DSR comes before any byte is written. The last
    ///    draw left the cursor on frame row `c` — the composer's, or the frame's top-left
    ///    while a surface hides it — and a resize carries it with its cell, so the `c`
    ///    rows above it and everything below it are the old frame's: `start = cursor − c`.
    ///    Without a known row (nothing drawn since the last recreation or insert)
    ///    `start` is the tracked top.
    /// 2. **The overhang.** What a reflow grew ABOVE the cursor lies above `start`: the
    ///    top separator's second piece, a staged row that wrapped. It is ours only on an
    ///    emulator that reflows, so it is claimed only once the emulator has SHOWN that
    ///    it does ([`Term::learn_reflow`]) and then by a lower bound of our own rows'
    ///    growth ([`Drawn::growth_above`]). Unknown or not reflowing, nothing above
    ///    `start` is touched; short of the truth, a row is duplicated, never lost. (A
    ///    hidden cursor sits on the frame's first row: nothing of the frame is above it.)
    /// 3. **The floor.** A frame that was flush with the bottom stays flush:
    ///    `top = S' − new_h`. An emulator keeps its last row on the bottom through a
    ///    reflow — the cursor estimate alone lags whatever grew BELOW the cursor — and a
    ///    row grow pulls history in above it or adds blank rows below it. The rows between
    ///    `start` and the floor are old-frame rows: they are cleared and left as a blank
    ///    BAND between the transcript and the frame ([`Term::pad`]), which the next output
    ///    fills before anything scrolls. (Scrolling it away instead put blank rows at the
    ///    top of the screen, and the next reflow pushed them into the history — two per
    ///    step of a drag.) tmux eats the rows below the cursor on a row shrink: the cursor
    ///    then lands on the bottom row, the floor lies above `start`, and step 4 applies.
    /// 4. Otherwise the frame is recreated at `start`: `with_options` scrolls the
    ///    transcript into the history when the frame does not fit below it.
    /// 5. **Rows the emulator already archived.** A frame near the top that grows in a
    ///    reflow — a drastic narrowing right after startup — has its first rows pushed
    ///    off the screen into the history. Whole rows counted by the same lower bound
    ///    ([`Drawn::rows_pushed`]), up to `droppable` (the staged rows the frame opens
    ///    with), are returned: the caller drops them from the frame and the staging window,
    ///    since drawing them again would show them twice. `height(dropped)` is the frame
    ///    height without them.
    pub(crate) fn resize(
        &mut self,
        size: Size,
        droppable: u16,
        height: impl FnOnce(u16) -> u16,
    ) -> io::Result<u16> {
        // The size the terminal has NOW: in a fast drag the event's size can already be stale
        // (the next resize applied, its event not yet read), and every row this pass computes
        // must match the screen the cursor query is answered from.
        let size = self
            .real_size()
            .filter(|s| s.width > 0 && s.height > 0)
            .unwrap_or(size);
        let old = self.size;
        let flush = self.top.saturating_add(self.view_height) >= old.height;
        let mut size = size;
        let Some(d) = self.drawn.take() else {
            // Nothing on screen yet (only before the first draw): the tracked top.
            self.size = size;
            let at = self.top.min(size.height.saturating_sub(1));
            self.recreate(height(0), at, false)?;
            return Ok(0);
        };
        // A DSR that fails (crossterm's ~2 s timeout, a terminal that never answers) is not
        // a reason to unwind the loop — nor to erase on a guess: without the cursor's row
        // nothing ABOVE the cursor is touched ([`Term::resize_blind`]).
        let Ok(pos) = self.cursor_position() else {
            self.size = size;
            self.resize_blind(&d, height(0))?;
            return Ok(0);
        };
        // The cursor is the one current fact: a cursor below the last row of the size just
        // read means the terminal grew again in between — it has at least that many rows.
        // (Clamping the cursor to the stale size put the anchor on transcript rows.)
        if pos.y >= size.height {
            size.height = pos.y.saturating_add(1);
        }
        let cur = pos.y;
        if size.width < old.width {
            self.learn_reflow(&d, old, size, cur, flush);
        }
        let cols = (size.width < old.width && self.reflows == Some(true)).then_some(size.width);
        let above = d
            .cursor_row
            .saturating_add(cols.map_or(0, |c| d.growth_above(c)));
        let start = cur.saturating_sub(above);
        let dropped = d
            .rows_pushed(above.saturating_sub(cur), cols)
            .min(droppable);
        self.size = size;
        let new_h = height(dropped);
        let floor = size.height.saturating_sub(new_h);
        // Every move from here is RELATIVE to the cursor (stopgap, 2026-09-25): tmux can
        // resize again between the answer and these bytes — a grow pulls history rows back
        // and moves everything, the cursor included, down — and a row named by the answer
        // then holds a committed row. The cursor stays on its cell; so does a move from it.
        self.step(-i32::from(cur - start))?;
        if flush && floor > start {
            self.erase_below(start == 0)?;
            self.step(i32::from(floor - start))?;
            self.pad = self.pad.saturating_add(floor - start);
            self.recreate(new_h, floor, true)?;
        } else {
            self.recreate(new_h, start, true)?;
        }
        Ok(dropped)
    }

    /// A resize with no cursor answer (stopgap, 2026-09-25 — the verifier's DSR proxy lost a
    /// row on a height round trip, the tracked top the anchor of an erase on a screen that
    /// had grown under it). The last draw left the cursor on frame row `c`; nothing above it
    /// is erased — its rows stay, duplicated, as a residual — and the frame goes down onto
    /// the floor through rows that are provably blank: erased from the cursor row down,
    /// `new_h − 1` line feeds (scrolling the screen as far as needed, never overwriting) put
    /// at least `new_h` blank rows at the bottom, and `S − new_h` is the first of them.
    fn resize_blind(&mut self, d: &Drawn, new_h: u16) -> io::Result<()> {
        self.step(0)?;
        self.erase_below(d.top.saturating_add(d.cursor_row) == 0)?;
        for _ in 1..new_h {
            self.ctrl.write_all(b"\n")?;
        }
        self.ctrl.flush()?;
        if let Some(g) = &self.geo {
            g.step(i32::from(new_h.saturating_sub(1)));
        }
        // The blank rows between the kept rows and the floor are no band a write can fill.
        self.pad = 0;
        let floor = self.size.height.saturating_sub(new_h);
        self.recreate(new_h, floor, false)
    }

    /// Moves the cursor `rows` rows (negative = up) to column 0, relative to where it is.
    fn step(&mut self, rows: i32) -> io::Result<()> {
        let n = u16::try_from(rows.unsigned_abs()).unwrap_or(u16::MAX);
        match rows.signum() {
            -1 => queue!(self.ctrl, MoveUp(n))?,
            1 => queue!(self.ctrl, MoveDown(n))?,
            _ => {}
        }
        self.ctrl.write_all(b"\r")?;
        self.ctrl.flush()?;
        if let Some(g) = &self.geo {
            g.step(rows);
        }
        Ok(())
    }

    /// Erases from the cursor's row (the cursor at column 0) to the end of the screen. On
    /// the screen's first row (`home`) as an erase of the row and one from the next row
    /// down, never an erase-below from the home position, which tmux takes for a clear
    /// screen and files into the history (see [`LoopBackend`]'s `clear_region`). The cursor
    /// ends where it was.
    fn erase_below(&mut self, home: bool) -> io::Result<()> {
        if home {
            queue!(
                self.ctrl,
                Clear(CrosstermClear::CurrentLine),
                MoveDown(1),
                Clear(CrosstermClear::FromCursorDown),
                MoveUp(1)
            )?;
        } else {
            queue!(self.ctrl, Clear(CrosstermClear::FromCursorDown))?;
        }
        self.ctrl.flush()
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
            .view_height
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

    /// The W1 recreation proper: `MoveTo(0, at)`, a fresh `Inline(new_h)` terminal over
    /// the acknowledged size (`with_options` scrolls history up when the frame does not
    /// fit below `at`), the W2 pin, and the W3 clear from the new top down.
    ///
    /// `placed`: the caller already moved the cursor onto the frame's first row (`at` is
    /// then only where it believes that row is) — a resize pass, whose moves are relative.
    fn recreate(&mut self, new_h: u16, at: u16, placed: bool) -> io::Result<()> {
        // A FRESH observation before the W3 erase (P0, 2026-09-24). `with_options` places the
        // frame by counting the newlines that scroll past the bottom of the screen it BELIEVES
        // in — and in a fast drag neither the resize event nor the tty's own size is current
        // (tmux applies the next resize, pulling history rows back on a grow, before either
        // says so): fewer newlines scroll than counted, the computed top lands above the
        // frame, and the erase from there wiped a committed row. The one current fact is the
        // cursor: the newlines leave it on the frame's REAL bottom row. If it is not where the
        // computation expects, the screen has at least (exactly, when it is shorter) that many
        // rows; re-anchor the frame on the observed rows and check again — only then erase.
        // A cursor query that fails keeps the computed top.
        let (mut at, mut placed) = (at, placed);
        let mut tries = 0;
        let area = loop {
            tries += 1;
            self.construct(new_h, at, placed)?;
            let area = self.terminal.get_frame().area();
            let expected = area.bottom().saturating_sub(1);
            if let Ok(cur) = self.cursor_position()
                && cur.y != expected
                && tries < 3
            {
                let rows = if cur.y < expected {
                    cur.y.saturating_add(1)
                } else {
                    self.size.height.max(cur.y.saturating_add(1))
                };
                self.size.height = rows;
                at = cur.y.saturating_sub(area.height.saturating_sub(1));
                placed = false;
                continue;
            }
            self.top = area.y.min(self.size.height.saturating_sub(new_h)); // W2
            // W3, RELATIVE to the cursor (stopgap, 2026-09-25): the line feeds left it on the
            // frame's last row, and the erase starts `new_h − 1` rows up from THERE — the
            // frame's first row wherever the emulator has moved the screen since the check
            // (tmux, growing again, pulls history rows back onto the rows an absolute erase
            // named: the verifier's scroll-on-clear race). A fresh `Terminal`'s buffers are
            // empty already.
            self.step(-i32::from(area.height.saturating_sub(1)))?;
            self.erase_below(self.top == 0)?;
            // And the frame must still be where the draw will write it: a screen that moved
            // after the check moved the erased rows and the cursor with them — the frame is
            // rebuilt on the rows the cursor names, and they are checked again.
            match self.cursor_position() {
                Ok(cur) if cur.y != self.top && tries < 3 => {
                    let bottom = cur.y.saturating_add(area.height);
                    self.size.height = self.size.height.max(bottom);
                    at = cur.y;
                    placed = true;
                }
                _ => break area,
            }
        };
        self.view_height = new_h;
        self.pad = self.pad.min(self.top);
        self.terminal.backend_mut().at = Some(Position::new(0, self.top));
        // Every line empty, the cursor on the frame's top-left: a resize before the next draw
        // anchors on it like on any drawn frame.
        self.drawn = Some(Drawn {
            top: self.top,
            widths: vec![0; usize::from(area.height)],
            cursor_row: 0,
            cursor_x: 0,
        });
        self.line_len.clear();
        Ok(())
    }

    /// `MoveTo(0, at)` and a fresh `Inline(new_h)` terminal over the acknowledged size —
    /// `with_options` scrolls history up when the frame does not fit below `at`. No erase.
    fn construct(&mut self, new_h: u16, at: u16, placed: bool) -> io::Result<()> {
        if !placed {
            anchor(&mut self.ctrl, self.geo.as_ref(), at)?;
        }
        let backend = LoopBackend {
            inner: CrosstermBackend::new((self.make_writer)()),
            geo: self.geo.clone(),
            size: self.size,
            // Where `anchor` (or the caller's relative moves) put the cursor — the answer to
            // a cursor query that fails.
            at: Some(Position::new(0, at)),
        };
        self.terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(new_h),
            },
        )?;
        Ok(())
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

    /// Commits pre-wrapped rows into native scrollback via `insert_before`, sized by
    /// W6 `LINE_COUNT_SELF_CONSISTENCY`, then updates the tracked top (W2:
    /// push-down-then-pin — ratatui's own internal algorithm). Returns the row count.
    /// Closes the band a resize left above the frame (W5 step 3) when the drag is over: the
    /// region above the frame scrolls down over it, so the transcript sits on the frame again
    /// and the band's rows go to the top of the screen — never a hole the next output starts
    /// in. Once per drag, never inside a resize pass (a scroll during the drag would leave
    /// blank rows at the top for the drag's next reflow to push into the history). The
    /// cursor ends at home; the caller redraws.
    pub(crate) fn close_band(&mut self) -> io::Result<()> {
        let pad = std::mem::take(&mut self.pad).min(self.top);
        if pad == 0 {
            return Ok(());
        }
        write!(self.ctrl, "\x1b[1;{}r\x1b[{pad}T\x1b[r", self.top)?;
        self.ctrl.flush()?;
        // The frame did not move; the cursor (homed by the region reset) goes back onto its
        // top-left, where a resize before the next draw finds it.
        anchor(&mut self.ctrl, self.geo.as_ref(), self.top)?;
        self.drawn = self.drawn.take().map(|d| d.parked(self.top));
        Ok(())
    }

    pub(crate) fn insert_lines(&mut self, rows: &[String]) -> io::Result<u16> {
        if rows.is_empty() {
            return Ok(0);
        }
        let size = self.size;
        let width = size.width.max(1);
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
        let pad = self.pad.min(self.top);
        if pad > 0 {
            let k = pad.min(total);
            let rest = lines.split_off(usize::from(k));
            let area = ratatui::layout::Rect::new(0, self.top - pad, width, k);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            Paragraph::new(Text::from(lines)).render(area, &mut buf);
            let backend = self.terminal.backend_mut();
            backend.draw(
                buf.content
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !is_blank(c))
                    .map(|(i, c)| {
                        let i = u16::try_from(i).unwrap_or(u16::MAX);
                        (area.x + i % width, area.y + i / width, c)
                    }),
            )?;
            Backend::flush(backend)?;
            self.pad = pad - k;
            lines = rest;
        }
        if !lines.is_empty() {
            let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
            self.terminal.insert_before(n, |buf| {
                let area = buf.area;
                para.render(area, buf);
            })?;
            self.top = self
                .top
                .saturating_add(n)
                .min(size.height.saturating_sub(self.view_height));
        }
        // The insert left the cursor above the frame, which moved whole (or not at all): the
        // cursor goes onto its top-left, where a resize before the next draw finds it — a
        // resize with no frame row known to hold the cursor could only guess (1b, 2026-09-25).
        if let Some(d) = self.drawn.take() {
            self.terminal
                .set_cursor_position(Position::new(0, self.top))?;
            Backend::flush(self.terminal.backend_mut())?;
            self.drawn = Some(d.parked(self.top));
        }
        Ok(total)
    }

    /// Full clear from the viewport top to screen end + back-buffer reset (the W3
    /// primitive; also the T-32 `tea.ClearScreen` twin for surface Tab switches).
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.line_len.clear();
        self.terminal.clear()
    }

    /// The acknowledged terminal size (W5) — what the frame is laid out against.
    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// W4 `DRAW_WITH_INSERTS` end step: renders the frame rows top-down into the
    /// inline viewport and places the REAL cursor (`None` = hidden, surface open). Either
    /// way the physical cursor ends on a known frame row — the cursor's, or the frame's
    /// last row while it is hidden — which is what W5's resize anchor reads back.
    pub(crate) fn draw_frame(&mut self, view: &FrameView) -> io::Result<()> {
        let lines: Vec<Line<'static>> = view.rows.iter().map(|r| ansi_to_spans(r)).collect();
        let cursor = view.cursor;
        let line_len = std::mem::take(&mut self.line_len);
        let mut placed = None;
        self.terminal.draw(|f| {
            let area = f.area();
            f.render_widget(Paragraph::new(Text::from(lines)), area);
            let buf = f.buffer_mut();
            let widths = visible_widths(buf, area);
            // A row whose line may run past what it now shows: ratatui's diff writes a
            // SPACE into each cell that went blank, and an emulator counts written cells as
            // line length — even after an `EL` (tmux does). Its cells go along to be
            // written again over an erased line.
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
            let pos = match cursor {
                Some((x, y)) => {
                    let pos = Position::new(
                        x.min(area.width.saturating_sub(1)),
                        area.y.saturating_add(y).min(bottom),
                    );
                    f.set_cursor_position(pos);
                    pos
                }
                None => Position::new(0, area.y),
            };
            placed = Some((pos, area.y, widths, redraw));
        })?;
        let Some((pos, top, widths, redraw)) = placed else {
            return Ok(());
        };
        // What the diff may have written: nothing past the longer of the old line and the
        // new content.
        self.line_len = widths
            .iter()
            .enumerate()
            .map(|(i, &w)| w.max(line_len.get(i).copied().unwrap_or(0)))
            .collect();
        let rewrote = !redraw.is_empty();
        if rewrote {
            self.rewrite_rows(&redraw, &widths, top)?;
        }
        if cursor.is_none() || rewrote {
            // Hidden: park it on the frame's top-left, where the next resize can find it
            // with no frame row above it to rewrap. Cursor-invisible, so the move costs
            // nothing on screen. (Visible, after a rewrite: back where the draw put it.)
            self.terminal.set_cursor_position(pos)?;
        }
        self.drawn = Some(Drawn {
            top,
            widths,
            cursor_row: pos.y.saturating_sub(top),
            cursor_x: pos.x,
        });
        Ok(())
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
        let backend = self.terminal.backend_mut();
        for (y, cells) in rows {
            backend.set_cursor_position(Position::new(0, *y))?;
            backend.clear_region(ClearType::CurrentLine)?;
            backend.draw(
                cells
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !is_blank(c))
                    .map(|(x, c)| (u16::try_from(x).unwrap_or(u16::MAX), *y, c)),
            )?;
            let i = usize::from(y.saturating_sub(top));
            if let (Some(len), Some(&w)) = (self.line_len.get_mut(i), widths.get(i)) {
                *len = w;
            }
        }
        Backend::flush(backend)
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
    /// for what a frame taller than the last one may have left below it: the exit round
    /// repaints the frame without the flushed staging window, and W3's clear covers the
    /// old rows only when the height changed.
    pub(crate) fn park_cursor(&mut self) -> io::Result<()> {
        let bottom = self.top.saturating_add(self.view_height);
        queue!(self.ctrl, MoveTo(0, bottom.saturating_sub(1)))?;
        self.ctrl.write_all(b"\r\n")?;
        queue!(self.ctrl, Clear(CrosstermClear::FromCursorDown))?;
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
    /// The same frame at `top` with the cursor parked on its top-left.
    fn parked(self, top: u16) -> Self {
        Self {
            top,
            cursor_row: 0,
            cursor_x: 0,
            ..self
        }
    }

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

/// A cell that shows nothing: a space with no background and no modifier.
fn is_blank(c: &Cell) -> bool {
    c.symbol() == " " && c.bg == ratatui::style::Color::Reset && c.modifier.is_empty()
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

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
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

    /// The acknowledged-size CANARY (W5): the backend keeps answering the size iota last took
    /// from a resize event, whatever the terminal reports, until `Term::resize` acknowledges
    /// the new one. If a ratatui upgrade or a refactor broke this seam, `Terminal::draw`'s own
    /// autoresize would run ratatui's inline resize again (row 0 + `ESC[2J` on a narrowing).
    #[test]
    fn the_backend_answers_the_acknowledged_size_until_iota_acknowledges_a_new_one() {
        let (mut t, buf, geo) = term(80, 24, 4, 20);
        t.draw_frame(&frame()).unwrap();
        geo.set_size(60, 24);
        assert_eq!(
            t.terminal.size().unwrap().width,
            80,
            "the backend must not read the tty"
        );
        let mark = buf.bytes().len();
        t.draw_frame(&frame()).unwrap(); // `draw` autoresizes — against the acknowledged size
        let drawn = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert!(
            !drawn.contains("\u{1b}[2J"),
            "ratatui's inline resize ran: {drawn:?}"
        );
        assert_eq!(t.terminal.size().unwrap().width, 80);
        t.resize(
            ratatui::layout::Size {
                width: 60,
                height: 24,
            },
            0,
            |_| 4,
        )
        .unwrap();
        assert_eq!(
            t.terminal.size().unwrap().width,
            60,
            "acknowledged by `Term::resize`"
        );
    }

    /// The verifier's P0 (2026-09-24), at the seam: in a fast drag tmux has already grown the
    /// screen to 28 rows — pulling a history row back, so the frame and the cursor moved down
    /// one — while both the event and the tty still say 27. A recreation computed on 27 rows
    /// counts one newline-scroll too many and puts the top a row above the frame; it must see
    /// the cursor land a row lower than it expected and re-anchor before the W3 erase, which
    /// used to wipe the committed row above the frame.
    #[test]
    fn a_resize_on_a_stale_size_never_erases_above_the_frame() {
        let (mut t, buf, geo) = term(120, 27, 9, 18);
        let tall = FrameView {
            rows: (0..9).map(|i| format!("row-{i}")).collect(),
            cursor: Some((2, 6)),
        };
        t.draw_frame(&tall).unwrap();
        // tmux: 27 → 28, one history row pulled back; the reports lag.
        geo.set_real_rows(28);
        geo.shift_cursor(1);
        let mark = buf.bytes().len();
        t.resize(
            ratatui::layout::Size {
                width: 120,
                height: 27,
            },
            0,
            |_| 9,
        )
        .unwrap();
        let after = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert_eq!(t.top, 19, "the frame is where the terminal put it");
        for row in 1..=19 {
            assert!(
                !after.contains(&format!("\u{1b}[{row};1H\u{1b}[J")),
                "an erase from row {row}, above the frame (row 20): {after:?}"
            );
        }
    }

    /// The cursor answers from a screen that grew AGAIN after the size was read (reported 26,
    /// the cursor on row 27): the cursor wins — the screen has at least 28 rows — and the
    /// anchor is the cursor's frame top, never a top computed from a cursor clamped to the
    /// stale size (which put the anchor two rows up, on the transcript, and erased them).
    #[test]
    fn a_cursor_beyond_the_reported_size_wins_over_it() {
        let (mut t, buf, geo) = term(120, 26, 9, 17);
        let tall = FrameView {
            rows: (0..9).map(|i| format!("row-{i}")).collect(),
            cursor: Some((2, 6)),
        };
        t.draw_frame(&tall).unwrap();
        geo.set_real_rows(30);
        geo.shift_cursor(4); // tmux grew twice, pulling history rows back: the frame moved down 4
        let mark = buf.bytes().len();
        t.resize(
            ratatui::layout::Size {
                width: 120,
                height: 26,
            },
            0,
            |_| 9,
        )
        .unwrap();
        let after = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert!(
            t.top >= 21,
            "the anchor is the cursor's frame top (21), got {}",
            t.top
        );
        for row in 1..=21 {
            assert!(
                !after.contains(&format!("\u{1b}[{row};1H\u{1b}[J")),
                "an erase from row {row}, above the frame (row 22): {after:?}"
            );
        }
    }

    /// A terminal that never answers the cursor query (crossterm times out with an error) must
    /// not take the loop down on a resize, nor erase on a guess (1c, 2026-09-25: the tracked
    /// top as the anchor of an erase lost a row on a height round trip): the first erase is
    /// from the cursor's own row, nothing above it is touched, the frame goes onto the floor
    /// through the rows it cleared, and the frame is drawn.
    #[test]
    fn a_resize_whose_dsr_fails_erases_nothing_above_the_cursor_and_does_not_unwind() {
        let (mut t, buf, geo) = term(80, 24, 4, 20);
        t.draw_frame(&frame()).unwrap();
        geo.set_dsr_fails(true);
        geo.set_size(70, 24);
        let mark = buf.bytes().len();
        t.resize(
            ratatui::layout::Size {
                width: 70,
                height: 24,
            },
            0,
            |_| 4,
        )
        .expect("a failed DSR must not unwind the resize");
        let after = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert!(
            after.starts_with("\r\u{1b}[J"),
            "the first erase is from the cursor's row, with no move up before it: {after:?}"
        );
        assert_eq!(t.top, 20, "the frame is on the floor");
        t.draw_frame(&frame()).expect("the frame draws after it");
        assert!(
            t.ensure_height(5).is_ok(),
            "a later recreation survives it too"
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
