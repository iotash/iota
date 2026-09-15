//! Pure streaming markdown→ANSI renderer (internal/markdown; `TUI_CONTRACTS` §3): zero terminal deps, zero
//! async, no regex (hand parsers). The width ruler and its escape-aware companion it shares with `ui` and
//! `repl` live in `crate::markdown::text::{width, ansi}`.
//!
//! The streaming renderer (`TUI_CONTRACTS` §3.4; markdown.go Writer). ONE `Writer` per
//! content block. State: byte line buffer; the open buffering block as one [`Block`] value
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
//! exactly ONE blank line; `open_preview` pays the separating blank BEFORE the preview
//! opens (`gap_paid` credit) so the streaming layout equals the settled layout — the
//! preview-pays-separator law, for all five buffering block types.

pub(crate) mod blocks;
pub mod highlight;
pub(crate) mod html;
pub(crate) mod inline;
pub(crate) mod link;
pub(crate) mod preview;
pub(crate) mod sink;
pub(crate) mod style;

pub use highlight::{CodeHighlighter, PlainIndent, SyntectHighlighter};
pub use link::hyperlink;
pub use preview::PreviewHandle;
pub use sink::Sink;
pub use style::Style;

use crate::markdown::blocks::math::{display_open, is_display_close};
use crate::markdown::inline::{highlight_line, is_block_line, is_list_line, split_list_marker};

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

/// One parsed item of a buffering list block (Go listItem). Parsed by [`ListBlock`];
/// rendered by `blocks::list::render_list`.
pub(crate) struct ListItem {
    /// Nesting depth derived from source indentation (clamped, never skips a level).
    pub(crate) level: usize,
    /// `"•"`, `"☐"`, `"☑"`, or the ordered token as written (`"3."`, `"7)"`).
    pub(crate) marker: String,
    /// Item text: first line + continuations (`""` = intra-item paragraph break).
    pub(crate) lines: Vec<String>,
}

/// The uniform left margin of rendered display-math rows — the same two-space rule as
/// code blocks, so formulas and code sit on one left rule (markdown.go mathIndent).
const MATH_INDENT: &str = "  ";

/// The buffering block the writer is inside — markdown.go's five `in*` flags and their
/// buffers as ONE value. At most one block is open at a time (a fence, a table, a list, a
/// quote or a display-math block) and `None` is the plain paragraph path. Each variant owns
/// its raw lines and its live preview, and renders itself once, when it closes.
enum Block {
    /// No block open: blanks, headings, rules and paragraph lines dispatch directly.
    None,
    /// Inside a fenced code block.
    Code(CodeBlock),
    /// Buffering table rows.
    Table(TableBlock),
    /// Buffering list items.
    List(ListBlock),
    /// Buffering quote lines.
    Quote(QuoteBlock),
    /// Inside a `$$` / `\[` display-math block.
    Math(MathBlock),
}

/// Closes a block's live preview, if it opened one — always BEFORE the rendered block is
/// written, so the preview row is released to the block that replaces it.
fn close_view(view: &mut Option<Box<dyn PreviewHandle>>) {
    if let Some(mut v) = view.take() {
        v.close();
    }
}

/// A fenced code block (markdown.go:219-249): the language tag of the opening fence and
/// the raw lines up to the closing one.
struct CodeBlock {
    lang: String,
    lines: Vec<String>,
    view: Option<Box<dyn PreviewHandle>>,
    /// The display-math block a fence line interrupted. Go's dispatch checks the fence
    /// FIRST and opens the code block without closing an open `$$` block, so the formula
    /// is open again once the fence closes — including at the end of input, where
    /// markdown.go:379-399 renders the fence alone and leaves `inMath` set. Kept as written.
    interrupted: Option<MathBlock>,
}

impl CodeBlock {
    fn push(&mut self, line: &str) {
        self.lines.push(line.to_owned());
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    /// Closes the preview and renders the block; `render_code` output already carries its
    /// trailing newline (the indentCode shape).
    fn render(mut self, opts: RenderOptions) -> String {
        close_view(&mut self.view);
        let code = self.lines.join("\n");
        blocks::code::render_code(&code, &self.lang, opts)
    }
}

/// A table block: the parsed cells of each row and its separator flag
/// (markdown.go:1291-1313).
struct TableBlock {
    rows: Vec<Vec<String>>,
    seps: Vec<bool>,
    view: Option<Box<dyn PreviewHandle>>,
}

impl TableBlock {
    /// Buffers one table row (parsed cells + separator flag), mirroring the raw line into
    /// the preview.
    fn push(&mut self, line: &str) {
        let cells = parse_table_cells(line);
        self.seps.push(is_table_separator(&cells));
        self.rows.push(cells);
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    fn render(mut self, width: usize, color: bool) -> Option<String> {
        close_view(&mut self.view);
        if self.rows.is_empty() {
            return None;
        }
        let rendered = blocks::table::render_table(&self.rows, &self.seps, width, color);
        Some(format!("{rendered}\n"))
    }
}

/// A list block (markdown.go:876-943): the parsed items, whether a blank between items
/// made the list loose, and whether one blank is currently HELD — it ends the list if
/// another blank follows, makes the list loose if an item or continuation follows.
struct ListBlock {
    items: Vec<ListItem>,
    loose: bool,
    blank: bool,
    view: Option<Box<dyn PreviewHandle>>,
}

impl ListBlock {
    /// Feeds one line; returns whether it was consumed. When it was not, the list ends:
    /// the caller flushes it (re-emitting the held blank) and processes the line normally
    /// (markdown.go:876-919).
    fn consume(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if self.blank {
                // A second blank ends the list; both blanks re-emit after it (the
                // held one by the flush, the current one via the caller).
                return false;
            }
            self.blank = true;
            self.preview(line);
            return true;
        }
        if is_list_line(line) {
            if self.blank {
                // The held blank separated two items: the list is loose.
                self.blank = false;
                self.loose = true;
            }
            self.append_item(line);
            self.preview(line);
            return true;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // An indented table under a list item is still a table (LLMs commonly
            // nest one below a bullet): end the list and let the caller's table
            // branch take the line — the same courtesy the fence branch extends.
            if is_table_line(trimmed) {
                return false;
            }
            // The same courtesy for display math, which LLMs nest under a numbered
            // step at least as often ("3. the rigorous version:" then an indented
            // "$$…$$"). Without this the fence and the formula were swallowed as
            // continuation text and echoed raw, since the display-math branch sits
            // after this one. Both the bare opening fence and the complete one-line
            // form escape; the closing fence never reaches here (a display block
            // buffers ahead of the list check once open).
            if display_open(trimmed).is_some() {
                return false;
            }
            // Indented text continues the previous item; a held blank becomes an
            // intra-item paragraph break and makes the list loose.
            let held = self.blank;
            if held {
                self.blank = false;
                self.loose = true;
            }
            if let Some(it) = self.items.last_mut() {
                if held {
                    it.lines.push(String::new());
                }
                it.lines.push(trimmed.to_owned());
            }
            self.preview(line);
            return true;
        }
        false
    }

    /// Parses a marker line into a new item (markdown.go:922-943): first item forced
    /// level 0, later levels clamped to prev+1; ordered tokens kept AS WRITTEN; task
    /// checkboxes become `☐`/`☑` with the marker text stripped; else `•`.
    fn append_item(&mut self, line: &str) {
        let (marker, rest) = split_list_marker(line).unwrap_or(("", line));
        let bullet = marker.trim_start_matches([' ', '\t']);
        let mut level = indent_level(&marker[..marker.len() - bullet.len()]);
        if let Some(prev) = self.items.last() {
            if level > prev.level + 1 {
                level = prev.level + 1; // never skip a level
            }
        } else {
            level = 0; // a block always starts at the top level
        }
        let mut rest = rest.to_owned();
        let glyph = if bullet.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            bullet.trim().to_owned() // ordered: keep the number as written
        } else if let Some(t) = task_marker(&rest) {
            let glyph = if t.contains(['x', 'X']) { "☑" } else { "☐" };
            let n = t.len();
            rest.drain(..n);
            glyph.to_owned()
        } else {
            "•".to_owned()
        };
        self.items.push(ListItem {
            level,
            marker: glyph,
            lines: vec![rest],
        });
    }

    /// Mirrors a consumed raw line into the live block preview.
    fn preview(&mut self, line: &str) {
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    fn render(mut self, color: bool) -> Option<String> {
        close_view(&mut self.view);
        if self.items.is_empty() {
            return None;
        }
        let rendered = blocks::list::render_list(&self.items, self.loose, color);
        Some(format!("{rendered}\n"))
    }
}

/// A quote block: the inner lines with their `> ` markers stripped (markdown.go:993-1006).
struct QuoteBlock {
    body: Vec<String>,
    view: Option<Box<dyn PreviewHandle>>,
}

impl QuoteBlock {
    fn append(&mut self, line: &str) {
        self.body.push(strip_quote_marker(line).to_owned());
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    fn render(mut self, width: usize, opts: RenderOptions) -> Option<String> {
        close_view(&mut self.view);
        if self.body.is_empty() {
            return None;
        }
        let rendered = blocks::quote::render_quote(&self.body, width, opts);
        Some(format!("{rendered}\n"))
    }
}

/// A display-math block: the raw source lines between the `$$` / `\[` fences (the
/// one-line form is a `MathBlock` of one line that renders at once, no preview).
struct MathBlock {
    lines: Vec<String>,
    view: Option<Box<dyn PreviewHandle>>,
}

impl MathBlock {
    fn append(&mut self, line: &str) {
        self.lines.push(line.to_owned());
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    /// Renders the buffered display-math block (markdown.go:1433-1455): a
    /// whitespace-only source renders NOTHING (the paid gap credit may remain
    /// consumed); otherwise every row is prefixed by the two-space `MATH_INDENT` and the
    /// block rides `begin_block`/`end_block`. The body transform is the mathtext 2D layout
    /// (markdown.go:1443 `mathtext.Render2D`; DESIGN D16 step 2), which degrades to the cleaned
    /// linear source when the formula cannot be laid out; either way the rows print in normal
    /// color (never dim: dim is decoration-only).
    fn render(mut self, width: usize) -> Option<String> {
        close_view(&mut self.view);
        let src = self.lines.join("\n");
        if src.trim().is_empty() {
            return None; // an empty $$ block renders nothing (mirrors the quote)
        }
        let width = width.saturating_sub(MATH_INDENT.len());
        let (block, _ok) = crate::mathtext::render_2d(&src, width);
        let mut out = String::new();
        for r in block.split('\n') {
            out.push_str(MATH_INDENT);
            out.push_str(r);
            out.push('\n');
        }
        Some(out)
    }
}

/// Streaming markdown renderer; ONE per content block.
pub struct Writer {
    sink: Box<dyn Sink>,
    opts: RenderOptions,
    buf: Vec<u8>,
    /// The open buffering block, if any.
    block: Block,
    /// The buffering block's separating blank was already written when its preview
    /// opened, so `begin_block` must not write it again.
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
                    Block::Math(math) => Some(math),
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
            return;
        }

        // A buffering list block consumes marker lines, indented continuations, and
        // one held blank; anything else flushes the block (the held blank re-emits)
        // and falls through.
        if let Block::List(list) = &mut self.block {
            if list.consume(line) {
                return;
            }
            self.flush_block();
        }

        // A buffering quote block consumes consecutive quote lines; the first
        // non-quote line flushes it and falls through.
        if let Block::Quote(quote) = &mut self.block {
            if is_quote_line(line) {
                quote.append(line);
                return;
            }
            self.flush_block();
        }

        // A buffering display-math block consumes lines until its closing fence.
        if let Block::Math(math) = &mut self.block {
            if !is_display_close(line) {
                math.append(line);
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
                let body = MathBlock {
                    lines: vec![body],
                    view: None,
                }
                .render(self.term_width());
                self.render_block(body);
            } else {
                let view = self.open_preview("rendering math…");
                let lines = Vec::new();
                self.block = Block::Math(MathBlock { lines, view });
            }
            return;
        }

        // Only a table can still be open here; its FIRST row opens the live preview.
        if is_table_line(line) {
            if let Block::Table(table) = &mut self.block {
                table.push(line);
            } else {
                let view = self.open_preview("rendering table…");
                let mut table = TableBlock {
                    rows: Vec::new(),
                    seps: Vec::new(),
                    view,
                };
                table.push(line);
                self.block = Block::Table(table);
            }
            return;
        }

        self.flush_block(); // a pending table

        if is_list_line(line) {
            let view = self.open_preview("rendering list…");
            let mut list = ListBlock {
                items: Vec::new(),
                loose: false,
                blank: false,
                view,
            };
            list.append_item(line);
            list.preview(line);
            self.block = Block::List(list);
            return;
        }

        if is_quote_line(line) {
            let view = self.open_preview("rendering quote…");
            let mut quote = QuoteBlock {
                body: Vec::new(),
                view,
            };
            quote.append(line);
            self.block = Block::Quote(quote);
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
        let lang = fence
            .trim()
            .strip_prefix("```")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let view = self.open_preview(&code_label(&lang));
        self.block = Block::Code(CodeBlock {
            lang,
            lines: Vec::new(),
            view,
            interrupted,
        });
    }

    /// Closes and renders the open block, whichever it is. A list re-emits its held blank
    /// after the render (the blank turned out to END the list, not to make it loose) —
    /// routed through `emit_blank` so it joins the blank-run collapse. A fence leaves the
    /// display-math block it interrupted open again (`CodeBlock::interrupted`).
    fn flush_block(&mut self) {
        let width = self.term_width();
        let opts = self.opts;
        let mut resumed = None;
        let (body, held_blank) = match std::mem::replace(&mut self.block, Block::None) {
            Block::None => return,
            Block::Code(mut code) => {
                resumed = code.interrupted.take();
                (Some(code.render(opts)), false)
            }
            Block::Table(table) => (table.render(width, opts.color), false),
            Block::List(list) => {
                let held = list.blank;
                (list.render(opts.color), held)
            }
            Block::Quote(quote) => (quote.render(width, opts), false),
            Block::Math(math) => (math.render(width), false),
        };
        self.render_block(body);
        if held_blank {
            self.emit_blank();
        }
        self.block = resumed.map_or(Block::None, Block::Math);
    }

    /// Writes a rendered block between `begin_block` and `end_block`; `None` (an empty
    /// table, quote or formula) writes nothing and leaves the paid gap credit untouched.
    fn render_block(&mut self, body: Option<String>) {
        if let Some(body) = body {
            self.begin_block();
            self.sink.write(&body);
            self.end_block();
        }
    }

    /// Block-layout width: the sink's LIVE width (a mid-stream resize changes the
    /// next block), 80 when the sink reports none.
    fn term_width(&self) -> usize {
        let w = self.sink.width();
        if w == 0 { 80 } else { w }
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
    /// block→block boundary — unless the block's preview already paid it. Never
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

    /// Opens a buffering block's live preview, paying its separating blank FIRST —
    /// the preview occupies the row the rendered block will, so it must sit where
    /// that block will sit (the preview-pays-separator law; `begin_block` then skips
    /// the blank it already owes).
    fn open_preview(&mut self, label: &str) -> Option<Box<dyn PreviewHandle>> {
        if self.needs_gap() {
            self.sink.write("\n");
            self.gap_paid = true;
        }
        self.sink.block_preview(label)
    }

    /// The blank AFTER a block is produced lazily by the next unit's separator, so
    /// nothing is emitted here — state only.
    fn end_block(&mut self) {
        self.last_unit = Unit::Block;
    }
}

/// The buffering code preview label (markdown.go:1372-1377) — U+2026 ellipsis.
fn code_label(lang: &str) -> String {
    if lang.is_empty() {
        "rendering code…".to_owned()
    } else {
        format!("rendering code ({lang})…")
    }
}

/// isQuoteLine twin: the trimmed form is exactly `">"` or starts with `"> "`.
pub(crate) fn is_quote_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == ">" || trimmed.starts_with("> ")
}

/// stripQuoteMarker twin: removes exactly ONE leading `"> "` (or a bare `">"`).
pub(crate) fn strip_quote_marker(line: &str) -> &str {
    let trimmed = line.trim();
    if trimmed == ">" {
        return "";
    }
    trimmed.strip_prefix("> ").unwrap_or(trimmed)
}

/// indentLevel twin: every two columns are one nesting level (a tab counts as two).
pub(crate) fn indent_level(indent: &str) -> usize {
    let cols: usize = indent.chars().map(|r| if r == '\t' { 2 } else { 1 }).sum();
    cols / 2
}

/// taskMarkerRe hand parser — Go `^\[([ xX])\](?: |$)`: returns the matched prefix
/// (including the trailing space when present) or `None`.
pub(crate) fn task_marker(rest: &str) -> Option<&str> {
    let b = rest.as_bytes();
    if b.len() >= 3 && b[0] == b'[' && matches!(b[1], b' ' | b'x' | b'X') && b[2] == b']' {
        if b.len() == 3 {
            return Some(&rest[..3]);
        }
        if b[3] == b' ' {
            return Some(&rest[..4]);
        }
    }
    None
}

/// isTableLine twin: the trimmed line starts with `'|'`.
pub(crate) fn is_table_line(line: &str) -> bool {
    line.trim().starts_with('|')
}

/// parseTableCells twin (markdown.go:1291-1313): strip one leading and one trailing
/// `|`, split on `|`; per cell replace `\t` with one space (three rulers disagreed on
/// tabs), strip variation selectors (VS16/VS15 — cursor-advance ambiguity; flags and
/// ZWJ sequences deliberately kept), trim.
pub(crate) fn parse_table_cells(line: &str) -> Vec<String> {
    let mut t = line.trim();
    t = t.strip_prefix('|').unwrap_or(t);
    t = t.strip_suffix('|').unwrap_or(t);
    t.split('|')
        .map(|p| {
            strip_variation_selectors(&p.replace('\t', " "))
                .trim()
                .to_owned()
        })
        .collect()
}

/// stripVariationSelectors twin: drops U+FE0F/U+FE0E so bordered layouts stay aligned
/// on every terminal (only the bare base rune advances consistently everywhere).
pub(crate) fn strip_variation_selectors(s: &str) -> String {
    if !s.contains(['\u{FE0F}', '\u{FE0E}']) {
        return s.to_owned();
    }
    s.chars()
        .filter(|r| *r != '\u{FE0F}' && *r != '\u{FE0E}')
        .collect()
}

/// isTableSeparator twin: every cell matches `^:?-+:?$` (alignment colons parsed but
/// IGNORED).
pub(crate) fn is_table_separator(cells: &[String]) -> bool {
    !cells.is_empty() && cells.iter().all(|c| sep_cell(c))
}

fn sep_cell(c: &str) -> bool {
    let b = c.as_bytes();
    let mut i = usize::from(b.first() == Some(&b':'));
    let dash_start = i;
    while i < b.len() && b[i] == b'-' {
        i += 1;
    }
    if i == dash_start {
        return false;
    }
    if i < b.len() && b[i] == b':' {
        i += 1;
    }
    i == b.len()
}

/// splitBR twin — Go `(?i)<br\s*/?>`: splits a cell into multi-line segments.
pub(crate) fn split_br(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut seg_start = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' && i + 2 < b.len() && (b[i + 1] | 0x20) == b'b' && (b[i + 2] | 0x20) == b'r'
        {
            let mut j = i + 3;
            while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n' | b'\x0c' | b'\r') {
                j += 1;
            }
            if j < b.len() && b[j] == b'/' {
                j += 1;
            }
            if j < b.len() && b[j] == b'>' {
                parts.push(s[seg_start..i].to_owned());
                i = j + 1;
                seg_start = i;
                continue;
            }
        }
        i += 1;
    }
    parts.push(s[seg_start..].to_owned());
    parts
}

/// plainSink shape over `io::Write` (fixed width, no previews; write errors ignored —
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

    fn block_preview(&mut self, _label: &str) -> Option<Box<dyn PreviewHandle>> {
        None
    }
}

/// A Writer emitting plain rendered output to `w` at a fixed layout width — the
/// piped/test shape, no live previews. Renders with the Monokai theme and the process's
/// color decision ([`crate::color::enabled`] — Go's `NewWriterTo` read `color.NoColor` the
/// same way; a test process never decides, so it renders with color ON); callers needing
/// other options build [`Writer::new`] over their own [`Sink`].
pub fn new_writer_to(w: Box<dyn std::io::Write + Send>, width: usize) -> Writer {
    Writer::new(
        Box::new(PlainSink { w, width }),
        RenderOptions {
            color: crate::color::enabled(),
            code_theme: CodeTheme::Monokai,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{
        indent_level, is_quote_line, is_table_separator, parse_table_cells, split_br,
        strip_quote_marker, task_marker,
    };

    // Hand-parser pins for Go's tableSepRe `^:?-+:?$` (markdown spec §regexes).
    #[test]
    fn table_separator_pins() {
        let cells = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(is_table_separator(&cells(&[
            "---", "-", ":--:", ":-", "-:"
        ])));
        assert!(!is_table_separator(&cells(&["---", ""])));
        assert!(!is_table_separator(&cells(&["::"])));
        assert!(!is_table_separator(&cells(&["x--"])));
        assert!(!is_table_separator(&cells(&["--x"])));
        assert!(!is_table_separator(&[]));
    }

    // Hand-parser pins for Go's brRe `(?i)<br\s*/?>`.
    #[test]
    fn split_br_pins() {
        assert_eq!(split_br("a<br>b"), ["a", "b"]);
        assert_eq!(split_br("a<BR/>b"), ["a", "b"]);
        assert_eq!(split_br("a<br />b"), ["a", "b"]);
        assert_eq!(split_br("a<Br\t/>b"), ["a", "b"]);
        assert_eq!(split_br("a<brx>b"), ["a<brx>b"]);
        assert_eq!(split_br("a<b>r</b>"), ["a<b>r</b>"]);
        assert_eq!(split_br("plain"), ["plain"]);
    }

    // Go: internal/markdown/markdown.go:993-1006 quote line shapes.
    #[test]
    fn quote_marker_pins() {
        assert!(is_quote_line("> x"));
        assert!(is_quote_line("  >"));
        assert!(!is_quote_line(">x"));
        assert_eq!(strip_quote_marker(">"), "");
        assert_eq!(strip_quote_marker("> quoted"), "quoted");
        assert_eq!(strip_quote_marker("  > q"), "q");
    }

    // Go: internal/markdown/markdown.go:853-863 (tab = two columns, cols/2).
    #[test]
    fn indent_level_pins() {
        assert_eq!(indent_level(""), 0);
        assert_eq!(indent_level(" "), 0);
        assert_eq!(indent_level("  "), 1);
        assert_eq!(indent_level("\t"), 1);
        assert_eq!(indent_level("    "), 2);
    }

    // Go: internal/markdown/markdown.go:838 taskMarkerRe `^\[([ xX])\](?: |$)`.
    #[test]
    fn task_marker_pins() {
        assert_eq!(task_marker("[ ] write"), Some("[ ] "));
        assert_eq!(task_marker("[x] ship"), Some("[x] "));
        assert_eq!(task_marker("[X]"), Some("[X]"));
        assert_eq!(task_marker("[y] no"), None);
        assert_eq!(task_marker("[x]!"), None);
        assert_eq!(task_marker("x"), None);
    }

    // Go: internal/markdown/markdown.go:1291-1313.
    #[test]
    fn parse_table_cells_pins() {
        assert_eq!(parse_table_cells("| a | b |"), ["a", "b"]);
        assert_eq!(parse_table_cells("|a|b"), ["a", "b"]);
        assert_eq!(parse_table_cells("| a\tb |"), ["a b"]);
        assert_eq!(parse_table_cells("| \u{2696}\u{FE0F} c |"), ["\u{2696} c"]);
    }
}
