//! The list block: its item parser and buffering state ([`ListBlock`], markdown.go:876-943)
//! and the renderList/buildList twin (markdown.go:1173-1265, Go's
//! lipgloss/list hand-ported) — verbatim markers left-padded to a common width so
//! ordered numbers right-align, hanging indent aligned with the item text, nested
//! sublists under their preceding item, loose lists joined with a blank between
//! top-level blocks, task glyphs served as recorded (`TUI_DESIGN` §7).

use crate::markdown::blocks::close_view;
use crate::markdown::blocks::math::display_open;
use crate::markdown::inline::{DIM, highlight_inline, is_list_line, split_list_marker};
use crate::markdown::{PreviewHandle, is_table_line};
use crate::text::width::str_width;

/// One parsed item of a buffering list block (Go listItem). Parsed by [`ListBlock`];
/// rendered by [`render_list`].
pub(crate) struct ListItem {
    /// Nesting depth derived from source indentation (clamped, never skips a level).
    pub(crate) level: usize,
    /// `"•"`, `"☐"`, `"☑"`, or the ordered token as written (`"3."`, `"7)"`).
    pub(crate) marker: String,
    /// Item text: first line + continuations (`""` = intra-item paragraph break).
    pub(crate) lines: Vec<String>,
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

/// A list block (markdown.go:876-943): the parsed items, whether a blank between items
/// made the list loose, and whether one blank is currently HELD — it ends the list if
/// another blank follows, makes the list loose if an item or continuation follows.
pub(crate) struct ListBlock {
    items: Vec<ListItem>,
    loose: bool,
    blank: bool,
    view: Option<Box<dyn PreviewHandle>>,
}

impl ListBlock {
    /// The live preview's label while the block buffers.
    pub(crate) const LABEL: &'static str = "rendering list…";

    /// A block opened by its first marker line, with the preview the Writer opened for it.
    pub(crate) fn open(line: &str, view: Option<Box<dyn PreviewHandle>>) -> Self {
        let mut list = Self {
            items: Vec::new(),
            loose: false,
            blank: false,
            view,
        };
        list.append_item(line);
        list.preview(line);
        list
    }

    /// Whether one blank is held: the Writer re-emits it after the block renders (the
    /// blank turned out to END the list).
    pub(crate) fn holds_blank(&self) -> bool {
        self.blank
    }

    /// Feeds one line; returns whether it was consumed. When it was not, the list ends:
    /// the caller flushes it (re-emitting the held blank) and processes the line normally
    /// (markdown.go:876-919).
    pub(crate) fn consume(&mut self, line: &str) -> bool {
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

    /// Closes the preview and renders the block, trailing newline included.
    pub(crate) fn render(mut self, color: bool) -> Option<String> {
        close_view(&mut self.view);
        if self.items.is_empty() {
            return None;
        }
        let rendered = render_list(&self.items, self.loose, color);
        Some(format!("{rendered}\n"))
    }
}

/// renderList twin (markdown.go:1173-1203). Each top-level item (with its nested
/// descendants) renders as its own block; blocks join with `"\n"` (tight) or `"\n\n"`
/// (loose — Go's thin manual layer over lipgloss/list, which has no inter-item
/// spacing). Enumerator alignment across blocks is kept by padding every top-level
/// marker to the same width first (`top_w`).
pub(crate) fn render_list(items: &[ListItem], loose: bool, color: bool) -> String {
    let top_w = items
        .iter()
        .filter(|it| it.level == 0)
        .map(|it| str_width(&it.marker))
        .max()
        .unwrap_or(0);

    let mut blocks: Vec<String> = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        while end < items.len() && items[end].level > 0 {
            end += 1;
        }
        blocks.push(build_list(&items[start..end], top_w, color).join("\n"));
        start = end;
    }

    let sep = if loose { "\n\n" } else { "\n" };
    blocks.join(sep)
}

/// buildList twin (markdown.go:1213-1254): one run of items whose first entry sets
/// the base level; deeper runs become nested sublists attached to the item before
/// them. Markers are the items' own (bullets, task glyphs, ordered numbers AS WRITTEN
/// — stock enumerators renumber from 1, so the recorded marker is served verbatim),
/// left-padded to the common width `w = max(min_width, widest marker)`; the dim span
/// carries the marker plus its one-space enumerator padding (Go `EnumeratorStyle`
/// PaddingRight(1)). Continuation lines and sublists hang by `w + 1` columns, aligned
/// with the item text.
fn build_list(items: &[ListItem], min_width: usize, color: bool) -> Vec<String> {
    let Some(first) = items.first() else {
        return Vec::new();
    };
    let base = first.level;
    let w = items
        .iter()
        .filter(|it| it.level == base)
        .map(|it| str_width(&it.marker))
        .max()
        .unwrap_or(0)
        .max(min_width);
    let indent = " ".repeat(w + 1);

    let mut rows: Vec<String> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if items[i].level > base {
            // A nested run merges into the item right before it, indented by the
            // parent's hanging indent; its own markers align among themselves.
            let mut j = i;
            while j < items.len() && items[j].level > base {
                j += 1;
            }
            for r in build_list(&items[i..j], 0, color) {
                rows.push(if r.is_empty() {
                    r
                } else {
                    format!("{indent}{r}")
                });
            }
            i = j;
            continue;
        }
        let it = &items[i];
        let pad = " ".repeat(w.saturating_sub(str_width(&it.marker)));
        for (k, line) in it.lines.iter().enumerate() {
            if k == 0 {
                rows.push(format!(
                    "{pad}{}{}",
                    DIM.render(&format!("{} ", it.marker), color),
                    highlight_inline(line, color)
                ));
            } else if line.is_empty() {
                rows.push(String::new()); // intra-item paragraph break (loose list)
            } else {
                rows.push(format!("{indent}{}", highlight_inline(line, color)));
            }
        }
        i += 1;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{indent_level, task_marker};

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
}
