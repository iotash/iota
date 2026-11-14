#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The full-screen terminal over a vt100 replay: the frame pinned to the bottom, the
//! conversation on it, a resize as one full repaint, half-screen scrolling, and the exit that
//! hands the conversation to the main screen.

use ratatui::layout::Size;

use super::{SYNC_BEGIN, Screen, TRANSCRIPT_MARK, ViewOp};
use crate::ui::render::frame::FrameView;
use crate::ui::testutil::SharedBuf;

fn screen(w: u16, h: u16) -> (Screen<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    (
        Screen::new(buf.clone(), Size::new(w, h), false).unwrap(),
        buf,
    )
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
    s.draw_frame(&frame(40), None).unwrap();
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
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    s.resize(Size::new(20, 10));
    s.draw_frame(&frame(20), None).unwrap();
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
    s.draw_frame(&frame(40), None).unwrap();
    for _ in 0..40 {
        s.view(ViewOp::HalfUp).unwrap();
    }
    s.draw_frame(&frame(40), None).unwrap();
    let g = grid(&replay(&buf, 12, 40));
    assert_eq!(g[0], "line-000", "the top of the session: {g:#?}");
    assert_eq!(g[11], "status", "the frame stays");
    // Clamped at the top: one PgDn moves at once.
    s.view(ViewOp::HalfDown).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(grid(&replay(&buf, 12, 40))[0], "line-004");
    for _ in 0..40 {
        s.view(ViewOp::HalfDown).unwrap();
    }
    s.draw_frame(&frame(40), None).unwrap();
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
    s.draw_frame(&frame(40), None).unwrap();
    s.resize(Size::new(20, 12));
    s.leave().unwrap();
    let mut p = vt100::Parser::new(12, 40, 1000);
    p.process(b"$ iota\r\n");
    p.process(&buf.bytes());
    assert!(!p.screen().alternate_screen(), "the main screen is back");
    let g = grid(&p);
    assert_eq!(g[0], "$ iota", "{g:#?}");
    assert_eq!(g[1], TRANSCRIPT_MARK);
    assert_eq!(&g[2..7], &rows(5)[..]);
    assert_eq!(g[7], "w".repeat(20), "wrapped at the exit width");
    assert_eq!(g[8], "wwwwwwwwwwEND");
    assert!(
        !g.iter().any(|r| r.contains("status")),
        "no frame row: {g:#?}"
    );
    assert_eq!(
        p.screen().cursor_position(),
        (9, 0),
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
    s.draw_frame(&frame(80), None).unwrap();
    s.end_batch().unwrap();
    let first = buf.bytes()[mark..].to_vec();
    assert!(first.starts_with(SYNC_BEGIN), "a full paint is one block");
    let mark = buf.bytes().len();
    let mut typed = frame(80);
    typed.rows[1] = "❯ x".to_owned();
    typed.cursor = Some((3, 1));
    s.begin_batch();
    s.draw_frame(&typed, None).unwrap();
    s.end_batch().unwrap();
    let key = &buf.bytes()[mark..];
    assert!(!key.is_empty() && key.len() < 64, "{} bytes", key.len());
    assert!(!key.starts_with(SYNC_BEGIN), "a keystroke goes out bare");
}

const SU_REGION: &str = "\u{1b}[r";

fn fresh_paint(rows_in: &[String], w: u16, h: u16, f: &FrameView) -> Vec<String> {
    let (mut s, buf) = screen(w, h);
    s.push(rows_in);
    s.draw_frame(f, None).unwrap();
    grid(&replay(&buf, h, w))
}

/// Following the tail, a new line scrolls the viewport with ONE scroll-region operation and the
/// draw after it writes the new line, not every row that moved — and the screen it leaves is the
/// screen a fresh paint of the same state draws.
#[test]
fn following_output_scrolls_the_viewport_with_one_operation() {
    let (mut s, buf) = screen(80, 24);
    let all: Vec<String> = (0..60)
        .map(|i| format!("{i:02} {}", "x".repeat(60)))
        .collect();
    s.push(&all[..40]);
    s.draw_frame(&frame(80), None).unwrap();
    let mut sizes = Vec::new();
    for i in 40..60 {
        let mark = buf.bytes().len();
        s.push(&all[i..=i]);
        s.draw_frame(&frame(80), None).unwrap();
        let out = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
        assert!(
            out.contains("\u{1b}[1;20r\u{1b}[1S"),
            "line {i}: no scroll: {out:?}"
        );
        sizes.push(out.len());
    }
    assert!(
        sizes.iter().all(|n| *n < 200),
        "a line costs the line, not the viewport: {sizes:?}"
    );
    assert_eq!(
        grid(&replay(&buf, 24, 80)),
        fresh_paint(&all, 80, 24, &frame(80))
    );
}

/// Several lines between two frames are one scroll of that many lines.
#[test]
fn a_batch_of_lines_is_one_scroll_of_that_many() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(20));
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    let more: Vec<String> = (20..23).map(|i| format!("line-{i:03}")).collect();
    s.push(&more);
    s.draw_frame(&frame(40), None).unwrap();
    let out = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert_eq!(out.matches("S\u{1b}[r").count(), 1, "{out:?}");
    assert!(out.contains("\u{1b}[1;8r\u{1b}[3S"), "{out:?}");
    let mut all = rows(20);
    all.extend(more);
    assert_eq!(
        grid(&replay(&buf, 12, 40)),
        fresh_paint(&all, 40, 12, &frame(40))
    );
}

/// No scroll where the viewport did not simply move up: a paused view, a resize, a frame that
/// changed height, a clear.
#[test]
fn no_scroll_when_the_viewport_did_not_just_move_up() {
    let scrolled = |buf: &SharedBuf, mark: usize| {
        String::from_utf8_lossy(&buf.bytes()[mark..]).contains(SU_REGION)
    };
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(30));
    s.draw_frame(&frame(40), None).unwrap();
    // Paused.
    s.view(ViewOp::HalfUp).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    s.push(&rows(1));
    s.draw_frame(&frame(40), None).unwrap();
    assert!(!scrolled(&buf, mark), "a paused view scrolled");
    // Back on the tail: the first frame repaints, the next one scrolls.
    s.view(ViewOp::Bottom).unwrap();
    let mark = buf.bytes().len();
    s.draw_frame(&frame(40), None).unwrap();
    assert!(
        !scrolled(&buf, mark),
        "the frame that resumed following scrolled"
    );
    let mark = buf.bytes().len();
    s.push(&rows(1));
    s.draw_frame(&frame(40), None).unwrap();
    assert!(scrolled(&buf, mark), "following again, a new line scrolls");
    // A resize.
    s.resize(Size::new(40, 14));
    s.push(&rows(1));
    let mark = buf.bytes().len();
    s.draw_frame(&frame(40), None).unwrap();
    assert!(!scrolled(&buf, mark), "a resize scrolled");
    // A taller frame.
    let mut tall = frame(40);
    tall.rows.insert(0, "preview".to_owned());
    s.push(&rows(1));
    let mark = buf.bytes().len();
    s.draw_frame(&tall, None).unwrap();
    assert!(
        !scrolled(&buf, mark),
        "a frame that changed height scrolled"
    );
    // A clear.
    s.clear().unwrap();
    s.push(&rows(1));
    let mark = buf.bytes().len();
    s.draw_frame(&tall, None).unwrap();
    assert!(!scrolled(&buf, mark), "a cleared screen scrolled");
}

/// A paused view: new output moves nothing on screen, the last viewport row counts it, and the
/// view survives a resize — the anchored row stays the first one shown.
#[test]
fn a_paused_view_holds_through_output_and_a_resize() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(40));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::HalfUp).unwrap();
    s.view(ViewOp::HalfUp).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let before = grid(&replay(&buf, 12, 40));
    assert!(before[7].contains("Ctrl+End"), "{before:#?}");
    s.push(&rows(5));
    s.draw_frame(&frame(40), None).unwrap();
    let after = grid(&replay(&buf, 12, 40));
    assert_eq!(&after[..7], &before[..7], "new output moved the view");
    assert!(after[7].contains("↓ 5 new lines"), "{after:#?}");
    let mark = buf.bytes().len();
    s.resize(Size::new(30, 16));
    s.draw_frame(&frame(30), None).unwrap();
    let mut p = vt100::Parser::new(12, 40, 0);
    p.process(&buf.bytes()[..mark]);
    p.screen_mut().set_size(16, 30);
    p.process(&buf.bytes()[mark..]);
    assert_eq!(
        grid(&p)[0],
        before[0],
        "the anchored row is still the first one shown"
    );
}

/// Ctrl+O: the main screen holds the whole conversation under the mark, nothing is drawn while it
/// is out (output keeps collecting), and the next key repaints the full screen with what arrived.
/// An exit after a dump writes everything once more and never leaves the alternate screen twice.
#[test]
fn a_dump_writes_the_conversation_out_and_the_next_key_comes_back() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(30));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::Dump).unwrap();
    let mut p = vt100::Parser::new(12, 40, 100);
    p.process(&buf.bytes());
    assert!(
        !p.screen().alternate_screen(),
        "the main screen shows the dump"
    );
    let mark = buf.bytes().len();
    s.push(&rows(2));
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(buf.bytes().len(), mark, "nothing drawn while dumped");
    s.view(ViewOp::Return).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let p = replay(&buf, 12, 40);
    assert!(p.screen().alternate_screen());
    let g = grid(&p);
    assert_eq!(g[7], "line-001", "the rows that arrived meanwhile: {g:#?}");
    assert_eq!(g[11], "status");
    s.push(&["after".to_owned()]);
    let mark = buf.bytes().len();
    s.view(ViewOp::Dump).unwrap();
    s.push(&["last".to_owned()]);
    s.leave().unwrap();
    let tail = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert_eq!(
        tail.matches("\u{1b}[?1049l").count(),
        1,
        "one leave: {tail:?}"
    );
    // Each row once, in order, across the three writes; one mark, at the start.
    let mut p = vt100::Parser::new(12, 40, 200);
    p.process(b"$ iota\r\n");
    p.process(&buf.bytes());
    let all = reachable(&mut p);
    let mut want = vec!["$ iota".to_owned(), TRANSCRIPT_MARK.to_owned()];
    want.extend(rows(30));
    want.extend(rows(2));
    want.extend(["after".to_owned(), "last".to_owned()]);
    let got: Vec<String> = all.into_iter().filter(|r| !r.is_empty()).collect();
    assert_eq!(got, want);
}

/// Every row reachable on the main screen: its history, oldest first, then the screen.
fn reachable(p: &mut vt100::Parser) -> Vec<String> {
    let cols = p.screen().size().1;
    let screen: Vec<String> = p
        .screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect();
    p.screen_mut().set_scrollback(usize::MAX);
    let depth = p.screen().scrollback();
    let mut all = Vec::with_capacity(depth + screen.len());
    for off in (1..=depth).rev() {
        p.screen_mut().set_scrollback(off);
        all.push(
            p.screen()
                .rows(0, cols)
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_owned(),
        );
    }
    p.screen_mut().set_scrollback(0);
    all.extend(screen);
    all
}

/// The current resident set of this process, in KiB (`ps`, so no extra dependency).
fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// M1 (tui-plan §7) — a measurement, not a check: a synthetic 100k-row session (coloured rows of
/// 40–120 columns, one in ten wider than the 120-column screen) — the RSS it adds, one `PgUp`
/// frame at the deepest point, one resize repaint there and one at the tail. Runs only with
/// `IOTA_MEASURE_M1=1`; `cargo test --release --lib m1 -- --nocapture` for the numbers.
#[test]
fn m1_a_hundred_thousand_rows() {
    use std::time::Instant;
    if std::env::var_os("IOTA_MEASURE_M1").is_none() {
        return;
    }
    let rows: Vec<String> = (0..100_000)
        .map(|i| {
            let len = if i % 10 == 0 { 200 } else { 40 + i % 80 };
            format!(
                "\u{1b}[36m{i:06}\u{1b}[0m {}",
                "lorem ipsum ".repeat(len / 12 + 1)[..len].to_owned()
            )
        })
        .collect();
    let (mut s, _buf) = screen(120, 40);
    let base = rss_kib();
    for chunk in rows.chunks(20) {
        s.push(chunk);
    }
    drop(rows);
    s.draw_frame(&frame(120), None).unwrap();
    let grown = rss_kib();
    let frame_time = |s: &mut Screen<SharedBuf>, op: Option<ViewOp>| {
        let t = Instant::now();
        if let Some(op) = op {
            s.view(op).unwrap();
        }
        s.draw_frame(&frame(120), None).unwrap();
        t.elapsed()
    };
    let tail_pgup = frame_time(&mut s, Some(ViewOp::HalfUp));
    s.view(ViewOp::Top).unwrap();
    s.draw_frame(&frame(120), None).unwrap();
    for _ in 0..3 {
        s.view(ViewOp::HalfDown).unwrap();
    }
    s.draw_frame(&frame(120), None).unwrap();
    let deep_pgup = frame_time(&mut s, Some(ViewOp::HalfUp));
    let top = frame_time(&mut s, Some(ViewOp::Top));
    s.resize(Size::new(100, 40));
    let deep_resize = frame_time(&mut s, None);
    s.view(ViewOp::Bottom).unwrap();
    s.draw_frame(&frame(100), None).unwrap();
    s.resize(Size::new(90, 40));
    let tail_resize = frame_time(&mut s, None);
    let bottom = frame_time(&mut s, Some(ViewOp::Bottom));
    s.view(ViewOp::Search(super::search::SearchOp::Open))
        .unwrap();
    let t = Instant::now();
    s.view(ViewOp::Search(super::search::SearchOp::Query(
        "000123".to_owned(),
    )))
    .unwrap();
    let query = t.elapsed();
    let searched = frame_time(&mut s, None);
    println!(
        "M1: RSS +{} KiB for 100k rows; PgUp frame at the tail {tail_pgup:?}, near the first row \
         {deep_pgup:?}; Ctrl+Home frame {top:?}; resize repaint near the first row {deep_resize:?}, \
         at the tail {tail_resize:?}; Ctrl+End frame {bottom:?}; a query over every row {query:?}, \
         the frame on its hit {searched:?}",
        grown.saturating_sub(base)
    );
}

/// The point of no return is the first frame: a screen built and fed rows writes nothing, and one
/// left before any frame writes the conversation out on the main screen it never left — no
/// alternate-screen sequence either way.
#[test]
fn nothing_reaches_the_terminal_before_the_first_frame() {
    let (mut s, buf) = screen(40, 12);
    s.push(&rows(3));
    assert!(
        buf.bytes().is_empty(),
        "written before the first frame: {:?}",
        buf.bytes()
    );
    s.leave().unwrap();
    let out = String::from_utf8_lossy(&buf.bytes()).into_owned();
    assert!(
        !out.contains("\u{1b}[?1049"),
        "an alternate-screen sequence: {out:?}"
    );
    assert!(
        out.contains("line-000") && out.contains("line-002"),
        "{out:?}"
    );
    // …and a screen that drew a frame entered it with that frame.
    let (mut s, buf) = screen(40, 12);
    s.draw_frame(&frame(40), None).unwrap();
    assert!(String::from_utf8_lossy(&buf.bytes()).starts_with("\u{1b}[?1049h"));
}

/// The write-out policy across a resize: rows a mid-session write-out wrote stay as they were written
/// (the terminal owns them now), and the exit writes the rest wrapped at the width it has then.
#[test]
fn rows_written_before_a_resize_keep_their_width_and_the_rest_take_the_exit_width() {
    let (mut s, buf) = screen(40, 12);
    s.push(&[format!("{}END", "a".repeat(30))]);
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::Dump).unwrap();
    s.view(ViewOp::Return).unwrap();
    s.push(&[format!("{}END", "b".repeat(30))]);
    s.resize(Size::new(20, 12));
    s.draw_frame(&frame(20), None).unwrap();
    s.leave().unwrap();
    let out = String::from_utf8_lossy(&buf.bytes()).into_owned();
    assert!(
        out.contains(&format!("{}END\u{1b}[0m\r\n", "a".repeat(30))),
        "the first row as written at 40"
    );
    assert!(
        out.contains(&format!(
            "{}\u{1b}[0m\r\n{}END",
            "b".repeat(20),
            "b".repeat(10)
        )),
        "the second wrapped at the exit's 20: {out:?}"
    );
    assert_eq!(out.matches(TRANSCRIPT_MARK).count(), 1);
}
