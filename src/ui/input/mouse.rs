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
//! | the composer | the conversation | the cursor goes under the pointer |
//!
//! A drag held at the viewport's top row or below its last one scrolls a line every
//! [`EDGE_STEP`], so a selection crosses screens without the pointer moving. Where a terminal
//! delivers no reports (iTerm2 with reporting off in its profile, tmux without `mouse on`), nothing
//! here runs and the keyboard does everything it did before.

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::ui::render::frame::FrameMap;
use crate::ui::runtime::event_loop::Model;
use crate::ui::runtime::screen::search::SearchOp;
use crate::ui::runtime::screen::{SelectOp, ViewOp};

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
    /// The pointer while the left button is held down on the conversation.
    pub(crate) held: Option<(u16, u16)>,
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

/// What screen row `row` is, for a frame laid out under a `view_h`-row viewport.
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
    /// Any other frame row.
    Frame,
}

/// Hit-tests a screen row (`search`: a search line is shown; `paused`: the view is scrolled
/// back, its last row the way back).
pub(crate) fn target(row: u16, view_h: u16, map: &FrameMap, search: bool, paused: bool) -> Target {
    if row < view_h {
        return match (row + 1 == view_h, search, paused) {
            (true, true, _) => Target::SearchLine,
            (true, false, true) => Target::WayBack,
            _ => Target::View,
        };
    }
    let fr = usize::from(row - view_h);
    if let Some(top) = map.surface.filter(|&top| fr >= top) {
        return Target::Surface(fr - top);
    }
    if (map.composer..map.composer + map.composer_rows).contains(&fr) {
        return Target::Composer(fr - map.composer);
    }
    Target::Frame
}

/// Whether a drag at screen row `row` is at the viewport's edge (its top row, its last row, or
/// below it), where it scrolls.
pub(crate) fn at_edge(row: u16, view_h: u16) -> bool {
    row == 0 || row + 1 >= view_h
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
        m.view_h,
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
            press(m, at, col);
            if at == Target::View {
                m.mouse.held = Some((col, row));
                m.mouse.last_step = None;
                m.mouse.selected = true;
                m.view_ops
                    .push(ViewOp::Select(SelectOp::Press { col, row, clicks }));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if m.mouse.held.is_some() {
                m.mouse.held = Some((col, row));
                if !at_edge(row, m.view_h) || m.mouse.last_step.is_none() {
                    m.mouse.last_step = at_edge(row, m.view_h).then(Instant::now);
                    m.view_ops.push(ViewOp::Select(SelectOp::Drag { col, row }));
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) if m.mouse.held.is_some() => {
            m.mouse.held = None;
            m.mouse.last_step = None;
            m.view_ops.push(ViewOp::Select(SelectOp::Release));
        }
        _ => {}
    }
}

/// A left press anywhere but the conversation.
fn press(m: &mut Model, at: Target, col: u16) {
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
        Target::Composer(row) => {
            // The surface owns the input while it is open; the composer is only shown.
            if m.surface.is_some() {
                return;
            }
            if m.search.take().is_some() {
                m.view_ops.push(ViewOp::Search(SearchOp::Accept));
            }
            m.composer.click(col, row, m.width.max(1));
        }
    }
}

/// The loop's edge step: a drag held at the viewport's edge scrolls another line when
/// [`EDGE_STEP`] has passed since the last.
pub(crate) fn tick_edge(m: &mut Model, now: Instant) {
    let Some((col, row)) = m.mouse.held else {
        return;
    };
    if !at_edge(row, m.view_h) {
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

/// When the edge step is next due, while a drag is held at the edge.
pub(crate) fn edge_due(m: &Model, now: Instant) -> Option<Duration> {
    let (_, row) = m.mouse.held?;
    at_edge(row, m.view_h).then(|| {
        m.mouse.last_step.map_or(Duration::ZERO, |at| {
            EDGE_STEP.saturating_sub(now.saturating_duration_since(at))
        })
    })
}

#[cfg(test)]
mod tests;
