//! Tabbed-surface state (internal/ui/tabbed.go): per-panel state, commit-all
//! `result()`, `set_focus` (abandons a half-typed query, keeps an applied one), the
//! `inputField` consumers' shared geometry (`panel_height`, `scroll_to`, visible-row
//! paging), `slider_step`'s integer step indices, the browser's directory loader, and
//! the byte-exact hint rows (tabbed.go:916-947, `·` = U+00B7).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::ui::facade::{Panel, PanelBody, PanelKind, PanelResult, PreviewFn, TabbedResult};

use super::field::Field;
use super::search::{SearchMode, SearchState};

/// One row of the directory browser (tabbed.go:137-141).
#[derive(Debug, Clone)]
pub(crate) struct BrowseEntry {
    /// Display name (`"../"`, `"dir/"`, `"file"`).
    pub(crate) name: String,
    /// Full path.
    pub(crate) path: PathBuf,
    /// Whether Enter descends instead of committing.
    pub(crate) is_dir: bool,
}

/// The mutable per-panel state (tabbed.go:144-179). Render-computed values (page
/// size, scroll offset, wrapped geometry, pending centre, pan windows) are settled by
/// the render pass, which owns the state mutably.
pub(crate) struct PanelState {
    /// Cursor as an UNDERLYING index into `items`/`entries` (never renumbered by a
    /// filter — the commit contract).
    pub(crate) cursor: usize,
    /// Multi checks, keyed by underlying index (ascending at commit for free).
    pub(crate) checked: BTreeSet<usize>,
    /// Slider value; `None` = default.
    pub(crate) value: Option<f64>,
    /// Switch state.
    pub(crate) on: bool,
    /// Scroll offset in VISIBLE rows (row panels) / rendered rows (View).
    pub(crate) offset: usize,
    /// Live copy of `Panel::items`/`Panel::lines` (the refresh target; Custom panels
    /// carry the appended `"Other…"` row).
    pub(crate) items: Vec<String>,
    /// Browser: current directory.
    pub(crate) dir: PathBuf,
    /// Browser: current entries.
    pub(crate) entries: Vec<BrowseEntry>,
    /// Browser: chosen file path (`""` = none).
    pub(crate) chosen: String,
    /// Browser: last read error, rendered as an `ERR_PREFIX` row.
    pub(crate) error_text: String,
    /// View: the last `c` copied to the clipboard (the ✓ hint).
    pub(crate) copied: bool,
    /// View(Wrap): wrapped row count from the last render.
    pub(crate) wrapped: usize,
    /// View(!Wrap): horizontal pan offset (h/l).
    pub(crate) pan_offset: usize,
    /// The one-line field (Input panels and the inline Custom editor).
    pub(crate) input: Field,
    /// The field's horizontal window start column (`input_field`).
    pub(crate) input_offset: usize,
    /// List/Multi Custom: the inline editor is open.
    pub(crate) editing: bool,
    /// Last visible row budget, for paging.
    pub(crate) rows: usize,
    /// Per-panel search (search.go); survives tabbing away and back.
    pub(crate) search: SearchState,
    /// Visible row → underlying index; ALWAYS populated (identity unfiltered).
    pub(crate) view: Vec<usize>,
    /// View: logical line to centre at the next render (`None` = none).
    pub(crate) pending_center: Option<usize>,
    /// View(Wrap): logical line → first wrapped row, from the last render.
    pub(crate) wrap_starts: Vec<usize>,
    /// Picker: the preview renderer, MOVED here out of [`Panel::preview`] when the surface
    /// opens (tabbed.go's `p.Preview` stays on the panel; the render path owns the state
    /// mutably, and an `FnMut` needs exactly that).
    pub(crate) preview: Option<PreviewFn>,
    /// Picker: the last rendered preview and the selection/geometry it was rendered for
    /// (tabbed.go:151 `prevIdx`); `None` until the first render.
    pub(crate) preview_cache: Option<PreviewCache>,
}

/// A rendered picker preview, keyed by what produced it.
pub(crate) struct PreviewCache {
    /// The cursor the preview was rendered for.
    pub(crate) idx: usize,
    /// The pane width the preview was rendered for.
    pub(crate) cols: usize,
    /// The pane height the preview was rendered for.
    pub(crate) rows: usize,
    /// The rendered rows.
    pub(crate) lines: Vec<String>,
}

impl PanelState {
    fn empty() -> Self {
        Self {
            cursor: 0,
            checked: BTreeSet::new(),
            value: None,
            on: false,
            offset: 0,
            items: Vec::new(),
            dir: PathBuf::new(),
            entries: Vec::new(),
            chosen: String::new(),
            error_text: String::new(),
            copied: false,
            wrapped: 0,
            pan_offset: 0,
            input: Field::new(),
            input_offset: 0,
            editing: false,
            rows: 0,
            search: SearchState::new(),
            view: Vec::new(),
            pending_center: None,
            wrap_starts: Vec::new(),
            preview: None,
            preview_cache: None,
        }
    }

    /// Fresh state for one panel (tabbed.go:190-254 newSurface, per-kind arm).
    ///
    /// `p` is taken mutably only to move a Picker's preview closure into the state; nothing else
    /// on the panel is touched.
    fn for_panel(p: &mut Panel) -> Self {
        let mut st = Self::empty();
        match &mut p.body {
            // A Picker is a single-select row list plus a preview pane: same items, same cursor
            // clamp, no checkboxes and no `"Other…"` row (tabbed.go:198-203).
            PanelBody::Picker(picker) => {
                st.items.clone_from(&picker.items);
                st.cursor = if picker.cursor < st.items.len() {
                    picker.cursor
                } else {
                    0
                };
                st.preview = picker.preview.take();
            }
            PanelBody::List(list) | PanelBody::Multi(list) => {
                st.items.clone_from(&list.items);
                if list.custom {
                    st.items.push("Other…".to_owned());
                }
                st.cursor = if list.cursor < st.items.len() {
                    list.cursor
                } else {
                    0
                };
                st.checked.extend(list.checked.iter().copied());
            }
            PanelBody::Slider(slider) => st.value = slider.value,
            PanelBody::Switch { on } => st.on = *on,
            PanelBody::Input(input) => st.input.set_value(&input.text),
            PanelBody::View(view) => st.items.clone_from(&view.lines),
            PanelBody::Browser { dir } => {
                let dir = if dir.as_os_str().is_empty() {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                } else {
                    dir.clone()
                };
                st.set_dir(p, &dir);
            }
        }
        st
    }

    /// Loads a browser panel's directory: `"../"` + dirs + files, name-sorted,
    /// dot-hidden entries skipped; a read error keeps the old directory and renders
    /// the message. Descending drops any filter: the query was aimed at the directory
    /// being left (tabbed.go:259-289).
    pub(crate) fn set_dir(&mut self, p: &Panel, dir: &Path) {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                self.error_text = e.to_string();
                return;
            }
        };
        self.dir = dir.to_path_buf();
        self.error_text.clear();
        self.entries.clear();
        if let Some(parent) = dir.parent().filter(|q| !q.as_os_str().is_empty()) {
            self.entries.push(BrowseEntry {
                name: "../".to_owned(),
                path: parent.to_path_buf(),
                is_dir: true,
            });
        }
        let mut named: Vec<(String, PathBuf, bool)> = rd
            .filter_map(Result::ok)
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                (name, e.path(), is_dir)
            })
            .collect();
        named.sort_by(|a, b| a.0.cmp(&b.0)); // os.ReadDir is name-sorted — replicate
        let (mut dirs, mut files) = (Vec::new(), Vec::new());
        for (name, path, is_dir) in named {
            if name.starts_with('.') {
                continue;
            }
            if is_dir {
                dirs.push(BrowseEntry {
                    name: format!("{name}/"),
                    path,
                    is_dir: true,
                });
            } else {
                files.push(BrowseEntry {
                    name,
                    path,
                    is_dir: false,
                });
            }
        }
        self.entries.append(&mut dirs);
        self.entries.append(&mut files);
        self.cursor = 0;
        self.offset = 0;
        self.search_clear(p);
    }

    /// Keeps the cursor visible in an `h`-row window. Both coordinates are VISIBLE
    /// rows: `offset` counts them, and the cursor (an underlying index) translates
    /// through the view mapping (tabbed.go:976-991).
    pub(crate) fn scroll_to(&mut self, h: usize) {
        let n = self.view.len();
        let vp = self.view_pos();
        let mut off = self.offset;
        if vp < off {
            off = vp;
        }
        if vp >= off + h {
            off = vp + 1 - h;
        }
        if n == 0 {
            off = 0;
        } else if off > n - 1 {
            off = n - 1;
        }
        self.offset = off;
    }

    /// Moves the cursor / scroll of the focused panel by one step (model.go:895-902).
    pub(crate) fn nav(&mut self, p: &Panel, dir: isize) {
        match p.kind() {
            PanelKind::List | PanelKind::Multi | PanelKind::Picker | PanelKind::Browser => {
                let vp = isize::try_from(self.view_pos()).unwrap_or(isize::MAX);
                self.set_view_pos(vp + dir);
            }
            PanelKind::View => {
                self.offset = self.offset.saturating_add_signed(dir); // upper bound clamped at render
            }
            _ => {}
        }
    }

    /// Moves by one visible page (model.go:881-892 — v1 ←→ / Ctrl+B/F semantics). Row
    /// panels step through VISIBLE rows, so a filter's gaps are skipped rather than
    /// paged across.
    pub(crate) fn page(&mut self, p: &Panel, dir: isize) {
        let step = isize::try_from(self.rows.max(1)).unwrap_or(1);
        match p.kind() {
            PanelKind::List | PanelKind::Multi | PanelKind::Picker | PanelKind::Browser => {
                let vp = isize::try_from(self.view_pos()).unwrap_or(isize::MAX);
                self.set_view_pos(vp + dir * step);
            }
            PanelKind::View => {
                self.offset = self.offset.saturating_add_signed(dir * step); // upper bound clamped at render
            }
            _ => {}
        }
    }

    /// The highlighted item's preview, rendered only when the selection or the pane geometry has
    /// changed since the last frame (tabbed.go:867-874).
    ///
    /// An expensive renderer (a multi-MB decode) then runs once per selection, not once per
    /// frame — which is the whole reason the closure is a `FnMut` owning its own cache.
    pub(crate) fn preview_rows(&mut self, cols: usize, rows: usize) -> Vec<String> {
        if let Some(c) = &self.preview_cache
            && c.idx == self.cursor
            && c.cols == cols
            && c.rows == rows
        {
            return c.lines.clone();
        }
        let cursor = self.cursor;
        let lines = self
            .preview
            .as_mut()
            .map(|f| f(cursor, cols, rows))
            .unwrap_or_default();
        self.preview_cache = Some(PreviewCache {
            idx: cursor,
            cols,
            rows,
            lines: lines.clone(),
        });
        lines
    }
}

/// An open Tabbed surface's state: focus + per-panel states. The transition fn
/// `surface_key` (mod.rs) drives it; a render returns its cursor target with its rows.
pub(crate) struct SurfaceState {
    /// The focused tab.
    pub(crate) focus: usize,
    /// Wizard mode: Enter on a non-last tab advances instead of committing.
    pub(crate) enter_advances: bool,
    /// The panels, each with its live state.
    pub(crate) slots: Vec<PanelSlot>,
    /// The terminal's row count, published by the loop before each render (`0` = unknown, the
    /// Go model's own pre-`WindowSizeMsg` state). Only the Picker reads it, to keep its inline
    /// preview from crowding out the composer (tabbed.go:797-799 `m.height`).
    term_h: u16,
    /// The terminal's background tone, published by the loop before each render (dark is the
    /// safe default; the input shade follows it).
    dark: bool,
}

/// One panel of an open surface: the spec it was opened with and its live state.
pub(crate) struct PanelSlot {
    /// The panel as the caller specified it. A Picker's preview closure is MOVED out of
    /// it into [`PanelState`] when the surface opens (it is an `FnMut`; every render sees
    /// the spec through a shared borrow), so `has_preview()` reads false afterwards — the
    /// facade's summary is taken at the `tabbed()` call, before the surface opens.
    pub(crate) spec: Panel,
    /// What the keys and renders have done to it.
    pub(crate) state: PanelState,
}

impl PanelSlot {
    fn open(mut spec: Panel) -> Self {
        let mut state = PanelState::for_panel(&mut spec);
        state.rebuild_view(&spec); // identity to start with; every render reads it
        Self { spec, state }
    }
}

impl SurfaceState {
    /// Fresh state over `panels` (tabbed.go:190 newSurface).
    pub(crate) fn new(enter_advances: bool, panels: Vec<Panel>) -> Self {
        Self {
            focus: 0,
            enter_advances,
            slots: panels.into_iter().map(PanelSlot::open).collect(),
            term_h: 0,
            dark: true,
        }
    }

    /// The focused panel's spec and state (`slots[focus]`, split for the key handlers).
    pub(crate) fn focused(&mut self) -> (&Panel, &mut PanelState) {
        let slot = &mut self.slots[self.focus];
        (&slot.spec, &mut slot.state)
    }

    /// Publishes the terminal height for the next render (`0` = unknown).
    pub(crate) fn set_term_height(&mut self, h: u16) {
        self.term_h = h;
    }

    /// Publishes the terminal's background tone for the next render.
    pub(crate) fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
    }

    /// Whether the terminal background is dark, as last published.
    pub(crate) fn dark(&self) -> bool {
        self.dark
    }

    /// The terminal height the last [`Self::set_term_height`] published.
    pub(crate) fn term_height(&self) -> usize {
        usize::from(self.term_h)
    }

    /// The commit-all snapshot (tabbed.go:292-309): per panel `{cursor (underlying),
    /// value, on, path, text}`; a Custom panel commits `custom` = trimmed input with
    /// `text` forced empty; `checked` = ascending underlying indices.
    pub(crate) fn result(&self) -> TabbedResult {
        let mut out = TabbedResult {
            cancelled: false,
            focused: self.focus,
            panels: Vec::with_capacity(self.slots.len()),
        };
        for slot in &self.slots {
            let ps = &slot.state;
            let custom = slot.spec.custom();
            let mut pr = PanelResult {
                cursor: ps.cursor,
                checked: ps
                    .checked
                    .iter()
                    .copied()
                    .filter(|j| *j < ps.items.len())
                    .collect(),
                value: ps.value,
                on: ps.on,
                path: ps.chosen.clone(),
                text: ps.input.value().to_owned(),
                custom: String::new(),
            };
            if custom {
                ps.input.value().trim().clone_into(&mut pr.custom);
                pr.text = String::new();
            }
            out.panels.push(pr);
        }
        out
    }

    /// Moves the focused tab (tabbed.go:313-331): closes an open Custom editor and
    /// abandons a half-typed (`Typing`) query on the way out — an APPLIED filter
    /// survives tabbing away and back.
    pub(crate) fn set_focus(&mut self, i: usize) {
        if let Some(slot) = self.slots.get_mut(self.focus) {
            let (p, ps) = (&slot.spec, &mut slot.state);
            if ps.editing {
                ps.editing = false;
            }
            if ps.search.mode == SearchMode::Typing {
                ps.search_clear(p);
            }
        }
        self.focus = i;
    }
}

/// Ports the v1 `SliderPanel` semantics: integer step indices (no float accumulation),
/// default↔range transitions, Max clamp (tabbed.go:409-428). A zero step is inert
/// (caller error; Go would produce NaN).
pub(crate) fn slider_step(ps: &mut PanelState, p: &Panel, dir: i32) {
    let Some(sl) = p.as_slider() else { return };
    let Some(v) = ps.value else {
        if dir > 0 {
            ps.value = Some(sl.min);
        }
        return;
    };
    if sl.step == 0.0 {
        return;
    }
    let steps = ((v - sl.min) / sl.step).round() + f64::from(dir);
    let mut nv = sl.min + steps * sl.step;
    nv = (nv * 1e9).round() / 1e9;
    if nv < sl.min {
        ps.value = None;
        return;
    }
    ps.value = Some(nv.min(sl.max));
}

/// A panel's visible row budget: `height` override, else 10 (View 15), clamped to the
/// item count, minimum 1 (tabbed.go:431-446).
pub(crate) fn panel_height(p: &Panel, n: usize) -> usize {
    let mut h = if p.height > 0 {
        p.height
    } else if p.kind() == PanelKind::View {
        15
    } else {
        10
    };
    if n < h {
        h = n;
    }
    h.max(1)
}

/// Below this width the preview is dropped entirely (tabbed.go:753).
pub(crate) const PICKER_MIN_WIDTH: usize = 64;
/// Preview columns never exceed this (tabbed.go:754).
pub(crate) const PICKER_PREVIEW_CAP: usize = 44;
/// Columns between the preview pane and the list (tabbed.go:755).
pub(crate) const PICKER_GUTTER: usize = 2;
/// List columns the preview must never eat into (tabbed.go:756).
pub(crate) const PICKER_MIN_LIST: usize = 18;
/// Preview rows, `Panel::height` overriding, terminal permitting (tabbed.go:757).
pub(crate) const PICKER_ROWS: usize = 14;
/// Frame rows the preview must leave for everything else (tabbed.go:758).
pub(crate) const PICKER_FRAME_ROWS: usize = 12;

/// The preview pane's width; `0` = no preview (tabbed.go:762-777).
///
/// The preview claims a share of the width, capped, and only while the terminal is wide enough
/// for both columns to stay readable — below that a plain list beats two squeezed ones.
pub(crate) fn picker_preview_cols(ps: &PanelState, w: usize) -> usize {
    if ps.preview.is_none() || w < PICKER_MIN_WIDTH {
        return 0;
    }
    let mut cols = (w * 2 / 5).min(PICKER_PREVIEW_CAP);
    if w.saturating_sub(cols).saturating_sub(PICKER_GUTTER) < PICKER_MIN_LIST {
        cols = w
            .saturating_sub(PICKER_GUTTER)
            .saturating_sub(PICKER_MIN_LIST);
    }
    if cols < 8 { 0 } else { cols }
}

/// Go `clampInt` (in order: floor then cap; never panics on lo > hi).
pub(crate) fn clamp(v: usize, lo: usize, hi: usize) -> usize {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// The v1 promptui help lines plus whatever an applied search adds: a row panel keeps
/// ALL its keys and gains `"c clear"`; a View swaps in the n/p walker until Esc puts
/// the query back up (tabbed.go:883-903).
pub(crate) fn surface_hint(p: &Panel, ps: &PanelState) -> String {
    if ps.search.mode == SearchMode::Applied {
        let q = truncate_query(&ps.search.query);
        if p.kind() == PanelKind::View {
            let n = ps.search.hits.len();
            if n > 0 {
                return format!(
                    "{q} {}/{} · n next · p prev · q/Esc edit",
                    ps.search.hit_idx + 1,
                    n
                );
            }
            return format!("{q} no match · q/Esc edit");
        }
        let (shown, ok) = ps.filtered_count(p);
        let lead = if ok {
            format!("{q} {shown}/{}", ps.row_count(p))
        } else {
            format!("{q} no match")
        };
        return format!("{lead} · {} · c clear", base_hint(p, ps));
    }
    if ps.search_available(p) {
        return format!("{} · / search", base_hint(p, ps));
    }
    base_hint(p, ps)
}

/// Renders a query for the hint row, clipped to 24 runes so a long one cannot crowd
/// out the keys behind it (tabbed.go:907-914).
pub(crate) fn truncate_query(q: &str) -> String {
    const MAX: usize = 24;
    let mut s = q.to_owned();
    if q.chars().count() > MAX {
        s = q.chars().take(MAX).collect::<String>() + "…";
    }
    format!("\"{s}\"")
}

/// The per-kind base hint rows, byte-exact (tabbed.go:916-947; `·` = U+00B7).
pub(crate) fn base_hint(p: &Panel, ps: &PanelState) -> String {
    let hint = match p.kind() {
        PanelKind::Slider => "←→ adjust · g default · G max · Enter confirm · q/Esc cancel",
        PanelKind::Switch => "Space toggle · ←→ off/on · Enter confirm · q/Esc cancel",
        PanelKind::Input => "←→ move · Enter confirm · Esc cancel",
        PanelKind::List | PanelKind::Multi => {
            if ps.editing {
                "Enter confirm · Esc back to options"
            } else if p.kind() == PanelKind::Multi {
                "↑↓ move · ←→ page · Space toggle · Enter confirm · q/Esc cancel"
            } else {
                "↑↓ move · ←→ page · Enter confirm · q/Esc cancel"
            }
        }
        // tabbed.go:924 (the picker's hint; T3).
        PanelKind::Picker => "↑↓ move · ←→ page · Enter confirm · q/Esc cancel",
        PanelKind::View => {
            if ps.copied {
                "✓ copied to clipboard"
            } else if p.wrap() {
                "↑↓ scroll · ←→ page · g/G top/bottom · c copy · q/Esc close"
            } else {
                "↑↓ scroll · ←→ page · h/l pan · g/G top/bottom · c copy · q/Esc close"
            }
        }
        PanelKind::Browser => {
            "↑↓ move · ←→ page · Enter open/choose · g/G top/bottom · q/Esc cancel"
        }
    };
    hint.to_owned()
}

/// The live feedback beside the query: how much the filter keeps, or how many hits a
/// View has, plus the keys (tabbed.go:731-748).
pub(crate) fn search_typing_hint(p: &Panel, ps: &PanelState) -> String {
    let q = ps.search_draft();
    if q.is_empty() {
        return "type to search · Esc cancel".to_owned();
    }
    if p.kind() == PanelKind::View {
        let n = ps.search.hits.len();
        if n > 0 {
            return format!("{n} hits · Enter search · Esc cancel");
        }
        return "no match · Esc cancel".to_owned();
    }
    let total = ps.row_count(p);
    let (shown, ok) = ps.filtered_count(p);
    if ok {
        format!("{shown} of {total} · Enter filter · Esc cancel")
    } else {
        "no match · Esc cancel".to_owned()
    }
}

/// How far through a scrollable panel we are. Row panels measure progress through
/// what is VISIBLE, so a filter's percentage describes the filtered list rather than
/// the hidden whole (tabbed.go:952-971). Integer form of Go's
/// `int(float(pos)/float(max)*100 + 0.5)`.
pub(crate) fn scroll_percent(p: &Panel, ps: &PanelState) -> Option<usize> {
    fn pct(pos: usize, max: usize) -> usize {
        (pos * 200 + max) / (max * 2)
    }
    match p.kind() {
        PanelKind::List | PanelKind::Multi | PanelKind::Picker | PanelKind::Browser => {
            let n = ps.view.len();
            if n > panel_height(p, n) && n > 1 {
                return Some(pct(ps.view_pos(), n - 1));
            }
            None
        }
        PanelKind::View => {
            let n = if p.wrap() && ps.wrapped > 0 {
                ps.wrapped // set at render
            } else {
                ps.items.len()
            };
            let h = panel_height(p, n);
            let max_off = n.saturating_sub(h);
            if max_off > 0 {
                return Some(pct(ps.offset.min(max_off), max_off));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod slider_tests;

#[cfg(test)]
mod switch_tests;

#[cfg(test)]
mod browser_tests;

#[cfg(test)]
mod picker_tests;
