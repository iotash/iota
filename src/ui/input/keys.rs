//! Composer key routing — the 9-row precedence table (`TUI_CONTRACTS` §6;
//! model.go updateKey, `KeyEventKind::Press` only): (full screen) the dump's return and Ctrl+O → surface → (full screen) the search → PgUp/PgDn/Ctrl+Home/Ctrl+End → Ctrl+C/D → ESC → Tab
//! completion → ↑ queue-pop → ↑/↓ history → newline (Ctrl+J, Alt+Enter, Shift+Enter) /
//! Enter submit → the enumerated emacs edit set → text insert.
//!
//! The newline keys live HERE, not in the shared `Editor::on_key`: the surface's one-line
//! `Field`s run that too, and they are one line by design (their pastes flatten newlines).
//! What reaches this table differs per key (`DIVERGENCES.md` X-65). Ctrl+J is LF, a different
//! byte from Enter's CR, so it inserts in every terminal. Alt+Enter and Shift+Enter insert only
//! where the terminal reports the modifier; an unreported Shift+Enter is a bare Enter and
//! submits — except under Ghostty's defaults, whose `ESC[27;2;13~` crossterm drops whole, so
//! nothing happens (one line of Ghostty config fixes it). Ctrl+Enter is not bound: without a
//! keyboard protocol it carries no CONTROL — the same CR as Enter, or dropped like Shift+Enter
//! under Ghostty — so a newline on it would work almost nowhere; where it arrives, it submits.
//!
//! Any key that is not Tab ends the completion cycle (model.go:436); any key that
//! reaches the edit set ends history navigation (model.go:493), except ↑/↓, which only move
//! the cursor there — a multi-line entry is walked through, not out of (X-89). ESC with no
//! scopes falls through to the edit set exactly like Go — the fall-through is what resets the
//! completion cycle and history navigation at idle (the contract table's row-3 "else
//! no-op" refers to scope firing only).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::suggest;
use crate::ui::runtime::event_loop::Model;
use crate::ui::runtime::screen::search::SearchOp;
use crate::ui::runtime::screen::{Prompt, ViewOp};
use crate::ui::surface::field::{Field, input_field};

/// Routes one key press through the composer precedence table
/// (model.go updateKey, `KeyEventKind::Press` only).
pub(crate) fn update_key(m: &mut Model, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Row 0: the full screen's dump. Ctrl+O writes the conversation out on the main screen, and
    // the next key — whatever it is, and only that — comes back (tui-plan D5). Ahead of the
    // surface, so the conversation can be written out while a dialog waits.
    if m.dumped {
        m.dumped = false;
        m.view_ops.push(ViewOp::Return);
        return;
    }
    if m.fullscreen
        && key.code == KeyCode::Char('o')
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        m.dumped = true;
        m.view_ops.push(ViewOp::Dump);
        return;
    }

    // Row 1: an open surface owns ALL keys (rendered below the composer); its
    // ESC/Ctrl+C NEVER fires turn scopes (model.go:377-394).
    if m.surface.is_some() {
        m.route_surface_key(key);
        return;
    }

    // Row 1a: the full screen's conversation search (tui-plan S3). Ctrl+R opens it anywhere; `/`
    // opens it on a scrolled-back view with an empty composer — following the tail, a `/` there
    // starts a slash command. Once open, the search takes the keys it owns (see `search_key`).
    if m.fullscreen {
        if m.search.is_some() {
            if search_key(m, &key) {
                return;
            }
        } else if opens_search(m, &key) {
            m.search = Some(SearchField::new());
            m.view_ops.push(ViewOp::Search(SearchOp::Open));
            return;
        }
    }

    // Row 1b: the full screen's view keys (tui-plan D5) — PgUp/PgDn half a screen, Ctrl+Home
    // the first line, Ctrl+End the tail (followed again; PgDn reaching it does the same). None
    // of them is bound elsewhere: the edit set takes Home/End without Ctrl only. The inline
    // terminal leaves them to the edit set, as before.
    if m.fullscreen
        && let Some(how) = view_key(&key)
    {
        m.view_ops.push(how);
        return;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Row 2: Ctrl+C / Ctrl+D — scopes fire the TURN cancel; idle interrupts a
    // parked waiter (the caller's cue to exit — double-Ctrl+C-exits emerges).
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
        if m.cancels.is_empty() {
            m.fail_waiter_interrupted();
        } else {
            m.fire_cancel(0);
        }
        return;
    }

    // Row 3: ESC fires the INNERMOST scope; with none it falls through (below) to
    // the edit set, where it lands as a state-reset no-op — Go parity.
    if key.code == KeyCode::Esc && !m.cancels.is_empty() {
        m.fire_cancel(m.cancels.len() - 1);
        return;
    }

    // Row 4: Tab cycles the completion matches of the prefix captured at the FIRST
    // press, writing each value straight into the composer; Enter is never claimed.
    if key.code == KeyCode::Tab {
        suggest::tab_complete(&mut m.composer, &m.commands);
        return;
    }
    // Any other key ends the completion cycle (model.go:436).
    m.composer.suggestion_base.clear();
    m.composer.suggestion_index = None;

    // Row 5: ↑ on an EMPTY composer pops the NEWEST queued item (LIFO, one per
    // press) — popping IS the un-queue (model.go:444-449). Only what the USER typed
    // pops: a host notice is not a draft to edit, and taking it out of the queue would
    // lose it.
    if key.code == KeyCode::Up
        && m.composer.is_blank()
        && let Some(i) = m.newest_typed()
    {
        if let Some(last) = m.take_queued_typed(i) {
            m.composer.set_value(&last);
        }
        return;
    }

    // Row 6: ↑ on the draft's top display row and ↓ on its bottom one walk the input
    // history; anywhere else the arrows move the cursor (the edit set). A one-row draft is
    // on both edges.
    if key.code == KeyCode::Up && m.composer.at_first_row(m.frame_width()) {
        m.composer.history_up();
        return;
    }
    if key.code == KeyCode::Down
        && m.composer.at_last_row(m.frame_width())
        && m.composer.history_can_forward()
    {
        m.composer.history_down();
        return;
    }

    // Row 7a: a newline into the draft, never a submit. Ctrl+J is the one chord every
    // target terminal delivers distinctly (raw-mode LF); Alt+Enter is ESC CR; Shift+Enter
    // arrives only when the terminal itself reports SHIFT (a CSI-u mapping, tmux
    // `extended-keys`, the Windows console) — elsewhere it is a bare Enter that submits
    // below, or, under Ghostty's defaults, no event at all (X-65).
    let newline = (ctrl && key.code == KeyCode::Char('j'))
        || (key.code == KeyCode::Enter
            && key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT));
    if newline {
        m.composer.insert_str("\n");
        m.composer.end_history_nav();
        return;
    }

    // Row 7: Enter submits (trim; empty ignored; waiter else queue; reset) — Ctrl+Enter
    // included.
    if key.code == KeyCode::Enter {
        m.submit();
        return;
    }

    // Rows 8–9: the editing set + text insert; any edit ends history navigation. ↑/↓ reach
    // here only to move between rows, and a row move inside a recalled multi-line entry must
    // leave the walk on it, or the ↑ off its top row would restart from the newest entry and
    // overwrite the saved draft with the entry.
    m.composer.handle_edit_key(&key, m.frame_width());
    if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
        m.composer.end_history_nav();
    }
}

/// The conversation search's query field (the model's half of the search; the hits and the walk
/// live with the buffer, `screen::search`). Typing, every key edits the query and the view rides
/// to its nearest hit; applied, `n`/`p` walk the hits.
pub(crate) struct SearchField {
    field: Field,
    typing: bool,
    /// The field's pan window (`input_field`).
    offset: usize,
}

impl SearchField {
    fn new() -> Self {
        Self {
            field: Field::new(),
            typing: true,
            offset: 0,
        }
    }

    /// A paste into the query being typed (its newlines flattened): the query it makes, or `None`
    /// when the query is not being typed.
    pub(crate) fn paste(&mut self, data: &str) -> Option<SearchOp> {
        if !self.typing {
            return None;
        }
        self.field.insert_str(&data.replace(['\r', '\n'], " "));
        Some(SearchOp::Query(self.field.value().to_owned()))
    }

    /// The search line's query: the field's window and cursor while typing, the query once applied.
    pub(crate) fn prompt(&mut self, width: usize) -> Prompt {
        if !self.typing {
            return Prompt {
                text: self.field.value().to_owned(),
                cursor: None,
            };
        }
        let (text, cursor) = input_field(&self.field, &mut self.offset, width / 2);
        Prompt {
            text,
            cursor: Some(cursor),
        }
    }
}

/// Whether `key` opens a search: Ctrl+R anywhere, `/` on a scrolled-back view with nothing typed.
fn opens_search(m: &Model, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    (ctrl && key.code == KeyCode::Char('r'))
        || (key.code == KeyCode::Char('/') && !ctrl && m.view_paused && m.composer.is_blank())
}

/// One key while a search is on; `false` hands it on down the table. Typing: Enter applies the
/// query (an empty one ends the search), Esc ends it with the view back where it started,
/// Ctrl+C/Ctrl+D keep their meaning, every other key edits the query. Applied: `n`/`p` walk,
/// `/` or Ctrl+R starts a new query, Enter leaves with the view on the hit, Esc leaves with it
/// back where it started; any other character leaves like Enter and is typed, and the view keys
/// (`PgUp`, `Ctrl+End`…) work with the search still on.
fn search_key(m: &mut Model, key: &KeyEvent) -> bool {
    let Some(s) = m.search.as_mut() else {
        return false;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
        return false;
    }
    let op = if s.typing {
        match key.code {
            KeyCode::Esc => None,
            KeyCode::Enter if s.field.value().trim().is_empty() => None,
            KeyCode::Enter => {
                s.typing = false;
                return true;
            }
            _ => {
                s.field.handle_key(key);
                Some(SearchOp::Query(s.field.value().to_owned()))
            }
        }
    } else {
        match key.code {
            KeyCode::Char('n') if !ctrl => Some(SearchOp::Step(1)),
            KeyCode::Char('p') if !ctrl => Some(SearchOp::Step(-1)),
            KeyCode::Char('/') if !ctrl => Some(SearchOp::Open),
            KeyCode::Char('r') if ctrl => Some(SearchOp::Open),
            KeyCode::Esc => None,
            KeyCode::Enter => {
                m.search = None;
                m.view_ops.push(ViewOp::Search(SearchOp::Accept));
                return true;
            }
            KeyCode::Char(_) => {
                m.search = None;
                m.view_ops.push(ViewOp::Search(SearchOp::Accept));
                return false;
            }
            _ => return false,
        }
    };
    match op {
        None => {
            m.search = None;
            m.view_ops.push(ViewOp::Search(SearchOp::Cancel));
        }
        Some(SearchOp::Open) => {
            s.typing = true;
            s.field.clear();
            s.offset = 0;
            m.view_ops.push(ViewOp::Search(SearchOp::Open));
        }
        Some(op) => m.view_ops.push(ViewOp::Search(op)),
    }
    true
}

/// The full screen's scroll request for `key`, if it is one.
fn view_key(key: &KeyEvent) -> Option<ViewOp> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => Some(ViewOp::HalfUp),
        KeyCode::PageDown => Some(ViewOp::HalfDown),
        KeyCode::Home if ctrl => Some(ViewOp::Top),
        KeyCode::End if ctrl => Some(ViewOp::Bottom),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
