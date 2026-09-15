//! Composer key routing — the 9-row precedence table (`TUI_CONTRACTS` §6;
//! model.go updateKey, `KeyEventKind::Press` only): surface → Ctrl+C/D → ESC → Tab
//! completion → ↑ queue-pop → ↑/↓ history → Enter → the enumerated emacs edit set →
//! text insert.
//!
//! Any key that is not Tab ends the completion cycle (model.go:436); any key that
//! reaches the edit set ends history navigation (model.go:493). ESC with no scopes
//! falls through to the edit set exactly like Go — the fall-through is what resets the
//! completion cycle and history navigation at idle (the contract table's row-3 "else
//! no-op" refers to scope firing only).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::event_loop::Model;
use super::suggest;

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
    if key.code == KeyCode::Up && m.composer.history_navigable(m.width) {
        m.composer.history_up();
        return;
    }
    if key.code == KeyCode::Down
        && m.composer.history_navigable(m.width)
        && m.composer.history_can_forward()
    {
        m.composer.history_down();
        return;
    }

    // Row 7: Enter submits (trim; empty ignored; waiter else queue; reset).
    if key.code == KeyCode::Enter {
        m.submit();
        return;
    }

    // Rows 8–9: the editing set + text insert; any edit ends history navigation.
    m.composer.handle_edit_key(&key, m.width);
    m.composer.end_history_nav();
}

#[cfg(test)]
mod tests;
