//! The conversation buffer of the full screen: every committed row, the wrap of each row at the
//! width the screen has, and where the viewport is.
//!
//! - **Rows** are the rendered ANSI strings the app committed (T-05), each with its display width
//!   measured once. A row as wide as the screen or narrower is one display line; a wider one is
//!   hard-wrapped by `wrap_ansi` and the result is cached with the width it was wrapped at, so a
//!   draw wraps a row once per width, not once per frame.
//! - **The view** either follows the tail or is anchored on a ROW: the display line at the
//!   viewport's top is `(row, line within it)`. New output appends below and moves nothing; a
//!   resize keeps the anchored row at the top (its line clamped to its new height). Scrolling up
//!   leaves the tail; getting back to the bottom (scrolling down to it, or `Ctrl+End`) follows it
//!   again. Rows committed while the view is anchored are counted for the `↓ N new lines` row.
//! - **The cap**: past [`ROW_CAP`] rows the oldest are dropped and a first row says so. The
//!   session log (`messages.jsonl`) holds everything; `/export` writes it out.
//!
//! Every walk is bounded by what it covers — a viewport's lines, a scroll step's lines — never by
//! the depth of the session.

use std::collections::VecDeque;

use crate::text::ansi::{ansi_width, wrap_ansi};
use crate::ui::render::theme::{FAINT, RESET};

/// The most rows the buffer keeps (tui-plan §1: 200k rows, ~20–40 MB).
pub(crate) const ROW_CAP: usize = 200_000;

/// The row standing in for the rows the cap dropped.
pub(crate) const TRIMMED: &str = "earlier output trimmed — /export has the full text";

/// One committed row and its wrap at the last width it was drawn at, when it is wider than that.
struct Row {
    text: String,
    cols: usize,
    wrap: Option<(usize, Vec<String>)>,
}

impl Row {
    fn new(text: String) -> Self {
        Self {
            cols: ansi_width(&text),
            text,
            wrap: None,
        }
    }

    /// The row's display lines at `width`.
    fn lines(&mut self, width: usize) -> &[String] {
        if self.cols <= width {
            return std::slice::from_ref(&self.text);
        }
        if self.wrap.as_ref().is_none_or(|(w, _)| *w != width) {
            self.wrap = Some((width, wrap_ansi(&self.text, width)));
        }
        self.wrap
            .as_ref()
            .map_or(&[], |(_, lines)| lines.as_slice())
    }

    fn height(&mut self, width: usize) -> usize {
        self.lines(width).len().max(1)
    }
}

/// A display line: a row and a line within its wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pos {
    pub(crate) row: usize,
    pub(crate) line: usize,
}

/// Where the viewport is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The viewport ends on the last line.
    Follow,
    /// The viewport's top line.
    At(Pos),
}

/// The buffer, its wrap cache and the view (module docs).
pub(crate) struct Conversation {
    rows: VecDeque<Row>,
    cap: usize,
    /// Whether rows were dropped; `rows[0]` is then the [`TRIMMED`] row.
    trimmed: bool,
    view: View,
    /// Rows committed while the view is anchored.
    unseen: usize,
    /// Rows committed since the last [`Conversation::take_fresh`].
    fresh: usize,
    /// Rows ever committed (a row's number, for [`Conversation::lines_after`]).
    pushed: u64,
}

impl Conversation {
    pub(crate) fn new() -> Self {
        Self::with_cap(ROW_CAP)
    }

    pub(crate) fn with_cap(cap: usize) -> Self {
        Self {
            rows: VecDeque::new(),
            cap: cap.max(2),
            trimmed: false,
            view: View::Follow,
            unseen: 0,
            fresh: 0,
            pushed: 0,
        }
    }

    /// Appends committed rows; past the cap, drops the oldest (the anchor moves with its row, or
    /// to the top when its row was dropped).
    pub(crate) fn push(&mut self, rows: &[String]) {
        self.rows.extend(rows.iter().map(|t| Row::new(t.clone())));
        self.fresh += rows.len();
        self.pushed += rows.len() as u64;
        if matches!(self.view, View::At(_)) {
            self.unseen += rows.len();
        }
        if self.rows.len() <= self.cap {
            return;
        }
        if !self.trimmed {
            self.rows
                .push_front(Row::new(format!("{FAINT}{TRIMMED}{RESET}")));
            self.trimmed = true;
            if let View::At(p) = &mut self.view {
                p.row += 1;
            }
        }
        let excess = self.rows.len() - self.cap;
        self.rows.drain(1..=excess);
        if let View::At(p) = &mut self.view {
            *p = if p.row > excess {
                Pos {
                    row: p.row - excess,
                    line: p.line,
                }
            } else {
                Pos { row: 0, line: 0 }
            };
        }
    }

    /// The display lines (at `width`) of the rows committed after the first `from`, oldest
    /// first, and the count to ask from next time (a dump writes each row once). Rows the cap
    /// dropped before they were asked for are stood in for by the [`TRIMMED`] row.
    pub(crate) fn lines_after(&mut self, from: u64, width: usize) -> (Vec<String>, u64) {
        let kept = u64::from(self.trimmed);
        let real = self.rows.len() as u64 - kept;
        let first = self.pushed - real;
        let start = if from < first {
            0
        } else {
            usize::try_from(kept + (from - first)).unwrap_or(usize::MAX)
        };
        let mut out = Vec::new();
        for row in self.rows.iter_mut().skip(start) {
            out.extend(row.lines(width).iter().cloned());
        }
        (out, self.pushed)
    }

    /// Whether the view follows the tail.
    pub(crate) fn following(&self) -> bool {
        self.view == View::Follow
    }

    /// Rows committed since the view left the tail.
    pub(crate) const fn unseen(&self) -> usize {
        self.unseen
    }

    /// The display lines (at `width`) of the rows committed since the last call.
    pub(crate) fn take_fresh(&mut self, width: usize) -> usize {
        let n = std::mem::take(&mut self.fresh).min(self.rows.len());
        let start = self.rows.len() - n;
        (start..self.rows.len())
            .map(|i| self.rows[i].height(width))
            .sum()
    }

    /// Scrolls `n` display lines toward the start (`h` is the viewport's height).
    pub(crate) fn scroll_up(&mut self, n: usize, width: usize, h: usize) {
        let top = self.top_line(width, h);
        let to = self.up(top, n, width);
        self.anchor(to);
        self.follow_if_at_bottom(width, h);
    }

    /// Scrolls `n` display lines toward the tail; reaching it follows it again.
    pub(crate) fn scroll_down(&mut self, n: usize, width: usize, h: usize) {
        let View::At(top) = self.view else {
            return;
        };
        let to = self.down(top, n, width);
        self.view = View::At(to);
        self.follow_if_at_bottom(width, h);
    }

    /// The first line of the session (`Ctrl+Home`).
    pub(crate) fn to_top(&mut self, width: usize, h: usize) {
        if !self.rows.is_empty() {
            self.anchor(Pos { row: 0, line: 0 });
            self.follow_if_at_bottom(width, h);
        }
    }

    /// The tail, followed again (`Ctrl+End`).
    pub(crate) fn to_bottom(&mut self) {
        self.view = View::Follow;
        self.unseen = 0;
    }

    /// The viewport's lines, top to bottom: `h` of them, or fewer when the whole session is
    /// shorter than the viewport (it then sits on the frame).
    pub(crate) fn view_lines(&mut self, width: usize, h: usize) -> Vec<String> {
        self.follow_if_at_bottom(width, h);
        let mut top = self.top_line(width, h);
        if let View::At(p) = &mut self.view {
            // A resize can leave the anchored line past its row's new height.
            p.line = p.line.min(self.rows[p.row].height(width) - 1);
            top = *p;
        }
        let mut out = Vec::with_capacity(h);
        let mut pos = top;
        while out.len() < h && pos.row < self.rows.len() {
            let lines = self.rows[pos.row].lines(width);
            out.extend(
                lines[pos.line.min(lines.len())..]
                    .iter()
                    .take(h - out.len())
                    .cloned(),
            );
            pos = Pos {
                row: pos.row + 1,
                line: 0,
            };
        }
        out
    }

    /// Anchors the view at `top`, leaving the tail.
    fn anchor(&mut self, top: Pos) {
        if self.following() {
            self.unseen = 0;
        }
        self.view = View::At(top);
    }

    /// An anchored view that shows the last line follows the tail again.
    fn follow_if_at_bottom(&mut self, width: usize, h: usize) {
        if let View::At(top) = self.view
            && self.lines_from(top, width, h) <= h
        {
            self.to_bottom();
        }
    }

    /// The line at the viewport's top: the anchor, or `h` lines up from the end.
    fn top_line(&mut self, width: usize, h: usize) -> Pos {
        match self.view {
            View::At(p) => p,
            View::Follow => match self.rows.len().checked_sub(1) {
                None => Pos { row: 0, line: 0 },
                Some(last) => {
                    let end = Pos {
                        row: last,
                        line: self.rows[last].height(width) - 1,
                    };
                    self.up(end, h.saturating_sub(1), width)
                }
            },
        }
    }

    /// The lines from `p` to the end, counted up to `limit + 1`.
    fn lines_from(&mut self, p: Pos, width: usize, limit: usize) -> usize {
        let mut n = 0;
        let mut row = p.row;
        let mut skip = p.line;
        while row < self.rows.len() && n <= limit {
            n += self.rows[row].height(width).saturating_sub(skip);
            skip = 0;
            row += 1;
        }
        n
    }

    /// `n` lines up from `p`, stopping at the first line.
    fn up(&mut self, mut p: Pos, mut n: usize, width: usize) -> Pos {
        while n > 0 {
            if p.line >= n {
                p.line -= n;
                break;
            }
            if p.row == 0 {
                p.line = 0;
                break;
            }
            n -= p.line + 1;
            p.row -= 1;
            p.line = self.rows[p.row].height(width) - 1;
        }
        p
    }

    /// `n` lines down from `p`, stopping at the last line.
    fn down(&mut self, mut p: Pos, mut n: usize, width: usize) -> Pos {
        while n > 0 && p.row < self.rows.len() {
            let last = self.rows[p.row].height(width) - 1;
            let left = last - p.line.min(last);
            if left >= n {
                p.line += n;
                break;
            }
            if p.row + 1 == self.rows.len() {
                p.line += left;
                break;
            }
            n -= left + 1;
            p.row += 1;
            p.line = 0;
        }
        p
    }
}

#[cfg(test)]
mod tests;
