//! The self-built composer (`TUI_DESIGN` §5; spike G3): grapheme cursor, CJK columns via
//! the `crate::text::width` ruler, soft wrap at the full width (no prompt glyph), 1..=5 rows with a derived
//! scroll viewport, real-cursor export, the ↑ history ring, and the completion-cycle
//! state the suggest module reads.
//!
//! Layout is computed per call from the width the frame passes in (`rows`/`cursor_pos`)
//! — the composer stores no width, so a resize needs no notification (Go called
//! `ta.SetWidth`; here the next draw simply re-wraps). The height follows the wrapped
//! row count — newline breaks and soft wraps alike — clamped to `MAX_COMPOSER_ROWS`
//! (model.go resizeComposer; a multi-line draft counts its wrapped rows, not its logical
//! lines — `DIVERGENCES.md` X-65).

use crate::text::width::{graphemes, str_width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::editor::Editor;
use crate::ui::runtime::event_loop::MAX_COMPOSER_ROWS;

/// The composer state (model.go `ta` + the history/suggest model fields — the loop
/// model is WP44-frozen, so the WP46 state lives here).
pub(crate) struct Composer {
    /// The raw draft and its cursor; paste tags stay collapsed in here (model.go:84-88).
    editor: Editor,
    /// Submitted and queued inputs, ↑/↓ navigable (model.go:74-76).
    history: Vec<String>,
    /// `== history.len()` when not navigating.
    hist_idx: usize,
    /// Draft saved when history navigation starts.
    hist_draft: String,
    /// Completion cycle: the line as typed when the cycle started — the first Tab, ↑/↓ or click
    /// on the list (`""` = not cycling — model.go:81; the list filters by it).
    pub(crate) suggestion_base: String,
    /// Completion cycle position: the highlighted match (`None` until the cycle lands one).
    pub(crate) suggestion_index: Option<usize>,
    /// The candidate list's first shown item (its scroll window; `suggest::frame_list`).
    pub(crate) suggestion_top: usize,
}

/// Content columns: the whole width — the composer has no prompt glyph, the text starts at column 0.
fn content_width(width: u16) -> usize {
    usize::from(width).max(1)
}

/// Hard-wraps `value` into display rows at `w` content columns: byte spans per row,
/// newline separators excluded, a wide glyph never split (spike G1/G3 math).
fn wrap_spans(value: &str, w: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut row_start = 0usize;
    let mut curw = 0usize;
    let mut off = 0usize;
    for g in graphemes(value) {
        if g == "\n" {
            spans.push((row_start, off));
            row_start = off + g.len();
            curw = 0;
            off += g.len();
            continue;
        }
        let gw = str_width(g).max(1);
        if curw + gw > w && curw > 0 {
            spans.push((row_start, off));
            row_start = off;
            curw = 0;
        }
        curw += gw;
        off += g.len();
    }
    spans.push((row_start, value.len()));
    spans
}

impl Composer {
    /// An empty single-row composer.
    pub(crate) fn new() -> Self {
        Self {
            editor: Editor::new(),
            history: Vec::new(),
            hist_idx: 0,
            hist_draft: String::new(),
            suggestion_base: String::new(),
            suggestion_index: None,
            suggestion_top: 0,
        }
    }

    /// The raw draft (paste tags kept — expansion happens in `paste::make_input`).
    pub(crate) fn value(&self) -> &str {
        self.editor.value()
    }

    /// Replaces the draft; cursor moves to the end (Go setDraft shape).
    pub(crate) fn set_value(&mut self, s: &str) {
        self.editor.set_value(s);
    }

    /// Moves the cursor to the end of the draft.
    pub(crate) fn move_to_end(&mut self) {
        self.editor.move_to_end();
    }

    /// Clears the draft back to one empty row — the Enter collapse (sheds BOTTOM
    /// rows; model.go:475-476). History, pastes and the cycle state are untouched.
    pub(crate) fn reset(&mut self) {
        self.editor.clear();
    }

    /// Inserts text at the cursor (paste tags, verbatim single-line pastes, and the
    /// newline keys — `keys.rs`).
    pub(crate) fn insert_str(&mut self, s: &str) {
        self.editor.insert_str(s);
    }

    /// Whether the draft is blank (whitespace only) — the ↑ queue-pop gate.
    pub(crate) fn is_blank(&self) -> bool {
        self.editor.value().trim().is_empty()
    }

    /// The height the frame renders: the wrapped row count, newline breaks included,
    /// clamped to 1..=`MAX_COMPOSER_ROWS` (model.go resizeComposer). A long logical line
    /// inside a multi-line draft grows the box by every row it wraps to (X-65).
    fn effective_height(total_rows: usize) -> usize {
        total_rows.clamp(1, MAX_COMPOSER_ROWS)
    }

    /// The cursor's (display row, display column) under the same wrap walk as
    /// [`wrap_spans`] — recorded BEFORE the cluster at the cursor is placed, so a
    /// cursor on a wrap boundary reads as the end of the previous row (spike G3).
    fn cursor_rowcol(&self, w: usize) -> (usize, usize) {
        let mut row = 0usize;
        let mut curw = 0usize;
        let mut off = 0usize;
        for g in graphemes(self.editor.value()) {
            if off == self.editor.cursor() {
                return (row, curw);
            }
            if g == "\n" {
                row += 1;
                curw = 0;
                off += g.len();
                continue;
            }
            let gw = str_width(g).max(1);
            if curw + gw > w && curw > 0 {
                row += 1;
                curw = 0;
            }
            curw += gw;
            off += g.len();
        }
        (row, curw)
    }

    /// The derived scroll offset: the viewport shows `h` rows and keeps the cursor
    /// visible, preferring the top. When the content fits, the offset is 0 — which IS
    /// the Go growth-snap law's observable result (the viewport never stays scrolled
    /// after growth; model.go resizeComposer — see DEVIATIONS3).
    fn scroll_offset(cursor_row: usize, h: usize) -> usize {
        cursor_row.saturating_sub(h.saturating_sub(1))
    }

    /// The composer rows: the draft's display rows from column 0, no prompt glyph and no
    /// continuation indent, exactly `effective_height` rows (short content pads with empty rows).
    pub(crate) fn rows(&self, width: u16) -> Vec<String> {
        let w = content_width(width);
        let spans = wrap_spans(self.editor.value(), w);
        let h = Self::effective_height(spans.len());
        let (cursor_row, _) = self.cursor_rowcol(w);
        let offset = Self::scroll_offset(cursor_row, h);
        (0..h)
            .map(|vi| {
                spans
                    .get(offset + vi)
                    .map_or("", |&(s, e)| &self.editor.value()[s..e])
                    .to_owned()
            })
            .collect()
    }

    /// The real-cursor target within the composer block: (display column of the text,
    /// visible row). CJK-aware via the ruler — the IME anchor
    /// (spike G3: `"中文测试"` → column 8).
    pub(crate) fn cursor_pos(&self, width: u16) -> (u16, u16) {
        let w = content_width(width);
        let total = wrap_spans(self.editor.value(), w).len();
        let h = Self::effective_height(total);
        let (cursor_row, cursor_col) = self.cursor_rowcol(w);
        let offset = Self::scroll_offset(cursor_row, h);
        (
            u16::try_from(cursor_col).unwrap_or(u16::MAX),
            u16::try_from(cursor_row - offset).unwrap_or(u16::MAX),
        )
    }

    /// A click on the composer block: the cursor goes to the grapheme under display column `col`
    /// of visible row `row`, or to the end of that row when
    /// the click is past it (a row past the draft's last: the draft's end). A wide glyph's second
    /// column is that glyph.
    pub(crate) fn click(&mut self, col: u16, row: usize, width: u16) {
        let w = content_width(width);
        let spans = wrap_spans(self.editor.value(), w);
        let h = Self::effective_height(spans.len());
        let (cursor_row, _) = self.cursor_rowcol(w);
        let offset = Self::scroll_offset(cursor_row, h);
        let Some(&(start, end)) = spans.get(offset + row) else {
            self.editor.move_to_end();
            return;
        };
        let col = usize::from(col);
        let mut walked = 0usize;
        let mut off = start;
        for g in graphemes(&self.editor.value()[start..end]) {
            let gw = str_width(g).max(1);
            if walked + gw > col {
                break;
            }
            walked += gw;
            off += g.len();
        }
        self.editor.set_cursor(off);
        self.end_history_nav();
    }

    // ---- history ring (model.go:497-524) ----

    /// Records a submitted/queued input and resets navigation; immediate duplicates
    /// collapse, like readline (model.go pushHistory).
    pub(crate) fn push_history(&mut self, text: &str) {
        if self.history.last().is_some_and(|last| last == text) {
            self.hist_idx = self.history.len();
            return;
        }
        self.history.push(text.to_owned());
        self.hist_idx = self.history.len();
    }

    /// The display row holding the cursor, with the row count, at `width`: the first span
    /// that contains it, ends inclusive. So a cursor at a row's end — a wrap boundary or a
    /// newline byte — belongs to that row, one just past a newline to the next, and an empty
    /// logical line's zero-width span holds the cursor sitting on it.
    fn cursor_row(&self, width: u16) -> (usize, usize) {
        let spans = wrap_spans(self.editor.value(), content_width(width));
        let cursor = self.editor.cursor();
        let last = spans.len().saturating_sub(1);
        let row = spans
            .iter()
            .position(|&(start, end)| (start..=end).contains(&cursor))
            .unwrap_or(last);
        (row, spans.len())
    }

    /// Whether the cursor is on the top display row, where ↑ recalls history.
    pub(crate) fn at_first_row(&self, width: u16) -> bool {
        self.cursor_row(width).0 == 0
    }

    /// Whether the cursor is on the bottom display row, where ↓ walks history forward.
    pub(crate) fn at_last_row(&self, width: u16) -> bool {
        let (row, rows) = self.cursor_row(width);
        row + 1 == rows
    }

    /// Whether ↓ still has somewhere to go (else the key falls to the edit set).
    pub(crate) fn history_can_forward(&self) -> bool {
        self.hist_idx < self.history.len()
    }

    /// ↑: saves the draft on the first step, then walks newest-first
    /// (model.go:453-462).
    pub(crate) fn history_up(&mut self) {
        if self.hist_idx == self.history.len() {
            self.hist_draft = self.editor.value().to_owned();
        }
        if self.hist_idx > 0 {
            self.hist_idx -= 1;
            if let Some(entry) = self.history.get(self.hist_idx).cloned() {
                self.set_value(&entry);
            }
        }
    }

    /// ↓: walks forward, restoring the saved draft past the newest entry
    /// (model.go:463-471).
    pub(crate) fn history_down(&mut self) {
        if self.hist_idx >= self.history.len() {
            return;
        }
        self.hist_idx += 1;
        if self.hist_idx == self.history.len() {
            let draft = self.hist_draft.clone();
            self.set_value(&draft);
        } else if let Some(entry) = self.history.get(self.hist_idx).cloned() {
            self.set_value(&entry);
        }
    }

    /// Any edit leaves history navigation (model.go:493); a row move is not one (`keys.rs`).
    pub(crate) fn end_history_nav(&mut self) {
        self.hist_idx = self.history.len();
    }

    // ---- the enumerated edit set (key-table rows 8–9, TUI_CONTRACTS §6) ----

    /// The editing set is the shared [`Editor`]'s (`on_key`); what stays here is what only a
    /// multi-row composer has — ↑/↓ as display-row cursor movement, reached only when history
    /// navigation did not claim the arrows.
    pub(crate) fn handle_edit_key(&mut self, key: &KeyEvent, width: u16) {
        if !self.editor.on_key(key) && !key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Up => self.move_cursor_row(false, width),
                KeyCode::Down => self.move_cursor_row(true, width),
                _ => {}
            }
        }
    }

    /// Moves the cursor one display row up/down, holding the display column
    /// (the textarea CursorUp/Down twin for multi-row drafts). A no-op past either
    /// edge.
    fn move_cursor_row(&mut self, down: bool, width: u16) {
        let w = content_width(width);
        let spans = wrap_spans(self.editor.value(), w);
        let (cursor_row, cursor_col) = self.cursor_rowcol(w);
        let target = if down {
            cursor_row + 1
        } else {
            cursor_row.wrapping_sub(1)
        };
        let Some(&(start, end)) = spans.get(target) else {
            return;
        };
        let mut walked = 0usize;
        let mut off = start;
        for g in graphemes(&self.editor.value()[start..end]) {
            let gw = str_width(g).max(1);
            if walked + gw > cursor_col {
                break;
            }
            walked += gw;
            off += g.len();
        }
        self.editor.set_cursor(off);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! WP46 composer suite — `TestWrappedComposerLayout`, the crown-jewel FULL frame
    //! golden (owned HERE; WP44's `frame_goldens.rs` never renders composer content), the
    //! history-navigation and paste-tag suites, and the composer CJK cursor-column /
    //! wrap-math units (spike G3 vectors).
    //!
    //! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
    //! in-file (formerly a `#[path]`-mounted `tests/composer.rs` of the terminal crate; merged 2026-09-02).

    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;
    use crate::ui::input::composer::Composer;

    use crate::ui::facade::{Panel, StatusData, Suggestion, TabbedSpec};
    use crate::ui::runtime::msgs::UiMsg;
    use crate::ui::testutil::{down, enter, key, plain, test_model, type_text, up};
    use crossterm::event::KeyCode;

    fn find(rows: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
        rows.iter().position(|r| pred(r))
    }

    fn separator_indices(rows: &[String]) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.starts_with(&crate::ui::render::frame::SEPARATOR_GLYPH.repeat(3)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Renders raw ANSI rows through `ansi_to_spans` into a ratatui `TestBackend` and
    /// reads the cell grid back as plain strings — the L2 geometry instrument.
    fn render_plain(rows: &[String], w: u16, h: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let lines: Vec<ratatui::text::Line<'static>> = rows
            .iter()
            .map(|r| crate::ui::render::spans::ansi_to_spans(r))
            .collect();
        term.draw(|f| {
            f.render_widget(
                ratatui::widgets::Paragraph::new(ratatui::text::Text::from(lines)),
                f.area(),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let width = usize::from(buf.area.width);
        buf.content
            .chunks(width)
            .map(|cells| {
                let mut s = String::new();
                let mut skip = 0usize;
                for c in cells {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let sym = c.symbol();
                    s.push_str(sym);
                    skip = str_width(sym).saturating_sub(1);
                }
                s.trim_end().to_owned()
            })
            .collect()
    }

    /// THE CROWN JEWEL: full layout order with real composer content — queue < sep1 <
    /// composer < sep2 < status, exactly 2 separators, status the LAST frame row; an open
    /// surface replaces the status row below the composer and close restores it;
    /// the completion list renders between sep1 and a blank gap over the composer; the status row
    /// keeps its content while an item is highlighted — through both the raw rows and the
    /// `TestBackend` cell grid.
    #[test]
    fn the_wrapped_composer_layout_is_the_full_frame_golden() {
        let mut m = test_model();
        m.apply(UiMsg::Status(StatusData {
            model: "gpt-4o".to_owned(),
            ctx_used: 1,
            ctx_window: 100,
            ..StatusData::default()
        }));
        type_text(&mut m, "queued");
        enter(&mut m); // no waiter → queue row above the top separator
        type_text(&mut m, "draft"); // the composer row: no prompt glyph, the text at column 0

        let rows = plain(&mut m);
        let seps = separator_indices(&rows);
        assert_eq!(seps.len(), 2, "want exactly 2 separators:\n{rows:#?}");
        let queue = find(&rows, |r| r.contains("» queued")).expect("queue row");
        let composer_row = find(&rows, |r| r == "draft").expect("composer row");
        let status = find(&rows, |r| r.contains("gpt-4o · ")).expect("status row");
        assert!(
            queue < seps[0] && seps[0] < composer_row && composer_row < seps[1] && seps[1] < status,
            "layout order wrong (queue={queue} sep={seps:?} composer={composer_row} status={status}):\n{rows:#?}"
        );
        assert_eq!(status, rows.len() - 1, "status not the frame's last row");

        // The TestBackend cell grid agrees on the order.
        let v = m.frame_view();
        let h = u16::try_from(v.rows.len()).unwrap();
        let grid = render_plain(&v.rows, 80, h);
        let g_composer = find(&grid, |r| r == "draft").expect("composer in grid");
        let g_status = find(&grid, |r| r.contains("gpt-4o")).expect("status in grid");
        assert!(g_composer < g_status, "grid order wrong:\n{grid:#?}");

        // A surface replaces the status row, below the composer; ESC restores it.
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::TabbedOpen {
            spec: TabbedSpec {
                panels: vec![Panel::list("/model".to_owned(), vec!["a".to_owned()])],
                ..TabbedSpec::default()
            },
            reply: tx,
        });
        let rows = plain(&mut m);
        assert!(
            !rows.iter().any(|r| r.contains("gpt-4o · ")),
            "status visible while a surface is open:\n{rows:#?}"
        );
        let composer_row = find(&rows, |r| r == "draft").expect("composer row");
        let surface_row = find(&rows, |r| r.contains("/model")).expect("surface row");
        assert!(composer_row < surface_row, "surface not below the composer");
        m.handle_key(key(KeyCode::Esc));
        let res = rx.try_recv().expect("surface reply missing");
        assert!(res.cancelled, "esc should cancel the surface");
        assert!(
            plain(&mut m).iter().any(|r| r.contains("gpt-4o · ")),
            "status not restored after the surface closed"
        );

        // The completion list sits under the upper separator, one blank gap row over the composer,
        // one row per item with its description beside it, and the status row keeps its slot
        // throughout.
        m.apply(UiMsg::Commands(vec![Suggestion {
            value: "/model".to_owned(),
            label: String::new(),
            desc: "Pick a model".to_owned(),
        }]));
        m.composer.reset();
        type_text(&mut m, "/m");
        let rows = plain(&mut m);
        let seps = separator_indices(&rows);
        let item = find(&rows, |r| r == "  model  Pick a model").expect("list item row");
        let composer_row = find(&rows, |r| r == "/m").expect("composer row");
        let status = find(&rows, |r| r.contains("gpt-4o · ")).expect("status row");
        assert!(
            seps.len() == 2
                && seps[0] + 1 == item
                && rows[item + 1].is_empty()
                && item + 2 == composer_row
                && seps[1] + 1 == status,
            "the list must sit between the upper separator and the gap over the composer (item={item} sep={seps:?} status={status}):\n{rows:#?}"
        );

        // Selecting one highlights it; the status row stays where and what it was.
        m.handle_key(key(KeyCode::Tab));
        let rows = plain(&mut m);
        assert!(
            rows.iter().any(|r| r.trim_end() == "▸ model  Pick a model"),
            "{rows:#?}"
        );
        assert!(
            rows.last().is_some_and(|r| r.contains("gpt-4o · ")),
            "{rows:#?}"
        );
        assert_eq!(m.composer.value(), "/model", "tab must write the candidate");
    }

    /// The row boundaries: a row's end (newline byte or wrap boundary) is that row's, past a
    /// newline is the next one's, an empty line's zero-width span holds its own row. `width`
    /// 10 is 10 content columns.
    #[test]
    fn cursor_row_owns_each_rows_end() {
        let at = |value: &str, cursor: usize, width: u16| {
            let mut c = Composer::new();
            c.set_value(value);
            c.editor.set_cursor(cursor);
            c.cursor_row(width)
        };
        assert_eq!(at("", 0, 10), (0, 1));
        assert_eq!(at("abc", 0, 10), (0, 1));
        assert_eq!(at("abc", 3, 10), (0, 1));
        assert_eq!(at("a\nb", 1, 10), (0, 2), "on the newline byte");
        assert_eq!(at("a\nb", 2, 10), (1, 2), "just after the newline");
        assert_eq!(at("a\n\nb", 2, 10), (1, 3), "on the empty line");
        assert_eq!(at("a\n\nb", 3, 10), (2, 3), "after the empty line");
        assert_eq!(at("a\n", 2, 10), (1, 2), "the empty last line");
        let long = "x".repeat(25);
        assert_eq!(at(&long, 10, 10), (0, 3), "a wrap boundary ends the row");
        assert_eq!(at(&long, 11, 10), (1, 3));
        assert_eq!(at(&long, 20, 10), (1, 3));
        assert_eq!(at(&long, 21, 10), (2, 3));
        assert_eq!(at("中中中", 6, 4), (0, 2), "two wide glyphs fill 4 columns");
        assert_eq!(
            at("中中中", 6, 3),
            (1, 3),
            "3 columns hold one, its gap unused"
        );
    }

    /// The row the edges read is the row the cursor renders on and `move_cursor_row` moves
    /// from; if they disagreed, an arrow could land on neither a move nor a recall.
    #[test]
    fn cursor_row_agrees_with_the_rendered_row() {
        for value in [
            "",
            "abc",
            "a\nb",
            "\n",
            "\n\n",
            "a\n\nb\n",
            &"x".repeat(25),
            &format!("{}\n{}", "y".repeat(10), "z".repeat(21)),
            "中文测试中文测试",
            "a中b文c\n中",
        ] {
            let mut c = Composer::new();
            c.set_value(value);
            let offsets = value.char_indices().map(|(i, _)| i).chain([value.len()]);
            for cursor in offsets {
                c.editor.set_cursor(cursor);
                assert_eq!(
                    c.cursor_row(12).0,
                    c.cursor_rowcol(super::content_width(12)).0,
                    "{value:?} at {cursor}"
                );
            }
        }
    }

    /// ↑ recalls newest-first including queued items; ↓ walks back to the saved draft.
    #[test]
    fn up_and_down_walk_the_history_and_keep_the_draft() {
        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        type_text(&mut m, "one");
        enter(&mut m);
        let got = rx.try_recv().expect("waiter not served").expect("read err");
        assert_eq!(got.text, "one");
        type_text(&mut m, "two");
        enter(&mut m); // no waiter → queued (history too)

        type_text(&mut m, "dra");
        up(&mut m);
        assert_eq!(m.composer.value(), "two", "↑ must recall the newest entry");
        up(&mut m);
        assert_eq!(m.composer.value(), "one", "↑↑ must walk further back");
        down(&mut m);
        down(&mut m);
        assert_eq!(m.composer.value(), "dra", "↓↓ must restore the saved draft");
    }

    /// A multi-line paste collapses to a `[#N …]` tag in the composer while BOTH sides of
    /// the submitted input carry real content — Text in full, Display for the echo;
    /// single-line pastes insert verbatim.
    #[test]
    fn a_multi_line_paste_becomes_a_tag_and_submits_its_content() {
        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::input::paste::on_paste(&mut m, "line1\nline2\nline3");
        assert!(
            m.composer.value().starts_with("[#1 line1… 3 lines]"),
            "composer = {:?}, want a paste tag",
            m.composer.value()
        );
        enter(&mut m);
        let r = rx.try_recv().expect("waiter not served").expect("read err");
        assert_eq!(r.text, "line1\nline2\nline3", "Text must expand the paste");
        assert_eq!(
            r.display, "line1\nline2\nline3",
            "Display must echo expanded"
        );

        // Single-line pastes insert verbatim.
        crate::ui::input::paste::on_paste(&mut m, "inline");
        assert_eq!(m.composer.value(), "inline");
    }

    /// A long paste echoes bounded — head plus a count — so one paste cannot bury the
    /// reply, while the model still receives every line.
    #[test]
    fn a_long_paste_echo_is_truncated() {
        let content = (1..=60)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::input::paste::on_paste(&mut m, &content);
        type_text(&mut m, " please review");
        enter(&mut m);
        let r = rx.try_recv().expect("waiter not served").expect("read err");

        assert!(r.text.contains("line60"), "Text must carry the whole paste");
        let echo: Vec<&str> = r.display.split('\n').collect();
        assert_eq!(
            echo.len(),
            crate::ui::input::paste::PASTE_ECHO_MAX_LINES + 1,
            "echo rows = {}, want {} head rows + the count",
            echo.len(),
            crate::ui::input::paste::PASTE_ECHO_MAX_LINES + 1
        );
        assert_eq!(
            echo[crate::ui::input::paste::PASTE_ECHO_MAX_LINES],
            "… +40 more lines please review",
            "last echo row mismatch"
        );
        assert!(!r.display.contains("line60"), "echo must stop at the cap");
    }

    /// ↑ recall restores the composer's own text — the TAG, not the blob — and a
    /// re-submit expands it again from the store.
    #[test]
    fn recalling_a_pasted_entry_keeps_its_tag() {
        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::input::paste::on_paste(&mut m, "alpha\nbeta");
        enter(&mut m);
        rx.try_recv().expect("waiter not served").expect("read err");

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 2, reply: tx });
        up(&mut m);
        assert!(
            m.composer.value().starts_with("[#1"),
            "recalled composer = {:?}, want the tag (an editable stand-in)",
            m.composer.value()
        );
        enter(&mut m);
        let r = rx.try_recv().expect("waiter not served").expect("read err");
        assert_eq!(
            r.text, "alpha\nbeta",
            "re-submitted Text must expand the paste again"
        );
    }

    /// Spike G3 vectors: CJK cursor columns through the real frame — `"中文测试"` puts
    /// the REAL cursor at column 8 (4×2 CJK cells from column 0 — no prompt glyph); Backspace deletes one
    /// grapheme, double-width aware.
    // Established by the ratatui inline spike (tag go-final: rust/spikes/ratatui-inline).
    #[test]
    fn composer_cjk_cursor_columns() {
        let mut m = test_model();
        type_text(&mut m, "中文测试");
        assert_eq!(m.composer.cursor_pos(80), (8, 0), "CJK cursor column");
        let v = m.frame_view();
        let rows: Vec<String> = v.rows.iter().map(|r| strip_sgr(r)).collect();
        let composer_row = find(&rows, |r| r == "中文测试").expect("composer row at column 0");
        assert_eq!(
            v.cursor,
            Some((8, u16::try_from(composer_row).unwrap())),
            "real cursor must sit right of the 8 CJK columns on the composer row"
        );

        m.handle_key(key(KeyCode::Backspace));
        assert_eq!(m.composer.value(), "中文测", "grapheme backspace");
        assert_eq!(m.composer.cursor_pos(80), (6, 0));
    }

    /// Wrap math: soft wrap at the full width in display columns, a wide rune never split; a
    /// single logical line follows its wrapped height 1..=5; scrolled viewports snap back
    /// once the content fits; Enter collapse resets to one row.
    #[test]
    fn composer_wrap_growth_scroll_and_collapse() {
        let mut c = Composer::new();

        // width 8 → 8 content columns: 5 CJK (10 cols) wrap 4+1, never splitting a rune; both
        // rows start at column 0.
        c.set_value("中中中中中");
        assert_eq!(c.rows(8), ["中中中中", "中"]);

        // width 7: only 3 CJK (6 cols) fit a row — a wide rune never straddles the boundary.
        assert_eq!(c.rows(7), ["中中中", "中中"]);

        // At 80 columns the text has all 80: 80 x's are one row, the 81st wraps.
        let mut c = Composer::new();
        c.set_value(&"x".repeat(80));
        assert_eq!(c.rows(80).len(), 1, "80 columns of text fill one row");
        c.set_value(&"x".repeat(200)); // 200 cols at 80 → 3 rows
        assert_eq!(c.rows(80).len(), 3, "auto growth follows the wrap");

        // …capped at 5 rows with the viewport scrolled to keep the cursor visible.
        let mut c = Composer::new();
        let draft: String = ('a'..='g').map(|ch| ch.to_string().repeat(8)).collect();
        c.set_value(&draft); // width 8 → 7 wrapped rows a…g
        let rows = c.rows(8);
        assert_eq!(rows.len(), 5, "growth cap");
        assert_eq!(
            rows[0], "cccccccc",
            "cursor at the end scrolls the top rows out"
        );
        let (x, y) = c.cursor_pos(8);
        assert_eq!((x, y), (8, 4), "cursor pinned to the last visible row");

        // Home snaps the viewport back to the top (the derived scroll-snap law).
        c.handle_edit_key(&key(KeyCode::Home), 8);
        let rows = c.rows(8);
        assert_eq!(rows[0], "aaaaaaaa", "viewport must snap back to the top");
        assert_eq!(c.cursor_pos(8), (0, 0));

        // Enter collapse sheds the bottom rows back to one empty row.
        c.reset();
        assert_eq!(c.rows(8), [""]);
    }

    /// A multi-line draft grows by its WRAPPED rows, not its logical lines (X-65): a 200-column
    /// line plus a newline plus `b` is 3 + 1 rows at width 80 — typed, set, or folded back.
    #[test]
    fn a_multi_line_draft_grows_by_its_wrapped_rows() {
        let long = "x".repeat(200); // 200 cols at 80 content cols → 3 rows

        let mut m = test_model();
        type_text(&mut m, &long);
        m.handle_key(crate::ui::testutil::ctrl('j'));
        type_text(&mut m, "b");
        let rows = m.composer.rows(80);
        assert_eq!(rows.len(), 4, "3 wrapped rows + 1: {rows:?}");
        assert_eq!(rows[0], "x".repeat(80), "the whole draft fits: no scroll");
        assert_eq!(rows[3], "b", "a continuation row starts at column 0 too");
        assert_eq!(m.composer.cursor_pos(80), (1, 3));

        let mut c = Composer::new();
        c.set_value(&format!("{long}\nb"));
        assert_eq!(c.rows(80).len(), 4, "set_value: same height");

        // The fold-back: an interrupt joins a long queued item into the draft.
        let mut m = test_model();
        let turn = tokio_util::sync::CancellationToken::new();
        m.apply(UiMsg::ScopePush(turn));
        type_text(&mut m, &long);
        enter(&mut m); // a turn is running and no reader is parked → queued
        type_text(&mut m, "b");
        m.handle_key(key(KeyCode::Esc));
        assert_eq!(m.composer.value(), format!("{long}\nb"));
        assert_eq!(m.composer.rows(80).len(), 4, "fold-back: same height");

        // Still capped: a sixth row scrolls.
        let mut c = Composer::new();
        c.set_value(&format!("{long}\n{long}"));
        assert_eq!(c.rows(80).len(), 5, "growth cap");
    }

    /// A paste that arrives while a surface is open belongs to the surface's focused
    /// input field, not to the composer's `[#N …]` tag store — a `/model` manual-input
    /// field or an Ask "Other…" editor must be pasteable. The composer's draft is
    /// untouched, and closing the surface leaves it exactly as it was.
    #[test]
    fn paste_while_a_surface_is_open_lands_in_the_field() {
        let mut m = test_model();
        type_text(&mut m, "draft");

        let (tx, _rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::TabbedOpen {
            spec: TabbedSpec {
                panels: vec![
                    Panel::input("Model".to_owned(), String::new(), "model name".to_owned())
                        .with_input_width(20),
                ],
                ..TabbedSpec::default()
            },
            reply: tx,
        });

        m.route_paste("gpt-5\nmini");
        let s = m.surface.as_ref().expect("surface open");
        assert_eq!(
            s.st.slots[0].state.input.value(),
            "gpt-5 mini",
            "the paste must land in the focused field, flattened"
        );
        assert_eq!(
            m.composer.value(),
            "draft",
            "the composer draft must not absorb a surface paste"
        );
        assert!(
            !m.composer.value().contains("[#"),
            "no paste tag may be minted while a surface owns the input"
        );
    }

    /// The real cursor a surface input field exports is in SURFACE-BLOCK coordinates; the
    /// frame offsets it by everything between the composer's first row and the surface —
    /// the composer's own rows and the lower separator (no completion list is drawn
    /// under a surface). A stale MULTI-ROW draft under the surface must not lift the IME anchor.
    #[test]
    fn surface_field_cursor_offset_follows_the_composer_block() {
        let mut m = test_model();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::TabbedOpen {
            spec: TabbedSpec {
                panels: vec![
                    Panel::input("Model".to_owned(), String::new(), "model name".to_owned())
                        .with_input_width(20),
                ],
                ..TabbedSpec::default()
            },
            reply: tx,
        });

        // One-row composer: the cursor sits on the field's row inside the rendered frame.
        let v = m.frame_view();
        let (_, y1) = v.cursor.expect("the input field exports a real cursor");
        let rows: Vec<String> = v.rows.iter().map(|r| strip_sgr(r)).collect();
        let seps = separator_indices(&rows);
        assert_eq!(seps.len(), 2, "want exactly 2 separators:\n{rows:#?}");
        assert!(
            usize::from(y1) > seps[1],
            "the field cursor must sit inside the surface block (y={y1}, sep={seps:?})"
        );

        // Grow the draft to three rows: the anchor moves down by exactly two rows, i.e.
        // it tracks the composer block instead of assuming a one-row composer.
        m.composer.set_value(&"x".repeat(200)); // 200 cols at 80 content cols → 3 rows
        assert_eq!(
            m.composer.rows(80).len(),
            3,
            "precondition: 3 composer rows"
        );
        let v = m.frame_view();
        let (_, y3) = v.cursor.expect("the input field still exports a cursor");
        assert_eq!(y3, y1 + 2, "the anchor must follow the composer's height");

        // …and it still points at the same row of the rendered frame.
        let rows: Vec<String> = v.rows.iter().map(|r| strip_sgr(r)).collect();
        let seps = separator_indices(&rows);
        assert!(
            usize::from(y3) > seps[1] && usize::from(y3) < rows.len(),
            "cursor y={y3} outside the surface block (sep={seps:?}, rows={})",
            rows.len()
        );
    }
}
