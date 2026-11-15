//! The full screen's conversation search (tui-plan S3): the View panel's JUMP mode over the
//! buffer. The same matcher and walk (`surface::search`: `find_hits`, `step_hit`, `highlight_line`),
//! hits recorded on LOGICAL rows — a row before it is wrapped — and turned into a display line only
//! when the view jumps (`Conversation::jump_to`), so a match a wrap splits is found and lands on the
//! line its first byte is on.
//!
//! Hits are kept by block NUMBER (blocks are numbered as committed), so output arriving during a
//! search is scanned on its own — only the new blocks — and the cap dropping old blocks drops their
//! hits without moving the others. A hit is (block number, which occurrence in the block), and what
//! is scanned never depends on the width — a row's text, a laid-out block's plain lines at the width
//! it was first laid out at — so a resize rescans nothing and the walk keeps its hit: only the
//! block a jump lands in is laid out at the new width, to find the line its occurrence is on. The walk starts at the hit nearest above where the view was, the way
//! a search through a history goes back first; `n`/`p` step through document order and wrap at the
//! ends. Leaving keeps the view where the walk put it (`Enter`) or puts it back exactly where it was
//! before the search (`Esc`).

use super::conversation::{Conversation, Highlight};
use crate::ui::surface::search::{find_hits, step_hit};

/// What the keys ask of the search (the model owns the query field; see `keys.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchOp {
    /// A search starts (or the applied one is reopened for a new query).
    Open,
    /// The query as typed so far: the view rides to its nearest hit, or back to where it was.
    Query(String),
    /// `n` (`1`) or `p` (`-1`).
    Step(i32),
    /// Leave, the view staying on the hit.
    Accept,
    /// Leave, the view back where the search started.
    Cancel,
}

/// One search over the buffer.
pub(crate) struct Search {
    /// Where the view was when the search started (`Conversation::mark`).
    origin: Option<(u64, usize)>,
    /// The row number to start the walk above: the last row the view showed then.
    above: u64,
    query: String,
    /// (block number, which occurrence of the query in it), in document order.
    hits: Vec<(u64, usize)>,
    idx: usize,
    /// The blocks scanned so far, by number: the next one to scan.
    scanned: u64,
}

impl Search {
    /// A search starting from where the view is now; `above` is the last row shown.
    pub(crate) fn new(conv: &Conversation, above: u64) -> Self {
        Self {
            origin: conv.mark(),
            above,
            query: String::new(),
            hits: Vec::new(),
            idx: 0,
            scanned: conv.pushed(),
        }
    }

    /// Where the view was when the search started.
    pub(crate) const fn origin(&self) -> Option<(u64, usize)> {
        self.origin
    }

    /// Searches the whole buffer for `query`; parks on the hit nearest above where the view was.
    pub(crate) fn set_query(&mut self, conv: &mut Conversation, query: &str, width: usize) {
        query.clone_into(&mut self.query);
        conv.take_changed();
        self.hits = Self::scan(conv, 0, query, width);
        self.scanned = conv.pushed();
        self.idx = self
            .hits
            .iter()
            .rposition(|&(n, _)| n <= self.above)
            .unwrap_or(0);
    }

    /// Scans the blocks committed since the last scan, and again those replaced since (a live
    /// block that grew: its text is new), appending their hits; drops the hits of blocks the cap
    /// dropped. (`width` lays out only a block no draw has reached yet.)
    pub(crate) fn catch_up(&mut self, conv: &mut Conversation, width: usize) {
        let changed = conv.take_changed();
        if conv.pushed() == self.scanned && changed.is_none() {
            return;
        }
        let from = changed.map_or(self.scanned, |c| c.min(self.scanned));
        let parked = self.current();
        let gone = conv.first_held();
        self.hits.retain(|&(n, _)| n >= gone && n < from);
        self.hits.extend(Self::scan(conv, from, &self.query, width));
        self.scanned = conv.pushed();
        self.idx = parked
            .and_then(|h| self.hits.iter().position(|x| *x == h))
            .unwrap_or(0);
    }

    /// The query.
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// `n`/`p`, wrapping at the ends.
    pub(crate) fn step(&mut self, dir: i32) {
        if !self.hits.is_empty() {
            self.idx = step_hit(self.idx, self.hits.len(), dir);
        }
    }

    /// The hit the walk is on.
    pub(crate) fn current(&self) -> Option<(u64, usize)> {
        self.hits.get(self.idx).copied()
    }

    /// What a draw highlights.
    pub(crate) fn highlight(&self) -> Option<Highlight<'_>> {
        (!self.query.is_empty()).then(|| Highlight {
            query: &self.query,
            current: self.current(),
        })
    }

    /// The counter beside the query: `3/17`, or that nothing matched.
    pub(crate) fn status(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        match self.hits.len() {
            0 => "no match".to_owned(),
            n => format!("{}/{n}", self.idx + 1),
        }
    }

    fn scan(conv: &mut Conversation, from: u64, query: &str, width: usize) -> Vec<(u64, usize)> {
        let rows = conv.texts_from(from, width);
        let mut out: Vec<(u64, usize)> = Vec::new();
        for h in find_hits(rows.iter().map(|(_, t)| *t), query) {
            let n = rows[h.line].0;
            let k = match out.last() {
                Some(&(m, k)) if m == n => k + 1,
                _ => 0,
            };
            out.push((n, k));
        }
        out
    }
}

#[cfg(test)]
mod tests;
