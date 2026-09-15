//! The editing core the composer and the surface's one-line field share (Phase 5 PR-15): a
//! value with a byte-offset cursor that only ever rests on a grapheme boundary, and the
//! enumerated emacs edit set (`TUI_CONTRACTS` §6 rows 8–9) as ONE key handler, [`Editor::on_key`].
//! Line-scoped where it matters — Home/End, Ctrl+K/U/W stop at the logical line under the
//! cursor — which a single-line user never notices, since its value carries no `'\n'`.
//!
//! What is NOT here is anyone's ladder: ↑/↓ (the composer's history or display-row moves), Tab,
//! Enter, ESC and the control chords a surface owns stay with their owners, and `on_key` answers
//! `false` for them so the owner can route on. Every arm is pinned in `keys/tests.rs`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::text::width::graphemes;

/// A text value and its cursor, edited through the emacs subset.
pub(crate) struct Editor {
    value: String,
    /// Byte offset into `value`; always on a grapheme boundary.
    cursor: usize,
}

impl Editor {
    /// An empty value, cursor at the start.
    pub(crate) fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
        }
    }

    /// The value as typed.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// The cursor's byte offset (a grapheme boundary).
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Places the cursor at byte `at`, clamped to the value; the caller's offset must sit on
    /// a grapheme boundary (the composer's display-row moves walk graphemes to find one).
    pub(crate) fn set_cursor(&mut self, at: usize) {
        let at = at.min(self.value.len());
        debug_assert!(
            self.value.is_char_boundary(at),
            "cursor off a char boundary"
        );
        self.cursor = at;
    }

    /// Replaces the value; the cursor moves to the end.
    pub(crate) fn set_value(&mut self, s: &str) {
        s.clone_into(&mut self.value);
        self.cursor = self.value.len();
    }

    /// Clears the value; the cursor returns to the start.
    pub(crate) fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Moves the cursor to the end.
    pub(crate) fn move_to_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Inserts text at the cursor (the paste path; the cursor lands after it).
    pub(crate) fn insert_str(&mut self, s: &str) {
        self.value.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// The editing set (`TUI_CONTRACTS` §6 rows 8–9): ←/→ by grapheme · Home/End ·
    /// Backspace (under any modifier) / Ctrl+H grapheme-back · Delete · Ctrl+A/E line
    /// start/end · Ctrl+B/F char · Ctrl+K kill-to-line-end · Ctrl+U kill-to-line-start ·
    /// Ctrl+W word-back — line-scoped like the bubbles textarea it replaces — and text insert
    /// of every char key that is not a control chord (Alt+char included: an ESC-prefixed
    /// sequence lands as its letter). Returns whether the key was one of these; a `false`
    /// leaves the value and cursor untouched and hands the key back to the owner.
    pub(crate) fn on_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (ctrl, key.code) {
            (true, KeyCode::Char('a')) | (false, KeyCode::Home) => {
                self.cursor = self.line_start();
            }
            (true, KeyCode::Char('e')) | (false, KeyCode::End) => {
                self.cursor = self.line_end();
            }
            (true, KeyCode::Char('b')) | (false, KeyCode::Left) => {
                self.cursor = self.prev_boundary();
            }
            (true, KeyCode::Char('f')) | (false, KeyCode::Right) => {
                self.cursor = self.next_boundary();
            }
            (true, KeyCode::Char('k')) => {
                let end = self.line_end();
                self.value.drain(self.cursor..end);
            }
            (true, KeyCode::Char('u')) => {
                let start = self.line_start();
                self.value.drain(start..self.cursor);
                self.cursor = start;
            }
            (true, KeyCode::Char('w')) => self.delete_word_back(),
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
            _ => return false,
        }
        true
    }

    /// Byte start of the logical line under the cursor.
    fn line_start(&self) -> usize {
        self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    /// Byte end of the logical line under the cursor (exclusive of the newline).
    fn line_end(&self) -> usize {
        self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |i| self.cursor + i)
    }

    /// Ctrl+W: skip whitespace back, then delete to the start of the previous word —
    /// never crossing the line start (bubbles deleteWordLeft parity).
    fn delete_word_back(&mut self) {
        let ls = self.line_start();
        let head = &self.value[ls..self.cursor];
        let trimmed = head.trim_end_matches(char::is_whitespace);
        let cut_rel = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let cut = ls + cut_rel;
        self.value.drain(cut..self.cursor);
        self.cursor = cut;
    }

    /// The grapheme boundary before the cursor.
    fn prev_boundary(&self) -> usize {
        let mut prev = 0;
        let mut at = 0;
        for g in graphemes(&self.value[..self.cursor]) {
            prev = at;
            at += g.len();
        }
        prev
    }

    /// The grapheme boundary after the cursor.
    fn next_boundary(&self) -> usize {
        graphemes(&self.value[self.cursor..])
            .next()
            .map_or(self.value.len(), |g| self.cursor + g.len())
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// `on_key` owns exactly the edit set: an unowned key answers `false` and leaves the
    /// value and cursor alone; an owned one answers `true` even when it changes nothing.
    #[test]
    fn on_key_says_what_it_owns() {
        let mut e = Editor::new();
        e.set_value("ab");
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::F(1),
        ] {
            assert!(
                !e.on_key(&KeyEvent::new(code, KeyModifiers::NONE)),
                "{code:?}"
            );
        }
        assert!(!e.on_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)));
        assert_eq!((e.value(), e.cursor()), ("ab", 2));
        assert!(
            e.on_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            "→ at the end is still owned"
        );
        assert!(e.on_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert_eq!(e.value(), "abc");
    }

    /// `set_cursor` clamps to the value.
    #[test]
    fn set_cursor_clamps() {
        let mut e = Editor::new();
        e.set_value("中文");
        e.set_cursor(99);
        assert_eq!(e.cursor(), 6);
        e.set_cursor(3);
        assert_eq!(e.cursor(), 3);
    }
}
