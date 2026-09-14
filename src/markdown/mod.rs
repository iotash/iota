//! Pure streaming markdown→ANSI renderer (internal/markdown; `TUI_CONTRACTS` §3): zero terminal deps, zero
//! async, no regex (hand parsers). The width ruler and its escape-aware companion it shares with `ui` and
//! `repl` live in `crate::markdown::text::{width, ansi}`.
//!
//! The streaming renderer (`TUI_CONTRACTS` §3.4; markdown.go Writer). ONE `Writer` per
//! content block. State: byte line buffer; five mutually exclusive buffering blocks
//! (fence/table/list/quote/math); `gap_paid`; `last_unit` spacing state.
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

pub(crate) mod code;
pub mod highlight;
pub(crate) mod html;
pub(crate) mod inline;
pub(crate) mod link;
pub(crate) mod list;
pub(crate) mod math;
pub(crate) mod quote;
pub(crate) mod sink;
pub(crate) mod style;
pub(crate) mod table;

pub use highlight::{CodeHighlighter, PlainIndent, SyntectHighlighter};
pub use link::hyperlink;
pub(crate) use math::MathRenderer;
pub use sink::{PreviewHandle, Sink};
pub use style::Style;

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

/// One parsed item of a buffering list block (Go listItem). Parsed here by the
/// dispatch; rendered by `list::render_list` (WP42's file).
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

/// Streaming markdown renderer; ONE per content block.
pub struct Writer {
    sink: Box<dyn Sink>,
    opts: RenderOptions,
    buf: Vec<u8>,
    in_fence: bool,
    fence_lang: String,
    code_lines: Vec<String>,
    table_rows: Vec<Vec<String>>,
    table_seps: Vec<bool>,
    in_list: bool,
    list_items: Vec<ListItem>,
    list_loose: bool,
    list_blank: bool,
    in_quote: bool,
    quote_body: Vec<String>,
    in_math: bool,
    math_lines: Vec<String>,
    /// The buffering block's separating blank was already written when its preview
    /// opened, so `begin_block` must not write it again.
    gap_paid: bool,
    last_unit: Unit,
    table_view: Option<Box<dyn PreviewHandle>>,
    code_view: Option<Box<dyn PreviewHandle>>,
    list_view: Option<Box<dyn PreviewHandle>>,
    quote_view: Option<Box<dyn PreviewHandle>>,
    math_view: Option<Box<dyn PreviewHandle>>,
    /// The display-math body transform (T-08/T-15, CLOSED): [`crate::mathtext::Mathtext`], the
    /// 2D layout engine. The hook stays a trait object so a test can swap the transform without
    /// touching the state machine.
    math_renderer: Box<dyn MathRenderer>,
}

impl Writer {
    /// A renderer writing to `sink` with `opts`. `last_unit` starts `None` so leading
    /// blanks are suppressed and no separator precedes the first unit.
    pub fn new(sink: Box<dyn Sink>, opts: RenderOptions) -> Self {
        Self {
            sink,
            opts,
            buf: Vec::new(),
            in_fence: false,
            fence_lang: String::new(),
            code_lines: Vec::new(),
            table_rows: Vec::new(),
            table_seps: Vec::new(),
            in_list: false,
            list_items: Vec::new(),
            list_loose: false,
            list_blank: false,
            in_quote: false,
            quote_body: Vec::new(),
            in_math: false,
            math_lines: Vec::new(),
            gap_paid: false,
            last_unit: Unit::None,
            table_view: None,
            code_view: None,
            list_view: None,
            quote_view: None,
            math_view: None,
            // markdown.go:1443: display math renders through the mathtext engine (DESIGN D16
            // step 2 — the inline half flipped with `render_inline` in WP61).
            math_renderer: Box::new(crate::mathtext::Mathtext),
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

    /// Final partial line through the SAME dispatch; closes any open block —
    /// fence/math/list/quote are mutually exclusive, a surviving table closes last
    /// (markdown.go:379-399).
    pub fn flush(&mut self) {
        if !self.buf.is_empty() {
            let taken = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&taken).into_owned();
            self.consume_line(&line);
        }
        if self.in_fence {
            self.in_fence = false;
            self.flush_code();
        } else if self.in_math {
            self.flush_math();
        } else if self.in_list {
            self.finish_list();
        } else if self.in_quote {
            self.flush_quote();
        }
        if !self.table_rows.is_empty() {
            self.flush_table();
        }
    }

    /// consumeLine twin (markdown.go:219-369) — THE dispatch ordering law.
    fn consume_line(&mut self, line: &str) {
        // Fenced code block: buffer until the closing fence, then render once. A fence
        // line inside list/quote/table context always wins (flush-first).
        if line.trim().starts_with("```") {
            if self.in_fence {
                self.in_fence = false;
                self.flush_code();
            } else {
                if !self.table_rows.is_empty() {
                    self.flush_table();
                }
                if self.in_list {
                    self.finish_list();
                }
                if self.in_quote {
                    self.flush_quote();
                }
                self.in_fence = true;
                line.trim()
                    .strip_prefix("```")
                    .unwrap_or_default()
                    .trim()
                    .clone_into(&mut self.fence_lang);
                self.code_lines.clear();
                let label = code_label(&self.fence_lang);
                self.code_view = self.open_preview(&label);
            }
            return;
        }
        if self.in_fence {
            self.code_lines.push(line.to_owned());
            if let Some(v) = &mut self.code_view {
                v.write_raw_line(line);
            }
            return;
        }

        // A buffering list block consumes marker lines, indented continuations, and
        // one held blank; anything else flushes the block and falls through.
        if self.in_list && self.list_consume(line) {
            return;
        }

        // A buffering quote block consumes consecutive quote lines; the first
        // non-quote line flushes it and falls through.
        if self.in_quote {
            if is_quote_line(line) {
                self.quote_append(line);
                return;
            }
            self.flush_quote();
        }

        // A buffering display-math block consumes lines until its closing fence.
        if self.in_math {
            if math::is_display_close(line) {
                self.flush_math();
            } else {
                self.math_append(line);
            }
            return;
        }

        // Open a display-math block: a bare "$$"/"\[" fence starts a buffered block,
        // a complete one-line "$$…$$"/"\[…\]" renders at once. Either form first
        // flushes a pending table/list (the quote path already flushed above).
        if let Some((body, one_line)) = math::display_open(line) {
            if !self.table_rows.is_empty() {
                self.flush_table();
            }
            if self.in_list {
                self.finish_list();
            }
            if one_line {
                self.math_lines = vec![body];
                self.flush_math();
            } else {
                self.start_math();
            }
            return;
        }

        if is_table_line(line) {
            self.table_consume(line);
            return;
        }

        if !self.table_rows.is_empty() {
            self.flush_table();
        }

        if is_list_line(line) {
            self.start_list(line);
            return;
        }

        if is_quote_line(line) {
            self.start_quote(line);
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

    // ---- table ----

    /// Buffers one table row (parsed cells + separator flag), opening the live
    /// preview on the FIRST row and mirroring each raw line.
    fn table_consume(&mut self, line: &str) {
        if self.table_rows.is_empty() {
            self.table_view = self.open_preview("rendering table…");
        }
        let cells = parse_table_cells(line);
        self.table_seps.push(is_table_separator(&cells));
        self.table_rows.push(cells);
        if let Some(v) = &mut self.table_view {
            v.write_raw_line(line);
        }
    }

    fn flush_table(&mut self) {
        if let Some(mut v) = self.table_view.take() {
            v.close();
        }
        let rows = std::mem::take(&mut self.table_rows);
        let seps = std::mem::take(&mut self.table_seps);
        if rows.is_empty() {
            return;
        }
        self.begin_block();
        let rendered =
            crate::markdown::table::render_table(&rows, &seps, self.term_width(), self.opts.color);
        self.sink.write(&format!("{rendered}\n"));
        self.end_block();
    }

    // ---- list ----

    /// Opens a list block with the given marker line as its first item.
    fn start_list(&mut self, line: &str) {
        self.in_list = true;
        self.list_view = self.open_preview("rendering list…");
        self.list_append_item(line);
        self.list_preview(line);
    }

    /// Feeds one line to the buffering list block; returns whether it was consumed.
    /// When it was not, the block (and any held blank) has already been flushed and
    /// the caller must process the line normally (markdown.go:876-919).
    fn list_consume(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if self.list_blank {
                // A second blank ends the list; both blanks re-emit after it (the
                // held one inside finish_list, the current one via the caller).
                self.finish_list();
                return false;
            }
            self.list_blank = true;
            self.list_preview(line);
            return true;
        }
        if is_list_line(line) {
            if self.list_blank {
                // The held blank separated two items: the list is loose.
                self.list_blank = false;
                self.list_loose = true;
            }
            self.list_append_item(line);
            self.list_preview(line);
            return true;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // An indented table under a list item is still a table (LLMs commonly
            // nest one below a bullet): flush the list and let the caller's table
            // branch take the line — the same courtesy the fence branch extends.
            if is_table_line(trimmed) {
                self.finish_list();
                return false;
            }
            // The same courtesy for display math, which LLMs nest under a numbered
            // step at least as often ("3. the rigorous version:" then an indented
            // "$$…$$"). Without this the fence and the formula were swallowed as
            // continuation text and echoed raw, since the display-math branch sits
            // after this one. Both the bare opening fence and the complete one-line
            // form escape; the closing fence never reaches here (a display block
            // buffers ahead of the list check once open).
            if crate::mathtext::delim::display_open(trimmed).is_some() {
                self.finish_list();
                return false;
            }
            // Indented text continues the previous item; a held blank becomes an
            // intra-item paragraph break and makes the list loose.
            let held = self.list_blank;
            if held {
                self.list_blank = false;
                self.list_loose = true;
            }
            if let Some(it) = self.list_items.last_mut() {
                if held {
                    it.lines.push(String::new());
                }
                it.lines.push(trimmed.to_owned());
            }
            self.list_preview(line);
            return true;
        }
        self.finish_list();
        false
    }

    /// Parses a marker line into a new item (markdown.go:922-943): first item forced
    /// level 0, later levels clamped to prev+1; ordered tokens kept AS WRITTEN; task
    /// checkboxes become `☐`/`☑` with the marker text stripped; else `•`.
    fn list_append_item(&mut self, line: &str) {
        let (marker, rest) = split_list_marker(line).unwrap_or(("", line));
        let bullet = marker.trim_start_matches([' ', '\t']);
        let mut level = indent_level(&marker[..marker.len() - bullet.len()]);
        if let Some(prev) = self.list_items.last() {
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
        self.list_items.push(ListItem {
            level,
            marker: glyph,
            lines: vec![rest],
        });
    }

    /// Mirrors a consumed raw line into the live block preview.
    fn list_preview(&mut self, line: &str) {
        if let Some(v) = &mut self.list_view {
            v.write_raw_line(line);
        }
    }

    /// Flushes the buffering list and re-emits a held blank after it (the blank
    /// turned out to END the list, not to make it loose); the re-emit routes through
    /// `emit_blank` so it participates in the blank-run collapse.
    fn finish_list(&mut self) {
        self.flush_list();
        if self.list_blank {
            self.list_blank = false;
            self.emit_blank();
        }
    }

    fn flush_list(&mut self) {
        if let Some(mut v) = self.list_view.take() {
            v.close();
        }
        let items = std::mem::take(&mut self.list_items);
        let loose = self.list_loose;
        self.list_loose = false;
        self.in_list = false;
        if items.is_empty() {
            return;
        }
        self.begin_block();
        let rendered = crate::markdown::list::render_list(&items, loose, self.opts.color);
        self.sink.write(&format!("{rendered}\n"));
        self.end_block();
    }

    // ---- quote ----

    fn start_quote(&mut self, line: &str) {
        self.in_quote = true;
        self.quote_body.clear();
        self.quote_view = self.open_preview("rendering quote…");
        self.quote_append(line);
    }

    fn quote_append(&mut self, line: &str) {
        self.quote_body.push(strip_quote_marker(line).to_owned());
        if let Some(v) = &mut self.quote_view {
            v.write_raw_line(line);
        }
    }

    fn flush_quote(&mut self) {
        if let Some(mut v) = self.quote_view.take() {
            v.close();
        }
        let body = std::mem::take(&mut self.quote_body);
        self.in_quote = false;
        if body.is_empty() {
            return;
        }
        self.begin_block();
        let rendered = crate::markdown::quote::render_quote(&body, self.term_width(), self.opts);
        self.sink.write(&format!("{rendered}\n"));
        self.end_block();
    }

    // ---- code ----

    fn flush_code(&mut self) {
        if let Some(mut v) = self.code_view.take() {
            v.close();
        }
        let code = self.code_lines.join("\n");
        let lang = std::mem::take(&mut self.fence_lang);
        self.code_lines.clear();
        self.begin_block();
        // render_code output already carries its trailing newline (indentCode shape).
        let rendered = crate::markdown::code::render_code(&code, &lang, self.opts);
        self.sink.write(&rendered);
        self.end_block();
    }

    // ---- display math ----

    fn start_math(&mut self) {
        self.in_math = true;
        self.math_lines.clear();
        self.math_view = self.open_preview("rendering math…");
    }

    fn math_append(&mut self, line: &str) {
        self.math_lines.push(line.to_owned());
        if let Some(v) = &mut self.math_view {
            v.write_raw_line(line);
        }
    }

    /// Renders the buffered display-math block (markdown.go:1433-1455): a
    /// whitespace-only source renders NOTHING (the paid gap credit may remain
    /// consumed); otherwise the block rides `begin_block`/`end_block` with every row
    /// prefixed by the two-space `MATH_INDENT`. The body transform is the mathtext 2D layout,
    /// which degrades to the cleaned linear source when the formula cannot be laid out; either
    /// way the rows print in normal color (never dim: dim is decoration-only).
    fn flush_math(&mut self) {
        if let Some(mut v) = self.math_view.take() {
            v.close();
        }
        let lines = std::mem::take(&mut self.math_lines);
        self.in_math = false;
        let src = lines.join("\n");
        if src.trim().is_empty() {
            return; // an empty $$ block renders nothing (mirrors flush_quote)
        }
        self.begin_block();
        let width = self.term_width().saturating_sub(MATH_INDENT.len());
        let rows = self.math_renderer.render_2d(&src, width);
        let mut out = String::new();
        for r in &rows {
            out.push_str(MATH_INDENT);
            out.push_str(r);
            out.push('\n');
        }
        self.sink.write(&out);
        self.end_block();
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
