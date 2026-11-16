#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP43 L1 suite: the region's call preview (`region_test.go`, `model_test.go`'s region
//! tests, `sink_test.go` — incl. the `TwoRoundToolTurn` GOLDEN) driven through the
//! `Emit::Test` seam, plus the Live-publish ordering and sink-lifecycle pins. The staging-window
//! cases (tail, residue, overflow chunking, the metered block preview) went with the window in
//! tui-plan S7b; the spacing GOLDEN and the preview's own laws stay.
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
//! in-file (formerly a `#[path]`-mounted `tests/region.rs` of the terminal crate; merged 2026-09-02).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ui::facade::UiStreamSink;

use crate::ui::render::region::{Emit, LivePublish, PREVIEW_WINDOW, Region, RegionSnapshot};
use crate::ui::render::sink::StreamSink;

/// A region publishing through the test seam (Go `&region{emit: …}`).
fn region_emitting(f: impl FnMut(Vec<String>, RegionSnapshot) + Send + 'static) -> Region {
    Region::new(Emit::Test(Box::new(f)))
}

/// Owned-string vector shorthand.
fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// A committed row with embedded newlines reaches the conversation as one row per line: the
/// buffer counts a row as one line before it wraps it.
#[test]
fn region_commit_splits_embedded_newlines() {
    let out: Arc<Mutex<Vec<String>>> = Arc::default();
    let o = Arc::clone(&out);
    let mut r = region_emitting(move |rows, _| o.lock().unwrap().extend(rows));
    r.commit(s(&["Error: 400 {\n  \"message\": \"bad\"\n}"]));
    assert_eq!(
        *out.lock().unwrap(),
        s(&["Error: 400 {", "  \"message\": \"bad\"", "}"])
    );
}

/// The one-row invariant on the preview side: labels (fresh open AND the in-place
/// relabel), rolling preview lines, and the status-row detail each render — and count
/// in the cursor offset — as exactly one frame row, so embedded line breaks collapse to
/// spaces on entry. The rolling body keeps the last [`PREVIEW_WINDOW`] lines.
#[test]
fn region_preview_entries_collapse_to_one_row() {
    let mut r = region_emitting(|_, _| {});

    r.open_call_preview("[shell\ncommand:a]"); // fresh open
    assert_eq!(r.label, "[shell command:a]");
    r.open_call_preview("[shell\r\ncommand:b]"); // ensure branch: relabel in place
    assert_eq!(r.label, "[shell command:b]");
    r.set_call_detail("1.2k\ntokens");
    assert_eq!(r.detail, "1.2k tokens");

    r.preview_line("|a|\n|b|");
    assert_eq!(r.preview_tail, s(&["|a| |b|"]));
    for l in ["c", "d", "e"] {
        r.preview_line(l);
    }
    assert_eq!(r.preview_tail.len(), PREVIEW_WINDOW);
    assert_eq!(r.preview_tail, s(&["c", "d", "e"]));
}

/// New (no Go twin — `setCallBody` ships without a unit test; `T3_TEST_PLAN` §2 pins it):
/// progressive image frames replace the widget body WHOLESALE through the one-row rule, each
/// replacement publishes a fresh snapshot, and a frame racing the settle is a no-op — it must
/// never resurrect a closed widget or invent one where none is up.
#[test]
fn region_set_call_body_replaces_the_widget_body() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = region_emitting(move |_, snap| c.lock().unwrap().push(snap));

    // No preview at all: dropped.
    r.set_call_body(s(&["▀▀▀"]));
    assert!(r.preview_tail.is_empty(), "no widget: {:?}", r.preview_tail);

    r.open_call_preview("image");
    let before = snaps.lock().unwrap().len();
    r.set_call_body(s(&["frame\none", "frame two"]));
    assert_eq!(
        r.preview_tail,
        s(&["frame one", "frame two"]),
        "one_row applied"
    );
    assert_eq!(
        snaps.lock().unwrap().len(),
        before + 1,
        "each frame publishes"
    );

    // The next frame REPLACES (a longer body does not append to the last).
    r.set_call_body(s(&["only"]));
    assert_eq!(r.preview_tail, s(&["only"]));

    // Settled (the widget stays on screen for the morph): a late frame is dropped rather
    // than repainting rows the next commit is about to replace.
    r.close_preview();
    r.set_call_body(s(&["late"]));
    assert_eq!(
        r.preview_tail,
        s(&["only"]),
        "after settle the body freezes"
    );
}

/// GOLDEN: replays the region op sequence of a full two-round tool turn (thinking
/// widget → pending call raise → settle → results → next round) and pins the committed
/// stream: exactly one blank separator per block boundary, no
/// widget row ever leaking into scrollback.
#[test]
fn region_two_round_tool_turn() {
    let scroll: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = Arc::clone(&scroll);
    let mut r = region_emitting(move |over, _| c.lock().unwrap().extend(over));

    // round 1
    r.commit(s(&["▌ user prompt"])); // user block
    r.commit(s(&[""])); // openThinking separator
    r.open_call_preview("Thinking"); // thinking widget
    r.close_preview(); // settleThinking
    r.commit(s(&["◇ thought for <1s A"])); // marker
    r.commit(s(&[""])); // pendingCall separator
    r.open_call_preview("[shell …]"); // composing raise
    r.open_call_preview("[shell command:pwd]"); // relabel in place
    r.close_preview(); // settleCall
    r.commit(s(&["[shell command:pwd]"])); // header
    r.commit(s(&["  ⎿ /Users/joyqi"])); // toolLines

    // round 2
    r.commit(s(&[""])); // openThinking separator
    r.open_call_preview("Thinking");
    r.close_preview();
    r.commit(s(&["◇ thought for <1s B"]));
    r.commit(s(&[""]));
    r.commit(s(&["final content"]));

    let stream = scroll.lock().unwrap().clone();

    let want = [
        "▌ user prompt",
        "",
        "◇ thought for <1s A",
        "",
        "[shell command:pwd]",
        "  ⎿ /Users/joyqi",
        "",
        "◇ thought for <1s B",
        "",
        "final content",
    ]
    .join("\n");
    assert_eq!(stream.join("\n"), want);
}

/// The call clock pauses while the user is consulted (approval prompts, interactive
/// tools): `paused_at` freezes the elapsed figure, resume shifts `since` forward by the
/// paused span so the figure continues where it froze, and every widget teardown clears
/// the pause state.
#[test]
fn region_clock_pause() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = region_emitting(move |_, snap| c.lock().unwrap().push(snap));

    r.pause_clock(); // no call preview: a no-op, publishes nothing
    assert!(
        snaps.lock().unwrap().is_empty(),
        "pause without a preview must be silent"
    );

    r.open_call_preview("[edit_file …]");
    let before = r.since.unwrap();
    r.pause_clock();
    assert!(r.paused_at.is_some(), "paused_at not set");
    let frozen = snaps.lock().unwrap().last().unwrap().clone();
    assert!(
        frozen.paused_at.is_some(),
        "snapshot must carry paused_at for the model to freeze the figure"
    );
    r.pause_clock(); // idempotent: already paused

    std::thread::sleep(Duration::from_millis(5));
    r.resume_clock();
    assert!(r.paused_at.is_none(), "resume must clear paused_at");
    assert!(
        r.since.unwrap() > before,
        "since must shift forward by the paused span"
    );

    r.resume_clock(); // idempotent: not paused

    r.pause_clock();
    r.drop_preview(); // teardown clears the pause with the widget
    assert!(r.paused_at.is_none(), "drop_preview must clear paused_at");
}

/// The tool-call lifecycle widget — header + live status row — keeps its clock and detail
/// across relabels; settling is a deferred close: the widget stays up until the next commit, its
/// result, takes it down, and a late detail never resurrects it.
#[test]
fn region_call_preview() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = region_emitting(move |_, snap| c.lock().unwrap().push(snap));
    let last = || snaps.lock().unwrap().last().unwrap().clone();

    r.open_call_preview("[write_file …]");
    let snap = last();
    assert!(snap.since.is_some(), "call preview not raised: {snap:?}");
    assert_eq!(snap.label, "[write_file …]");
    let started = snap.since;

    r.set_call_detail("0.4k tokens");
    assert_eq!(last().detail, "0.4k tokens", "detail not recorded");

    // Expanding the header (composing → full args) keeps the clock and detail.
    r.open_call_preview("[write_file path:a.txt]");
    let snap = last();
    assert_eq!(snap.label, "[write_file path:a.txt]");
    assert_eq!(
        snap.since, started,
        "ensure should relabel in place keeping the clock"
    );
    assert_eq!(snap.detail, "0.4k tokens", "ensure should keep the detail");

    r.close_preview();
    r.set_call_detail("late"); // after the deferred close: no-op
    assert_eq!(
        last().label,
        "[write_file path:a.txt]",
        "settled widget stays up"
    );
    r.commit(s(&["[write_file path:a.txt]"]));
    let snap = last();
    assert!(
        snap.label.is_empty() && snap.since.is_none() && snap.detail.is_empty(),
        "widget should be settled with detail cleared: {snap:?}"
    );
}

/// Markdown's block spacing is a lone `""` row; it reaches the conversation like any row, in
/// order. (Go's joinOverflow `""`→`" "` substitution is a bubbletea insertAbove workaround, NOT
/// ported — T-02.)
#[test]
fn region_blank_line_passes_through_in_order() {
    let out: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = Arc::clone(&out);
    let mut r = region_emitting(move |rows, _| c.lock().unwrap().extend(rows));
    r.commit(s(&["a", "b"]));
    r.commit(s(&[""]));
    r.commit(s(&["c"]));
    assert_eq!(out.lock().unwrap().clone(), s(&["a", "b", "", "c"]));
}

/// The Live seam: a commit publishes its blocks as one batch, and the snapshot always FOLLOWS
/// them — the publish-ordering law the loop's mailbox consumer relies on; a preview change
/// publishes the snapshot alone.
#[test]
fn live_publish_blocks_then_snapshot() {
    #[derive(Debug, PartialEq)]
    enum Ev {
        Blocks(Vec<String>),
        Snap(String),
    }
    struct RecPub(Arc<Mutex<Vec<Ev>>>);
    impl LivePublish for RecPub {
        fn scrollback(&self, rows: Vec<crate::ui::block::Block>) {
            let rows: Vec<String> = rows.iter().flat_map(|b| b.render(80)).collect();
            self.0.lock().unwrap().push(Ev::Blocks(rows));
        }
        fn region(&self, snap: RegionSnapshot) {
            self.0.lock().unwrap().push(Ev::Snap(snap.label));
        }
    }

    let events: Arc<Mutex<Vec<Ev>>> = Arc::default();
    let mut r = Region::new(Emit::Live {
        tx: Box::new(RecPub(Arc::clone(&events))),
    });
    r.commit(s(&["l0", "l1", "l2", "l3"]));
    r.open_call_preview("[shell …]");
    let got = std::mem::take(&mut *events.lock().unwrap());
    assert_eq!(
        got,
        vec![
            Ev::Blocks(s(&["l0", "l1", "l2", "l3"])),
            Ev::Snap(String::new()),
            Ev::Snap("[shell …]".to_owned()),
        ]
    );
}

/// New: `UiStreamSink::done` = `drop_preview` + the scope-pop callback (sink.go:24-27) —
/// a leaked/deferred preview dies with the turn.
#[test]
fn stream_sink_done_drops_preview_and_pops_scope() {
    let region = Arc::new(Mutex::new(region_emitting(|_, _| {})));
    let popped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&popped);
    let sink = StreamSink::new(Arc::clone(&region), move || {
        flag.store(true, Ordering::Relaxed);
    });

    region.lock().unwrap().open_call_preview("[shell …]");
    sink.done();
    let r = region.lock().unwrap();
    assert_eq!(r.label, "", "done must drop the preview");
    assert!(r.since.is_none());
    drop(r);
    assert!(
        popped.load(Ordering::Relaxed),
        "done must pop the turn scope"
    );
}

/// New: the theme pins (`TUI_CONTRACTS` §9) — raw SGR consts byte-exact, the dark
/// atomic defaulting true, and the 236/254 input shades.
#[test]
fn theme_sgr_pins() {
    assert_eq!(crate::ui::render::theme::FAINT, "\x1b[2m");
    assert_eq!(crate::ui::render::theme::CYAN, "\x1b[36m");
    assert_eq!(crate::ui::render::theme::GREEN, "\x1b[32m");
    assert_eq!(crate::ui::render::theme::YELLOW, "\x1b[33m");
    assert_eq!(crate::ui::render::theme::RED, "\x1b[31m");
    assert_eq!(crate::ui::render::theme::REV_ON, "\x1b[7m");
    assert_eq!(crate::ui::render::theme::RESET, "\x1b[0m");
    assert_eq!(crate::ui::render::theme::ERR_PREFIX, "\x1b[31m⚠ \x1b[0m");
    assert_eq!(crate::ui::render::theme::SEARCH_HIT, "\x1b[7m");
    assert_eq!(crate::ui::render::theme::SEARCH_CUR, "\x1b[7;33m");

    assert_eq!(crate::ui::render::theme::input_bg(true), "\x1b[48;5;236m");
    assert_eq!(crate::ui::render::theme::input_bg(false), "\x1b[48;5;254m");
}
