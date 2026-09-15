//! List renderer: the renderList/buildList twin (markdown.go:1173-1265, Go's
//! lipgloss/list hand-ported) — verbatim markers left-padded to a common width so
//! ordered numbers right-align, hanging indent aligned with the item text, nested
//! sublists under their preceding item, loose lists joined with a blank between
//! top-level blocks, task glyphs served as recorded (`TUI_DESIGN` §7).

use crate::markdown::ListItem;
use crate::markdown::inline::{DIM, highlight_inline};
use crate::text::width::str_width;

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
