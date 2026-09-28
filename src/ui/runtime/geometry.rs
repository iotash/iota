//! The synthetic terminal headless tests run the REAL writer stack on (`Term::headless`), with
//! the races a live one has injected (a DSR failing or late, a size lagging the screen, a resize
//! right after an answer). Test configuration only: the live build has none of it.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ratatui::layout::{Position, Size};

use crate::sync::lock;

/// Synthetic terminal geometry for headless tests: the backend answers size and
/// cursor-position queries from here instead of the real tty. Cloned handles share
/// state, so a test mutates what the "terminal" reports while `Term` keeps its bookkeeping
/// mirrored (`InlineTerminal::mirror`).
#[derive(Clone)]
pub(crate) struct Geometry {
    size: Arc<Mutex<Size>>,
    cursor: Arc<Mutex<Position>>,
    /// Test seam: the cursor query fails (a terminal that never answers the DSR — crossterm
    /// gives up after ~2 s with an error).
    dsr_fails: Arc<AtomicBool>,
    /// Test seam: every answer is ONE query late — what crossterm does after a query has timed
    /// out: the late reply waits in its event queue and the next query takes it at once
    /// (`cursor/sys/unix.rs` `read_position_raw` polls the queue first). The first query of
    /// the lag fails (it timed out); each later one answers where the cursor was at the query
    /// before it. `None` = answers are current.
    lagged: Arc<Mutex<Option<Lag>>>,
    /// Test seam: the rows the screen REALLY has when the reported size lags it (an emulator applies
    /// the next resize before the tty size or the event says so); the cursor moves within it.
    real_rows: Arc<Mutex<Option<u16>>>,
    /// Test seam: what the emulator does right AFTER it answers a cursor query — a
    /// resize landing between the answer and the next byte the pass writes. Fires once, on
    /// the query whose index (from 0) it is armed with.
    after_query: Arc<Mutex<AfterQuery>>,
}

/// The lagged-answer seam's state: the query that timed out, then the answer held for the next.
#[derive(Clone, Copy)]
enum Lag {
    TimedOut,
    Late(Position),
}

type AfterQuery = (usize, Option<(usize, Box<dyn FnOnce() + Send>)>);

impl Geometry {
    /// A synthetic terminal of `width`×`height` cells, cursor at the origin.
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self {
            size: Arc::new(Mutex::new(Size { width, height })),
            cursor: Arc::new(Mutex::new(Position::ORIGIN)),
            dsr_fails: Arc::new(AtomicBool::new(false)),
            lagged: Arc::new(Mutex::new(None)),
            real_rows: Arc::new(Mutex::new(None)),
            after_query: Arc::new(Mutex::new((0, None))),
        }
    }

    /// Runs `f` right after the `nth` cursor query from now (0 = the next one) is answered.
    pub(crate) fn after_query(&self, nth: usize, f: impl FnOnce() + Send + 'static) {
        let mut g = lock(&self.after_query);
        let seen = g.0;
        g.1 = Some((seen + nth, Box::new(f)));
    }

    /// The screen really has `rows` rows while the reported size stays what it was.
    pub(crate) fn set_real_rows(&self, rows: u16) {
        *lock(&self.real_rows) = Some(rows);
    }

    /// The last row the cursor can reach: the real screen's, or the reported one's.
    fn last_row(&self) -> u16 {
        (*lock(&self.real_rows))
            .unwrap_or_else(|| self.size().height)
            .saturating_sub(1)
    }

    /// Makes every cursor answer one query late from now on (see `lagged`).
    pub(crate) fn set_answers_lagged(&self) {
        *lock(&self.lagged) = Some(Lag::TimedOut);
    }

    /// Makes every cursor query fail from now on (or answer again).
    pub(crate) fn set_dsr_fails(&self, fails: bool) {
        self.dsr_fails.store(fails, Ordering::Relaxed);
    }

    /// The synthetic DSR: the cursor, or the error a terminal that never answers yields.
    pub(super) fn query_cursor(&self) -> io::Result<Position> {
        if self.dsr_fails.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the terminal did not answer the cursor position query",
            ));
        }
        let now = self.cursor();
        let answer = {
            let mut lag = lock(&self.lagged);
            match lag.replace(Lag::Late(now)) {
                None => {
                    *lag = None;
                    now
                }
                Some(Lag::Late(late)) => late,
                Some(Lag::TimedOut) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the terminal did not answer in time (its reply will come late)",
                    ));
                }
            }
        };
        let hook = {
            let mut g = lock(&self.after_query);
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
        Ok(answer)
    }

    /// Changes the reported terminal size (pair with a scripted `Event::Resize`).
    pub(crate) fn set_size(&self, width: u16, height: u16) {
        *lock(&self.size) = Size { width, height };
    }

    /// Moves the synthetic cursor by `rows` (negative = up) — what an emulator does to
    /// the cursor when a resize reflows or drops the rows above it (pair with
    /// [`Geometry::set_size`] ahead of the scripted `Event::Resize`).
    pub(crate) fn shift_cursor(&self, rows: i32) {
        let pos = self.cursor();
        let y = (i32::from(pos.y) + rows).clamp(0, i32::from(u16::MAX));
        self.put_cursor(Position::new(pos.x, u16::try_from(y).unwrap_or(0)));
    }

    /// The reported size.
    pub(crate) fn size(&self) -> Size {
        *lock(&self.size)
    }

    /// The reported cursor position (the synthetic DSR answer).
    pub(crate) fn cursor(&self) -> Position {
        *lock(&self.cursor)
    }

    /// The cursor is put on `pos` (an absolute move).
    pub(super) fn put_cursor(&self, pos: Position) {
        *lock(&self.cursor) = pos;
    }

    /// The cursor moves to column `x` of its row.
    pub(super) fn set_col(&self, x: u16) {
        let pos = self.cursor();
        self.put_cursor(Position::new(x, pos.y));
    }

    /// `CUU n`: up, stopping on the top row.
    pub(super) fn up(&self, n: u16) {
        let pos = self.cursor();
        self.put_cursor(Position::new(pos.x, pos.y.saturating_sub(n)));
    }

    /// `n` × `LF`: down, the screen scrolling under a cursor that stays on the last row.
    pub(super) fn lf(&self, n: u16) {
        let pos = self.cursor();
        let last = self.last_row();
        self.put_cursor(Position::new(pos.x, pos.y.saturating_add(n).min(last)));
    }
}
