#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The mouse's hit-testing, its click counting and its routing into the model.

use std::time::{Duration, Instant};

use super::*;

fn map(composer: usize, rows: usize, surface: Option<usize>) -> FrameMap {
    FrameMap {
        composer,
        composer_rows: rows,
        surface,
    }
}

/// Above the frame is the conversation (its last row the search line or the way back, when one
/// is shown); in the frame, the composer's and the surface's rows by their own index.
#[test]
fn a_row_is_the_viewport_the_composer_or_the_surface() {
    // A 20-row viewport over a frame: spacer, separator, two composer rows, separator, surface.
    let m = map(2, 2, Some(5));
    assert_eq!(target(0, 20, &m, false, false), Target::View);
    assert_eq!(target(19, 20, &m, false, false), Target::View);
    assert_eq!(target(19, 20, &m, true, false), Target::SearchLine);
    assert_eq!(target(19, 20, &m, false, true), Target::WayBack);
    assert_eq!(target(18, 20, &m, true, true), Target::View);
    assert_eq!(
        target(20, 20, &m, false, false),
        Target::Frame,
        "the spacer"
    );
    assert_eq!(target(22, 20, &m, false, false), Target::Composer(0));
    assert_eq!(target(23, 20, &m, false, false), Target::Composer(1));
    assert_eq!(
        target(24, 20, &m, false, false),
        Target::Frame,
        "the separator"
    );
    assert_eq!(target(25, 20, &m, false, false), Target::Surface(0));
    assert_eq!(target(31, 20, &m, false, false), Target::Surface(6));
    let none = map(2, 1, None);
    assert_eq!(target(25, 20, &none, false, false), Target::Frame);
}

/// The top row and anything from the last viewport row down is the edge a held drag scrolls at.
#[test]
fn the_edge_is_the_top_row_and_the_last_one_down() {
    assert!(at_edge(0, 20));
    assert!(!at_edge(1, 20));
    assert!(!at_edge(18, 20));
    assert!(at_edge(19, 20));
    assert!(at_edge(25, 20));
}

/// Presses on one cell within the window count up to three and start over; another cell or a
/// late press is a single click.
#[test]
fn presses_on_one_cell_count_to_a_triple() {
    let mut s = MouseState::default();
    let t = Instant::now();
    assert_eq!(s.press(3, 4, t), 1);
    assert_eq!(s.press(3, 4, t + Duration::from_millis(200)), 2);
    assert_eq!(s.press(3, 4, t + Duration::from_millis(400)), 3);
    assert_eq!(s.press(3, 4, t + Duration::from_millis(600)), 1);
    assert_eq!(
        s.press(4, 4, t + Duration::from_millis(700)),
        1,
        "another cell"
    );
    assert_eq!(
        s.press(4, 4, t + Duration::from_millis(700) + MULTI_CLICK),
        1,
        "too late"
    );
}

// --- routed through the model ---------------------------------------------------------------------

use crossterm::event::{Event, KeyModifiers};

use crate::ui::facade::{Panel, TabbedSpec};
use crate::ui::runtime::msgs::UiMsg;
use crate::ui::testutil::{key, test_model, type_text};
use crossterm::event::KeyCode;

/// A report at (`col`, `row`).
fn ev(kind: MouseEventKind, col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// The model after a frame, as the loop leaves it: the frame laid out, the viewport above it.
fn laid_out(m: &mut Model) {
    let view = m.frame_view();
    m.view_h = 24 - u16::try_from(view.rows.len()).unwrap();
}

/// A click on the composer puts the cursor under the pointer — on a wide glyph, the glyph.
#[test]
fn a_click_on_the_composer_places_its_cursor() {
    let mut m = test_model();
    type_text(&mut m, "hello 中文 world");
    laid_out(&mut m);
    let row = m.view_h + u16::try_from(m.frame_map.composer).unwrap();
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 2 + 3, row));
    assert_eq!(m.composer.cursor_pos(80), (5, 0), "before the second `l`");
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 2 + 9, row));
    assert_eq!(
        m.composer.cursor_pos(80),
        (10, 0),
        "on `文`, its first column"
    );
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 70, row));
    assert_eq!(
        m.composer.cursor_pos(80),
        (2 + 16, 0),
        "past the end: the end"
    );
    assert!(m.view_ops.is_empty(), "nothing for the conversation");
}

/// On a surface, the first click gives a row the cursor and the second chooses it; the wheel
/// moves the list; the conversation is untouched.
#[test]
fn a_surface_row_takes_the_cursor_then_a_second_click_chooses_it() {
    let mut m = test_model();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TabbedOpen {
        spec: TabbedSpec {
            panels: vec![Panel::list(
                "Pick",
                vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
            )],
            ..TabbedSpec::default()
        },
        reply: tx,
    });
    laid_out(&mut m);
    let top = m.view_h + u16::try_from(m.frame_map.surface.unwrap()).unwrap();
    // Row 0 is the title chip; the items follow.
    let gamma = top + 3;
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 4, gamma));
    assert!(m.surface.is_some(), "one click only moves the cursor");
    assert_eq!(m.surface.as_ref().unwrap().st.slots[0].state.cursor, 2);
    m.handle_event(ev(MouseEventKind::ScrollUp, 4, gamma));
    assert_eq!(
        m.surface.as_ref().unwrap().st.slots[0].state.cursor,
        1,
        "the wheel"
    );
    laid_out(&mut m);
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 4, top + 2));
    let result = rx.try_recv().expect("the second click chose beta");
    assert!(!result.cancelled);
    assert_eq!(result.panels[0].cursor, 1);
    assert!(m.view_ops.is_empty(), "nothing for the conversation");
}

/// The wheel over the conversation or the composer scrolls the conversation; a press, drag and
/// release on it are the selection's; a key afterwards drops the selection.
#[test]
fn the_conversation_takes_the_wheel_and_the_selection() {
    let mut m = test_model();
    laid_out(&mut m);
    let composer = m.view_h + u16::try_from(m.frame_map.composer).unwrap();
    m.handle_event(ev(MouseEventKind::ScrollUp, 0, 3));
    m.handle_event(ev(MouseEventKind::ScrollDown, 0, composer));
    assert_eq!(
        std::mem::take(&mut m.view_ops),
        vec![ViewOp::Wheel(-WHEEL_LINES), ViewOp::Wheel(WHEEL_LINES)]
    );
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 1, 5));
    m.handle_event(ev(MouseEventKind::Drag(MouseButton::Left), 4, 6));
    m.handle_event(ev(MouseEventKind::Up(MouseButton::Left), 4, 6));
    assert_eq!(
        std::mem::take(&mut m.view_ops),
        vec![
            ViewOp::Select(SelectOp::Press {
                col: 1,
                row: 5,
                clicks: 1
            }),
            ViewOp::Select(SelectOp::Drag { col: 4, row: 6 }),
            ViewOp::Select(SelectOp::Release),
        ]
    );
    m.handle_key(key(KeyCode::Char('x')));
    assert_eq!(m.view_ops, vec![ViewOp::Select(SelectOp::Clear)]);
    assert_eq!(m.composer.value(), "x", "and the key is typed");
}

/// A drag held at the edge steps every `EDGE_STEP` without the pointer moving; released, it stops.
#[test]
fn a_drag_held_at_the_edge_keeps_scrolling() {
    let mut m = test_model();
    laid_out(&mut m);
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 1, 5));
    m.handle_event(ev(MouseEventKind::Drag(MouseButton::Left), 1, 0));
    m.view_ops.clear();
    let t = Instant::now();
    tick_edge(&mut m, t);
    assert!(m.view_ops.is_empty(), "not yet due");
    tick_edge(&mut m, t + EDGE_STEP);
    tick_edge(&mut m, t + EDGE_STEP * 2);
    assert_eq!(
        m.view_ops,
        vec![ViewOp::Select(SelectOp::Drag { col: 1, row: 0 }); 2]
    );
    assert!(edge_due(&m, t + EDGE_STEP * 2).is_some());
    m.handle_event(ev(MouseEventKind::Up(MouseButton::Left), 1, 0));
    assert_eq!(
        edge_due(&m, t + EDGE_STEP * 3),
        None,
        "released: no more steps"
    );
}

/// The search line, clicked while an applied query is walked, takes the keys again; the way-back
/// row returns to the tail.
#[test]
fn the_search_line_and_the_way_back_row_answer_a_click() {
    let mut m = test_model();
    laid_out(&mut m);
    m.handle_key(crate::ui::testutil::ctrl('r'));
    type_text(&mut m, "needle");
    m.handle_key(key(KeyCode::Enter));
    m.view_ops.clear();
    let last = m.view_h - 1;
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 3, last));
    type_text(&mut m, "s");
    assert_eq!(
        m.view_ops,
        vec![ViewOp::Search(SearchOp::Query("needles".to_owned()))],
        "typing again edits the kept query"
    );
    m.search = None;
    m.view_ops.clear();
    m.view_paused = true;
    m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 3, last));
    assert_eq!(m.view_ops, vec![ViewOp::Bottom]);
}

/// A release that is never reported (the button came up outside the window) must not leave a
/// drag scrolling at the edge for good: a key drops it (and the selection), losing focus and a
/// resize end it as a release would.
#[test]
fn a_lost_release_is_ended_by_a_key_focus_loss_or_a_resize() {
    let t = Instant::now();
    let held_at_top = |m: &mut Model| {
        laid_out(m);
        m.handle_event(ev(MouseEventKind::Down(MouseButton::Left), 1, 5));
        m.handle_event(ev(MouseEventKind::Drag(MouseButton::Left), 1, 0));
        m.view_ops.clear();
        assert!(edge_due(m, t).is_some(), "scrolling at the edge");
    };
    let mut m = test_model();
    held_at_top(&mut m);
    m.handle_key(key(KeyCode::Esc));
    assert_eq!(
        edge_due(&m, t + EDGE_STEP * 10),
        None,
        "a key ends the drag"
    );
    tick_edge(&mut m, t + EDGE_STEP * 10);
    assert_eq!(
        m.view_ops,
        vec![ViewOp::Select(SelectOp::Clear)],
        "and drops it, no step after"
    );

    let mut m = test_model();
    held_at_top(&mut m);
    m.handle_event(Event::FocusLost);
    assert_eq!(
        edge_due(&m, t + EDGE_STEP * 10),
        None,
        "losing focus ends it"
    );
    assert_eq!(m.view_ops, vec![ViewOp::Select(SelectOp::Release)]);

    let mut m = test_model();
    held_at_top(&mut m);
    m.handle_event(Event::Resize(100, 30));
    assert_eq!(edge_due(&m, t + EDGE_STEP * 10), None, "a resize ends it");
    assert_eq!(m.view_ops, vec![ViewOp::Select(SelectOp::Release)]);
}
