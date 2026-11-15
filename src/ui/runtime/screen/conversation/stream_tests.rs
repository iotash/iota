#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A block still being written GROWS in place (tui-plan S7b): a reply streams through the real
//! markdown renderer a line at a time, every commit is pushed into the buffer as it comes, and the
//! buffer shows the open block line by line — one block, replaced, never a second one appended.

use super::{Conversation, Highlight};
use crate::text::ansi::{ansi_width, strip_sgr};
use crate::ui::runtime::screen::search::Search;
use crate::ui::testutil::Streamed;

/// Everything the buffer shows at `width`, top to bottom, SGR stripped.
fn shown(c: &mut Conversation, width: usize) -> Vec<String> {
    c.view_lines(width, 500, None)
        .iter()
        .map(|l| strip_sgr(l))
        .collect()
}

/// `n` lines of a code block's source, numbered.
fn code_line(i: usize) -> String {
    format!("let line_{i} = compute({i}); // the {i}th line of a long streaming code block")
}

#[test]
fn a_code_block_streams_line_by_line_as_one_block() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("Here is the code:\n```rust\n"));
    let blocks = c.pushed();
    for i in 0..40 {
        c.push(s.feed(&format!("{}\n", code_line(i))));
        let lines = shown(&mut c, 120);
        // The text, its separator, then exactly the lines that have arrived — each once.
        assert_eq!(
            lines.len(),
            2 + i + 1,
            "after line {i}:\n{}",
            lines.join("\n")
        );
        assert_eq!(lines[2 + i], format!("  {}", code_line(i)));
        assert!(
            lines
                .iter()
                .all(|l| !l.contains("rendering") && !l.contains(" lines")),
            "a counter instead of the code:\n{}",
            lines.join("\n")
        );
        assert_eq!(c.pushed(), blocks, "line {i} appended a block");
    }
    c.push(s.feed("```\n"));
    c.push(s.end());
    assert_eq!(c.pushed(), blocks, "the close appended a block");
    assert_eq!(shown(&mut c, 120).len(), 2 + 40);
}

#[test]
fn a_table_streams_its_rows_as_they_arrive() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("| name | value |\n|---|---|\n"));
    let blocks = c.pushed();
    for i in 1..=6 {
        c.push(s.feed(&format!(
            "| row {i} | a cell that grows the column a little more {} |\n",
            "x".repeat(i)
        )));
        let lines = shown(&mut c, 80);
        // Top border, header, then a rule and a row per data row, bottom border.
        assert_eq!(
            lines.len(),
            3 + 2 * i,
            "after row {i}:\n{}",
            lines.join("\n")
        );
        assert!(
            lines.iter().any(|l| l.contains(&format!("row {i}"))),
            "row {i} not shown:\n{}",
            lines.join("\n")
        );
        // The box is whole at every step: its columns are re-filled from all the rows so far.
        let w0 = ansi_width(&lines[0]);
        assert!(
            lines.iter().all(|l| ansi_width(l) == w0),
            "ragged at row {i}:\n{}",
            lines.join("\n")
        );
        assert!(lines[0].starts_with('┌') && lines.last().unwrap().starts_with('└'));
        assert_eq!(c.pushed(), blocks);
    }
}

#[test]
fn a_list_and_a_quote_stream_an_item_and_a_line_at_a_time() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    for i in 1..=4 {
        c.push(s.feed(&format!("- item {i}\n")));
        assert_eq!(shown(&mut c, 80).len(), i);
        assert_eq!(c.pushed(), 1);
    }
    c.push(s.feed("\nthen\n\n"));
    for i in 1..=3 {
        c.push(s.feed(&format!("> quoted {i}\n")));
        let lines = shown(&mut c, 80);
        assert!(lines.last().unwrap().starts_with("│ quoted"), "{lines:#?}");
        assert_eq!(lines.iter().filter(|l| l.contains("quoted")).count(), i);
    }
}

#[test]
fn a_resize_mid_stream_lays_out_again_what_has_arrived() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("```\n"));
    for i in 0..5 {
        c.push(s.feed(&format!("{}\n", code_line(i))));
    }
    assert_eq!(shown(&mut c, 120).len(), 5, "one row a line at 120");
    // Narrower: the five lines wrap under the gutter, none from column 0.
    let narrow = shown(&mut c, 40);
    assert!(narrow.len() > 5);
    assert!(
        narrow
            .iter()
            .all(|l| l.starts_with("  ") && ansi_width(l) <= 40),
        "{narrow:#?}"
    );
    // The stream goes on at the new width, and widening again puts every line back on one row.
    for i in 5..8 {
        c.push(s.feed(&format!("{}\n", code_line(i))));
        assert!(shown(&mut c, 40).iter().all(|l| l.starts_with("  ")));
    }
    let wide = shown(&mut c, 120);
    assert_eq!(wide.len(), 8, "{wide:#?}");
    assert_eq!(wide[7], format!("  {}", code_line(7)));
}

#[test]
fn a_block_replaced_while_it_is_the_anchor_keeps_the_view_on_it() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("```\n"));
    for i in 0..30 {
        c.push(s.feed(&format!("{}\n", code_line(i))));
    }
    // Scrolled back into the open block: the viewport's top is its line 10.
    c.go_top(120, 10);
    c.scroll_down(10, 120, 10);
    let mark = c.mark().expect("anchored");
    assert_eq!(mark.1, 10);
    let top = c.view_lines(120, 10, None)[0].clone();
    for i in 30..40 {
        c.push(s.feed(&format!("{}\n", code_line(i))));
    }
    assert_eq!(c.mark(), Some(mark), "the anchor moved off its block");
    assert_eq!(
        c.view_lines(120, 10, None)[0],
        top,
        "the text under the view moved"
    );
    assert!(!c.following());
}

#[test]
fn a_search_finds_what_a_growing_block_takes_and_a_grown_block_counts_once() {
    let mut s = Streamed::new();
    let mut c = Conversation::with_cap(3);
    c.push(s.feed("```\nfirst\n"));
    let mut search = Search::new(&c, 0);
    search.set_query(&mut c, "needle", 80);
    assert_eq!(search.current(), None);
    // The block grows a line holding the query: its search text is recomputed, and found.
    for l in ["second", "a needle here", "third"] {
        c.push(s.feed(&format!("{l}\n")));
        search.catch_up(&mut c, 80);
    }
    let hit = search.current().expect("the grown line is found");
    let hl = Highlight {
        query: "needle",
        current: Some(hit),
    };
    let lit = c.view_lines(80, 10, Some(&hl));
    assert!(lit.iter().any(|l| l.contains("\x1b[7;33m")), "{lit:#?}");
    // Many replacements and still one block: the cap of three never trims it.
    for i in 0..50 {
        c.push(s.feed(&format!("{i}\n")));
    }
    assert_eq!(c.pushed(), 1);
    assert!(
        shown(&mut c, 80)[0].contains("first"),
        "the cap dropped the growing block"
    );
}

#[test]
fn a_write_out_stops_above_an_open_block_and_writes_it_once_closed() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("before\n\n```\none\n"));
    let (lines, next) = c.lines_after(0, 80, false);
    assert_eq!(lines, ["before", ""], "the open block was written");
    c.push(s.feed("two\n```\n"));
    c.push(s.feed("after\n"));
    let (lines, _) = c.lines_after(next, 80, false);
    assert_eq!(lines, ["  one", "  two", "", "after"]);
    // The exit writes an open block as it stands: nothing more will come.
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("```\nhalf\n"));
    assert_eq!(c.lines_after(0, 80, true).0, ["  half"]);
}

#[test]
fn a_growing_block_at_the_tail_scrolls_by_what_it_grew() {
    let mut s = Streamed::new();
    let mut c = Conversation::new();
    c.push(s.feed("```\na\n"));
    c.view_lines(80, 10, None);
    c.take_fresh(80, 10);
    c.push(s.feed("b\nc\n"));
    assert_eq!(c.take_fresh(80, 10), 2, "two lines grew");
    c.push(s.feed("d\n"));
    // Not drawn since: what it grew is measured against the height it was last drawn at.
    assert_eq!(c.take_fresh(80, 10), 1);
}
