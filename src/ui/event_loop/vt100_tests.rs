#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP44 L2b — terminal semantics via vt100 (`TUI_TEST_PLAN` §L2b) plus the loop units.
//!
//! Drives the REAL loop writer stack (`Terminal<CrosstermBackend<W>>` over a shared
//! byte buffer, geometry answered synthetically) and feeds the emitted bytes to a
//! `vt100::Parser`, and proves the scroll-region byte shape (no per-insert full clears —
//! the no-flicker mechanism) of the one shipped build.
//! The W10 `IDLE_WAKE` liveness units and the W4 snapshot-implies-draw unit (the T-03
//! replacement) live here too — they assert on the real byte stream.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::ui::event_loop::{EventSource, Model};
use crate::ui::facade::{ProgressState, StatusData};
use crate::ui::msgs::UiMsg;
use crate::ui::region::RegionSnapshot;
use crossterm::event::Event;
use tokio_util::sync::CancellationToken;

const SPINNER_GLYPHS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// A cloneable byte sink shared between the terminal stack and the assertions.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Scripted [`EventSource`] over a channel (the loop's poll deadline still elapses
/// for real, so W10 timing is genuine).
struct ChannelEvents {
    rx: mpsc::Receiver<Event>,
    pending: Option<Event>,
}

impl EventSource for ChannelEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        if self.pending.is_some() {
            return Ok(true);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(e) => {
                self.pending = Some(e);
                Ok(true)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                Ok(false)
            }
        }
    }

    fn read(&mut self) -> io::Result<Event> {
        if let Some(e) = self.pending.take() {
            return Ok(e);
        }
        self.rx
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "no scripted event"))
    }
}

/// CSI-final byte counters (the spike's `CountWriter` idea, made assertions):
/// DECSTBM set/reset, in-region scrolls, erase-display.
#[derive(Default, Debug)]
struct EscCounts {
    sr_set: usize,
    sr_reset: usize,
    up: usize,
    down: usize,
    ed: usize,
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

/// A headless [`crate::ui::term::Term`] over a fresh shared buffer.
fn direct_term(
    view_height: u16,
    top: u16,
) -> (
    crate::ui::term::Term<SharedBuf>,
    SharedBuf,
    crate::ui::term::Geometry,
) {
    let geo = crate::ui::term::Geometry::new(80, 24);
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let t = crate::ui::term::Term::new(
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
    geo: crate::ui::term::Geometry,
    width: Arc<AtomicU16>,
    height: Arc<AtomicU16>,
    region: Arc<Mutex<crate::ui::region::Region>>,
    join: thread::JoinHandle<io::Result<()>>,
}

fn start_loop() -> LoopHarness {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let (tx, rx) = mpsc::channel();
    let region = Arc::new(Mutex::new(crate::ui::region::Region::new(
        crate::ui::region::Emit::Live {
            tx: Box::new(crate::ui::msgs::MailboxPublish(tx.clone())),
        },
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    let shared = crate::ui::event_loop::LoopShared {
        width: Arc::clone(&width),
        height: Arc::clone(&height),
        region: Arc::clone(&region),
    };
    let geo = crate::ui::term::Geometry::new(80, 24);
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    // start_top 19 = screen_h − the idle frame height (5 rows): the viewport starts
    // at the bottom, so the composer cursor row is constant from the first insert.
    let t = crate::ui::term::Term::new(Box::new(move || wtr.clone()), 1, 19, Some(geo.clone()))
        .unwrap();
    let (etx, erx) = mpsc::channel();
    let events = ChannelEvents {
        rx: erx,
        pending: None,
    };
    let join = thread::spawn(move || crate::ui::event_loop::run_loop(&rx, events, t, shared));
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

/// W4 `DRAW_WITH_INSERTS`: an iteration containing an insert batch ends with a draw +
/// cursor restore — the composer cursor cell is IDENTICAL before and after each
/// insert, and the idle frame carries no spinner glyph. Replaces Go's
/// `TestRegionSnapshotChangesView` cursor-bump mechanism (T-03 divergence).
// Go: model_test.go:515 (mechanism replaced — see TUI_DIVERGENCES T-03)
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

/// W5 `RESIZE_PASS_FIRST` + the flush law: a WIDTH change schedules
/// `region.flush_tail()` as a post-update job (tail → scrollback, the open preview
/// SURVIVES); a height-only change flushes nothing.
// Go: model_test.go:1125
#[test]
fn resize_flushes_staging_tail_loop() {
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

    // WIDTH change → the post-update job flushes the tail; the preview survives.
    h.geo.set_size(70, 24);
    h.etx.send(Event::Resize(70, 24)).unwrap();
    assert!(
        h.wait_until(Duration::from_secs(1), |h| {
            h.width.load(Ordering::Relaxed) == 70 && h.region.lock().unwrap().tail.is_empty()
        }),
        "width change did not flush the staging tail"
    );
    assert!(
        !h.region.lock().unwrap().label.is_empty(),
        "the open preview must survive the flush"
    );

    // HEIGHT-only change → no flush.
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
        vec!["tail-c".to_owned()],
        "height-only change must not flush the tail"
    );
    h.quit_and_join(Duration::from_secs(2));
}

/// Streaming inserts ride DECSTBM + in-region scrolls, and NO per-insert full clear
/// appears — erase-display comes only from the deliberate height-change clears (W3). The
/// spike's G1 byte proof, in-process.
#[test]
fn scroll_region_bytes_present_no_per_insert_ed() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')));
    for i in 0..8 {
        h.tx.send(UiMsg::Scrollback(vec![format!("line-{i:02}")]))
            .unwrap();
    }
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("line-07")),
        "inserts never landed:\n{}",
        h.contents()
    );
    let counts = scan(&h.buf.bytes());
    h.quit_and_join(Duration::from_secs(2));
    assert!(
        counts.sr_set >= 4,
        "scroll-region traffic missing (feature inert?): {counts:?}"
    );
    assert!(
        counts.ed <= 4,
        "per-insert full clears leaked (flicker mechanism): {counts:?} for 8 inserts"
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
    assert_eq!(
        t.top, 20,
        "frame anchor must stay pinned at the bottom (W2)"
    );

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

    // Scrollback reach is the wart-W9 story. The scrolling-regions path scrolls the
    // partial region 0..top, and a strict-DEC emulator — this vt100 crate — DISCARDS rows
    // scrolled out of a partial region instead of filing them in scrollback. tmux 3.7c
    // preserves them (spike G1); per-terminal hand verification is the release gate
    // (`docs/TUI-VERIFY.md` §2), and there is no fallback build — a discarding emulator
    // is a bug to fix. The visible window still holds the contiguous tail of the
    // oversized insert.
    for i in 10..30 {
        assert!(
            visible.contains(&format!("tall-{i:02}")),
            "visible tail row tall-{i:02} missing:\n{visible}"
        );
    }
}

/// The spike's G2 shrink law, in-process: recreation to a smaller inline height +
/// clear leaves ZERO ghost rows, and subsequent inserts walk the frame back down.
#[test]
fn shrink_recreation_walks_down_without_ghosts() {
    let (mut t, buf, _geo) = direct_term(4, 18);
    t.ensure_height(10).unwrap();
    let tall = crate::ui::frame::FrameView {
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

    // Close: recreate smaller (W1) + clear (W3), then output self-heals the freed rows.
    let small = crate::ui::frame::FrameView {
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

fn test_model() -> crate::ui::event_loop::Model {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let region = Arc::new(Mutex::new(crate::ui::region::Region::new(
        crate::ui::region::Emit::Test(Box::new(|_, _| {})),
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    crate::ui::event_loop::Model::new(crate::ui::event_loop::LoopShared {
        width,
        height,
        region,
    })
}

/// The busy state machine: detail rides the phase without touching its clock, a new
/// phase clears stale detail, detail at idle is dropped (the render half lives in
/// `tests/frame_goldens.rs`).
// Go: model_test.go:1010 (state-machine half)
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
        crate::ui::event_loop::IDLE_POLL_MAX,
        "fully idle must still poll finite (W10)"
    );

    m.apply(UiMsg::BusyOn("x".to_owned()));
    m.dirty = false;
    assert!(m.poll_deadline() <= crate::ui::event_loop::IDLE_POLL_MAX);
    assert!(m.spin_ticking, "busy must start the spinner chain");

    m.apply(UiMsg::Region(RegionSnapshot {
        label: "rendering…".to_owned(),
        ..RegionSnapshot::default()
    }));
    m.dirty = false;
    assert!(
        m.poll_deadline() <= crate::ui::event_loop::STREAM_POLL_CAP,
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
/// `中 文 一 行` for a padded CJK row; `LoopBackend::draw` drops the covered cells so the
/// insert path and ratatui's width-aware buffer diff agree.
// Go: (no Go twin — a ratatui backend artefact; DEVIATIONS3 `NEEDS: [WP44] term.rs`)
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
    let region = Arc::new(Mutex::new(crate::ui::region::Region::new(
        crate::ui::region::Emit::Test(Box::new(|_, _| {})),
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    Model::new(crate::ui::event_loop::LoopShared {
        width,
        height,
        region,
    })
}

fn text(buf: &SharedBuf) -> String {
    String::from_utf8_lossy(&buf.bytes()).into_owned()
}

// Go: internal/ui/progress_test.go:13 TestProgressStates — the facade's states reach the
// wire as OSC 9;4 sequences, emitted ON CHANGE only, and `ProgressNone` clears the bar. Go
// asserted the bubbletea `View`'s ProgressBar because its renderer owned emission; the Rust
// loop IS the renderer, so the assertion is the byte stream itself. `\x1b[?1004h` at the
// head proves `run_loop` (not `Term::new`) turned focus reporting on.
#[test]
fn test_progress_states() {
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

// Go: internal/ui/progress_test.go:44 TestNotifyFocusGating — silent while focused (whoever
// is watching needs no bell), one write carrying BOTH standard channels while blurred (the
// OSC 9 notification, whose BEL only terminates the sequence, then a ringing BEL), and a
// digest opening `"4;"` defused by a leading space so it cannot parse as a progress report.
//
// Driven through `Model` + a real `Term` rather than the loop thread: the gate's whole point
// is the ORDER of a focus event against a ping, which a two-channel harness cannot pin.
#[test]
fn test_notify_focus_gating() {
    let (mut t, buf, _geo) = direct_term(1, 19);
    let mut m = notify_model();

    let ping = |m: &mut Model, t: &mut crate::ui::term::Term<SharedBuf>, s: &str| {
        m.apply(UiMsg::Notify(s.to_owned()));
        crate::ui::event_loop::drain_notify(m, t).unwrap();
    };

    let before = buf.bytes().len();
    ping(&mut m, &mut t, "approval needed"); // focused (the default): silent
    assert_eq!(
        buf.bytes().len(),
        before,
        "a focused terminal got {:?}, want silence",
        text(&buf)
    );

    m.handle_event(Event::FocusLost, &mut t).unwrap();
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

    m.handle_event(Event::FocusGained, &mut t).unwrap();
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
