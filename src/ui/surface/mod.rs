//! The declarative surface engine (`TUI_DESIGN` §6): the pure `surface_key` transition
//! fn + the strict-precedence dispatch ladder (model.go:587-876 surfaceKey; frozen
//! shape `TUI_CONTRACTS` §5/§6), the render/cursor/tick seams the loop consumes, paste
//! routing into input fields, and the exec clipboard.
//!
//! Ladder (strict precedence): (1) an open inline Custom editor owns the keyboard;
//! (2) `searchTyping` owns the keyboard; (3) View+`searchApplied` hands n/p/q/Esc to the
//! hit walker; (4) `PanelInput` owns letters; (5) Ctrl chords (c cancel, p/n ↑↓, b/f
//! page); (6) Tab = next panel (wraps), Esc/q = cancel, Enter = commit path (Browser
//! descend-vs-commit → Custom-open → `enter_advances` → commit-all); (7) per-kind keys.
//! ANY Tab press with the surface still open forces a full repaint (T-32 — the CJK
//! cell-diff insurance, `terminal.clear()` on the loop side).

pub(crate) mod field;
pub(crate) mod panels;
pub(crate) mod search;
pub(crate) mod tabbed;

// Sibling alias so the child modules can say `super::theme` (a habit from the crate era, when the
// `#[path]` test mounts compiled `surface/` standalone; still the shortest path).
pub(crate) use super::theme;

use std::io::{self, Write as _};
use std::process::{Command, Stdio};

use crate::text::ansi::strip_sgr;
use crate::ui::facade::{PanelKind, TabbedResult};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use search::SearchMode;
pub(crate) use tabbed::SurfaceState;
use tabbed::slider_step;

/// What a routed key did to the surface (frozen shape, `TUI_CONTRACTS` §5).
pub(crate) enum SurfaceEffect {
    /// Nothing the loop must act on (internal state may have moved).
    None,
    /// The surface closed with this result; the loop replies and drops it.
    Close(TabbedResult),
    /// Force a full-frame repaint (`terminal.clear()`) — the T-32 Tab-switch CJK
    /// cell-diff insurance.
    ForceRedraw,
}

/// A cancelled close.
fn cancelled() -> TabbedResult {
    TabbedResult {
        cancelled: true,
        ..TabbedResult::default()
    }
}

/// The pure key-transition fn (model.go:587 surfaceKey + the updateKey Tab wrapper,
/// model.go:377-393): routes one key press through the precedence ladder. Any Tab
/// press that leaves the surface open returns [`SurfaceEffect::ForceRedraw`] (T-32).
impl SurfaceState {
    /// One key press through the precedence ladder.
    pub(crate) fn key(&mut self, key: KeyEvent) -> SurfaceEffect {
        if key.kind != KeyEventKind::Press {
            return SurfaceEffect::None;
        }
        if self.slots.is_empty() {
            return SurfaceEffect::Close(cancelled());
        }
        let was_tab = key.code == KeyCode::Tab;
        match dispatch(self, &key) {
            Some(result) => SurfaceEffect::Close(result),
            None if was_tab => SurfaceEffect::ForceRedraw,
            None => SurfaceEffect::None,
        }
    }
}

/// The ladder body; `Some` = the surface closed with that result.
fn dispatch(st: &mut SurfaceState, key: &KeyEvent) -> Option<TabbedResult> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let focus = st.focus;

    // (1) An open inline "Other…" editor owns the keyboard (model.go:597-625).
    if matches!(
        st.slots[focus].spec.kind(),
        PanelKind::List | PanelKind::Multi
    ) && st.slots[focus].spec.custom()
        && st.slots[focus].state.editing
    {
        return editor_key(st, key, ctrl);
    }

    // (2) An open query field owns the keyboard — letters must type; every keystroke
    // re-applies the query live (model.go:631-644).
    //
    // A COMBO's field is open for the life of the panel, so the two keys that would LEAVE a
    // search mean something else there: Esc cancels the surface (there is no search to step out
    // of) and Enter commits the panel. The list is still navigable underneath — ↑↓ and Ctrl+P/N
    // reach it, ←→ stay with the text cursor, since a model id is long enough to edit inside.
    if st.slots[focus].state.search.mode == SearchMode::Typing {
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(cancelled());
        }
        let combo = st.slots[focus].spec.combo();
        if combo {
            match (ctrl, key.code) {
                (_, KeyCode::Esc) => return Some(cancelled()),
                (_, KeyCode::Enter) => return enter_commit(st),
                (_, KeyCode::Tab) => {
                    st.set_focus((focus + 1) % st.slots.len());
                    return None;
                }
                (false, KeyCode::Up) | (true, KeyCode::Char('p')) => {
                    let (p, ps) = st.focused();
                    ps.nav(p, -1);
                    return None;
                }
                (false, KeyCode::Down) | (true, KeyCode::Char('n')) => {
                    let (p, ps) = st.focused();
                    ps.nav(p, 1);
                    return None;
                }
                (false, KeyCode::PageUp) => {
                    let (p, ps) = st.focused();
                    ps.page(p, -1);
                    return None;
                }
                (false, KeyCode::PageDown) => {
                    let (p, ps) = st.focused();
                    ps.page(p, 1);
                    return None;
                }
                _ => {}
            }
        }
        let (p, ps) = st.focused();
        match key.code {
            KeyCode::Esc => ps.search_clear(p), // ESC in the field leaves search entirely
            KeyCode::Enter => ps.search_apply(p),
            _ => {
                ps.search.input.handle_key(key);
                ps.search_live(p);
            }
        }
        return None;
    }

    // (3) A View under an applied search hands n/p/q/Esc to the hit walker; every
    // other key still reaches the panel below (model.go:648-663).
    if st.slots[focus].spec.kind() == PanelKind::View
        && st.slots[focus].state.search.mode == SearchMode::Applied
    {
        let ps = &mut st.slots[focus].state;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                ps.search_edit();
                return None;
            }
            KeyCode::Char('n') if !ctrl => {
                ps.search_step(1);
                return None;
            }
            KeyCode::Char('p') if !ctrl => {
                ps.search_step(-1);
                return None;
            }
            _ => {}
        }
    }

    // (4) Input panels own the keyboard: letters must type, not navigate; only the
    // surface-level chords stay routed (model.go:668-686).
    if st.slots[focus].spec.kind() == PanelKind::Input {
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(cancelled());
        }
        match key.code {
            KeyCode::Tab => {
                st.set_focus((focus + 1) % st.slots.len());
                None
            }
            KeyCode::Esc => Some(cancelled()),
            KeyCode::Enter => {
                if st.enter_advances && focus + 1 < st.slots.len() {
                    st.set_focus(focus + 1);
                    return None;
                }
                Some(st.result())
            }
            _ => {
                st.slots[focus].state.input.handle_key(key);
                None
            }
        }
    } else if ctrl {
        // (5) readline heritage: Ctrl+P/N mirror ↑↓, Ctrl+B/F mirror ←→ (paging) —
        // model.go:688-703.
        let (p, ps) = st.focused();
        match key.code {
            KeyCode::Char('c') => return Some(cancelled()),
            KeyCode::Char('p') => ps.nav(p, -1),
            KeyCode::Char('n') => ps.nav(p, 1),
            KeyCode::Char('b') => ps.page(p, -1),
            KeyCode::Char('f') => ps.page(p, 1),
            _ => {}
        }
        None
    } else {
        code_key(st, key)
    }
}

/// Row 1 — the inline Custom editor: Ctrl+C cancels the surface; ESC closes JUST the
/// editor (back to the options — the ask survives); Enter confirms the text (a Multi
/// checks the row and stays, a single-select proceeds); else feed the field.
fn editor_key(st: &mut SurfaceState, key: &KeyEvent, ctrl: bool) -> Option<TabbedResult> {
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(cancelled());
    }
    let focus = st.focus;
    let enter_advances = st.enter_advances;
    let n = st.slots.len();
    let (p, ps) = st.focused();
    let other_idx = p.items().len();
    let multi = p.kind() == PanelKind::Multi;
    match key.code {
        KeyCode::Esc => ps.editing = false,
        KeyCode::Enter => {
            ps.editing = false;
            let text = ps.input.value().trim().to_owned();
            if multi {
                if text.is_empty() {
                    ps.checked.remove(&other_idx);
                } else {
                    ps.checked.insert(other_idx);
                }
                return None;
            }
            if text.is_empty() {
                return None; // nothing entered: stay on the options
            }
            if enter_advances && focus + 1 < n {
                st.set_focus(focus + 1);
                return None;
            }
            return Some(st.result());
        }
        _ => ps.input.handle_key(key),
    }
    None
}

/// Rows 6–7 — the surface chords and per-kind keys (model.go:704-876).
fn code_key(st: &mut SurfaceState, key: &KeyEvent) -> Option<TabbedResult> {
    let focus = st.focus;
    match key.code {
        KeyCode::Tab => {
            st.set_focus((focus + 1) % st.slots.len());
            None
        }
        KeyCode::Esc | KeyCode::Char('q') => Some(cancelled()),
        KeyCode::Enter => enter_commit(st),
        KeyCode::Up => {
            let (p, ps) = st.focused();
            ps.nav(p, -1);
            None
        }
        KeyCode::Down => {
            let (p, ps) = st.focused();
            ps.nav(p, 1);
            None
        }
        KeyCode::Left => {
            arrow(st, -1);
            None
        }
        KeyCode::Right => {
            arrow(st, 1);
            None
        }
        KeyCode::Char(' ') => space_key(st),
        KeyCode::Char(c) => char_key(st, c),
        _ => None,
    }
}

/// ←/→: slider steps, switch sets, everything else pages (model.go:741-760).
fn arrow(st: &mut SurfaceState, dir: i32) {
    let (p, ps) = st.focused();
    match p.kind() {
        PanelKind::Slider => slider_step(ps, p, dir),
        PanelKind::Switch => ps.on = dir > 0,
        // `signum` is -1, 0 or 1, so the narrowing to `isize` is total; spelling it
        // as a match keeps the crate free of `as` casts.
        _ => ps.page(
            p,
            match dir.signum() {
                1 => 1,
                -1 => -1,
                _ => 0,
            },
        ),
    }
}

/// Space: switch toggle, Multi check toggle, the Custom editor's check-by-editing
/// flow, View page-forward (model.go:761-788, 812-820 — the Code and Text arms are
/// identical; crossterm delivers one `Char(' ')`).
fn space_key(st: &mut SurfaceState) -> Option<TabbedResult> {
    let (p, ps) = st.focused();
    match p.kind() {
        PanelKind::Switch => ps.on = !ps.on,
        PanelKind::List => {
            if p.custom() && ps.cursor == p.items().len() {
                ps.editing = true; // Space (re)opens the editor on a single-select
                return None;
            }
        }
        PanelKind::Multi => {
            if p.custom() && ps.cursor == p.items().len() {
                if ps.checked.contains(&ps.cursor) {
                    ps.checked.remove(&ps.cursor); // uncheck; the draft text stays
                } else {
                    ps.editing = true; // check-by-editing (Enter confirms)
                }
                ps.copied = false;
                return None;
            }
            if ps.checked.contains(&ps.cursor) {
                ps.checked.remove(&ps.cursor);
            } else {
                ps.checked.insert(ps.cursor);
            }
        }
        PanelKind::View => ps.page(p, 1), // v1: Space pages a view forward
        _ => {}
    }
    ps.copied = false;
    None
}

/// The text keys (model.go:789-876): `/` search, `c` copy/clear, `q` cancel, `b`
/// page-back, `j`/`k` move, `h`/`l` slider/switch/pan/page, `g`/`G` first/last. Any
/// other HANDLED key clears the copy confirmation.
fn char_key(st: &mut SurfaceState, c: char) -> Option<TabbedResult> {
    let (p, ps) = st.focused();
    let mut handled = true;
    match c {
        '/' => {
            if ps.search_available(p) {
                ps.search_open(p);
                return None;
            }
            handled = false;
        }
        'c' => {
            if p.kind() == PanelKind::View {
                ps.copied = copy_to_clipboard(&strip_sgr(&ps.items.join("\n"))).is_ok();
                return None; // keep the ✓ hint until another key
            }
            // On a row panel "c" was free, and an applied filter claims it: the
            // panel's own keys stay untouched, so this is the way back to the list.
            if ps.search.mode == SearchMode::Applied {
                ps.search_clear(p);
                return None;
            }
        }
        'b' => {
            if p.kind() == PanelKind::View {
                ps.page(p, -1);
            }
        }
        'j' => ps.nav(p, 1),
        'k' => ps.nav(p, -1),
        'h' => match p.kind() {
            PanelKind::Slider => slider_step(ps, p, -1),
            PanelKind::Switch => ps.on = false,
            PanelKind::View if !p.wrap() => ps.pan_offset = ps.pan_offset.saturating_sub(1),
            _ => ps.page(p, -1),
        },
        'l' => match p.kind() {
            PanelKind::Slider => slider_step(ps, p, 1),
            PanelKind::Switch => ps.on = true,
            PanelKind::View if !p.wrap() => ps.pan_offset += 1,
            _ => ps.page(p, 1),
        },
        'g' => {
            if p.kind() == PanelKind::Slider {
                ps.value = None;
            } else {
                ps.set_view_pos(0);
                ps.offset = 0;
                ps.pan_offset = 0;
            }
        }
        'G' => match p.kind() {
            PanelKind::Slider => ps.value = Some(p.as_slider().map_or(0.0, |s| s.max)),
            PanelKind::View => ps.offset = 1 << 30, // render clamps to the last page
            _ => {
                let last = isize::try_from(ps.view.len().saturating_sub(1)).unwrap_or(0);
                ps.set_view_pos(last);
            }
        },
        _ => handled = false,
    }
    if handled {
        ps.copied = false; // any other handled key clears the copy confirmation
    }
    None
}

/// The Enter commit path (model.go:711-734): Browser descends on a directory (no
/// commit) / records the chosen file; an empty "Other…" opens the editor instead;
/// then `enter_advances` moves to the next tab; else commit-all.
///
/// A combo's `use "…" as typed` row needs no arm of its own: it commits like any row, and the
/// result says which one the cursor was on (`PanelResult`).
fn enter_commit(st: &mut SurfaceState) -> Option<TabbedResult> {
    let focus = st.focus;
    let enter_advances = st.enter_advances;
    let n = st.slots.len();
    let (p, ps) = st.focused();
    if p.kind() == PanelKind::Browser {
        let entry = ps.entries.get(ps.cursor).cloned();
        if let Some(e) = entry {
            if e.is_dir {
                ps.set_dir(p, &e.path); // descend; do not submit
                return None;
            }
            ps.chosen = e.path.to_string_lossy().into_owned(); // fall through
        }
    }
    if matches!(p.kind(), PanelKind::List | PanelKind::Multi)
        && p.custom()
        && ps.cursor == p.items().len()
        && ps.input.value().trim().is_empty()
    {
        // Empty Other: Enter opens the editor. With text already entered the row
        // behaves like any option (fall through to advance / commit) — Space edits it.
        ps.editing = true;
        return None;
    }
    if enter_advances && focus + 1 < n {
        st.set_focus(focus + 1);
        return None;
    }
    Some(st.result())
}

/// What one render produced: the rows for the bottom-zone slot and, when a field owns the
/// real cursor, its target in surface-block coordinates (column, row).
#[derive(Default)]
pub(crate) struct Rendered {
    /// The surface block, one string per row.
    pub(crate) rows: Vec<String>,
    /// The real-cursor target; `None` while no field owns the cursor.
    pub(crate) cursor: Option<(u16, u16)>,
}

/// Renders the surface (tabbed.go renderSurface). The scroll clamps, row budgets and the
/// picker's preview cache it settles are written back into `st`.
impl SurfaceState {
    /// Renders the surface into rows for a `width`-column terminal.
    pub(crate) fn render(&mut self, width: u16) -> Rendered {
        panels::render_rows(self, width)
    }
}

/// One live-refresh pass (tabbed.go:318-341 surfTickMsg, generation-guarded by the
/// LOOP before it calls here): every panel with a `refresh` closure re-reads its
/// rows, the cursor clamps, the view re-filters against the new content, and a View
/// with an applied query re-collects its hits ANCHORED ON THE HIT — content growing
/// above shifts indices but not the match being read.
impl SurfaceState {
    /// One live-refresh pass over every panel with a `refresh` closure.
    pub(crate) fn tick(&mut self) {
        for slot in &mut self.slots {
            let (p, ps) = (&mut slot.spec, &mut slot.state);
            let items = match p.refresh.as_mut() {
                Some(refresh) => refresh(),
                None => continue,
            };
            ps.items = items;
            if ps.cursor >= ps.items.len() {
                ps.cursor = ps.items.len().saturating_sub(1);
            }
            // Re-filter against the new content: rows grown under an applied query must
            // be judged by it too.
            ps.rebuild_view(p);
            ps.sync_cursor();
            if p.kind() == PanelKind::View && !ps.search.query.is_empty() {
                let q = ps.search.query.clone();
                ps.recollect_hits(&q);
            }
        }
    }
}

/// Routes a bracketed paste into the focused `Input` field or open Custom editor,
/// flattened to one line and trimmed (model.go:234-241; one-line fields).
///
/// Wired by [`crate::ui::oneshot::run_surface`]; the in-REPL loop's `Event::Paste` arm
/// still drops surface pastes (`NEEDS: [WP44] event_loop.rs` in DEVIATIONS3 carries the
/// one-line call).
impl SurfaceState {
    /// Routes a bracketed paste into the focused field.
    pub(crate) fn paste(&mut self, data: &str) {
        let Some(slot) = self.slots.get_mut(self.focus) else {
            return;
        };
        let (p, ps) = (&slot.spec, &mut slot.state);
        if p.kind() != PanelKind::Input && !ps.editing {
            return;
        }
        let flat = data.replace("\r\n", " ").replace(['\r', '\n'], " ");
        ps.input.insert_str(flat.trim());
    }
}

/// Pipes text into the platform clipboard tool (clipboard.go:14-39, the v1 promptui
/// implementation ported): `pbcopy` on macOS; `clip` on Windows; `wl-copy` /
/// `xclip -selection clipboard` / `xsel --clipboard --input` elsewhere. A tool that
/// is absent (spawn fails with `NotFound`) falls through to the next candidate; none
/// found is the error `"no clipboard tool found"`. OSC 52 stays unshipped (T-21).
pub(crate) fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let candidates: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else if cfg!(target_os = "windows") {
        &[&["clip"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    for c in candidates {
        let mut child = match Command::new(c[0])
            .args(&c[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            return Ok(());
        }
        return Err(io::Error::other(format!("{} failed: {status}", c[0])));
    }
    Err(io::Error::other("no clipboard tool found"))
}

#[cfg(test)]
mod tests;
