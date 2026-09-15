#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP54 View jump-search suite (internal/ui/`search_test.go:415-640`, 7 tests).
//!
//! Row panels FILTER (WP47's `tests/search.rs`); a View JUMPS. The laws pinned here:
//! hits are collected against LOGICAL lines and the landing line is centred at render
//! time — "the key sets the intent, the render resolves it", because only the render
//! knows the wrapped geometry; `n`/`p` walk and wrap around; a live panel's refresh
//! must NOT yank the walker back to the first hit, and when content appears above the
//! parked match every index shifts but the match being read does not; the ESC ladder is
//! walker → query field → off → close; and the highlight lives in the rendered body,
//! disappearing with the search.
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/view_search.rs` of the terminal crate; merged 2026-09-02).

use std::sync::{Arc, Mutex, PoisonError};

use crate::text::ansi::strip_sgr;
use crate::ui::facade::{Panel, RefreshFn, TabbedResult};
use crossterm::event::KeyCode;

use crate::ui::testutil::{Surf, ch, key};

use crate::ui::surface::SurfaceEffect;
use crate::ui::surface::search::{SearchHit, SearchMode};
use crate::ui::theme::SEARCH_CUR;

/// Go `viewBody(n)`: filler with a hit near the top (line 5) and one far down (line 30).
fn view_body(n: usize) -> Vec<String> {
    let mut out: Vec<String> = (0..n).map(|i| format!("line {i}: nothing here")).collect();
    "line 5: needle one".clone_into(&mut out[5]);
    "line 30: needle two".clone_into(&mut out[30]);
    out
}

/// Go `openSearchView`.
fn open_search_view(lines: Vec<String>) -> Surf {
    Surf::open(vec![Panel::view("Body".to_owned(), lines)])
}

/// A live View whose `refresh` closure re-reads a shared slot the test can rewrite —
/// `/tools` and `/debug` refresh twice a second under exactly this shape.
fn live_view(title: &str, slot: &Arc<Mutex<Vec<String>>>) -> Panel {
    let body = Arc::clone(slot);
    let refresh: RefreshFn =
        Box::new(move || body.lock().unwrap_or_else(PoisonError::into_inner).clone());
    Panel::view(
        title.to_owned(),
        slot.lock().unwrap_or_else(PoisonError::into_inner).clone(),
    )
    .with_refresh(refresh)
}

// --- the suite --------------------------------------------------------------

// Enter enters the
// walker parked on the first hit, and the landing line sits near the middle of the
// window rather than scraping the top or bottom edge.
#[test]
fn enter_parks_the_walker_on_the_first_hit_centred() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");

    assert_eq!(
        s.ps(0).search.mode,
        SearchMode::Applied,
        "Enter did not enter the walker"
    );
    assert_eq!(s.ps(0).search.hits.len(), 2, "wrong hit count");

    let _ = s.content(); // the render resolves the pending jump
    // The first hit sits at line 5, closer to the top than half a window, so centring
    // clamps to the top rather than scrolling past the start.
    assert_eq!(
        s.ps(0).offset,
        0,
        "a hit near the top must clamp to offset 0"
    );

    // The second is far enough down to actually land mid-window.
    s.tap(ch('n'));
    let _ = s.content();
    let rows = s.ps(0).rows;
    assert_eq!(
        s.ps(0).offset,
        30 - rows / 2,
        "hit line 30 not centred in {rows} rows"
    );
}

// n walks forward through
// the hits and wraps at the end; p walks back.
#[test]
fn n_and_p_walk_the_hits_and_wrap() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");

    s.tap(ch('n'));
    assert_eq!(s.ps(0).search.hit_idx, 1, "after n");
    s.tap(ch('n'));
    assert_eq!(s.ps(0).search.hit_idx, 0, "n past the last hit must wrap");
    s.tap(ch('p'));
    assert_eq!(
        s.ps(0).search.hit_idx,
        1,
        "p before the first hit must wrap"
    );
}

// A live panel
// (/tools, /debug refresh twice a second) must not yank the walker back to the first hit
// under the reader: with the walker reset every tick, n could never reach the third hit.
#[test]
fn a_refresh_leaves_the_walker_where_the_reader_is() {
    let body = Arc::new(Mutex::new(view_body(60)));
    let mut s = Surf::open(vec![live_view("Tools", &body)]);
    s.search("needle");
    s.tap(ch('n'));
    let _ = s.content();
    let (want_idx, want_off) = (s.ps(0).search.hit_idx, s.ps(0).offset);

    s.tick();
    let _ = s.content();
    assert_eq!(
        s.ps(0).search.hit_idx,
        want_idx,
        "a refresh moved the walker"
    );
    assert_eq!(s.ps(0).offset, want_off, "a refresh scrolled the body");
}

// The
// walker is anchored to the HIT, not to its ordinal: content appearing above it
// renumbers every index while the reader is still looking at the same match.
#[test]
fn content_appearing_above_reanchors_the_walker_to_its_hit() {
    let base = view_body(60);
    let live = Arc::new(Mutex::new(base.clone()));
    let mut s = Surf::open(vec![live_view("Log", &live)]);
    s.search("needle");
    s.tap(ch('n'));
    let parked: SearchHit = s.ps(0).current_hit().expect("the second hit, at line 30");
    assert_eq!(parked.line, 30);

    // A new matching row arrives at the top: every hit index shifts by one.
    {
        let mut slot = live.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = vec!["line -1: needle zero".to_owned()];
        next.extend(base.iter().cloned());
        *slot = next;
    }
    s.tick();

    assert_eq!(
        s.ps(0).search.hits.len(),
        3,
        "hits not re-collected after the new row"
    );
    let got = s.ps(0).current_hit().expect("the walker lost its hit");
    assert_eq!(
        got.line,
        parked.line + 1,
        "the walker must stay on the same match, now one line lower"
    );
}

// From the walker, q/Esc
// reopens the query field (the user is refining, not leaving), Esc there drops the
// search, and only a third Esc reaches the surface itself.
#[test]
fn the_escape_ladder_is_walker_then_field_then_off_then_close() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");

    s.tap(key(KeyCode::Esc));
    assert_eq!(
        s.ps(0).search.mode,
        SearchMode::Typing,
        "ESC from the walker must reopen the field"
    );
    s.tap(key(KeyCode::Esc));
    assert_eq!(
        s.ps(0).search.mode,
        SearchMode::Off,
        "ESC in the field must drop the search"
    );
    // Only now does ESC reach the surface itself.
    let r: TabbedResult = match s.press(key(KeyCode::Esc)) {
        SurfaceEffect::Close(r) => r,
        _ => panic!("a third ESC should close the surface"),
    };
    assert!(r.cancelled);
}

/// The `q` half of the same ladder rung: from the walker `q` REFINES rather than
/// closing, which is why the applied-search hint advertises `q/Esc edit`.
#[test]
fn view_search_q_reopens_the_field_from_the_walker() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");
    s.tap(ch('q'));
    assert_eq!(s.ps(0).search.mode, SearchMode::Typing);
    // Refining is seeded with the applied query and the cursor after it.
    assert_eq!(s.ps(0).search.input.value(), "needle");
}

// The rendered panel
// carries the highlight, and dropping the search takes it away again.
#[test]
fn the_highlight_lives_in_the_rendered_body_and_leaves_with_the_search() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");
    assert!(
        s.content().contains(SEARCH_CUR),
        "the current hit is not highlighted in the rendered body"
    );
    s.tap(key(KeyCode::Esc)); // → typing
    s.tap(key(KeyCode::Esc)); // → off
    assert!(
        !s.content().contains(SEARCH_CUR),
        "the highlight survived leaving search"
    );
}

// With
// Wrap on, offset counts WRAPPED rows while hits are recorded against logical lines: the
// jump has to convert, or long lines send it to the wrong place.
#[test]
fn centring_a_wrapped_view_counts_wrapped_rows() {
    let long = "padding ".repeat(30); // wraps to several rows at width 80
    let mut lines = vec![long; 40];
    lines[20] = "the needle is here".to_owned();
    let mut s = Surf::open(vec![Panel::view("Body".to_owned(), lines).with_wrap(true)]);

    s.search("needle");
    let _ = s.content();

    let starts = s.ps(0).wrap_starts.clone();
    assert_eq!(starts.len(), 40, "wrap_starts must map every logical line");
    let want_row = starts[20];
    assert!(
        want_row > 20,
        "the fixture is not exercising wrapping: logical line 20 starts at row {want_row}"
    );
    let rows = s.ps(0).rows;
    assert_eq!(
        s.ps(0).offset,
        want_row - rows / 2,
        "wrapped row {want_row} not centred in {rows} rows"
    );
}

/// The applied-search hint row is byte-exact and counts the walker's position, which is
/// the only place the hit ordinal is shown (tabbed.go:883-903).
#[test]
fn view_search_hint_row_counts_the_walker() {
    let mut s = open_search_view(view_body(60));
    s.search("needle");
    let plain = strip_sgr(&s.content());
    assert!(
        plain.contains("\"needle\" 1/2 · n next · p prev · q/Esc edit"),
        "applied hint wrong:\n{plain:?}"
    );
    s.tap(ch('n'));
    assert!(strip_sgr(&s.content()).contains("\"needle\" 2/2 · n next · p prev · q/Esc edit"));

    // A query that matches nothing keeps the body and says so.
    s.tap(key(KeyCode::Esc)); // → typing (seeded with "needle")
    s.typed("XX");
    s.tap(key(KeyCode::Enter));
    assert!(strip_sgr(&s.content()).contains("\"needleXX\" no match · q/Esc edit"));
}
