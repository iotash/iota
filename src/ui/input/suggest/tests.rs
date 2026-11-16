#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The completion suite: `match_suggestions`' argument-entry laws, the cycle (Tab, ↑/↓ by item,
//! Esc, a click), and the list's layout — the two columns (the label column's width, cap and
//! stillness under scrolling), item heights, the description's measure, the two-line cap and its
//! ellipsis, the window keeping the highlight whole, the hues.
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so the tests live beside
//! them.

use crate::text::ansi::strip_sgr;
use crate::text::width::str_width;
use crate::ui::facade::Suggestion;
use crate::ui::input::composer::Composer;
use crate::ui::input::suggest::{
    DESC_LINES, DESC_MEASURE, LABEL_CAP, ListView, cancel, choose, frame_list, item_rows,
    label_column, layout, match_suggestions, step, tab_complete,
};
use crate::ui::render::theme::{CYAN, FAINT, RESET};

/// Builds a suggestion table from plain values (Go `cmdTable`).
fn cmd_table(vals: &[&str]) -> Vec<Suggestion> {
    vals.iter()
        .map(|v| Suggestion {
            value: (*v).to_owned(),
            ..Suggestion::default()
        })
        .collect()
}

fn sug(value: &str, desc: &str) -> Suggestion {
    Suggestion {
        value: value.to_owned(),
        label: String::new(),
        desc: desc.to_owned(),
    }
}

fn vals(ms: &[&Suggestion]) -> Vec<String> {
    ms.iter().map(|m| m.value.clone()).collect()
}

fn plain(rows: &[String]) -> Vec<String> {
    rows.iter().map(|r| strip_sgr(r)).collect()
}

/// Where the description starts on a plain row: the first text past the label's own column 3
/// that follows two spaces (a label holds no double space, a description is single-spaced).
/// `None` for a row with no description.
fn desc_at(row: &str) -> Option<usize> {
    let cs: Vec<char> = row.chars().collect();
    (5..cs.len()).find(|&i| cs[i] != ' ' && cs[i - 1] == ' ' && cs[i - 2] == ' ')
}

/// A description `words` words long.
fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("word{i:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Entries whose value carries an argument stay hidden until the line has a space;
/// whole-line prefix matching silences ordinary commands once arguments start.
// Go: model_test.go:2079
#[test]
fn test_match_suggestions_argument_entries() {
    let cmds = cmd_table(&[
        "/file",
        "/skills",
        "/skills brain-page",
        "/skills code-review",
    ]);

    let got = vals(&match_suggestions(&cmds, "/skill"));
    assert_eq!(got, vec!["/skills"], "before the space: {got:?}");
    let got = vals(&match_suggestions(&cmds, "/skills"));
    assert_eq!(got, vec!["/skills"], "on the bare command: {got:?}");
    let got = match_suggestions(&cmds, "/skills ");
    assert_eq!(got.len(), 2, "after the space: {:?}", vals(&got));
    let got = vals(&match_suggestions(&cmds, "/skills br"));
    assert_eq!(got, vec!["/skills brain-page"], "narrowing: {got:?}");
    let got = match_suggestions(&cmds, "/file some/path");
    assert!(
        got.is_empty(),
        "ordinary arguments suggested {:?}",
        vals(&got)
    );
}

/// A `"/"` prefix shows the list; Tab cycles the matches of the line typed before the FIRST
/// press, writing values into the composer; the cycle wraps around.
// Go: model_test.go:685
#[test]
fn test_slash_tab_completion() {
    let cmds = cmd_table(&["/file", "/session", "/status", "/model"]);
    let mut c = Composer::new();
    c.set_value("/s");
    let rows = plain(&frame_list(&mut c, &cmds, 80, 20).rows);
    assert_eq!(rows, vec!["   session", "   status"], "one row per item");
    tab_complete(&mut c, &cmds);
    assert_eq!(c.value(), "/session", "tab");
    tab_complete(&mut c, &cmds);
    assert_eq!(
        c.value(),
        "/status",
        "tab tab must cycle the ORIGINAL prefix"
    );
    tab_complete(&mut c, &cmds);
    assert_eq!(c.value(), "/session", "cycle wrap");
}

/// An item's height is its description's lines — one when it has none or one, two at most, the
/// second ending in `…` when the text ran on. The first line shares the label's row; the second
/// starts at the same column, never under the label. The description wraps at the measure, not at
/// the terminal: at 200 columns it is still at most `DESC_MEASURE` wide.
#[test]
fn item_heights_wrap_at_the_measure_and_cap_at_two_lines() {
    let bare = item_rows(&sug("/help", ""), false, 4, 80);
    assert_eq!(plain(&bare), vec!["   help"]);

    let short = item_rows(&sug("/help", "Show the commands"), false, 4, 80);
    assert_eq!(plain(&short), vec!["   help  Show the commands"]);

    // Twelve 6-column words (+ spaces) are 83 columns: two lines at the 72 measure, not one at 200.
    let two = plain(&item_rows(&sug("/x", &words(12)), false, 1, 200));
    assert_eq!(two.len(), 2, "{two:#?}");
    assert!(two[0].starts_with("   x  word00"), "{two:#?}");
    assert!(
        two[1].starts_with("      word"),
        "the second line starts in the description column: {two:#?}"
    );
    for row in &two {
        assert!(
            str_width(&row[7..]) <= DESC_MEASURE,
            "wider than the measure: {row:?}"
        );
    }
    assert!(
        !two[1].ends_with('…'),
        "fits in two lines, so no ellipsis: {two:#?}"
    );

    // Forty words are far past two lines: two shown, the second cut with `…`.
    let long = plain(&item_rows(&sug("/x", &words(40)), false, 1, 200));
    assert_eq!(long.len(), DESC_LINES, "{long:#?}");
    assert!(
        long[1].ends_with('…'),
        "the second line must end in …: {long:#?}"
    );
    assert!(str_width(long[1].trim_start()) <= DESC_MEASURE, "{long:#?}");

    // A narrow terminal narrows the measure: 40 columns leave 33 beside a 1-wide label column.
    let narrow = plain(&item_rows(&sug("/x", &words(8)), false, 1, 40));
    assert_eq!(narrow.len(), 2, "{narrow:#?}");
    assert!(narrow.iter().all(|r| str_width(r) <= 40), "{narrow:#?}");

    // The description's own newlines are not layout.
    let nl = plain(&item_rows(&sug("/x", "one\ntwo"), false, 1, 80));
    assert_eq!(nl, vec!["   x  one two"]);
}

/// The two columns at 80 and 160 columns: the label column is as wide as the longest label, the
/// description starts two past it on EVERY row of the list — label rows, second lines, items with
/// no description alike — and a label past [`LABEL_CAP`] is cut with `…` at the cap.
#[test]
fn the_list_is_two_columns_at_80_and_160() {
    let long = "a-label-that-runs-far-past-the-thirty-column-cap";
    let cmds = [
        sug("/review", &words(40)),
        sug("/go", "Short"),
        sug("/bare", ""),
        sug(&format!("/{long}"), &words(12)),
    ];
    let ms: Vec<&Suggestion> = cmds.iter().collect();
    for w in [80usize, 160] {
        assert_eq!(label_column(&ms, w), LABEL_CAP, "{w}: capped");
        let (view, _) = layout(&ms, Some(0), 0, w, 40);
        let rows = plain(&view.rows);
        let at = 3 + LABEL_CAP + 2;
        for (r, item) in rows.iter().zip(&view.items) {
            if *item == 2 {
                assert_eq!(r, "   bare", "{w}: no description, one row");
                continue;
            }
            assert_eq!(
                desc_at(r),
                Some(at),
                "{w}: the column moved on {r:?}\n{rows:#?}"
            );
            assert!(str_width(r) - at <= DESC_MEASURE.min(w - at), "{w}: {r:?}");
        }
        assert_eq!(view.items, vec![0, 0, 1, 2, 3, 3], "{w}: {rows:#?}");
        let cut: String = long.chars().take(LABEL_CAP - 1).collect();
        assert!(
            rows[4].starts_with(&format!("   {cut}…  word00")),
            "{w}: the long label is cut at the cap: {rows:#?}"
        );
        assert!(rows[1].ends_with('…'), "{w}: two lines, then …: {rows:#?}");
    }
    // Without the long label the column is the longest of the rest: `review`, 6 wide.
    let (view, _) = layout(&ms[..3], Some(0), 0, 80, 40);
    let rows = plain(&view.rows);
    assert_eq!(rows[2], format!("   go{}Short", " ".repeat(6 - 2 + 2)));
}

/// The label column is measured over ALL the matches, not the shown window: walking down a list
/// whose longest label scrolls out of sight, the description column never moves.
#[test]
fn the_description_column_holds_still_while_the_list_scrolls() {
    let mut cmds = vec![sug("/a-long-command-name", "first")];
    cmds.extend((0..10).map(|i| sug(&format!("/c{i}"), &words(3))));
    let at = 3 + "a-long-command-name".len() + 2;
    let mut c = Composer::new();
    c.set_value("/");
    let mut scrolled = false;
    for i in 0..cmds.len() {
        step(&mut c, &cmds, true);
        let view = frame_list(&mut c, &cmds, 80, 4);
        scrolled |= view.items[0] > 0;
        for r in plain(&view.rows) {
            assert_eq!(desc_at(&r), Some(at), "step {i}: the column moved on {r:?}");
        }
    }
    assert!(scrolled, "the window never scrolled");
}

/// ↑/↓ move the highlight by ITEM, whatever each item's height; the composer holds the
/// highlighted value, so Enter (a submit) commits it; ↓ from nothing lands on the first and ↑ on
/// the last; both wrap.
#[test]
fn the_highlight_moves_by_item_across_items_of_different_heights() {
    let cmds = vec![
        sug("/a", ""),
        sug("/b", &words(40)),
        sug("/c", "short"),
        sug("/d", &words(12)),
    ];
    let mut c = Composer::new();
    c.set_value("/");
    assert!(step(&mut c, &cmds, true));
    assert_eq!((c.value(), c.suggestion_index), ("/a", Some(0)));
    for want in ["/b", "/c", "/d", "/a"] {
        assert!(step(&mut c, &cmds, true));
        assert_eq!(c.value(), want);
    }
    assert!(step(&mut c, &cmds, false));
    assert_eq!(c.value(), "/d", "↑ from the first wraps to the last");
    assert_eq!(
        c.suggestion_base, "/",
        "the list still filters by what was typed"
    );

    // The highlighted item's label row carries the pointer, whatever its height.
    let rows = plain(&frame_list(&mut c, &cmds, 80, 40).rows);
    assert!(
        rows.iter().any(|r| r.starts_with(" › d  word00")),
        "{rows:#?}"
    );

    // From no highlight ↑ lands on the last item.
    let mut c = Composer::new();
    c.set_value("/");
    assert!(step(&mut c, &cmds, false));
    assert_eq!(c.value(), "/d");

    // No list, no step: the key goes on to the history.
    let mut c = Composer::new();
    c.set_value("hello");
    assert!(!step(&mut c, &cmds, true));
}

/// Esc during the cycle puts the typed line back and drops the highlight; with no cycle it is not
/// the list's.
#[test]
fn esc_puts_the_typed_line_back() {
    let cmds = cmd_table(&["/model", "/mode"]);
    let mut c = Composer::new();
    c.set_value("/mo");
    assert!(!cancel(&mut c), "nothing to cancel yet");
    step(&mut c, &cmds, true);
    assert_eq!(c.value(), "/model");
    assert!(cancel(&mut c));
    assert_eq!(c.value(), "/mo");
    assert!(c.suggestion_base.is_empty() && c.suggestion_index.is_none());
}

/// The window keeps the highlighted item WHOLE: walking down a list taller than the room, every
/// row of the highlighted item is shown at every step, the window never exceeds the room, and it
/// scrolls only as far as it must (stepping back up inside the window does not move it).
#[test]
fn the_window_keeps_the_highlight_fully_visible() {
    let cmds: Vec<Suggestion> = (0..12)
        .map(|i| {
            sug(
                &format!("/c{i:02}"),
                &words(if i % 2 == 0 { 40 } else { 3 }),
            )
        })
        .collect();
    let room = 8;
    let mut c = Composer::new();
    c.set_value("/c");
    for i in 0..cmds.len() {
        step(&mut c, &cmds, true);
        let ListView { rows, items } = frame_list(&mut c, &cmds, 80, room);
        assert!(
            rows.len() <= room,
            "step {i}: {} rows in {room}",
            rows.len()
        );
        let want = item_rows(&cmds[i], true, 3, 80).len();
        let got = items.iter().filter(|&&it| it == i).count();
        assert_eq!(
            got, want,
            "step {i}: the highlight shows {got} of its {want} rows"
        );
        assert!(
            plain(&rows)
                .iter()
                .any(|r| r.starts_with(&format!(" › c{i:02}  "))),
            "step {i}"
        );
    }
    // At the bottom; one step up stays inside the window, which must not move.
    let before = frame_list(&mut c, &cmds, 80, room).items;
    step(&mut c, &cmds, false);
    let after = frame_list(&mut c, &cmds, 80, room).items;
    assert_eq!(before, after, "an in-window step scrolled the window");
}

/// A room shorter than the highlighted item still shows the item, alone and whole — the list
/// shrinks to it rather than disappearing.
#[test]
fn no_room_shrinks_the_list_to_the_highlighted_item() {
    let cmds = [
        sug("/a", "first"),
        sug("/b", &words(40)),
        sug("/c", "third"),
    ];
    let ms: Vec<&Suggestion> = cmds.iter().collect();
    let (view, _) = layout(&ms, Some(1), 0, 80, 0);
    assert_eq!(view.items, vec![1, 1], "{:#?}", plain(&view.rows));
    let (view, _) = layout(&ms, None, 0, 80, 0);
    assert_eq!(view.items, vec![0], "with no highlight, the first item");
}

/// A click maps a row to its item over variable heights: every row of an item — its label row and
/// its second description line — belongs to that item, and choosing it highlights it.
#[test]
fn a_click_maps_any_row_of_an_item_to_it() {
    let cmds = vec![sug("/a", ""), sug("/b", &words(40)), sug("/c", "short")];
    let mut c = Composer::new();
    c.set_value("/");
    let view = frame_list(&mut c, &cmds, 80, 20);
    assert_eq!(view.items, vec![0, 1, 1, 2]);
    // The second description row of /b.
    assert!(choose(&mut c, &cmds, view.items[2]));
    assert_eq!((c.value(), c.suggestion_index), ("/b", Some(1)));
}

/// The per-skill rows (`"/skills <name>"`, Label = the bare name) behave in the list exactly like
/// Go's row: `"/skills"` offers ONE candidate — the command, not its catalog — and only after the
/// space do the skills appear, labelled bare, each with ITS OWN description. The table is built
/// beside `repl::commands`; the rows are spelled out here so `ui` names nothing above it.
// Go: chat/skillcmd_test.go:97 TestSkillCommandsCompletion (the completion half)
#[test]
fn test_skill_rows_hide_until_a_space_then_show_bare_names() {
    let cmds = vec![
        sug("/skills", "List skills, or run one: /skills <name>"),
        Suggestion {
            value: "/skills brain-page".to_owned(),
            label: "brain-page".to_owned(),
            desc: "Read and write brain pages".to_owned(),
        },
        Suggestion {
            value: "/skills code-review".to_owned(),
            label: "code-review".to_owned(),
            desc: String::new(),
        },
    ];
    let mut c = Composer::new();
    c.set_value("/skills");
    let rows = plain(&frame_list(&mut c, &cmds, 80, 20).rows);
    assert_eq!(
        rows,
        vec!["   skills  List skills, or run one: /skills <name>"],
        "the catalog leaked before the space"
    );

    let mut c = Composer::new();
    c.set_value("/skills ");
    let rows = plain(&frame_list(&mut c, &cmds, 80, 20).rows);
    assert_eq!(
        rows,
        vec![
            "   brain-page   Read and write brain pages",
            "   code-review"
        ],
        "bare names, each with its own description"
    );
    // Tab writes the WHOLE value back, so Enter sends `/skills brain-page`.
    tab_complete(&mut c, &cmds);
    assert_eq!(c.value(), "/skills brain-page");
}

/// The list is chrome under the composer: unselected labels and every description faint, the
/// highlighted label (and its pointer) cyan, the description column faint beside it; no leading
/// slash is repeated.
// Go: model_test.go:2177
#[test]
fn test_suggest_row_hues() {
    let cmds = vec![
        sug("/status", "Provider, tokens, tools"),
        sug("/session", "Resume or delete"),
    ];
    let mut c = Composer::new();
    c.set_value("/s");
    tab_complete(&mut c, &cmds);
    let rows = frame_list(&mut c, &cmds, 80, 20).rows;
    assert_eq!(
        rows[0],
        format!(" {CYAN}› status{RESET}   {FAINT}Provider, tokens, tools{RESET}")
    );
    assert_eq!(
        rows[1],
        format!("   {FAINT}session{RESET}  {FAINT}Resume or delete{RESET}")
    );
    assert!(!plain(&rows).iter().any(|r| r.contains("/status")));
}
