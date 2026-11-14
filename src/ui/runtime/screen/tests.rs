#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The full-screen terminal over a vt100 replay: the frame pinned to the bottom, the
//! conversation on it, a resize as one full repaint, half-screen scrolling, and the exit that
//! hands the conversation to the main screen.

use ratatui::layout::Size;

use super::{SYNC_BEGIN, Screen};
use crate::ui::render::frame::FrameView;
use crate::ui::testutil::SharedBuf;

fn screen(w: u16, h: u16) -> (Screen<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    (Screen::new(buf.clone(), Size::new(w, h)).unwrap(), buf)
}

fn frame(w: usize) -> FrameView {
    FrameView {
        rows: vec![
            "┄".repeat(w),
            "❯ ".to_owned(),
            "┄".repeat(w),
            "status".to_owned(),
        ],
        cursor: Some((2, 1)),
    }
}

fn rows(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("line-{i:03}")).collect()
}

fn replay(buf: &SharedBuf, h: u16, w: u16) -> vt100::Parser {
    let mut p = vt100::Parser::new(h, w, 1000);
    p.process(&buf.bytes());
    p
}

fn grid(p: &vt100::Parser) -> Vec<String> {
    let cols = p.screen().size().1;
    p.screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

#[test]
fn the_conversation_sits_on_the_frame_and_the_frame_on_the_bottom() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(3));
    s.draw_frame(&frame(40)).unwrap();
    let p = replay(&buf, 12, 40);
    assert!(p.screen().alternate_screen(), "on the alternate screen");
    let g = grid(&p);
    assert_eq!(g[11], "status", "{g:#?}");
    assert_eq!(g[8], "┄".repeat(40));
    assert_eq!(&g[5..8], &["line-000", "line-001", "line-002"], "{g:#?}");
    assert!(g[..5].iter().all(String::is_empty), "{g:#?}");
    assert_eq!(
        p.screen().cursor_position(),
        (9, 2),
        "the composer's cursor"
    );
}

#[test]
fn a_resize_repaints_the_whole_screen_at_the_new_size() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(20));
    s.push(&[format!("{}END", "w".repeat(30))]);
    s.draw_frame(&frame(40)).unwrap();
    let mark = buf.bytes().len();
    s.resize(Size::new(20, 10));
    s.draw_frame(&frame(20)).unwrap();
    // The emulator itself resized before the repaint arrived.
    let mut p = vt100::Parser::new(12, 40, 0);
    p.process(&buf.bytes()[..mark]);
    p.screen_mut().set_size(10, 20);
    p.process(&buf.bytes()[mark..]);
    let g = grid(&p);
    assert_eq!(g[9], "status");
    assert_eq!(g[6], "┄".repeat(20), "the frame is full width: {g:#?}");
    assert_eq!(g[5], "wwwwwwwwwwEND", "the wide row wraps at the new width");
    assert_eq!(g[4], "w".repeat(20));
    assert_eq!(g[3], "line-019");
    let names: Vec<&String> = g.iter().filter(|r| r.starts_with("line-")).collect();
    let mut uniq = names.clone();
    uniq.dedup();
    assert_eq!(names.len(), uniq.len(), "no duplicate: {g:#?}");
}

#[test]
fn page_keys_walk_back_to_the_first_row_and_down_to_the_tail() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(50));
    s.draw_frame(&frame(40)).unwrap();
    for _ in 0..40 {
        s.scroll_half(true);
    }
    s.draw_frame(&frame(40)).unwrap();
    let g = grid(&replay(&buf, 12, 40));
    assert_eq!(g[0], "line-000", "the top of the session: {g:#?}");
    assert_eq!(g[11], "status", "the frame stays");
    // Clamped at the top: one PgDn moves at once.
    s.scroll_half(false);
    s.draw_frame(&frame(40)).unwrap();
    assert_eq!(grid(&replay(&buf, 12, 40))[0], "line-004");
    for _ in 0..40 {
        s.scroll_half(false);
    }
    s.draw_frame(&frame(40)).unwrap();
    assert_eq!(
        grid(&replay(&buf, 12, 40))[7],
        "line-049",
        "back on the tail"
    );
}

#[test]
fn the_exit_writes_the_conversation_into_the_main_screen_at_the_exit_width() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(5));
    s.push(&[format!("{}END", "w".repeat(30))]);
    s.draw_frame(&frame(40)).unwrap();
    s.resize(Size::new(20, 12));
    s.leave().unwrap();
    let mut p = vt100::Parser::new(12, 40, 1000);
    p.process(b"$ iota\r\n");
    p.process(&buf.bytes());
    assert!(!p.screen().alternate_screen(), "the main screen is back");
    let g = grid(&p);
    assert_eq!(g[0], "$ iota", "{g:#?}");
    assert_eq!(&g[1..6], &rows(5)[..]);
    assert_eq!(g[6], "w".repeat(20), "wrapped at the exit width");
    assert_eq!(g[7], "wwwwwwwwwwEND");
    assert!(
        !g.iter().any(|r| r.contains("status")),
        "no frame row: {g:#?}"
    );
    assert_eq!(
        p.screen().cursor_position(),
        (8, 0),
        "a fresh line for the shell"
    );
}

#[test]
fn a_frame_larger_than_sync_min_is_one_synchronized_update_and_a_keystroke_is_bare() {
    let (mut s, buf) = screen(80, 24);
    s.push(
        &(0..30)
            .map(|i| format!("{i:02} {}", "x".repeat(70)))
            .collect::<Vec<_>>(),
    );
    let mark = buf.bytes().len();
    s.begin_batch();
    s.draw_frame(&frame(80)).unwrap();
    s.end_batch().unwrap();
    let first = buf.bytes()[mark..].to_vec();
    assert!(first.starts_with(SYNC_BEGIN), "a full paint is one block");
    let mark = buf.bytes().len();
    let mut typed = frame(80);
    typed.rows[1] = "❯ x".to_owned();
    typed.cursor = Some((3, 1));
    s.begin_batch();
    s.draw_frame(&typed).unwrap();
    s.end_batch().unwrap();
    let key = &buf.bytes()[mark..];
    assert!(!key.is_empty() && key.len() < 64, "{} bytes", key.len());
    assert!(!key.starts_with(SYNC_BEGIN), "a keystroke goes out bare");
}
