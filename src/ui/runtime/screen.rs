//! The full-screen terminal (`docs/design/fullscreen-tui.md`).
//!
//! iota owns the alternate screen (`ESC[?1049h` on entry, `l` on exit) and draws every frame
//! with ratatui's own [`Terminal`] in [`Viewport::Fullscreen`]: the alternate screen has no
//! history and no reflow, so every byte may name a row.
//!
//! The screen is the conversation's rectangle with the frame (the loop's [`FrameView`] rows,
//! pinned to the bottom) drawn over its last rows. The rectangle is what the frame leaves with no
//! completion list open; the list's area ([`FrameView::float`]) floats over it, so opening,
//! narrowing or closing the list never moves the conversation. The VISIBLE rows are the ones above
//! the frame: only they are drawn, marked for the selection and the mouse, and scrolled; a covered
//! row is laid out and followed like any other but has no mark. A conversation shorter than its
//! rectangle sits on the frame, so its blank rows take the area before any of its lines is
//! covered. The conversation is a buffer of committed
//! BLOCKS ([`Conversation`], `ui::block`): fixed rows, and blocks laid out from their source at
//! the width of the draw. A block's display lines are computed when a draw first needs them at a
//! width and cached with it (a row wider than the screen is hard-wrapped by `wrap_ansi`), and
//! only the lines a draw shows go through `ansi_to_spans`.
//!
//! - **A frame** is one `Terminal::draw`: autoresize, layout, `Buffer::diff`, the cursor. The
//!   bytes collect in [`SyncOut`] and leave in one write per loop iteration, under X-55's rule
//!   (a write longer than `SYNC_MIN` is one DEC 2026 synchronized update).
//! - **A resize** is a size handed to the backend; the next draw clears the screen and repaints
//!   every cell, at a changed size or the one it last drew. No cursor query is ever made ([`Out`]
//!   answers ratatui's own `get_cursor_position` from the last position it set).
//! - **The point of no return** is the first frame: until it is drawn nothing reaches the
//!   terminal, so a run that fails before it leaves the shell's screen as it was.
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
use ratatui::style::Modifier;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::ui::render::frame::FrameView;
use crate::ui::render::selection::{self, Point, Selection};
use crate::ui::render::spans::ansi_to_spans;
use crate::ui::render::theme::{FAINT, RESET, REV_ON};

/// Whether a live [`Screen`] holds the alternate screen now — set by the live constructor only
/// (`Term::new`), cleared by [`Screen::leave`] and by `restore_terminal`, which reads it.
pub(crate) static ALT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The bytes that hand the main screen back: the cursor shown, the alternate screen left.
pub(crate) const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// Mouse reporting on (the config's `mouse:`, on by default — tui-plan D1): presses and releases
/// (1000), motion while a button is held (1002, a drag — not 1003's every hover), SGR coordinates
/// (1006, no 223-column ceiling). Requested on entering the alternate screen and dropped on every
/// way out of it, so the main screen — a write-out, a suspend, the exit — always has the terminal's
/// own selection back.
pub(crate) const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
/// See [`MOUSE_ON`].
pub(crate) const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// Whether a live [`Screen`] has mouse reporting on now — kept beside [`ALT_ACTIVE`], for
/// `restore_terminal`.
pub(crate) static MOUSE_ACTIVE: AtomicBool = AtomicBool::new(false);

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
    /// The wheel: this many lines toward the tail (negative: toward the start). Like `PgUp`, a
    /// step up leaves the tail; reaching the bottom follows it again.
    Wheel(i32),
    /// A mouse step of the in-app selection over the conversation (`render/selection.rs`).
    Select(SelectOp),
}

/// The columns a drag must cover on its own line to select (and copy) anything.
const DRAG_MIN_COLS: usize = 2;

/// A mouse step of the selection, in screen cells (column, row from the top).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectOp {
    /// The left button went down; `clicks` is 1, 2 (a word) or 3 (the whole row).
    Press { col: u16, row: u16, clicks: u8 },
    /// The pointer moved with the button held. At the viewport's top row or below its last one
    /// the view scrolls a line first, so a selection can cross screens.
    Drag { col: u16, row: u16 },
    /// The button came up: a drag, a word or a row is copied ([`Screen::take_copied`]); a plain
    /// click selects nothing.
    Release,
    /// The selection is dropped (a key was pressed).
    Clear,
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
    /// The conversation's rectangle at the last draw (what half a screen is): the rows above the
    /// frame without its floating rows.
    view_h: u16,
    /// The rows above the frame at the last draw: the conversation's rows not covered.
    vis_h: u16,
    /// The open batches' depth, shared with the [`SyncOut`] under the backend.
    batch: Arc<AtomicUsize>,
    /// The last frame drawn, while it is what the screen shows (a clear or a resize forgets it).
    last: Option<Shown>,
    /// Which screen the terminal shows ([`Place`]).
    place: Place,
    /// Whether this screen is the process's terminal: only then is [`ALT_ACTIVE`] kept, for
    /// `restore_terminal` (a test's screen must never make it write to the real stdout).
    live: bool,
    /// Rows the dumps and the exit have written into the main screen so far (each is written
    /// once).
    written: u64,
    /// The search under way, if one is.
    search: Option<Search>,
    /// A resize happened with a search on: the next draw puts the view back on its hit.
    rejump: bool,
    /// A resize was taken since the last draw: the next one clears and repaints every cell, even
    /// at the size it last drew (tmux ends a burst that comes back where it began with `SIGWINCH`
    /// signals the loop merges into that size, after cropping and regrowing the pane's grid).
    resized: bool,
    /// Whether mouse reporting is requested while the alternate screen is shown ([`MOUSE_ON`]);
    /// `false` never writes a mouse sequence at all.
    mouse: bool,
    /// The selection over the conversation, while there is one.
    sel: Option<Selection>,
    /// Whether the button that made [`Screen::sel`] is still down, and whether it has moved (a
    /// word or a row counts as moved: it copies on release).
    drag: Option<bool>,
    /// What each visible row showed at the last draw, top to bottom: (row number, line), `None`
    /// for a blank row, the trimmed marker, the search line or the way back to the tail. A row
    /// the frame covers has no mark.
    marks: Vec<Option<(u64, usize)>>,
    /// The text the last release selected, until the loop copies it.
    copied: Option<String>,
}

/// Which screen the terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The main screen, before the first frame: the alternate screen is entered by that frame
    /// and not before — a run that fails earlier never leaves the shell's screen (tui-plan S4).
    Main,
    /// The alternate screen.
    Alt,
    /// The main screen, the conversation written out on it (`Ctrl+O`) until the next key.
    Out,
    /// The main screen again, for good: the exit wrote the conversation out.
    Left,
}

/// A drawn frame and the layout it was drawn with — what a scroll of the viewport starts from.
struct Shown {
    buf: Buffer,
    view_h: u16,
    /// The visible rows.
    vis: u16,
    /// The screen row under the conversation's last line.
    bottom: u16,
    following: bool,
}

impl<W: Write> Screen<W> {
    /// The full screen on `out` (`size` is the terminal's), writing nothing yet: the first frame
    /// enters the alternate screen ([`Place::Main`]). `live`: `out` is the process's terminal.
    pub(crate) fn new(out: W, size: Size, live: bool, mouse: bool) -> io::Result<Self> {
        let batch = Arc::new(AtomicUsize::new(0));
        let out = SyncOut {
            out,
            pending: Vec::new(),
            batch: Arc::clone(&batch),
        };
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
            vis_h: size.height,
            batch,
            last: None,
            place: Place::Main,
            live,
            search: None,
            rejump: false,
            resized: false,
            written: 0,
            mouse,
            sel: None,
            drag: None,
            marks: Vec::new(),
            copied: None,
        })
    }

    /// The size the screen is laid out at (the last one [`Screen::resize`] took).
    pub(crate) fn size(&self) -> Size {
        self.term.backend().size
    }

    /// Takes the terminal's new size; the next draw clears the screen and repaints every cell,
    /// whether the size changed or not — the whole resize protocol.
    pub(crate) fn resize(&mut self, size: Size) {
        self.term.backend_mut().size = size;
        self.resized = true;
        // A selection is made at a width; another width wraps its rows differently.
        self.sel = None;
        self.rejump = self.search.is_some();
        self.drag = None;
        self.conv.release();
    }

    /// The rows above the frame at the last draw (the conversation's visible rows).
    pub(crate) fn view_height(&self) -> u16 {
        self.vis_h
    }

    /// The text the last release selected, once (the loop hands it to the clipboard).
    pub(crate) fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    /// Appends committed blocks to the conversation. Nothing is written: the next draw lays out
    /// what it shows.
    pub(crate) fn push(&mut self, blocks: Vec<Block>) {
        self.conv.push(blocks);
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
        // Half of what is SEEN: under the list's area the rectangle is taller than the screen
        // shows, and half of it would page past a line never shown.
        let half = usize::from(self.vis_h / 2).max(1);
        match op {
            ViewOp::HalfUp => self.conv.scroll_up(half, width, h),
            ViewOp::HalfDown => self.conv.scroll_down(half, width, h),
            ViewOp::Top => self.conv.go_top(width, h),
            ViewOp::Bottom => self.conv.go_bottom(),
            ViewOp::Dump => self.dump()?,
            ViewOp::Return => self.back()?,
            ViewOp::Search(op) => self.search_op(op, width, h),
            ViewOp::Wheel(n) => {
                let lines = usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
                if n < 0 {
                    self.conv.scroll_up(lines, width, h);
                } else {
                    self.conv.scroll_down(lines, width, h);
                }
            }
            ViewOp::Select(op) => self.select(op, width, h),
        }
        Ok(())
    }

    /// One step of the selection (`SelectOp`; `render/selection.rs` for what is selected).
    fn select(&mut self, op: SelectOp, width: usize, h: usize) {
        match op {
            SelectOp::Press { col, row, clicks } => {
                self.sel = None;
                self.drag = None;
                let Some(Some((n, line))) = self.marks.get(usize::from(row)).copied() else {
                    return;
                };
                let col = usize::from(col);
                let at = |col| Point { row: n, line, col };
                let sel = match clicks {
                    1 => Some(Selection {
                        anchor: at(col),
                        head: at(col),
                    }),
                    2 => self
                        .conv
                        .plain_lines(n, width)
                        .and_then(|l| selection::word_at(&l.get(line)?.0, col))
                        .map(|(a, b)| Selection {
                            anchor: at(a),
                            head: at(b),
                        }),
                    _ => self.conv.plain_lines(n, width).map(|l| {
                        let (first, last) = selection::line_around(&l, line);
                        Selection {
                            anchor: Point {
                                row: n,
                                line: first,
                                col: 0,
                            },
                            head: Point {
                                row: n,
                                line: last,
                                col: width,
                            },
                        }
                    }),
                };
                if sel.is_some() {
                    self.sel = sel;
                    self.drag = Some(clicks > 1);
                    // The text under the pointer stays put while the button is down.
                    self.conv.hold(width, h);
                }
            }
            SelectOp::Drag { col, row } => {
                if self.drag.is_none() {
                    return;
                }
                let last = self.marks.len().saturating_sub(1);
                if row == 0 {
                    self.conv.scroll_up(1, width, h);
                    self.mark_view(width);
                } else if usize::from(row) >= last {
                    // Faster the further below the viewport the pointer is: the frame's rows give
                    // the distance (1, 3, 5 … lines a step).
                    let past = usize::from(row) - last;
                    self.conv.scroll_down(1 + 2 * past, width, h);
                    self.mark_view(width);
                }
                let head = self.point_near(col, row, width);
                if let (Some(sel), Some(head)) = (&mut self.sel, head) {
                    sel.head = head;
                    // A one-cell jitter of a click is not a selection: it would overwrite the
                    // clipboard with a character or two.
                    let a = sel.anchor;
                    let far = (a.row, a.line) != (head.row, head.line)
                        || a.col.abs_diff(head.col) >= DRAG_MIN_COLS;
                    self.drag = self.drag.map(|moved| moved || far);
                }
            }
            SelectOp::Release => match self.drag.take().inspect(|_| self.conv.release()) {
                Some(true) => {
                    if let Some(sel) = self.sel {
                        let text = selection::text(&sel, |n| self.conv.plain_lines(n, width));
                        self.copied = (!text.is_empty()).then_some(text);
                    }
                }
                Some(false) => self.sel = None,
                None => {}
            },
            SelectOp::Clear => {
                self.sel = None;
                self.drag = None;
                self.conv.release();
            }
        }
    }

    /// Whether the last visible row is the way back to the tail: scrolled back — but not a view
    /// a drag holds at the bottom with nothing new below it.
    fn way_back_shown(&self) -> bool {
        !self.conv.following() && (!self.conv.held() || self.conv.unseen() > 0)
    }

    /// The buffer cell under screen cell (`col`, `row`), held to the visible lines: above the first
    /// one it is that line's start, below the last one (the frame — the completion list over a
    /// covered line too — the way back to the tail) that line's end.
    fn point_near(&self, col: u16, row: u16, width: usize) -> Option<Point> {
        let first = self.marks.iter().position(Option::is_some)?;
        let last = self.marks.iter().rposition(Option::is_some)?;
        let y = usize::from(row);
        let (y, col) = if y < first {
            (first, 0)
        } else if y > last {
            (last, width)
        } else {
            (y, usize::from(col))
        };
        let (n, line) = self.marks[..=y].iter().rev().find_map(|m| *m)?;
        Some(Point { row: n, line, col })
    }

    /// Records what each visible row shows for the view as it is now ([`Screen::view_lines`]'s
    /// rows) — the search line or the way back to the tail on the last one.
    fn mark_view(&mut self, width: usize) {
        let (h, vis) = (usize::from(self.view_h), usize::from(self.vis_h));
        let shown = self.conv.view_marks(width, h);
        let mut marks = vec![None; sits_at(shown.len(), vis, h)];
        marks.extend(shown);
        marks.resize(vis, None);
        if (self.search.is_some() || self.way_back_shown())
            && let Some(last) = marks.last_mut()
        {
            *last = None;
        }
        self.marks = marks;
    }

    /// `Ctrl+O` (Claude Code's `[`, without a transcript mode — tui-plan D5): leaves the
    /// alternate screen and writes the conversation into the main screen, so the terminal's own
    /// search, copy mode and selection reach all of it. The screen stays there — nothing is drawn,
    /// output keeps collecting in the buffer — until the next key ([`Screen::back`]). Each row is
    /// written once: a dump writes what no dump before it did, so the main screen's history holds
    /// the conversation once, in order, the first part under a [`TRANSCRIPT_MARK`] line.
    fn dump(&mut self) -> io::Result<()> {
        if self.place != Place::Alt {
            return Ok(());
        }
        self.place = Place::Out;
        self.hold_alt(false);
        self.write_out(true, false)
    }

    /// Enters the alternate screen, cleared — the next draw writes every cell.
    fn enter(&mut self) -> io::Result<()> {
        queue!(self.out(), EnterAlternateScreen, Clear(CtClear::All))?;
        if self.mouse {
            self.out().write_all(MOUSE_ON)?;
        }
        self.place = Place::Alt;
        self.hold_alt(true);
        self.clear()
    }

    /// Tells `restore_terminal` whether the alternate screen — and with it mouse reporting — is
    /// held now (a live screen only).
    fn hold_alt(&self, held: bool) {
        if self.live {
            ALT_ACTIVE.store(held, Ordering::Relaxed);
            MOUSE_ACTIVE.store(held && self.mouse, Ordering::Relaxed);
        }
    }

    /// The bytes that leave the alternate screen: mouse reporting off first (when it was on), then
    /// [`LEAVE`].
    fn leave_seq(&self) -> Vec<u8> {
        let mut seq = Vec::new();
        if self.mouse {
            seq.extend_from_slice(MOUSE_OFF);
        }
        seq.extend_from_slice(LEAVE);
        seq
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
                    s.set_query(&mut self.conv, &q, width);
                    match s.current() {
                        Some(hit) => self.conv.jump_to(hit, s.query(), width, h),
                        None => self.conv.restore(s.origin(), width, h),
                    }
                }
            }
            SearchOp::Step(dir) => {
                if let Some(s) = &mut self.search {
                    s.step(dir);
                    if let Some(hit) = s.current() {
                        self.conv.jump_to(hit, s.query(), width, h);
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

    /// Job control: leaves the alternate screen (the conversation is not written out — `fg` brings
    /// the screen back), turns bracketed paste and focus reporting off and the tty cooked, and stops
    /// the process (`SIGSTOP`, which nothing can catch — the shell sees a stopped job). On `fg` the
    /// modes go back on; the next frame enters the alternate screen again and paints every cell
    /// ([`Place::Main`]). Returns the terminal's size then (`None` for a test's screen, which does
    /// not stop).
    #[cfg(unix)]
    pub(crate) fn suspend(&mut self, focus: bool) -> io::Result<Option<Size>> {
        let mut seq = Vec::new();
        if self.place == Place::Alt {
            self.place = Place::Main;
            self.hold_alt(false);
            seq = self.leave_seq();
        }
        seq.extend_from_slice(b"\x1b[?2004l");
        if focus {
            seq.extend_from_slice(super::osc::CSI_FOCUS_OFF.as_bytes());
        }
        self.write_raw(&seq)?;
        self.last = None;
        if !self.live {
            return Ok(None);
        }
        crossterm::terminal::disable_raw_mode()?;
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGSTOP).map_err(io::Error::from)?;
        crossterm::terminal::enable_raw_mode()?;
        let mut seq = String::from("\x1b[?2004h");
        if focus {
            seq.push_str(super::osc::CSI_FOCUS_ON);
        }
        self.write_raw(seq.as_bytes())?;
        let (width, height) = crossterm::terminal::size()?;
        Ok(Some(Size::new(width, height)))
    }

    /// Back from a dump: the alternate screen again, cleared and repainted whole by the next draw.
    fn back(&mut self) -> io::Result<()> {
        if self.place == Place::Out {
            self.enter()?;
        }
        Ok(())
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
        self.batched(|s| s.term.clear())
    }

    /// Runs `op` inside a batch of its own: whatever it writes leaves as one write (X-55), also
    /// outside the loop's batches (the session picker draws without them).
    fn batched<T>(&mut self, op: impl FnOnce(&mut Self) -> io::Result<T>) -> io::Result<T> {
        self.begin_batch();
        let done = op(self);
        let written = self.end_batch();
        done.and_then(|v| written.map(|()| v))
    }

    /// One frame, one write: the conversation's visible rows over the frame's rows, the real
    /// cursor where the frame puts it (hidden when it puts none). Returns how many of the frame's
    /// top rows it left out ([`FrameMap::cut_for`]): what a click below the viewport counts from.
    pub(crate) fn draw_frame(
        &mut self,
        view: &FrameView,
        prompt: Option<&Prompt>,
    ) -> io::Result<usize> {
        self.batched(|s| s.paint(view, prompt))
    }

    fn paint(&mut self, view: &FrameView, prompt: Option<&Prompt>) -> io::Result<usize> {
        match self.place {
            Place::Out | Place::Left => return Ok(0),
            Place::Main => self.enter()?,
            Place::Alt => {}
        }
        let Size { width, height } = self.size();
        if std::mem::take(&mut self.resized) {
            // `Terminal::resize` clears the screen and the back buffer at any area, the drawn
            // one too, where `autoresize` would diff against a grid the terminal no longer shows.
            self.term.resize(Rect::new(0, 0, width, height))?;
        } else {
            self.term.autoresize()?;
        }
        let frame_h = u16::try_from(view.rows.len())
            .unwrap_or(u16::MAX)
            .min(height);
        let float = u16::try_from(view.float).unwrap_or(u16::MAX).min(frame_h);
        // The conversation keeps the rectangle the frame leaves without its floating rows; the
        // frame is drawn over that rectangle's last `float` rows.
        let view_h = height - (frame_h - float);
        let vis = height - frame_h;
        self.view_h = view_h;
        self.vis_h = vis;
        if let Some(s) = &mut self.search {
            s.catch_up(&mut self.conv, usize::from(width));
            // A resize re-lays-out the block the walk is on: the view goes back to its hit.
            if std::mem::take(&mut self.rejump)
                && let Some(hit) = s.current()
            {
                self.conv
                    .jump_to(hit, s.query(), usize::from(width), usize::from(view_h));
            }
        }
        let (lines, bottom) = self.view_lines(usize::from(width), prompt);
        let fresh = self
            .conv
            .take_fresh(usize::from(width), usize::from(view_h));
        if let Some(n) = self.scrolled_by(fresh, bottom) {
            self.scroll_region(n, vis)?;
        }
        self.mark_view(usize::from(width));
        let lit = self.selected_cells(&lines);
        let conv = Rect::new(0, 0, width, vis);
        let frame = Rect::new(0, vis, width, frame_h);
        // A frame taller than the screen leaves rows out at its top (`FrameMap::cut_for`).
        let cut = view.map.cut_for(view.rows.len(), usize::from(frame_h));
        let frame_lines: Vec<Line<'static>> =
            view.rows[cut..].iter().map(|r| ansi_to_spans(r)).collect();
        // While a query is typed the real cursor is on the search line (an IME's candidate window
        // follows it), not in the composer.
        let typing = self
            .search
            .as_ref()
            .and(prompt)
            .and_then(|p| p.cursor)
            .filter(|_| vis > 0);
        let cursor = match typing {
            Some(col) => Some(Position::new(
                u16::try_from(col + 1)
                    .unwrap_or(u16::MAX)
                    .min(width.saturating_sub(1)),
                vis - 1,
            )),
            None => view.cursor.and_then(|(x, y)| {
                let y = y.checked_sub(u16::try_from(cut).unwrap_or(u16::MAX))?;
                Some(Position::new(
                    x.min(width.saturating_sub(1)),
                    vis.saturating_add(y.min(frame_h.saturating_sub(1))),
                ))
            }),
        };
        let done = self.term.draw(|f| {
            f.render_widget(Paragraph::new(Text::from(lines)), conv);
            f.render_widget(Paragraph::new(Text::from(frame_lines)), frame);
            // The selection is the screen's cells only, never the rows: a write-out stays plain.
            let buf = f.buffer_mut();
            for &(y, from, to) in &lit {
                for x in from..to.min(width) {
                    buf[(x, y)].modifier.insert(Modifier::REVERSED);
                }
            }
            if let Some(pos) = cursor {
                f.set_cursor_position(pos);
            }
        })?;
        self.last = Some(Shown {
            buf: done.buffer.clone(),
            view_h,
            vis,
            bottom,
            following: self.conv.following(),
        });
        Ok(cut)
    }

    /// The cells the selection covers on this draw, as (screen row, from column, to column):
    /// `lines` are the visible rows, and [`Screen::marks`] says where each came from — a covered
    /// row has no mark, so the selection is never painted onto the frame.
    fn selected_cells(&self, lines: &[Line<'_>]) -> Vec<(u16, u16, u16)> {
        let Some(sel) = self.sel else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (y, mark) in self.marks.iter().enumerate() {
            let Some((n, line)) = *mark else { continue };
            let cols = lines.get(y).map_or(0, Line::width);
            if let Some((from, to)) = sel.span(n, line, cols) {
                let cell = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
                out.push((cell(y), cell(from), cell(to)));
            }
        }
        out
    }

    /// How far the visible rows' content moved up since the last frame, when that is one scroll:
    /// the tail followed then and now, the screen, the conversation's rectangle and its visible
    /// rows the same size, its last line on the same screen row (`bottom`: a short conversation
    /// lifted into its blank rows grows under the frame instead), and fewer new lines than the
    /// visible rows hold (more is a whole new viewport anyway).
    fn scrolled_by(&self, fresh: usize, bottom: u16) -> Option<u16> {
        let last = self.last.as_ref()?;
        let n = u16::try_from(fresh).ok()?;
        (last.following
            && self.conv.following()
            && last.buf.area.as_size() == self.size()
            && last.view_h == self.view_h
            && last.vis == self.vis_h
            && last.bottom == bottom
            && n > 0
            && n < self.vis_h)
            .then_some(n)
    }

    /// Scrolls the visible rows up by `n` with ONE operation — `DECSTBM` over them (never over
    /// the frame, which may float over the conversation's last rows),
    /// `SU n`, the margins reset — where a cell diff would rewrite every row that moved (tui-plan
    /// S2; S1 streamed 3.4× inline's bytes for that). ratatui keeps its previous frame to itself,
    /// so it is handed the same scroll first: the last frame shifted up by `n`, applied with the
    /// backend muted, becomes the previous frame the next draw diffs against — that draw then
    /// writes the `n` new lines and whatever else changed.
    fn scroll_region(&mut self, n: u16, vis: u16) -> io::Result<()> {
        let Some(Shown { mut buf, .. }) = self.last.take() else {
            return Ok(());
        };
        let width = usize::from(buf.area.width);
        let region = usize::from(vis) * width;
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
        write!(self.out(), "\x1b[1;{vis}r\x1b[{n}S\x1b[r")
    }

    /// The visible rows as spans, top to bottom, the search's matches highlighted, and the
    /// screen row under the conversation's last line: the conversation is laid out in its
    /// rectangle ([`sits_at`] places it) and only the rows above the frame are kept. The last
    /// visible row is the search line while a search is on and, otherwise, the way back to the
    /// tail when the view is anchored.
    fn view_lines(&mut self, width: usize, prompt: Option<&Prompt>) -> (Vec<Line<'static>>, u16) {
        let (h, vis) = (usize::from(self.view_h), usize::from(self.vis_h));
        let hl = self.search.as_ref().and_then(Search::highlight);
        let shown = self.conv.view_lines(width, h, hl.as_ref());
        let top = sits_at(shown.len(), vis, h);
        let bottom = u16::try_from(top + shown.len()).unwrap_or(u16::MAX);
        let mut lines = vec![String::new(); top];
        lines.extend(shown);
        lines.resize(vis, String::new());
        if let (Some(s), Some(p)) = (&self.search, prompt)
            && vis > 0
        {
            let hint = if p.cursor.is_some() {
                "Enter walks the hits · Esc goes back"
            } else {
                "n/p walk · Enter stays · Esc goes back"
            };
            let status = s.status();
            let sep = if status.is_empty() { "" } else { " · " };
            let line = format!("/{}  {FAINT}{status}{sep}{hint}{RESET}", p.text);
            if let Some(last) = lines.last_mut() {
                *last = line;
            }
        } else if self.way_back_shown()
            && let Some(last) = lines.last_mut()
        {
            *last = match self.conv.unseen() {
                0 => format!("{REV_ON} ↓ Ctrl+End follows the output again {RESET}"),
                1 => format!("{REV_ON} ↓ 1 new line · Ctrl+End {RESET}"),
                n => format!("{REV_ON} ↓ {n} new lines · Ctrl+End {RESET}"),
            };
        }
        (lines.iter().map(|r| ansi_to_spans(r)).collect(), bottom)
    }

    /// The exit: leaves the alternate screen (unless a dump already did) and writes what no dump
    /// has into the main screen — the conversation survives in the shell's history, whole.
    pub(crate) fn leave(&mut self) -> io::Result<()> {
        let place = std::mem::replace(&mut self.place, Place::Left);
        match place {
            Place::Left => return Ok(()),
            // Never entered (no frame was drawn) or already out: nothing to leave.
            Place::Main | Place::Out => self.write_out(false, true)?,
            Place::Alt => self.write_out(true, true)?,
        }
        self.hold_alt(false);
        Ok(())
    }

    /// Writes the rows not written yet into the main screen at the width the terminal has now —
    /// leaving the alternate screen first when `leave`, under a [`TRANSCRIPT_MARK`] line the first
    /// time there is a row to write (an empty conversation, the session picker's, writes nothing)
    /// — each row closed with an SGR reset and `CRLF` (raw mode is on), then clears what was
    /// below: the next line is a fresh one. A dump stops above a block still being written; the
    /// exit (`whole`) writes everything.
    fn write_out(&mut self, leave: bool, whole: bool) -> io::Result<()> {
        let width = usize::from(self.size().width.max(1));
        let mut dump = Vec::new();
        if leave {
            dump = self.leave_seq();
        }
        let first = self.written == 0;
        let (lines, written) = self.conv.lines_after(self.written, width, whole);
        if first && !lines.is_empty() {
            dump.extend_from_slice(format!("{FAINT}{TRANSCRIPT_MARK}{RESET}\r\n").as_bytes());
        }
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

/// The screen row a conversation of `n` lines starts on, in a rectangle of `h` rows whose first
/// `vis` are visible: on the frame while it fits above it, else as high as it needs to be — the
/// blank rows above a short one take the frame's floating rows before any line is covered — and
/// never higher than the rectangle's top.
fn sits_at(n: usize, vis: usize, h: usize) -> usize {
    n.clamp(vis, h.max(vis)) - n
}

/// The longest write that goes out without a synchronized update — an empirical threshold, not
/// a transport guarantee: the macOS pty's read block (1024 B, measured; Linux reads 4096, a TCP
/// segment carries ~1448), so a write this size or smaller usually lands in one read. Nothing
/// here controls how many bytes the receiving side reads at once (`write_all` promises no single
/// write; TCP keeps no application write boundary, RFC 9293 §3.7): small writes keep the reads
/// few, large ones ask a DEC 2026 terminal to present them at once. Chosen by size, never by
/// terminal, and not a knob (X-55).
pub(crate) const SYNC_MIN: usize = 1024;

/// DEC private mode 2026, synchronized output: begin and end of one update.
pub(crate) const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// See [`SYNC_BEGIN`].
pub(crate) const SYNC_END: &[u8] = b"\x1b[?2026l";

/// The byte sink under the backend: everything a frame writes collects here, and leaves in one
/// write at the end of the outermost batch (or at a flush outside any).
struct SyncOut<W: Write> {
    out: W,
    pending: Vec<u8>,
    batch: Arc<AtomicUsize>,
}

impl<W: Write> SyncOut<W> {
    /// Hands the batch's bytes to the terminal in one write (X-55). A write longer than
    /// [`SYNC_MIN`] goes out as one DEC 2026 synchronized update (`ESC[?2026h … ESC[?2026l`): the
    /// transport may split it, and an emulator that knows the mode shows the screen before the
    /// block or after it, never a half-drawn frame in between. A shorter write goes out bare — it
    /// usually lands in one read, and a terminal that repaints a whole pane per block (tmux 3.7)
    /// pays nothing for typing or streaming. Sent blind, like crossterm's
    /// `BeginSynchronizedUpdate`; the criterion is the size, never the terminal. Inside a batch
    /// the bytes wait for its end.
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

use crate::ui::block::Block;
use conversation::Conversation;
use search::{Search, SearchOp};

#[cfg(test)]
mod tests;
