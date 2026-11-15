//! Pure streaming markdown→ANSI renderer (internal/markdown; `TUI_CONTRACTS` §3): zero terminal deps, zero
//! async, no regex (hand parsers). The width ruler and its escape-aware companion it shares with `ui` and
//! `repl` live in `crate::markdown::text::{width, ansi}`.
//!
//! The streaming renderer (`TUI_CONTRACTS` §3.4; markdown.go Writer). ONE `Writer` per
//! content block. State: byte line buffer; the open buffering block as one `Block` value
//! (fence/table/list/quote/math, at most one at a time); `gap_paid`; `last_unit` spacing state.
//!
//! LINE FRAMING: `write` appends bytes and extracts complete lines at each `\n` (a
//! UTF-8 sequence never contains `0x0A`, so byte-splitting is safe even when chunks
//! split multi-byte chars); each line goes through `consume_line` — the single home of
//! every "does this line belong to the open block" decision. `flush` feeds the final
//! partial line through the SAME dispatch (the unified-dispatch law: a hand-maintained
//! copy drifted for years — tables dropped last rows, headings/math/list markers in a
//! final partial line printed raw).
//!
//! SPACING STATE MACHINE (markdown.go:1092-1165): units None/Blank/Text/Block;
//! `emit_blank` collapses blank runs and drops leading blanks; `emit_text` separates
//! only after a block; `begin_block`/`end_block` bound every block-level element with
//! exactly ONE blank line; `open_block` pays the separating blank BEFORE the block first
//! shows (`gap_paid` credit) so the streaming layout equals the settled layout — the
//! open-block-pays-separator law, for all five buffering block types.
//!
//! GROWTH (tui-plan S7b): a sink that shows blocks while they are written
//! ([`Sink::shows_open_blocks`]) gets the open block as it stands after every line it takes
//! ([`Sink::grow_block`]) and once more when it closes ([`Sink::write_block`]); the terminal
//! replaces the one block in place each time, so a code block, a table, a list, a quote or a
//! formula appears line by line at the width of the screen.

pub(crate) mod blocks;
pub(crate) mod fragment;
pub mod highlight;
pub(crate) mod html;
pub(crate) mod inline;
pub(crate) mod link;
pub(crate) mod sink;
pub(crate) mod style;

pub use fragment::Fragment;
pub use highlight::{CodeHighlighter, PlainIndent, SyntectHighlighter};
pub use link::hyperlink;
pub use sink::Sink;
pub use style::Style;

use crate::markdown::blocks::code::CodeBlock;
use crate::markdown::blocks::list::ListBlock;
use crate::markdown::blocks::math::{MathBlock, display_open, is_display_close};
use crate::markdown::blocks::quote::{QuoteBlock, is_quote_line};
use crate::markdown::blocks::table::{TableBlock, is_table_line};
use crate::markdown::inline::{highlight_line, is_block_line, is_list_line};

/// Code-highlight theme, chosen by the host's background detect.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CodeTheme {
    /// Dark-background theme.
    Monokai,
    /// Light-background theme.
    Github,
}

/// Per-Writer render options (a fresh Writer per content block is exactly when Go
/// re-read its globals). `color` is the ONE switch gating every escape byte in the
/// crate — SGR and OSC alike — while layout stays unchanged.
#[derive(Clone, Copy)]
pub struct RenderOptions {
    /// Whether SGR styling is emitted at all.
    pub color: bool,
    /// The code theme of the syntect highlighter.
    pub code_theme: CodeTheme,
}

/// Spacing-state classification of the last emitted unit (Go mdUnit).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// Nothing emitted yet (suppresses leading blanks).
    None,
    /// The last emitted unit was a blank line.
    Blank,
    /// The last emitted unit was a paragraph line.
    Text,
    /// The last emitted unit was a rendered block element.
    Block,
}

/// The buffering block the writer is inside — markdown.go's five `in*` flags and their
/// buffers as ONE value. At most one block is open at a time (a fence, a table, a list, a
/// quote or a display-math block) and `None` is the plain paragraph path. Each variant owns
/// what it has taken so far and gives it as a [`Fragment`] at any point.
enum Block {
    /// No block open: blanks, headings, rules and paragraph lines dispatch directly.
    None,
    /// Inside a fenced code block (boxed: it carries its highlighter's parse state).
    Code(Box<CodeBlock>),
    /// Buffering table rows.
    Table(TableBlock),
    /// Buffering list items.
    List(ListBlock),
    /// Buffering quote lines.
    Quote(QuoteBlock),
    /// Inside a `$$` / `\[` display-math block.
    Math(MathBlock),
}

/// Streaming markdown renderer; ONE per content block.
pub struct Writer {
    sink: Box<dyn Sink>,
    opts: RenderOptions,
    buf: Vec<u8>,
    /// The open buffering block, if any.
    block: Block,
    /// The buffering block's separating blank was already written when it opened, so
    /// `begin_block` must not write it again.
    gap_paid: bool,
    last_unit: Unit,
}

impl Writer {
    /// A renderer writing to `sink` with `opts`. `last_unit` starts `None` so leading
    /// blanks are suppressed and no separator precedes the first unit.
    pub fn new(sink: Box<dyn Sink>, opts: RenderOptions) -> Self {
        Self {
            sink,
            opts,
            buf: Vec::new(),
            block: Block::None,
            gap_paid: false,
            last_unit: Unit::None,
        }
    }

    /// Feeds a chunk of markdown bytes; complete lines dispatch immediately.
    pub fn write(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(idx) = self.buf.iter().position(|&b| b == b'\n') {
            let taken: Vec<u8> = self.buf.drain(..=idx).collect();
            let line = String::from_utf8_lossy(&taken[..taken.len() - 1]).into_owned();
            self.consume_line(&line);
        }
    }

    /// Final partial line through the SAME dispatch, then the open block closes
    /// (markdown.go:379-399).
    pub fn flush(&mut self) {
        if !self.buf.is_empty() {
            let taken = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&taken).into_owned();
            self.consume_line(&line);
        }
        self.flush_block();
    }

    /// consumeLine twin (markdown.go:219-369) — THE dispatch ordering law.
    fn consume_line(&mut self, line: &str) {
        // Fenced code block: buffer until the closing fence, then render once. A fence
        // line always wins: it closes an open fence, and otherwise flushes a pending
        // table/list/quote first — but NOT an open display-math block, which it
        // interrupts and which resumes after the closing fence (Go's dispatch order).
        if line.trim().starts_with("```") {
            if matches!(self.block, Block::Code(_)) {
                self.flush_block(); // the closing fence
            } else {
                let interrupted = match std::mem::replace(&mut self.block, Block::None) {
                    Block::Math(math) => {
                        self.sink.withdraw_block();
                        Some(math)
                    }
                    other => {
                        self.block = other;
                        self.flush_block();
                        None
                    }
                };
                self.open_code(line, interrupted);
            }
            return;
        }
        if let Block::Code(code) = &mut self.block {
            code.push(line);
            self.grow();
            return;
        }

        // A buffering list block consumes marker lines, indented continuations, and
        // one held blank; anything else flushes the block (the held blank re-emits)
        // and falls through.
        if let Block::List(list) = &mut self.block {
            if list.consume(line) {
                self.grow();
                return;
            }
            self.flush_block();
        }

        // A buffering quote block consumes consecutive quote lines; the first
        // non-quote line flushes it and falls through.
        if let Block::Quote(quote) = &mut self.block {
            if is_quote_line(line) {
                quote.append(line);
                self.grow();
                return;
            }
            self.flush_block();
        }

        // A buffering display-math block consumes lines until its closing fence.
        if let Block::Math(math) = &mut self.block {
            if !is_display_close(line) {
                math.append(line);
                self.grow();
                return;
            }
            self.flush_block();
            return;
        }

        // Open a display-math block: a bare "$$"/"\[" fence starts a buffered block,
        // a complete one-line "$$…$$"/"\[…\]" renders at once. Either form first
        // flushes a pending table/list (the quote path already flushed above).
        if let Some((body, one_line)) = display_open(line) {
            self.flush_block();
            if one_line {
                let block = MathBlock::one_line(body).fragment(self.opts);
                self.render_block(block);
            } else {
                self.open_block(Block::Math(MathBlock::open()));
            }
            return;
        }

        // Only a table can still be open here; its FIRST row opens it.
        if is_table_line(line) {
            if let Block::Table(table) = &mut self.block {
                table.push(line);
                self.grow();
            } else {
                self.open_block(Block::Table(TableBlock::open(line)));
            }
            return;
        }

        self.flush_block(); // a pending table

        if is_list_line(line) {
            self.open_block(Block::List(ListBlock::open(line)));
            return;
        }

        if is_quote_line(line) {
            self.open_block(Block::Quote(QuoteBlock::open(line)));
            return;
        }

        // Plain path: a blank collapses; a heading or horizontal rule is a block-level
        // element bounded by one blank above and below; anything else is a paragraph
        // line that stays adjacent to its neighbours.
        if line.trim().is_empty() {
            self.emit_blank();
        } else if is_block_line(line) {
            self.begin_block();
            let styled = highlight_line(line, self.opts.color);
            self.sink.write(&format!("{styled}\n"));
            self.end_block();
        } else {
            let styled = highlight_line(line, self.opts.color);
            self.emit_text(&styled);
        }
    }

    /// Opens a fenced code block at `fence` (its language tag is what follows the
    /// backticks), carrying the display-math block the fence interrupted, if any.
    fn open_code(&mut self, fence: &str, interrupted: Option<MathBlock>) {
        let lang = CodeBlock::lang_of(fence);
        self.open_block(Block::Code(Box::new(CodeBlock::new(
            &lang,
            self.opts,
            interrupted,
        ))));
    }

    /// The open block as it stands, whichever it is (`None`: no block open, or one with nothing
    /// to show yet).
    fn fragment(&self) -> Option<Fragment> {
        let opts = self.opts;
        match &self.block {
            Block::None => None,
            Block::Code(code) => Some(code.fragment(opts)),
            Block::Table(table) => table.fragment(opts),
            Block::List(list) => list.fragment(opts),
            Block::Quote(quote) => quote.fragment(opts),
            Block::Math(math) => math.fragment(opts),
        }
    }

    /// Hands the open block as it stands to a sink that shows blocks while they are written.
    fn grow(&mut self) {
        if !self.sink.shows_open_blocks() {
            return;
        }
        if let Some(block) = self.fragment() {
            self.sink.grow_block(block);
        }
    }

    /// Closes and renders the open block, whichever it is. A list re-emits its held blank
    /// after the render (the blank turned out to END the list, not to make it loose) —
    /// routed through `emit_blank` so it joins the blank-run collapse. A fence leaves the
    /// display-math block it interrupted open again (`CodeBlock::interrupted`), opened like a new
    /// block: it pays its gap below the code and shows again from there.
    fn flush_block(&mut self) {
        let block = self.fragment();
        let (resumed, held_blank) = match &mut self.block {
            Block::None => return,
            Block::Code(code) => (code.take_interrupted(), false),
            Block::List(list) => (None, list.holds_blank()),
            _ => (None, false),
        };
        self.block = Block::None;
        self.render_block(block);
        if held_blank {
            self.emit_blank();
        }
        if let Some(math) = resumed {
            self.open_block(Block::Math(math));
        }
    }

    /// Writes a closed block between `begin_block` and `end_block`, as its source
    /// ([`Sink::write_block`]); `None` (an empty table, quote or formula) writes nothing and
    /// leaves the paid gap credit untouched.
    fn render_block(&mut self, block: Option<Fragment>) {
        if let Some(block) = block {
            self.begin_block();
            self.sink.write_block(block);
            self.end_block();
        }
    }

    // ---- spacing state machine ----

    /// No-op when the last unit was a blank (or nothing yet), else one `"\n"`.
    fn emit_blank(&mut self) {
        if matches!(self.last_unit, Unit::Blank | Unit::None) {
            return;
        }
        self.last_unit = Unit::Blank;
        self.sink.write("\n");
    }

    /// One already-styled paragraph line; a separating blank first only after a block.
    fn emit_text(&mut self, s: &str) {
        if self.last_unit == Unit::Block {
            self.sink.write("\n");
        }
        self.last_unit = Unit::Text;
        self.sink.write(&format!("{s}\n"));
    }

    /// Right before a block renders: one separating blank on a text→block or
    /// block→block boundary — unless the open block already paid it. Never
    /// updates `last_unit` (the render + `end_block` do that).
    fn begin_block(&mut self) {
        if self.gap_paid {
            self.gap_paid = false;
            return;
        }
        if self.needs_gap() {
            self.sink.write("\n");
        }
    }

    fn needs_gap(&self) -> bool {
        matches!(self.last_unit, Unit::Text | Unit::Block)
    }

    /// Opens a buffering block, paying its separating blank FIRST — the open block shows
    /// where the closed one will sit (the open-block-pays-separator law; `begin_block` then
    /// skips the blank it already owes) — and shows what it holds already.
    fn open_block(&mut self, block: Block) {
        if self.needs_gap() {
            self.sink.write("\n");
            self.gap_paid = true;
        }
        self.block = block;
        self.grow();
    }

    /// The blank AFTER a block is produced lazily by the next unit's separator, so
    /// nothing is emitted here — state only.
    fn end_block(&mut self) {
        self.last_unit = Unit::Block;
    }
}

/// plainSink shape over `io::Write` (fixed width; write errors ignored —
/// T-07: the Sink is infallible because facade commits are fire-and-forget).
struct PlainSink {
    w: Box<dyn std::io::Write + Send>,
    width: usize,
}

impl Sink for PlainSink {
    fn write(&mut self, rendered: &str) {
        let _ = self.w.write_all(rendered.as_bytes()); // T-07: errors ignored
    }

    fn width(&self) -> usize {
        self.width
    }
}

/// A Writer emitting plain rendered output to `w` at a fixed layout width — the
/// piped/test shape. Renders with the Monokai theme and the process's
/// color decision ([`crate::app::color::enabled`] — Go's `NewWriterTo` read `color.NoColor` the
/// same way; a test process never decides, so it renders with color ON); callers needing
/// other options build [`Writer::new`] over their own [`Sink`].
pub fn new_writer_to(w: Box<dyn std::io::Write + Send>, width: usize) -> Writer {
    Writer::new(
        Box::new(PlainSink { w, width }),
        RenderOptions {
            color: crate::app::color::enabled(),
            code_theme: CodeTheme::Monokai,
        },
    )
}
