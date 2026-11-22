//! The mouse (tui-plan S6; `docs/design/fullscreen-tui.md` "The mouse"): where a report lands
//! and what it asks for. Reports exist only while the config's `mouse:` is on (the default) and
//! the alternate screen is shown — off, iota requests none and the terminal keeps its own wheel and
//! selection.
//!
//! | where | wheel | left press / drag / release |
//! |---|---|---|
//! | the conversation viewport | three lines (up leaves the tail; the bottom follows it again) | a selection: drag, double-click a word, triple-click a row; release copies |
//! | the viewport's last row while scrolled back (`↓ … Ctrl+End`) | — | back to the tail |
//! | the search line | — | the field takes the keys again |
//! | a surface | its list (a View: three lines) | a row gets the cursor; the row it is on is chosen; a tab chip focuses it; the hint row focuses the search field |
//! | the composer | the conversation | the cursor goes under the pointer; a drag selects the draft, double-click all of it, triple-click the logical line; release copies |
//! | the completion list | the conversation | the item under the pointer (its label or description rows) is highlighted |
//!
//! A drag held at the viewport's top row or below its last one scrolls every [`EDGE_STEP`] (one
//! line at the top; below the viewport 1, 3, 5 … lines with the distance), so a selection crosses
//! screens without the pointer moving. Only a drag does: a press that has not moved yet is a click,
//! and the view stays put under it however long it is held — for hundreds of screens, `Ctrl+O` and the terminal's own
//! selection are the faster way. A key, the window losing focus or a resize ends a drag whose
//! release never came. Where a terminal delivers no reports (iTerm2 with reporting off in its
//! profile, `tmux -CC`), nothing here runs and the keyboard does everything it did before; a tmux
//! passes them on with its `mouse` option on or off (tmux 3.8, L4 47).
//!
//! A drag stays where it started: one pressed on the composer selects the draft (a pointer above
//! it is the first visible row's start, below it the last one's end; the composer never scrolls the
//! conversation), one pressed on the conversation selects the conversation. Pressing on either
//! drops the other's selection, so one selection is on the screen at a time.

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::suggest;
use crate::ui::render::frame::FrameMap;
use crate::ui::runtime::event_loop::Model;
use crate::ui::runtime::screen::search::SearchOp;
use crate::ui::runtime::screen::{DRAG_MIN_COLS, SelectOp, ViewOp};

/// Lines one wheel step scrolls the conversation.
pub(crate) const WHEEL_LINES: i32 = 3;

/// Two presses on the same cell this close together are a double-click (three, a triple).
pub(crate) const MULTI_CLICK: Duration = Duration::from_millis(500);

/// How often a drag held at the viewport's edge scrolls one line.
pub(crate) const EDGE_STEP: Duration = Duration::from_millis(40);

/// How long the status row says what a selection copied.
pub(crate) const COPIED_SHOWN: Duration = Duration::from_secs(4);

/// What the mouse carries from one report to the next.
#[derive(Debug, Default)]
pub(crate) struct MouseState {
    /// The last press: its cell, when, and how many presses in a row it was.
    last_press: Option<(u16, u16, Instant, u8)>,
    /// The pointer while the left button is held down on the conversation or the composer.
    pub(crate) held: Option<(u16, u16)>,
    /// Whether the held button was pressed on the composer: its drag selects the draft.
    pub(crate) in_composer: bool,
    /// Whether the pointer has moved since the press (a Drag report came): only then does a
    /// button held at the viewport's edge scroll it.
    pub(crate) moved: bool,
    /// When a held drag last scrolled at the viewport's edge.
    pub(crate) last_step: Option<Instant>,
    /// Whether a selection may be on the screen (a key then drops it).
    pub(crate) selected: bool,
}

impl MouseState {
    /// Counts a press at (`col`, `row`): 1, or 2 and 3 for presses on the same cell within
    /// [`MULTI_CLICK`] of the one before (a fourth starts over).
    pub(crate) fn press(&mut self, col: u16, row: u16, now: Instant) -> u8 {
        let n = match self.last_press {
            Some((c, r, at, n))
                if c == col && r == row && now.saturating_duration_since(at) < MULTI_CLICK =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.last_press = Some((col, row, now, n));
        n
    }
}

/// What screen row `row` is, for a frame laid out under `vis_h` visible conversation rows (the
/// rows above the frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// The conversation.
    View,
    /// The viewport's last row while it is the search line.
    SearchLine,
    /// The viewport's last row while it is the way back to the tail.
    WayBack,
    /// A row of the composer (its visible row).
    Composer(usize),
    /// A row of the open surface (its row in the block).
    Surface(usize),
    /// A row of the completion list (its row in the list).
    List(usize),
    /// Any other frame row.
    Frame,
}

/// Hit-tests a screen row (`search`: a search line is shown; `paused`: the view is scrolled
/// back, its last row the way back).
pub(crate) fn target(row: u16, vis_h: u16, map: &FrameMap, search: bool, paused: bool) -> Target {
    if row < vis_h {
        return match (row + 1 == vis_h, search, paused) {
            (true, true, _) => Target::SearchLine,
            (true, false, true) => Target::WayBack,
            _ => Target::View,
        };
    }
    let fr = usize::from(row - vis_h) + map.cut;
    if let Some(top) = map.surface.filter(|&top| fr >= top) {
        return Target::Surface(fr - top);
    }
    if let Some(top) = map
        .list
        .filter(|&top| (top..top + map.list_rows).contains(&fr))
    {
        return Target::List(fr - top);
    }
    if (map.composer..map.composer + map.composer_rows).contains(&fr) {
        return Target::Composer(fr - map.composer);
    }
    Target::Frame
}

/// Whether a drag at screen row `row` is at the viewport's edge (its top row, its last row, or
/// below it), where it scrolls.
pub(crate) fn at_edge(row: u16, vis_h: u16) -> bool {
    row == 0 || row + 1 >= vis_h
}

/// Routes one mouse report (module docs for the table).
pub(crate) fn update_mouse(m: &mut Model, ev: MouseEvent) {
    // A write-out has the terminal's own mouse back (reporting is off there): a stray report is
    // not the key that comes back.
    if m.dumped {
        return;
    }
    let (col, row) = (ev.column, ev.row);
    let at = target(
        row,
        m.vis_h,
        &m.frame_map,
        m.search.is_some(),
        m.view_paused,
    );
    match ev.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let dir = if ev.kind == MouseEventKind::ScrollUp {
                -1
            } else {
                1
            };
            match (at, m.surface.as_mut()) {
                (Target::Surface(_), Some(s)) => s.st.wheel(dir),
                _ => m
                    .view_ops
                    .push(ViewOp::Wheel(i32::try_from(dir).unwrap_or(1) * WHEEL_LINES)),
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let clicks = m.mouse.press(col, row, Instant::now());
            press(m, at, col, row, clicks);
            if at == Target::View {
                m.composer.clear_selection();
                m.mouse.held = Some((col, row));
                m.mouse.in_composer = false;
                m.mouse.moved = false;
                m.mouse.last_step = None;
                m.mouse.selected = true;
                m.view_ops
                    .push(ViewOp::Select(SelectOp::Press { col, row, clicks }));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if m.mouse.in_composer {
                composer_drag(m, col, row);
            } else if m.mouse.held.is_some() {
                m.mouse.held = Some((col, row));
                m.mouse.moved = true;
                if !at_edge(row, m.vis_h) || m.mouse.last_step.is_none() {
                    m.mouse.last_step = at_edge(row, m.vis_h).then(Instant::now);
                    m.view_ops.push(ViewOp::Select(SelectOp::Drag { col, row }));
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) if m.mouse.held.is_some() => {
            end_drag(m, SelectOp::Release);
        }
        _ => {}
    }
}

/// A left press anywhere but the conversation.
fn press(m: &mut Model, at: Target, col: u16, y: u16, clicks: u8) {
    match at {
        Target::View | Target::Frame => {}
        Target::SearchLine => {
            if let Some(s) = m.search.as_mut() {
                s.edit();
            }
        }
        Target::WayBack => m.view_ops.push(ViewOp::Bottom),
        Target::Surface(row) => {
            // A double-click is two clicks: the first gives a row the cursor, the second chooses it.
            if let Some(s) = m.surface.as_mut() {
                let effect = s.st.click(row, usize::from(col));
                m.surface_effect(effect);
            }
        }
        Target::List(row) => {
            // A click anywhere in an item's rows — its label or its description — highlights it.
            if let Some(&item) = m.list_items.get(row) {
                suggest::choose(&mut m.composer, &m.commands, item);
            }
        }
        Target::Composer(row) => {
            // The surface owns the input while it is open; the composer is only shown.
            if m.surface.is_some() {
                return;
            }
            if m.search.take().is_some() {
                m.view_ops.push(ViewOp::Search(SearchOp::Accept));
            }
            m.composer.click(col, row, m.width.max(1));
            // The press starts the composer's selection — a double-click the whole draft, a
            // triple-click the logical line under the pointer — and the conversation's goes.
            match clicks {
                2 => m.composer.select_all(),
                3 => m.composer.select_line(),
                _ => m.composer.select_at_cursor(),
            }
            m.mouse.held = Some((col, y));
            m.mouse.in_composer = true;
            m.mouse.moved = false;
            m.mouse.last_step = None;
            if std::mem::take(&mut m.mouse.selected) {
                m.view_ops.push(ViewOp::Select(SelectOp::Clear));
            }
        }
    }
}

/// A drag pressed on the composer: once the pointer has left the press's cell by
/// [`DRAG_MIN_COLS`] or a row (a click's jitter selects nothing), the selection's focus follows it,
/// by [`Composer::click`](super::composer::Composer::click)'s rule on the composer's rows.
fn composer_drag(m: &mut Model, col: u16, row: u16) {
    let Some((pcol, prow, ..)) = m.mouse.last_press else {
        return;
    };
    if !m.mouse.moved && prow == row && usize::from(pcol.abs_diff(col)) < DRAG_MIN_COLS {
        return;
    }
    m.mouse.moved = true;
    m.mouse.held = Some((col, row));
    let rows = m.frame_map.composer_rows.max(1);
    let fr = usize::from(row.saturating_sub(m.vis_h)) + m.frame_map.cut;
    let (col, at) = if row < m.vis_h || fr < m.frame_map.composer {
        (0, 0)
    } else if fr - m.frame_map.composer >= rows {
        (u16::MAX, rows - 1)
    } else {
        (col, fr - m.frame_map.composer)
    };
    m.composer.select_to(col, at, m.width.max(1));
}

/// A composer drag's release: a selection is copied as a conversation release's is (the screen's
/// copy slot, the clipboard, the status row); a press that selected nothing was a click.
fn composer_release(m: &mut Model) {
    match m.composer.selected_text() {
        Some(text) => m.view_ops.push(ViewOp::Copy(text.to_owned())),
        None => m.composer.clear_selection(),
    }
}

/// Ends a drag whose button is still held — a key, the window losing focus, a resize: the release
/// may never be reported (the button came up outside the window), and a drag held at the edge would
/// scroll forever. `op` is what the selection gets (`Release` keeps and copies it, `Clear` drops
/// it). A drag on the composer is copied by a `Release`; a `Clear` (a key) keeps its selection
/// for the key to act on. Returns whether a drag was on.
pub(crate) fn end_drag(m: &mut Model, op: SelectOp) -> bool {
    if m.mouse.held.take().is_none() {
        return false;
    }
    m.mouse.moved = false;
    m.mouse.last_step = None;
    if std::mem::take(&mut m.mouse.in_composer) {
        if op == SelectOp::Release {
            composer_release(m);
        }
        return true;
    }
    if op == SelectOp::Clear {
        m.mouse.selected = false;
    }
    m.view_ops.push(ViewOp::Select(op));
    true
}

/// The loop's edge step: a drag held at the viewport's edge scrolls another line when
/// [`EDGE_STEP`] has passed since the last. A press that has not moved yet is no drag.
pub(crate) fn tick_edge(m: &mut Model, now: Instant) {
    let Some((col, row)) = m.mouse.held else {
        return;
    };
    if m.mouse.in_composer || !m.mouse.moved || !at_edge(row, m.vis_h) {
        return;
    }
    if m.mouse
        .last_step
        .is_some_and(|at| now.saturating_duration_since(at) < EDGE_STEP)
    {
        return;
    }
    m.mouse.last_step = Some(now);
    m.view_ops.push(ViewOp::Select(SelectOp::Drag { col, row }));
    m.dirty = true;
}

/// When the edge step is next due, while a drag (not a press that has not moved) is held at the
/// edge.
pub(crate) fn edge_due(m: &Model, now: Instant) -> Option<Duration> {
    let (_, row) = m.mouse.held?;
    (!m.mouse.in_composer && m.mouse.moved && at_edge(row, m.vis_h)).then(|| {
        m.mouse.last_step.map_or(Duration::ZERO, |at| {
            EDGE_STEP.saturating_sub(now.saturating_duration_since(at))
        })
    })
}

#[cfg(test)]
mod tests;
