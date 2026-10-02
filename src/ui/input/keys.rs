//! Composer key routing — the 9-row precedence table (`TUI_CONTRACTS` §6;
//! model.go updateKey, `KeyEventKind::Press` only): surface → Ctrl+C/D → ESC → Tab
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
//! reaches the edit set ends history navigation (model.go:493). ESC with no scopes
//! falls through to the edit set exactly like Go — the fall-through is what resets the
//! completion cycle and history navigation at idle (the contract table's row-3 "else
//! no-op" refers to scope firing only).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::suggest;
use crate::ui::runtime::event_loop::Model;

/// Routes one key press through the composer precedence table
/// (model.go updateKey, `KeyEventKind::Press` only).
pub(crate) fn update_key(m: &mut Model, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Row 1: an open surface owns ALL keys (rendered below the composer); its
    // ESC/Ctrl+C NEVER fires turn scopes (model.go:377-394).
    if m.surface.is_some() {
        m.route_surface_key(key);
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

    // Row 6: ↑/↓ walk the input history while the composer holds a single wrapped
    // row (multi-row drafts keep the arrows for cursor movement — the edit set).
    if key.code == KeyCode::Up && m.composer.history_navigable(m.frame_width()) {
        m.composer.history_up();
        return;
    }
    if key.code == KeyCode::Down
        && m.composer.history_navigable(m.frame_width())
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

    // Rows 8–9: the editing set + text insert; any edit ends history navigation.
    m.composer.handle_edit_key(&key, m.frame_width());
    m.composer.end_history_nav();
}

#[cfg(test)]
mod tests;
