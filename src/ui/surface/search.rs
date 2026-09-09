//! Panel search (internal/ui/search.go, 1:1): row panels FILTER (original-index law,
//! view map never empty, Other…-never-filtered, no-match-shows-all, checks by
//! underlying index), View panels JUMP (hits on logical lines, n/p walker, re-anchor
//! on refresh); both share the `/` entry, the query field that takes over the hint
//! row, and the case-folded matcher. `ascii_fold` preserves byte length exactly — the
//! invariant the whole offset scheme rests on; `highlight_line` replays a line's own
//! SGR state after every highlight closes.

use crate::text::ansi::{sgr_carry, strip_sgr};

use super::tabbed::{PanelState, panel_height};
use super::theme::{SEARCH_CUR, SEARCH_HIT};
use crate::ui::facade::{Panel, PanelKind};

/// The per-panel search mode (search.go:24-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SearchMode {
    /// No search.
    #[default]
    Off,
    /// The query line owns the keyboard.
    Typing,
    /// Row panels: filtered; View: n/p navigation.
    Applied,
}

/// One occurrence in a View panel: the LOGICAL line (pre-wrap) and the byte offset of
/// the match within that line's plain text (search.go:35-38).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SearchHit {
    /// Logical line index.
    pub(crate) line: usize,
    /// Byte offset within the line's plain text.
    pub(crate) off: usize,
}

/// One panel's search state; lives per panel so tabbing away and back leaves a filter
/// (or a highlight) exactly as it was (search.go:42-59).
pub(crate) struct SearchState {
    /// The mode.
    pub(crate) mode: SearchMode,
    /// The applied query (`""` = none).
    pub(crate) query: String,
    /// The query field (styled like the Custom editor: no prompt, no internal SGR,
    /// real terminal cursor for IME).
    pub(crate) input: super::field::Field,
    /// View hits, in document order.
    pub(crate) hits: Vec<SearchHit>,
    /// The hit n/p is parked on.
    pub(crate) hit_idx: usize,
    /// How many rows the last rebuild actually matched (row panels). NOT `view.len()`:
    /// a query matching nothing falls back to showing everything, and the hint row
    /// must tell those two apart.
    pub(crate) matched: usize,
    /// The query field's horizontal window start column (`input_field`).
    pub(crate) input_offset: usize,
}

impl SearchState {
    /// Fresh, off.
    pub(crate) fn new() -> Self {
        Self {
            mode: SearchMode::Off,
            query: String::new(),
            input: super::field::Field::new(),
            hits: Vec::new(),
            hit_idx: 0,
            matched: 0,
            input_offset: 0,
        }
    }
}

// --- matching ---------------------------------------------------------------

/// Lowercases A–Z and nothing else. Byte length is preserved exactly, which is what
/// lets match offsets index back into the original string; full Unicode lowercasing
/// cannot promise that (U+0130 lowercases to two runes). Case only distinguishes
/// ASCII anyway (search.go:79).
pub(crate) fn ascii_fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// The non-overlapping `[start, end)` byte ranges where `query` occurs in `plain`,
/// case-insensitively (search.go:91).
pub(crate) fn match_ranges(plain: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() || query.len() > plain.len() {
        return Vec::new();
    }
    let (lp, lq) = (ascii_fold(plain), ascii_fold(query));
    let mut out = Vec::new();
    let mut from = 0;
    while from + lq.len() <= lp.len() {
        let Some(i) = lp[from..].find(&lq) else { break };
        let s = from + i;
        out.push((s, s + lq.len()));
        from = s + lq.len();
    }
    out
}

/// Whether `query` occurs in `text` case-insensitively (search.go:110).
pub(crate) fn matches_query(text: &str, query: &str) -> bool {
    query.is_empty() || ascii_fold(text).contains(&ascii_fold(query))
}

// --- highlighting -----------------------------------------------------------

/// Byte length of the ANSI CSI escape starting at `b[i]`, or 0 (clip.go ansiLen — the
/// CSI-only scanner `highlight_line`'s walk uses, exactly like Go).
fn csi_len_at(b: &[u8], i: usize) -> usize {
    if i + 1 >= b.len() || b[i] != 0x1b || b[i + 1] != b'[' {
        return 0;
    }
    let mut j = i + 2;
    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
        j += 1;
    }
    if j < b.len() {
        j += 1; // include the final byte
    }
    j - i
}

/// Reverse-videos every occurrence of `query` in `line`, which may already carry its
/// own SGR (search.go:134-180).
///
/// Offsets are computed on the PLAIN text and walked back through the escapes, so a
/// highlight can never land inside an escape sequence and slice it apart. A highlight
/// closes with a reset followed by a REPLAY of the line's own SGR state — without the
/// replay, the first hit in a coloured line would strip the colour off everything
/// after it. Escapes met inside a hit re-assert the highlight in the other direction.
/// `cur_off` is the plain-text offset of the hit n/p is parked on (`None` for none),
/// which gets the brighter treatment.
pub(crate) fn highlight_line(line: &str, query: &str, cur_off: Option<usize>) -> String {
    if query.is_empty() {
        return line.to_owned();
    }
    let ranges = match_ranges(&strip_sgr(line), query);
    if ranges.is_empty() {
        return line.to_owned();
    }

    let bytes = line.as_bytes();
    let mut b = String::with_capacity(line.len() + 16);
    let mut state = String::new(); // the line's own SGR, as of this point
    let mut open = ""; // the highlight SGR while one is open
    let mut hl_end: Option<usize> = None; // plain offset where the open highlight ends
    let mut pos = 0; // plain-text byte offset
    let mut ri = 0;

    let mut i = 0;
    while i < line.len() {
        let n = csi_len_at(bytes, i);
        if n > 0 {
            let seq = &line[i..i + n];
            b.push_str(seq);
            state = sgr_carry(&state, seq);
            if hl_end.is_some() {
                b.push_str(open); // the line's own escape may have reset us
            }
            i += n;
            continue;
        }
        if hl_end.is_some_and(|end| pos >= end) {
            b.push_str("\x1b[0m");
            b.push_str(&state);
            open = "";
            hl_end = None;
        }
        while ri < ranges.len() && ranges[ri].0 < pos {
            ri += 1;
        }
        if hl_end.is_none() && ri < ranges.len() && pos == ranges[ri].0 {
            open = if cur_off == Some(pos) {
                SEARCH_CUR
            } else {
                SEARCH_HIT
            };
            b.push_str(open);
            hl_end = Some(ranges[ri].1);
            ri += 1;
        }
        let size = line[i..].chars().next().map_or(1, char::len_utf8);
        b.push_str(&line[i..i + size]);
        pos += size;
        i += size;
    }
    if hl_end.is_some() {
        b.push_str("\x1b[0m");
        b.push_str(&state);
    }
    b
}

// --- row-panel plumbing + lifecycle (methods on the per-panel state) ---------

impl PanelState {
    /// How many underlying rows the panel holds, filtering aside (search.go:198).
    pub(crate) fn row_count(&self, p: &Panel) -> usize {
        if p.kind() == PanelKind::Browser {
            self.entries.len()
        } else {
            self.items.len()
        }
    }

    /// Row `i`'s text for matching, stripped of any styling the caller baked in
    /// (search.go:207).
    pub(crate) fn row_text(&self, p: &Panel, i: usize) -> String {
        if p.kind() == PanelKind::Browser {
            self.entries
                .get(i)
                .map(|e| e.name.clone())
                .unwrap_or_default()
        } else {
            self.items.get(i).map(|s| strip_sgr(s)).unwrap_or_default()
        }
    }

    /// Recomputes the visible-row mapping. `view` is ALWAYS populated (identity when
    /// nothing is filtered), so rendering and navigation have one code path. Two rows
    /// never get filtered out: a Custom panel's `"Other…"`, and every row when the
    /// query matches nothing at all (search.go:228-263).
    pub(crate) fn rebuild_view(&mut self, p: &Panel) {
        let n = self.row_count(p);
        self.view.clear();
        let q = if p.kind() == PanelKind::View || self.search.mode == SearchMode::Off {
            String::new()
        } else {
            self.search_draft()
        };
        if q.is_empty() {
            self.search.matched = n;
            self.view.extend(0..n);
            return;
        }
        let other_idx = if p.custom() {
            Some(p.items().len())
        } else {
            None
        };
        let mut hits = 0;
        for i in 0..n {
            if Some(i) == other_idx {
                self.view.push(i);
                continue;
            }
            if matches_query(&self.row_text(p, i), &q) {
                self.view.push(i);
                hits += 1;
            }
        }
        self.search.matched = hits;
        if hits == 0 {
            // Nothing matched: show everything rather than nothing.
            self.view.clear();
            self.view.extend(0..n);
        }
    }

    /// How many rows a live filter keeps; `false` when the query matches nothing (the
    /// view then shows everything, unfiltered — search.go:273).
    pub(crate) fn filtered_count(&self, p: &Panel) -> (usize, bool) {
        if self.search_draft().is_empty() {
            (self.row_count(p), true)
        } else {
            (self.search.matched, self.search.matched > 0)
        }
    }

    /// The query being typed, or the applied one once committed (search.go:281).
    pub(crate) fn search_draft(&self) -> String {
        if self.search.mode == SearchMode::Typing {
            self.search.input.value().to_owned()
        } else {
            self.search.query.clone()
        }
    }

    /// Where the cursor sits among the VISIBLE rows (0 when the cursor's row was
    /// filtered out from under it — search.go:290).
    pub(crate) fn view_pos(&self) -> usize {
        self.view
            .iter()
            .position(|&idx| idx == self.cursor)
            .unwrap_or(0)
    }

    /// Moves the cursor to visible row `vp`, translating back to the underlying index
    /// the caller reads at commit (search.go:301).
    pub(crate) fn set_view_pos(&mut self, vp: isize) {
        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.view.len() - 1;
        let idx = usize::try_from(vp.max(0)).unwrap_or(0).min(max);
        self.cursor = self.view[idx];
    }

    /// Pulls the cursor back onto a visible row after a refilter (search.go:310).
    pub(crate) fn sync_cursor(&mut self) {
        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        if self.view.contains(&self.cursor) {
            return;
        }
        self.cursor = self.view[0];
    }

    /// Gates `'/'`: row panels need the opt-in flag, View needs nothing, and both need
    /// content that actually overflows its window (search.go:328-346). The row count
    /// judged is the UNFILTERED one: refining a filter stays possible.
    pub(crate) fn search_available(&self, p: &Panel) -> bool {
        match p.kind() {
            PanelKind::View => {
                let mut n = self.items.len();
                if p.wrap() && self.wrapped > 0 {
                    n = self.wrapped;
                }
                n > panel_height(p, n)
            }
            PanelKind::List | PanelKind::Multi | PanelKind::Picker | PanelKind::Browser => {
                if !p.search {
                    return false;
                }
                let n = self.row_count(p);
                n > panel_height(p, n)
            }
            _ => false,
        }
    }

    /// Starts a FRESH query: the field comes up empty even when a query is already
    /// applied — `'/'` means "search for something new"; refining is `search_edit`'s
    /// job (search.go:350).
    pub(crate) fn search_open(&mut self, p: &Panel) {
        self.search.mode = SearchMode::Typing;
        self.search.input.clear();
        self.search_live(p);
    }

    /// Re-applies the in-progress query on every keystroke: a row panel narrows as you
    /// type, a View re-collects its hits and rides to the first one (search.go:362).
    pub(crate) fn search_live(&mut self, p: &Panel) {
        if p.kind() == PanelKind::View {
            let q = self.search.input.value().to_owned();
            self.collect_hits(&q);
            if let Some(first) = self.search.hits.first() {
                self.search.hit_idx = 0;
                self.pending_center = Some(first.line);
            }
            return;
        }
        self.rebuild_view(p);
        self.sync_cursor();
    }

    /// Commits the query: Enter hands the keyboard back to the panel (filtered, for a
    /// row panel) or to n/p (for a View) — search.go:377.
    pub(crate) fn search_apply(&mut self, p: &Panel) {
        self.search.query = self.search.input.value().trim().to_owned();
        if self.search.query.is_empty() {
            self.search_clear(p);
            return;
        }
        self.search.mode = SearchMode::Applied;
        if p.kind() == PanelKind::View {
            let q = self.search.query.clone();
            self.collect_hits(&q);
            if let Some(hit) = self.search.hits.get(self.search.hit_idx) {
                self.pending_center = Some(hit.line);
            }
            return;
        }
        self.rebuild_view(p);
        self.sync_cursor();
    }

    /// Reopens the query field from the applied state (View's q/Esc) to REFINE it, so
    /// here the applied query is seeded with the cursor after it (search.go:398).
    pub(crate) fn search_edit(&mut self) {
        self.search.mode = SearchMode::Typing;
        let q = self.search.query.clone();
        self.search.input.set_value(&q);
        self.search.input.move_to_end();
    }

    /// Drops the search entirely: the filter lifts, the highlight goes, and the panel
    /// is exactly as it was before `'/'` (search.go:408).
    pub(crate) fn search_clear(&mut self, p: &Panel) {
        self.search.mode = SearchMode::Off;
        self.search.query.clear();
        self.search.hits.clear();
        self.search.hit_idx = 0;
        self.search.input.clear();
        if p.kind() != PanelKind::View {
            self.rebuild_view(p);
            self.sync_cursor();
        }
    }

    /// Finds every occurrence in a View's LOGICAL lines. Wrapping is a render-time
    /// concern, so hits are recorded against logical lines and converted when the
    /// panel draws (search.go:426).
    pub(crate) fn collect_hits(&mut self, query: &str) {
        self.search.hits.clear();
        self.search.hit_idx = 0;
        if query.trim().is_empty() {
            return;
        }
        for (i, line) in self.items.iter().enumerate() {
            for (start, _) in match_ranges(&strip_sgr(line), query) {
                self.search.hits.push(SearchHit {
                    line: i,
                    off: start,
                });
            }
        }
    }

    /// Rescans after a live panel's content changed, keeping the walker parked where
    /// it was. The anchor is the HIT itself, not its ordinal: content that grew above
    /// it shifts every index but not the match the user is reading (search.go:445).
    pub(crate) fn recollect_hits(&mut self, query: &str) {
        let Some(anchor) = self.current_hit() else {
            self.collect_hits(query);
            return;
        };
        self.collect_hits(query);
        if let Some(i) = self.search.hits.iter().position(|h| *h == anchor) {
            self.search.hit_idx = i;
            return;
        }
        // The anchored hit is gone (its line changed): settle on the first one still
        // at or after where it used to be, so the walker does not leap.
        if let Some(i) = self
            .search
            .hits
            .iter()
            .position(|h| h.line > anchor.line || (h.line == anchor.line && h.off > anchor.off))
        {
            self.search.hit_idx = i;
            return;
        }
        if let Some(n) = self.search.hits.len().checked_sub(1) {
            self.search.hit_idx = n;
        }
    }

    /// Walks the hits (`+1` next / `-1` prev), wrapping around, and asks the next
    /// render to centre the landing line (search.go:474).
    pub(crate) fn search_step(&mut self, dir: i32) {
        let n = self.search.hits.len();
        if n == 0 {
            return;
        }
        self.search.hit_idx = if dir > 0 {
            (self.search.hit_idx + 1) % n
        } else {
            (self.search.hit_idx + n - 1) % n
        };
        self.pending_center = Some(self.search.hits[self.search.hit_idx].line);
    }

    /// The hit n/p is parked on, or `None` (search.go:484).
    pub(crate) fn current_hit(&self) -> Option<SearchHit> {
        if self.search.mode == SearchMode::Off {
            return None;
        }
        self.search.hits.get(self.search.hit_idx).copied()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod view_search_tests;
