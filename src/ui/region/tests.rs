#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP43 L1 suite: the staging-window ports (`region_test.go`, `model_test.go`'s region
//! tests, `sink_test.go` — incl. the `TwoRoundToolTurn` GOLDEN) driven through the
//! `Emit::Test` seam, plus the new Live-publish chunk-ordering and sink-lifecycle pins.
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
//! in-file (formerly a `#[path]`-mounted `tests/region.rs` of the terminal crate; merged 2026-09-02).

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ui::facade::{PreviewHandle, UiStreamSink};

use crate::ui::region::{
    Emit, LivePublish, PREVIEW_WINDOW, Region, RegionSnapshot, TAIL_KEEP, chunk_overflow,
};
use crate::ui::sink::{PreviewWriter, StreamSink, count_lines};

/// A region publishing through the test seam with width/height 0 (Go `&region{emit: …}`).
fn test_region(f: impl FnMut(Vec<String>, RegionSnapshot) + Send + 'static) -> Region {
    Region::new(
        Emit::Test(Box::new(f)),
        Arc::new(AtomicU16::new(0)),
        Arc::new(AtomicU16::new(0)),
    )
}

/// Owned-string vector shorthand.
fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// Window rows Go's `TestRegionMorph` counts: tail + preview header + body.
fn rows_morph(snap: &RegionSnapshot) -> usize {
    let mut n = snap.tail.len();
    if !snap.label.is_empty() {
        n += 1 + snap.preview_tail.len();
    }
    n
}

/// Window rows incl. residue (`TestRegionMorphResidue` / `PreviewOverPreview`).
fn rows_residue(snap: &RegionSnapshot) -> usize {
    snap.tail.len()
        + snap.residue.len()
        + if snap.label.is_empty() {
            0
        } else {
            1 + snap.preview_tail.len()
        }
}

/// Window rows incl. the call-preview status row (`TestRegionCallPreview`).
fn rows_call(snap: &RegionSnapshot) -> usize {
    let mut n = snap.tail.len() + snap.residue.len();
    if !snap.label.is_empty() {
        n += 1 + snap.preview_tail.len();
        if snap.since.is_some() {
            n += 1;
        }
    }
    n
}

/// A committed entry with embedded newlines must expand to one tail entry per visual
/// row: all window bookkeeping (tail height, rebalance, overflow) — and through it the
/// frame anchor and the composer cursor — assumes one row per entry.
// Go: region_test.go:14
#[test]
fn region_commit_splits_embedded_newlines() {
    let mut r = test_region(|_, _| {});
    r.commit(s(&["Error: 400 {\n  \"message\": \"bad\"\n}"]));
    assert_eq!(r.tail, s(&["Error: 400 {", "  \"message\": \"bad\"", "}"]));
}

/// Overwide entries are hard-wrapped on commit to width−1 (SGR rows keep style via
/// `wrap_ansi`); rows that already fit — like `UserBlock`'s full-width rows — pass through
/// untouched.
// Go: region_test.go:29
#[test]
fn region_commit_wraps_to_screen_width() {
    let scroll: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = Arc::clone(&scroll);
    let width = Arc::new(AtomicU16::new(10));
    let mut r = Region::new(
        Emit::Test(Box::new(move |over, _| c.lock().unwrap().extend(over))),
        width,
        Arc::new(AtomicU16::new(0)),
    );

    r.commit(s(&[
        "aaaaaaaaaabbbbbbbbbbcc",
        "\x1b[31mdddddddddddd\x1b[0m",
        "fits-fine",
    ]));
    let want = s(&[
        "aaaaaaaaa",
        "abbbbbbbb",
        "bbcc",
        "\x1b[31mddddddddd",
        "\x1b[31mddd\x1b[0m",
        "fits-fine",
    ]);
    let mut all = scroll.lock().unwrap().clone();
    all.extend(r.tail.clone());
    assert_eq!(all, want);
}

/// Width 0 (startup before the first resize, emit-seam tests) skips wrapping.
// Go: region_test.go:54
#[test]
fn region_commit_no_width_no_wrap() {
    let mut r = test_region(|_, _| {});
    r.commit(vec!["x".repeat(500)]);
    assert_eq!(r.tail.len(), 1, "tail rows = {}, want 1", r.tail.len());
}

/// The one-row invariant on the preview side: labels (fresh open AND the in-place
/// relabel), rolling preview lines, and the status-row detail each render — and count
/// in the cursor offset — as exactly one frame row, so embedded line breaks collapse to
/// spaces on entry.
// Go: region_test.go:66
#[test]
fn region_preview_entries_collapse_to_one_row() {
    let mut r = test_region(|_, _| {});

    r.open_call_preview("[bash\ncommand:a]"); // fresh open
    assert_eq!(r.label, "[bash command:a]");
    r.open_call_preview("[bash\r\ncommand:b]"); // ensure branch: relabel in place
    assert_eq!(r.label, "[bash command:b]");
    r.set_call_detail("1.2k\ntokens");
    assert_eq!(r.detail, "1.2k tokens");

    r.open_preview("rendering\ntable…"); // plain preview label
    assert_eq!(r.label, "rendering table…");
    r.preview_line("|a|\n|b|");
    assert_eq!(r.preview_tail, s(&["|a| |b|"]));
}

/// New (no Go twin — `setCallBody` ships without a unit test; `T3_TEST_PLAN` §2 pins it):
/// progressive image frames replace the widget body WHOLESALE through the one-row rule, each
/// replacement publishes a fresh snapshot, and a frame racing the settle is a no-op — it must
/// never resurrect a closed widget or invent one where none is up.
#[test]
fn region_set_call_body_replaces_the_widget_body() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));

    // No preview at all: dropped.
    r.set_call_body(s(&["▀▀▀"]));
    assert!(r.preview_tail.is_empty(), "no widget: {:?}", r.preview_tail);

    // A PLAIN preview is not a call preview (`since` is unset): still dropped.
    r.open_preview("rendering table…");
    r.set_call_body(s(&["▀▀▀"]));
    assert!(
        r.preview_tail.is_empty(),
        "plain preview: {:?}",
        r.preview_tail
    );
    r.drop_preview();

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
/// widget → pending call raise → settle → results → next round) and pins the scrollback
/// stream (overflow ∪ final tail): exactly one blank separator per block boundary, no
/// widget row ever leaking into scrollback.
// Go: region_test.go:96
#[test]
fn region_two_round_tool_turn() {
    let scroll: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = Arc::clone(&scroll);
    let mut r = test_region(move |over, _| c.lock().unwrap().extend(over));

    // round 1
    r.commit(s(&["❯ user prompt"])); // user block
    r.commit(s(&[""])); // openThinking separator
    r.open_call_preview("Thinking"); // thinking widget
    r.close_preview(); // settleThinking
    r.commit(s(&["◇ thought for <1s A"])); // marker
    r.commit(s(&[""])); // pendingCall separator
    r.open_call_preview("[bash …]"); // composing raise
    r.open_call_preview("[bash command:pwd]"); // relabel in place
    r.close_preview(); // settleCall
    r.commit(s(&["[bash command:pwd]"])); // header
    r.commit(s(&["  ⎿ /Users/joyqi"])); // toolLines

    // round 2
    r.commit(s(&[""])); // openThinking separator
    r.open_call_preview("Thinking");
    r.close_preview();
    r.commit(s(&["◇ thought for <1s B"]));
    r.commit(s(&[""]));
    r.commit(s(&["final content"]));

    r.flush();
    let mut stream = scroll.lock().unwrap().clone();
    stream.extend(r.tail.clone()); // tail is empty post-flush (Go appends it the same)

    let want = [
        "❯ user prompt",
        "",
        "◇ thought for <1s A",
        "",
        "[bash command:pwd]",
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
// Go: region_test.go:148
#[test]
fn region_clock_pause() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));

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

/// `relabel_preview` updates a plain preview's header in place — and ONLY that: call
/// previews relabel through `open_call_preview`, and a closed preview must not be
/// resurrected by a throttled counter racing the flush.
// Go: region_test.go:191
#[test]
fn region_relabel_preview() {
    let mut r = test_region(|_, _| {});

    r.open_preview("rendering table…");
    r.relabel_preview("rendering table… · 12 lines");
    assert_eq!(r.label, "rendering table… · 12 lines");
    assert!(
        r.preview_tail.is_empty(),
        "relabel must not grow the preview: {:?}",
        r.preview_tail
    );

    r.close_preview();
    r.relabel_preview("rendering table… · 99 lines");
    assert_eq!(
        r.label, "rendering table… · 12 lines",
        "closed preview resurrected"
    );

    r.drop_preview();
    r.open_call_preview("[bash …]");
    r.relabel_preview("nope");
    assert_eq!(
        r.label, "[bash …]",
        "call preview must ignore relabel_preview"
    );
}

/// The single-row preview closes the residue/shrink class: a SHORT block (2-row list)
/// morphing over its 1-row preview leaves no residue, and the end-of-turn `drop_preview`
/// finds nothing to shrink.
// Go: region_test.go:220
#[test]
fn region_short_block_no_residue() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));

    r.commit(s(&["intro text"]));
    r.open_preview("rendering list…"); // single header row, no source lines
    r.close_preview();
    r.commit(s(&["• a", "• b"])); // the rendered short list morphs over it

    assert!(
        r.residue.is_empty(),
        "short block left residue: {:?}",
        r.residue
    );
    assert_eq!(r.label, "", "preview not replaced");
    let last = snaps.lock().unwrap().last().unwrap().clone();
    let rows = last.tail.len() + last.residue.len();
    r.drop_preview(); // end of turn
    let last = snaps.lock().unwrap().last().unwrap().clone();
    let after = last.tail.len() + last.residue.len();
    assert_eq!(after, rows, "end-of-turn drop shrank the window");
}

/// Pins the staging-window contract: preview growth STEALS tail rows (commits, never
/// shrinks); the block's rendered lines REPLACE the preview in place; total height
/// never exceeds `TAIL_KEEP` and never shrinks across the flush.
// Go: model_test.go:214
#[test]
fn region_morph() {
    let overflows: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let (co, cs) = (Arc::clone(&overflows), Arc::clone(&snaps));
    let mut r = test_region(move |over, snap| {
        if !over.is_empty() {
            co.lock().unwrap().push(over);
        }
        cs.lock().unwrap().push(snap);
    });
    let last = || snaps.lock().unwrap().last().unwrap().clone();

    // Warm the window with committed lines.
    r.commit(s(&["p1", "p2", "p3", "p4"]));
    assert_eq!(rows_morph(&last()), TAIL_KEEP, "warm rows");

    // Preview opens and grows: tail rows are stolen (committed), height stays.
    r.open_preview("rendering table…");
    for l in ["|a|", "|b|", "|c|", "|d|"] {
        r.preview_line(l);
    }
    let snap = last();
    assert_eq!(
        rows_morph(&snap),
        TAIL_KEEP,
        "rows during preview must stay constant"
    );
    assert_eq!(
        snap.preview_tail.len(),
        PREVIEW_WINDOW,
        "preview tail = {:?}",
        snap.preview_tail
    );
    assert_eq!(
        snap.preview_tail[0], "|b|",
        "want last {PREVIEW_WINDOW} source lines"
    );
    let flat: Vec<String> = overflows.lock().unwrap().concat();
    assert_eq!(
        flat.join(","),
        "p1,p2,p3,p4",
        "stolen tail commits, want p1..p4 in order"
    );

    // Deferred close + the rendered block: replaced IN PLACE, height constant, head
    // rows overflow above.
    r.close_preview();
    overflows.lock().unwrap().clear();
    r.commit(s(&["t1", "t2", "t3", "t4", "t5", "t6"]));
    let snap = last();
    assert_eq!(snap.label, "", "preview not replaced by the block");
    assert_eq!(snap.tail.join(","), "t3,t4,t5,t6", "window after morph");
    let over = overflows.lock().unwrap().clone();
    assert_eq!(over.len(), 1, "overflow batches = {over:?}");
    assert_eq!(over[0].join(","), "t1,t2", "overflow = {over:?}");
    assert_eq!(
        rows_morph(&snap),
        TAIL_KEEP,
        "rows after morph (no shrink across flush)"
    );
}

/// A preview collapsing into FEWER lines than it occupied (the thinking window folding
/// to its one-line marker) must not shrink the window — the uncovered rows stay as
/// residue and later commits consume them top-down.
// Go: model_test.go:281
#[test]
fn region_morph_residue() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));
    let last = || snaps.lock().unwrap().last().unwrap().clone();

    // Warm to full height, then a thinking preview takes the window over.
    r.commit(s(&["p1", "p2", "p3", "p4"]));
    r.open_preview("rendering table…");
    for l in ["think1", "think2", "think3"] {
        r.preview_line(l);
    }
    assert_eq!(rows_residue(&last()), TAIL_KEEP, "rows during thinking");

    // Collapse: one marker line replaces the header; the three thinking rows become
    // residue — height unchanged, composer must not move.
    r.close_preview();
    r.commit(s(&["◇ thought for 3s"]));
    let snap = last();
    assert_eq!(snap.label, "");
    assert_eq!(
        snap.residue,
        s(&["think1", "think2", "think3"]),
        "want 3 residue rows"
    );
    assert_eq!(rows_residue(&snap), TAIL_KEEP, "the composer would move");

    // Later commits overwrite residue top-down, height still constant.
    r.commit(s(&["", "reply-1"]));
    let snap = last();
    assert_eq!(snap.residue, s(&["think3"]), "residue after 2 lines");
    assert_eq!(rows_residue(&snap), TAIL_KEEP, "rows mid-consumption");
    r.commit(s(&["reply-2", "reply-3"]));
    let snap = last();
    assert!(snap.residue.is_empty(), "residue should be fully consumed");
    assert_eq!(rows_residue(&snap), TAIL_KEEP, "rows after consumption");

    // A new preview's header row also consumes a residue row.
    r.open_preview("rendering table…");
    r.preview_line("t1");
    r.preview_line("t2");
    r.close_preview();
    r.commit(s(&["◇ thought for 1s"])); // residue = [t1 t2]
    let before = rows_residue(&last());
    r.open_preview("rendering code…");
    let snap = last();
    assert_eq!(
        snap.residue.len(),
        1,
        "open_preview over residue: {:?}",
        snap.residue
    );
    assert_eq!(rows_residue(&snap), before, "want {before} rows");

    // drop_preview (turn boundary) clears residue too.
    r.drop_preview();
    let snap = last();
    assert!(
        snap.residue.is_empty() && snap.label.is_empty(),
        "drop left {snap:?}"
    );
}

/// The tool-call lifecycle widget — header + live status row — keeps its clock across
/// relabels, and settling (deferred close + the header/result commits) never changes
/// the window height.
// Go: model_test.go:356
#[test]
fn region_call_preview() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));
    let last = || snaps.lock().unwrap().last().unwrap().clone();

    r.commit(s(&["p1", "p2", "p3", "p4"]));
    r.open_call_preview("[write_file …]");
    let snap = last();
    assert!(snap.since.is_some(), "call preview not raised: {snap:?}");
    assert_eq!(snap.label, "[write_file …]");
    assert_eq!(rows_call(&snap), TAIL_KEEP, "rows with call widget");
    let started = snap.since;

    // The live detail lands on the status row without touching the height.
    r.set_call_detail("0.4k tokens");
    let snap = last();
    assert_eq!(snap.detail, "0.4k tokens", "detail not recorded");
    assert_eq!(rows_call(&snap), TAIL_KEEP, "rows after detail");

    // Expanding the header (composing → full args) keeps the clock and detail.
    r.open_call_preview("[write_file path:a.txt]");
    let snap = last();
    assert_eq!(snap.label, "[write_file path:a.txt]");
    assert_eq!(
        snap.since, started,
        "ensure should relabel in place keeping the clock"
    );
    assert_eq!(snap.detail, "0.4k tokens", "ensure should keep the detail");
    assert_eq!(rows_call(&snap), TAIL_KEEP, "rows after relabel");

    // Settle: deferred close + the single header commit takes the spinner row; the
    // status row leaves a blank placeholder consumed by the result.
    r.close_preview();
    r.set_call_detail("late"); // after the deferred close: no-op
    r.commit(s(&["[write_file path:a.txt]"]));
    let snap = last();
    assert!(
        snap.label.is_empty() && snap.since.is_none() && snap.detail.is_empty(),
        "widget should be settled with detail cleared: {snap:?}"
    );
    assert_eq!(
        snap.residue,
        s(&[""]),
        "status row should leave a blank placeholder"
    );
    assert_eq!(
        rows_call(&snap),
        TAIL_KEEP,
        "rows after settle (composer would move)"
    );
    r.commit(s(&["  ⎿ wrote 1.2 KB"]));
    let snap = last();
    assert!(
        snap.residue.is_empty(),
        "result line should consume the placeholder, got {:?}",
        snap.residue
    );
    assert_eq!(rows_call(&snap), TAIL_KEEP, "rows after result");
}

/// Opening a preview over an existing one folds the old rows into residue
/// (back-to-back streamed tool calls) — height flat.
// Go: model_test.go:460
#[test]
fn region_preview_over_preview() {
    let snaps: Arc<Mutex<Vec<RegionSnapshot>>> = Arc::default();
    let c = Arc::clone(&snaps);
    let mut r = test_region(move |_, snap| c.lock().unwrap().push(snap));
    let last = || snaps.lock().unwrap().last().unwrap().clone();

    r.commit(s(&["p1", "p2", "p3", "p4"]));
    r.open_preview("Calling write_file");
    for l in ["a1", "a2", "a3"] {
        r.preview_line(l);
    }
    let before = rows_residue(&last());

    r.close_preview();
    r.open_preview("Calling bash");
    let snap = last();
    assert_eq!(snap.label, "Calling bash");
    assert!(
        snap.preview_tail.is_empty(),
        "new preview wrong: preview_tail={:?}",
        snap.preview_tail
    );
    assert_eq!(
        snap.residue,
        s(&["a1", "a2", "a3"]),
        "old preview rows should fold into residue"
    );
    assert_eq!(rows_residue(&snap), before, "rows across preview swap");
}

/// Markdown's block spacing is a lone `""` line; when it overflows the window by itself
/// it must still land in scrollback, in order. (Ordering half only: Go's joinOverflow
/// `""`→`" "` substitution is a bubbletea insertAbove workaround, NOT ported — T-02: a
/// blank ratatui `Line` inserts as one blank row.)
// Go: model_test.go:614
#[test]
fn region_blank_line_survives_overflow() {
    let overflows: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = Arc::clone(&overflows);
    let mut r = test_region(move |over, _| c.lock().unwrap().extend(over));

    // Fill the window, then push a blank through it alone.
    r.commit(s(&["a", "b", "c", "d"]));
    r.commit(s(&[""])); // blank enters the window, "a" overflows
    for l in ["e", "f", "g", "h"] {
        r.commit(s(&[l])); // b, c, d overflow; then the blank ALONE
    }
    assert_eq!(
        overflows.lock().unwrap().clone(),
        s(&["a", "b", "c", "d", ""])
    );
}

/// Pins the insert safety contract: the region must never publish a scrollback batch of
/// ≥ screen-height lines (kept per T-06 as cheap insurance for the W2 arithmetic).
// Go: model_test.go:1392
#[test]
fn chunk_overflow_below_screen_height() {
    let lines: Vec<String> = (0..100).map(|i| format!("l{i}")).collect();
    let chunks = chunk_overflow(lines, 30);
    let mut total = 0;
    for c in &chunks {
        assert!(
            c.len() <= 15,
            "chunk of {} lines exceeds half the screen height",
            c.len()
        );
        total += c.len();
    }
    assert_eq!(total, 100, "chunking lost lines");
    assert_eq!(chunks[0][0], "l0", "order broken: first line");
    let last_chunk = chunks.last().unwrap();
    assert_eq!(
        last_chunk[last_chunk.len() - 1],
        "l99",
        "order broken: last line"
    );
    assert!(
        chunk_overflow(Vec::new(), 30).is_empty(),
        "empty overflow must produce no chunks"
    );
    // Tiny terminals still make progress.
    let five: Vec<String> = (0..5).map(|i| format!("l{i}")).collect();
    assert_eq!(
        chunk_overflow(five, 1).len(),
        3,
        "h=1 chunking, want 3 chunks (size 2)"
    );
}

/// The preview writer METERS the stream instead of forwarding it: the header gains a
/// throttled `"· N lines"` counter and no source line ever enters the window — the
/// preview stays exactly one row. (Adapted per the line-based facade `PreviewHandle`:
/// one call per consumed source line; Go's byte-chunk `partial` flag lives in the
/// producer now.)
// Go: sink_test.go:12
#[test]
fn preview_writer_counts() {
    let region = Arc::new(Mutex::new(test_region(|_, _| {})));
    region.lock().unwrap().open_preview("rendering table…");
    let mut w = PreviewWriter {
        region: Arc::clone(&region),
        base: "rendering table…".to_owned(),
        lines: 0,
        last: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
        closed: false,
    };

    w.write_raw_line("| a | b |"); // past the throttle: relabels at 1 line
    w.write_raw_line("| 1 | 2 |"); // within the fresh throttle window: label stays put
    {
        let r = region.lock().unwrap();
        assert!(
            r.preview_tail.is_empty(),
            "source lines leaked into the window: {:?}",
            r.preview_tail
        );
        assert!(r.label.contains("1 line"), "label = {:?}", r.label);
    }
    assert_eq!(w.lines, 2, "count = {}", w.lines);

    // Force the throttle open again: the counter catches up to the line count.
    w.last = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    w.write_raw_line("| 3 |");
    assert!(
        region.lock().unwrap().label.contains("3 lines"),
        "label = {:?}",
        region.lock().unwrap().label
    );

    w.close();
    assert!(
        !region.lock().unwrap().open,
        "close must defer-close the preview"
    );
}

/// A block that flushes before the first throttle tick never shows a counter at all —
/// the short-block case stays visually silent (`last` starts at open time, so the first
/// tick waits a full period).
// Go: sink_test.go:42
#[test]
fn preview_writer_quiet_for_short_blocks() {
    let region = Arc::new(Mutex::new(test_region(|_, _| {})));
    region.lock().unwrap().open_preview("rendering list…");
    let mut w = PreviewWriter {
        region: Arc::clone(&region),
        base: "rendering list…".to_owned(),
        lines: 0,
        last: Instant::now(),
        closed: false,
    };
    w.write_raw_line("- a");
    w.write_raw_line("- b");
    w.close();
    assert_eq!(
        region.lock().unwrap().label,
        "rendering list…",
        "short block must keep the bare label"
    );
}

// Go: sink_test.go:53
#[test]
fn count_lines_wording() {
    assert_eq!(count_lines(1), "1 line");
    assert_eq!(count_lines(5), "5 lines");
}

/// New (no Go twin — the live path went through the bubbletea Program): the Live seam
/// chunks the overflow below the screen height and the snapshot always FOLLOWS the
/// chunks — the publish-ordering law WP44's mailbox consumer relies on.
#[test]
fn live_publish_chunks_then_snapshot() {
    #[derive(Debug, PartialEq)]
    enum Ev {
        Chunk(Vec<String>),
        Snap(usize), // tail length — enough to identify the snapshot
    }
    struct RecPub(Arc<Mutex<Vec<Ev>>>);
    impl LivePublish for RecPub {
        fn scrollback(&self, rows: Vec<String>) {
            self.0.lock().unwrap().push(Ev::Chunk(rows));
        }
        fn region(&self, snap: RegionSnapshot) {
            self.0.lock().unwrap().push(Ev::Snap(snap.tail.len()));
        }
    }

    let events: Arc<Mutex<Vec<Ev>>> = Arc::default();
    let mut r = Region::new(
        Emit::Live {
            tx: Box::new(RecPub(Arc::clone(&events))),
        },
        Arc::new(AtomicU16::new(0)),
        // chunk size = max(2, 6/2) = 3; on a 6-row terminal the T-40 frame floor also
        // leaves the staging window empty, so the commit itself overflows all four.
        Arc::new(AtomicU16::new(6)),
    );

    r.commit(s(&["l0", "l1", "l2", "l3"])); // 4 overflow rows → chunks of 3 + 1
    r.flush(); // nothing left staged: a bare snapshot
    let got = std::mem::take(&mut *events.lock().unwrap());
    assert_eq!(
        got,
        vec![
            Ev::Chunk(s(&["l0", "l1", "l2"])),
            Ev::Chunk(s(&["l3"])),
            Ev::Snap(0),
            Ev::Snap(0),
        ]
    );
}

/// T-40 (no Go twin): the staging window's cap is `TAIL_KEEP` on any terminal with room
/// for it and TRIMMED on a short one, so the frame always leaves `max(2, screen_h/2)`
/// rows — the batch `chunk_overflow` inserts — above itself. Without this the frame on a
/// sub-12-row terminal overlaps the rows `insert_before` is scrolling out and the
/// scrollback is permanently damaged. A HEIGHT change re-applies the cap immediately.
#[test]
fn staging_window_has_a_floor_on_short_terminals() {
    let height = Arc::new(AtomicU16::new(24));
    let last: Arc<Mutex<Option<RegionSnapshot>>> = Arc::default();
    let seen = Arc::clone(&last);
    let mut r = Region::new(
        Emit::Test(Box::new(move |_over, snap| {
            *seen.lock().unwrap() = Some(snap);
        })),
        Arc::new(AtomicU16::new(80)),
        Arc::clone(&height),
    );
    let staged = |last: &Arc<Mutex<Option<RegionSnapshot>>>| {
        last.lock().unwrap().as_ref().map_or(0, |s| s.tail.len())
    };

    r.commit(s(&["a", "b", "c", "d", "e", "f"]));
    assert_eq!(
        staged(&last),
        TAIL_KEEP,
        "80x24 has room for the full window"
    );

    // Shrinking to 12 rows leaves a 6-row insert batch and a 5-row minimum frame: one
    // staged row. The retrim happens without any further output.
    height.store(12, Ordering::Relaxed);
    r.retrim();
    assert_eq!(staged(&last), 1, "12 rows leave exactly one staged row");

    // At 10 rows the frame is already at its floor — nothing may stage.
    height.store(10, Ordering::Relaxed);
    r.retrim();
    assert_eq!(staged(&last), 0, "a 10-row terminal stages nothing");
    r.commit(s(&["x", "y"]));
    assert_eq!(
        staged(&last),
        0,
        "and new output goes straight to scrollback"
    );

    // Growing back restores the full window as output refills it.
    height.store(24, Ordering::Relaxed);
    r.commit(s(&["1", "2", "3", "4", "5"]));
    assert_eq!(staged(&last), TAIL_KEEP, "the window refills after a grow");
}

/// New: `UiStreamSink::done` = `drop_preview` + the scope-pop callback (sink.go:24-27) —
/// a leaked/deferred preview dies with the turn.
#[test]
fn stream_sink_done_drops_preview_and_pops_scope() {
    let region = Arc::new(Mutex::new(test_region(|_, _| {})));
    let popped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&popped);
    let sink = StreamSink::new(Arc::clone(&region), move || {
        flag.store(true, Ordering::Relaxed);
    });

    region.lock().unwrap().open_call_preview("[bash …]");
    sink.done();
    let r = region.lock().unwrap();
    assert_eq!(r.label, "", "done must drop the preview");
    assert!(r.since.is_none() && r.residue.is_empty());
    drop(r);
    assert!(
        popped.load(Ordering::Relaxed),
        "done must pop the turn scope"
    );
}

/// New: `block_preview` opens the metered one-row preview through the facade trait, and
/// the first counter tick waits a FULL throttle period (sink.go:18-21); `close` is the
/// deferred close.
#[test]
fn stream_sink_block_preview_meters() {
    let region = Arc::new(Mutex::new(test_region(|_, _| {})));
    let sink = StreamSink::new(Arc::clone(&region), || {});

    let mut h = sink.block_preview("rendering code…");
    assert_eq!(region.lock().unwrap().label, "rendering code…");
    h.write_raw_line("fn main() {}");
    h.write_raw_line("// two lines, fresh writer: still inside the first period");
    assert_eq!(
        region.lock().unwrap().label,
        "rendering code…",
        "first tick must wait a full throttle period"
    );
    h.close();
    let r = region.lock().unwrap();
    assert!(!r.open, "close must defer-close");
    assert_eq!(
        r.label, "rendering code…",
        "deferred close keeps the row until the morph"
    );
}

/// New: Drop = close (facade contract), and a late Drop after an explicit close must
/// NOT touch a newer preview (the closed-guard).
#[test]
fn preview_writer_drop_closes() {
    let region = Arc::new(Mutex::new(test_region(|_, _| {})));
    let sink = StreamSink::new(Arc::clone(&region), || {});

    // Drop without close: the preview defer-closes.
    let h = sink.block_preview("rendering table…");
    drop(h);
    assert!(
        !region.lock().unwrap().open,
        "Drop must defer-close the preview"
    );

    // Explicit close, then a NEW preview, then the old writer's Drop: stays open.
    let mut h = sink.block_preview("rendering list…");
    h.close();
    region.lock().unwrap().open_preview("rendering quote…");
    drop(h);
    assert!(
        region.lock().unwrap().open,
        "a closed writer's Drop must not close a newer preview"
    );
}

/// New: the theme pins (`TUI_CONTRACTS` §9) — raw SGR consts byte-exact, the dark
/// atomic defaulting true, and the 236/254 input shades.
#[test]
fn theme_sgr_pins() {
    assert_eq!(crate::ui::theme::FAINT, "\x1b[2m");
    assert_eq!(crate::ui::theme::CYAN, "\x1b[36m");
    assert_eq!(crate::ui::theme::GREEN, "\x1b[32m");
    assert_eq!(crate::ui::theme::YELLOW, "\x1b[33m");
    assert_eq!(crate::ui::theme::RED, "\x1b[31m");
    assert_eq!(crate::ui::theme::REV_ON, "\x1b[7m");
    assert_eq!(crate::ui::theme::RESET, "\x1b[0m");
    assert_eq!(crate::ui::theme::ERR_PREFIX, "\x1b[31m⚠ \x1b[0m");
    assert_eq!(crate::ui::theme::SEARCH_HIT, "\x1b[7m");
    assert_eq!(crate::ui::theme::SEARCH_CUR, "\x1b[7;33m");

    assert_eq!(crate::ui::theme::input_bg(true), "\x1b[48;5;236m");
    assert_eq!(crate::ui::theme::input_bg(false), "\x1b[48;5;254m");
}
