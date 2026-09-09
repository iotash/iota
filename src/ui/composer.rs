//! The self-built composer (`TUI_DESIGN` §5; spike G3): grapheme cursor, CJK columns via
//! the `crate::text::width` ruler, soft wrap at width−2, 1..=5 rows with a derived
//! scroll viewport, real-cursor export, the ↑ history ring, and the completion-cycle
//! state the suggest module reads.
//!
//! Layout is computed per call from the width the frame passes in (`rows`/`cursor_pos`)
//! — the composer stores no width, so a resize needs no notification (Go called
//! `ta.SetWidth`; here the next draw simply re-wraps). The stored `height` matters only
//! for multi-logical-line drafts (the queue fold-back); a single logical line follows
//! its wrapped height, clamped to `MAX_COMPOSER_ROWS` (model.go resizeComposer).

use crate::text::width::{graphemes, str_width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::theme::{CYAN, RESET};

/// Composer growth cap (model.go:29; duplicated from the loop to keep this module
/// self-contained — the two consts are pinned equal by the WP46 tests).
const MAX_ROWS: usize = 5;

/// The composer state (model.go `ta` + the history/suggest model fields — the loop
/// model is WP44-frozen, so the WP46 state lives here).
pub(crate) struct Composer {
    /// The raw draft; paste tags stay collapsed in here (model.go:84-88).
    value: String,
    /// Byte offset of the cursor — always on a grapheme boundary.
    cursor: usize,
    /// Explicit height for multi-logical-line drafts (`fire_cancel`'s fold-back);
    /// single-line drafts derive their height from the wrap instead.
    height: usize,
    /// Submitted and queued inputs, ↑/↓ navigable (model.go:74-76).
    history: Vec<String>,
    /// `== history.len()` when not navigating.
    hist_idx: usize,
    /// Draft saved when history navigation starts.
    hist_draft: String,
    /// Completion cycle: the prefix captured at the FIRST Tab press (`""` = not
    /// cycling — model.go:81; read by `suggest::frame_slots`).
    pub(crate) suggestion_base: String,
    /// Completion cycle position (`None` until the first Tab lands a candidate).
    pub(crate) suggestion_index: Option<usize>,
}

/// Content columns available inside the 2-column prompt gutter.
fn content_width(width: u16) -> usize {
    usize::from(width).saturating_sub(2).max(1)
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
            value: String::new(),
            cursor: 0,
            height: 1,
            history: Vec::new(),
            hist_idx: 0,
            hist_draft: String::new(),
            suggestion_base: String::new(),
            suggestion_index: None,
        }
    }

    /// The raw draft (paste tags kept — expansion happens in `paste::make_input`).
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// Replaces the draft; cursor moves to the end (Go setDraft shape). Explicit
    /// height tracks the logical line count; a single line re-derives at render.
    pub(crate) fn set_value(&mut self, s: &str) {
        s.clone_into(&mut self.value);
        self.cursor = self.value.len();
        self.height = self.line_count().clamp(1, MAX_ROWS);
    }

    /// Moves the cursor to the end of the draft.
    pub(crate) fn move_to_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Clears the draft back to one empty row — the Enter collapse (sheds BOTTOM
    /// rows; model.go:475-476). History, pastes and the cycle state are untouched.
    pub(crate) fn reset(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.height = 1;
    }

    /// Inserts text at the cursor (paste tags and verbatim single-line pastes).
    pub(crate) fn insert_str(&mut self, s: &str) {
        self.value.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.height = self.line_count().clamp(1, MAX_ROWS);
    }

    /// Whether the draft is blank (whitespace only) — the ↑ queue-pop gate.
    pub(crate) fn is_blank(&self) -> bool {
        self.value.trim().is_empty()
    }

    /// Logical (newline-separated) line count.
    pub(crate) fn line_count(&self) -> usize {
        self.value.split('\n').count()
    }

    /// Sets the explicit height (multi-logical-line drafts keep it; `fire_cancel`).
    pub(crate) fn set_height(&mut self, rows: usize) {
        self.height = rows.clamp(1, MAX_ROWS);
    }

    /// The height the frame renders: a single logical line follows its wrapped height
    /// (model.go resizeComposer); multi-line drafts keep the explicit height.
    fn effective_height(&self, total_rows: usize) -> usize {
        if self.line_count() <= 1 {
            total_rows.clamp(1, MAX_ROWS)
        } else {
            self.height.clamp(1, MAX_ROWS)
        }
    }

    /// The cursor's (display row, display column) under the same wrap walk as
    /// [`wrap_spans`] — recorded BEFORE the cluster at the cursor is placed, so a
    /// cursor on a wrap boundary reads as the end of the previous row (spike G3).
    fn cursor_rowcol(&self, w: usize) -> (usize, usize) {
        let mut row = 0usize;
        let mut curw = 0usize;
        let mut off = 0usize;
        for g in graphemes(&self.value) {
            if off == self.cursor {
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

    /// The styled composer rows: cyan `"❯ "` on display row 0, two-space
    /// continuations on every other row (model.go:109-114), exactly
    /// `effective_height` rows (short content pads with bare continuation rows).
    pub(crate) fn rows(&self, width: u16) -> Vec<String> {
        let w = content_width(width);
        let spans = wrap_spans(&self.value, w);
        let h = self.effective_height(spans.len());
        let (cursor_row, _) = self.cursor_rowcol(w);
        let offset = Self::scroll_offset(cursor_row, h);
        (0..h)
            .map(|vi| {
                let gi = offset + vi;
                let text = spans.get(gi).map_or("", |&(s, e)| &self.value[s..e]);
                if gi == 0 {
                    format!("{CYAN}❯ {RESET}{text}")
                } else {
                    format!("  {text}")
                }
            })
            .collect()
    }

    /// The real-cursor target within the composer block: (display column including
    /// the 2-col prompt, visible row). CJK-aware via the ruler — the IME anchor
    /// (spike G3: `"中文测试"` → column 10).
    pub(crate) fn cursor_pos(&self, width: u16) -> (u16, u16) {
        let w = content_width(width);
        let total = wrap_spans(&self.value, w).len();
        let h = self.effective_height(total);
        let (cursor_row, cursor_col) = self.cursor_rowcol(w);
        let offset = Self::scroll_offset(cursor_row, h);
        (
            u16::try_from(2 + cursor_col).unwrap_or(u16::MAX),
            u16::try_from(cursor_row - offset).unwrap_or(u16::MAX),
        )
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

    /// History is ↑/↓ navigable only while the composer holds a single logical line
    /// that fits one wrapped row (model.go historyNavigable).
    pub(crate) fn history_navigable(&self, width: u16) -> bool {
        self.line_count() <= 1 && wrap_spans(&self.value, content_width(width)).len() <= 1
    }

    /// Whether ↓ still has somewhere to go (else the key falls to the edit set).
    pub(crate) fn history_can_forward(&self) -> bool {
        self.hist_idx < self.history.len()
    }

    /// ↑: saves the draft on the first step, then walks newest-first
    /// (model.go:453-462).
    pub(crate) fn history_up(&mut self) {
        if self.hist_idx == self.history.len() {
            self.hist_draft = self.value.clone();
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

    /// Any edit leaves history navigation (model.go:493).
    pub(crate) fn end_history_nav(&mut self) {
        self.hist_idx = self.history.len();
    }

    // ---- the enumerated edit set (key-table rows 8–9, TUI_CONTRACTS §6) ----

    /// The editing set: ←/→ by grapheme · Home/End · Backspace/Ctrl+H grapheme-back ·
    /// Delete · Ctrl+A/E home/end · Ctrl+B/F char · Ctrl+K kill-to-end · Ctrl+U
    /// kill-to-start · Ctrl+W word-back — line-scoped like the bubbles textarea it
    /// replaces — plus text insert and ↑/↓ display-row movement in multi-row drafts.
    pub(crate) fn handle_edit_key(&mut self, key: &KeyEvent, width: u16) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (ctrl, key.code) {
            (true, KeyCode::Char('a')) | (false, KeyCode::Home) => {
                self.cursor = self.line_start();
            }
            (true, KeyCode::Char('e')) | (false, KeyCode::End) => {
                self.cursor = self.line_end();
            }
            (true, KeyCode::Char('b')) | (false, KeyCode::Left) => {
                self.cursor = self.prev_boundary();
            }
            (true, KeyCode::Char('f')) | (false, KeyCode::Right) => {
                self.cursor = self.next_boundary();
            }
            (true, KeyCode::Char('k')) => {
                let end = self.line_end();
                self.value.drain(self.cursor..end);
            }
            (true, KeyCode::Char('u')) => {
                let start = self.line_start();
                self.value.drain(start..self.cursor);
                self.cursor = start;
            }
            (true, KeyCode::Char('w')) => self.delete_word_back(),
            (true, KeyCode::Char('h')) | (_, KeyCode::Backspace) => {
                let p = self.prev_boundary();
                self.value.drain(p..self.cursor);
                self.cursor = p;
            }
            (false, KeyCode::Delete) => {
                let n = self.next_boundary();
                self.value.drain(self.cursor..n);
            }
            // Reached only when history navigation did not claim the arrows: cursor
            // movement by display row within a multi-row draft.
            (false, KeyCode::Up) => self.move_cursor_row(false, width),
            (false, KeyCode::Down) => self.move_cursor_row(true, width),
            (false, KeyCode::Char(c)) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }
        self.height = self.line_count().clamp(1, MAX_ROWS);
    }

    /// Byte start of the logical line under the cursor.
    fn line_start(&self) -> usize {
        self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    /// Byte end of the logical line under the cursor (exclusive of the newline).
    fn line_end(&self) -> usize {
        self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |i| self.cursor + i)
    }

    /// Ctrl+W: skip whitespace back, then delete to the start of the previous word —
    /// never crossing the line start (bubbles deleteWordLeft parity).
    fn delete_word_back(&mut self) {
        let ls = self.line_start();
        let head = &self.value[ls..self.cursor];
        let trimmed = head.trim_end_matches(char::is_whitespace);
        let cut_rel = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let cut = ls + cut_rel;
        self.value.drain(cut..self.cursor);
        self.cursor = cut;
    }

    /// Moves the cursor one display row up/down, holding the display column
    /// (the textarea CursorUp/Down twin for multi-row drafts). A no-op past either
    /// edge.
    fn move_cursor_row(&mut self, down: bool, width: u16) {
        let w = content_width(width);
        let spans = wrap_spans(&self.value, w);
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
        for g in graphemes(&self.value[start..end]) {
            let gw = str_width(g).max(1);
            if walked + gw > cursor_col {
                break;
            }
            walked += gw;
            off += g.len();
        }
        self.cursor = off;
    }

    /// The grapheme boundary before the cursor.
    fn prev_boundary(&self) -> usize {
        let mut prev = 0;
        let mut at = 0;
        for g in graphemes(&self.value[..self.cursor]) {
            prev = at;
            at += g.len();
        }
        prev
    }

    /// The grapheme boundary after the cursor.
    fn next_boundary(&self) -> usize {
        graphemes(&self.value[self.cursor..])
            .next()
            .map_or(self.value.len(), |g| self.cursor + g.len())
    }
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! WP46 composer suite — `TestWrappedComposerLayout`, the crown-jewel FULL frame
    //! golden (owned HERE; WP44's `frame_goldens.rs` never renders composer content), the
    //! history-navigation and paste-tag suites, and the composer CJK cursor-column /
    //! wrap-math units (spike G3 vectors).
    //!
    //! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
    //! in-file (formerly a `#[path]`-mounted `tests/composer.rs` of the terminal crate; merged 2026-09-02).

    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex};

    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;
    use crate::ui::composer::Composer;
    use crate::ui::event_loop::{LoopShared, Model};
    use crate::ui::facade::{Panel, StatusData, Suggestion, TabbedSpec};
    use crate::ui::msgs::UiMsg;
    use crate::ui::region::{Emit, Region};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// A loop model over the test-seam region at 80×24 (Go `newTestModel`).
    fn test_model() -> Model {
        let width = Arc::new(AtomicU16::new(80));
        let height = Arc::new(AtomicU16::new(24));
        let region = Arc::new(Mutex::new(Region::new(
            Emit::Test(Box::new(|_, _| {})),
            Arc::clone(&width),
            Arc::clone(&height),
        )));
        Model::new(LoopShared {
            width,
            height,
            region,
        })
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(m: &mut Model, s: &str) {
        for ch in s.chars() {
            m.handle_key(key(KeyCode::Char(ch)));
        }
    }

    fn enter(m: &mut Model) {
        m.handle_key(key(KeyCode::Enter));
    }

    fn up(m: &mut Model) {
        m.handle_key(key(KeyCode::Up));
    }

    fn down(m: &mut Model) {
        m.handle_key(key(KeyCode::Down));
    }

    /// SGR-stripped frame rows (the Go `stripSGR(content(m))` instrument).
    fn plain(m: &mut Model) -> Vec<String> {
        m.frame_view().rows.iter().map(|r| strip_sgr(r)).collect()
    }

    fn find(rows: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
        rows.iter().position(|r| pred(r))
    }

    fn separator_indices(rows: &[String]) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.starts_with("───"))
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
            .map(|r| crate::ui::spans::ansi_to_spans(r))
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
    /// completion candidates render INSIDE the composer block (above sep2); the selected
    /// candidate's description takes the status slot — through both the raw rows and the
    /// `TestBackend` cell grid.
    // Go: model_test.go:1041
    #[test]
    fn test_wrapped_composer_layout() {
        let mut m = test_model();
        m.apply(UiMsg::Status(StatusData {
            model: "gpt-4o".to_owned(),
            ctx_used: 1,
            ctx_window: 100,
            ..StatusData::default()
        }));
        type_text(&mut m, "queued");
        enter(&mut m); // no waiter → queue row above the top separator

        let rows = plain(&mut m);
        let seps = separator_indices(&rows);
        assert_eq!(seps.len(), 2, "want exactly 2 separators:\n{rows:#?}");
        let queue = find(&rows, |r| r.contains("» queued")).expect("queue row");
        let composer_row = find(&rows, |r| r.contains('❯')).expect("composer row");
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
        let g_composer = find(&grid, |r| r.contains('❯')).expect("composer in grid");
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
        let composer_row = find(&rows, |r| r.contains('❯')).expect("composer row");
        let surface_row = find(&rows, |r| r.contains("/model")).expect("surface row");
        assert!(composer_row < surface_row, "surface not below the composer");
        m.handle_key(key(KeyCode::Esc));
        let res = rx.try_recv().expect("surface reply missing");
        assert!(res.cancelled, "esc should cancel the surface");
        assert!(
            plain(&mut m).iter().any(|r| r.contains("gpt-4o · ")),
            "status not restored after the surface closed"
        );

        // Completion candidates land INSIDE the composer block — above the lower
        // separator; the status row keeps its slot until a candidate is selected.
        m.apply(UiMsg::Commands(vec![Suggestion {
            value: "/model".to_owned(),
            label: String::new(),
            desc: "Pick a model".to_owned(),
        }]));
        type_text(&mut m, "/m");
        let rows = plain(&mut m);
        let seps = separator_indices(&rows);
        let cand = find(&rows, |r| r.contains("⎿ model")).expect("candidates row");
        let status = find(&rows, |r| r.contains("gpt-4o · ")).expect("status row");
        assert!(
            seps.len() == 2 && cand < seps[1] && seps[1] < status,
            "candidates not enclosed by the composer block (cand={cand} sep={seps:?} status={status}):\n{rows:#?}"
        );

        // Selecting one moves its description into the status row's slot.
        m.handle_key(key(KeyCode::Tab));
        let joined = plain(&mut m).join("\n");
        assert!(
            !joined.contains("gpt-4o · ") && joined.contains("Pick a model"),
            "the description should take the status row while cycling:\n{joined}"
        );
        assert_eq!(m.composer.value(), "/model", "tab must write the candidate");
    }

    /// ↑ recalls newest-first including queued items; ↓ walks back to the saved draft.
    // Go: model_test.go:646
    #[test]
    fn test_history_navigation() {
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
    // Go: model_test.go:710
    #[test]
    fn test_paste_tags() {
        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::paste::on_paste(&mut m, "line1\nline2\nline3");
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
        crate::ui::paste::on_paste(&mut m, "inline");
        assert_eq!(m.composer.value(), "inline");
    }

    /// A long paste echoes bounded — head plus a count — so one paste cannot bury the
    /// reply, while the model still receives every line.
    // Go: model_test.go:735
    #[test]
    fn test_paste_echo_truncates() {
        let content = (1..=60)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::paste::on_paste(&mut m, &content);
        type_text(&mut m, " please review");
        enter(&mut m);
        let r = rx.try_recv().expect("waiter not served").expect("read err");

        assert!(r.text.contains("line60"), "Text must carry the whole paste");
        let echo: Vec<&str> = r.display.split('\n').collect();
        assert_eq!(
            echo.len(),
            crate::ui::paste::PASTE_ECHO_MAX_LINES + 1,
            "echo rows = {}, want {} head rows + the count",
            echo.len(),
            crate::ui::paste::PASTE_ECHO_MAX_LINES + 1
        );
        assert_eq!(
            echo[crate::ui::paste::PASTE_ECHO_MAX_LINES],
            "… +40 more lines please review",
            "last echo row mismatch"
        );
        assert!(!r.display.contains("line60"), "echo must stop at the cap");
    }

    /// ↑ recall restores the composer's own text — the TAG, not the blob — and a
    /// re-submit expands it again from the store.
    // Go: model_test.go:768
    #[test]
    fn test_paste_history_recall_keeps_tag() {
        let mut m = test_model();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        m.apply(UiMsg::ReadReq { id: 1, reply: tx });
        crate::ui::paste::on_paste(&mut m, "alpha\nbeta");
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
    /// the REAL cursor at column 10 (2-col prompt + 4×2 CJK cells); Backspace deletes one
    /// grapheme, double-width aware.
    // Established by the ratatui inline spike (tag go-final: rust/spikes/ratatui-inline).
    #[test]
    fn composer_cjk_cursor_columns() {
        let mut m = test_model();
        type_text(&mut m, "中文测试");
        assert_eq!(m.composer.cursor_pos(80), (10, 0), "CJK cursor column");
        let v = m.frame_view();
        let rows: Vec<String> = v.rows.iter().map(|r| strip_sgr(r)).collect();
        let composer_row = find(&rows, |r| r.contains("❯ 中文测试")).expect("composer row");
        assert_eq!(
            v.cursor,
            Some((10, u16::try_from(composer_row).unwrap())),
            "real cursor must sit at prompt(2) + 8 CJK columns on the composer row"
        );

        m.handle_key(key(KeyCode::Backspace));
        assert_eq!(m.composer.value(), "中文测", "grapheme backspace");
        assert_eq!(m.composer.cursor_pos(80), (8, 0));
    }

    /// Wrap math: soft wrap at width−2 in display columns, a wide rune never split; a
    /// single logical line follows its wrapped height 1..=5; scrolled viewports snap back
    /// once the content fits; Enter collapse resets to one row.
    // Go: (composer wrap-law units — model.go resizeComposer; spike G3 wrap vectors)
    #[test]
    fn composer_wrap_growth_scroll_and_collapse() {
        let mut c = Composer::new();

        // width 10 → 8 content columns: 5 CJK (10 cols) wrap 4+1, never splitting a rune.
        c.set_value("中中中中中");
        assert_eq!(c.rows(10).len(), 2);
        let rows = c.rows(10);
        assert!(strip_sgr(&rows[0]).ends_with("中中中中"), "{rows:?}");
        assert_eq!(strip_sgr(&rows[1]), "  中", "{rows:?}");

        // width 9 → 7 content columns: only 3 CJK (6 cols) fit a row — a wide rune never
        // straddles the boundary.
        assert_eq!(c.rows(9).len(), 2);
        assert!(strip_sgr(&c.rows(9)[0]).ends_with("中中中"));

        // A long ASCII line grows the composer with its wrapped height…
        let mut c = Composer::new();
        c.set_value(&"x".repeat(200)); // 200 cols at 78 content cols → 3 rows
        assert_eq!(c.rows(80).len(), 3, "auto growth follows the wrap");

        // …capped at 5 rows with the viewport scrolled to keep the cursor visible.
        let mut c = Composer::new();
        c.set_value(&"y".repeat(7 * 8)); // width 10 → 8 content cols → 7 wrapped rows
        let rows = c.rows(10);
        assert_eq!(rows.len(), 5, "growth cap");
        assert!(
            !rows[0].contains('❯'),
            "cursor at the end must scroll the prompt row out: {rows:?}"
        );
        let (x, y) = c.cursor_pos(10);
        assert_eq!((x, y), (10, 4), "cursor pinned to the last visible row");

        // Home snaps the viewport back to the top (the derived scroll-snap law).
        c.handle_edit_key(&key(KeyCode::Home), 10);
        let rows = c.rows(10);
        assert!(
            rows[0].contains('❯'),
            "viewport must snap back to the top: {rows:?}"
        );
        assert_eq!(c.cursor_pos(10), (2, 0));

        // Enter collapse sheds the bottom rows back to one empty prompt row.
        c.reset();
        let rows = c.rows(10);
        assert_eq!(rows.len(), 1);
        assert_eq!(strip_sgr(&rows[0]), "❯ ");
    }

    /// A paste that arrives while a surface is open belongs to the surface's focused
    /// input field, not to the composer's `[#N …]` tag store — a `/model` manual-input
    /// field or an Ask "Other…" editor must be pasteable. The composer's draft is
    /// untouched, and closing the surface leaves it exactly as it was.
    // Go: internal/ui/model.go:1279 (tea.PasteMsg → m.surface.Paste when a surface is open)
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
    /// the composer's own rows, the candidates row when one renders, and the lower
    /// separator. A stale MULTI-ROW draft under the surface must not lift the IME anchor.
    // Go: internal/ui/model.go:1060-1076 (View tracks rowsAbove for the cursor)
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
        m.composer.set_value(&"x".repeat(200)); // 200 cols at 78 content cols → 3 rows
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
