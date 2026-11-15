#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The full-screen terminal over a vt100 replay: the frame pinned to the bottom, the
//! conversation on it, a resize as one full repaint, half-screen scrolling, and the exit that
//! hands the conversation to the main screen.

use ratatui::layout::Size;

/// Committed rows as the buffer's blocks.
fn rows_of(lines: &[String]) -> Vec<crate::ui::block::Block> {
    crate::ui::block::Block::rows(lines.iter().cloned())
}

use super::{SYNC_BEGIN, Screen, TRANSCRIPT_MARK, ViewOp};
use crate::ui::render::frame::FrameView;
use crate::ui::testutil::SharedBuf;

fn screen(w: u16, h: u16) -> (Screen<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    (
        Screen::new(buf.clone(), Size::new(w, h), false, false).unwrap(),
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
        map: crate::ui::render::frame::FrameMap::default(),
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
    s.push(rows_of(&rows(3)));
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

/// A resize repaints the whole screen at the new size: the screen grows taller than the
/// conversation, so the rows the old layout used — its frame at rows 8–11 — are blank at the new
/// size and a repaint that did not clear first would leave that frame there as a ghost.
#[test]
fn a_resize_repaints_the_whole_screen_at_the_new_size() {
    let (mut s, buf) = screen(40, 12);
    s.push(rows_of(&rows(2)));
    s.push(rows_of(&[format!("{}END", "w".repeat(30))]));
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    s.resize(Size::new(20, 20));
    s.draw_frame(&frame(20), None).unwrap();
    // The emulator itself resized before the repaint arrived.
    let mut p = vt100::Parser::new(12, 40, 0);
    p.process(&buf.bytes()[..mark]);
    p.screen_mut().set_size(20, 20);
    p.process(&buf.bytes()[mark..]);
    let g = grid(&p);
    assert_eq!(g[19], "status");
    assert_eq!(g[16], "┄".repeat(20), "the frame is full width: {g:#?}");
    assert_eq!(
        g[15], "wwwwwwwwwwEND",
        "the wide row wraps at the new width"
    );
    assert_eq!(g[14], "w".repeat(20));
    assert_eq!(&g[12..14], &["line-000", "line-001"], "{g:#?}");
    assert!(
        g[..12].iter().all(String::is_empty),
        "the old layout left a ghost: {g:#?}"
    );
    assert_eq!(g.iter().filter(|r| *r == "status").count(), 1, "{g:#?}");
}

#[test]
fn page_keys_walk_back_to_the_first_row_and_down_to_the_tail() {
    let (mut s, buf) = screen(40, 12);
    s.push(rows_of(&rows(50)));
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
    s.push(rows_of(&rows(5)));
    s.push(rows_of(&[format!("{}END", "w".repeat(30))]));
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

/// After a resume the buffer holds only the replayed rounds, so the exit writes only those: a
/// ten-round session leaves its last three in the main screen, once each, and nothing older
/// (DIVERGENCES X-98/X-99).
#[test]
fn the_exit_after_a_resume_writes_only_the_replayed_rounds() {
    use crate::provider::model::Message;
    use crate::repl::render::replay::{echo_rounds, resume_echo};
    let history: Vec<Message> = (0..10)
        .flat_map(|i| {
            [
                Message::user(format!("q{i}")),
                Message::assistant(format!("r{i}")),
            ]
        })
        .collect();
    let lines = echo_rounds(resume_echo(&history), |_| false, 40, None);
    let (mut s, buf) = screen(40, 12);
    s.push(lines);
    s.draw_frame(&frame(40), None).unwrap();
    s.leave().unwrap();
    let mut p = vt100::Parser::new(12, 40, 1000);
    p.process(&buf.bytes());
    let all = reachable(&mut p);
    let count = |t: &str| all.iter().filter(|r| r.trim() == t).count();
    for i in 7..10 {
        assert_eq!(count(&format!("❯ q{i}")), 1, "round {i} once: {all:#?}");
        assert_eq!(count(&format!("r{i}")), 1, "reply {i} once: {all:#?}");
    }
    for i in 0..7 {
        assert!(
            !all.iter()
                .any(|r| r.contains(&format!("q{i}")) || r.contains(&format!("r{i}"))),
            "round {i} was not replayed, so it is not written: {all:#?}"
        );
    }
}

#[test]
fn a_frame_larger_than_sync_min_is_one_synchronized_update_and_a_keystroke_is_bare() {
    let (mut s, buf) = screen(80, 24);
    s.push(rows_of(
        &(0..30)
            .map(|i| format!("{i:02} {}", "x".repeat(70)))
            .collect::<Vec<_>>(),
    ));
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
    s.push(rows_of(rows_in));
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
    s.push(rows_of(&all[..40]));
    s.draw_frame(&frame(80), None).unwrap();
    let mut sizes = Vec::new();
    for i in 40..60 {
        let mark = buf.bytes().len();
        s.push(rows_of(&all[i..=i]));
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
    s.push(rows_of(&rows(20)));
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    let more: Vec<String> = (20..23).map(|i| format!("line-{i:03}")).collect();
    s.push(rows_of(&more));
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
    s.push(rows_of(&rows(30)));
    s.draw_frame(&frame(40), None).unwrap();
    // Paused.
    s.view(ViewOp::HalfUp).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let mark = buf.bytes().len();
    s.push(rows_of(&rows(1)));
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
    s.push(rows_of(&rows(1)));
    s.draw_frame(&frame(40), None).unwrap();
    assert!(scrolled(&buf, mark), "following again, a new line scrolls");
    // A resize.
    s.resize(Size::new(40, 14));
    s.push(rows_of(&rows(1)));
    let mark = buf.bytes().len();
    s.draw_frame(&frame(40), None).unwrap();
    assert!(!scrolled(&buf, mark), "a resize scrolled");
    // A taller frame.
    let mut tall = frame(40);
    tall.rows.insert(0, "preview".to_owned());
    s.push(rows_of(&rows(1)));
    let mark = buf.bytes().len();
    s.draw_frame(&tall, None).unwrap();
    assert!(
        !scrolled(&buf, mark),
        "a frame that changed height scrolled"
    );
    // A clear.
    s.clear().unwrap();
    s.push(rows_of(&rows(1)));
    let mark = buf.bytes().len();
    s.draw_frame(&tall, None).unwrap();
    assert!(!scrolled(&buf, mark), "a cleared screen scrolled");
}

/// A paused view: new output moves nothing on screen, the last viewport row counts it, and the
/// view survives a resize — the anchored row stays the first one shown.
#[test]
fn a_paused_view_holds_through_output_and_a_resize() {
    let (mut s, buf) = screen(40, 12);
    s.push(rows_of(&rows(40)));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::HalfUp).unwrap();
    s.view(ViewOp::HalfUp).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let before = grid(&replay(&buf, 12, 40));
    assert!(before[7].contains("Ctrl+End"), "{before:#?}");
    s.push(rows_of(&rows(5)));
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
    s.push(rows_of(&rows(30)));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::Dump).unwrap();
    let mut p = vt100::Parser::new(12, 40, 100);
    p.process(&buf.bytes());
    assert!(
        !p.screen().alternate_screen(),
        "the main screen shows the dump"
    );
    let mark = buf.bytes().len();
    s.push(rows_of(&rows(2)));
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(buf.bytes().len(), mark, "nothing drawn while dumped");
    s.view(ViewOp::Return).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    let p = replay(&buf, 12, 40);
    assert!(p.screen().alternate_screen());
    let g = grid(&p);
    assert_eq!(g[7], "line-001", "the rows that arrived meanwhile: {g:#?}");
    assert_eq!(g[11], "status");
    s.push(rows_of(&["after".to_owned()]));
    let mark = buf.bytes().len();
    s.view(ViewOp::Dump).unwrap();
    s.push(rows_of(&["last".to_owned()]));
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
        s.push(rows_of(chunk));
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
    s.push(rows_of(&rows(3)));
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
    s.push(rows_of(&[format!("{}END", "a".repeat(30))]));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::Dump).unwrap();
    s.view(ViewOp::Return).unwrap();
    s.push(rows_of(&[format!("{}END", "b".repeat(30))]));
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

// --- the mouse (tui-plan S6) ---------------------------------------------------------------------

use super::{LEAVE, MOUSE_OFF, MOUSE_ON, SelectOp};

fn mouse_screen(w: u16, h: u16) -> (Screen<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    (
        Screen::new(buf.clone(), Size::new(w, h), false, true).unwrap(),
        buf,
    )
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn select(s: &mut Screen<SharedBuf>, op: SelectOp) {
    s.view(ViewOp::Select(op)).unwrap();
}

/// Reporting is asked for on the alternate screen only, and dropped before every way out of it —
/// a write-out, the exit — so the main screen has the terminal's own selection; off, no mouse
/// sequence is written at all.
#[test]
fn mouse_reporting_lives_with_the_alternate_screen_and_the_off_switch_writes_none() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(3)));
    s.draw_frame(&frame(40), None).unwrap();
    assert!(
        has(&buf.bytes(), MOUSE_ON),
        "asked for with the first frame"
    );
    let mark = buf.bytes().len();
    s.view(ViewOp::Dump).unwrap();
    let dump = buf.bytes()[mark..].to_vec();
    let mut off_then_leave = MOUSE_OFF.to_vec();
    off_then_leave.extend_from_slice(LEAVE);
    assert!(
        dump.starts_with(&off_then_leave),
        "off before the main screen"
    );
    let mark = buf.bytes().len();
    s.view(ViewOp::Return).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    assert!(
        has(&buf.bytes()[mark..], MOUSE_ON),
        "back on with the full screen"
    );
    s.leave().unwrap();
    assert!(
        has(&buf.bytes()[mark..], &off_then_leave),
        "off at the exit"
    );
    let p = replay(&buf, 12, 40);
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );

    let (mut s, buf) = screen(40, 12);
    s.push(rows_of(&rows(3)));
    s.draw_frame(&frame(40), None).unwrap();
    s.view(ViewOp::Dump).unwrap();
    s.view(ViewOp::Return).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    s.leave().unwrap();
    assert!(
        !has(&buf.bytes(), b"\x1b[?1000") && !has(&buf.bytes(), b"\x1b[?1006"),
        "the off switch never touches mouse reporting"
    );
}

/// The wheel scrolls three lines at a step: up leaves the tail (new output then holds still), down
/// to the bottom follows it again.
#[test]
fn the_wheel_pauses_following_and_the_bottom_follows_again() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(50)));
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(grid(&replay(&buf, 12, 40))[7], "line-049");
    s.view(ViewOp::Wheel(-3)).unwrap();
    assert!(s.paused(), "a step up leaves the tail");
    s.push(rows_of(&rows(2)));
    s.draw_frame(&frame(40), None).unwrap();
    let g = grid(&replay(&buf, 12, 40));
    assert_eq!(
        g[0], "line-039",
        "three lines up, held through new output: {g:#?}"
    );
    s.view(ViewOp::Wheel(3)).unwrap();
    s.view(ViewOp::Wheel(3)).unwrap();
    assert!(!s.paused(), "the bottom follows the tail again");
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(grid(&replay(&buf, 12, 40))[7], "line-001", "the tail");
}

/// A drag held at the viewport's top row scrolls a line a step, so the selection runs three
/// screens back; the release copies every line from there down to the press, in order, plain.
#[test]
fn a_drag_past_the_top_edge_selects_across_three_screens() {
    let (mut s, _buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(100)));
    s.draw_frame(&frame(40), None).unwrap();
    // The viewport is rows 0–7 (lines 92–99); press on line-098, col 0.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 6,
            clicks: 1,
        },
    );
    for _ in 0..24 {
        select(&mut s, SelectOp::Drag { col: 0, row: 0 });
        s.draw_frame(&frame(40), None).unwrap();
    }
    select(&mut s, SelectOp::Release);
    let text = s.take_copied().expect("the drag copies");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() > 24, "three viewports' worth: {}", lines.len());
    let want: Vec<String> = (99 - lines.len()..99)
        .map(|i| format!("line-{i:03}"))
        .collect();
    assert_eq!(&lines[..lines.len() - 1], &want[..want.len() - 1]);
    assert_eq!(lines[lines.len() - 1], "l", "the press's own cell ends it");
    assert!(!text.contains('\x1b'), "plain text");
    assert!(s.take_copied().is_none(), "copied once");
}

/// A drag below the viewport scrolls toward the tail and follows it at the bottom.
#[test]
fn a_drag_into_the_frame_scrolls_down_to_the_tail() {
    let (mut s, _buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(100)));
    s.draw_frame(&frame(40), None).unwrap();
    for _ in 0..4 {
        s.view(ViewOp::HalfUp).unwrap();
    }
    s.draw_frame(&frame(40), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 0,
            clicks: 1,
        },
    );
    for _ in 0..40 {
        select(&mut s, SelectOp::Drag { col: 39, row: 10 });
        s.draw_frame(&frame(40), None).unwrap();
    }
    select(&mut s, SelectOp::Release);
    let text = s.take_copied().unwrap();
    assert!(text.ends_with("line-099"), "down to the last line: {text}");
    s.draw_frame(&frame(40), None).unwrap();
    assert!(
        !s.paused(),
        "released at the bottom, it follows the tail again"
    );
}

/// The mirror of the drag below: held on the viewport's top row past the session's first line,
/// the edge scroll stops there and the viewport keeps all its rows at every step.
#[test]
fn a_drag_above_the_top_row_keeps_the_viewport_full() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(20)));
    s.draw_frame(&frame(40), None).unwrap();
    // The viewport is rows 0–7 (lines 12–19); press on line-017, col 0.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 5,
            clicks: 1,
        },
    );
    for _ in 0..20 {
        select(&mut s, SelectOp::Drag { col: 0, row: 0 });
        s.draw_frame(&frame(40), None).unwrap();
        let g = grid(&replay(&buf, 12, 40));
        assert!(g[..8].iter().all(|l| l.starts_with("line-")), "{g:#?}");
    }
    let g = grid(&replay(&buf, 12, 40));
    let head: Vec<String> = (0..8).map(|i| format!("line-{i:03}")).collect();
    assert_eq!(&g[..8], &head[..], "at the first line: {g:#?}");
    select(&mut s, SelectOp::Release);
    let text = s.take_copied().expect("the drag copies");
    assert!(text.starts_with("line-000\n"), "{text}");
    assert_eq!(text.lines().count(), 18, "{text}");
}

/// The owner's drag: pressed at the tail, dragged down past the viewport's last row and on into
/// the frame and below it. The viewport keeps all its rows — the last one included — while the
/// pointer is out there, and the selection runs to the last line.
#[test]
fn a_drag_below_the_last_row_keeps_the_viewport_full() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(100)));
    s.draw_frame(&frame(40), None).unwrap();
    let tail: Vec<String> = (92..100).map(|i| format!("line-{i:03}")).collect();
    // The viewport is rows 0–7 (lines 92–99); press on line-094, col 0.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 2,
            clicks: 1,
        },
    );
    for row in [7, 8, 9, 10, 11, 11, 11] {
        select(&mut s, SelectOp::Drag { col: 3, row });
        s.draw_frame(&frame(40), None).unwrap();
        let g = grid(&replay(&buf, 12, 40));
        assert_eq!(&g[..8], &tail[..], "pointer on row {row}: {g:#?}");
    }
    select(&mut s, SelectOp::Release);
    let text = s.take_copied().expect("the drag copies");
    assert_eq!(text, tail[2..].join("\n"));

    // Scrolled back, the edge scroll moves the view down to the tail and stops there, every step
    // a full viewport.
    for _ in 0..4 {
        s.view(ViewOp::HalfUp).unwrap();
    }
    s.draw_frame(&frame(40), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 0,
            clicks: 1,
        },
    );
    for _ in 0..20 {
        select(&mut s, SelectOp::Drag { col: 3, row: 11 });
        s.draw_frame(&frame(40), None).unwrap();
        let g = grid(&replay(&buf, 12, 40));
        assert!(g[..8].iter().all(|l| l.starts_with("line-")), "{g:#?}");
    }
    let g = grid(&replay(&buf, 12, 40));
    assert_eq!(&g[..8], &tail[..], "at the tail: {g:#?}");
    select(&mut s, SelectOp::Release);
    assert!(s.take_copied().unwrap().ends_with("line-099"));
}

/// A double-click takes the word under it; a triple-click the whole row; a plain click selects
/// and copies nothing.
#[test]
fn a_double_click_copies_the_word_and_a_click_nothing() {
    let (mut s, _buf) = mouse_screen(40, 12);
    s.push(rows_of(&["see src/ui/mod.rs:12. now".to_owned()]));
    s.draw_frame(&frame(40), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 9,
            row: 7,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied(), None, "a plain click");
    select(
        &mut s,
        SelectOp::Press {
            col: 9,
            row: 7,
            clicks: 2,
        },
    );
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied().as_deref(), Some("src/ui/mod.rs:12"));
    select(
        &mut s,
        SelectOp::Press {
            col: 9,
            row: 7,
            clicks: 3,
        },
    );
    select(&mut s, SelectOp::Release);
    assert_eq!(
        s.take_copied().as_deref(),
        Some("see src/ui/mod.rs:12. now")
    );
    // A press on a blank row above a short conversation selects nothing.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 0,
            clicks: 2,
        },
    );
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied(), None);
}

/// The selection is drawn on the screen's cells only: shown in reverse video, and a write-out
/// after it carries the rows as committed — no reverse video, no selection at all.
#[test]
fn the_selection_is_drawn_reversed_and_never_written_out() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(8)));
    s.draw_frame(&frame(40), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 2,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 7, row: 4 });
    s.draw_frame(&frame(40), None).unwrap();
    let p = replay(&buf, 12, 40);
    let inv = |row, col| p.screen().cell(row, col).is_some_and(vt100::Cell::inverse);
    assert!(inv(2, 0) && inv(3, 7) && inv(4, 7), "drawn reversed");
    assert!(!inv(1, 0) && !inv(4, 8) && !inv(5, 0), "only the selection");
    let mark = buf.bytes().len();
    s.view(ViewOp::Dump).unwrap();
    let out = buf.bytes()[mark..].to_vec();
    assert!(has(&out, b"line-002") && has(&out, b"line-004"));
    assert!(!has(&out, b"\x1b[7m"), "no reverse video in the write-out");
    assert_eq!(s.take_copied(), None, "a write-out copies nothing");
}

/// A drag that starts while the view follows holds it: output arriving mid-drag moves nothing
/// under the pointer, so the drag selects the text it was pressed and moved over. Released, the
/// view follows the tail again if it still shows the last line; otherwise it stays, scrolled back.
#[test]
fn a_drag_holds_a_following_view_until_the_release() {
    let (mut s, buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(20)));
    s.draw_frame(&frame(40), None).unwrap();
    // Rows 0–7 show line-012…line-019.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 4,
            clicks: 1,
        },
    );
    s.push(rows_of(&[
        "new-a".to_owned(),
        "new-b".to_owned(),
        "new-c".to_owned(),
    ]));
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(
        grid(&replay(&buf, 12, 40))[0],
        "line-012",
        "held, not scrolled"
    );
    assert!(
        grid(&replay(&buf, 12, 40))[7].contains("3 new lines"),
        "the way-back row counts what arrived below"
    );
    select(&mut s, SelectOp::Drag { col: 7, row: 6 });
    select(&mut s, SelectOp::Release);
    assert_eq!(
        s.take_copied().as_deref(),
        Some("line-016\nline-017\nline-018")
    );
    s.draw_frame(&frame(40), None).unwrap();
    assert!(
        s.paused(),
        "output arrived below: the view stays on what was selected"
    );
    s.view(ViewOp::Bottom).unwrap();
    s.draw_frame(&frame(40), None).unwrap();
    assert_eq!(grid(&replay(&buf, 12, 40))[7], "new-c");
    // With nothing new below, the release follows the tail again at once.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 4,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 7, row: 5 });
    select(&mut s, SelectOp::Release);
    s.draw_frame(&frame(40), None).unwrap();
    assert!(!s.paused(), "released at the bottom, it follows");
}

/// A press that jitters a cell before the release is a click: nothing is selected and the
/// clipboard keeps what it had. Two columns, or another line, is a drag.
#[test]
fn a_one_cell_jitter_copies_nothing() {
    let (mut s, _buf) = mouse_screen(40, 12);
    s.push(rows_of(&rows(8)));
    s.draw_frame(&frame(40), None).unwrap();
    select(
        &mut s,
        SelectOp::Press {
            col: 3,
            row: 2,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 4, row: 2 });
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied(), None, "a jitter");
    select(
        &mut s,
        SelectOp::Press {
            col: 3,
            row: 2,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 5, row: 2 });
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied().as_deref(), Some("e-0"));
}

/// Below the viewport the edge scroll speeds up with the distance: three rows into the frame
/// moves seven lines a step, the viewport's last row one.
#[test]
fn the_edge_scroll_speeds_up_with_the_distance_below_the_viewport() {
    let top_after = |row: u16| {
        let (mut s, buf) = mouse_screen(40, 12);
        s.push(rows_of(&rows(100)));
        s.draw_frame(&frame(40), None).unwrap();
        s.view(ViewOp::Top).unwrap();
        s.draw_frame(&frame(40), None).unwrap();
        select(
            &mut s,
            SelectOp::Press {
                col: 0,
                row: 0,
                clicks: 1,
            },
        );
        select(&mut s, SelectOp::Drag { col: 0, row });
        s.draw_frame(&frame(40), None).unwrap();
        grid(&replay(&buf, 12, 40))[0].clone()
    };
    assert_eq!(top_after(7), "line-001", "the last row: one line");
    assert_eq!(top_after(10), "line-007", "three below: seven lines");
}

/// Copying laid-out blocks (S7a review B1): a drag over a code block and a table copies them line
/// for line, as shown — a laid-out block is many lines, not one row wrapped into segments — and a
/// triple-click inside one copies the one line under the pointer. A row the screen wraps still
/// copies back as the one line it was committed as.
#[test]
fn a_drag_over_a_code_block_and_a_table_copies_them_line_for_line() {
    let (mut s, _buf) = mouse_screen(60, 20);
    let long = format!("tail-{}", "w".repeat(90));
    let mut blocks = crate::ui::testutil::markdown_blocks(
        "```\nfn main() {\n    body();\n}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
    );
    blocks.push(crate::ui::block::Block::Row(long.clone()));
    s.push(blocks);
    s.draw_frame(&frame(60), None).unwrap();
    // The viewport is rows 0–15 and the conversation sits on the frame: 3 code lines, a blank, a
    // 5-line table, the long row's 2 segments — 11 lines, rows 5–15.
    select(
        &mut s,
        SelectOp::Press {
            col: 0,
            row: 5,
            clicks: 1,
        },
    );
    select(&mut s, SelectOp::Drag { col: 59, row: 15 });
    select(&mut s, SelectOp::Release);
    let text = s.take_copied().expect("the drag copies");
    assert_eq!(
        text,
        [
            "  fn main() {",
            "      body();",
            "  }",
            "",
            "┌─────┬─────┐",
            "│ a   │ b   │",
            "├─────┼─────┤",
            "│ 1   │ 2   │",
            "└─────┴─────┘",
            &long,
        ]
        .join("\n")
    );
    // A triple-click on the code block's middle line copies that line alone.
    select(
        &mut s,
        SelectOp::Press {
            col: 3,
            row: 6,
            clicks: 3,
        },
    );
    select(&mut s, SelectOp::Release);
    assert_eq!(s.take_copied().as_deref(), Some("      body();"));
}
