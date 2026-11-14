#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The buffer: the tail followed, the wrap cache, the anchor surviving new output and a resize,
//! the way back to the tail, and the cap.

use super::{Conversation, Pos, TRIMMED, View};

fn rows(from: usize, n: usize) -> Vec<String> {
    (from..from + n).map(|i| format!("r{i:03}")).collect()
}

fn conv(n: usize) -> Conversation {
    let mut c = Conversation::new();
    c.push(&rows(0, n));
    c
}

#[test]
fn following_shows_the_tail_and_a_short_session_whole() {
    let mut c = conv(50);
    assert_eq!(c.view_lines(20, 5, None), rows(45, 5));
    let mut short = conv(3);
    assert_eq!(
        short.view_lines(20, 5, None),
        rows(0, 3),
        "fewer lines than the viewport"
    );
    assert!(short.following());
}

#[test]
fn a_row_wider_than_the_screen_is_wrapped_once_per_width() {
    let mut c = Conversation::new();
    c.push(&[format!("{}END", "w".repeat(27)), "next".to_owned()]);
    assert_eq!(
        c.view_lines(10, 5, None),
        ["wwwwwwwwww", "wwwwwwwwww", "wwwwwwwEND", "next"]
    );
    assert_eq!(
        c.rows[0].wrap.as_ref().map(|(w, l)| (*w, l.len())),
        Some((10, 3))
    );
    assert!(c.rows[1].wrap.is_none(), "a row that fits is never wrapped");
    // Wider again: the row fits and no wrap is consulted.
    assert_eq!(
        c.view_lines(40, 5, None)[0],
        format!("{}END", "w".repeat(27))
    );
    // Another width replaces the cached wrap.
    c.view_lines(15, 5, None);
    assert_eq!(c.rows[0].wrap.as_ref().map(|(w, _)| *w), Some(15));
}

#[test]
fn scrolling_up_anchors_on_a_row_and_new_output_does_not_move_the_view() {
    let mut c = conv(50);
    c.scroll_up(10, 20, 5);
    assert!(!c.following());
    let before = c.view_lines(20, 5, None);
    assert_eq!(before, rows(35, 5));
    c.push(&rows(50, 7));
    assert_eq!(
        c.view_lines(20, 5, None),
        before,
        "new output moved the view"
    );
    assert_eq!(c.unseen(), 7);
}

#[test]
fn a_resize_keeps_the_anchored_row_at_the_top() {
    let mut c = Conversation::new();
    for i in 0..40 {
        c.push(&[format!("{i:02}{}", "x".repeat(28))]); // 30 columns each
    }
    c.scroll_up(20, 40, 6);
    let top = c.view_lines(40, 6, None)[0].clone();
    // Narrower: each row is three lines now; the same row is still the first one shown.
    let narrow = c.view_lines(10, 6, None);
    assert_eq!(narrow[0], top[..10]);
    // A line inside a wrapped row past its new height is clamped to the row's last line.
    c.view = View::At(Pos { row: 5, line: 2 });
    let wide = c.view_lines(40, 6, None);
    assert!(wide[0].starts_with("05"), "{wide:?}");
}

#[test]
fn reaching_the_bottom_follows_again_and_ctrl_end_does_too() {
    let mut c = conv(50);
    c.scroll_up(20, 20, 10);
    c.push(&rows(50, 3));
    c.scroll_down(5, 20, 10);
    assert!(!c.following());
    c.scroll_down(100, 20, 10);
    assert!(c.following(), "PgDn to the bottom resumes following");
    assert_eq!(c.unseen(), 0);
    assert_eq!(c.view_lines(20, 10, None), rows(43, 10));
    c.scroll_up(5, 20, 10);
    c.push(&rows(53, 2));
    c.go_bottom();
    assert!(c.following());
    assert_eq!(c.view_lines(20, 10, None)[9], "r054");
}

#[test]
fn ctrl_home_shows_the_first_line_and_page_up_stops_there() {
    let mut c = conv(50);
    c.go_top(20, 10);
    assert_eq!(c.view_lines(20, 10, None), rows(0, 10));
    c.scroll_up(5, 20, 10);
    assert_eq!(c.view_lines(20, 10, None)[0], "r000");
    let mut short = conv(4);
    short.go_top(20, 10);
    assert!(short.following(), "a session that fits stays on the tail");
}

#[test]
fn past_the_cap_the_oldest_rows_go_and_a_first_row_says_so() {
    let mut c = Conversation::with_cap(10);
    c.push(&rows(0, 8));
    c.scroll_up(3, 80, 3); // anchored on r002
    assert_eq!(c.view_lines(80, 3, None)[0], "r002");
    c.push(&rows(8, 3)); // 11 rows: r000 and r001 go, the notice comes in
    assert_eq!(c.rows.len(), 10);
    assert!(c.rows[0].text.contains(TRIMMED));
    assert_eq!(c.rows[1].text, "r002");
    assert_eq!(
        c.view_lines(80, 3, None)[0],
        "r002",
        "the anchor moved with its row"
    );
    c.push(&rows(11, 5)); // r002 goes too: the anchor falls back to the top
    assert!(c.view_lines(80, 3, None)[0].contains(TRIMMED));
    c.push(&rows(16, 4));
    assert_eq!(c.rows.len(), 10);
    assert_eq!(
        c.rows.iter().filter(|r| r.text.contains(TRIMMED)).count(),
        1
    );
}

#[test]
fn a_dump_asks_for_the_rows_after_the_last_one_written() {
    let mut c = Conversation::with_cap(6);
    c.push(&["a".to_owned(), "b".repeat(12)]);
    let (lines, next) = c.lines_after(0, 5);
    assert_eq!(lines, ["a", "bbbbb", "bbbbb", "bb"]);
    assert_eq!(c.lines_after(next, 5).0, Vec::<String>::new());
    c.push(&rows(0, 2));
    let (lines, next) = c.lines_after(next, 5);
    assert_eq!(lines, ["r000", "r001"]);
    // Past the cap before the next dump: what was dropped unwritten is the notice row.
    c.push(&rows(2, 6));
    let (lines, _) = c.lines_after(next, 80);
    assert!(lines[0].contains(TRIMMED), "{lines:?}");
    assert_eq!(lines[1..], rows(3, 5)[..]);
}

#[test]
fn fresh_lines_count_what_was_committed_since_the_last_draw() {
    let mut c = Conversation::new();
    c.push(&rows(0, 3));
    assert_eq!(c.take_fresh(20), 3);
    assert_eq!(c.take_fresh(20), 0);
    c.push(&["x".repeat(25)]);
    assert_eq!(c.take_fresh(10), 3, "display lines, not rows");
}
