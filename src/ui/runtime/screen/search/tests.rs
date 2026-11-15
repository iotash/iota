#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The conversation search on the full screen, replayed through vt100: the View panel's JUMP cases
//! (`surface/search/view_search_tests.rs`) carried onto the buffer — the first jump centred, n/p
//! wrapping, the walker holding its hit through new output, Esc's way back, a new query from the
//! walker, the highlight living and leaving with the search, a wrapped row counted by its lines, the
//! counter — and what only the buffer has: a jump while following the tail, a hit 500 screens back
//! and the exact way back from it, a query that matches nothing, a hit that straddles a wrap.

use ratatui::layout::Size;

use super::SearchOp;
use crate::ui::render::frame::FrameView;
use crate::ui::render::theme::{SEARCH_CUR, SEARCH_HIT};
use crate::ui::runtime::screen::{Prompt, Screen, ViewOp};
use crate::ui::testutil::SharedBuf;

const W: u16 = 40;
const H: u16 = 12;

fn frame() -> FrameView {
    FrameView {
        rows: vec![
            "┄".repeat(40),
            "❯ ".to_owned(),
            "┄".repeat(40),
            "status".to_owned(),
        ],
        cursor: Some((2, 1)),
        map: crate::ui::render::frame::FrameMap::default(),
    }
}

struct T {
    s: Screen<SharedBuf>,
    buf: SharedBuf,
    query: String,
    typing: bool,
}

impl T {
    fn new(rows: &[String]) -> Self {
        let buf = SharedBuf::default();
        let mut s = Screen::new(buf.clone(), Size::new(W, H), false, false).unwrap();
        s.push(rows);
        s.draw_frame(&frame(), None).unwrap();
        Self {
            s,
            buf,
            query: String::new(),
            typing: false,
        }
    }

    fn op(&mut self, op: SearchOp) {
        match &op {
            SearchOp::Open => {
                self.typing = true;
                self.query.clear();
            }
            SearchOp::Query(q) => self.query.clone_from(q),
            SearchOp::Accept | SearchOp::Cancel => self.query.clear(),
            SearchOp::Step(_) => {}
        }
        self.s.view(ViewOp::Search(op)).unwrap();
        self.draw();
    }

    /// Types `q` and presses Enter: the walker applied.
    fn search(&mut self, q: &str) {
        self.op(SearchOp::Open);
        self.op(SearchOp::Query(q.to_owned()));
        self.typing = false;
        self.draw();
    }

    fn draw(&mut self) {
        let on = !self.query.is_empty() || self.typing;
        let prompt = on.then(|| Prompt {
            text: self.query.clone(),
            cursor: self.typing.then_some(self.query.len()),
        });
        self.s.draw_frame(&frame(), prompt.as_ref()).unwrap();
    }

    fn parser(&self) -> vt100::Parser {
        let mut p = vt100::Parser::new(H, W, 0);
        p.process(&self.buf.bytes());
        p
    }

    fn grid(&self) -> Vec<String> {
        let p = self.parser();
        p.screen()
            .rows(0, W)
            .map(|r| r.trim_end().to_owned())
            .collect()
    }

    /// The viewport's rows (the frame's four and the search line aside).
    fn view(&self) -> Vec<String> {
        self.grid()[..usize::from(H) - 5].to_vec()
    }

    /// Whether the cell at (row, col) is drawn reversed (a highlight).
    fn reversed(&self, row: u16, col: u16) -> bool {
        self.parser()
            .screen()
            .cell(row, col)
            .is_some_and(vt100::Cell::inverse)
    }
}

fn rows(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("line-{i:05}")).collect()
}

/// `rows(n)` with `word` written into rows `at`.
fn rows_with(n: usize, word: &str, at: &[usize]) -> Vec<String> {
    let mut r = rows(n);
    for &i in at {
        r[i] = format!("line-{i:05} {word}");
    }
    r
}

#[test]
fn a_query_rides_to_the_nearest_hit_above_the_view_centred() {
    let mut t = T::new(&rows_with(200, "needle", &[20, 120, 150]));
    t.search("needle");
    let v = t.view();
    assert!(
        v[4].starts_with("line-00150 needle"),
        "centred on the nearest hit above: {v:#?}"
    );
    assert!(
        t.grid()[7].contains("3/3"),
        "the third hit in document order: {:?}",
        t.grid()[7]
    );
}

#[test]
fn n_and_p_walk_the_hits_and_wrap_at_both_ends() {
    let mut t = T::new(&rows_with(200, "needle", &[20, 120, 150]));
    t.search("needle");
    t.op(SearchOp::Step(1)); // past the last: the first
    assert!(
        t.view()[4].starts_with("line-00020 needle"),
        "{:#?}",
        t.view()
    );
    assert!(t.grid()[7].contains("1/3"));
    t.op(SearchOp::Step(-1)); // before the first: the last
    assert!(t.view()[4].starts_with("line-00150 needle"));
    t.op(SearchOp::Step(-1));
    assert!(t.view()[4].starts_with("line-00120 needle"));
    assert!(t.grid()[7].contains("2/3"));
}

#[test]
fn a_hit_500_screens_back_and_esc_returns_exactly_where_the_view_was() {
    // 500 screens of 7 lines, one hit near the start; the view scrolled back a little first.
    let mut t = T::new(&rows_with(3600, "faraway", &[10]));
    t.s.view(ViewOp::HalfUp).unwrap();
    t.s.view(ViewOp::HalfUp).unwrap();
    t.draw();
    let before = t.view();
    t.search("faraway");
    assert!(
        t.view()[4].starts_with("line-00010 faraway"),
        "{:#?}",
        t.view()
    );
    t.op(SearchOp::Cancel);
    assert_eq!(t.view(), before, "Esc came back somewhere else");
    // From the tail, Esc comes back to the tail — following again.
    t.s.view(ViewOp::Bottom).unwrap();
    t.draw();
    let tail = t.view();
    t.search("faraway");
    t.op(SearchOp::Cancel);
    assert_eq!(t.view(), tail);
    assert!(!t.s.paused(), "following again");
}

#[test]
fn esc_while_typing_also_goes_back() {
    let mut t = T::new(&rows_with(300, "needle", &[5]));
    let before = t.view();
    t.op(SearchOp::Open);
    t.op(SearchOp::Query("need".to_owned()));
    assert!(
        t.view()[4].starts_with("line-00005"),
        "the live query rode to the hit"
    );
    t.op(SearchOp::Cancel);
    assert_eq!(t.view(), before);
}

#[test]
fn a_query_that_matches_nothing_says_so_and_leaves_the_view_alone() {
    let mut t = T::new(&rows_with(300, "needle", &[5]));
    t.s.view(ViewOp::HalfUp).unwrap();
    t.draw();
    let before = t.view();
    t.op(SearchOp::Open);
    t.op(SearchOp::Query("needle".to_owned()));
    t.op(SearchOp::Query("needlex".to_owned()));
    assert_eq!(
        t.view(),
        before,
        "a query narrowed to nothing puts the view back"
    );
    assert!(t.grid()[7].contains("no match"), "{:?}", t.grid()[7]);
    t.op(SearchOp::Step(1));
    assert_eq!(t.view(), before, "n with no hit moves nothing");
}

#[test]
fn a_jump_while_following_leaves_the_tail_and_enter_stays_there() {
    let mut t = T::new(&rows_with(100, "needle", &[40]));
    assert!(!t.s.paused());
    t.search("needle");
    assert!(t.s.paused(), "the jump anchored the view");
    assert!(t.view()[4].starts_with("line-00040 needle"));
    t.op(SearchOp::Accept);
    assert!(
        t.view()[4].starts_with("line-00040 needle"),
        "Enter left the view on the hit"
    );
    assert!(
        t.grid()[7].contains("Ctrl+End"),
        "…with the way back: {:?}",
        t.grid()[7]
    );
}

#[test]
fn output_during_a_search_is_searched_and_the_walker_keeps_its_hit() {
    let mut t = T::new(&rows_with(100, "needle", &[10, 50]));
    t.search("needle");
    t.op(SearchOp::Step(-1)); // on l10
    assert!(t.grid()[7].contains("1/2"));
    t.s.push(&rows_with(1, "needle", &[0])); // a new row with a hit
    t.draw();
    assert!(
        t.view()[4].starts_with("line-00010 needle"),
        "the view did not move"
    );
    assert!(
        t.grid()[7].contains("1/3"),
        "the new row's hit counts: {:?}",
        t.grid()[7]
    );
}

#[test]
fn a_new_query_from_the_walker_keeps_the_way_back() {
    let mut t = T::new(&rows_with(300, "alpha beta", &[5, 200]));
    let before = t.view();
    t.search("alpha");
    t.op(SearchOp::Step(1));
    t.op(SearchOp::Open); // `/` again
    t.op(SearchOp::Query("beta".to_owned()));
    assert!(t.view()[4].starts_with("line-00200"));
    t.op(SearchOp::Cancel);
    assert_eq!(t.view(), before, "Esc goes back to before the FIRST query");
}

#[test]
fn hits_are_highlighted_the_current_one_brighter_and_they_go_with_the_search() {
    let mut t = T::new(&rows_with(50, "needle", &[20, 21]));
    t.search("needle");
    // l21 is the nearest above: current; l20 the other.
    let cur = (0..8)
        .find(|&r| t.view()[r].starts_with("line-00021"))
        .unwrap();
    let other = (0..8)
        .find(|&r| t.view()[r].starts_with("line-00020"))
        .unwrap();
    let col = u16::try_from("line-00021 ".len()).unwrap();
    assert!(t.reversed(u16::try_from(cur).unwrap(), col));
    assert!(t.reversed(u16::try_from(other).unwrap(), col));
    assert!(
        !t.reversed(u16::try_from(cur).unwrap(), 0),
        "only the match"
    );
    let bytes = String::from_utf8_lossy(&t.buf.bytes()).into_owned();
    assert!(
        bytes.contains("\u{1b}[7;33m") || bytes.contains(SEARCH_CUR) || bytes.contains(SEARCH_HIT)
    );
    t.op(SearchOp::Accept);
    let cur = (0..8)
        .find(|&r| t.view()[r].starts_with("line-00021"))
        .unwrap();
    assert!(
        !t.reversed(u16::try_from(cur).unwrap(), col),
        "the highlight left with the search"
    );
}

#[test]
fn a_hit_that_straddles_a_wrap_is_found_lands_on_its_first_line_and_lights_both() {
    // 40 columns: the row wraps at 40, and "needle" starts at column 37 — `nee|dle`.
    let row = format!("{}needle and the rest", "x".repeat(37));
    let mut r = rows(100);
    r[30] = row;
    let mut t = T::new(&r);
    t.search("needle");
    let v = t.view();
    let first = (0..8)
        .find(|&i| v[i].ends_with("nee"))
        .expect("the row's first line");
    assert_eq!(v[first + 1], "dle and the rest", "{v:#?}");
    assert_eq!(first, 4, "centred on the line the match starts on: {v:#?}");
    let f = u16::try_from(first).unwrap();
    assert!(t.reversed(f, 37) && t.reversed(f, 39), "`nee` lit");
    assert!(t.reversed(f + 1, 0) && t.reversed(f + 1, 2), "`dle` lit");
    assert!(!t.reversed(f + 1, 4), "and nothing past it");
}

#[test]
fn a_row_wrapped_into_many_lines_is_counted_by_them_when_centring() {
    // A hit in the 4th line of a long wrapped row lands on that line, centred.
    let long = format!("{}target{}", "y".repeat(130), "z".repeat(30));
    let mut r = rows(60);
    r[25] = long;
    let mut t = T::new(&r);
    t.search("target");
    let v = t.view();
    assert!(
        v[4].contains("target") || (v[4].contains("targ") || v[5].starts_with("et")),
        "{v:#?}"
    );
    assert!(
        v[4].starts_with("yyyyyyyyyy"),
        "the 4th line of the row (130 y's = 3 lines + 10): {v:#?}"
    );
}

#[test]
fn the_search_line_shows_the_query_and_the_cursor_sits_on_it_while_typing() {
    let mut t = T::new(&rows(30));
    t.op(SearchOp::Open);
    t.op(SearchOp::Query("lin".to_owned()));
    let line = &t.grid()[7];
    assert!(line.starts_with("/lin"), "{line:?}");
    let p = t.parser();
    assert_eq!(
        p.screen().cursor_position(),
        (7, 4),
        "the cursor after the query"
    );
}

#[test]
fn rows_the_cap_drops_take_their_hits_with_them() {
    let buf = SharedBuf::default();
    let mut s = Screen::new(buf, Size::new(W, H), false, false).unwrap();
    let mut conv = crate::ui::runtime::screen::conversation::Conversation::with_cap(10);
    conv.push(&rows_with(10, "needle", &[1, 8]));
    let mut search = super::Search::new(&conv, 9);
    search.set_query(&conv, "needle");
    assert_eq!(search.status(), "2/2");
    conv.push(&rows(5)); // l1 goes
    search.catch_up(&conv);
    assert_eq!(search.status(), "1/1");
    drop(s.view(ViewOp::Bottom));
}

/// Search and the write-out stack: Ctrl+O with a search on writes the rows plain (no highlight), the
/// key after it comes back to the search as it was, and an exit with a search on writes plain rows too.
#[test]
fn a_write_out_during_a_search_writes_plain_rows_and_the_search_survives() {
    let mut t = T::new(&rows_with(50, "needle", &[20]));
    t.search("needle");
    let mark = t.buf.bytes().len();
    t.s.view(ViewOp::Dump).unwrap();
    let dumped = String::from_utf8_lossy(&t.buf.bytes()[mark..]).into_owned();
    assert!(
        dumped.contains("line-00020 needle\u{1b}[0m\r\n"),
        "the row, plain: {dumped:?}"
    );
    assert!(
        !dumped.contains(SEARCH_HIT) && !dumped.contains(SEARCH_CUR),
        "a highlight was written out"
    );
    t.s.view(ViewOp::Return).unwrap();
    t.draw();
    assert!(
        t.grid()[7].contains("1/1"),
        "the search is still on: {:?}",
        t.grid()[7]
    );
    t.s.push(&rows_with(1, "needle", &[0]));
    t.draw();
    let mark = t.buf.bytes().len();
    t.s.leave().unwrap();
    let out = String::from_utf8_lossy(&t.buf.bytes()[mark..]).into_owned();
    assert!(
        out.contains("line-00000 needle\u{1b}[0m\r\n") && !out.contains(SEARCH_HIT),
        "{out:?}"
    );
}
