//! The in-composer completion (`TUI_DESIGN` §6; suggest.go until the full screen): the
//! `match_suggestions` whole-line prefix filter, the completion cycle, and the candidate LIST the
//! frame draws above the composer.
//!
//! The list is a single-select picker in the fzf / Claude Code shape, in two columns: the label,
//! then — at one column for every match, [`GAP`] past the longest label (capped at
//! [`LABEL_CAP`]) — its description, word-wrapped at `min(remaining width, DESC_MEASURE)` and cut
//! with `…` after [`DESC_LINES`] lines. The cycle is the one `Tab` has always driven: `Tab`
//! and `↓` step forward, `↑` back, a click picks an item, and every step writes the item's value
//! into the composer — so `Enter` commits the highlighted item by submitting the line, and `Esc`
//! puts back what was typed. The window over the items is in WHOLE items and keeps the highlighted
//! one fully visible; with no room for more it holds the highlighted item alone.

use crate::markdown::blocks::table::word_wrap_ansi;
use crate::text::ansi::truncate_ansi;
use crate::text::width::str_width;
use crate::ui::facade::Suggestion;

use super::composer::Composer;
use crate::ui::render::theme::{CYAN, FAINT, RESET};

/// A description's measure, in columns: 72, the classic line length of terminal prose (a commit
/// body, a man page). A wide terminal keeps a description a short block beside its label instead
/// of one long line.
pub(crate) const DESC_MEASURE: usize = 72;

/// The most lines a description takes; the last one ends in `…` when there was more.
pub(crate) const DESC_LINES: usize = 2;

/// Where a label starts: a one-column margin, then the pointer's two.
const LABEL_INDENT: usize = 3;

/// The widest the label column gets; a longer label is cut with `…`. Command and skill names are
/// short — 30 is generous.
pub(crate) const LABEL_CAP: usize = 30;

/// The space between the label column and the description column.
const GAP: usize = 2;

/// Filters the table by whole-line prefix (suggest.go matchSuggestions). Entries whose
/// value carries an argument (`"/skills brain-page"`) stay hidden until the line has a
/// space, so `"/skills"` offers the command itself rather than its whole catalog;
/// whole-line prefix matching keeps ordinary commands quiet after their arguments
/// start — nothing registered begins with `"/file some/path"`.
pub(crate) fn match_suggestions<'a>(commands: &'a [Suggestion], line: &str) -> Vec<&'a Suggestion> {
    if !line.starts_with('/') || line.contains('\n') {
        return Vec::new();
    }
    let typed_arg = line.contains(' ');
    commands
        .iter()
        .filter(|c| (typed_arg || !c.value.contains(' ')) && c.value.starts_with(line))
        .collect()
}

/// What the list filters by: the line as typed when the cycle started, else the line.
fn base(c: &Composer) -> &str {
    if c.suggestion_base.is_empty() {
        c.value()
    } else {
        &c.suggestion_base
    }
}

/// The matches the list shows for the composer's line.
pub(crate) fn matches_for<'a>(c: &Composer, commands: &'a [Suggestion]) -> Vec<&'a Suggestion> {
    match_suggestions(commands, base(c))
}

/// What a candidate shows: its explicit label, else the value with its leading slash dropped —
/// a candidate shows only the part the user has not already typed (suggest.go suggestLabel).
fn suggest_label(s: &Suggestion) -> &str {
    if s.label.is_empty() {
        s.value.strip_prefix('/').unwrap_or(&s.value)
    } else {
        &s.label
    }
}

/// One step of the cycle: captures the typed line as the base on the first step, moves the
/// highlight to `pick(current, matches)`, and writes that match's value into the composer.
/// `false` (nothing changed) when the line has no matches.
fn cycle(
    c: &mut Composer,
    commands: &[Suggestion],
    pick: impl FnOnce(Option<usize>, usize) -> usize,
) -> bool {
    let typed = base(c).to_owned();
    let ms = match_suggestions(commands, &typed);
    if ms.is_empty() {
        return false;
    }
    if c.suggestion_base.is_empty() {
        c.suggestion_base = typed;
        c.suggestion_index = None;
    }
    let i = pick(c.suggestion_index.filter(|&i| i < ms.len()), ms.len());
    c.suggestion_index = Some(i);
    let value = ms[i].value.clone();
    c.set_value(&value);
    true
}

/// Tab (key-table row 4): steps forward through the matches of the line typed before the FIRST
/// press, wrapping, writing each candidate's value straight into the composer; Enter is never
/// claimed (model.go:421-435). A Tab with no matches is consumed as a no-op.
pub(crate) fn tab_complete(c: &mut Composer, commands: &[Suggestion]) {
    cycle(c, commands, |i, n| i.map_or(0, |i| (i + 1) % n));
}

/// `↓` (`down`) / `↑` on the list: the highlight moves one ITEM, wrapping; from no highlight `↓`
/// lands on the first item and `↑` on the last. `false` when there is no list to move in.
pub(crate) fn step(c: &mut Composer, commands: &[Suggestion], down: bool) -> bool {
    cycle(c, commands, |i, n| match (i, down) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
    })
}

/// A click on the list's `item`: the highlight goes there.
pub(crate) fn choose(c: &mut Composer, commands: &[Suggestion], item: usize) -> bool {
    cycle(c, commands, |_, n| item.min(n - 1))
}

/// `Esc` during the cycle: the line goes back to what was typed and the highlight is dropped.
/// `false` (the key goes on) when there is no cycle to cancel.
pub(crate) fn cancel(c: &mut Composer) -> bool {
    if c.suggestion_base.is_empty() {
        return false;
    }
    let typed = std::mem::take(&mut c.suggestion_base);
    c.suggestion_index = None;
    c.set_value(&typed);
    true
}

/// The list as the frame draws it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ListView {
    /// The rows, top to bottom (raw ANSI, one visual line each).
    pub(crate) rows: Vec<String>,
    /// For each row, the match it belongs to — where a click lands.
    pub(crate) items: Vec<usize>,
}

/// The label column's width for these matches at `width`: the longest label among ALL of them —
/// not just the shown window, so the column holds still while the list scrolls — capped at
/// [`LABEL_CAP`] and at what the terminal leaves beside the indent.
pub(crate) fn label_column(ms: &[&Suggestion], width: usize) -> usize {
    ms.iter()
        .map(|s| str_width(suggest_label(s)))
        .max()
        .unwrap_or(0)
        .min(LABEL_CAP)
        .min(width.saturating_sub(LABEL_INDENT))
        .max(1)
}

/// The rows of one item at `width` with a label column `col` wide. The first row is the label — a
/// cyan `›` and label when `selected`, faint otherwise, cut with `…` past `col` — then, [`GAP`]
/// past the column, the description's first line, faint; its further lines start at the same
/// column. The description wraps at `min(width - its column, DESC_MEASURE)`, at most
/// [`DESC_LINES`] lines, the last cut with `…` when the text runs on. An item with no description
/// is its label row.
pub(crate) fn item_rows(s: &Suggestion, selected: bool, col: usize, width: usize) -> Vec<String> {
    let label = truncate_ansi(suggest_label(s), col, "…");
    let head = if selected {
        format!(" {CYAN}› {label}{RESET}")
    } else {
        format!("   {FAINT}{label}{RESET}")
    };
    // A description is prose: its own breaks and runs of spaces are not layout.
    let desc = s.desc.split_whitespace().collect::<Vec<_>>().join(" ");
    if desc.is_empty() {
        return vec![head];
    }
    let at = LABEL_INDENT + col + GAP;
    let measure = width.saturating_sub(at).clamp(2, DESC_MEASURE);
    let mut lines = word_wrap_ansi(&desc, measure);
    if lines.len() > DESC_LINES {
        lines.truncate(DESC_LINES);
        if let Some(last) = lines.last_mut() {
            *last = format!("{}…", truncate_ansi(last, measure - 1, "").trim_end());
        }
    }
    let gap = " ".repeat(col - str_width(&label) + GAP);
    let indent = " ".repeat(at);
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                format!("{head}{gap}{FAINT}{l}{RESET}")
            } else {
                format!("{indent}{FAINT}{l}{RESET}")
            }
        })
        .collect()
}

/// Lays the matches out in `room` rows at `width`: the window starts at `top` (the last draw's)
/// and moves just enough for the highlighted item `cur` to be whole inside it, then reaches back
/// while the items above fit into rows the bottom leaves free. An item that does not fit whole is
/// left out — except the highlighted one (or the first, with none), which is shown even when the
/// room is shorter. Returns the view and the window's new top.
pub(crate) fn layout(
    ms: &[&Suggestion],
    cur: Option<usize>,
    top: usize,
    width: usize,
    room: usize,
) -> (ListView, usize) {
    let col = label_column(ms, width);
    let items: Vec<Vec<String>> = ms
        .iter()
        .enumerate()
        .map(|(i, s)| item_rows(s, cur == Some(i), col, width))
        .collect();
    let span = |from: usize, to: usize| items[from..to].iter().map(Vec::len).sum::<usize>();
    let (mut top, at) = match cur {
        Some(i) => (top.min(i), i),
        None => (0, 0),
    };
    while top < at && span(top, at + 1) > room {
        top += 1;
    }
    while top > 0 && span(top - 1, items.len()) <= room {
        top -= 1;
    }
    let mut view = ListView::default();
    for (i, rows) in items.into_iter().enumerate().skip(top) {
        if i > top && view.rows.len() + rows.len() > room {
            break;
        }
        view.items.extend(std::iter::repeat_n(i, rows.len()));
        view.rows.extend(rows);
    }
    (view, top)
}

/// The list for the composer's line in `room` rows (empty with no matches), its window kept on
/// the composer for the next draw.
pub(crate) fn frame_list(
    c: &mut Composer,
    commands: &[Suggestion],
    width: u16,
    room: usize,
) -> ListView {
    let ms = matches_for(c, commands);
    let cur = if c.suggestion_base.is_empty() {
        None
    } else {
        c.suggestion_index.filter(|&i| i < ms.len())
    };
    let (view, top) = if ms.is_empty() {
        (ListView::default(), 0)
    } else {
        layout(&ms, cur, c.suggestion_top, usize::from(width), room)
    };
    c.suggestion_top = top;
    view
}

#[cfg(test)]
mod tests;
