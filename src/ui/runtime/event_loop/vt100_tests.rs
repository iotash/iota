#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP44 L2b — terminal semantics via vt100 (`TUI_TEST_PLAN` §L2b) plus the loop units.
//!
//! Drives the REAL loop writer stack (`Term` over iota's own `InlineTerminal`, a shared
//! byte buffer, geometry answered synthetically) and feeds the emitted bytes to a
//! `vt100::Parser`, and proves the insert byte shape (`IL` from the cursor, no scroll
//! region, no per-insert erase — the no-flicker mechanism) of the one shipped build.
//! The W10 `IDLE_WAKE` liveness units and the W4 snapshot-implies-draw unit (the T-03
//! replacement) live here too — they assert on the real byte stream.

use std::io::{self};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::ui::facade::{ProgressState, StatusData};
use crate::ui::render::region::RegionSnapshot;
use crate::ui::runtime::event_loop::Model;
use crate::ui::runtime::msgs::UiMsg;
use crate::ui::testutil::{ChannelEvents, SPINNER_GLYPHS, SharedBuf, test_model};
use crossterm::event::Event;
use tokio_util::sync::CancellationToken;

/// CSI-final byte counters (the spike's `CountWriter` idea, made assertions):
/// DECSTBM set/reset, scrolls, erase-display, insert-line, absolute moves.
#[derive(Default, Debug)]
struct EscCounts {
    sr_set: usize,
    sr_reset: usize,
    up: usize,
    down: usize,
    ed: usize,
    il: usize,
    cup: usize,
}

fn scan(bytes: &[u8]) -> EscCounts {
    let mut c = EscCounts::default();
    let mut st = 0u8;
    let mut saw_semi = false;
    for &b in bytes {
        match st {
            0 => {
                if b == 0x1b {
                    st = 1;
                }
            }
            1 => {
                if b == b'[' {
                    st = 2;
                    saw_semi = false;
                } else {
                    st = 0;
                }
            }
            _ => {
                if b == b';' {
                    saw_semi = true;
                } else if (0x40..=0x7e).contains(&b) {
                    match b {
                        b'r' if saw_semi => c.sr_set += 1,
                        b'r' => c.sr_reset += 1,
                        b'S' => c.up += 1,
                        b'T' => c.down += 1,
                        b'J' => c.ed += 1,
                        b'L' => c.il += 1,
                        b'H' => c.cup += 1,
                        _ => {}
                    }
                    st = 0;
                }
            }
        }
    }
    c
}

fn parse(buf: &SharedBuf) -> vt100::Parser {
    let mut p = vt100::Parser::new(24, 80, 500);
    p.process(&buf.bytes());
    p
}

/// [`parse`], with the emulator moving the cursor `shift` rows (negative = up) at byte
/// `mark` — the move a reflow makes and `Geometry::shift_cursor` tells the synthetic DSR:
/// a resize carries the cursor with its cell, and every byte after it is counted from there.
fn parse_shifted(buf: &SharedBuf, mark: usize, shift: i32) -> vt100::Parser {
    let bytes = buf.bytes();
    let mut p = vt100::Parser::new(24, 80, 500);
    p.process(&bytes[..mark]);
    let n = shift.unsigned_abs();
    if shift < 0 {
        p.process(format!("\u{1b}[{n}A").as_bytes());
    } else if shift > 0 {
        p.process(format!("\u{1b}[{n}B").as_bytes());
    }
    p.process(&bytes[mark..]);
    p
}

/// A headless [`crate::ui::runtime::term::Term`] over a fresh shared buffer.
fn direct_term(
    view_height: u16,
    top: u16,
) -> (
    crate::ui::runtime::term::Term<SharedBuf>,
    SharedBuf,
    crate::ui::runtime::term::Geometry,
) {
    let geo = crate::ui::runtime::term::Geometry::new(80, 24);
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let t = crate::ui::runtime::term::Term::new(
        Box::new(move || wtr.clone()),
        view_height,
        top,
        Some(geo.clone()),
    )
    .unwrap();
    (t, buf, geo)
}

/// The running loop under test: real `run_loop` on its own thread, scripted events,
/// synthetic geometry, all bytes captured.
struct LoopHarness {
    tx: mpsc::Sender<UiMsg>,
    etx: mpsc::Sender<Event>,
    buf: SharedBuf,
    geo: crate::ui::runtime::term::Geometry,
    width: Arc<AtomicU16>,
    height: Arc<AtomicU16>,
    region: Arc<Mutex<crate::ui::render::region::Region>>,
    join: thread::JoinHandle<io::Result<()>>,
}

fn start_loop() -> LoopHarness {
    // start_top 19 = screen_h − the idle frame height (5 rows): the viewport starts
    // at the bottom, so the composer cursor row is constant from the first insert.
    start_loop_at(19)
}

/// [`start_loop`] with the viewport starting on row `start_top`.
fn start_loop_at(start_top: u16) -> LoopHarness {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let (tx, rx) = mpsc::channel();
    let region = Arc::new(Mutex::new(crate::ui::render::region::Region::new(
        crate::ui::render::region::Emit::Live {
            tx: Box::new(crate::ui::runtime::msgs::MailboxPublish(tx.clone())),
        },
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    let shared = crate::ui::runtime::event_loop::LoopShared {
        width: Arc::clone(&width),
        height: Arc::clone(&height),
        region: Arc::clone(&region),
    };
    let geo = crate::ui::runtime::term::Geometry::new(80, 24);
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let t = crate::ui::runtime::term::Term::new(
        Box::new(move || wtr.clone()),
        1,
        start_top,
        Some(geo.clone()),
    )
    .unwrap();
    let (etx, erx) = mpsc::channel();
    let events = ChannelEvents::new(erx);
    let join =
        thread::spawn(move || crate::ui::runtime::event_loop::run_loop(&rx, events, t, shared));
    LoopHarness {
        tx,
        etx,
        buf,
        geo,
        width,
        height,
        region,
        join,
    }
}

impl LoopHarness {
    fn contents(&self) -> String {
        parse(&self.buf).screen().contents()
    }

    fn cursor(&self) -> (u16, u16) {
        parse(&self.buf).screen().cursor_position()
    }

    fn wait_until(&self, timeout: Duration, pred: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred(self) {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        pred(self)
    }

    fn quit_and_join(self, timeout: Duration) {
        let _ = self.tx.send(UiMsg::Quit);
        let deadline = Instant::now() + timeout;
        while !self.join.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(self.join.is_finished(), "the loop thread did not terminate");
        self.join
            .join()
            .expect("loop thread panicked")
            .expect("loop io error");
    }
}

/// W10 `IDLE_WAKE` unit 1: a mailbox message posted while the loop is FULLY idle (no
/// events, no timers) renders within one finite poll deadline — no wake channel needed.
#[test]
fn idle_wake_renders_mailbox_message_within_deadline() {
    let h = start_loop();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')),
        "initial frame never painted:\n{}",
        h.contents()
    );
    thread::sleep(Duration::from_millis(200)); // deep idle: several deadlines elapse
    h.tx.send(UiMsg::Status(StatusData {
        model: "wake-check".to_owned(),
        ..StatusData::default()
    }))
    .unwrap();
    // The 500ms bound is 10× the 50ms deadline — generous for CI, far below human
    // perception; an infinite idle poll would fail this forever.
    assert!(
        h.wait_until(Duration::from_millis(500), |h| h
            .contents()
            .contains("wake-check")),
        "idle mailbox message starved (W10):\n{}",
        h.contents()
    );
    h.quit_and_join(Duration::from_secs(2));
}

/// W10 `IDLE_WAKE` unit 2: `Quit` posted while fully idle terminates the loop within
/// finite deadlines (the `ui.close()`-at-idle deadlock regression).
#[test]
fn quit_at_idle_terminates_the_loop() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    thread::sleep(Duration::from_millis(150)); // fully idle
    h.quit_and_join(Duration::from_secs(1));
}

/// The exit paints one last frame. `close()` flushes the staging window into scrollback and
/// THEN posts `Quit`, on the one mailbox; the frame that still showed the window's rows has
/// to be repainted without them — Go's renderer flushed its last `View()` on stop — or the
/// screen keeps them twice: once in the scrollback the flush sent them to, once more in the
/// frame left standing. In the wild: the banner, at the end of a short chat.
#[test]
fn quit_repaints_the_frame_without_the_flushed_window() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    h.region
        .lock()
        .unwrap()
        .commit(vec!["banner-row".to_owned()]);
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h
            .contents()
            .contains("banner-row")),
        "the staged row never rendered:\n{}",
        h.contents()
    );
    // What `close()` does: flush, then Quit.
    h.region.lock().unwrap().flush();
    let buf = h.buf.clone();
    h.quit_and_join(Duration::from_secs(2));
    let screen = parse(&buf).screen().contents();
    assert_eq!(
        screen.matches("banner-row").count(),
        1,
        "the flushed row is on the screen twice — the frame was not repainted on exit:\n{screen}"
    );
    // The frame itself stays: the composer row, once, under the row it used to show.
    assert_eq!(screen.matches('❯').count(), 1, "{screen}");
    let row_of = |needle: &str| screen.lines().position(|l| l.contains(needle));
    assert!(row_of("banner-row") < row_of("❯"), "{screen}");
}

/// W4 `DRAW_WITH_INSERTS`: an iteration containing an insert batch ends with a draw +
/// cursor restore — the composer cursor cell is IDENTICAL before and after each
/// insert, and the idle frame carries no spinner glyph. Replaces Go's
/// `TestRegionSnapshotChangesView` cursor-bump mechanism (T-03 divergence).
// The mechanism replaced Go's snapshot message (T-03).
#[test]
fn snapshot_implies_draw_and_cursor_restored() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h.cursor().1 == 2),
        "composer cursor never settled at prompt col 2: {:?}",
        h.cursor()
    );
    let pos0 = h.cursor();

    for line in ["insert-alpha", "insert-beta"] {
        h.tx.send(UiMsg::Scrollback(vec![line.to_owned()])).unwrap();
        h.tx.send(UiMsg::Region(RegionSnapshot::default())).unwrap();
        assert!(
            h.wait_until(Duration::from_secs(1), |h| h.contents().contains(line)
                && h.cursor() == pos0),
            "{line}: insert landed but the cursor was not restored to {pos0:?} \
             (now {:?}) — the insert iteration must end with a draw",
            h.cursor()
        );
    }
    assert!(
        !h.contents().chars().any(|c| SPINNER_GLYPHS.contains(c)),
        "idle frame must carry no spinner glyph:\n{}",
        h.contents()
    );
    h.quit_and_join(Duration::from_secs(2));
}

/// W5 `RESIZE_PASS_FIRST` + the retrim law: a resize keeps the staging window IN the
/// re-anchored frame — no flush on a WIDTH change (it only committed the rows a second
/// time under the reflow's ghost), the open preview SURVIVES — and `region.retrim()`
/// runs as the post-update job for either dimension.
#[test]
fn resize_keeps_the_staging_tail_in_the_frame_loop() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    {
        let mut r = h.region.lock().unwrap();
        r.commit(vec!["tail-a".to_owned(), "tail-b".to_owned()]);
        r.open_preview("rendering…");
    }
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h.contents().contains("tail-a")),
        "staged tail never rendered:\n{}",
        h.contents()
    );

    // WIDTH change → the tail stays staged and on screen; the preview survives.
    h.geo.set_size(70, 24);
    h.etx.send(Event::Resize(70, 24)).unwrap();
    assert!(
        h.wait_until(Duration::from_secs(1), |h| {
            let got = h.width.load(Ordering::Relaxed);
            got <= 70 && got + crate::ui::runtime::event_loop::DRAG_MARGIN_MAX >= 70
        }),
        "width never stored"
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        h.region.lock().unwrap().tail,
        vec!["tail-a".to_owned(), "tail-b".to_owned()],
        "a width change must not flush the staging tail"
    );
    assert!(
        !h.region.lock().unwrap().label.is_empty(),
        "the open preview must survive the resize"
    );

    // HEIGHT-only change → the same: nothing flushed.
    h.region.lock().unwrap().commit(vec!["tail-c".to_owned()]);
    h.geo.set_size(70, 20);
    h.etx.send(Event::Resize(70, 20)).unwrap();
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h.height.load(Ordering::Relaxed)
            == 20),
        "height never stored"
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        h.region.lock().unwrap().tail,
        vec![
            "tail-a".to_owned(),
            "tail-b".to_owned(),
            "tail-c".to_owned()
        ],
        "a height-only change within the cap must not flush the tail"
    );
    let screen = h.contents();
    assert_eq!(
        screen.matches("tail-a").count(),
        1,
        "the staged row must be on screen exactly once:\n{screen}"
    );
    h.quit_and_join(Duration::from_secs(2));
}

/// Streaming inserts ride `IL` counted from the cursor (the frame moves down under the new
/// rows; the screen scrolls with `LF`, into the native history), NO scroll region is ever
/// set, and NO per-insert erase appears — erase-display comes only from the frame's height
/// changes, and those erase only the rows below the rows the frame keeps. After the one
/// anchor at startup no byte names an absolute row (`inline-resize-owned`, B).
#[test]
fn inserts_ride_insert_line_with_no_region_and_no_per_insert_ed() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let mark = h.buf.bytes().len();
    for i in 0..8 {
        h.tx.send(UiMsg::Scrollback(vec![format!("line-{i:02}")]))
            .unwrap();
    }
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("line-07")),
        "inserts never landed:\n{}",
        h.contents()
    );
    let bytes = h.buf.bytes();
    let all = scan(&bytes);
    let counts = scan(&bytes[mark..]);
    h.quit_and_join(Duration::from_secs(2));
    assert!(
        counts.il >= 4,
        "insert-line traffic missing: {counts:?} for 8 inserts"
    );
    assert_eq!(
        (all.sr_set, all.sr_reset),
        (0, 0),
        "a scroll region was set: {all:?}"
    );
    assert!(
        counts.ed <= 1,
        "per-insert erases leaked (flicker mechanism): {counts:?} for 8 inserts"
    );
    assert_eq!(
        all.cup, 1,
        "an absolute move past the startup anchor: {all:?}"
    );
}

/// T-01 characterization: a line whose width is EXACTLY the terminal width inserts as
/// exactly ONE row with its last column intact — the `sanitizeOverflow` hazard class
/// does not exist under W6 `LINE_COUNT_SELF_CONSISTENCY`.
#[test]
fn exact_width_line_inserts_single_row() {
    let (mut t, buf, _geo) = direct_term(4, 20);
    let full = "x".repeat(80);
    let n = t.insert_lines(std::slice::from_ref(&full)).unwrap();
    assert_eq!(n, 1, "exact-width line measured as {n} rows");
    let p = parse(&buf);
    let contents = p.screen().contents();
    let hits = contents.lines().filter(|l| *l == full).count();
    assert_eq!(
        hits, 1,
        "exact-width line must land as exactly one row:\n{contents}"
    );
    let row = contents.lines().position(|l| l == full).unwrap();
    let cell = p.screen().cell(u16::try_from(row).unwrap(), 79).unwrap();
    assert_eq!(cell.contents(), "x", "right border eaten (T-01)");
}

/// T-06 characterization: a single insert TALLER than the screen. ratatui documents
/// that overflowing top lines go straight into scrollback; this records the observed
/// behavior (order preserved, nothing lost) — the region's `max(2, h/2)` chunking
/// stays as insurance regardless.
#[test]
fn over_screen_height_insert_characterization() {
    let (mut t, buf, _geo) = direct_term(4, 20);
    let rows: Vec<String> = (0..30).map(|i| format!("tall-{i:02}")).collect();
    t.insert_lines(&rows).unwrap();
    assert_eq!(t.top(), 20, "the frame stays pinned at the bottom");

    let p = parse(&buf);
    let visible = p.screen().contents();
    assert!(
        visible.contains("tall-29"),
        "last inserted row missing from the screen:\n{visible}"
    );
    // Visible subset in ascending order.
    let seq: Vec<usize> = visible
        .lines()
        .filter_map(|l| l.strip_prefix("tall-").and_then(|n| n.parse().ok()))
        .collect();
    assert!(
        seq.windows(2).all(|w| w[0] < w[1]),
        "visible insert order broken: {seq:?}"
    );

    // Scrollback reach used to be the wart-W9 story: the scrolling-regions path scrolled the
    // partial region 0..top, and a strict-DEC emulator — this vt100 crate — DISCARDED the
    // rows scrolled out of it. Inserts scroll the WHOLE screen now (`LF` from the cursor), the
    // way a shell does, so every row reaches the history — in order, exactly once.
    let mut p = parse(&buf);
    let all = reachable(&mut p);
    let got: Vec<usize> = all
        .iter()
        .filter_map(|l| {
            l.strip_prefix("tall-")
                .and_then(|n| n.trim_end().parse().ok())
        })
        .collect();
    assert_eq!(
        got,
        (0..30).collect::<Vec<_>>(),
        "every row reachable once, in order:\n{}",
        all.join("\n")
    );
}

/// DEC 2026 (X-55): the pairs of `ESC[?2026h` / `ESC[?2026l` in `bytes`, and whether every
/// begin is closed before the next one opens.
fn sync_pairs(bytes: &[u8]) -> (usize, bool) {
    let s = String::from_utf8_lossy(bytes);
    let (mut open, mut pairs, mut nested) = (false, 0, false);
    for (i, _) in s.match_indices("\u{1b}[?2026") {
        match s[i + 7..].chars().next() {
            Some('h') => {
                nested |= open;
                open = true;
            }
            Some('l') => {
                nested |= !open;
                open = false;
                pairs += 1;
            }
            _ => {}
        }
    }
    (pairs, !nested && !open)
}

/// DEC 2026 by SIZE (X-55): a write the transport cannot split — at most `SYNC_MIN` bytes, one
/// pty read — goes out bare: a keystroke's composer redraw, a one-line insert, a height change,
/// a clear, a whole small batch, an OSC. A write larger than that goes out as exactly ONE
/// synchronized update around all its bytes: a 12-line insert of wide rows, a `/model`-sized
/// frame opening (the height change and the draw in one batch). The criterion is the size of
/// what one `write_all` carries, never the terminal; vt100 ignores the mode, so every replay
/// in this file runs through the same bytes.
#[test]
fn a_write_is_one_synchronized_update_exactly_when_the_transport_would_split_it() {
    use crate::ui::runtime::inline_term::SYNC_MIN;
    let (mut t, buf, _geo) = direct_term(4, 20);
    let view = |rows: Vec<String>, cursor: (u16, u16)| crate::ui::render::frame::FrameView {
        rows,
        cursor: Some(cursor),
    };
    let composer = |draft: &str| {
        view(
            vec![
                "┄".repeat(80),
                format!("❯ {draft}"),
                "┄".repeat(80),
                "status".to_owned(),
            ],
            (2 + u16::try_from(draft.len()).unwrap(), 1),
        )
    };
    // One write, its size, its pairs; the rule: wrapped exactly when longer than SYNC_MIN.
    let write = |t: &mut crate::ui::runtime::term::Term<SharedBuf>,
                 op: &dyn Fn(&mut crate::ui::runtime::term::Term<SharedBuf>)|
     -> (usize, usize) {
        let mark = buf.bytes().len();
        op(t);
        let bytes = buf.bytes()[mark..].to_vec();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (pairs, balanced) = sync_pairs(&bytes);
        assert!(balanced, "{text:?}");
        let body = bytes.len() - pairs * 16;
        if body > SYNC_MIN {
            assert_eq!(pairs, 1, "a {body}-byte write must be one block: {text:?}");
            assert!(
                text.starts_with("\u{1b}[?2026h") && text.ends_with("\u{1b}[?2026l"),
                "the block holds the whole write: {text:?}"
            );
        } else {
            assert_eq!(pairs, 0, "a {body}-byte write goes out bare: {text:?}");
        }
        (body, pairs)
    };
    t.draw_frame(&composer("")).unwrap();
    // Small: a keystroke, a one-line insert, a height change, a clear, a small batch, an OSC.
    let (n, pairs) = write(&mut t, &|t| t.draw_frame(&composer("h")).unwrap());
    assert!(
        pairs == 0 && n < 64,
        "a keystroke echo is a few bytes, bare: {n} B"
    );
    let (_, pairs) = write(&mut t, &|t| {
        t.insert_lines(&["one line of output".to_owned()]).unwrap();
    });
    assert_eq!(pairs, 0, "a one-line insert is bare");
    write(&mut t, &|t| {
        t.ensure_height(6).unwrap();
    });
    write(&mut t, &|t| t.clear().unwrap());
    let (_, pairs) = write(&mut t, &|t| {
        t.begin_batch();
        t.ensure_height(4).unwrap();
        t.set_title("small").unwrap();
        t.set_progress(ProgressState::Busy).unwrap();
        t.draw_frame(&composer("hi")).unwrap();
        t.end_batch().unwrap();
    });
    assert_eq!(pairs, 0, "a small batch is bare, the OSCs in it");
    let (_, pairs) = write(&mut t, &|t| t.set_title("bare").unwrap());
    assert_eq!(pairs, 0, "an OSC alone is bare");
    // Large: a 12-line insert of wide rows; a /model-sized frame opening in one batch.
    let wide: Vec<String> = (0..12)
        .map(|i| {
            format!(
                "{i:02} \u{1b}[31m{}\u{1b}[0m \u{1b}[32m{}\u{1b}[0m \u{1b}[1m{}\u{1b}[0m",
                "x".repeat(24),
                "y".repeat(24),
                "z".repeat(24)
            )
        })
        .collect();
    let (n, pairs) = write(&mut t, &|t| {
        t.insert_lines(&wide).unwrap();
    });
    assert!(
        pairs == 1 && n > SYNC_MIN,
        "a 12-line insert is one block: {n} B"
    );
    let panel: Vec<String> = (0..16)
        .map(|i| format!("  model-{i:02} {}", "·".repeat(60)))
        .collect();
    let (n, pairs) = write(&mut t, &|t| {
        t.begin_batch();
        t.ensure_height(16).unwrap();
        t.draw_frame(&view(panel.clone(), (2, 1))).unwrap();
        t.end_batch().unwrap();
    });
    assert!(
        pairs == 1 && n > SYNC_MIN,
        "a panel opening is one block: {n} B"
    );
    let screen = parse(&buf).screen().contents();
    assert!(
        screen.contains("model-15") && screen.contains("11 xxx"),
        "{screen}"
    );
}

/// No cursor query is ever made inside a synchronized update: `WezTerm` holds its answer and
/// Alacritty the query itself until the block ends (or a timeout), so a resize pass would
/// stall or read a stale cursor. Every query of a pass is checked where it is answered.
#[test]
fn no_cursor_query_sits_inside_a_synchronized_update() {
    fn arm(
        geo: &crate::ui::runtime::term::Geometry,
        buf: &SharedBuf,
        seen: &Arc<Mutex<Vec<bool>>>,
    ) {
        let (g, b, s) = (geo.clone(), buf.clone(), Arc::clone(seen));
        geo.after_query(0, move || {
            s.lock().unwrap().push(sync_pairs(&b.bytes()).1);
            arm(&g, &b, &s);
        });
    }
    let (mut t, buf, geo) = direct_term(5, 19);
    t.draw_frame(&crate::ui::render::frame::FrameView {
        rows: (0..5).map(|i| format!("row-{i}")).collect(),
        cursor: Some((2, 2)),
    })
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    arm(&geo, &buf, &seen);
    for (w, h) in [(60, 24), (60, 20), (80, 24)] {
        geo.set_size(w, h);
        t.resize(
            ratatui::layout::Size {
                width: w,
                height: h,
            },
            5,
        )
        .unwrap();
    }
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() >= 6, "two queries a pass: {seen:?}");
    assert!(
        seen.iter().all(|closed| *closed),
        "a query inside a block: {seen:?}"
    );
}

/// A height change is a field write and a repaint of what changed (`inline-resize-owned`,
/// B): no recreation, no clear. A surface opening below the composer (the frame grows by two
/// rows at the bottom of the screen) and closing again erases only the rows below the rows
/// the frame keeps — never one it keeps — and rewrites none of them: the separator, the
/// composer and the status row are not written at all, so nothing on screen blinks. Under
/// W1+W3 each change erased the frame from its first row and repainted every cell.
#[test]
fn a_height_change_clears_nothing_it_keeps_and_rewrites_nothing_unchanged() {
    let (mut t, buf, _geo) = direct_term(4, 20);
    let rows = |extra: &[&str]| crate::ui::render::frame::FrameView {
        rows: ["SEP-top", "❯ draft", "SEP-bottom", "status"]
            .iter()
            .chain(extra)
            .map(|r| (*r).to_owned())
            .collect(),
        cursor: Some((7, 1)),
    };
    t.draw_frame(&rows(&[])).unwrap();
    let kept = ["SEP-top", "draft", "SEP-bottom", "status"];
    for (h, extra) in [(6, &["pick-1", "pick-2"][..]), (4, &[][..])] {
        let mark = buf.bytes().len();
        assert!(t.ensure_height(h).unwrap(), "the height changed to {h}");
        t.draw_frame(&rows(extra)).unwrap();
        let bytes = buf.bytes();
        let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
        assert!(
            !after.contains("\u{1b}[2J"),
            "a clear screen at h={h}:\n{after:?}"
        );
        assert_eq!(
            scan(&bytes[mark..]).ed,
            1,
            "one erase, of the rows below the kept ones:\n{after:?}"
        );
        for k in kept {
            assert!(
                !after.contains(k),
                "the unchanged row {k:?} was written again at h={h}:\n{after:?}"
            );
        }
        let screen = parse(&buf).screen().contents();
        let lines: Vec<&str> = screen.lines().collect();
        // The taller frame scrolled the screen by two rows; the shorter one keeps its first
        // row (the next output walks it back down).
        let top = 18;
        assert_eq!(
            lines.get(top..top + 4).map(<[&str]>::to_vec),
            Some(vec!["SEP-top", "❯ draft", "SEP-bottom", "status"]),
            "the kept rows at h={h}:\n{screen}"
        );
        assert!(
            extra.iter().all(|e| screen.contains(e)) && (h > 4 || !screen.contains("pick-")),
            "the rows below at h={h}:\n{screen}"
        );
    }
}

/// The spike's G2 shrink law, in-process: a smaller frame erases the rows it gives up —
/// ZERO ghost rows — and subsequent inserts walk the frame back down.
#[test]
fn shrink_recreation_walks_down_without_ghosts() {
    let (mut t, buf, _geo) = direct_term(4, 18);
    t.ensure_height(10).unwrap();
    let tall = crate::ui::render::frame::FrameView {
        rows: (0..10)
            .map(|i| {
                if i == 5 {
                    "SURFGHOST-row".to_owned()
                } else {
                    format!("row-{i}")
                }
            })
            .collect(),
        cursor: Some((2, 0)),
    };
    t.draw_frame(&tall).unwrap();
    assert!(
        parse(&buf).screen().contents().contains("SURFGHOST-row"),
        "tall frame never painted"
    );

    // Close: the frame gets shorter (the rows it gives up erased), then output walks it down.
    let small = crate::ui::render::frame::FrameView {
        rows: vec![
            String::new(),
            "❯ ".to_owned(),
            String::new(),
            "status".to_owned(),
        ],
        cursor: Some((2, 1)),
    };
    t.ensure_height(4).unwrap();
    t.draw_frame(&small).unwrap();
    t.insert_lines(&["after-one".to_owned()]).unwrap();
    t.draw_frame(&small).unwrap();

    let contents = parse(&buf).screen().contents();
    assert!(
        !contents.contains("SURFGHOST"),
        "ghost rows survived the shrink:\n{contents}"
    );
    assert!(
        contents.contains("after-one"),
        "walk-down insert missing:\n{contents}"
    );
}

// ---------------------------------------------------------------------------
// Loop-model units (no terminal)
// ---------------------------------------------------------------------------

/// The busy state machine: detail rides the phase without touching its clock, a new
/// phase clears stale detail, detail at idle is dropped (the render half lives in
/// `tests/frame_goldens.rs`).
#[test]
fn busy_detail_state_machine() {
    let mut m = test_model();
    m.apply(UiMsg::BusyOn("Composing tool call — write_file".to_owned()));
    let since = m.busy.as_ref().unwrap().since;
    m.apply(UiMsg::BusyDetail("4.2 KB".to_owned()));
    assert_eq!(m.busy.as_ref().unwrap().detail, "4.2 KB");
    assert_eq!(
        m.busy.as_ref().unwrap().since,
        since,
        "detail update must not reset the phase clock"
    );

    m.apply(UiMsg::BusyOn("Compacting context…".to_owned()));
    assert_eq!(
        m.busy.as_ref().unwrap().detail,
        "",
        "a new phase must clear the previous detail"
    );

    m.apply(UiMsg::BusyOff);
    m.apply(UiMsg::BusyDetail("ghost".to_owned()));
    assert!(
        m.busy.is_none(),
        "detail with no busy phase must be dropped"
    );
}

/// W10 pure unit: the poll deadline is ALWAYS finite — `IDLE_POLL_MAX` fully idle,
/// tighter under the spinner/streaming timers, zero while a paint is owed; and the
/// spinner chain stops itself at idle.
#[test]
fn idle_poll_deadline_always_finite() {
    let mut m = test_model();
    assert_eq!(
        m.poll_deadline(),
        Duration::ZERO,
        "a pending first paint must poll at zero"
    );
    m.dirty = false;
    assert_eq!(
        m.poll_deadline(),
        crate::ui::runtime::event_loop::IDLE_POLL_MAX,
        "fully idle must still poll finite (W10)"
    );

    m.apply(UiMsg::BusyOn("x".to_owned()));
    m.dirty = false;
    assert!(m.poll_deadline() <= crate::ui::runtime::event_loop::IDLE_POLL_MAX);
    assert!(m.spin_ticking, "busy must start the spinner chain");

    m.apply(UiMsg::Region(RegionSnapshot {
        label: "rendering…".to_owned(),
        ..RegionSnapshot::default()
    }));
    m.dirty = false;
    assert!(
        m.poll_deadline() <= crate::ui::runtime::event_loop::STREAM_POLL_CAP,
        "streaming must cap the poll deadline"
    );

    m.apply(UiMsg::BusyOff);
    m.apply(UiMsg::Region(RegionSnapshot::default()));
    m.tick_spin();
    assert!(
        !m.spin_ticking,
        "the spinner chain must stop itself at idle"
    );
}

/// The job clock: while a background job runs the loop wakes once a second to repaint the status row's
/// segment — and not at all otherwise, so an idle iota with no job never paints (the stale-frame budgets
/// of scenarios 06 and 14 rest on that).
#[test]
fn job_clock_ticks_only_while_a_job_runs() {
    use crate::ui::runtime::event_loop::{IDLE_POLL_MAX, JOB_TICK};
    let mut m = test_model();
    m.dirty = false;
    m.tick_jobs();
    assert!(!m.dirty, "no job, no repaint");
    assert_eq!(m.poll_deadline(), IDLE_POLL_MAX);

    let job = crate::shell::jobs::JobInfo {
        id: "b1".to_owned(),
        command: "sleep 30".to_owned(),
        pid: None,
        started: Instant::now(),
        output_path: std::path::PathBuf::from("/tmp/b1.log"),
    };
    m.apply(UiMsg::Jobs(vec![job]));
    assert!(m.dirty, "a new running set repaints at once");
    m.dirty = false;
    assert!(
        m.poll_deadline() <= IDLE_POLL_MAX,
        "the deadline stays finite and no later than the idle cap"
    );
    m.tick_jobs();
    assert!(
        !m.dirty,
        "a fresh set is a full tick away from its first repaint"
    );
    // A second later the tick is due: one repaint, then the next one is a second out again.
    m.last_job_tick = Instant::now()
        .checked_sub(JOB_TICK)
        .unwrap_or_else(Instant::now);
    m.tick_jobs();
    assert!(m.dirty, "the clock did not repaint after a second");
    m.dirty = false;
    m.tick_jobs();
    assert!(!m.dirty, "the clock repainted twice in one second");

    m.apply(UiMsg::Jobs(Vec::new()));
    m.dirty = false;
    assert_eq!(
        m.poll_deadline(),
        IDLE_POLL_MAX,
        "the last job gone, the loop is idle again"
    );
    m.last_job_tick = Instant::now()
        .checked_sub(JOB_TICK)
        .unwrap_or_else(Instant::now);
    m.tick_jobs();
    assert!(!m.dirty, "a due tick with no job must not repaint");
}

/// The cancel-scope stack: ESC fires the innermost only; Ctrl+C fires index 0 and
/// truncates the whole stack (the loop-side half; key routing tests are WP45/WP46's).
#[test]
fn cancel_scope_stack_fire_truncates() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    let tool = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    m.apply(UiMsg::ScopePush(tool.clone()));

    m.fire_cancel(1); // ESC: the innermost
    assert!(tool.is_cancelled() && !turn.is_cancelled());
    assert_eq!(m.cancels.len(), 1);

    m.fire_cancel(0); // Ctrl+C: the turn
    assert!(turn.is_cancelled());
    assert!(m.cancels.is_empty());
}

/// Wide graphemes survive an insert. A wide grapheme's continuation cell reads back as a
/// SPACE, so a backend handed the WHOLE buffer (ratatui's `draw_lines` path) would print
/// `中 文 一 行` for a padded CJK row; every write goes through ratatui's width-aware buffer
/// diff (against a blank buffer for an insert), which drops the covered cells.
// A ratatui backend artefact with no Go twin.
#[test]
fn wide_runes_insert_intact() {
    let (mut t, buf, _geo) = direct_term(1, 19);
    // The user-echo shape: a padded, styled row (the padding is what makes the row's
    // trailing cells non-blank, which is where the fallback path used to desync).
    let row = format!("\x1b[7m❯ 中文一行{}\x1b[0m", " ".repeat(80 - 2 - 8));
    t.insert_lines(std::slice::from_ref(&row)).unwrap();
    let screen = parse(&buf).screen().contents();
    assert!(
        screen.contains("❯ 中文一行"),
        "wide runes split by the insert path:\n{screen:?}"
    );
    assert!(
        !screen.contains("中 文"),
        "a continuation cell leaked a spurious space:\n{screen:?}"
    );

    // The unpadded markdown shape must stay intact too.
    let (mut t, buf, _geo) = direct_term(1, 19);
    t.insert_lines(&["中文一行 tail".to_owned()]).unwrap();
    let screen = parse(&buf).screen().contents();
    assert!(
        screen.contains("中文一行 tail"),
        "wide runes split in an unpadded row:\n{screen:?}"
    );
}

// ------------------------------------------------------------------
// T3 — OSC 9;4 progress, OSC 9 notify, focus reporting (WP67)
// ------------------------------------------------------------------

/// A loop model over a test-seam region at 80x24 (the `queue_tests` twin, reused here
/// so the notify gate can be driven without racing the loop thread).
fn notify_model() -> Model {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let region = Arc::new(Mutex::new(crate::ui::render::region::Region::new(
        crate::ui::render::region::Emit::Test(Box::new(|_, _| {})),
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    Model::new(crate::ui::runtime::event_loop::LoopShared {
        width,
        height,
        region,
    })
}

fn text(buf: &SharedBuf) -> String {
    String::from_utf8_lossy(&buf.bytes()).into_owned()
}

// The facade's states reach the
// wire as OSC 9;4 sequences, emitted ON CHANGE only, and `ProgressNone` clears the bar. Go
// asserted the bubbletea `View`'s ProgressBar because its renderer owned emission; the Rust
// loop IS the renderer, so the assertion is the byte stream itself. `\x1b[?1004h` at the
// head proves `run_loop` (not `Term::new`) turned focus reporting on.
#[test]
fn progress_states_reach_the_wire_as_osc_9_4_on_change() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    assert!(
        text(&h.buf).contains("\x1b[?1004h"),
        "focus reporting is off — Notify gating would never see blur"
    );
    assert!(
        !text(&h.buf).contains("]9;4"),
        "an idle loop advertises a progress bar:\n{:?}",
        text(&h.buf)
    );

    for (state, want) in [
        (ProgressState::Busy, "\x1b]9;4;3\x07"),
        (ProgressState::Input, "\x1b]9;4;4;100\x07"),
        (ProgressState::Error, "\x1b]9;4;2;100\x07"),
        (ProgressState::None, "\x1b]9;4;0\x07"),
    ] {
        h.tx.send(UiMsg::Progress(state)).unwrap();
        assert!(
            h.wait_until(Duration::from_secs(1), |h| text(&h.buf).contains(want)),
            "state {state:?} never emitted {want:?}:\n{:?}",
            text(&h.buf)
        );
    }

    // A repeated state emits nothing (the title's emit-on-change law).
    let before = text(&h.buf).matches("\x1b]9;4;0\x07").count();
    h.tx.send(UiMsg::Progress(ProgressState::None)).unwrap();
    h.tx.send(UiMsg::Status(StatusData {
        model: "settled".to_owned(),
        ..StatusData::default()
    }))
    .unwrap();
    assert!(h.wait_until(Duration::from_secs(1), |h| h.contents().contains("settled")));
    assert_eq!(
        text(&h.buf).matches("\x1b]9;4;0\x07").count(),
        before,
        "a repeated state re-emitted its sequence"
    );
    h.quit_and_join(Duration::from_secs(2));
}

// Silent while focused (whoever
// is watching needs no bell), one write carrying BOTH standard channels while blurred (the
// OSC 9 notification, whose BEL only terminates the sequence, then a ringing BEL), and a
// digest opening `"4;"` defused by a leading space so it cannot parse as a progress report.
//
// Driven through `Model` + a real `Term` rather than the loop thread: the gate's whole point
// is the ORDER of a focus event against a ping, which a two-channel harness cannot pin.
#[test]
fn a_notification_rings_only_while_blurred() {
    let (mut t, buf, _geo) = direct_term(1, 19);
    let mut m = notify_model();

    let ping = |m: &mut Model, t: &mut crate::ui::runtime::term::Term<SharedBuf>, s: &str| {
        m.apply(UiMsg::Notify(s.to_owned()));
        crate::ui::runtime::event_loop::drain_notify(m, t).unwrap();
    };

    let before = buf.bytes().len();
    ping(&mut m, &mut t, "approval needed"); // focused (the default): silent
    assert_eq!(
        buf.bytes().len(),
        before,
        "a focused terminal got {:?}, want silence",
        text(&buf)
    );

    m.handle_event(Event::FocusLost);
    ping(&mut m, &mut t, "approval needed");
    let got = String::from_utf8_lossy(&buf.bytes()[before..]).into_owned();
    assert!(
        got.starts_with("\x1b]9;approval needed") && got.ends_with("\x07\x07"),
        "blurred ping = {got:?}, want OSC 9 + BEL"
    );

    let mark = buf.bytes().len();
    ping(&mut m, &mut t, "4; things to fix");
    let got = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert!(
        got.starts_with("\x1b]9; 4;"),
        "collision-prone ping = {got:?}, want a leading space defusing 9;4"
    );

    m.handle_event(Event::FocusGained);
    let mark = buf.bytes().len();
    ping(&mut m, &mut t, "done");
    assert_eq!(
        buf.bytes().len(),
        mark,
        "a refocused terminal got {:?}, want silence",
        text(&buf)
    );
}

/// New (no Go twin — bubbletea's renderer did this at Program close, `cursed_renderer.go:
/// 178-180,212-214`): dropping the `Term` at the end of `run_loop` turns focus reporting
/// back off and clears a bar that is still showing.
#[test]
fn exit_clears_the_progress_bar_and_focus_mode() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    h.tx.send(UiMsg::Progress(ProgressState::Busy)).unwrap();
    assert!(h.wait_until(Duration::from_secs(1), |h| {
        text(&h.buf).contains("\x1b]9;4;3\x07")
    }));
    let buf = h.buf.clone();
    let busy_at = text(&buf).find("\x1b]9;4;3\x07").expect("busy emitted");
    h.quit_and_join(Duration::from_secs(2));

    let out = text(&buf);
    let reset = out
        .rfind("\x1b]9;4;0\x07")
        .expect("the exit must clear the bar");
    let focus_off = out
        .rfind("\x1b[?1004l")
        .expect("the exit must turn focus reporting off");
    assert!(reset > busy_at && focus_off > busy_at, "cleared too early");
}

/// The other half of the exit law: a run that never raised a bar has nothing to clear.
#[test]
fn exit_without_a_bar_emits_no_reset() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let buf = h.buf.clone();
    h.quit_and_join(Duration::from_secs(2));
    let out = text(&buf);
    assert!(!out.contains("]9;4;0"), "cleared a bar that never showed");
    assert!(
        out.contains("\x1b[?1004l"),
        "focus reporting was turned on by run_loop and must be turned off"
    );
}

/// D17: a `Term` built OUTSIDE `run_loop` (the one-shot `--resume` picker, `oneshot.rs:62`,
/// and the harnesses here) never enables focus reporting, so its drop must not disable a
/// mode the process never set.
#[test]
fn a_term_that_never_enabled_focus_reporting_writes_no_disable() {
    let (t, buf, _geo) = direct_term(1, 19);
    drop(t);
    let out = text(&buf);
    assert!(!out.contains("[?1004"), "unexpected focus mode: {out:?}");
    assert!(!out.contains("]9;4"), "unexpected progress bytes: {out:?}");
}

// ---------------------------------------------------------------------------
// W5 resize: iota re-anchors; ratatui's resize (row 0 + ESC[2J) never runs
// ---------------------------------------------------------------------------

/// Every row a user can reach in `p`: the history (oldest first), then the screen.
fn reachable(p: &mut vt100::Parser) -> Vec<String> {
    let cols = p.screen().size().1;
    let screen: Vec<String> = p.screen().rows(0, cols).collect();
    p.screen_mut().set_scrollback(usize::MAX);
    let depth = p.screen().scrollback();
    let mut all = Vec::with_capacity(depth + screen.len());
    for off in (1..=depth).rev() {
        p.screen_mut().set_scrollback(off);
        all.push(p.screen().rows(0, cols).next().unwrap_or_default());
    }
    p.screen_mut().set_scrollback(0);
    all.extend(screen);
    all
}

thread_local! {
    /// Erase-belows the emulator executed with its cursor on the home position — counted by
    /// [`process_counting`] over every replay in this thread, read by the resize asserts.
    static HOME_ERASES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Feeds `bytes` into `p`, counting each erase-below (`CSI J`, `CSI 0J`) the emulator
/// executes on the HOME position: tmux (`scroll-on-clear`, its default) takes that for a clear
/// screen and files the whole screen into the history first. Counted in the emulator, not in
/// the bytes — iota's erases are counted from the cursor, so no byte says where they land.
fn process_counting(p: &mut vt100::Parser, bytes: &[u8]) {
    let mut rest = bytes;
    loop {
        let hit = [b"\x1b[J".as_slice(), b"\x1b[0J".as_slice()]
            .iter()
            .filter_map(|pat| {
                rest.windows(pat.len())
                    .position(|w| w == *pat)
                    .map(|i| (i, pat.len()))
            })
            .min();
        let Some((i, len)) = hit else {
            p.process(rest);
            return;
        };
        p.process(&rest[..i]);
        if p.screen().cursor_position() == (0, 0) {
            HOME_ERASES.with(|c| c.set(c.get() + 1));
        }
        p.process(&rest[i..i + len]);
        rest = &rest[i + len..];
    }
}

/// How many home-position erase-belows the replays in this thread have executed (and reset).
fn home_erases() -> usize {
    HOME_ERASES.with(std::cell::Cell::take)
}

/// How many scroll-downs (`CSI n T`) `bytes` carries: a resize must scroll nothing — the
/// region scroll-down that closed the resize band put blank rows at the top of the
/// screen, and the next reflow pushed them into the history.
fn scroll_downs(bytes: &str) -> usize {
    bytes
        .split("\u{1b}[")
        .skip(1)
        .filter(|seq| {
            let digits = seq.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && seq[digits..].starts_with('T')
        })
        .count()
}

/// Replays `bytes` through an emulator that is resized by `emulate` at byte `mark` —
/// the point the scripted `Event::Resize` was sent — and returns every row the user can
/// reach afterwards and the screen alone. vt100 neither reflows nor archives an
/// `ESC[2J`, so a row that ratatui's narrowing path wiped stays wiped here: exactly the
/// emulators that lost it live (Ghostty, herdr).
fn replay_resized(
    bytes: &[u8],
    mark: usize,
    emulate: impl FnOnce(&mut vt100::Parser),
) -> (Vec<String>, Vec<String>) {
    let mut p = vt100::Parser::new(24, 80, 5000);
    p.process(&bytes[..mark]);
    emulate(&mut p);
    process_counting(&mut p, &bytes[mark..]);
    let cols = p.screen().size().1;
    let screen: Vec<String> = p.screen().rows(0, cols).collect();
    (reachable(&mut p), screen)
}

/// A REFLOWING emulator — tmux 3.7c, and ONLY tmux (hence the name) — a row shrink first eats the
/// rows below the cursor and then moves the top rows into the history; then every screen
/// row is a hard line that rewraps at the new width, the rows stay anchored to the
/// bottom (what grows pushes the top rows into the history), and the cursor moves with
/// its cell. vt100 underneath does the rest; `history` keeps what the model pushed out
/// plus vt100's own scrolled-off rows from each incarnation.
///
/// It is the only reflow model here. herdr and Ghostty behave differently in ways it does not
/// capture — they do not pull history rows back on a grow, and a height drag's band stays as
/// blank rows in their history (X-52's over-budget list) — so that class is found on the real
/// terminals only; a second model is added when a bug needs one, not before.
struct TmuxReflowEmu {
    p: vt100::Parser,
    history: Vec<String>,
    fed: usize,
}

impl TmuxReflowEmu {
    fn new() -> Self {
        Self {
            p: vt100::Parser::new(24, 80, 5000),
            history: Vec::new(),
            fed: 0,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        process_counting(&mut self.p, &bytes[self.fed..]);
        self.fed = bytes.len();
    }

    /// Feeds what the app wrote so far, then resizes to `w`×`h` with a reflow; returns
    /// how many rows the cursor moved (for `Geometry::shift_cursor`).
    fn resize(&mut self, bytes: &[u8], w: u16, h: u16) -> i32 {
        self.feed(bytes);
        let all = reachable(&mut self.p);
        let rows = usize::from(self.p.screen().size().0);
        let (cur_row, cur_col) = self.p.screen().cursor_position();
        let (old_history, screen) = all.split_at(all.len() - rows);
        self.history.extend_from_slice(old_history);
        // tmux resizes the rows FIRST: a shrink eats the rows below the cursor, then
        // takes the rest from the top into the history.
        let mut screen = screen.to_vec();
        let orig_row = i32::from(cur_row);
        let mut cur_row = usize::from(cur_row);
        let needed = rows.saturating_sub(usize::from(h));
        let eat = needed.min(rows - 1 - cur_row);
        screen.truncate(rows - eat);
        let push = needed - eat;
        self.history.extend(screen.drain(..push));
        cur_row -= push;
        // A row GROW pulls lines back out of the history onto the top of the screen (tmux:
        // `screen_resize_y`), moving everything — the cursor included — down.
        let pull = usize::from(h).saturating_sub(rows).min(self.history.len());
        let back = self.history.split_off(self.history.len() - pull);
        screen.splice(0..0, back);
        cur_row += pull;
        let wide = usize::from(w);
        let mut lines: Vec<String> = Vec::new();
        let mut cursor_line = 0;
        for (i, row) in screen.iter().enumerate() {
            let chars: Vec<char> = row.chars().collect();
            let pieces = chars.len().div_ceil(wide).max(1);
            if i == cur_row {
                cursor_line = lines.len() + (usize::from(cur_col) / wide).min(pieces - 1);
            }
            if chars.is_empty() {
                lines.push(String::new());
            } else {
                lines.extend(chars.chunks(wide).map(|c| c.iter().collect::<String>()));
            }
        }
        let height = usize::from(h);
        let pushed = lines.len().saturating_sub(height);
        self.history.extend(lines.drain(..pushed));
        lines.resize(height, String::new());
        let new_row = cursor_line - pushed;
        let mut p = vt100::Parser::new(h, w, 5000);
        for (i, line) in lines.iter().enumerate() {
            p.process(format!("\u{1b}[{};1H{line}", i + 1).as_bytes());
        }
        p.process(format!("\u{1b}[{};{}H", new_row + 1, cur_col + 1).as_bytes());
        self.p = p;
        i32::try_from(new_row).unwrap() - orig_row
    }

    /// The rows in the history so far (what was pushed off the top), after feeding.
    fn history(&mut self, bytes: &[u8]) -> Vec<String> {
        self.feed(bytes);
        let rows = usize::from(self.p.screen().size().0);
        let mut all = self.history.clone();
        let own = reachable(&mut self.p);
        all.extend(own[..own.len() - rows].iter().cloned());
        all
    }

    /// Feeds the rest; returns every reachable row and the screen.
    fn finish(&mut self, bytes: &[u8]) -> (Vec<String>, Vec<String>) {
        self.feed(bytes);
        let cols = self.p.screen().size().1;
        let screen: Vec<String> = self.p.screen().rows(0, cols).collect();
        let mut all = self.history.clone();
        all.extend(reachable(&mut self.p));
        (all, screen)
    }
}

/// The loop with twelve committed lines at idle: four staged in the 9-row frame (four
/// staged rows, spacer, two separators, composer, status), eight above it.
fn idle_loop() -> LoopHarness {
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    lh.region
        .lock()
        .unwrap()
        .commit((0..12).map(|i| format!("line-{i:02}")).collect());
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains("line-11")));
    thread::sleep(Duration::from_millis(150)); // settle: the loop is idle from here
    lh
}

/// Sends one resize and waits for its pass and the retrim job's round to land.
fn resize_and_settle(lh: &LoopHarness, w: u16, h: u16) {
    resize_step(lh, w, h);
    // Past DRAG_SETTLE: an isolated resize, the full width back.
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(150));
}

/// One step of a DRAG: the resize lands and its pass runs, and the next step comes well
/// inside `DRAG_SETTLE`, so the burst layout (a column short) holds between steps.
fn drag_step(lh: &LoopHarness, w: u16, h: u16) {
    resize_step(lh, w, h);
    thread::sleep(Duration::from_millis(60));
}

/// Sends one resize and waits for its pass: the shared width carries the frame's width
/// (the terminal's, or a column short while the burst lasts).
fn resize_step(lh: &LoopHarness, w: u16, h: u16) {
    lh.geo.set_size(w, h);
    lh.etx.send(Event::Resize(w, h)).unwrap();
    assert!(lh.wait_until(Duration::from_secs(1), |lh| {
        let got = lh.width.load(Ordering::Relaxed);
        let short = crate::ui::runtime::event_loop::DRAG_MARGIN_MAX;
        got <= w && got + short >= w && lh.height.load(Ordering::Relaxed) == h
    }));
    thread::sleep(Duration::from_millis(40));
}

/// ONE scripted resize to `w`×`h` of the idle loop, with `emulate` standing in for the
/// emulator's own resize and `cursor_shift` for where it moved the cursor. Returns the
/// reachable rows, the screen, and the bytes the loop wrote after the event.
fn idle_resize(
    w: u16,
    h: u16,
    cursor_shift: i32,
    emulate: impl FnOnce(&mut vt100::Parser),
) -> (Vec<String>, Vec<String>, String) {
    let lh = idle_loop();
    let mark = lh.buf.bytes().len();
    lh.geo.shift_cursor(cursor_shift);
    resize_and_settle(&lh, w, h);
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
    let (all, screen) = replay_resized(&bytes, mark, emulate);
    lh.quit_and_join(Duration::from_secs(2));
    (all, screen, after)
}

// X-52's residual (1) since B3, measured on the tmux model (idle, four staged rows, two
// separators): the rows a reflow grows above the cursor stay behind as a duplicate — the
// frame's first rows that far up. A drag's first step wraps the top separator and a staged
// row the diff lengthened with written blanks (tmux counts them): 2. A resize after a settled
// one is another first step (the diagonal after a width step: 1 + 2).
const BUDGET_DRAG: usize = 2;
const BUDGET_2X: usize = 2;
const BUDGET_3X: usize = 4;
const BUDGET_LARGE: usize = 4;
const BUDGET_DIAGONAL: usize = 2;
const BUDGET_DIAGONAL_AFTER_STEP: usize = 3;
const BUDGET_STARTUP_3X: usize = 3;

/// The resize invariants every direction must keep: ratatui's full clear never ran, no
/// committed line is lost, the rows the resize left twice — a committed line reachable more
/// than once, a separator row beyond the frame's pair — stay within `budget` (X-52's residual
/// (1): the rows a reflow grew above the cursor are never claimed since B3, so the frame's
/// first rows that far up stay behind as a duplicate), and the frame is still flush with the
/// bottom: the composer on the screen's third-last row (composer, separator, status).
fn assert_resize_clean(tag: &str, all: &[String], screen: &[String], after: &str, budget: usize) {
    let dump = screen.join("\n");
    assert!(
        !after.contains("\u{1b}[2J"),
        "{tag}: ESC[2J after the resize — ratatui's inline resize ran:\n{after:?}"
    );
    assert!(
        scroll_downs(after) <= 1,
        "{tag}: a resize inserted rows (a scroll-down) — only the band close at the end of a drag may:\n{after:?}"
    );
    assert_eq!(
        home_erases(),
        0,
        "{tag}: an erase-below from the home position — tmux files the screen into the history:\n{after:?}"
    );
    let mut extra = 0;
    for i in 0..12 {
        let line = format!("line-{i:02}");
        let n = all.iter().filter(|r| r.contains(line.as_str())).count();
        assert!(n >= 1, "{tag}: {line} LOST:\n{}", all.join("\n"));
        extra += n - 1;
    }
    let seps = all.iter().filter(|r| r.contains('┄')).count();
    assert!(
        seps >= 2,
        "{tag}: {seps} separator rows:\n{}",
        all.join("\n")
    );
    extra += seps - 2;
    assert!(
        extra <= budget,
        "{tag}: {extra} rows left twice, over the budget of {budget}:\n{}",
        all.join("\n")
    );
    let composers: Vec<usize> = screen
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains('❯'))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        composers,
        vec![screen.len() - 3],
        "{tag}: the composer left the bottom:\n{dump}"
    );
}

/// The owner's repro (2026-09-23): an idle WIDTH shrink. ratatui's inline resize re-anchored
/// the frame at row 0 behind an `ESC[2J` — composer at the top, every transcript row
/// still on screen erased. iota now owns the resize: same rows, same place. (vt100 does
/// not reflow: this is the xterm-without-reflow half; the reflow half is below.)
#[test]
fn idle_width_shrink_keeps_the_frame_down_and_every_row() {
    let (all, screen, after) = idle_resize(60, 24, 0, |p| p.screen_mut().set_size(24, 60));
    assert_resize_clean("width 80→60", &all, &screen, &after, 0);
}

/// The control: a width GROW never took ratatui's clear path and must still hold.
#[test]
fn idle_width_grow_keeps_the_frame_down_and_every_row() {
    let (all, screen, after) = idle_resize(100, 24, 0, |p| p.screen_mut().set_size(24, 100));
    assert_resize_clean("width 80→100", &all, &screen, &after, 0);
}

/// A HEIGHT shrink as xterm-family emulators do it with the cursor near the bottom: the
/// top `d` rows go to history and everything, the cursor included, moves up `d`. The
/// synthetic DSR is moved with it (`shift_cursor`) — without that, it would answer a row
/// the smaller screen no longer has. Emulators that instead drop blank rows below the
/// frame keep the cursor put; that variant is the tmux layer's (scenario 06).
#[test]
fn idle_height_shrink_keeps_the_frame_down_and_every_row() {
    let (all, screen, after) = idle_resize(80, 20, -4, |p| {
        // Push the top 4 rows into history (a full-screen scroll), then drop the
        // now-blank bottom 4 rows; the cursor goes with its cell, 4 rows up.
        let (row, col) = p.screen().cursor_position();
        p.process(format!("\u{1b}[24;1H{}", "\n".repeat(4)).as_bytes());
        p.process(format!("\u{1b}[{};{}H", row - 3, col + 1).as_bytes());
        p.screen_mut().set_size(20, 80);
    });
    assert_resize_clean("height 24→20", &all, &screen, &after, 0);
}

/// The verifier's round-2/3 repro, in the reflow model: a 30-step drag, one column per step,
/// under W5's BURST layout (the owner's decision, 2026-09-24): full width at rest, a column
/// short while the drag lasts. Only the first step rewraps the full-width separators (a
/// two-row band, filled by the next output); every later step rewraps nothing. What the
/// first step grew above the cursor stays behind (B3's budget: 2 rows), no blank row reaches
/// the history, the composer stays flush, and when the drag settles the separators are full
/// width again — a repaint, no reflow.
#[test]
fn settled_drag_in_a_reflowing_emulator_stays_flush_and_clean() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    let mark = lh.buf.bytes().len();
    for w in (50..80).rev() {
        let moved = emu.resize(&lh.buf.bytes(), w, 24);
        lh.geo.shift_cursor(moved);
        drag_step(&lh, w, 24);
    }
    // The drag settles: the full width comes back with a repaint, nothing reflows.
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(150));
    assert_eq!(
        lh.width.load(Ordering::Relaxed),
        50,
        "the full width is restored"
    );
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
    let history = emu.history(&bytes);
    let (all, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    let last_line = history.iter().rposition(|r| r.contains("line-"));
    let first_line = history.iter().position(|r| r.contains("line-"));
    if let (Some(a), Some(b)) = (first_line, last_line) {
        let interleaved = history[a..=b]
            .iter()
            .filter(|r| r.trim().is_empty())
            .count();
        assert_eq!(
            interleaved,
            0,
            "a blank row entered the history before the transcript had left the screen:\n{}",
            history.join("\n")
        );
    }
    let band = last_line.map_or(0, |b| {
        history[b + 1..]
            .iter()
            .filter(|r| r.trim().is_empty())
            .count()
    });
    assert_eq!(band, 0, "blank rows of a resize band reached the history");
    assert_resize_clean("30-step drag 80→50", &all, &screen, &after, BUDGET_DRAG);
    let sep = screen.iter().rev().find(|r| r.contains('┄')).unwrap();
    assert_eq!(
        sep.chars().filter(|&c| c == '┄').count(),
        50,
        "the separators are full width again"
    );
}

/// A drastic narrowing — a maximized window restored, a full-width pane halved: the old
/// width is 2× and 3× the new one, every staged row and both separators split into
/// several pieces. Nothing is claimed above the cursor (B3): what grew there stays behind
/// as a duplicate within the budget, and nothing is lost.
#[test]
fn a_2x_and_a_3x_narrowing_in_a_reflowing_emulator_stay_clean() {
    for (w, tag, budget) in [(40, "2× 80→40", BUDGET_2X), (26, "3× 80→26", BUDGET_3X)] {
        let lh = idle_loop();
        let mut emu = TmuxReflowEmu::new();
        let mark = lh.buf.bytes().len();
        let moved = emu.resize(&lh.buf.bytes(), w, 24);
        lh.geo.shift_cursor(moved);
        resize_and_settle(&lh, w, 24);
        let bytes = lh.buf.bytes();
        let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
        let (all, screen) = emu.finish(&bytes);
        lh.quit_and_join(Duration::from_secs(2));
        assert_resize_clean(tag, &all, &screen, &after, budget);
    }
}

/// One large narrowing in the reflow model — every staged row and both separators
/// rewrap — lands as cleanly as the drag's single steps.
#[test]
fn one_large_narrowing_in_a_reflowing_emulator_stays_clean() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    let moved = emu.resize(&lh.buf.bytes(), 30, 24);
    lh.geo.shift_cursor(moved);
    resize_and_settle(&lh, 30, 24);
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes).into_owned();
    let (all, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    assert_resize_clean("width 80→30", &all, &screen, &after, BUDGET_LARGE);
}

/// The anchor law in isolation, for a frame that is NOT flush with the bottom: the
/// cursor moved `k` rows, and the recreated frame lands at `cursor − its frame row`
/// with nothing above that row cleared.
#[test]
fn resize_anchors_at_the_cursor_row_minus_its_frame_row() {
    let (mut t, buf, geo) = direct_term(5, 10);
    let frame = crate::ui::render::frame::FrameView {
        rows: vec![
            String::new(),
            "SEP".to_owned(),
            "❯ ".to_owned(),
            "SEP".to_owned(),
            "status".to_owned(),
        ],
        cursor: Some((2, 2)),
    };
    t.draw_frame(&frame).unwrap();
    assert_eq!(geo.cursor(), ratatui::layout::Position::new(2, 12));
    let mark = buf.bytes().len();
    geo.shift_cursor(-3);
    geo.set_size(60, 24);
    t.resize(
        ratatui::layout::Size {
            width: 60,
            height: 24,
        },
        5,
    )
    .unwrap();
    assert_eq!(
        t.top(),
        7,
        "anchor must be cursor (9) − the composer's frame row (2)"
    );
    t.draw_frame(&frame).unwrap();
    let after = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert!(
        !after.contains("\u{1b}[2J"),
        "full clear on resize:\n{after:?}"
    );
    // Counted from the cursor, not from a row number: up the composer's frame row (2) and
    // erase from there (EL 2 on the row, then an erase-below from its second column).
    assert!(
        after.starts_with("\u{1b}[2A\r\u{1b}[2K\u{1b}[C\u{1b}[J"),
        "the first bytes after the event must climb the frame's 2 rows and erase:\n{after:?}"
    );
    let screen = parse_shifted(&buf, mark, -3).screen().contents();
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(
        rows.get(9).copied(),
        Some("❯"),
        "the composer on row 9:\n{screen}"
    );
}

/// A flush frame stays flush through a reflow: rows below the cursor multiplied (the
/// cursor went up 3 on an unchanged height), so the new top is the floor `S' − h`; the
/// band from the cursor estimate down is cleared and left blank ABOVE the frame — nothing
/// is scrolled or inserted — and the next output fills it before anything scrolls.
#[test]
fn a_flush_frame_resizes_onto_the_floor_and_the_band_takes_the_next_output() {
    let (mut t, buf, geo) = direct_term(5, 19);
    let frame = crate::ui::render::frame::FrameView {
        rows: vec![
            String::new(),
            "SEP".to_owned(),
            "❯ ".to_owned(),
            "SEP".to_owned(),
            "status".to_owned(),
        ],
        cursor: Some((2, 2)),
    };
    t.draw_frame(&frame).unwrap();
    let mark = buf.bytes().len();
    geo.shift_cursor(-3);
    geo.set_size(60, 24);
    t.resize(
        ratatui::layout::Size {
            width: 60,
            height: 24,
        },
        5,
    )
    .unwrap();
    assert_eq!(t.top(), 19, "a flush frame lands on the floor (24 − 5)");
    let after = String::from_utf8_lossy(&buf.bytes()[mark..]).into_owned();
    assert!(
        after.starts_with("\u{1b}[2A\r\u{1b}[2K\u{1b}[C\u{1b}[J\r\n\n\n"),
        "clear from the cursor estimate (16, two rows up), then down 3 onto the floor:\n{after:?}"
    );
    assert_eq!(
        scroll_downs(&after),
        0,
        "a resize scrolls nothing:\n{after:?}"
    );
    t.draw_frame(&frame).unwrap();
    // Three rows of output fill the three-row band: the frame stays flush.
    t.insert_lines(&["a".to_owned(), "b".to_owned(), "c".to_owned()])
        .unwrap();
    t.draw_frame(&frame).unwrap();
    assert_eq!(
        t.top(),
        19,
        "the band took the output; the frame is flush again"
    );
    let screen = parse_shifted(&buf, mark, -3).screen().contents();
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(
        &rows[16..19],
        &["a", "b", "c"],
        "output landed in the band:\n{screen}"
    );
}

/// A surface hides the cursor, and the anchor must still be known: a hidden-cursor draw
/// parks the physical cursor on the frame's top-left — no frame row above it can rewrap —
/// so a resize while a picker or `/jobs` is open re-anchors exactly like one under the
/// composer (it used to keep a stale top until the surface closed).
#[test]
fn resize_with_a_hidden_cursor_anchors_on_the_frames_first_row() {
    let (mut t, _buf, geo) = direct_term(6, 18);
    let surface = crate::ui::render::frame::FrameView {
        rows: (0..6).map(|i| format!("panel-{i}")).collect(),
        cursor: None,
    };
    t.draw_frame(&surface).unwrap();
    assert_eq!(
        geo.cursor(),
        ratatui::layout::Position::new(0, 18),
        "a hidden-cursor draw must leave the cursor on the frame's top-left"
    );
    geo.shift_cursor(-2);
    geo.set_size(80, 22);
    t.resize(
        ratatui::layout::Size {
            width: 80,
            height: 22,
        },
        6,
    )
    .unwrap();
    assert_eq!(t.top(), 16, "anchor must be the cursor (16) itself");
}

/// The verifier's `/model` repro, in the reflow model: a resize while a surface is open
/// (cursor hidden, the frame taller than the idle one, its rows wide) used to leave half
/// the old frame — a separator and an empty `❯` — in the middle of the conversation.
#[test]
fn resize_with_a_surface_open_leaves_no_half_frame() {
    let (mut t, buf, geo) = direct_term(1, 23);
    let history: Vec<String> = (0..6).map(|i| format!("hist-{i}")).collect();
    t.insert_lines(&history).unwrap();
    let view = |w: usize| crate::ui::render::frame::FrameView {
        rows: [String::new(), "┄".repeat(w), "❯ ".to_owned(), "┄".repeat(w)]
            .into_iter()
            .chain((0..4).map(|i| format!("model-{i} {}", "·".repeat(w - 12))))
            .collect(),
        cursor: None,
    };
    t.ensure_height(8).unwrap();
    t.draw_frame(&view(80)).unwrap();
    let mut emu = TmuxReflowEmu::new();
    let moved = emu.resize(&buf.bytes(), 60, 24);
    geo.shift_cursor(moved);
    geo.set_size(60, 24);
    t.resize(
        ratatui::layout::Size {
            width: 60,
            height: 24,
        },
        8,
    )
    .unwrap();
    t.draw_frame(&view(60)).unwrap();
    let (all, screen) = emu.finish(&buf.bytes());
    let dump = all.join("\n");
    assert_eq!(all.iter().filter(|r| r.contains('❯')).count(), 1, "{dump}");
    assert_eq!(all.iter().filter(|r| r.contains('┄')).count(), 2, "{dump}");
    for h in &history {
        let n = all.iter().filter(|r| r.contains(h.as_str())).count();
        assert_eq!(n, 1, "{h} reachable {n} times:\n{dump}");
    }
    assert!(
        screen[23].contains("model-3"),
        "the frame left the bottom:\n{dump}"
    );
}

/// W6 across a narrowing: a staged row wrapped for 80 columns stays staged, rewrapped for
/// 60 by the resize's retrim; output that later pushes it out inserts one row per entry
/// (a debug build used to panic the loop thread on this insert).
#[test]
fn width_shrink_rewraps_a_wide_staged_row() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let wide = format!("wide-{}", "x".repeat(70));
    h.region.lock().unwrap().commit(vec![wide]);
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains("wide-")));
    resize_and_settle(&h, 60, 24);
    let tail = h.region.lock().unwrap().tail.clone();
    assert_eq!(
        tail.len(),
        2,
        "the wide row must be rewrapped in place: {tail:?}"
    );
    assert!(
        tail.iter().all(|r| crate::text::ansi::ansi_width(r) < 60),
        "{tail:?}"
    );
    h.region
        .lock()
        .unwrap()
        .commit((0..6).map(|i| format!("after-{i}")).collect());
    assert!(h.wait_until(Duration::from_secs(1), |h| h.contents().contains("after-5")));
    assert!(!h.join.is_finished(), "the loop thread died on the insert");
    h.quit_and_join(Duration::from_secs(2));
}

/// A narrowing that also shortens the pane: tmux eats the rows below the cursor BEFORE it
/// rewraps, the cursor lands on the bottom row and the frame stays at its first row. Nothing
/// is claimed above the cursor (B3): what the reflow grew there stays behind as a duplicate,
/// within the budget; nothing is lost — alone, and after a width-only step.
#[test]
fn a_diagonal_narrowing_loses_nothing_and_stays_within_the_budget() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    let moved = emu.resize(&lh.buf.bytes(), 70, 20);
    lh.geo.shift_cursor(moved);
    resize_and_settle(&lh, 70, 20);
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes).into_owned();
    let (all, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    assert_resize_clean("80×24→70×20", &all, &screen, &after, BUDGET_DIAGONAL);

    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    let moved = emu.resize(&lh.buf.bytes(), 76, 24);
    lh.geo.shift_cursor(moved);
    resize_and_settle(&lh, 76, 24);
    let moved = emu.resize(&lh.buf.bytes(), 60, 20);
    lh.geo.shift_cursor(moved);
    resize_and_settle(&lh, 60, 20);
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes).into_owned();
    let (all, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    assert_resize_clean(
        "76×24, then 60×20",
        &all,
        &screen,
        &after,
        BUDGET_DIAGONAL_AFTER_STEP,
    );
}

/// The verifier's banner repro, in the reflow model: right after startup the frame sits
/// near the top with the banner still staged in it, and a 3× narrowing grows the frame so
/// much that the emulator pushes its first rows — the staged banner rows — off the top into
/// the history. Since B3 they are not recognised there: the frame draws them again, a
/// duplicate within the budget. Nothing is lost, and the frame on row 0 is never erased from
/// the home position.
#[test]
fn a_3x_narrowing_at_startup_loses_nothing_and_stays_within_the_budget() {
    let lh = start_loop_at(0);
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let banner: Vec<String> = (0..3)
        .map(|i| format!("│ banner-{i} {}│", "·".repeat(20)))
        .collect();
    lh.region.lock().unwrap().commit(banner.clone());
    assert!(lh.wait_until(Duration::from_secs(2), |h| {
        h.contents().contains("banner-2")
    }));
    thread::sleep(Duration::from_millis(150));
    let mut emu = TmuxReflowEmu::new();
    let mark = lh.buf.bytes().len();
    let moved = emu.resize(&lh.buf.bytes(), 27, 24);
    lh.geo.shift_cursor(moved);
    // One width-only narrowing of a frame near the top proves the reflow: the cursor moved.
    resize_and_settle(&lh, 27, 24);
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
    let (all, _) = emu.finish(&bytes);
    let staged = lh.region.lock().unwrap().tail.clone();
    lh.quit_and_join(Duration::from_secs(2));
    let dump = all.join("\n");
    assert_eq!(
        scroll_downs(&after),
        0,
        "a resize scrolls nothing:\n{after:?}"
    );
    assert_eq!(
        home_erases(),
        0,
        "the frame re-anchored on row 0 must not be erased from the home position:\n{after:?}"
    );
    let mut extra = 0;
    for b in &banner {
        let key = b.split_whitespace().nth(1).unwrap();
        let n = all.iter().filter(|r| r.contains(key)).count();
        assert!(n >= 1, "{key} LOST:\n{dump}");
        extra += n - 1;
    }
    let seps = all.iter().filter(|r| r.contains('┄')).count();
    assert!(seps >= 2, "{dump}");
    extra += seps - 2;
    assert!(
        extra <= BUDGET_STARTUP_3X,
        "{extra} rows left twice, over the budget of {BUDGET_STARTUP_3X}:\n{dump}"
    );
    let _ = staged;
}

// ---------------------------------------------------------------------------
// Wide graphemes at a row's end: a row's width is its last grapheme's RIGHT edge
// ---------------------------------------------------------------------------

/// Sends `keys` to the loop one by one and returns the composer row (the screen row that
/// starts with `❯`) once it reads `want`, or the last one seen.
fn composer_after(lh: &LoopHarness, keys: &[Event], want: &str) -> (String, usize) {
    let mark = lh.buf.bytes().len();
    for k in keys {
        lh.etx.send(k.clone()).unwrap();
        thread::sleep(Duration::from_millis(40));
    }
    let row = |h: &LoopHarness| {
        let p = parse(&h.buf);
        let cols = p.screen().size().1;
        p.screen()
            .rows(0, cols)
            .find(|r| r.starts_with('❯'))
            .unwrap_or_default()
            .trim_end()
            .to_owned()
    };
    lh.wait_until(Duration::from_secs(1), |h| row(h) == want);
    (row(lh), mark)
}

fn key(code: crossterm::event::KeyCode) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::NONE,
    ))
}

fn typed(s: &str) -> Vec<Event> {
    s.chars()
        .map(|c| key(crossterm::event::KeyCode::Char(c)))
        .collect()
}

/// Every erase-in-line / erase-line the loop emitted after `mark` lands on a grapheme
/// boundary of the composer row: never on the right half of a wide glyph (an erase that
/// starts there wipes the WHOLE glyph in a vt100-family emulator, and ratatui, which still
/// holds it, never draws it again).
fn assert_erases_on_boundaries(lh: &LoopHarness, mark: usize, row: &str) {
    let bytes = lh.buf.bytes();
    let after = String::from_utf8_lossy(&bytes[mark..]).into_owned();
    let mut right_halves = Vec::new();
    let mut col = 0usize;
    for g in unicode_segmentation::UnicodeSegmentation::graphemes(row, true) {
        let w = crate::text::width::str_width(g);
        if w == 2 {
            right_halves.push(col + 2); // 1-based column of the right half
        }
        col += w.max(1);
    }
    for seq in after.split("\u{1b}[").skip(1) {
        // `ROW;COLH` immediately followed by `ESC[K`.
        if let Some((pos, rest)) = seq.split_once('H')
            && rest.is_empty()
            && let Some((_, c)) = pos.split_once(';')
            && let Ok(c) = c.parse::<usize>()
        {
            let next = after
                .split(&format!("\u{1b}[{pos}H"))
                .nth(1)
                .unwrap_or_default();
            if next.starts_with("\u{1b}[K") {
                assert!(
                    !right_halves.contains(&c),
                    "an EL starts on the right half of a wide glyph (column {c}) of {row:?}:\n{after:?}"
                );
            }
        }
    }
}

#[test]
fn backspace_after_a_wide_grapheme_keeps_the_row_whole() {
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let (row, _) = composer_after(&lh, &typed("中文测试"), "❯ 中文测试");
    assert_eq!(row, "❯ 中文测试");
    let bs = key(crossterm::event::KeyCode::Backspace);
    let (row, mark) = composer_after(&lh, std::slice::from_ref(&bs), "❯ 中文测");
    assert_eq!(row, "❯ 中文测", "one Backspace must remove exactly 试");
    assert_erases_on_boundaries(&lh, mark, &row);
    let (row, _) = composer_after(&lh, &[bs.clone(), bs], "❯ 中");
    assert_eq!(row, "❯ 中");
    lh.quit_and_join(Duration::from_secs(2));
}

#[test]
fn deleting_inside_a_wide_row_keeps_the_rest_whole() {
    use crossterm::event::KeyCode;
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    composer_after(&lh, &typed("中文测试"), "❯ 中文测试");
    let (row, _) = composer_after(
        &lh,
        &[
            key(KeyCode::Left),
            key(KeyCode::Left),
            key(KeyCode::Backspace),
        ],
        "❯ 中测试",
    );
    assert_eq!(row, "❯ 中测试", "← ← Backspace removes 文 only");
    let (row, _) = composer_after(&lh, &[key(KeyCode::Home), key(KeyCode::Delete)], "❯ 测试");
    assert_eq!(row, "❯ 测试", "Home Delete removes 中 only");
    lh.quit_and_join(Duration::from_secs(2));
}

/// A wide emoji at the row's end. (vt100 does not join a skin-tone modifier onto its base —
/// it would show `👍🏽` as `👍` — so the modifier case is the unit test's,
/// `term::tests::a_row_ending_in_a_wide_grapheme_is_as_wide_as_its_right_edge`.)
#[test]
fn backspace_after_a_wide_emoji_keeps_it_whole() {
    use crossterm::event::KeyCode;
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    let (row, _) = composer_after(&lh, &typed("a👍x"), "❯ a👍x");
    assert_eq!(row, "❯ a👍x");
    let (row, mark) = composer_after(&lh, &[key(KeyCode::Backspace)], "❯ a👍");
    assert_eq!(row, "❯ a👍", "Backspace removes x only");
    assert_erases_on_boundaries(&lh, mark, &row);
    lh.quit_and_join(Duration::from_secs(2));
}

/// A multi-line paste becomes a `[#N …]` tag, which ends in an ASCII `]` — a row ends in a
/// wide grapheme only when one follows the tag; deleting after it must keep that grapheme
/// whole.
#[test]
fn a_wide_grapheme_after_a_paste_tag_stays_whole() {
    use crossterm::event::KeyCode;
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    lh.etx
        .send(Event::Paste("文字\n第二行".to_owned()))
        .unwrap();
    assert!(lh.wait_until(Duration::from_secs(1), |h| h.contents().contains("[#1")));
    let tag = "[#1 文字… 2 lines]";
    composer_after(&lh, &typed("中文"), &format!("❯ {tag}中文"));
    let (row, mark) = composer_after(&lh, &[key(KeyCode::Backspace)], &format!("❯ {tag}中"));
    assert_eq!(row, format!("❯ {tag}中"), "Backspace removes 文 only");
    assert_erases_on_boundaries(&lh, mark, &row);
    lh.quit_and_join(Duration::from_secs(2));
}

/// After a drag, the next output follows the previous content directly: the first step's
/// band (the full-width separators rewrapped before the burst layout could take over) is
/// written into from its top, right under the transcript, and the frame stays flush — the
/// output never starts above a hole.
#[test]
fn the_next_output_follows_the_transcript_after_a_drag() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    for w in (75..80).rev() {
        let moved = emu.resize(&lh.buf.bytes(), w, 24);
        lh.geo.shift_cursor(moved);
        drag_step(&lh, w, 24);
    }
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(150));
    lh.region
        .lock()
        .unwrap()
        .commit((0..6).map(|i| format!("next-{i}")).collect());
    assert!(lh.wait_until(Duration::from_secs(1), |h| h.contents().contains("next-5")));
    thread::sleep(Duration::from_millis(150));
    let (all, screen) = emu.finish(&lh.buf.bytes());
    lh.quit_and_join(Duration::from_secs(2));
    let dump = all.join("\n");
    let last_old = all.iter().rposition(|r| r.contains("line-11")).unwrap();
    let first_new = all.iter().position(|r| r.contains("next-0")).unwrap();
    assert_eq!(
        first_new,
        last_old + 1,
        "the next output follows line-11 directly:\n{dump}"
    );
    let composer = screen.iter().position(|r| r.contains('❯')).unwrap();
    assert_eq!(composer, screen.len() - 3, "the frame stays flush:\n{dump}");
}

/// A drag whose terminal sends TWO-column steps: the burst layout gives up as many columns
/// as the widest step seen (up to `DRAG_MARGIN_MAX`), so after the first step no step rewraps
/// the frame either — no blank row of a band reaches the history.
#[test]
fn a_drag_of_two_column_steps_leaves_no_band() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    for w in (60..=78).rev().step_by(2) {
        let moved = emu.resize(&lh.buf.bytes(), w, 24);
        lh.geo.shift_cursor(moved);
        drag_step(&lh, w, 24);
    }
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(150));
    let bytes = lh.buf.bytes();
    let history = emu.history(&bytes);
    let (all, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    let last_line = history.iter().rposition(|r| r.contains("line-"));
    let band = last_line.map_or(0, |b| {
        history[b + 1..]
            .iter()
            .filter(|r| r.trim().is_empty())
            .count()
    });
    assert_eq!(band, 0, "blank rows of a resize band reached the history");
    assert_resize_clean("10 two-column steps 80→60", &all, &screen, "", BUDGET_DRAG);
}

/// The drag's settle deadline is part of the loop's poll deadline, so the full width comes
/// back promptly without a busy loop; and it comes back only once the burst is over.
#[test]
fn the_drag_settle_deadline_bounds_the_poll_and_restores_the_width() {
    let mut m = crate::ui::testutil::test_model();
    m.width = 80;
    m.dirty = false;
    m.drag_until = Some(Instant::now() + Duration::from_millis(30));
    m.drag_margin = 2;
    assert!(m.poll_deadline() <= Duration::from_millis(30));
    assert_eq!(
        m.frame_width(),
        78,
        "two columns short while the drag lasts"
    );
    m.settle_drag();
    assert_eq!(m.frame_width(), 78, "not before the deadline");
    thread::sleep(Duration::from_millis(40));
    m.settle_drag();
    assert_eq!(m.frame_width(), 80, "the full width is back");
    assert_eq!(m.shared.width.load(Ordering::Relaxed), 80);
    assert!(m.dirty, "a repaint is due");
}

/// The verifier's round-2 repro of the burst layout: a PACED drag, one step a second (a hand
/// that pauses, a keyboard resize). The steps stay inside one burst (`DRAG_SETTLE` is two
/// seconds), so only the first rewraps the full-width rows; and when the drag is over its band
/// is closed — the transcript sits on the frame again, no hole is left for the next output.
#[test]
fn a_paced_drag_stays_one_burst_and_leaves_no_hole() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    for w in (75..80).rev() {
        let moved = emu.resize(&lh.buf.bytes(), w, 24);
        lh.geo.shift_cursor(moved);
        resize_step(&lh, w, 24);
        thread::sleep(Duration::from_millis(1000));
    }
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(200));
    let bytes = lh.buf.bytes();
    let history = emu.history(&bytes);
    let (_, screen) = emu.finish(&bytes);
    lh.quit_and_join(Duration::from_secs(2));
    let dump = screen.join("\n");
    let last_line = history.iter().rposition(|r| r.contains("line-"));
    let band = last_line.map_or(0, |b| {
        history[b + 1..]
            .iter()
            .filter(|r| r.trim().is_empty())
            .count()
    });
    assert_eq!(band, 0, "blank rows of a resize band reached the history");
    let a = screen.iter().position(|r| r.contains("line-07")).unwrap();
    let b = screen.iter().position(|r| r.contains("line-08")).unwrap();
    assert_eq!(
        b,
        a + 1,
        "no hole between the transcript and the frame:\n{dump}"
    );
}

/// An interaction ends a drag at once: a key pressed a moment after a resize brings the full
/// width back without waiting for `DRAG_SETTLE`.
#[test]
fn a_key_ends_the_drag() {
    let lh = idle_loop();
    resize_step(&lh, 76, 24);
    assert!(
        lh.width.load(Ordering::Relaxed) < 76,
        "the drag narrows the frame"
    );
    lh.etx
        .send(key(crossterm::event::KeyCode::Char('x')))
        .unwrap();
    assert!(
        lh.wait_until(Duration::from_millis(500), |h| h
            .width
            .load(Ordering::Relaxed)
            == 76),
        "a key did not end the drag"
    );
    lh.quit_and_join(Duration::from_secs(2));
}

/// Every `line-NN` of the idle loop's twelve is reachable exactly once.
fn assert_no_line_lost_or_doubled(tag: &str, all: &[String]) {
    for i in 0..12 {
        let line = format!("line-{i:02}");
        let n = all.iter().filter(|r| r.contains(line.as_str())).count();
        assert_eq!(
            n,
            1,
            "{tag}: {line} reachable {n} times:\n{}",
            all.join("\n")
        );
    }
}

/// The verifier's P0 (2026-09-24): a HEIGHT drag down and back up in tmux lost a committed
/// row. tmux pulls history rows back when the height grows, and in a fast drag the terminal
/// is already at the NEXT size when the loop handles a resize event — here the emulator has
/// shrunk to 20 and grown back to 21 (pulling a row back) before the loop reads the stale
/// "20" event. The pass must take the size the terminal has now, and verify where the frame
/// really is before it erases anything; the old pass erased the row above the frame.
#[test]
fn a_height_drag_down_and_back_loses_no_row() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    for h in [22, 20] {
        let moved = emu.resize(&lh.buf.bytes(), 80, h);
        lh.geo.shift_cursor(moved);
        drag_step(&lh, 80, h);
    }
    // The race: the emulator goes 20 → 21 (a row pulled back), and only THEN does the loop
    // read an event — a stale one still saying 20.
    let moved = emu.resize(&lh.buf.bytes(), 80, 21);
    lh.geo.shift_cursor(moved);
    lh.geo.set_size(80, 21);
    lh.etx.send(Event::Resize(80, 20)).unwrap();
    thread::sleep(Duration::from_millis(150));
    for h in [21, 23, 24] {
        let moved = emu.resize(&lh.buf.bytes(), 80, h);
        lh.geo.shift_cursor(moved);
        drag_step(&lh, 80, h);
    }
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(200));
    let (all, _) = emu.finish(&lh.buf.bytes());
    lh.quit_and_join(Duration::from_secs(2));
    assert_no_line_lost_or_doubled("height 24→20→(21)→24", &all);
}

/// An oscillating height (29/28/27/26/30 in the verifier's run), every step a grow or a
/// shrink with history pulled back on the grows, stale events included: nothing is lost.
#[test]
fn an_oscillating_height_loses_no_row() {
    let lh = idle_loop();
    let mut emu = TmuxReflowEmu::new();
    let mut prev = 24;
    for (i, h) in [23, 22, 21, 20, 24, 23, 22, 21, 24, 22, 20, 24]
        .into_iter()
        .enumerate()
    {
        let moved = emu.resize(&lh.buf.bytes(), 80, h);
        lh.geo.shift_cursor(moved);
        lh.geo.set_size(80, h);
        // Every third event arrives late: the loop reads the size before this one.
        let seen = if i % 3 == 2 { prev } else { h };
        lh.etx.send(Event::Resize(80, seen)).unwrap();
        thread::sleep(Duration::from_millis(120));
        prev = h;
    }
    lh.etx.send(Event::Resize(80, prev)).unwrap();
    thread::sleep(crate::ui::runtime::event_loop::DRAG_SETTLE + Duration::from_millis(300));
    let (all, _) = emu.finish(&lh.buf.bytes());
    lh.quit_and_join(Duration::from_secs(2));
    assert_no_line_lost_or_doubled("oscillating height", &all);
}

// --- The stopgap's races (2026-09-25), kept under X-54: no erase trusts a position the
// emulator may have moved. The stopgap closed them inside the recreation; the owned terminal
// counts every byte from the cursor, and these replays still fail on 4da669a. ---

/// A 9-row frame flush on 80×24 under `lines` committed rows `t-NN` (≥ 15), the composer cursor on
/// frame row 6 (or hidden, parked on the frame's first row), every byte also fed to a tmux
/// model.
fn raced_term(
    lines: usize,
    cursor: Option<(u16, u16)>,
) -> (
    crate::ui::runtime::term::Term<SharedBuf>,
    SharedBuf,
    crate::ui::runtime::term::Geometry,
    Arc<Mutex<TmuxReflowEmu>>,
    crate::ui::render::frame::FrameView,
) {
    // The transcript as plain output ahead of the frame (vt100 files no region-scrolled row
    // into its history), its last row right above the frame's top.
    let buf = SharedBuf::default();
    let mut prelude = String::new();
    for i in 0..lines {
        std::fmt::Write::write_fmt(&mut prelude, format_args!("t-{i:02}\r\n")).unwrap();
    }
    prelude.push_str(&"\n".repeat(8));
    io::Write::write_all(&mut buf.clone(), prelude.as_bytes()).unwrap();
    let geo = crate::ui::runtime::term::Geometry::new(80, 24);
    let wtr = buf.clone();
    let mut t = crate::ui::runtime::term::Term::new(
        Box::new(move || wtr.clone()),
        9,
        15,
        Some(geo.clone()),
    )
    .unwrap();
    let frame = crate::ui::render::frame::FrameView {
        // The status row runs 70 columns: a narrowing to 60 wraps it BELOW the cursor.
        rows: (0..9)
            .map(|i| match i {
                8 => format!("f-8 {}", "x".repeat(66)),
                _ => format!("f-{i}"),
            })
            .collect(),
        cursor,
    };
    t.draw_frame(&frame).unwrap();
    (
        t,
        buf,
        geo,
        Arc::new(Mutex::new(TmuxReflowEmu::new())),
        frame,
    )
}

/// The model resizes to `h` rows NOW — the bytes written so far on its screen — and the
/// synthetic tty follows: the cursor moved with its cell, the size is the new one.
fn emu_resize(
    emu: &Mutex<TmuxReflowEmu>,
    buf: &SharedBuf,
    geo: &crate::ui::runtime::term::Geometry,
    (w, h): (u16, u16),
) {
    let moved = emu.lock().unwrap().resize(&buf.bytes(), w, h);
    geo.shift_cursor(moved);
    geo.set_size(w, h);
}

/// Arms the tmux model to resize to `h` rows right after the `nth` cursor query is answered.
fn resize_after_query(
    nth: usize,
    size: (u16, u16),
    emu: &Arc<Mutex<TmuxReflowEmu>>,
    buf: &SharedBuf,
    geo: &crate::ui::runtime::term::Geometry,
) {
    let (emu, buf, g) = (Arc::clone(emu), buf.clone(), geo.clone());
    geo.after_query(nth, move || emu_resize(&emu, &buf, &g, size));
}

/// One resize pass on `h` rows as the loop runs it, and the draw after it.
fn pass(
    t: &mut crate::ui::runtime::term::Term<SharedBuf>,
    frame: &crate::ui::render::frame::FrameView,
    (width, height): (u16, u16),
) {
    t.resize(ratatui::layout::Size { width, height }, 9)
        .unwrap();
    t.draw_frame(frame).unwrap();
}

/// Every `t-NN` of `lines` is still reachable (a duplicate is the budget's; a loss is not).
fn assert_no_committed_row_lost(tag: &str, lines: usize, all: &[String]) {
    for i in 0..lines {
        let line = format!("t-{i:02}");
        assert!(
            all.iter().any(|r| r.trim_end() == line),
            "{tag}: {line} lost:\n{}",
            all.join("\n")
        );
    }
}

/// The scroll-on-clear race (the verifier's ~2/85): tmux grows the screen — pulling history
/// rows back, the frame and the cursor moving down with them — between a cursor answer and
/// the erase that follows it. An erase from the frame's top as an ABSOLUTE row wiped the rows
/// pulled onto it; one counted up from the cursor lands on the frame.
#[test]
fn a_grow_after_the_recreations_check_erases_no_committed_row() {
    for cursor in [Some((2, 6)), None] {
        let (mut t, buf, geo, emu, frame) = raced_term(40, cursor);
        emu.lock().unwrap().feed(&buf.bytes());
        // Queries: the pass's first (0), its post-placement recheck (1), the next pass's (2).
        resize_after_query(2, (80, 26), &emu, &buf, &geo);
        pass(&mut t, &frame, (80, 24));
        pass(&mut t, &frame, (80, 26));
        let (all, _) = emu.lock().unwrap().finish(&buf.bytes());
        assert_no_committed_row_lost(&format!("race, cursor {cursor:?}"), 40, &all);
    }
}

/// 1b (the verifier's streaming height drag, 6/6): the pass's own erase — the band a flush
/// frame leaves when it goes down onto the floor — ran on the row the cursor query named,
/// and tmux grew in between: the committed rows pulled onto that row were erased. Every
/// cursor query a pass makes is raced here, with the frame going down onto the floor (the
/// status row wrapped below the cursor, which moved up a row).
#[test]
fn a_grow_between_the_cursor_query_and_the_band_erase_loses_no_row() {
    for nth in 0..3 {
        for cursor in [Some((2, 6)), None] {
            let (mut t, buf, geo, emu, frame) = raced_term(40, cursor);
            emu.lock().unwrap().feed(&buf.bytes());
            emu_resize(&emu, &buf, &geo, (60, 24));
            resize_after_query(nth, (60, 27), &emu, &buf, &geo);
            pass(&mut t, &frame, (60, 24));
            pass(&mut t, &frame, (60, 27));
            let (all, _) = emu.lock().unwrap().finish(&buf.bytes());
            assert_no_committed_row_lost(&format!("query {nth}, cursor {cursor:?}"), 40, &all);
        }
    }
}

/// 1c (the verifier's DSR proxy, 2/2): with the cursor query failing, a height round trip in
/// tmux must lose no row — without the cursor nothing ABOVE it is erased.
#[test]
fn a_height_round_trip_without_a_cursor_answer_loses_no_row() {
    for cursor in [Some((2, 6)), None] {
        for lines in [19, 40] {
            let (mut t, buf, geo, emu, frame) = raced_term(lines, cursor);
            emu.lock().unwrap().feed(&buf.bytes());
            geo.set_dsr_fails(true);
            for h in [27, 24, 20, 24, 29, 24] {
                emu_resize(&emu, &buf, &geo, (80, h));
                pass(&mut t, &frame, (80, h));
            }
            let (all, _) = emu.lock().unwrap().finish(&buf.bytes());
            assert_no_committed_row_lost(
                &format!("dsr fails, {lines} lines, cursor {cursor:?}"),
                lines,
                &all,
            );
        }
    }
}
