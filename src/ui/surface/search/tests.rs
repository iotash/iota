#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP47 row-panel filter-search suite (internal/ui/`search_test.go` ports): the matcher
//! (`ascii_fold`'s byte-length invariant, `match_ranges`), `highlight_line`'s SGR
//! replay, the `'/'` availability gate, and the eight row-filter laws — live narrowing
//! per keystroke, the original-index commit contract, `c` clear, ESC-in-field, the
//! no-match-shows-everything fallback, checks keyed by underlying index, the
//! `"Other…"` row never filtered out, and matching through baked-in styling.
//!
//! The View-panel jump search (hit walker, re-anchoring, wrapped centring) rides with
//! WP54 (T-12); the row filter is whole here.
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/search.rs` of the terminal crate; merged 2026-09-02).

use crate::text::ansi::strip_sgr;
use crate::ui::facade::{Panel, PanelBody, PanelKind, TabbedResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::surface::search::{
    SearchMode, ascii_fold, highlight_line, match_ranges, matches_query,
};
use crate::ui::surface::tabbed::PanelState;
use crate::ui::surface::{SurfaceEffect, SurfaceState};
use crate::ui::theme::{CYAN, FAINT, RESET, SEARCH_CUR, SEARCH_HIT};

// --- harness ----------------------------------------------------------------

/// One open surface driven exactly as the loop drives it: keys through the pure
/// `surface_key` ladder, rows through `render_surface` at 80 columns (Go `content(m)`
/// narrowed to the surface block — the frame around it is WP44/WP46 territory).
struct Surf {
    st: SurfaceState,
}

impl Surf {
    fn open(panels: Vec<Panel>) -> Self {
        let st = SurfaceState::new(false, panels);
        Self { st }
    }

    fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    /// Presses a key that must leave the surface open.
    fn tap(&mut self, k: KeyEvent) {
        assert!(
            !matches!(self.press(k), SurfaceEffect::Close(_)),
            "key closed the surface unexpectedly"
        );
    }

    fn typed(&mut self, s: &str) {
        for c in s.chars() {
            self.tap(ch(c));
        }
    }

    fn ps(&self, i: usize) -> &PanelState {
        &self.st.slots[i].state
    }

    fn rows(&mut self) -> Vec<String> {
        self.st.render(80).rows
    }

    fn content(&mut self) -> String {
        self.rows().join("\n")
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

fn long_items(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("item-{i:02}")).collect()
}

/// Go `openSearchList`: a filterable list long enough for `'/'` to be live.
fn open_search_list(items: Vec<String>) -> Surf {
    Surf::open(vec![
        Panel::list("Model".to_owned(), items).with_search(true),
    ])
}

// --- matching ---------------------------------------------------------------

/// The invariant the whole offset scheme rests on: match offsets index back into the
/// ORIGINAL string, so folding must never change how many bytes anything takes
/// (`to_lowercase` cannot promise it — U+0130 lowercases to two runes).
// Go: internal/ui/search_test.go:19 TestAsciiFoldPreservesByteLength
#[test]
fn test_ascii_fold_preserves_byte_length() {
    for s in ["Hello", "ÄÖÜ", "İstanbul", "中文 MiXeD", "ß"] {
        assert_eq!(
            ascii_fold(s).len(),
            s.len(),
            "ascii_fold({s:?}) changed the byte length"
        );
    }
    assert_eq!(ascii_fold("AbC-XyZ"), "abc-xyz");
}

// Go: internal/ui/search_test.go:30 TestMatchRanges
#[test]
fn test_match_ranges() {
    /// name, text, query, expected ranges.
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(usize, usize)],
    );
    let cases: &[Case] = &[
        ("simple", "hello world", "world", &[(6, 11)]),
        ("case insensitive", "Hello World", "hello", &[(0, 5)]),
        ("repeated non-overlapping", "aaaa", "aa", &[(0, 2), (2, 4)]),
        ("no match", "hello", "zzz", &[]),
        ("query longer than text", "hi", "hello", &[]),
        ("empty query", "hello", "", &[]),
        ("cjk", "中文测试", "文测", &[(3, 9)]),
    ];
    for (name, text, query, want) in cases {
        assert_eq!(
            match_ranges(text, query),
            *want,
            "{name}: match_ranges({text:?}, {query:?})"
        );
    }
    // The predicate the row filter runs on shares the fold (empty = everything).
    assert!(matches_query("Hello World", "hello"));
    assert!(matches_query("anything", ""));
    assert!(!matches_query("hello", "zzz"));
}

// --- highlighting -----------------------------------------------------------

/// The basic wrap: the hit is reversed, the rest of the line is untouched, and
/// stripping the SGR gives the original back.
// Go: internal/ui/search_test.go:57 TestHighlightLinePlain
#[test]
fn test_highlight_line_plain() {
    let got = highlight_line("hello world", "world", None);
    assert!(
        got.contains(&format!("{SEARCH_HIT}world")),
        "hit not highlighted: {got:?}"
    );
    assert_eq!(strip_sgr(&got), "hello world");
}

/// Why this cannot be a string replace: a line carrying its own SGR (a pre-styled row,
/// a coloured body) must still be coloured AFTER the highlight closes — a bare reset
/// would strip the rest of the line bare.
// Go: internal/ui/search_test.go:71 TestHighlightLineKeepsOwnColor
#[test]
fn test_highlight_line_keeps_own_color() {
    let line = format!("{CYAN}error: file not found{RESET}");
    let got = highlight_line(&line, "file", None);
    assert_eq!(strip_sgr(&got), "error: file not found");
    let after = &got[got.find("file").unwrap() + "file".len()..];
    assert!(
        after.contains(CYAN),
        "line colour not replayed after the hit: {got:?}"
    );
}

/// Offsets are computed on the plain text, so an escape sequence sitting inside a match
/// survives whole.
// Go: internal/ui/search_test.go:86 TestHighlightLineNeverSplitsEscapes
#[test]
fn test_highlight_line_never_splits_escapes() {
    let line = format!("abc{CYAN}def");
    let got = highlight_line(&line, "cd", None);
    assert!(got.contains(CYAN), "escape mangled: {got:?}");
    assert_eq!(strip_sgr(&got), "abcdef");
}

/// The hit `n`/`p` is parked on is brighter than the others.
// Go: internal/ui/search_test.go:99 TestHighlightLineCurrentHit
#[test]
fn test_highlight_line_current_hit() {
    let got = highlight_line("foo bar foo", "foo", Some(8));
    assert!(
        got.contains(SEARCH_CUR),
        "current hit not distinguished: {got:?}"
    );
    assert!(
        got.contains(SEARCH_HIT),
        "other hit not highlighted: {got:?}"
    );
}

// Go: internal/ui/search_test.go:109 TestHighlightLineNoMatchReturnsInput
#[test]
fn test_highlight_line_no_match_returns_input() {
    let line = format!("{CYAN}hello{RESET}");
    assert_eq!(highlight_line(&line, "zzz", None), line);
}

// --- the '/' gate -----------------------------------------------------------

/// `'/'` is inert without the opt-in, and inert on a list that already fits on screen —
/// there is nothing to search for when every option is visible. A View ignores the flag
/// but still needs overflow.
// Go: internal/ui/search_test.go:137 TestSearchGatedByFlagAndOverflow
#[test]
fn test_search_gated_by_flag_and_overflow() {
    let cases: &[(&str, PanelKind, bool, usize, bool)] = &[
        ("flag off, long list", PanelKind::List, false, 40, false),
        ("flag on, short list", PanelKind::List, true, 3, false),
        ("flag on, long list", PanelKind::List, true, 40, true),
        ("view ignores the flag", PanelKind::View, false, 40, true),
        ("view, short body", PanelKind::View, false, 3, false),
    ];
    for (name, kind, search, n, want) in cases {
        let rows = long_items(*n);
        let mut p = Panel::of("P", PanelBody::empty(*kind)).with_search(*search);
        match &mut p.body {
            PanelBody::View(v) => v.lines.clone_from(&rows),
            PanelBody::List(l) | PanelBody::Multi(l) => l.items.clone_from(&rows),
            PanelBody::Picker(pk) => pk.items.clone_from(&rows),
            _ => {}
        }
        let surface = Surf::open(vec![p]);
        assert_eq!(
            surface.ps(0).search_available(&surface.st.slots[0].spec),
            *want,
            "{name}: search_available"
        );
    }

    // And the gate is what `'/'` consults: on a short list the key falls through
    // unhandled instead of opening a query field.
    let mut short = open_search_list(long_items(3));
    short.tap(ch('/'));
    assert_eq!(short.ps(0).search.mode, SearchMode::Off);
    let mut long = open_search_list(long_items(40));
    long.tap(ch('/'));
    assert_eq!(long.ps(0).search.mode, SearchMode::Typing);
}

// --- row-panel filtering ----------------------------------------------------

/// Narrowing happens as the query is typed — before Enter, so a query that matches
/// nothing is visible immediately.
// Go: internal/ui/search_test.go:169 TestSearchFiltersLive
#[test]
fn test_search_filters_live() {
    let mut s = open_search_list(long_items(40));
    assert_eq!(s.ps(0).view.len(), 40, "unfiltered view");

    s.tap(ch('/'));
    assert_eq!(
        s.ps(0).search.mode,
        SearchMode::Typing,
        "\"/\" did not open the query field"
    );
    s.typed("item-1");
    assert_eq!(s.ps(0).view.len(), 10, "live filter kept the wrong rows");
    // Still only TYPING: the panel narrows before anything is committed.
    assert_eq!(s.ps(0).search.mode, SearchMode::Typing);
    assert!(s.ps(0).search.query.is_empty());
}

/// The contract every caller depends on: `Cursor` is read back as an index into the
/// ORIGINAL items, so filtering must not renumber it.
// Go: internal/ui/search_test.go:191 TestSearchCursorStaysUnderlyingIndex
#[test]
fn test_search_cursor_stays_underlying_index() {
    let mut s = open_search_list(long_items(40));
    s.tap(ch('/'));
    s.typed("item-3");
    s.tap(key(KeyCode::Enter)); // apply the filter
    s.tap(key(KeyCode::Down)); // second match: item-31
    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled, "surface cancelled");
    assert_eq!(
        r.panels[0].cursor, 31,
        "Cursor must index the ORIGINAL items"
    );
}

/// `c` lifts the filter — the panel's own keys were never taken away.
// Go: internal/ui/search_test.go:213 TestSearchClearRestoresList
#[test]
fn test_search_clear_restores_list() {
    let mut s = open_search_list(long_items(40));
    s.tap(ch('/'));
    s.typed("item-1");
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.ps(0).view.len(), 10, "filter not applied");
    assert_eq!(s.ps(0).search.mode, SearchMode::Applied);

    s.tap(ch('c'));
    assert_eq!(s.ps(0).search.mode, SearchMode::Off);
    assert_eq!(s.ps(0).view.len(), 40);
}

/// ESC in the query field drops the search entirely rather than applying a half-typed
/// query — and it does NOT close the surface.
// Go: internal/ui/search_test.go:233 TestSearchEscapeInFieldLeavesSearch
#[test]
fn test_search_escape_in_field_leaves_search() {
    let mut s = open_search_list(long_items(40));
    s.tap(ch('/'));
    s.typed("item-1");
    assert!(
        !matches!(s.press(key(KeyCode::Esc)), SurfaceEffect::Close(_)),
        "ESC in the query field closed the whole surface"
    );
    assert_eq!(s.ps(0).search.mode, SearchMode::Off);
    assert_eq!(s.ps(0).view.len(), 40);
    // Only now does ESC reach the surface itself.
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);
}

/// A filter that emptied the list would leave a cursor pointing at nothing Enter could
/// still commit, so no-match falls back to the whole list — and says so.
// Go: internal/ui/search_test.go:253 TestSearchNoMatchShowsEverything
#[test]
fn test_search_no_match_shows_everything() {
    let mut s = open_search_list(long_items(40));
    s.tap(ch('/'));
    s.typed("zzzz");
    assert_eq!(s.ps(0).view.len(), 40, "no-match view must show everything");
    // The fallback stays distinguishable from "matched everything", or the hint row
    // would claim 40 of 40 matches for a query that found none.
    assert!(
        !s.ps(0).filtered_count(&s.st.slots[0].spec).1,
        "filtered_count reported a match for a query that found none"
    );
    assert!(
        s.content().contains("no match"),
        "the query row does not say the search found nothing:\n{}",
        s.content()
    );
}

/// Multi checks are recorded against underlying indices, so options checked under one
/// query are still submitted after another query hides them.
// Go: internal/ui/search_test.go:277 TestSearchMultiChecksSurviveFiltering
#[test]
fn test_search_multi_checks_survive_filtering() {
    let mut s = Surf::open(vec![
        Panel::multi("Flags".to_owned(), long_items(40)).with_search(true),
    ]);

    // Check item-05 through one filter…
    s.tap(ch('/'));
    s.typed("item-05");
    s.tap(key(KeyCode::Enter));
    s.tap(ch(' '));
    // …then switch to a filter that hides it and check item-22.
    s.tap(ch('/'));
    s.typed("item-22");
    s.tap(key(KeyCode::Enter));
    s.tap(ch(' '));

    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(
        r.panels[0].checked,
        vec![5, 22],
        "a hidden check must still commit"
    );
}

/// `"Other…"` is the way to answer when no option fits, so a filter must never hide it.
// Go: internal/ui/search_test.go:305 TestSearchKeepsCustomRowVisible
#[test]
fn test_search_keeps_custom_row_visible() {
    let mut s = Surf::open(vec![
        Panel::list("Model".to_owned(), long_items(40))
            .with_search(true)
            .with_custom(true),
    ]);
    s.tap(ch('/'));
    s.typed("item-1");
    let other_idx = 40;
    assert!(
        s.ps(0).view.contains(&other_idx),
        "the Other… row was filtered away: {:?}",
        s.ps(0).view
    );
    // It is the LAST visible row, so scrolling to it always reaches the editor.
    assert_eq!(s.ps(0).view.last(), Some(&other_idx));
    s.tap(key(KeyCode::Enter)); // apply, handing the keyboard back to the panel
    s.tap(ch('G'));
    assert_eq!(s.ps(0).cursor, other_idx);
    assert!(
        s.content().contains("Other…"),
        "the Other… row is not reachable under the filter:\n{}",
        s.content()
    );
}

/// Rows can arrive pre-coloured (a `/debug` request list), and the query must match
/// what the user SEES, not the escape bytes carrying it.
// Go: internal/ui/search_test.go:330 TestSearchMatchesThroughStyling
#[test]
fn test_search_matches_through_styling() {
    let mut items = long_items(40);
    items[7] = format!("{CYAN}item-07{RESET}{FAINT} (styled){RESET}");
    let mut s = open_search_list(items);

    s.tap(ch('/'));
    s.typed("styled");
    assert_eq!(
        s.ps(0).view,
        vec![7],
        "the filter must keep just the styled row"
    );

    // And the escape bytes themselves must not be searchable.
    s.tap(key(KeyCode::Esc));
    s.tap(ch('/'));
    s.typed("36m");
    assert_eq!(
        s.ps(0).view.len(),
        40,
        "an SGR fragment matched rows; escapes must be invisible to search"
    );
}

// --- the view mapping's own laws --------------------------------------------

/// `view` is ALWAYS populated (identity when nothing is filtered) so rendering and
/// navigation have one code path, and `view_pos`/`set_view_pos`/`sync_cursor` translate
/// between visible rows and the underlying index the commit reads (search.go:186-289).
// Go: internal/ui/search.go:228-310 (rebuildView/viewPos/setViewPos/syncCursor laws —
// pinned in Go only through the model tests above; unit-covered here)
#[test]
fn view_map_is_identity_when_unfiltered_and_translates_when_filtered() {
    let mut s = open_search_list(long_items(40));
    assert_eq!(s.ps(0).view, (0..40).collect::<Vec<_>>());
    assert_eq!(s.ps(0).view_pos(), 0);

    s.tap(ch('/'));
    s.typed("item-2");
    assert_eq!(s.ps(0).view, (20..30).collect::<Vec<_>>());
    // The cursor was dragged onto the first visible row by sync_cursor.
    assert_eq!(s.ps(0).cursor, 20);
    assert_eq!(s.ps(0).view_pos(), 0);

    // Paging/navigation counts VISIBLE rows: a filter's gaps are skipped, never paged
    // across (the cursor stays an underlying index throughout).
    s.tap(key(KeyCode::Enter));
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    assert_eq!(s.ps(0).cursor, 22);
    assert_eq!(s.ps(0).view_pos(), 2);
    s.tap(ch('G'));
    assert_eq!(s.ps(0).cursor, 29, "G lands on the last VISIBLE row");
    s.tap(ch('g'));
    assert_eq!(s.ps(0).cursor, 20, "g lands on the first VISIBLE row");
}

/// Per-panel search survives tabbing away and back once APPLIED; a half-typed query is
/// abandoned by `set_focus`, so the field never holds the cursor from an unfocused tab.
/// Note the ladder makes Tab unreachable while typing (row 2 feeds it to the field —
/// Go behaves identically), which is why the abandon path is driven directly here.
// Go: internal/ui/tabbed.go:313-331 setFocus (per-panel search survival)
#[test]
fn tab_switch_abandons_typing_but_keeps_an_applied_filter() {
    let panels = vec![
        Panel::list("A".to_owned(), long_items(40)).with_search(true),
        Panel::list("B".to_owned(), long_items(40)).with_search(true),
    ];
    let mut s = Surf::open(panels);

    // Applied on tab A: survives the round trip.
    s.tap(ch('/'));
    s.typed("item-1");
    s.tap(key(KeyCode::Enter));
    s.tap(key(KeyCode::Tab));
    assert_eq!(s.st.focus, 1);
    s.tap(key(KeyCode::Tab));
    assert_eq!(s.st.focus, 0);
    assert_eq!(s.ps(0).search.mode, SearchMode::Applied);
    assert_eq!(s.ps(0).view.len(), 10);

    // Half-typed: Tab is swallowed by the query field (letters must type) …
    s.tap(ch('/'));
    s.typed("item-2");
    s.tap(key(KeyCode::Tab));
    assert_eq!(
        s.st.focus, 0,
        "Tab must not switch tabs out of the query field"
    );
    assert_eq!(s.ps(0).search.mode, SearchMode::Typing);
    // … and a focus move from anywhere else drops it.
    s.st.set_focus(1);
    assert_eq!(s.ps(0).search.mode, SearchMode::Off);
    assert_eq!(s.ps(0).view.len(), 40);
}

/// The query field REPLACES the hint row, so the surface's height is identical in and
/// out of search — entering search may never bounce the frame (tabbed.go:694-701).
// Go: internal/ui/tabbed.go:694-701 (renderSearchRow replaces the hint row)
#[test]
fn search_row_replaces_the_hint_row_without_changing_height() {
    let mut s = open_search_list(long_items(40));
    let before = s.rows().len();
    s.tap(ch('/'));
    assert_eq!(s.rows().len(), before, "entering search changed the height");
    let row = s.rows().pop().unwrap();
    assert!(
        row.starts_with(&format!("{CYAN}/{RESET}")),
        "the query row does not lead with the cyan slash: {row:?}"
    );
    assert!(
        strip_sgr(&row).ends_with("type to search · Esc cancel"),
        "typing hint: {:?}",
        strip_sgr(&row)
    );
    s.typed("item-1");
    assert!(strip_sgr(&s.rows().pop().unwrap()).ends_with("10 of 40 · Enter filter · Esc cancel"));
    s.typed("zzz");
    assert!(strip_sgr(&s.rows().pop().unwrap()).ends_with("no match · Esc cancel"));
    // And an applied filter's hint keeps every panel key plus the way back.
    s.tap(key(KeyCode::Esc));
    s.tap(ch('/'));
    s.typed("item-1");
    s.tap(key(KeyCode::Enter));
    assert_eq!(
        strip_sgr(&s.rows().pop().unwrap()),
        "\"item-1\" 10/40 · ↑↓ move · ←→ page · Enter confirm · q/Esc cancel · c clear"
    );
}
