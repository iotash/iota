//! The shared one-line input field — the bubbles-textinput replacement: value +
//! byte-offset cursor moved on grapheme boundaries, the enumerated emacs edit set
//! (`TUI_CONTRACTS` §6 row 8), and the `inputField` pan-window math + `inputCursorCols`
//! (tabbed.go:334-405) that own ONE model of what is visible. Shared by `PanelInput`,
//! the inline Custom editor, and the search query field.

use crate::text::width::{graphemes, str_width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A one-line text field with a real-terminal-cursor contract: no internal SGR, no
/// prompt, no own scrolling (the pan window below owns visibility — tabbed.go:366).
pub(crate) struct Field {
    value: String,
    /// Byte offset into `value`; always on a char (grapheme) boundary.
    cursor: usize,
}

impl Field {
    /// An empty field, cursor at the start.
    pub(crate) fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
        }
    }

    /// The typed value.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// Replaces the value; the cursor moves to the end.
    pub(crate) fn set_value(&mut self, s: &str) {
        s.clone_into(&mut self.value);
        self.cursor = self.value.len();
    }

    /// Clears the value.
    pub(crate) fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Moves the cursor to the end (Go textinput `CursorEnd` — search.go:398 searchEdit).
    pub(crate) fn move_to_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Places the cursor at a RUNE index (clamped) — the Go `SetCursor` twin; the
    /// surface tests drive the pan window through it.
    #[cfg(test)]
    pub(crate) fn set_cursor_runes(&mut self, idx: usize) {
        self.cursor = self
            .value
            .char_indices()
            .nth(idx)
            .map_or(self.value.len(), |(b, _)| b);
    }

    /// The cursor's ABSOLUTE display column in the full value (CJK = 2 cols).
    pub(crate) fn cursor_col(&self) -> usize {
        str_width(&self.value[..self.cursor])
    }

    /// Inserts text at the cursor (paste path; the caller flattens newlines).
    pub(crate) fn insert_str(&mut self, s: &str) {
        self.value.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// One key through the enumerated emacs subset (`TUI_CONTRACTS` §6 row 8 — shared
    /// verbatim by `PanelInput`, the Custom editor, and the search query field):
    /// ←/→ by grapheme · Home/End · Backspace/Ctrl+H grapheme-back · Delete ·
    /// Ctrl+A/E home/end · Ctrl+B/F char · Ctrl+K kill-to-end · Ctrl+U kill-to-start ·
    /// Ctrl+W word-back; any other char inserts.
    pub(crate) fn handle_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (ctrl, key.code) {
            (true, KeyCode::Char('a')) | (false, KeyCode::Home) => self.cursor = 0,
            (true, KeyCode::Char('e')) | (false, KeyCode::End) => self.cursor = self.value.len(),
            (true, KeyCode::Char('b')) | (false, KeyCode::Left) => {
                self.cursor = self.prev_boundary();
            }
            (true, KeyCode::Char('f')) | (false, KeyCode::Right) => {
                self.cursor = self.next_boundary();
            }
            (true, KeyCode::Char('k')) => self.value.truncate(self.cursor),
            (true, KeyCode::Char('u')) => {
                self.value.drain(..self.cursor);
                self.cursor = 0;
            }
            (true, KeyCode::Char('w')) => {
                let head = self.value[..self.cursor].trim_end();
                let cut = head.rfind(' ').map_or(0, |i| i + 1);
                self.value.drain(cut..self.cursor);
                self.cursor = cut;
            }
            (true, KeyCode::Char('h')) | (_, KeyCode::Backspace) => {
                let p = self.prev_boundary();
                self.value.drain(p..self.cursor);
                self.cursor = p;
            }
            (false, KeyCode::Delete) => {
                let n = self.next_boundary();
                self.value.drain(self.cursor..n);
            }
            (false, KeyCode::Char(c)) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
    }

    fn prev_boundary(&self) -> usize {
        let mut prev = 0;
        let mut at = 0;
        for g in graphemes(&self.value[..self.cursor]) {
            prev = at;
            at += g.len();
        }
        prev
    }

    fn next_boundary(&self) -> usize {
        graphemes(&self.value[self.cursor..])
            .next()
            .map_or(self.value.len(), |g| self.cursor + g.len())
    }
}

/// Renders a text field clipped to `box_w` columns and reports where the cursor sits
/// INSIDE that window, panning `off` as needed to keep the cursor visible
/// (tabbed.go:366-405 inputField).
///
/// The rendered view already ends in the cursor's own cell (a blank appended past the
/// value), so the value's last column and the cursor both fit inside the box. The
/// window never pans past the end (trailing blanks inside the box read as a rendering
/// bug), and a window that starts mid-glyph steps right until it lands on a boundary —
/// the cursor column follows the window, so it stays inside either way.
pub(crate) fn input_field(field: &Field, off: &mut usize, box_w: usize) -> (String, usize) {
    let box_w = box_w.max(1);
    // textinput draws a blank cell where the cursor sits, so the rendered field runs
    // one column past the value (tabbed_test.go:26 pins "short ").
    let full = format!("{} ", field.value());
    let cur = field.cursor_col();
    let total = str_width(&full);
    let mut o = *off;
    if total <= box_w {
        o = 0; // fits, cursor cell included: never pan
    } else {
        if o > cur {
            o = cur;
        }
        let edge = cur.saturating_sub(box_w - 1);
        if o < edge {
            o = edge;
        }
        // Don't pan past the end.
        let max = total - box_w;
        if o > max {
            o = max;
        }
    }
    // Cut the window, keeping any glyph that overlaps its edges (the x/ansi Cut
    // shape); step right while a wide glyph makes the cut overflow the box.
    loop {
        let view = cut_cols(&full, o, box_w);
        if str_width(&view) <= box_w || o >= total {
            *off = o;
            return (view, cur.saturating_sub(o));
        }
        o += 1;
    }
}

/// Glyphs overlapping display columns `[start, start+width)` — a wide rune straddling
/// either boundary is KEPT whole (the caller steps the window right on overflow).
fn cut_cols(s: &str, start: usize, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for g in graphemes(s) {
        let w = str_width(g);
        let end = col + w;
        if end > start && col < start + width {
            out.push_str(g);
        }
        col = end;
    }
    out
}
