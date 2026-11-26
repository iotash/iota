#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The tail slot (docs: the module's "tail slot"): an uncommitted last block, numbered `pushed`,
//! that follows the tail, takes appends above it, scrolls by its net growth, and is never
//! written out, searched or capped.

use std::time::Instant;

use super::{Conversation, Highlight, TRIMMED};
use crate::ui::block::{Block, Layout};
use crate::ui::render::region::RegionSnapshot;
use crate::ui::render::widget::CallWidget;

/// A tail block of `n` lines `t0`, `t1`, ….
struct Lines(usize);

impl Layout for Lines {
    fn lines(&self, _width: usize) -> Vec<String> {
        (0..self.0).map(|i| format!("t{i}")).collect()
    }
}

fn tail(n: usize) -> Block {
    Block::laid(Lines(n))
}

fn rows(from: usize, n: usize) -> Vec<String> {
    (from..from + n).map(|i| format!("r{i:03}")).collect()
}

fn conv(n: usize) -> Conversation {
    let mut c = Conversation::new();
    c.push(Block::rows(rows(0, n)));
    c
}

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_tail_block_follows_the_tail() {
    let mut c = conv(50);
    c.set_tail(Some(tail(3)));
    assert!(c.has_tail());
    assert_eq!(
        c.view_lines(20, 5, None),
        lines(&["r048", "r049", "t0", "t1", "t2"])
    );
    assert!(c.following());
    // A short session: it sits right under the last line.
    let mut short = conv(2);
    short.set_tail(Some(tail(2)));
    assert_eq!(
        short.view_lines(20, 10, None),
        lines(&["r000", "r001", "t0", "t1"])
    );
}

#[test]
fn an_append_lands_above_the_tail_block_and_moves_its_number_on() {
    let mut c = conv(50);
    c.set_tail(Some(tail(2)));
    assert_eq!(c.pushed(), 50);
    assert_eq!(
        c.plain_lines(50, 20).unwrap()[0].0,
        "t0",
        "numbered `pushed`"
    );
    c.push(Block::rows(rows(50, 2)));
    assert_eq!(
        c.view_lines(20, 5, None),
        lines(&["r049", "r050", "r051", "t0", "t1"])
    );
    assert_eq!(c.pushed(), 52);
    assert_eq!(c.plain_lines(51, 20).unwrap()[0].0, "r051");
    assert_eq!(c.plain_lines(52, 20).unwrap()[0].0, "t0");
    assert_eq!(c.first_held(), 0);
}

#[test]
fn the_tail_block_scrolls_the_tail_by_its_net_growth_only() {
    let mut c = conv(50);
    c.take_fresh(20, 100);
    c.set_tail(Some(tail(3)));
    assert_eq!(c.take_fresh(20, 100), 3, "rising is growth");
    c.set_tail(Some(tail(4)));
    assert_eq!(c.take_fresh(20, 100), 1, "one more row");
    c.set_tail(Some(tail(4)));
    assert_eq!(c.take_fresh(20, 100), 0, "the same height: nothing");
    c.push(Block::rows(rows(50, 2)));
    assert_eq!(c.take_fresh(20, 100), 2, "an append above it");
    c.set_tail(Some(tail(2)));
    assert_eq!(c.take_fresh(20, 100), 0, "shrinking scrolls nothing");
    // The settle: one summary row in, the widget out — net −1, floored at 0.
    c.set_tail(None);
    c.push(Block::rows(["summary"]));
    assert_eq!(c.take_fresh(20, 100), 0);
    assert_eq!(c.view_lines(20, 2, None), lines(&["r051", "summary"]));
    // Another width measures from the height it has there: a resize is no growth.
    c.set_tail(Some(tail(3)));
    c.take_fresh(20, 100);
    assert_eq!(c.take_fresh(30, 100), 0);
}

#[test]
fn scrolled_back_the_tail_block_scrolls_away_and_unseen_does_not_move() {
    let mut c = conv(50);
    c.set_tail(Some(tail(3)));
    c.scroll_up(10, 20, 5);
    assert!(!c.following());
    let before = c.view_lines(20, 5, None);
    assert_eq!(before, rows(38, 5));
    c.set_tail(Some(tail(5)));
    assert_eq!(
        c.view_lines(20, 5, None),
        before,
        "its growth moved the view"
    );
    assert_eq!(c.unseen(), 0, "its growth is not new output");
    assert!(!c.activity());
    c.note_activity();
    assert!(c.activity(), "a step row is activity");
    assert_eq!(c.unseen(), 0, "and still not a new line");
    c.push(Block::rows(["summary"]));
    assert_eq!(c.unseen(), 1, "a commit counts");
    assert_eq!(c.view_lines(20, 5, None), before);
    c.go_bottom();
    assert!(!c.activity() && c.unseen() == 0, "cleared with unseen");
    // Following, activity is nothing to report.
    c.note_activity();
    assert!(!c.activity());
    // Leaving the tail starts from nothing.
    c.scroll_up(10, 20, 5);
    assert!(!c.activity());
}

#[test]
fn a_resize_re_lays_the_tail_block_out() {
    let mut c = conv(5);
    let snap = RegionSnapshot {
        label: "[shell …]".to_owned(),
        preview_tail: vec!["x".repeat(60)],
        ..RegionSnapshot::default()
    };
    let w = CallWidget::new(&snap, 0, Instant::now()).unwrap();
    c.set_tail(Some(Block::laid(w)));
    let wide = c.view_lines(40, 3, None);
    let narrow = c.view_lines(20, 3, None);
    assert_eq!(
        crate::text::ansi::strip_ansi(&wide[2]),
        format!("  {}…", "x".repeat(35))
    );
    assert_eq!(
        crate::text::ansi::strip_ansi(&narrow[2]),
        format!("  {}…", "x".repeat(15))
    );
}

#[test]
fn the_marks_reach_the_tail_block() {
    let mut c = conv(3);
    c.set_tail(Some(tail(2)));
    c.view_lines(20, 5, None);
    assert_eq!(
        c.view_marks(20, 5),
        vec![
            Some((0, 0)),
            Some((1, 0)),
            Some((2, 0)),
            Some((3, 0)),
            Some((3, 1))
        ]
    );
}

#[test]
fn search_skips_the_tail_block() {
    let mut c = Conversation::new();
    c.push(Block::rows(["t0 here"]));
    c.set_tail(Some(tail(2)));
    let texts: Vec<(u64, String)> = c
        .texts_from(0, 20)
        .into_iter()
        .map(|(n, t)| (n, t.to_owned()))
        .collect();
    assert_eq!(texts, vec![(0, "t0 here".to_owned())]);
    assert!(
        c.texts_from(1, 20).is_empty(),
        "from the tail's number on: none"
    );
    // Nothing in it is lit either.
    let hl = Highlight {
        query: "t0",
        current: None,
    };
    let lit = c.view_lines(20, 5, Some(&hl));
    assert_ne!(lit[0], "t0 here", "the committed row is lit");
    assert_eq!(lit[1], "t0", "the tail is not");
}

#[test]
fn a_write_out_skips_the_tail_block() {
    let mut c = conv(3);
    c.set_tail(Some(tail(2)));
    let (out, next) = c.lines_after(0, 20, false);
    assert_eq!(out, rows(0, 3));
    assert_eq!(next, 3);
    let (out, next) = c.lines_after(0, 20, true);
    assert_eq!(out, rows(0, 3), "the exit does not write it either");
    assert_eq!(next, 3);
    c.push(Block::rows(rows(3, 1)));
    assert_eq!(c.lines_after(3, 20, false), (rows(3, 1), 4));
}

#[test]
fn a_write_out_stops_above_an_open_live_block_with_the_tail_block_up() {
    let mut c = conv(2);
    c.set_tail(Some(tail(2)));
    let mut md = crate::ui::testutil::Streamed::new();
    c.push(md.feed("\n```\n"));
    c.push(md.feed("fn a() {}\n"));
    let (out, next) = c.lines_after(0, 40, false);
    assert!(out.iter().all(|l| !l.contains("fn a")), "{out:?}");
    assert!(out.iter().all(|l| !l.starts_with('t')), "{out:?}");
    assert!(next < c.pushed());
}

#[test]
fn the_cap_never_drops_the_tail_block() {
    let mut c = Conversation::with_cap(5);
    c.set_tail(Some(tail(2)));
    c.push(Block::rows(rows(0, 10)));
    let all = c.view_lines(80, 50, None);
    assert_eq!(all.len(), 7, "the trimmed row, 4 rows, the tail: {all:?}");
    assert!(all[0].contains(TRIMMED));
    assert_eq!(&all[1..5], &rows(6, 4)[..]);
    assert_eq!(&all[5..], &lines(&["t0", "t1"])[..]);
    assert_eq!(c.first_held(), 6);
    assert_eq!(c.plain_lines(10, 20).unwrap()[1].0, "t1");
    assert_eq!(c.lines_after(0, 80, true).0.len(), 5);
}

#[test]
fn the_last_shown_block_is_the_last_committed_one_under_the_tail_block() {
    let mut c = conv(10);
    c.set_tail(Some(tail(3)));
    assert_eq!(c.last_shown(20, 5), 9);
    let mut empty = Conversation::new();
    empty.set_tail(Some(tail(3)));
    assert_eq!(empty.last_shown(20, 5), 0);
}

/// Risk 1: the view anchored inside the tail block, then the slot cleared — the anchor moves to
/// the last committed block instead of indexing past the end.
#[test]
fn clearing_the_tail_slot_with_the_anchor_on_it_does_not_panic() {
    let mut c = conv(50);
    c.set_tail(Some(tail(14)));
    c.view_lines(20, 5, None);
    c.scroll_up(3, 20, 5);
    assert_eq!(
        c.view_lines(20, 5, None),
        lines(&["t6", "t7", "t8", "t9", "t10"]),
        "anchored inside the tail"
    );
    let mark = c.mark();
    assert_eq!(mark, Some((50, 6)));
    c.set_tail(None);
    assert_eq!(c.view_lines(20, 5, None), rows(45, 5));
    assert!(c.following(), "the last block's start shows the tail");
    assert_eq!(c.view_marks(20, 5).len(), 5);
    // A mark taken on it goes back to the last committed block.
    c.restore(mark, 20, 5);
    assert_eq!(c.view_lines(20, 5, None), rows(45, 5));
    // Anchored on the tail far from the end too.
    let mut c = conv(3);
    c.set_tail(Some(tail(40)));
    c.scroll_up(10, 20, 5);
    c.set_tail(None);
    assert_eq!(c.view_lines(20, 5, None), rows(0, 3));
    // And with nothing committed at all.
    let mut c = Conversation::new();
    c.set_tail(Some(tail(40)));
    c.scroll_up(10, 20, 5);
    c.set_tail(None);
    assert!(c.view_lines(20, 5, None).is_empty());
    assert!(c.following());
}
