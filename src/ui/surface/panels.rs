//! Panel rendering (tabbed.go:449-748 renderSurface + renderSearchRow): the tab-bar
//! chips (identical padding across focus states — the bar width is focus-invariant),
//! the per-kind bodies, the search row that REPLACES the hint row (the frame height
//! never changes entering/leaving search), and the byte-exact hint composition.
//! Render-computed state (page size, scroll clamp, wrapped geometry, pending centre,
//! the real-cursor target) lands in the state's `Cell`s behind a shared borrow.

use std::fmt::Write as _;

use crate::text::ansi::{ansi_width, clip_line, truncate_ansi, wrap_ansi};

use super::Rendered;
use super::field::input_field;
use super::search::{SearchMode, highlight_line};
use super::tabbed::{
    PICKER_FRAME_ROWS, PICKER_GUTTER, PICKER_ROWS, PanelState, SurfaceState, clamp, panel_height,
    picker_preview_cols, scroll_percent, search_typing_hint, surface_hint,
};
use super::theme::{CYAN, ERR_PREFIX, FAINT, GREEN, RESET, REV_ON, input_bg};
use crate::ui::facade::{Panel, PanelKind};

/// A real-cursor target in surface-block coordinates.
fn cursor_at(x: usize, y: usize) -> (u16, u16) {
    (
        u16::try_from(x).unwrap_or(u16::MAX),
        u16::try_from(y).unwrap_or(u16::MAX),
    )
}

/// Draws the tab bar, the focused panel, and the help hint (tabbed.go:449-711) as one
/// row per element. The query field replaces the hint row rather than adding one.
pub(crate) fn render_rows(st: &mut SurfaceState, width: u16) -> Rendered {
    let focus = st.focus;
    let term_h = st.term_height();
    let dark = st.dark();
    let n = st.slots.len();
    let mut cursor = None;
    if focus >= n {
        return Rendered::default();
    }
    let w = usize::from(width).max(4);
    let mut rows = Vec::new();

    // Tab bar (multi-panel only). Both chip states carry identical " title " padding
    // so switching focus never changes the bar width (tabbed.go:454-473).
    if st.slots.len() > 1 {
        let mut bar = String::new();
        for (i, slot) in st.slots.iter().enumerate() {
            if i > 0 {
                bar.push_str(FAINT);
                bar.push_str(" │ ");
                bar.push_str(RESET);
            }
            let style = if i == focus { REV_ON } else { FAINT };
            let _ = write!(bar, "{style} {} {RESET}", slot.spec.title);
        }
        rows.push(truncate_ansi(&bar, w, "…"));
    } else {
        // Single-panel surfaces reuse the focused-chip style.
        rows.push(format!("{REV_ON} {} {RESET}", st.slots[focus].spec.title));
    }
    let slot = &mut st.slots[focus];
    let (p, ps) = (&slot.spec, &mut slot.state);

    if !p.prompt.is_empty() {
        rows.push(format!(
            "{FAINT} {}{RESET}",
            truncate_ansi(&p.prompt, w.saturating_sub(2).max(4), "…")
        ));
    }

    match p.kind() {
        PanelKind::List | PanelKind::Multi => {
            render_list(p, ps, w, &mut rows, &mut cursor, dark);
        }
        PanelKind::Picker => render_picker(term_h, p, ps, w, &mut rows),
        PanelKind::Slider => render_slider(p, ps, w, &mut rows),
        PanelKind::Switch => render_switch(ps, &mut rows),
        PanelKind::Input => render_input(p, ps, w, &mut rows, &mut cursor, dark),
        PanelKind::View => render_view(p, ps, w, &mut rows),
        PanelKind::Browser => render_browser(p, ps, w, &mut rows),
    }

    // The query field REPLACES the hint row: both are a single row, so the frame's
    // height is identical in and out of search (tabbed.go:698-701).
    if ps.search.mode == SearchMode::Typing {
        render_search_row(p, ps, w, &mut rows, &mut cursor);
        return Rendered { rows, cursor };
    }
    let mut hint = surface_hint(p, ps);
    if n > 1 {
        hint = format!("Tab switch · {hint}");
    }
    if let Some(pct) = scroll_percent(p, ps) {
        hint = format!("{hint} · {pct}%");
    }
    rows.push(format!("{FAINT}{}{RESET}", truncate_ansi(&hint, w, "…")));
    Rendered { rows, cursor }
}

/// One truncated row body with the v1 cursor-highlight semantics: the cursor row's
/// TEXT goes cyan when the row is plain; rows carrying their own ANSI keep it, with
/// just the cyan marker (tabbed.go:539-546).
fn styled_item(label: &str, is_cursor: bool, w: usize) -> String {
    let item = truncate_ansi(label, w.saturating_sub(6).max(4), "…");
    if is_cursor && !item.contains("\x1b[") {
        format!("{CYAN}{item}{RESET}")
    } else {
        item
    }
}

fn render_list(
    p: &Panel,
    ps: &mut PanelState,
    w: usize,
    rows: &mut Vec<String>,
    cursor: &mut Option<(u16, u16)>,
    dark: bool,
) {
    let h = panel_height(p, ps.view.len());
    ps.rows = h;
    ps.scroll_to(h);
    let offset = ps.offset;
    for &i in ps.view.iter().skip(offset).take(h) {
        let marker = if i == ps.cursor {
            format!("{CYAN}▸ {RESET}")
        } else {
            "  ".to_owned()
        };
        let box_prefix = if p.kind() == PanelKind::Multi {
            if ps.checked.contains(&i) {
                format!("{GREEN}[x] {RESET}")
            } else {
                format!("{FAINT}[ ] {RESET}")
            }
        } else {
            String::new()
        };
        if p.custom() && i == p.items().len() {
            // The "Other…" row: an inline editor while editing, the saved text once
            // entered, the plain affordance otherwise (tabbed.go:502-538).
            if ps.editing {
                let prefix_cols = if p.kind() == PanelKind::Multi { 6 } else { 2 };
                let box_w = clamp(40, 4, w.saturating_sub(prefix_cols + 4).max(4));
                let (mut field, cur_col) = input_field(&ps.input, &mut ps.input_offset, box_w);
                if ps.input.value().is_empty() {
                    field = format!("{FAINT}{}", truncate_ansi("your answer", box_w, "…"));
                }
                let pad = box_w.saturating_sub(ansi_width(&field));
                *cursor = Some(cursor_at(prefix_cols + cur_col, rows.len()));
                // No leading padding column: the field's first text cell sits exactly
                // where the option labels start (only a trailing pad closes the shade).
                let bg = input_bg(dark);
                rows.push(format!(
                    "{marker}{box_prefix}{bg}{field}{RESET}{bg}{} {RESET}",
                    " ".repeat(pad)
                ));
                continue;
            }
            let v = ps.input.value().trim().to_owned();
            let label = if v.is_empty() {
                "Other…".to_owned()
            } else {
                format!("Other: {v}")
            };
            rows.push(format!(
                "{marker}{box_prefix}{}",
                styled_item(&label, i == ps.cursor, w)
            ));
            continue;
        }
        rows.push(format!(
            "{marker}{box_prefix}{}",
            styled_item(&ps.items[i], i == ps.cursor, w)
        ));
    }
}

/// Draws the preview pane beside the item list, then the current item's detail line
/// (tabbed.go:784-863).
///
/// Rows are composed cell by cell: every preview row is padded to the pane width by DISPLAY
/// width, so the list column starts at the same screen column on every row regardless of the SGR
/// the preview carries (imgterm rows are dense with it).
fn render_picker(term_h: usize, p: &Panel, ps: &mut PanelState, w: usize, rows: &mut Vec<String>) {
    // The list window follows the item count; the preview gets its own row budget (a 3-item list
    // must still show a legible thumbnail), clamped so the inline frame keeps room for the
    // composer and the status row.
    let h = panel_height(p, ps.view.len());
    ps.rows = h;
    ps.scroll_to(h);

    let mut preview: Vec<String> = Vec::new();
    let cols = picker_preview_cols(ps, w);
    if cols > 0 && !ps.items.is_empty() {
        let mut prev_h = if p.height > 0 { p.height } else { PICKER_ROWS };
        if term_h > 0 {
            prev_h = clamp(prev_h, 4, term_h.saturating_sub(PICKER_FRAME_ROWS).max(4));
        }
        preview = ps.preview_rows(cols, prev_h);
    }

    // Columns hug the RENDERED preview, not the reserved pane: a portrait thumbnail bounded by
    // height comes out far narrower than its budget, and padding to the budget would open a gulf
    // between picture and list. Every row pads to the same measured width, so the list column
    // stays put.
    let preview_w = preview.iter().map(|r| ansi_width(r)).max().unwrap_or(0);
    let list_cols = if preview_w > 0 {
        w.saturating_sub(preview_w)
            .saturating_sub(PICKER_GUTTER)
            .saturating_sub(2)
    } else {
        w.saturating_sub(2) // the "▸ " marker gutter
    }
    .max(4);

    let offset = ps.offset;
    for r in 0..h.max(preview.len()) {
        let mut line = String::new();
        if preview_w > 0 {
            let cell = preview.get(r).map_or("", String::as_str);
            // ANSI-aware width: counting the SGR bytes as glyphs would collapse the pad to zero
            // and let the list column drift row by row.
            let pad = preview_w.saturating_sub(ansi_width(cell));
            line.push_str(cell);
            line.push_str(&" ".repeat(pad + PICKER_GUTTER));
        }
        if r < h
            && let Some(&i) = ps.view.get(offset + r)
        {
            let mut item = truncate_ansi(&ps.items[i], list_cols, "…");
            let marker = if i == ps.cursor {
                if !item.contains("\x1b[") {
                    item = format!("{CYAN}{item}{RESET}");
                }
                format!("{CYAN}▸ {RESET}")
            } else {
                "  ".to_owned()
            };
            line.push_str(&marker);
            line.push_str(&item);
        }
        rows.push(line.trim_end_matches(' ').to_owned());
    }

    // The detail line follows the cursor: a caller-sized string (an OSC 8 path link here)
    // printed verbatim — truncating a hyperlink would slice the escape apart.
    if let Some(d) = p.details().get(ps.cursor).filter(|d| !d.is_empty()) {
        rows.push(format!("{FAINT}{d}{RESET}"));
    }
}

/// The bar's filled cell count for a [0,1] ratio.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn bar_cells(pct: f64, width: usize) -> usize {
    // width ≤ 40 and pct ∈ [0,1]: exact enough for a cell count.
    ((pct.clamp(0.0, 1.0) * width as f64) + 0.5).floor() as usize
}

fn render_slider(p: &Panel, ps: &mut PanelState, w: usize, rows: &mut Vec<String>) {
    // Right-align the value label to "default"'s width so toggling between default
    // and a number never shifts the bar's origin (tabbed.go:548-566). The bar itself
    // is a plain filled bar (T-27 — charm's gradient/partial blocks not ported).
    let (min, max) = p.as_slider().map_or((0.0, 0.0), |sl| (sl.min, sl.max));
    let (label, pct) = match ps.value {
        None => ("default".to_owned(), 0.0),
        Some(v) => (
            format!("{v:.1}"),
            if max > min {
                (v - min) / (max - min)
            } else {
                0.0
            },
        ),
    };
    let mut val = format!("{label:>7}");
    if ps.value.is_none() {
        val = format!("{FAINT}{val}{RESET}");
    }
    let bar_w = clamp(w.saturating_sub(13), 10, 40);
    let filled = bar_cells(pct, bar_w).min(bar_w);
    let bar = format!(
        "{CYAN}{}{RESET}{FAINT}{}{RESET}",
        "█".repeat(filled),
        "░".repeat(bar_w - filled)
    );
    // Blank rows above and below give the lone bar row some breathing room.
    rows.push(String::new());
    rows.push(format!("  {val}  {bar}"));
    rows.push(String::new());
}

fn render_switch(ps: &mut PanelState, rows: &mut Vec<String>) {
    // Identical widths in both states (right-aligned label, fixed-length track) so
    // toggling never shifts the layout — the knob slides (tabbed.go:567-580).
    const TRACK: usize = 6;
    let (label, toggle) = if ps.on {
        ("On", format!("{GREEN}{}●{RESET}", "━".repeat(TRACK)))
    } else {
        ("Off", format!("{FAINT}●{}{RESET}", "─".repeat(TRACK)))
    };
    let mut val = format!("{label:>3}");
    if !ps.on {
        val = format!("{FAINT}{val}{RESET}");
    }
    rows.push(String::new());
    rows.push(format!("  {val}  {toggle}"));
    rows.push(String::new());
}

fn render_input(
    p: &Panel,
    ps: &mut PanelState,
    w: usize,
    rows: &mut Vec<String>,
    cursor: &mut Option<(u16, u16)>,
    dark: bool,
) {
    let input_w = p.as_input().map_or(0, |i| i.width);
    let placeholder = p.as_input().map_or("", |i| i.placeholder.as_str());
    let box_w = clamp(
        if input_w == 0 { 40 } else { input_w },
        4,
        w.saturating_sub(6).max(4),
    );
    let (mut view, cur_col) = input_field(&ps.input, &mut ps.input_offset, box_w);
    if ps.input.value().is_empty() && !placeholder.is_empty() {
        // Our own placeholder render: faint on the field background (tabbed.go:588-593).
        view = format!("{FAINT}{}", truncate_ansi(placeholder, box_w, "…"));
    }
    let pad = box_w.saturating_sub(ansi_width(&view));
    // Blank rows above and below, like the slider. Text renders in the DEFAULT
    // foreground on the adaptive shade; the trailing re-assert keeps the padding
    // shaded even if the field content ever carries a reset (tabbed.go:606-612).
    rows.push(String::new());
    *cursor = Some(cursor_at(3 + cur_col, rows.len()));
    let bg = input_bg(dark);
    rows.push(format!(
        "  {bg} {view}{RESET}{bg}{} {RESET}",
        " ".repeat(pad)
    ));
    rows.push(String::new());
}

fn render_view(p: &Panel, ps: &mut PanelState, w: usize, rows: &mut Vec<String>) {
    // Highlight BEFORE wrapping: hits are recorded against logical lines, and
    // wrap_ansi carries the SGR across row boundaries, so a match split by a wrap
    // stays highlighted on both rows (tabbed.go:613-641).
    let q = ps.search_draft();
    let mut lines: Vec<String> = if q.is_empty() {
        ps.items.clone()
    } else {
        let cur = ps.current_hit();
        ps.items
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let off = cur.filter(|c| c.line == i).map(|c| c.off);
                highlight_line(l, &q, off)
            })
            .collect()
    };
    if p.wrap() {
        // Remember where each logical line begins so a hit can be centred in
        // wrapped-row space, which is the only space offset speaks.
        let mut starts = Vec::with_capacity(lines.len());
        let mut out = Vec::new();
        for l in &lines {
            starts.push(out.len());
            out.extend(wrap_ansi(l, w));
        }
        lines = out;
        ps.wrap_starts = starts;
    }
    ps.wrapped = lines.len();
    let h = panel_height(p, lines.len());
    ps.rows = h;
    let max_off = lines.len().saturating_sub(h);
    let mut offset = ps.offset;
    // A pending jump lands here, where the wrapped geometry is finally known — "key
    // sets the intent, render resolves it" (tabbed.go:646-658).
    if let Some(line) = ps.pending_center.take() {
        let row = if p.wrap() {
            ps.wrap_starts.get(line).copied().unwrap_or(line)
        } else {
            line
        };
        offset = row.saturating_sub(h / 2);
    }
    offset = offset.min(max_off);
    ps.offset = offset;
    let pan_offset = if p.wrap() { 0 } else { ps.pan_offset };
    for l in lines.iter().skip(offset).take(h) {
        if pan_offset > 0 {
            rows.push(clip_line(l, pan_offset, w));
        } else {
            rows.push(truncate_ansi(l, w, "…"));
        }
    }
}

fn render_browser(p: &Panel, ps: &mut PanelState, w: usize, rows: &mut Vec<String>) {
    rows.push(format!(
        "{FAINT}{}{RESET}",
        truncate_ansi(&ps.dir.to_string_lossy(), w, "…")
    ));
    if !ps.error_text.is_empty() {
        rows.push(format!("{ERR_PREFIX}{}", ps.error_text));
    }
    let h = panel_height(p, ps.view.len());
    ps.rows = h;
    ps.scroll_to(h);
    let offset = ps.offset;
    for &i in ps.view.iter().skip(offset).take(h) {
        let marker = if i == ps.cursor {
            format!("{CYAN}▸ {RESET}")
        } else {
            "  ".to_owned()
        };
        let Some(e) = ps.entries.get(i) else { continue };
        let mut name = truncate_ansi(&e.name, w.saturating_sub(4).max(4), "…");
        if i == ps.cursor {
            name = format!("{CYAN}{name}{RESET}");
        } else if e.is_dir {
            name = format!("{FAINT}{name}{RESET}");
        }
        rows.push(format!("{marker}{name}"));
    }
}

/// Draws the query field in the hint row's place, with the live match count trailing
/// it, and parks the REAL terminal cursor in the field — IME preedit anchors to it,
/// as in the Input panel (tabbed.go:716-727).
fn render_search_row(
    p: &Panel,
    ps: &mut PanelState,
    w: usize,
    rows: &mut Vec<String>,
    cursor: &mut Option<(u16, u16)>,
) {
    let box_w = clamp(32, 4, w.saturating_sub(24).max(4));
    let (field, cur_col) = input_field(&ps.search.input, &mut ps.search.input_offset, box_w);
    *cursor = Some(cursor_at(1 + cur_col, rows.len()));
    rows.push(format!(
        "{CYAN}/{RESET}{field}{FAINT} · {}{RESET}",
        search_typing_hint(p, ps)
    ));
}
