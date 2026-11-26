#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The tail slot on the real screen: its growth is one scroll and a net shrink none, an append
//! above it is one scroll, the selection reaches it and follows its number, a clear drops a
//! selection on it, and neither the dump nor the exit writes it.

use ratatui::layout::Size;

use super::search::Search;
use super::{Screen, SelectOp, ViewOp};
use crate::ui::block::{Block, Layout};
use crate::ui::render::frame::{FrameMap, FrameView};
use crate::ui::testutil::SharedBuf;

/// A tail block of `n` lines `tail-0`, `tail-1`, ….
struct Lines(usize);

impl Layout for Lines {
    fn lines(&self, _width: usize) -> Vec<String> {
        (0..self.0).map(|i| format!("tail-{i}")).collect()
    }
}

fn tail(n: usize) -> Block {
    Block::laid(Lines(n))
}

fn rows(from: usize, n: usize) -> Vec<String> {
    (from..from + n).map(|i| format!("line-{i:03}")).collect()
}

/// A 40×12 screen with the mouse on.
fn screen() -> (Screen<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    (
        Screen::new(buf.clone(), Size::new(40, 12), false, true).unwrap(),
        buf,
    )
}

/// The 4-row frame: the visible conversation is rows 0–7.
fn frame() -> FrameView {
    FrameView {
        lit: Vec::new(),
        float: 0,
        rows: vec![
            "─".repeat(40),
            String::new(),
            "─".repeat(40),
            "status".to_owned(),
        ],
        cursor: Some((0, 1)),
        map: FrameMap::default(),
    }
}

fn grid(buf: &SharedBuf) -> Vec<String> {
    let mut p = vt100::Parser::new(12, 40, 0);
    p.process(&buf.bytes());
    p.screen()
        .rows(0, 40)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

/// What one draw wrote.
fn draw(s: &mut Screen<SharedBuf>, buf: &SharedBuf) -> String {
    let mark = buf.bytes().len();
    s.draw_frame(&frame(), None).unwrap();
    String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned()
}

/// The screen a fresh paint of `committed` and a tail of `n` lines (none for 0) draws.
fn fresh(committed: &[String], n: usize) -> Vec<String> {
    let (mut s, buf) = screen();
    s.push(Block::rows(committed.iter().cloned()));
    s.set_tail((n > 0).then(|| tail(n)));
    s.draw_frame(&frame(), None).unwrap();
    grid(&buf)
}

fn select(s: &mut Screen<SharedBuf>, op: SelectOp) {
    s.view(ViewOp::Select(op)).unwrap();
}

#[test]
fn the_tail_block_growing_is_one_scroll_and_a_net_shrink_none() {
    let (mut s, buf) = screen();
    s.push(Block::rows(rows(0, 20)));
    s.set_tail(Some(tail(2)));
    s.draw_frame(&frame(), None).unwrap();
    s.set_tail(Some(tail(3)));
    let out = draw(&mut s, &buf);
    assert_eq!(out.matches("S\u{1b}[r").count(), 1, "{out:?}");
    assert!(out.contains("\u{1b}[1;8r\u{1b}[1S"), "{out:?}");
    assert_eq!(grid(&buf), fresh(&rows(0, 20), 3));
    // The same height (the spinner's next frame): no scroll.
    s.set_tail(Some(tail(3)));
    let out = draw(&mut s, &buf);
    assert!(!out.contains("\u{1b}[r"), "{out:?}");
    // The settle: one summary row in, three tail rows out — no scroll, the right screen.
    s.set_tail(None);
    s.push(Block::rows(["summary"]));
    let out = draw(&mut s, &buf);
    assert!(!out.contains("\u{1b}[r"), "{out:?}");
    let mut all = rows(0, 20);
    all.push("summary".to_owned());
    assert_eq!(grid(&buf), fresh(&all, 0));
}

#[test]
fn an_append_above_the_tail_block_is_one_scroll_and_lands_above_it() {
    let (mut s, buf) = screen();
    s.push(Block::rows(rows(0, 20)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    s.push(Block::rows(rows(20, 2)));
    let out = draw(&mut s, &buf);
    assert!(out.contains("\u{1b}[1;8r\u{1b}[2S"), "{out:?}");
    let g = grid(&buf);
    assert_eq!(
        &g[3..8],
        &["line-020", "line-021", "tail-0", "tail-1", "tail-2"]
    );
    assert_eq!(g, fresh(&rows(0, 22), 3));
}

#[test]
fn scrolled_back_the_tail_block_scrolls_away_and_the_way_back_says_activity_but_counts_commits_only()
 {
    let (mut s, buf) = screen();
    s.push(Block::rows(rows(0, 30)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    s.view(ViewOp::HalfUp).unwrap();
    s.draw_frame(&frame(), None).unwrap();
    let before = grid(&buf);
    assert!(
        before.iter().all(|r| !r.starts_with("tail-")),
        "{before:#?}"
    );
    s.set_tail(Some(tail(4)));
    s.draw_frame(&frame(), None).unwrap();
    assert_eq!(
        grid(&buf),
        before,
        "its growth alone moved and said nothing"
    );
    s.set_tail(Some(tail(5)));
    s.note_activity();
    s.draw_frame(&frame(), None).unwrap();
    let g = grid(&buf);
    assert_eq!(g[..7], before[..7], "its growth moved nothing");
    assert!(
        g[7].contains("↓ new activity · Ctrl+End"),
        "no new line counted, the activity said: {g:#?}"
    );
    s.push(Block::rows(["summary"]));
    s.draw_frame(&frame(), None).unwrap();
    assert!(grid(&buf)[7].contains("1 new line"));
}

#[test]
fn a_drag_over_the_tail_block_copies_its_lines() {
    let (mut s, _buf) = screen();
    s.push(Block::rows(rows(0, 20)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    // Rows 5–7 are the tail.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 4,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 39, row: 6 });
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied().as_deref(), Some("line-019\ntail-0\ntail-1"));
}

/// An append above the tail moves its number on; a selection point on it moves with it, so the
/// release still copies the tail's lines, not the block that took its old number.
#[test]
fn a_selection_point_on_the_tail_block_follows_an_append_above_it() {
    let (mut s, _buf) = screen();
    s.push(Block::rows(rows(0, 20)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 5,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 39, row: 6 });
    s.push(Block::rows(rows(20, 2)));
    s.draw_frame(&frame(), None).unwrap();
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied().as_deref(), Some("tail-0\ntail-1"));
}

/// The slot cleared under a selection that reaches into it: the selection is gone, the release
/// copies nothing, the draw does not fall over.
#[test]
fn clearing_the_tail_slot_drops_a_selection_on_it() {
    let (mut s, buf) = screen();
    s.push(Block::rows(rows(0, 20)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 4,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 39, row: 6 });
    s.set_tail(None);
    s.draw_frame(&frame(), None).unwrap();
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied(), None);
    assert_eq!(grid(&buf), fresh(&rows(0, 20), 0));
    // A selection on committed rows alone survives the clear.
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 0,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 39, row: 1 });
    s.set_tail(None);
    s.draw_frame(&frame(), None).unwrap();
    select(&mut s, SelectOp::Release);
    assert!(s.take_copied().is_some());
}

#[test]
fn neither_the_dump_nor_the_exit_writes_the_tail_block() {
    let (mut s, buf) = screen();
    s.push(Block::rows(rows(0, 3)));
    s.set_tail(Some(tail(3)));
    s.draw_frame(&frame(), None).unwrap();
    let mark = buf.bytes().len();
    s.view(ViewOp::Dump).unwrap();
    let dump = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert!(
        dump.contains("line-002") && !dump.contains("tail-"),
        "{dump:?}"
    );
    s.view(ViewOp::Return).unwrap();
    s.push(Block::rows(["summary"]));
    s.set_tail(None);
    let mark = buf.bytes().len();
    s.leave().unwrap();
    let exit = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert!(
        exit.contains("summary") && !exit.contains("tail-"),
        "{exit:?}"
    );
    assert!(!exit.contains("line-002"), "written once: {exit:?}");
}

#[test]
fn a_search_finds_nothing_in_the_tail_block() {
    let (mut s, _buf) = screen();
    s.push(Block::rows(["a tail-0 mention"]));
    s.set_tail(Some(tail(3)));
    let mut search = Search::new(&s.conv, 0);
    search.set_query(&mut s.conv, "tail-", 40);
    assert_eq!(search.status(), "1/1");
    assert_eq!(search.current(), Some((0, 0)));
    s.set_tail(Some(tail(5)));
    search.catch_up(&mut s.conv, 40);
    assert_eq!(search.status(), "1/1", "a new tail is not new text");
}
