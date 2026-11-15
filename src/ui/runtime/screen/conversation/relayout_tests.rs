#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A width change re-lays-out what the buffer holds FROM ITS SOURCE (tui-plan S7a): a reply goes
//! through the real markdown renderer into the buffer as rows and laid-out blocks, and the same
//! buffer is then drawn at other widths — narrower and wider. The owner's five elements: the
//! table, the list, the code block, the quote (the banner's own test is in `repl::render::banner`),
//! plus the cap dropping a laid-out block whole.

use std::sync::{Arc, Mutex};

use super::{Conversation, TRIMMED};
use crate::markdown::{CodeTheme, Fragment, PreviewHandle, RenderOptions, Sink, Writer};
use crate::text::ansi::{ansi_width, strip_sgr};
use crate::ui::block::Block;

/// The markdown sink the terminal's is in shape: text as rows, each closed block as its source.
struct Blocks(Arc<Mutex<Vec<Block>>>, String);

impl Sink for Blocks {
    fn write(&mut self, rendered: &str) {
        self.1.push_str(rendered);
        while let Some(i) = self.1.find('\n') {
            let row = self.1[..i].to_owned();
            self.1.drain(..=i);
            self.0.lock().unwrap().push(Block::Row(row));
        }
    }

    fn write_block(&mut self, block: Fragment) {
        self.0.lock().unwrap().push(Block::laid(block));
    }

    fn width(&self) -> usize {
        80
    }

    fn block_preview(&mut self, _label: &str) -> Option<Box<dyn PreviewHandle>> {
        None
    }
}

/// `md` rendered (no color, so the assertions read the layout) into a buffer.
fn reply(md: &str) -> Conversation {
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut w = Writer::new(
        Box::new(Blocks(Arc::clone(&out), String::new())),
        RenderOptions {
            color: false,
            code_theme: CodeTheme::Monokai,
        },
    );
    w.write(md.as_bytes());
    w.flush();
    drop(w);
    let mut c = Conversation::new();
    c.push(std::mem::take(&mut *out.lock().unwrap()));
    c
}

/// Everything the buffer shows at `width`, top to bottom, SGR stripped.
fn shown(c: &mut Conversation, width: usize) -> Vec<String> {
    c.view_lines(width, 500, None)
        .iter()
        .map(|l| strip_sgr(l))
        .collect()
}

const TABLE: &str = "| name | what it does |\n|---|---|\n| region | stages the rows a preview replaces, in the frame above the composer |\n| conversation | holds every committed block and lays each one out at the screen's width |\n";

#[test]
fn a_table_is_laid_out_again_at_each_width_from_its_cells() {
    let mut c = reply(TABLE);
    for width in [80, 40, 160, 60] {
        let lines = shown(&mut c, width);
        // The width−1 law: a table never reaches the exact width.
        assert!(
            lines.iter().all(|l| ansi_width(l) < width),
            "{width}: a row reaches the width\n{}",
            lines.join("\n")
        );
        // The box holds on every row: re-wrapped rows of another width would start mid-cell.
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with(['┌', '│', '├', '└']) && l.ends_with(['┐', '│', '┤', '┘'])),
            "{width}: the box broke\n{}",
            lines.join("\n")
        );
        // Every row of the box is the same width: the columns were computed at this width.
        let w0 = ansi_width(&lines[0]);
        assert!(lines.iter().all(|l| ansi_width(l) == w0), "{width}: ragged");
    }
    // Widening re-flows: at 160 the long cells fit on one row each (header, two rows, three
    // rules and the two borders); at 40 they wrap inside their column.
    assert_eq!(shown(&mut c, 160).len(), 7, "{:#?}", shown(&mut c, 160));
    assert!(shown(&mut c, 40).len() > 7);
}

#[test]
fn a_list_keeps_its_hanging_indent_narrow_and_realigns_wide() {
    let md = "intro\n\n1. a first item whose text is long enough to wrap at sixty columns and keep going for a while\n2. second\n   - a nested bullet that is also long enough to wrap when the screen is sixty columns wide\n10. tenth\n";
    let mut c = reply(md);
    let narrow = shown(&mut c, 60);
    let joined = narrow.join("\n");
    assert!(narrow.iter().all(|l| ansi_width(l) <= 60), "{joined}");
    // The ordered markers right-align ( 1. / 10.) and the text after them starts at column 4;
    // the rows a long item wraps onto start at that same column.
    let first = narrow.iter().position(|l| l.starts_with(" 1. ")).unwrap();
    assert!(
        narrow[first + 1].starts_with("    ") && !narrow[first + 1].starts_with("     "),
        "the wrap lost the hanging indent:\n{joined}"
    );
    // The nested bullet hangs under its parent's text, and its own wrap under ITS text.
    let nested = narrow.iter().position(|l| l.starts_with("    • ")).unwrap();
    assert!(
        narrow[nested + 1].starts_with("      ") && !narrow[nested + 1].starts_with("       "),
        "the nested wrap lost its indent:\n{joined}"
    );
    // At 160 every item is one row again: the text realigned, nothing left wrapped.
    let wide = shown(&mut c, 160);
    assert_eq!(
        wide,
        [
            "intro",
            "",
            " 1. a first item whose text is long enough to wrap at sixty columns and keep going for a while",
            " 2. second",
            "    • a nested bullet that is also long enough to wrap when the screen is sixty columns wide",
            "10. tenth",
        ]
    );
}

#[test]
fn a_code_block_keeps_its_gutter_across_a_width_change() {
    let long = format!("let s = \"{}\";", "x".repeat(70));
    let mut c = reply(&format!("```\nfn main() {{\n    {long}\n}}\n```\n"));
    let wide = shown(&mut c, 120);
    assert_eq!(
        wide,
        ["  fn main() {", &format!("      {long}"), "  }"],
        "at 120 the long line is one row"
    );
    let narrow = shown(&mut c, 40);
    assert!(narrow.len() > 3, "at 40 the long line wraps: {narrow:#?}");
    assert!(
        narrow
            .iter()
            .all(|l| l.starts_with("  ") && ansi_width(l) <= 40),
        "a wrapped code row lost the gutter:\n{}",
        narrow.join("\n")
    );
    assert_eq!(shown(&mut c, 120), wide, "and widening re-flows it back");
}

#[test]
fn a_quote_keeps_its_bar_column_at_every_width() {
    let md = "> a quoted paragraph that is long enough to wrap at fifty columns, so the bar must be drawn on every row it wraps onto\n>\n> - and a list inside it\n";
    let mut c = reply(md);
    for width in [50, 120, 70] {
        let lines = shown(&mut c, width);
        assert!(
            lines.iter().all(|l| l.starts_with("│ ")),
            "{width}: a row without the bar\n{}",
            lines.join("\n")
        );
        // The rows pad to the block's width (the bar, its padding and the text area wrapped at
        // width−2): the bar's column and the block's right edge hold.
        assert!(
            lines.iter().all(|l| ansi_width(l) == width),
            "{width}: ragged\n{}",
            lines.join("\n")
        );
    }
    assert!(shown(&mut c, 50).len() > shown(&mut c, 120).len());
}

#[test]
fn the_cap_drops_a_laid_out_block_whole() {
    let mut c = Conversation::with_cap(4);
    let table = reply(TABLE).rows.pop_front().unwrap().block;
    c.push(vec![Block::from("before"), table, Block::from("after")]);
    assert!(
        shown(&mut c, 80).iter().any(|l| l.starts_with('┌')),
        "the table is held"
    );
    c.push(Block::rows(["one", "two"]));
    let lines = shown(&mut c, 80);
    assert!(lines[0].contains(TRIMMED), "{lines:#?}");
    assert_eq!(
        lines[1..],
        ["after", "one", "two"],
        "the table went whole: no orphan row of it"
    );
}
