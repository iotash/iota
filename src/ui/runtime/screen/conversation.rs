//! The conversation buffer of the full screen: every committed block, its lines at the width the
//! screen has, and where the viewport is.
//!
//! - **Blocks** are what the app committed (`ui::block`): fixed rows, and blocks laid out from
//!   their source. A block's display lines at a width are computed when a draw first needs them at
//!   that width and cached with it — a row as wide as the screen or narrower is its own line and
//!   costs nothing; a wider one is wrapped by `wrap_ansi`; a laid-out block is rendered from its
//!   source. A width change therefore re-lays-out exactly the blocks a draw touches, and the rest
//!   when (if ever) the view reaches them.
//! - **The view** either follows the tail or is anchored on a BLOCK: the display line at the
//!   viewport's top is `(block, line within it)`. New output appends below and moves nothing; a
//!   resize keeps the anchored block at the top (its line clamped to its new height). Scrolling up
//!   leaves the tail; getting back to the bottom (scrolling down to it, or `Ctrl+End`) follows it
//!   again. Blocks committed while the view is anchored are counted for the `↓ N new lines` row.
//! - **The cap**: past [`BLOCK_CAP`] blocks the oldest are dropped — each one whole — and a first
//!   row says so. The session log (`messages.jsonl`) holds everything; `/export` writes it out.
//!
//! Every walk is bounded by what it covers — a viewport's lines, a scroll step's lines — never by
//! the depth of the session.

use std::collections::VecDeque;

use crate::text::ansi::{ansi_width, strip_sgr, wrap_ansi};
use crate::ui::block::Block;
use crate::ui::render::theme::{FAINT, RESET};
use crate::ui::surface::search::{highlight_line, match_ranges};

/// The most blocks the buffer keeps (tui-plan §1: 200k rows, ~20–40 MB; a block is a row or a
/// laid-out source, so this is at least as many rows).
pub(crate) const BLOCK_CAP: usize = 200_000;

/// The row standing in for the blocks the cap dropped.
pub(crate) const TRIMMED: &str = "earlier output trimmed — /export has the full text";

/// A block's display lines at one width, and which of them continue the line before
/// (`Block::display`).
struct Laid {
    width: usize,
    lines: Vec<String>,
    joins: Vec<bool>,
}

/// One committed block and its display lines at the last width they were needed at.
struct Entry {
    block: Block,
    /// A row's display width (a laid-out block: 0, unused).
    cols: usize,
    lines: Option<Laid>,
    /// A laid-out block's search text: its plain lines at the FIRST width it was laid out at,
    /// joined by `\n`, kept for good — what a search scans never depends on the width, so a
    /// resize lays nothing out for a search (a row's search text is the row itself).
    text: Option<String>,
}

impl Entry {
    fn new(block: Block) -> Self {
        let cols = match &block {
            Block::Row(t) => ansi_width(t),
            Block::Laid(_) => 0,
        };
        Self {
            block,
            cols,
            lines: None,
            text: None,
        }
    }

    /// The block's display lines at `width`.
    fn lines(&mut self, width: usize) -> &[String] {
        self.fill(width);
        match (&self.block, &self.lines) {
            (Block::Row(t), _) if self.cols <= width => std::slice::from_ref(t),
            (_, Some(l)) => &l.lines,
            _ => &[],
        }
    }

    /// Fills the display lines' cache at `width` when it holds another width's (a row that fits
    /// needs none: it is its own one line).
    fn fill(&mut self, width: usize) {
        if matches!(self.block, Block::Row(_)) && self.cols <= width {
            return;
        }
        if self.lines.as_ref().is_none_or(|l| l.width != width) {
            let (lines, joins): (Vec<String>, _) = self.block.display(width).into_iter().unzip();
            if self.text.is_none() && matches!(self.block, Block::Laid(_)) {
                self.text = Some(strip_sgr(&lines.join("\n")));
            }
            self.lines = Some(Laid {
                width,
                lines,
                joins,
            });
        }
    }

    /// The display lines at `width` with whether each continues the line before (a segment the
    /// screen's wrap made).
    fn lines_joined(&mut self, width: usize) -> Vec<(String, bool)> {
        self.fill(width);
        match (&self.block, &self.lines) {
            (Block::Row(t), _) if self.cols <= width => vec![(t.clone(), false)],
            (_, Some(l)) => l
                .lines
                .iter()
                .cloned()
                .zip(l.joins.iter().copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// At least one line: an empty laid-out block still holds its place.
    fn height(&mut self, width: usize) -> usize {
        self.lines(width).len().max(1)
    }

    /// What a search scans: a row's text as written (its wrap does not split a match), a
    /// laid-out block's search text ([`Entry::text`]) — laid out at `width` only if it never was.
    fn search_text(&mut self, width: usize) -> &str {
        if let Block::Laid(_) = self.block
            && self.text.is_none()
        {
            self.fill(width); // a block never laid out has no cache either: this lays it out
        }
        match (&self.block, &self.text) {
            (Block::Row(t), _) => t,
            (Block::Laid(_), Some(t)) => t,
            (Block::Laid(_), None) => "",
        }
    }

    /// The display line at `width` holding occurrence `k` of `query` (0 when there is none): in a
    /// row, by the byte the match starts at (a match its wrap splits is on the line its first byte
    /// is on); in a laid-out block, by counting the occurrences line by line.
    fn line_of(&mut self, k: usize, query: &str, width: usize) -> usize {
        if let Block::Row(t) = &self.block {
            let off = match_ranges(&strip_sgr(t), query).get(k).map_or(0, |r| r.0);
            let mut seen = 0;
            return self
                .lines(width)
                .iter()
                .position(|l| {
                    seen += strip_sgr(l).len();
                    off < seen
                })
                .unwrap_or(0);
        }
        let mut seen = 0;
        self.lines(width)
            .iter()
            .position(|l| {
                seen += match_ranges(&strip_sgr(l), query).len();
                k < seen
            })
            .unwrap_or(0)
    }
}

/// What a draw highlights while a search is on: every occurrence of `query`, and brighter the
/// one n/p is parked on (block number, which occurrence in it).
pub(crate) struct Highlight<'a> {
    pub(crate) query: &'a str,
    pub(crate) current: Option<(u64, usize)>,
}

/// A display line: a block and a line within it.
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
    rows: VecDeque<Entry>,
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
    /// Whether the view is held where it is (a drag is selecting): it does not follow the tail
    /// even when it shows the last line, so new output never moves the text under the pointer.
    held: bool,
}

impl Conversation {
    pub(crate) fn new() -> Self {
        Self::with_cap(BLOCK_CAP)
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
            held: false,
        }
    }

    /// Appends committed blocks; past the cap, drops the oldest, each whole (the anchor moves
    /// with its block, or to the top when its block was dropped).
    pub(crate) fn push(&mut self, blocks: Vec<Block>) {
        let n = blocks.len();
        self.rows.extend(blocks.into_iter().map(Entry::new));
        self.fresh += n;
        self.pushed += n as u64;
        if matches!(self.view, View::At(_)) {
            self.unseen += n;
        }
        if self.rows.len() <= self.cap {
            return;
        }
        if !self.trimmed {
            self.rows
                .push_front(Entry::new(Block::Row(format!("{FAINT}{TRIMMED}{RESET}"))));
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

    /// The number of the oldest row still held (rows are numbered as they are committed; the
    /// [`TRIMMED`] row has none).
    fn first_number(&self) -> u64 {
        self.pushed - (self.rows.len() as u64 - u64::from(self.trimmed))
    }

    /// Where row number `n` sits, if it is still held.
    fn index_of(&self, n: u64) -> Option<usize> {
        let first = self.first_number();
        (n >= first && n < self.pushed)
            .then(|| usize::try_from(n - first).ok())
            .flatten()
            .map(|i| i + usize::from(self.trimmed))
    }

    /// The number of the row at `index` (`None` for the [`TRIMMED`] row).
    fn number_of(&self, index: usize) -> Option<u64> {
        let kept = usize::from(self.trimmed);
        (index >= kept).then(|| self.first_number() + (index - kept) as u64)
    }

    /// The number of the oldest block still held (blocks are numbered as they are committed; the
    /// [`TRIMMED`] row has none).
    pub(crate) fn first_held(&self) -> u64 {
        self.first_number()
    }

    /// The blocks numbered `from` on, as (number, search text) — what a search scans. A laid-out
    /// block's text is the one it got when it was first laid out, so this lays out only the blocks
    /// no draw ever reached (at `width`), once.
    pub(crate) fn texts_from(&mut self, from: u64, width: usize) -> Vec<(u64, &str)> {
        let first = self.first_number().max(from);
        let skip = self.index_of(first).unwrap_or(self.rows.len());
        for e in self.rows.iter_mut().skip(skip) {
            e.search_text(width);
        }
        self.rows
            .iter()
            .skip(skip)
            .zip(first..)
            .map(|(e, n)| {
                let t: &str = match (&e.block, &e.text) {
                    (Block::Row(t), _) | (Block::Laid(_), Some(t)) => t,
                    (Block::Laid(_), None) => "",
                };
                (n, t)
            })
            .collect()
    }

    /// The rows ever committed: the next row's number.
    pub(crate) const fn pushed(&self) -> u64 {
        self.pushed
    }

    /// Where the view is, by row number — what [`Conversation::restore`] takes back (`None`:
    /// following the tail).
    pub(crate) fn mark(&self) -> Option<(u64, usize)> {
        match self.view {
            View::Follow => None,
            View::At(p) => self.number_of(p.row).map(|n| (n, p.line)),
        }
    }

    /// Puts the view back where [`Conversation::mark`] found it (its row dropped meanwhile: the
    /// first row).
    pub(crate) fn restore(&mut self, mark: Option<(u64, usize)>, width: usize, h: usize) {
        match mark {
            None => self.go_bottom(),
            Some((n, line)) => {
                let row = self.index_of(n).unwrap_or(0);
                self.view = View::At(Pos { row, line });
                self.follow_if_at_bottom(width, h);
            }
        }
    }

    /// Anchors the view so the display line holding occurrence `k` of `query` in block `n` is in
    /// the middle of the viewport. Only that block is laid out (at `width`).
    pub(crate) fn jump_to(&mut self, (n, k): (u64, usize), query: &str, width: usize, h: usize) {
        let Some(row) = self.index_of(n) else {
            return;
        };
        let line = self.rows[row].line_of(k, query, width);
        let top = self.up(Pos { row, line }, h / 2, width);
        self.anchor(top);
        self.follow_if_at_bottom(width, h);
    }

    /// The number of the last row the viewport shows (0 when that is the [`TRIMMED`] row).
    pub(crate) fn last_shown(&mut self, width: usize, h: usize) -> u64 {
        if self.rows.is_empty() {
            return 0;
        }
        let top = self.top_line(width, h);
        let bottom = self.down(top, h.saturating_sub(1), width);
        self.number_of(bottom.row.min(self.rows.len() - 1))
            .unwrap_or(0)
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
        // Never past the top of the last screenful: a held view does not fall back to following
        // there, and an anchor beyond it would show the tail's last lines alone.
        let to = self.down(top, n, width);
        let floor = self.bottom_top(width, h);
        let to = if (to.row, to.line) > (floor.row, floor.line) {
            floor
        } else {
            to
        };
        self.view = View::At(to);
        self.follow_if_at_bottom(width, h);
    }

    /// The first line of the session (`Ctrl+Home`).
    pub(crate) fn go_top(&mut self, width: usize, h: usize) {
        if !self.rows.is_empty() {
            self.anchor(Pos { row: 0, line: 0 });
            self.follow_if_at_bottom(width, h);
        }
    }

    /// Holds the view where it is — a following view anchors on its top line — until
    /// [`Conversation::release`]: what a drag selects stays under the pointer while output arrives.
    pub(crate) fn hold(&mut self, width: usize, h: usize) {
        if self.following() {
            let top = self.top_line(width, h);
            self.anchor(top);
        }
        self.held = true;
    }

    /// Lets a held view go: one that shows the last line follows the tail again at the next draw.
    pub(crate) fn release(&mut self) {
        self.held = false;
    }

    /// Whether the view is held ([`Conversation::hold`]).
    pub(crate) const fn held(&self) -> bool {
        self.held
    }

    /// The tail, followed again (`Ctrl+End`).
    pub(crate) fn go_bottom(&mut self) {
        self.view = View::Follow;
        self.unseen = 0;
    }

    /// The viewport's lines, top to bottom: `h` of them, or fewer when the whole session is
    /// shorter than the viewport (it then sits on the frame). With a search on, every occurrence
    /// on a shown row is highlighted — on the row BEFORE it is wrapped, so a match split by a
    /// wrap is highlighted on both of its lines (`wrap_ansi` carries the SGR across).
    pub(crate) fn view_lines(
        &mut self,
        width: usize,
        h: usize,
        hl: Option<&Highlight<'_>>,
    ) -> Vec<String> {
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
            let lit = hl.and_then(|hl| self.highlighted(pos.row, width, hl));
            let lines = match &lit {
                Some(lines) => lines.as_slice(),
                None => self.rows[pos.row].lines(width),
            };
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

    /// Where each of the lines [`Conversation::view_lines`] returns comes from, in the same order:
    /// (row number, line within its wrap), `None` for the [`TRIMMED`] row. Called after it, so the
    /// anchor is already clamped.
    pub(crate) fn view_marks(&mut self, width: usize, h: usize) -> Vec<Option<(u64, usize)>> {
        let mut pos = self.top_line(width, h);
        let mut out = Vec::with_capacity(h);
        while out.len() < h && pos.row < self.rows.len() {
            let n = self.number_of(pos.row);
            let height = self.rows[pos.row].height(width);
            for line in pos.line.min(height)..height {
                if out.len() == h {
                    break;
                }
                out.push(n.map(|n| (n, line)));
            }
            pos = Pos {
                row: pos.row + 1,
                line: 0,
            };
        }
        out
    }

    /// Block number `n`'s display lines at `width`, SGR stripped (what a selection copies), each
    /// with whether it continues the line before — a row's wrap segments do, a laid-out block's
    /// own lines do not; `None` once the cap has dropped it.
    pub(crate) fn plain_lines(&mut self, n: u64, width: usize) -> Option<Vec<(String, bool)>> {
        let i = self.index_of(n)?;
        Some(
            self.rows[i]
                .lines_joined(width)
                .into_iter()
                .map(|(l, j)| (strip_sgr(&l), j))
                .collect(),
        )
    }

    /// Block `index`'s lines at `width` with the search's matches highlighted, or `None` when it
    /// has none (the cached lines serve then). A row is highlighted BEFORE it is wrapped, so a
    /// match its wrap splits is lit on both lines; a laid-out block line by line, its occurrences
    /// counted in order to find the current one.
    fn highlighted(
        &mut self,
        index: usize,
        width: usize,
        hl: &Highlight<'_>,
    ) -> Option<Vec<String>> {
        let n = self.number_of(index);
        let cur = hl.current.and_then(|(cn, k)| (Some(cn) == n).then_some(k));
        let entry = &mut self.rows[index];
        if let Block::Row(text) = &entry.block {
            let ranges = match_ranges(&strip_sgr(text), hl.query);
            if ranges.is_empty() {
                return None;
            }
            let off = cur.and_then(|k| ranges.get(k)).map(|r| r.0);
            let lit = highlight_line(text, hl.query, off);
            return Some(if entry.cols > width {
                wrap_ansi(&lit, width)
            } else {
                vec![lit]
            });
        }
        let mut seen = 0;
        let mut any = false;
        let mut out = Vec::new();
        for line in entry.lines(width) {
            let ranges = match_ranges(&strip_sgr(line), hl.query);
            if ranges.is_empty() {
                out.push(line.clone());
                continue;
            }
            any = true;
            let here = cur
                .and_then(|k| k.checked_sub(seen))
                .and_then(|i| ranges.get(i))
                .map(|r| r.0);
            out.push(highlight_line(line, hl.query, here));
            seen += ranges.len();
        }
        any.then_some(out)
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
        if self.held {
            return;
        }
        if let View::At(top) = self.view
            && self.lines_from(top, width, h) <= h
        {
            self.go_bottom();
        }
    }

    /// The line at the viewport's top: the anchor, or `h` lines up from the end.
    fn top_line(&mut self, width: usize, h: usize) -> Pos {
        match self.view {
            View::At(p) => p,
            View::Follow => self.bottom_top(width, h),
        }
    }

    /// The top line of the last screenful: `h` lines up from the end.
    fn bottom_top(&mut self, width: usize, h: usize) -> Pos {
        match self.rows.len().checked_sub(1) {
            None => Pos { row: 0, line: 0 },
            Some(last) => {
                let end = Pos {
                    row: last,
                    line: self.rows[last].height(width) - 1,
                };
                self.up(end, h.saturating_sub(1), width)
            }
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

#[cfg(test)]
mod relayout_tests;
