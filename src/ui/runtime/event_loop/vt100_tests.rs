#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP44 L2b — terminal semantics via vt100 (`TUI_TEST_PLAN` §L2b) plus the loop units.
//!
//! Drives the REAL loop writer stack (`Term` over the full screen, a shared byte buffer, a size
//! the test names) and feeds the emitted bytes to a `vt100::Parser`, which models the alternate
//! screen (`?1049`). The full screen's own mechanics — the buffer, the anchor, the scroll region,
//! the search, the write-out — are pinned in `screen/tests.rs` and its siblings; this file pins
//! what the loop does through it: the W10 `IDLE_WAKE` liveness units, the snapshot-implies-draw
//! unit (the T-03 replacement), X-55's sync rule on whole writes, wide graphemes, the OSC
//! sequences, the exit, and a resize as one repaint with no cursor query anywhere.

use crate::ui::block::Block;
use std::io::{self};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Committed rows as the buffer's blocks.
fn rows_of(lines: &[String]) -> Vec<Block> {
    Block::rows(lines.iter().cloned())
}

use ratatui::layout::Size;

use crate::ui::facade::{ProgressState, StatusData};
use crate::ui::render::region::RegionSnapshot;
use crate::ui::runtime::event_loop::Model;
use crate::ui::runtime::msgs::UiMsg;
use crate::ui::runtime::term::Term;
use crate::ui::testutil::{ChannelEvents, SPINNER_GLYPHS, SharedBuf, test_model};
use crossterm::event::Event;
use tokio_util::sync::CancellationToken;

/// Replays every byte written so far into an 80×24 emulator.
fn parse(buf: &SharedBuf) -> vt100::Parser {
    parse_sized(buf, 80, 24)
}

/// [`parse`] into a `w`×`h` emulator — the size the terminal has at the end. A resize repaints
/// every cell after a clear, so what was drawn at an earlier size leaves nothing behind.
fn parse_sized(buf: &SharedBuf, w: u16, h: u16) -> vt100::Parser {
    let mut p = vt100::Parser::new(h, w, 500);
    p.process(&buf.bytes());
    p
}

/// A headless [`Term`] at 80×24 over a fresh shared buffer.
fn direct_term() -> (Term<SharedBuf>, SharedBuf) {
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let t = Term::headless(Box::new(move || wtr.clone()), Size::new(80, 24), false).unwrap();
    (t, buf)
}

/// The running loop under test: real `run_loop` on its own thread, scripted events, all bytes
/// captured.
struct LoopHarness {
    tx: mpsc::Sender<UiMsg>,
    etx: mpsc::Sender<Event>,
    buf: SharedBuf,
    width: Arc<AtomicU16>,
    height: Arc<AtomicU16>,
    region: Arc<Mutex<crate::ui::render::region::Region>>,
    join: thread::JoinHandle<io::Result<()>>,
    /// What the terminal's (recording) clipboard was handed.
    copied: Arc<Mutex<Vec<String>>>,
}

/// The loop at 80×24, its region the live one's (committed rows go straight to the
/// conversation).
fn start_loop() -> LoopHarness {
    start_loop_mouse(false)
}

/// [`start_loop`] with the config's `mouse:` as given.
fn start_loop_mouse(mouse: bool) -> LoopHarness {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let (tx, rx) = mpsc::channel();
    let region = Arc::new(Mutex::new(crate::ui::render::region::Region::new(
        crate::ui::render::region::Emit::Live {
            tx: Box::new(crate::ui::runtime::msgs::MailboxPublish(tx.clone())),
        },
    )));
    let shared = crate::ui::runtime::event_loop::LoopShared {
        width: Arc::clone(&width),
        height: Arc::clone(&height),
    };
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let t = Term::headless(Box::new(move || wtr.clone()), Size::new(80, 24), mouse).unwrap();
    let copied = t.copied();
    let (etx, erx) = mpsc::channel();
    let events = ChannelEvents::new(erx);
    let join =
        thread::spawn(move || crate::ui::runtime::event_loop::run_loop(&rx, events, t, shared));
    LoopHarness {
        tx,
        etx,
        buf,
        width,
        height,
        region,
        join,
        copied,
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

    /// Waits until the loop has written everything owed for what happened before the call: a
    /// fresh title goes through the mailbox (FIFO, behind every message sent before it) and is
    /// written in the ONE batch of the iteration that drains it, after every earlier
    /// iteration's. Its OSC in the bytes is that write landed, whatever the load.
    fn sync(&self) {
        static SYNCS: AtomicUsize = AtomicUsize::new(0);
        let token = format!("<sync {}>", SYNCS.fetch_add(1, Ordering::Relaxed));
        self.tx.send(UiMsg::Title(token.clone())).unwrap();
        let written = |h: &Self| String::from_utf8_lossy(&h.buf.bytes()).contains(&token);
        assert!(self.wait_until(Duration::from_secs(10), written), "{token}");
    }

    /// Sends a resize to `w`×`h` and waits until the repaint it causes has landed: the size is
    /// stored at once, and the frame is drawn once the terminal has been quiet for `RESIZE_QUIET`.
    fn resize(&self, w: u16, h: u16) {
        self.etx.send(Event::Resize(w, h)).unwrap();
        let taken = |lh: &Self| {
            lh.width.load(Ordering::Relaxed) == w && lh.height.load(Ordering::Relaxed) == h
        };
        assert!(
            self.wait_until(Duration::from_secs(5), taken),
            "{w}x{h} never taken"
        );
        self.sync();
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

/// W10 `IDLE_WAKE` unit 1: a mailbox message posted while the loop is FULLY idle (no
/// events, no timers) renders within one finite poll deadline — no wake channel needed.
#[test]
fn idle_wake_renders_mailbox_message_within_deadline() {
    let h = start_loop();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')),
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
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    thread::sleep(Duration::from_millis(150)); // fully idle
    h.quit_and_join(Duration::from_secs(1));
}

/// The exit hands the conversation to the main screen once — X-49's duplicate (a frame left
/// standing under rows the flush had already sent to the history) has nothing to stand on now.
/// `close()` flushes the window and THEN posts `Quit`, on the one mailbox; the loop leaves the
/// alternate screen and writes every committed row out under the transcript mark: the banner
/// once, no frame row.
#[test]
fn the_exit_writes_the_conversation_out_once_and_no_frame() {
    use crate::ui::runtime::screen::TRANSCRIPT_MARK;
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    h.region
        .lock()
        .unwrap()
        .commit(vec!["banner-row".to_owned()]);
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h
            .contents()
            .contains("banner-row")),
        "the committed row never rendered:\n{}",
        h.contents()
    );
    // What `close()` does: Quit.
    let buf = h.buf.clone();
    h.quit_and_join(Duration::from_secs(2));
    let mut p = parse(&buf);
    assert!(!p.screen().alternate_screen(), "the main screen is back");
    let all = reachable(&mut p);
    let dump = all.join("\n");
    assert_eq!(
        all.iter().filter(|r| r.contains("banner-row")).count(),
        1,
        "{dump}"
    );
    assert!(
        !all.iter().any(|r| r.contains('─')),
        "a frame row left behind:\n{dump}"
    );
    let row_of = |needle: &str| all.iter().position(|l| l.contains(needle));
    assert!(row_of(TRANSCRIPT_MARK) < row_of("banner-row"), "{dump}");
}

/// W4's law, kept: an iteration that lands committed rows ends with a draw + cursor restore — the
/// composer cursor cell is IDENTICAL before and after each batch of rows, and the idle frame
/// carries no spinner glyph. Replaces Go's `TestRegionSnapshotChangesView` cursor-bump
/// mechanism (T-03 divergence).
#[test]
fn snapshot_implies_draw_and_cursor_restored() {
    let h = start_loop();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    assert!(
        h.wait_until(Duration::from_secs(1), |h| h.cursor().1 == 0),
        "composer cursor never settled at column 0 (no prompt glyph): {:?}",
        h.cursor()
    );
    let pos0 = h.cursor();

    for line in ["insert-alpha", "insert-beta"] {
        h.tx.send(UiMsg::Scrollback(Block::rows([line]))).unwrap();
        h.tx.send(UiMsg::Region(RegionSnapshot::default())).unwrap();
        assert!(
            h.wait_until(Duration::from_secs(1), |h| h.contents().contains(line)
                && h.cursor() == pos0),
            "{line}: the row landed but the cursor was not restored to {pos0:?} \
             (now {:?}) — the iteration must end with a draw",
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

/// The four-row idle frame with `draft` typed, the cursor after it.
fn composer(draft: &str) -> crate::ui::render::frame::FrameView {
    crate::ui::render::frame::FrameView {
        lit: Vec::new(),
        float: 0,
        rows: vec![
            "─".repeat(80),
            draft.to_owned(),
            "─".repeat(80),
            "status".to_owned(),
        ],
        cursor: Some((u16::try_from(draft.len()).unwrap(), 1)),
        map: crate::ui::render::frame::FrameMap::default(),
    }
}

/// T-01 characterization: a row whose width is EXACTLY the terminal width shows as exactly ONE
/// row with its last column intact — the `sanitizeOverflow` hazard class does not exist.
#[test]
fn exact_width_line_shows_as_a_single_row() {
    let (mut t, buf) = direct_term();
    let full = "x".repeat(80);
    t.push(Block::rows([full.clone()]));
    t.draw_frame(&composer(""), None).unwrap();
    let p = parse(&buf);
    let contents = p.screen().contents();
    let hits = contents.lines().filter(|l| *l == full).count();
    assert_eq!(
        hits, 1,
        "exact-width line must show as exactly one row:\n{contents}"
    );
    let row = contents.lines().position(|l| l == full).unwrap();
    let cell = p.screen().cell(u16::try_from(row).unwrap(), 79).unwrap();
    assert_eq!(cell.contents(), "x", "right border eaten (T-01)");
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

/// DEC 2026 by SIZE (X-55) — the threshold's choice and the pairing, not a transport claim: a
/// write of at most `SYNC_MIN` bytes goes out bare: a keystroke's composer redraw, a line of
/// output and its frame, a clear, a whole small batch, an OSC. A write larger than that goes out
/// as exactly ONE synchronized update around all its bytes: twelve wide rows of output, a
/// `/model`-sized frame opening. The criterion is the size of what one `write_all` carries, never
/// the terminal; a draw outside the loop's batch (the session picker's) is one write too. Whether
/// a transport splits a write is not tested here (nothing can pin it), and vt100 ignores the
/// mode, so every replay in this file runs through the same bytes.
#[test]
fn a_write_is_one_synchronized_update_exactly_when_it_is_longer_than_sync_min() {
    use crate::ui::runtime::screen::SYNC_MIN;
    let (mut t, buf) = direct_term();
    let view = |rows: Vec<String>, cursor: (u16, u16)| crate::ui::render::frame::FrameView {
        lit: Vec::new(),
        float: 0,
        rows,
        cursor: Some(cursor),
        map: crate::ui::render::frame::FrameMap::default(),
    };
    // One write, its size, its pairs; the rule: wrapped exactly when longer than SYNC_MIN.
    let write = |t: &mut Term<SharedBuf>, op: &dyn Fn(&mut Term<SharedBuf>)| -> (usize, usize) {
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
    write(&mut t, &|t| {
        t.draw_frame(&composer(""), None).unwrap();
    });
    // Small: a keystroke, a line of output, a clear, a small batch, an OSC.
    let (n, pairs) = write(&mut t, &|t| {
        t.draw_frame(&composer("h"), None).unwrap();
    });
    assert!(
        pairs == 0 && n < 64,
        "a keystroke echo is a few bytes, bare: {n} B"
    );
    let (_, pairs) = write(&mut t, &|t| {
        t.begin_batch();
        t.push(rows_of(&["one line of output".to_owned()]));
        t.draw_frame(&composer("h"), None).unwrap();
        t.end_batch().unwrap();
    });
    assert_eq!(pairs, 0, "a line of output is bare");
    write(&mut t, &|t| t.clear().unwrap());
    t.draw_frame(&composer("h"), None).unwrap();
    let (_, pairs) = write(&mut t, &|t| {
        t.begin_batch();
        t.set_title("small").unwrap();
        t.set_progress(ProgressState::Busy).unwrap();
        t.draw_frame(&composer("hi"), None).unwrap();
        t.end_batch().unwrap();
    });
    assert_eq!(pairs, 0, "a small batch is bare, the OSCs in it");
    let (_, pairs) = write(&mut t, &|t| t.set_title("bare").unwrap());
    assert_eq!(pairs, 0, "an OSC alone is bare");
    // Large: twelve wide rows of output; a /model-sized frame opening.
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
        t.begin_batch();
        t.push(rows_of(&wide));
        t.draw_frame(&composer("hi"), None).unwrap();
        t.end_batch().unwrap();
    });
    assert!(
        pairs == 1 && n > SYNC_MIN,
        "twelve wide rows are one block: {n} B"
    );
    let panel: Vec<String> = (0..16)
        .map(|i| format!("  model-{i:02} {}", "·".repeat(60)))
        .collect();
    let (n, pairs) = write(&mut t, &|t| {
        t.draw_frame(&view(panel.clone(), (2, 1)), None).unwrap();
    });
    assert!(
        pairs == 1 && n > SYNC_MIN,
        "a panel opening is one block: {n} B"
    );
    let screen = parse(&buf).screen().contents();
    assert!(screen.contains("model-15"), "{screen}");
}

/// The spike's G2 shrink law, on the full screen: a frame that gets shorter (a surface closing)
/// leaves no ghost of the rows it gave up — the conversation takes them — and the composer is on
/// the bottom again.
#[test]
fn a_frame_that_shrinks_leaves_no_ghost() {
    let (mut t, buf) = direct_term();
    let tall = crate::ui::render::frame::FrameView {
        lit: Vec::new(),
        float: 0,
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
        map: crate::ui::render::frame::FrameMap::default(),
    };
    t.draw_frame(&tall, None).unwrap();
    assert!(
        parse(&buf).screen().contents().contains("SURFGHOST-row"),
        "tall frame never painted"
    );
    t.push(rows_of(&["after-one".to_owned()]));
    t.draw_frame(&composer(""), None).unwrap();
    let p = parse(&buf);
    let contents = p.screen().contents();
    assert!(
        !contents.contains("SURFGHOST") && !contents.contains("row-"),
        "ghost rows survived the shrink:\n{contents}"
    );
    let rows: Vec<&str> = contents.lines().map(str::trim_end).collect();
    assert_eq!(
        rows.first(),
        Some(&"after-one"),
        "a short conversation starts at the top: {contents}"
    );
    assert_eq!(
        p.screen().cursor_position(),
        (21, 0),
        "the composer on the bottom"
    );
}

/// Wide graphemes survive the conversation. A wide grapheme's continuation cell reads back as a
/// SPACE, so a backend handed the WHOLE buffer would print `中 文 一 行` for a padded CJK row;
/// every write goes through ratatui's width-aware buffer diff, which drops the covered cells.
// A ratatui backend artefact with no Go twin.
#[test]
fn wide_runes_show_intact() {
    let (mut t, buf) = direct_term();
    // The user-echo shape: a padded, styled row (the padding is what makes the row's trailing
    // cells non-blank, which is where a fallback path would desync).
    let row = format!("▌\x1b[7m 中文一行{}\x1b[0m", " ".repeat(80 - 2 - 8));
    t.push(Block::rows([row.clone()]));
    // The unpadded markdown shape must stay intact too.
    t.push(rows_of(&["中文一行 tail".to_owned()]));
    t.draw_frame(&composer(""), None).unwrap();
    let screen = parse(&buf).screen().contents();
    assert!(
        screen.contains("▌ 中文一行"),
        "wide runes split:\n{screen:?}"
    );
    assert!(
        screen.contains("中文一行 tail"),
        "wide runes split in an unpadded row:\n{screen:?}"
    );
    assert!(
        !screen.contains("中 文"),
        "a continuation cell leaked a spurious space:\n{screen:?}"
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

/// The whole-turn layer (docs/design/busy-span.md §3.1): inside a turn the busy segment's clock is the
/// TURN's — a phase starting, ending or changing never resets it, only relabels the segment, which falls
/// back to `TURN_LABEL` between phases — and a wait on the user stands it still. The turn ending takes the
/// layer down, leaving a running job's static segment alone: no spinner chain outlives the turn.
#[test]
fn the_turn_layer_owns_the_busy_clock() {
    use crate::ui::facade::{TURN_LABEL, TurnBusy};
    let mut m = test_model();
    m.apply(UiMsg::Turn(TurnBusy::Running));
    assert!(m.spin_ticking, "a turn starts the spinner chain");
    let later = Instant::now() + Duration::from_secs(10);
    let at_start = m.busy_view(later).expect("the turn's segment");
    assert_eq!(
        at_start.label, TURN_LABEL,
        "between phases the turn waits on the model"
    );
    let clock = |v: &crate::ui::render::frame::BusyView| later.duration_since(v.since);
    assert!(clock(&at_start) >= Duration::from_secs(10));

    for label in ["Waiting for the first token", "Responding", "Running tools"] {
        m.apply(UiMsg::BusyOff);
        m.apply(UiMsg::BusyOn(label.to_owned()));
        let v = m.busy_view(later).unwrap();
        assert_eq!(v.label, label);
        assert_eq!(
            v.since, at_start.since,
            "{label}: a phase change reset the turn's clock"
        );
    }
    m.apply(UiMsg::BusyOff);
    let v = m.busy_view(later).unwrap();
    assert_eq!(
        v.label, TURN_LABEL,
        "a phase ending hands the segment back to the turn"
    );
    assert_eq!(v.since, at_start.since);

    // Waiting on the user: the figure stands still however long the wait.
    m.apply(UiMsg::Turn(TurnBusy::Waiting));
    let frozen = clock(&m.busy_view(later).unwrap());
    let much_later = later + Duration::from_secs(600);
    let v = m.busy_view(much_later).unwrap();
    assert_eq!(
        much_later.duration_since(v.since),
        frozen,
        "the clock ran while waiting"
    );
    m.apply(UiMsg::Turn(TurnBusy::Running));

    // The turn ends with a job still running: the segment goes, the job's stays, and nothing spins.
    m.apply(UiMsg::Jobs(vec![crate::shell::jobs::JobInfo {
        id: "b1".to_owned(),
        command: "sleep 600".to_owned(),
        pid: None,
        started: Instant::now(),
        output_path: std::path::PathBuf::from("/tmp/b1.log"),
    }]));
    m.apply(UiMsg::Turn(TurnBusy::Off));
    assert!(
        m.busy_view(later).is_none(),
        "the turn layer outlived the turn"
    );
    m.tick_spin();
    assert!(!m.spin_ticking, "a job alone keeps no spinner (point 5, B)");
    let status = m.frame_view().rows.last().cloned().unwrap();
    let status = crate::text::ansi::strip_sgr(&status);
    assert!(status.contains("job b1 sleep 600"), "{status:?}");
    assert!(
        !SPINNER_GLYPHS.chars().any(|g| status.contains(g)) && !status.contains("ESC"),
        "the job segment is static: {status:?}"
    );

    // Outside a turn a phase keeps its own clock (`/model`'s listing).
    m.apply(UiMsg::BusyOn("Fetching available models…".to_owned()));
    let v = m.busy_view(later).unwrap();
    assert_eq!(v.since, m.busy.as_ref().unwrap().since);
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

// ------------------------------------------------------------------
// T3 — OSC 9;4 progress, OSC 9 notify, focus reporting (WP67)
// ------------------------------------------------------------------

/// A loop model over a test-seam region at 80x24 (the `queue_tests` twin, reused here
/// so the notify gate can be driven without racing the loop thread).
fn notify_model() -> Model {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    Model::new(crate::ui::runtime::event_loop::LoopShared { width, height })
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
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
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
    let (mut t, buf) = direct_term();
    let mut m = notify_model();

    let ping = |m: &mut Model, t: &mut Term<SharedBuf>, s: &str| {
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
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
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
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
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
    let (t, buf) = direct_term();
    drop(t);
    let out = text(&buf);
    assert!(!out.contains("[?1004"), "unexpected focus mode: {out:?}");
    assert!(!out.contains("]9;4"), "unexpected progress bytes: {out:?}");
}

// ---------------------------------------------------------------------------
// W5 resize: iota re-anchors; ratatui's resize (row 0 + ESC[2J) never runs
// ---------------------------------------------------------------------------

/// Sends `keys` to the loop one by one and returns the composer row (the screen row under the
/// upper separator — the composer has no glyph of its own) once it reads `want`, or the last one
/// seen.
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
            .skip_while(|r| !r.starts_with('─'))
            .nth(1)
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
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    let (row, _) = composer_after(&lh, &typed("中文测试"), "中文测试");
    assert_eq!(row, "中文测试");
    let bs = key(crossterm::event::KeyCode::Backspace);
    let (row, mark) = composer_after(&lh, std::slice::from_ref(&bs), "中文测");
    assert_eq!(row, "中文测", "one Backspace must remove exactly 试");
    assert_erases_on_boundaries(&lh, mark, &row);
    let (row, _) = composer_after(&lh, &[bs.clone(), bs], "中");
    assert_eq!(row, "中");
    lh.quit_and_join(Duration::from_secs(2));
}

#[test]
fn deleting_inside_a_wide_row_keeps_the_rest_whole() {
    use crossterm::event::KeyCode;
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    composer_after(&lh, &typed("中文测试"), "中文测试");
    let (row, _) = composer_after(
        &lh,
        &[
            key(KeyCode::Left),
            key(KeyCode::Left),
            key(KeyCode::Backspace),
        ],
        "中测试",
    );
    assert_eq!(row, "中测试", "← ← Backspace removes 文 only");
    let (row, _) = composer_after(&lh, &[key(KeyCode::Home), key(KeyCode::Delete)], "测试");
    assert_eq!(row, "测试", "Home Delete removes 中 only");
    lh.quit_and_join(Duration::from_secs(2));
}

/// A wide emoji at the row's end. (vt100 does not join a skin-tone modifier onto its base —
/// it would show `👍🏽` as `👍` — so the modifier case is the unit test's,
/// `term::tests::a_row_ending_in_a_wide_grapheme_is_as_wide_as_its_right_edge`.)
#[test]
fn backspace_after_a_wide_emoji_keeps_it_whole() {
    use crossterm::event::KeyCode;
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    let (row, _) = composer_after(&lh, &typed("a👍x"), "a👍x");
    assert_eq!(row, "a👍x");
    let (row, mark) = composer_after(&lh, &[key(KeyCode::Backspace)], "a👍");
    assert_eq!(row, "a👍", "Backspace removes x only");
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
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    lh.etx
        .send(Event::Paste("文字\n第二行".to_owned()))
        .unwrap();
    assert!(lh.wait_until(Duration::from_secs(1), |h| h.contents().contains("[#1")));
    let tag = "[#1 文字… 2 lines]";
    composer_after(&lh, &typed("中文"), &format!("{tag}中文"));
    let (row, mark) = composer_after(&lh, &[key(KeyCode::Backspace)], &format!("{tag}中"));
    assert_eq!(row, format!("{tag}中"), "Backspace removes 文 only");
    assert_erases_on_boundaries(&lh, mark, &row);
    lh.quit_and_join(Duration::from_secs(2));
}

/// A queued two-line draft (Ctrl+J between the lines, X-65) keeps ONE frame row: the newline
/// shows as ` ⏎ `, the hint stays on that row, and the separator under it is whole. Without
/// the ` ⏎ ` the frame does not tear — the ratatui buffer drops the `\n` — but the row reads
/// `» ab`, two lines glued into a word that was never typed.
#[test]
fn a_queued_multi_line_draft_keeps_one_frame_row() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    let mut evs = typed("a");
    evs.push(Event::Key(KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL,
    )));
    evs.extend(typed("b"));
    evs.push(key(KeyCode::Enter)); // no reader parked → queued
    for ev in evs {
        lh.etx.send(ev).unwrap();
    }
    assert!(
        lh.wait_until(Duration::from_secs(2), |h| h.contents().contains("» a ⏎ b")),
        "the queue row never showed:\n{}",
        lh.contents()
    );
    lh.sync();
    let screen = lh.contents();
    let rows: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let q = rows
        .iter()
        .position(|r| r.contains("» a ⏎ b"))
        .expect("queue row");
    assert!(
        rows[q].ends_with("» a ⏎ b · ↑ edit"),
        "the hint must stay on the item's row:\n{screen}"
    );
    let sep = crate::ui::render::frame::SEPARATOR_GLYPH.repeat(80);
    assert_eq!(
        rows[q + 1],
        sep,
        "the separator under the queue is torn:\n{screen}"
    );
    assert_eq!(
        rows.iter().filter(|r| **r == sep).count(),
        2,
        "want both separators whole:\n{screen}"
    );
    assert_eq!(
        (rows[q + 2], rows[q + 3]),
        ("", sep.as_str()),
        "the empty composer row sits between the separators:\n{screen}"
    );
    lh.quit_and_join(Duration::from_secs(2));
}

// ---------------------------------------------------------------------------
// Resize: the size taken, every cell repainted; the terminal never asked
// ---------------------------------------------------------------------------

/// The loop with twelve committed lines at idle (all of them in the conversation).
fn idle_loop() -> LoopHarness {
    let lh = start_loop();
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains('─')));
    lh.region
        .lock()
        .unwrap()
        .commit((0..12).map(|i| format!("line-{i:02}")).collect());
    assert!(lh.wait_until(Duration::from_secs(2), |h| h.contents().contains("line-11")));
    lh.sync(); // the loop is idle from here
    lh
}

/// The rows of the screen `p` shows, trailing blanks trimmed.
fn grid(p: &vt100::Parser) -> Vec<String> {
    let cols = p.screen().size().1;
    p.screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

/// A resize — narrower, shorter, then wider and taller — is the size taken and every cell
/// repainted at it: the full width at once (no column held back while a "drag" lasts), the frame
/// on the bottom row, each committed line on the screen once, nothing of the old layout left
/// (X-52's residuals have no mechanism here). The exit then writes every line out once, at the
/// width the terminal has then.
#[test]
fn a_resize_repaints_every_cell_at_the_new_size() {
    let lh = idle_loop();
    let p = parse(&lh.buf);
    let g = grid(&p);
    let composer_from_bottom = g.len() - g.iter().position(|r| r.starts_with('─')).unwrap();
    for (w, h) in [(60, 20), (100, 30)] {
        lh.resize(w, h);
        assert_eq!(
            lh.width.load(Ordering::Relaxed),
            w,
            "the full width at once"
        );
        let p = parse_sized(&lh.buf, w, h);
        let g = grid(&p);
        let dump = g.join("\n");
        assert_eq!(g.len(), usize::from(h));
        let frame = g.iter().position(|r| r.starts_with('─')).unwrap();
        assert_eq!(
            g.len() - frame,
            composer_from_bottom,
            "{w}x{h}: the frame left the bottom:\n{dump}"
        );
        for i in 0..12 {
            let name = format!("line-{i:02}");
            let n = g.iter().filter(|r| **r == name).count();
            assert_eq!(n, 1, "{w}x{h}: {name} shown {n} times:\n{dump}");
        }
        let sep = crate::ui::render::frame::SEPARATOR_GLYPH.repeat(usize::from(w));
        assert_eq!(
            g.iter().filter(|r| **r == sep).count(),
            2,
            "{w}x{h}: whole separators:\n{dump}"
        );
    }
    let buf = lh.buf.clone();
    lh.quit_and_join(Duration::from_secs(2));
    let mut p = parse_sized(&buf, 100, 30);
    let all = reachable(&mut p);
    for i in 0..12 {
        let name = format!("line-{i:02}");
        let n = all.iter().filter(|r| r.trim_end() == name).count();
        assert_eq!(n, 1, "{name} written out {n} times:\n{}", all.join("\n"));
    }
}

/// The verifier's `/model` repro, on the full screen: a resize while a surface is open (cursor
/// hidden, the frame taller than the idle one, its rows wide) leaves one whole frame — never half
/// of the old one in the middle of the conversation.
#[test]
fn a_resize_with_a_surface_open_leaves_one_whole_frame() {
    let (mut t, buf) = direct_term();
    let history: Vec<String> = (0..6).map(|i| format!("hist-{i}")).collect();
    t.push(rows_of(&history));
    let view = |w: usize| crate::ui::render::frame::FrameView {
        lit: Vec::new(),
        float: 0,
        rows: [
            String::new(),
            "─".repeat(w),
            "draft".to_owned(),
            "─".repeat(w),
        ]
        .into_iter()
        .chain((0..4).map(|i| format!("model-{i} {}", "·".repeat(w - 12))))
        .collect(),
        cursor: None,
        map: crate::ui::render::frame::FrameMap::default(),
    };
    t.draw_frame(&view(80), None).unwrap();
    t.resize(Size::new(60, 24));
    t.draw_frame(&view(60), None).unwrap();
    let p = parse_sized(&buf, 60, 24);
    let g = grid(&p);
    let dump = g.join("\n");
    assert_eq!(g.iter().filter(|r| *r == "draft").count(), 1, "{dump}");
    assert_eq!(g.iter().filter(|r| r.contains('─')).count(), 2, "{dump}");
    for h in &history {
        let n = g.iter().filter(|r| r.contains(h.as_str())).count();
        assert_eq!(n, 1, "{h} shown {n} times:\n{dump}");
    }
    assert!(
        g[23].starts_with("model-3"),
        "the frame left the bottom:\n{dump}"
    );
    assert!(
        g.iter().all(|r| r.chars().count() <= 60),
        "a row wider than the screen:\n{dump}"
    );
}

/// The loop never asks the terminal where its cursor is — not at startup, not on a resize, not on
/// the way out (C3: inline asked once at startup and on every resize, and could not start without
/// an answer). A whole session's bytes carry no DSR (`ESC[6n`). (The OSC 11 and OSC 7501 probes
/// before the loop starts chase their queries with one, as a read terminator: `osc.rs`.)
#[test]
fn no_cursor_query_is_ever_sent() {
    let lh = idle_loop();
    for ev in typed("hello") {
        lh.etx.send(ev).unwrap();
    }
    lh.resize(60, 20);
    lh.resize(90, 28);
    lh.tx
        .send(UiMsg::Scrollback(Block::rows(["after the resizes"])))
        .unwrap();
    lh.sync();
    let buf = lh.buf.clone();
    lh.quit_and_join(Duration::from_secs(2));
    let out = text(&buf);
    assert!(!out.contains("\u{1b}[6n"), "a cursor query was written");
    assert!(
        out.contains("\u{1b}[?1049h") && out.contains("\u{1b}[?1049l"),
        "entered and left the alternate screen"
    );
}

/// The mouse end to end through the real loop: a drag over the conversation copies its text to
/// the clipboard, the status row says so with the native-selection key, and the wheel scrolls.
#[test]
fn a_drag_through_the_loop_copies_and_the_status_row_says_so() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let h = start_loop_mouse(true);
    let rows: Vec<String> = (0..40).map(|i| format!("row-{i:02}")).collect();
    h.tx.send(UiMsg::Scrollback(Block::rows(rows))).unwrap();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("row-39")),
        "{}",
        h.contents()
    );
    assert!(
        String::from_utf8_lossy(&h.buf.bytes()).contains("\x1b[?1002h"),
        "mouse reporting asked for"
    );
    let at = |kind, row| {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let rows_shown = parse(&h.buf).screen().contents();
    // The conversation's first row, the viewport's top edge: a press there that has not moved is a
    // click, so no loop iteration between it and the drag scrolls the view (`mouse::tick_edge`).
    let top = rows_shown
        .lines()
        .position(|l| l.starts_with("row-"))
        .unwrap();
    let top = u16::try_from(top).unwrap();
    h.etx
        .send(at(MouseEventKind::Down(MouseButton::Left), top))
        .unwrap();
    h.etx
        .send(at(MouseEventKind::Drag(MouseButton::Left), top + 2))
        .unwrap();
    h.etx
        .send(at(MouseEventKind::Up(MouseButton::Left), top + 2))
        .unwrap();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("copied")),
        "{}",
        h.contents()
    );
    let first = h
        .contents()
        .lines()
        .nth(usize::from(top))
        .unwrap()
        .trim()
        .to_owned();
    let copied = crate::sync::lock(&h.copied).clone();
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(
        copied[0].starts_with(&first) && copied[0].lines().count() == 3,
        "{copied:?}"
    );
    assert!(
        h.contents()
            .contains("(recorded) · Shift+drag selects in the terminal"),
        "the status row names the way: {}",
        h.contents()
    );
    h.etx.send(at(MouseEventKind::ScrollUp, 2)).unwrap();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h
            .contents()
            .contains("Ctrl+End")),
        "the wheel left the tail: {}",
        h.contents()
    );
    h.quit_and_join(Duration::from_secs(5));
}

/// The composer's selection on a WRAPPED draft at 80 columns, through the real loop: a drag from
/// the first row to the second selects across the wrap — both rows' cells reversed, nothing else
/// — the real cursor stays at the drag's end, the release copies exactly the selected text, and
/// the exit's write-out carries no reverse video.
#[test]
fn a_composer_selection_spans_the_wrap_and_the_write_out_stays_plain() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let h = start_loop_mouse(true);
    let draft: String = (0..100)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect();
    h.etx.send(Event::Paste(draft.clone())).unwrap();
    let rows_of_draft = |h: &LoopHarness| {
        let p = parse(&h.buf);
        let rows: Vec<String> = p.screen().rows(0, 80).collect();
        rows.iter().position(|r| *r == draft[..80])
    };
    assert!(
        h.wait_until(Duration::from_secs(2), |h| rows_of_draft(h).is_some()),
        "{}",
        h.contents()
    );
    let top = u16::try_from(rows_of_draft(&h).unwrap()).unwrap();
    let at = |kind, column, row| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    h.etx
        .send(at(MouseEventKind::Down(MouseButton::Left), 70, top))
        .unwrap();
    h.etx
        .send(at(MouseEventKind::Drag(MouseButton::Left), 10, top + 1))
        .unwrap();
    h.etx
        .send(at(MouseEventKind::Up(MouseButton::Left), 10, top + 1))
        .unwrap();
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("copied")),
        "{}",
        h.contents()
    );
    assert_eq!(*crate::sync::lock(&h.copied), [draft[70..90].to_owned()]);
    let p = parse(&h.buf);
    let lit: Vec<(u16, u16)> = (0..24)
        .flat_map(|r| (0..80).map(move |c| (r, c)))
        .filter(|&(r, c)| p.screen().cell(r, c).is_some_and(vt100::Cell::inverse))
        .collect();
    let want: Vec<(u16, u16)> = (70..80)
        .map(|c| (top, c))
        .chain((0..10).map(|c| (top + 1, c)))
        .collect();
    assert_eq!(lit, want, "both rows of the wrap reversed, nothing else");
    assert_eq!(h.cursor(), (top + 1, 10), "the cursor at the drag's end");
    let mark = h.buf.bytes().len();
    let buf = h.buf.clone();
    h.quit_and_join(Duration::from_secs(5));
    let bytes = buf.bytes();
    let out = String::from_utf8_lossy(&bytes[mark..]);
    let left = out
        .find("\u{1b}[?1049l")
        .expect("the exit left the alternate screen");
    assert!(
        !out[left..].contains("\u{1b}[7m") && !out[left..].contains(";7m"),
        "the write-out carries the selection: {:?}",
        &out[left..]
    );
}

/// A double-click on a WRAPPED draft at 80 columns, through the real loop: the whole draft is
/// reversed — all of its first row and the second up to its end, nothing else — and the release
/// copies all of it.
#[test]
fn a_composer_double_click_reverses_the_whole_wrapped_draft() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let h = start_loop_mouse(true);
    let draft: String = (0..100)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect();
    h.etx.send(Event::Paste(draft.clone())).unwrap();
    let rows_of_draft = |h: &LoopHarness| {
        let p = parse(&h.buf);
        let rows: Vec<String> = p.screen().rows(0, 80).collect();
        rows.iter().position(|r| *r == draft[..80])
    };
    assert!(
        h.wait_until(Duration::from_secs(2), |h| rows_of_draft(h).is_some()),
        "{}",
        h.contents()
    );
    let top = u16::try_from(rows_of_draft(&h).unwrap()).unwrap();
    for _ in 0..2 {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            h.etx
                .send(Event::Mouse(MouseEvent {
                    kind,
                    column: 5,
                    row: top + 1,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
        }
    }
    assert!(
        h.wait_until(Duration::from_secs(2), |h| h.contents().contains("copied")),
        "{}",
        h.contents()
    );
    assert_eq!(*crate::sync::lock(&h.copied), [draft.clone()]);
    let p = parse(&h.buf);
    let lit: Vec<(u16, u16)> = (0..24)
        .flat_map(|r| (0..80).map(move |c| (r, c)))
        .filter(|&(r, c)| p.screen().cell(r, c).is_some_and(vt100::Cell::inverse))
        .collect();
    let want: Vec<(u16, u16)> = (0..80)
        .map(|c| (top, c))
        .chain((0..20).map(|c| (top + 1, c)))
        .collect();
    assert_eq!(
        lit, want,
        "the whole draft reversed across the wrap, nothing else"
    );
    assert_eq!(h.cursor(), (top + 1, 20), "the cursor at the draft's end");
    h.quit_and_join(Duration::from_secs(5));
}

/// The copy status names what was copied, the ways it went — so a copy that reached only this
/// machine's clipboard never reads like one that reached the terminal's — and the native-selection
/// key; a text too long for OSC 52 that no tool took says so.
#[test]
fn the_copy_status_names_the_ways() {
    use crate::ui::runtime::clipboard::Copied;
    let mut m = test_model();
    let mut said = Vec::new();
    for (ways, osc_too_large) in [
        (&["pbcopy", "OSC 52"][..], false),
        (&["OSC 52"], false),
        (&["pbcopy"], false),
        (&["pbcopy"], true),
        (&["tmux"], false),
    ] {
        let ways = ways.iter().map(|w| (*w).to_owned()).collect();
        m.copy_done(Copied::Done {
            chars: 14,
            ways,
            osc_too_large,
        });
        said.push(m.copied.as_ref().map(|(t, _)| t.clone()).unwrap());
    }
    m.copy_done(Copied::TooLarge { chars: 1 });
    said.push(m.copied.as_ref().map(|(t, _)| t.clone()).unwrap());
    assert_eq!(
        said,
        [
            "copied 14 characters (pbcopy + OSC 52) · Shift+drag selects in the terminal",
            "copied 14 characters (OSC 52) · Shift+drag selects in the terminal",
            "copied 14 characters (pbcopy) · Shift+drag selects in the terminal",
            "copied 14 characters (pbcopy; too large for OSC 52) · Shift+drag selects in the terminal",
            "copied 14 characters (tmux) · Shift+drag selects in the terminal",
            "not copied: 1 character is too large — use Ctrl+O",
        ]
    );
}

/// C13: an open surface is drawn over the conversation's viewport, which yields it the rows — but
/// a long View stays under the interaction cap (half the 24-row screen: 12 rows), the
/// conversation keeps the rest, and closing it gives the viewport back, the conversation where it
/// was.
#[test]
fn a_surface_takes_the_interaction_cap_and_gives_the_viewport_back() {
    use crate::ui::facade::{Panel, TabbedSpec};
    let h = start_loop();
    let rows: Vec<String> = (0..40).map(|i| format!("conv-{i:02}")).collect();
    h.tx.send(UiMsg::Scrollback(Block::rows(rows))).unwrap();
    assert!(h.wait_until(Duration::from_secs(2), |h| h.contents().contains("conv-39")));
    let (reply, _rx) = tokio::sync::oneshot::channel();
    let lines: Vec<String> = (0..100).map(|i| format!("view-{i:03}")).collect();
    h.tx.send(UiMsg::TabbedOpen {
        spec: TabbedSpec {
            panels: vec![Panel::view("Long", lines)],
            ..TabbedSpec::default()
        },
        reply,
    })
    .unwrap();
    assert!(h.wait_until(Duration::from_secs(2), |h| {
        h.contents().contains("view-000")
    }));
    let screen = h.contents();
    let shown = screen.lines().filter(|l| l.starts_with("view-")).count();
    assert_eq!(shown, 12, "half the screen: {screen}");
    // 24 rows: the spacer, two separators, the composer, the title chip and the hint row are the
    // frame's, 12 the View's; the conversation keeps the other 6.
    assert!(
        screen.contains("conv-39") && screen.contains("conv-34") && !screen.contains("conv-33"),
        "the conversation keeps its rows: {screen}"
    );
    h.tx.send(UiMsg::SurfaceCancel).unwrap();
    assert!(h.wait_until(Duration::from_secs(2), |h| !h.contents().contains("view-")));
    let screen = h.contents();
    assert!(
        screen.contains("conv-39") && screen.contains("conv-21"),
        "{screen}"
    );
    h.quit_and_join(Duration::from_secs(5));
}

/// SF2 (S6 review): a pane so short that the frame's own rows — a five-row draft, the separators —
/// fill it still shows an open dialog whole. The frame keeps its bottom: the surface gets its
/// minimum rows and the composer above it yields; never the other way round, which put the
/// options Enter answers off the screen.
#[test]
fn a_dialog_in_a_pane_the_frame_fills_is_still_on_the_screen() {
    use crate::ui::facade::{Panel, TabbedSpec};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let h = start_loop();
    for c in "x".repeat(80 * 5).chars() {
        h.etx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )))
            .unwrap();
    }
    h.resize(80, 7);
    let (reply, _rx) = tokio::sync::oneshot::channel();
    h.tx.send(UiMsg::TabbedOpen {
        spec: TabbedSpec {
            panels: vec![Panel::list(
                "Allow?",
                vec!["Allow once".to_owned(), "Deny".to_owned()],
            )],
            ..TabbedSpec::default()
        },
        reply,
    })
    .unwrap();
    let shown = |h: &LoopHarness| {
        let p = parse_sized(&h.buf, 80, 7);
        let c = p.screen().contents();
        c.contains("Allow once") && c.contains("Deny") && c.contains("q/Esc cancel")
    };
    assert!(
        h.wait_until(Duration::from_secs(3), shown),
        "the dialog is off the screen:\n{}",
        parse_sized(&h.buf, 80, 7).screen().contents()
    );
    h.quit_and_join(Duration::from_secs(5));
}

/// The owner's S7a check through the real loop and screen: a reply's table, as the terminal's
/// markdown sink commits it (its source, laid out at the draw's width), then the window pulled
/// from 80 to 160 columns and back to 60. At every width the box on screen is whole and within
/// the width, and at 160 the long cells sit on one row each — the columns were computed again,
/// not the rows of 80 columns wrapped.
#[test]
fn a_table_on_screen_is_laid_out_again_when_the_window_widens_and_narrows() {
    let blocks = crate::ui::testutil::markdown_blocks(
        "| name | what it does |\n|---|---|\n| region | stages the rows a preview replaces, in the frame above the composer |\n| conversation | holds every committed block and lays each one out at the width |\n",
    );
    let lh = idle_loop();
    lh.tx.send(UiMsg::Scrollback(blocks)).unwrap();
    lh.sync();
    for (w, h) in [(80, 24), (160, 24), (60, 24)] {
        lh.resize(w, h);
        lh.sync();
        let g = grid(&parse_sized(&lh.buf, w, h));
        let dump = g.join("\n");
        let table: Vec<&String> = g
            .iter()
            .filter(|r| r.starts_with(['┌', '│', '├', '└']))
            .collect();
        assert!(table.len() >= 7, "{w}: the table is not on screen:\n{dump}");
        let width = table[0].trim_end().chars().count();
        for row in &table {
            let row = row.trim_end();
            assert!(
                row.ends_with(['┐', '│', '┤', '┘']) && row.chars().count() == width,
                "{w}: the box broke:\n{dump}"
            );
        }
        assert!(
            width < usize::from(w),
            "{w}: the table reaches the width:\n{dump}"
        );
        let one_row = g
            .iter()
            .any(|r| r.contains("in the frame above the composer"));
        assert_eq!(one_row, w == 160, "{w}: the long cell's rows:\n{dump}");
    }
    lh.quit_and_join(Duration::from_secs(2));
}

/// The skill rows a `/skills ` line offers: descriptions of every length a SKILL.md carries.
fn skill_rows() -> Vec<crate::ui::facade::Suggestion> {
    let skill = |name: &str, desc: &str| crate::ui::facade::Suggestion {
        value: format!("/skills {name}"),
        label: name.to_owned(),
        desc: desc.to_owned(),
        after: Some("/skills ".to_owned()),
    };
    vec![
        skill("brain-page", "Read and write brain pages"),
        skill(
            "code-review",
            "Review the current diff for correctness bugs, then for reuse, simplification and \
             efficiency cleanups, at the effort level given first; pass --fix to apply the \
             findings to the working tree after the review",
        ),
        skill("release", ""),
        skill(
            "investigate",
            "Systematic debugging with root cause investigation: reproduce, analyze, \
             hypothesize, then fix",
        ),
    ]
}

/// The last line of the conversation the completion screens show above the frame.
const CONV_LAST: &str = "the conversation's last line";

/// The model's frame as a `w`×`h` terminal shows it, one string per screen row, under a
/// two-line conversation ending in [`CONV_LAST`].
fn shown(m: &mut Model, cols: u16, lines: u16) -> Vec<String> {
    m.width = cols;
    m.height = lines;
    let view = m.frame_view();
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let mut term =
        Term::headless(Box::new(move || wtr.clone()), Size::new(cols, lines), false).unwrap();
    term.push(Block::rows(["a first line", CONV_LAST]));
    term.draw_frame(&view, None).unwrap();
    let parser = parse_sized(&buf, cols, lines);
    parser
        .screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

/// Where the screen's blocks are, top to bottom: the conversation's last line, the upper
/// separator (the list area's top edge), the list's first and last rows, the composer (one blank
/// gap row above it), the lower separator, the status row.
#[derive(Debug, PartialEq, Eq)]
struct Order {
    conv: Option<usize>,
    sep: usize,
    list: std::ops::RangeInclusive<usize>,
    composer: usize,
    lower: usize,
    status: usize,
}

/// The [`Order`] of `rows` (the list is the rows from the first one written under the upper
/// separator down to the gap row, which is blank and directly on the composer).
fn order(rows: &[String]) -> Order {
    let sep = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("separator");
    let composer = rows
        .iter()
        .position(|r| r.starts_with("/skills"))
        .expect("composer");
    let lower = composer + 1;
    assert!(rows[lower].starts_with('─'), "lower separator: {rows:#?}");
    let status = rows
        .iter()
        .position(|r| r.contains("claude-opus-5-5"))
        .expect("status");
    let conv = rows.iter().position(|r| r == CONV_LAST);
    let first = sep
        + 1
        + rows[sep + 1..composer]
            .iter()
            .position(|r| r.starts_with("▸ ") || r.starts_with("  "))
            .expect("list row");
    assert_eq!(
        rows[composer - 1],
        "",
        "the gap row under the list: {rows:#?}"
    );
    Order {
        conv,
        sep,
        list: first..=composer - 2,
        composer,
        lower,
        status,
    }
}

/// A model with the skill rows and a status row, `typed` in the composer.
fn skills_model(typed: &str) -> Model {
    let mut m = test_model();
    m.apply(UiMsg::Status(StatusData {
        model: "claude-opus-5-5".to_owned(),
        ..StatusData::default()
    }));
    let mut commands = skill_rows();
    commands.push(crate::ui::facade::Suggestion {
        value: "/skills".to_owned(),
        label: String::new(),
        desc: "List skills, or run one".to_owned(),
        after: None,
    });
    m.apply(UiMsg::Commands(commands));
    crate::ui::testutil::type_text(&mut m, typed);
    m
}

/// The completion list as a terminal shows it, at 80 and 160 columns, ABOVE the composer: the
/// two-line conversation at the screen's top, blank rows, the frame's one spacer row, the upper
/// separator (the list area's top edge), the list, one blank gap row, the composer, the lower
/// separator, the status row — in that order. Two columns: the labels at column 2 (the marker at 0,
/// under the composer's text) and — two past the longest label — the descriptions, at the same
/// column on every row; at 160 columns a description still wraps at the 72-column measure, and is
/// cut with `…` after two lines; an item with no description is its label alone. Run with
/// `--nocapture` to see the screens.
#[test]
fn the_completion_list_on_screen_at_80_and_160_columns() {
    use crate::ui::input::suggest::DESC_MEASURE;
    let mut m = skills_model("/skills ");
    crate::ui::input::suggest::tab_complete(&mut m.composer, &m.commands);
    crate::ui::input::suggest::tab_complete(&mut m.composer, &m.commands);
    // `code-review` and `investigate` are the longest labels: 11 columns, after the 2 of indent.
    let at = 2 + 11 + 2;
    for w in [80u16, 160] {
        let rows = shown(&mut m, w, 24);
        let dump = rows.join("\n");
        println!("--- {w}x24 ---\n{dump}");
        let o = order(&rows);
        // The conversation at the top (its last line on row 1), blank rows down to the frame's
        // constant spacer (12), the list's six rows, the gap (20).
        assert_eq!(
            o,
            Order {
                conv: Some(1),
                sep: 13,
                list: 14..=19,
                composer: 21,
                lower: 22,
                status: 23,
            },
            "{w}:\n{dump}"
        );
        assert!(
            rows[2..=12].iter().all(String::is_empty),
            "{w}: blank rows down to the area:\n{dump}"
        );
        let list = &rows[*o.list.start()..=*o.list.end()];
        assert!(list[0].starts_with("  brain-page   Read"), "{w}:\n{dump}");
        assert!(list[1].starts_with("▸ code-review  Review"), "{w}:\n{dump}");
        assert_eq!(
            list[3], "  release",
            "{w}: no description, one row:\n{dump}"
        );
        assert!(
            list[4].starts_with("  investigate  Systematic"),
            "{w}:\n{dump}"
        );
        for r in list.iter().filter(|r| r.as_str() != "  release") {
            let cs: Vec<char> = r.chars().collect();
            assert!(
                cs[at - 2] == ' ' && cs[at - 1] == ' ' && cs[at] != ' ',
                "{w}: the description column moved on {r:?}:\n{dump}"
            );
            assert!(
                cs.len() - at <= DESC_MEASURE.min(usize::from(w) - at),
                "{w}: past the measure: {r:?}"
            );
        }
        assert!(
            list[2].starts_with(&" ".repeat(at)) && list[2].ends_with('…'),
            "{w}: the second line in column two, cut with …:\n{dump}"
        );
    }
}

/// The list grows UPWARD: one more character that matches more items — `/skills` offers the
/// command alone, `/skills ` its whole catalog — adds rows at the list's top, the area's top edge
/// (the upper separator) rises with it, and the composer, the lower separator and the status row
/// keep their rows; a two-line conversation at the screen's top stays where it is (nothing is
/// covered).
#[test]
fn the_composer_holds_still_while_the_list_grows() {
    let mut m = skills_model("/skills");
    let before = shown(&mut m, 80, 24);
    println!("--- /skills ---\n{}", before.join("\n"));
    let b = order(&before);
    assert_eq!(b.list.clone().count(), 1, "{before:#?}");
    crate::ui::testutil::type_text(&mut m, " ");
    let after = shown(&mut m, 80, 24);
    println!("--- /skills␠ ---\n{}", after.join("\n"));
    let a = order(&after);
    assert_eq!(a.list.clone().count(), 6, "{after:#?}");
    assert_eq!(
        (a.composer, a.lower, a.status),
        (b.composer, b.lower, b.status),
        "the composer moved:\n{after:#?}"
    );
    assert_eq!(a.list.end(), b.list.end(), "the list's bottom moved");
    assert_eq!(
        (*a.list.start() + 5, a.sep + 5),
        (*b.list.start(), b.sep),
        "the list's top and the area's edge are what move"
    );
    assert_eq!(
        (a.conv, b.conv),
        (Some(1), Some(1)),
        "the conversation stays at the top"
    );
}

/// On a short terminal the highlighted item beats everything but the composer: the rows above the
/// area (the spacer) go first, then the gap row under the list, then the lower and the upper
/// separator (the area's top edge), then the status row, then the item's description from the
/// bottom — its label row and the composer stay.
#[test]
fn a_short_terminal_keeps_the_composer_and_the_highlighted_item() {
    let mut m = skills_model("/skills ");
    for _ in 0..4 {
        crate::ui::input::suggest::tab_complete(&mut m.composer, &m.commands);
    }
    let label = "▸ investigate  Systematic debugging with root cause investigation: reproduce,";
    let desc = "               analyze, hypothesize, then fix";
    let composer = "/skills investigate";
    let status = "claude-opus-5-5";
    let sep = "─".repeat(80);
    let screens: [(u16, Vec<&str>); 6] = [
        (7, vec![&sep, label, desc, "", composer, &sep, status]),
        (6, vec![&sep, label, desc, composer, &sep, status]),
        (5, vec![&sep, label, desc, composer, status]),
        (4, vec![label, desc, composer, status]),
        (3, vec![label, desc, composer]),
        (2, vec![label, composer]),
    ];
    for (lines, want) in screens {
        let rows = shown(&mut m, 80, lines);
        println!("--- 80x{lines} ---\n{}", rows.join("\n"));
        assert_eq!(rows, want, "80x{lines}");
        assert_eq!(m.list_items.first(), Some(&3), "the highlighted item alone");
    }
}

/// A wrapped composer under an open list on a short terminal: a 95-column command still matches,
/// so the composer is two rows with the one item over it. The shed order is the same as for a
/// one-row composer — the spacer (cut off the top), then the gap row, then the lower separator —
/// and the real cursor is on the composer's second row, after its last character, at every
/// height: the gap above the composer counts in the cursor's offset only while it is drawn.
#[test]
fn a_wrapped_composer_under_the_list_sheds_in_order_and_keeps_its_cursor() {
    let mut m = test_model();
    m.apply(UiMsg::Status(StatusData {
        model: "claude-opus-5-5".to_owned(),
        ..StatusData::default()
    }));
    let value = format!("/{}", "w".repeat(94));
    m.apply(UiMsg::Commands(vec![crate::ui::facade::Suggestion {
        value: value.clone(),
        label: "wide".to_owned(),
        desc: "one item".to_owned(),
        after: None,
    }]));
    crate::ui::testutil::type_text(&mut m, &value);
    let (first, second) = value.split_at(80);
    let item = "  wide  one item";
    let status = "claude-opus-5-5";
    let sep = "─".repeat(80);
    let screens: [(u16, Vec<&str>); 4] = [
        (8, vec!["", &sep, item, "", first, second, &sep, status]),
        (7, vec![&sep, item, "", first, second, &sep, status]),
        (6, vec![&sep, item, first, second, &sep, status]),
        (5, vec![&sep, item, first, second, status]),
    ];
    for (lines, want) in screens {
        m.width = 80;
        m.height = lines;
        let view = m.frame_view();
        let buf = SharedBuf::default();
        let wtr = buf.clone();
        let mut term =
            Term::headless(Box::new(move || wtr.clone()), Size::new(80, lines), false).unwrap();
        term.draw_frame(&view, None).unwrap();
        let parser = parse_sized(&buf, 80, lines);
        let rows: Vec<String> = parser
            .screen()
            .rows(0, 80)
            .map(|r| r.trim_end().to_owned())
            .collect();
        println!("--- 80x{lines} ---\n{}", rows.join("\n"));
        assert_eq!(rows, want, "80x{lines}");
        let row = want.iter().position(|r| *r == second).unwrap();
        assert_eq!(
            parser.screen().cursor_position(),
            (u16::try_from(row).unwrap(), 15),
            "80x{lines}: the cursor after the composer's last character"
        );
    }
}

// --- the list floats over the conversation -------------------------------------

/// One terminal kept across frames, the model's frame drawn over `conv`: each [`Floated::draw`]
/// is what the terminal shows after that frame, one row each, as its bytes and as plain text.
struct Floated {
    term: Term<SharedBuf>,
    buf: SharedBuf,
    size: (u16, u16),
}

impl Floated {
    fn new(m: &mut Model, conv: Vec<Block>, cols: u16, lines: u16) -> Self {
        m.width = cols;
        m.height = lines;
        let buf = SharedBuf::default();
        let wtr = buf.clone();
        let mut term =
            Term::headless(Box::new(move || wtr.clone()), Size::new(cols, lines), false).unwrap();
        term.push(conv);
        Self {
            term,
            buf,
            size: (cols, lines),
        }
    }

    /// One loop iteration's draw: the view keys first, then the frame with the search line.
    fn draw(&mut self, m: &mut Model) -> (Vec<Vec<u8>>, Vec<String>) {
        for op in std::mem::take(&mut m.view_ops) {
            self.term.view(op).unwrap();
        }
        let view = m.frame_view();
        let prompt = m.search_prompt();
        self.term.draw_frame(&view, prompt.as_ref()).unwrap();
        m.vis_h = self.term.view_height();
        let (cols, lines) = self.size;
        let parser = parse_sized(&self.buf, cols, lines);
        let screen = parser.screen();
        (
            screen.rows_formatted(0, cols).collect(),
            screen
                .rows(0, cols)
                .map(|r| r.trim_end().to_owned())
                .collect(),
        )
    }
}

/// The owner's ask: over a conversation that fills the screen, the completion list is an
/// overlay. Opening it leaves every conversation row above it byte for byte as it was with no
/// list — the conversation keeps its rectangle and the frame is drawn over its last rows. The area
/// is exactly the list, so a keystroke that narrows the matches brings the area's top edge (the
/// upper separator) down with the list and gives back the rows it no longer covers: they show the
/// conversation as it was, never blank rows.
#[test]
fn a_narrowing_match_reveals_the_conversation_and_moves_nothing_above_the_list() {
    let mut m = skills_model("");
    let conv = Block::rows((0..60).map(|i| format!("conv-{i:02}")));
    let mut t = Floated::new(&mut m, conv, 80, 24);
    let (closed, closed_txt) = t.draw(&mut m);
    println!("--- closed ---\n{}", closed_txt.join("\n"));
    crate::ui::testutil::type_text(&mut m, "/skills ");
    let (open, open_txt) = t.draw(&mut m);
    println!("--- /skills␠ ---\n{}", open_txt.join("\n"));
    crate::ui::testutil::type_text(&mut m, "c");
    let (narrow, narrow_txt) = t.draw(&mut m);
    println!("--- /skills c ---\n{}", narrow_txt.join("\n"));
    let o = order(&open_txt);
    let n = order(&narrow_txt);
    assert_eq!(o.list.clone().count(), 6, "{open_txt:#?}");
    assert_eq!(n.list.clone().count(), 2, "{narrow_txt:#?}");
    // The conversation filled the screen: its rows run to the area's edge, the spacer between.
    assert_eq!(open_txt[o.sep - 2], "conv-52", "{open_txt:#?}");
    assert_eq!(open_txt[o.sep - 1], "", "the spacer");
    for y in 0..o.sep - 1 {
        assert_eq!(open[y], closed[y], "row {y} moved when the list opened");
    }
    // The area is the list: the edge sits on its first row and came down by the four rows the
    // narrowing dropped; the list still sits on the gap row over the composer.
    assert_eq!(
        n.sep + 1,
        *n.list.start(),
        "no blank rows:\n{narrow_txt:#?}"
    );
    assert_eq!(
        n.sep,
        o.sep + 4,
        "the edge follows the list:\n{narrow_txt:#?}"
    );
    assert_eq!(narrow[n.sep], open[o.sep], "the top edge's bytes changed");
    assert_eq!(
        (n.list.end(), n.composer, n.lower, n.status),
        (o.list.end(), o.composer, o.lower, o.status),
        "the list sits on the gap over the composer"
    );
    // Every row above the spacer is the conversation as it stood with no list: the rows never
    // covered byte for byte, the four the narrowing gave back with the same text.
    for y in 0..o.sep - 1 {
        assert_eq!(
            narrow[y], closed[y],
            "row {y} moved when the matches narrowed"
        );
    }
    assert_eq!(
        narrow_txt[..n.sep - 1],
        closed_txt[..n.sep - 1],
        "the uncovered rows show the conversation"
    );
    assert_eq!(narrow_txt[n.sep - 2], "conv-56", "{narrow_txt:#?}");
    // Closing gives the covered rows back: the screen reads as it did before the list opened (the
    // rewritten rows' blank cells are written spaces now, so the text is compared, not the bytes),
    // and the rows the area never covered are byte for byte what they were.
    m.composer.set_value("");
    let (shut_b, shut) = t.draw(&mut m);
    assert_eq!(shut, closed_txt, "closing the list restores the screen");
    assert_eq!(
        shut_b[..o.sep - 1],
        closed[..o.sep - 1],
        "the rows never covered changed bytes"
    );
}

/// The owner's ask for a fresh session: the banner alone starts at the screen's top, and the
/// completion list opening under it, narrowing and closing leaves its rows byte for byte where
/// they were — the list takes the blank rows below it and covers nothing.
#[test]
fn a_fresh_session_keeps_its_banner_while_the_list_opens_narrows_and_closes() {
    use crate::repl::render::banner::{Banner, BannerFacts};
    let mut m = skills_model("");
    let dir = std::path::Path::new("/work/iota");
    let banner = Block::laid(Banner::new(&BannerFacts {
        mode: crate::config::AgentMode::Chat,
        session_id: None,
        ephemeral: true,
        resumed: false,
        dir,
        home: None,
        host: None,
    }));
    let mut t = Floated::new(&mut m, vec![banner], 80, 24);
    let (closed, closed_txt) = t.draw(&mut m);
    println!("--- fresh ---\n{}", closed_txt.join("\n"));
    let rows = closed_txt.iter().take_while(|r| !r.is_empty()).count();
    assert_eq!(rows, 5, "the banner's card, from row 0:\n{closed_txt:#?}");
    assert!(closed_txt[0].starts_with('╭'), "{closed_txt:#?}");
    let mut screens = Vec::new();
    for typed in ["/skills ", "c"] {
        crate::ui::testutil::type_text(&mut m, typed);
        screens.push(t.draw(&mut m));
    }
    m.composer.set_value("");
    screens.push(t.draw(&mut m));
    for (k, (bytes, txt)) in screens.iter().enumerate() {
        println!("--- step {k} ---\n{}", txt.join("\n"));
        assert_eq!(bytes[..rows], closed[..rows], "step {k}: the banner moved");
        let edge = txt.iter().position(|r| r.starts_with('─')).expect("edge");
        assert!(edge > rows, "step {k}: the banner is covered:\n{txt:#?}");
        assert!(
            txt[rows..edge].iter().all(String::is_empty),
            "step {k}: blank rows between the banner and the edge:\n{txt:#?}"
        );
    }
    assert_eq!(
        screens[0]
            .1
            .iter()
            .filter(|r| r.starts_with("  ") || r.starts_with("▸ "))
            .count(),
        6
    );
    assert_eq!(screens[2].1, closed_txt, "closing restores the screen");
}

/// A conversation that grows past the visible rows while the list is open: it starts at the top
/// with nothing covered, keeps growing downward under the area (its tail covered, the rows above
/// never moving), and once it outgrows its rectangle the view follows the tail as with no list —
/// the screen shows what it would show closed, less the rows the area covers.
#[test]
fn a_conversation_growing_under_an_open_list_moves_no_row_above_it() {
    let mut m = skills_model("/skills ");
    let line = |i: usize| format!("grow-{i:02}");
    let mut t = Floated::new(&mut m, Block::rows((0..3).map(line)), 80, 24);
    let (mut prev, txt) = t.draw(&mut m);
    let edge = order(&txt).sep;
    // The rectangle is the 19 rows a closed frame leaves; the open list and its gap row leave
    // `edge - 1` visible.
    let vis = edge - 1;
    assert_eq!(vis, 12, "{txt:#?}");
    let mut n = 3;
    // 3 → 8 → 12 (the visible rows full) → 13 → 18 → 19 (the rectangle exactly full) → 20 (the
    // first one-line scroll from a top-aligned state) → 22.
    for more in [5, 4, 1, 5, 1, 1, 2] {
        t.term.push(Block::rows((n..n + more).map(line)));
        let was = n;
        n += more;
        let (now, txt) = t.draw(&mut m);
        println!("--- {n} lines ---\n{}", txt.join("\n"));
        assert_eq!(order(&txt).sep, edge, "the area moved");
        if n <= 19 {
            // Top-aligned: line i on row i, the rows that were there unchanged.
            let shown = was.min(vis);
            assert_eq!(now[..shown], prev[..shown], "{was}→{n}: a row above moved");
            for (y, row) in txt[..n.min(vis)].iter().enumerate() {
                assert_eq!(*row, line(y), "{n}: {txt:#?}");
            }
            assert!(
                txt[n.min(vis)..edge].iter().all(String::is_empty),
                "{txt:#?}"
            );
        } else {
            // Past the rectangle the tail is followed: visible rows 0..=11 are lines n - 19 on.
            for (y, row) in txt[..vis].iter().enumerate() {
                assert_eq!(*row, line(n - 19 + y), "{n}: {txt:#?}");
            }
        }
        prev = now;
    }
    // Closing shows the tail on the rectangle's last row.
    m.composer.set_value("");
    let (_, txt) = t.draw(&mut m);
    assert_eq!(txt[18], line(n - 1), "{txt:#?}");
}

/// A search over an open list: the list is not drawn while the search is on (as under a surface),
/// so the hit lands on a row the screen shows — the area would have covered the half-height row
/// the search puts it on — and the search line is on the last visible row.
#[test]
fn a_search_over_an_open_list_shows_its_hit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut m = skills_model("");
    let conv = Block::rows((0..60).map(|i| format!("conv-{i:02}")));
    // 80×16: the 6-row list leaves 5 visible rows of an 11-row rectangle; the search puts its hit
    // half-way down the rectangle, row 5 — under the area, were it drawn.
    let mut t = Floated::new(&mut m, conv, 80, 16);
    t.draw(&mut m);
    crate::ui::testutil::type_text(&mut m, "/skills ");
    let (_, open) = t.draw(&mut m);
    assert_eq!(order(&open).list.clone().count(), 6, "{open:#?}");
    m.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    t.draw(&mut m);
    crate::ui::testutil::type_text(&mut m, "conv-10");
    let (_, rows) = t.draw(&mut m);
    println!("--- Ctrl+R conv-10 over /skills␠ ---\n{}", rows.join("\n"));
    let edge = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("separator");
    let hit = rows
        .iter()
        .position(|r| r == "conv-10")
        .expect("the hit is on screen");
    // The last visible row (over the frame's spacer) is the search line; the hit is above it.
    assert!(
        hit < edge - 2,
        "the hit is above the search line: {rows:#?}"
    );
    assert!(
        rows[edge - 2].starts_with("/conv-10"),
        "the search line: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.starts_with("  brain-page")),
        "no list during the search: {rows:#?}"
    );
    assert_eq!(
        m.frame_view().float,
        0,
        "the list's area is closed during a search"
    );
}

// --- the interaction cap: half the screen or 15 rows ---------------------------

/// The model's frame as a `cols`×`lines` terminal shows it over a 60-line conversation
/// (`conv-00`…`conv-59`), one string per screen row.
fn shown_over_conversation(m: &mut Model, cols: u16, lines: u16) -> Vec<String> {
    m.width = cols;
    m.height = lines;
    let view = m.frame_view();
    let buf = SharedBuf::default();
    let wtr = buf.clone();
    let mut term =
        Term::headless(Box::new(move || wtr.clone()), Size::new(cols, lines), false).unwrap();
    term.push(Block::rows((0..60).map(|i| format!("conv-{i:02}"))));
    term.draw_frame(&view, None).unwrap();
    let parser = parse_sized(&buf, cols, lines);
    parser
        .screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

/// A model with `panels` open as a surface (the reply's receiver kept alive beside it).
fn surface_model(
    panels: Vec<crate::ui::facade::Panel>,
) -> (
    Model,
    tokio::sync::oneshot::Receiver<crate::ui::facade::TabbedResult>,
) {
    let mut m = test_model();
    let (reply, rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TabbedOpen {
        spec: crate::ui::facade::TabbedSpec {
            panels,
            ..crate::ui::facade::TabbedSpec::default()
        },
        reply,
    });
    (m, rx)
}

/// `/file`'s surface as `repl::commands::file` opens it — the Attached list and the Add browser —
/// over a directory of 100 files (`f-000`…`f-099`, after the browser's `../`), on the browser.
fn file_model(
    dir: &std::path::Path,
) -> (
    Model,
    tokio::sync::oneshot::Receiver<crate::ui::facade::TabbedResult>,
) {
    use crate::ui::facade::Panel;
    for i in 0..100 {
        std::fs::write(dir.join(format!("f-{i:03}")), "").unwrap();
    }
    let (mut m, rx) = surface_model(vec![
        Panel::multi("Attached".to_owned(), Vec::new()).with_search(true),
        Panel::browser("Add".to_owned(), dir.to_path_buf()).with_search(true),
    ]);
    m.handle_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Tab,
    ));
    (m, rx)
}

/// The browser panel's state.
fn browser_state(m: &Model) -> &crate::ui::surface::tabbed::PanelState {
    &m.surface.as_ref().unwrap().st.slots[1].state
}

/// The owner's `/file` report: the directory browser took the whole screen. Now its list takes
/// the interaction cap — half the screen, at most 15 rows — the conversation keeps the rest, and
/// the last of 101 entries is reached by scrolling, `↓` a row at a time or `→` a page at a time.
/// Run with `--nocapture` to see the screens.
#[test]
fn file_browser_takes_the_interaction_cap_and_scrolls_to_its_last_entry() {
    use crossterm::event::{KeyCode, KeyEvent};
    // (screen, the cap, the list): at 12 rows the cap is 6, but the frame leaves the list 5 — the
    // room binds, as it always did.
    for (lines, cap, list) in [(40u16, 15usize, 15usize), (24, 12, 12), (12, 6, 5)] {
        assert_eq!(crate::ui::interaction_rows(usize::from(lines)), cap);
        let root = tempfile::tempdir().unwrap();
        let (mut m, _rx) = file_model(root.path());
        let rows = shown_over_conversation(&mut m, 80, lines);
        println!("--- /file 80x{lines} ---\n{}", rows.join("\n"));
        let entries = rows
            .iter()
            .filter(|r| r.contains("f-0") || r.contains("../"))
            .count();
        assert_eq!(entries, list, "{lines} rows:\n{}", rows.join("\n"));
        assert_eq!(browser_state(&m).rows, list);
        let conv = rows.iter().filter(|r| r.starts_with("conv-")).count();
        // The frame's own rows besides the surface: the spacer, two separators and the composer
        // (the status row yields to the surface); the surface's: the tab bar, the directory line,
        // the list, the hint.
        assert_eq!(
            conv,
            usize::from(lines) - 4 - (3 + list),
            "the conversation keeps its rows:\n{}",
            rows.join("\n")
        );
        assert_eq!(rows.contains(&"conv-59".to_owned()), conv > 0);
        // ↓ to the end: the window follows the cursor and stops with the last entry at its bottom.
        for _ in 0..100 {
            m.handle_key(KeyEvent::from(KeyCode::Down));
        }
        // 160 columns: the hint whole, its scroll percentage at the end.
        let rows = shown_over_conversation(&mut m, 160, lines);
        println!("--- /file 160x{lines}, at the end ---\n{}", rows.join("\n"));
        assert_eq!(browser_state(&m).offset, 101 - list, "scrolled to the end");
        let last = rows.iter().position(|r| r.contains("f-099")).unwrap();
        assert!(
            rows[last].starts_with("▸ "),
            "the cursor on it: {}",
            rows[last]
        );
        assert!(rows[last + 1].ends_with("100%"), "{}", rows[last + 1]);
        // → pages: from the top, ceil(100 / list) pages reach the same end.
        for _ in 0..100 {
            m.handle_key(KeyEvent::from(KeyCode::Up));
        }
        let _ = shown_over_conversation(&mut m, 80, lines);
        assert_eq!(browser_state(&m).offset, 0, "back at the top");
        for _ in 0..100usize.div_ceil(list) {
            m.handle_key(KeyEvent::from(KeyCode::Right));
            let _ = shown_over_conversation(&mut m, 80, lines);
        }
        assert_eq!(browser_state(&m).offset, 101 - list, "paged to the end");
    }
}

/// An explicit `height` keeps its meaning under the cap: 8 rows is 8, and 20 rows on a 40-row
/// screen is 20 — past the cap of 15, within the room.
#[test]
fn an_explicit_height_wins_under_the_cap() {
    use crate::ui::facade::Panel;
    let items: Vec<String> = (0..100).map(|i| format!("item-{i:03}")).collect();
    for h in [8usize, 20] {
        let (mut m, _rx) = surface_model(vec![
            Panel::list("Pick".to_owned(), items.clone()).with_height(h),
        ]);
        let rows = shown_over_conversation(&mut m, 80, 40);
        let shown = rows.iter().filter(|r| r.contains("item-")).count();
        assert_eq!(shown, h, "{}", rows.join("\n"));
    }
}

/// The completion list stays under the same cap: 40 matches on a 40-row screen take 15 rows, the
/// conversation keeps the rest, and `↑` / `↓` reach the items out of view — the window follows
/// the highlight.
#[test]
fn the_completion_list_takes_the_interaction_cap() {
    use crossterm::event::{KeyCode, KeyEvent};
    let mut m = test_model();
    m.apply(UiMsg::Commands(
        (0..40)
            .map(|i| crate::ui::facade::Suggestion {
                value: format!("/c{i:02}"),
                label: String::new(),
                desc: String::new(),
                after: None,
            })
            .collect(),
    ));
    crate::ui::testutil::type_text(&mut m, "/c");
    let rows = shown_over_conversation(&mut m, 80, 40);
    println!("--- /c 80x40 ---\n{}", rows.join("\n"));
    let list = |rows: &[String]| {
        rows.iter()
            .filter(|r| r.starts_with("  c") || r.starts_with("▸ c"))
            .count()
    };
    assert_eq!(list(&rows), 15, "{}", rows.join("\n"));
    assert_eq!(m.list_items, (0..15).collect::<Vec<_>>());
    assert!(
        rows.iter().any(|r| r.starts_with("conv-")),
        "{}",
        rows.join("\n")
    );
    // ↑ from no highlight: the last item, in view at the window's bottom.
    m.handle_key(KeyEvent::from(KeyCode::Up));
    let rows = shown_over_conversation(&mut m, 80, 40);
    println!("--- /c 80x40, ↑ ---\n{}", rows.join("\n"));
    assert_eq!(list(&rows), 15);
    assert_eq!(m.list_items, (25..40).collect::<Vec<_>>());
    assert!(rows.iter().any(|r| r == "▸ c39"), "{}", rows.join("\n"));
    // ↓ wraps to the first, and walking down 39 more reaches the last again.
    for _ in 0..40 {
        m.handle_key(KeyEvent::from(KeyCode::Down));
        let _ = shown_over_conversation(&mut m, 80, 40);
    }
    assert_eq!(m.list_items, (25..40).collect::<Vec<_>>());
    assert_eq!(m.composer.value(), "/c39");
}

/// tmux's burst that ends where it began (100×30 → 60×16 → 100×30, L4 38 on 3.4): the pane's grid
/// is cropped and regrown while the loop merges the `SIGWINCH` signals into one size — the size of the
/// last frame. That resize still repaints every cell, as one synchronized update: the emulator
/// whose grid was cropped and regrown in between shows what a fresh one replaying every byte does.
#[test]
fn a_resize_back_to_the_drawn_size_repaints_every_cell() {
    use crate::ui::runtime::screen::{SYNC_BEGIN, SYNC_END, SYNC_MIN};
    let lh = idle_loop();
    let before = lh.buf.bytes().len();
    let mut p = parse(&lh.buf);
    p.screen_mut().set_size(12, 40);
    p.screen_mut().set_size(24, 80);
    lh.resize(80, 24);
    let all = lh.buf.bytes();
    let after = &all[before..];
    p.process(after);
    let fresh = grid(&parse(&lh.buf));
    assert_eq!(grid(&p), fresh, "a stale screen after a same-size resize");
    // One write (the iteration's batch): a synchronized update exactly when it is longer than
    // SYNC_MIN (X-55).
    let count = |pat: &[u8]| after.windows(pat.len()).filter(|w| *w == pat).count();
    let pairs = usize::from(after.len() > SYNC_MIN);
    assert_eq!(
        (count(SYNC_BEGIN), count(SYNC_END)),
        (pairs, pairs),
        "{} bytes",
        after.len()
    );
}

// --- the background, asked again (X-108) -------------------------------------

/// Unix only, like the asking itself: elsewhere the loop never writes the question
/// (`requery::Requery::default`).
#[cfg(unix)]
mod asked_again {
    use super::*;

    /// How many times the loop has written the OSC 11 query.
    fn queries(h: &LoopHarness) -> usize {
        let q = crate::ui::runtime::osc::OSC11_QUERY;
        h.buf.bytes().windows(q.len()).filter(|w| *w == q).count()
    }

    /// Every cell of the screen, row-major: its text, its foreground, its background.
    fn cells(h: &LoopHarness) -> Vec<(String, vt100::Color, vt100::Color)> {
        let p = parse(&h.buf);
        let mut out = Vec::new();
        for r in 0..24 {
            for c in 0..80 {
                let cell = p.screen().cell(r, c).unwrap();
                out.push((cell.contents().to_owned(), cell.fgcolor(), cell.bgcolor()));
            }
        }
        out
    }

    /// Whether any cell has the 256-colour background `idx`.
    fn shaded(h: &LoopHarness, idx: u8) -> bool {
        cells(h).iter().any(|c| c.2 == vt100::Color::Idx(idx))
    }

    /// Sends the terminal's reply, as the key events crossterm makes of it.
    fn reply(h: &LoopHarness, colour: &str) {
        for ev in crate::ui::testutil::reply_events(colour, "\x07") {
            h.etx.send(ev).unwrap();
        }
    }

    /// One character key.
    fn ch(c: char) -> Event {
        key(crossterm::event::KeyCode::Char(c))
    }

    const LONG: Duration = Duration::from_secs(10);

    /// The current row's shade in a list on a dark terminal, and on a light one (`theme::row_bg`).
    const DARK_ROW: u8 = 237;
    const LIGHT_ROW: u8 = 253;

    /// The loop with a completion list open (`/` typed over two commands, `Down` onto the first):
    /// the list's current row is what an idle frame shades by the background's tone.
    fn start_listed() -> LoopHarness {
        let h = start_loop();
        let command = |value: &str| crate::ui::facade::Suggestion {
            value: value.to_owned(),
            label: String::new(),
            desc: String::new(),
            after: None,
        };
        h.tx.send(UiMsg::Commands(vec![command("/help"), command("/model")]))
            .unwrap();
        h.sync(); // the table is in before the keys that open the list over it
        h.etx.send(ch('/')).unwrap();
        h.etx.send(key(crossterm::event::KeyCode::Down)).unwrap();
        assert!(
            h.wait_until(LONG, |h| shaded(h, DARK_ROW)),
            "no list: {}",
            h.contents()
        );
        h
    }

    /// The window comes back and the OS has switched to light meanwhile: the loop asks once, and the
    /// answer re-shades the NEXT frame — the list's current row goes from the dark shade (237) to the
    /// light one (253) and no other cell changes its text, its foreground or its background. It is an
    /// ordinary frame: the screen is not cleared.
    #[test]
    fn a_flip_on_focus_reshades_the_next_frame_and_nothing_else() {
        let h = start_listed();
        h.sync();
        let before = cells(&h);
        assert!(!shaded(&h, LIGHT_ROW));
        assert_eq!(queries(&h), 0, "nothing is asked unprompted");

        h.etx.send(Event::FocusGained).unwrap();
        assert!(h.wait_until(LONG, |h| queries(h) == 1), "the question");
        let mark = h.buf.bytes().len();
        reply(&h, "rgb:ffff/ffff/ffff");
        assert!(
            h.wait_until(LONG, |h| shaded(h, LIGHT_ROW)),
            "the light row"
        );
        h.sync();

        let after = cells(&h);
        let (dark, light) = (vt100::Color::Idx(DARK_ROW), vt100::Color::Idx(LIGHT_ROW));
        let mut flipped = 0;
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            assert_eq!((&b.0, &b.1), (&a.0, &a.1), "cell {i}: text and foreground");
            if b.2 == dark {
                assert_eq!(a.2, light, "cell {i}");
                flipped += 1;
            } else {
                assert_eq!(a.2, b.2, "cell {i}: background");
            }
        }
        assert_eq!(flipped, 80, "the current row, edge to edge");
        let written = h.buf.bytes()[mark..].to_vec();
        assert!(
            !written.windows(4).any(|w| w == b"\x1b[2J"),
            "a flip cleared the screen: {:?}",
            String::from_utf8_lossy(&written)
        );
        assert_eq!(queries(&h), 1);
        h.quit_and_join(LONG);
    }

    /// THE input-safety unit, and it is about ORDER, not time: with the question out, a key, a
    /// paste, a key, the terminal's reply and one more key arrive in one go. Everything typed
    /// reaches the composer, in order, and not one character of the reply does.
    #[test]
    fn keys_and_a_paste_inside_the_reply_window_reach_the_composer() {
        let h = start_loop();
        h.etx.send(Event::FocusGained).unwrap();
        assert!(h.wait_until(LONG, |h| queries(h) == 1), "the question");

        h.etx.send(ch('a')).unwrap();
        h.etx.send(Event::Paste("-pasted-".to_owned())).unwrap();
        h.etx.send(ch('b')).unwrap();
        reply(&h, "rgb:ffff/ffff/ffff");
        h.etx.send(ch('c')).unwrap();

        assert!(
            h.wait_until(LONG, |h| h.contents().contains("a-pasted-bc")),
            "{}",
            h.contents()
        );
        h.sync();
        let screen = h.contents();
        assert!(
            !screen.contains("11;") && !screen.contains("rgb"),
            "the reply was typed: {screen}"
        );
        h.quit_and_join(LONG);
    }

    /// A terminal that never answers is asked ONE time. The miss ends it for the session: the
    /// REPL's between-turns call — parked on the question — is told "no answer", the next one at
    /// once, and another focus asks nothing. Keys go on working throughout. (What becomes of a
    /// reply that comes late is `requery::tests`' to say, on a clock of its own.)
    #[test]
    fn a_mute_terminal_is_asked_once() {
        let h = start_listed();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.tx.send(UiMsg::RequeryBackground(tx)).unwrap();
        assert_eq!(rx.blocking_recv().unwrap(), None, "the miss");
        assert_eq!(queries(&h), 1);

        h.etx.send(Event::FocusLost).unwrap();
        h.etx.send(Event::FocusGained).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.tx.send(UiMsg::RequeryBackground(tx)).unwrap();
        assert_eq!(rx.blocking_recv().unwrap(), None);
        h.sync();
        assert_eq!(queries(&h), 1, "asked again after a miss");

        assert!(shaded(&h, DARK_ROW) && !shaded(&h, LIGHT_ROW));
        h.etx.send(ch('z')).unwrap();
        assert!(h.wait_until(LONG, |h| h.contents().contains('z')));
        h.quit_and_join(LONG);
    }

    /// A host answers for the background (`Ui::background_from_host`, cmux): the terminal is
    /// never asked — not on focus, not by the REPL's call, which hears `None` at once.
    #[test]
    fn a_host_that_knows_the_background_silences_the_question() {
        let h = start_loop();
        h.tx.send(UiMsg::BackgroundFromHost).unwrap();
        h.sync();
        h.etx.send(Event::FocusGained).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.tx.send(UiMsg::RequeryBackground(tx)).unwrap();
        assert_eq!(rx.blocking_recv().unwrap(), None);
        h.sync();
        assert_eq!(queries(&h), 0);
        h.quit_and_join(LONG);
    }

    /// The REPL's between-turns call (`Ui::requery_background`): the loop writes the question at
    /// once — no focus needed — and the caller gets the terminal's answer, which the frame
    /// has taken too. A terminal that answered is asked again the next time.
    #[test]
    fn the_repl_ask_is_answered_with_the_terminals_reply() {
        let h = start_listed();
        for (n, colour, dark, shade) in [
            (1, "rgb:ffff/ffff/ffff", false, LIGHT_ROW),
            (2, "rgb:1e1e/1e1e/1e1e", true, DARK_ROW),
        ] {
            let (tx, rx) = tokio::sync::oneshot::channel();
            h.tx.send(UiMsg::RequeryBackground(tx)).unwrap();
            assert!(h.wait_until(LONG, |h| queries(h) == n), "question {n}");
            reply(&h, colour);
            assert_eq!(rx.blocking_recv().unwrap(), Some(dark));
            assert!(h.wait_until(LONG, |h| shaded(h, shade)), "shade {shade}");
        }
        h.quit_and_join(LONG);
    }
}
