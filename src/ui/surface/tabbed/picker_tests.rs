#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP64 Picker-panel suite (internal/ui/`model_test.go:1746,1807,1825`).
//!
//! The kind's three laws: the preview pane renders BESIDE the list and its renderer is
//! called once per (selection, geometry) rather than once per frame (a multi-MB image
//! decode per keystroke would be visible lag); a terminal too narrow for two readable
//! columns drops the preview entirely rather than squeezing both; and the list column
//! starts at the same screen column on every row, because the pane is padded by DISPLAY
//! width — imgterm rows are dense with SGR, and counting those bytes as glyphs would let
//! the column drift row by row.

use std::sync::{Arc, Mutex};

use crate::text::ansi::strip_sgr;
use crate::text::width::str_width;
use crate::ui::facade::{Panel, TabbedResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::surface::{SurfaceEffect, SurfaceState};

// --- harness ----------------------------------------------------------------

struct Surf {
    st: SurfaceState,
    width: u16,
}

impl Surf {
    /// Opens `panels` at `width × height` — the `WindowSizeMsg` the Go test sends first.
    fn open(panels: Vec<Panel>, width: u16, height: u16) -> Self {
        let mut st = SurfaceState::new(false, panels);
        st.set_term_height(height);
        Self { st, width }
    }

    fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    fn content(&mut self) -> String {
        self.st.render(self.width).rows.join("\n")
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

/// A counter the preview closure bumps, so "rendered again" is observable.
#[derive(Clone, Default)]
struct Calls(Arc<Mutex<usize>>);

impl Calls {
    fn get(&self) -> usize {
        *self.0.lock().expect("calls")
    }
}

// Go: internal/ui/model_test.go:1746 TestTabbedPicker
#[test]
fn test_tabbed_picker() {
    let calls = Calls::default();
    let seen = calls.clone();
    let geometry: Arc<Mutex<Vec<(usize, usize)>>> = Arc::default();
    let geo = Arc::clone(&geometry);
    let panel = Panel::picker(
        "Pick".to_owned(),
        vec!["first".to_owned(), format!("second {}", "x".repeat(200))],
    )
    .with_details(vec!["detail-one".to_owned(), "detail-two".to_owned()])
    .with_preview(Box::new(move |index, max_cols, max_rows| {
        *seen.0.lock().expect("calls") += 1;
        geo.lock().expect("geo").push((max_cols, max_rows));
        vec![format!("\x1b[38;5;42mPREVIEW-{index}\x1b[0m")]
    }));
    let mut s = Surf::open(vec![panel], 100, 40);

    let view = s.content();
    assert!(
        view.contains("PREVIEW-0") && strip_sgr(&view).contains("first"),
        "preview and list must render side by side:\n{view}"
    );
    assert!(view.contains("detail-one"), "detail line missing:\n{view}");
    for (cols, rows) in geometry.lock().expect("geo").iter() {
        assert!(*cols > 0 && *rows > 0, "preview geometry = {cols}x{rows}");
    }

    // Re-rendering the same frame must not re-invoke the preview.
    let before = calls.get();
    let _ = s.content();
    assert_eq!(calls.get(), before, "preview re-rendered without a change");

    // Every row stays within the terminal width (padding is display-width based, so the
    // list column cannot drift).
    for line in strip_sgr(&view).lines() {
        assert!(
            str_width(line) <= 100,
            "row exceeds terminal width ({}): {line:?}",
            str_width(line)
        );
    }

    s.press(key(KeyCode::Down));
    let view = s.content();
    assert!(
        view.contains("PREVIEW-1") && view.contains("detail-two"),
        "moving the cursor must refresh preview and detail:\n{view}"
    );
    assert!(
        calls.get() > before,
        "selection change must re-render the preview"
    );

    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled && r.panels[0].cursor == 1, "commit = {r:?}");
}

// Go: internal/ui/model_test.go:1807 TestTabbedPickerNarrowFallback
#[test]
fn test_tabbed_picker_narrow_fallback() {
    // A terminal too narrow for two columns drops the preview entirely rather than
    // squeezing both into an unreadable width.
    let mut s = Surf::open(
        vec![
            Panel::picker("Pick".to_owned(), vec!["only".to_owned()])
                .with_preview(Box::new(|_, _, _| vec!["PREVIEW".to_owned()])),
        ],
        50,
        30,
    );
    let view = s.content();
    assert!(
        !view.contains("PREVIEW"),
        "narrow terminal must drop the preview:\n{view}"
    );
    assert!(
        strip_sgr(&view).contains("only"),
        "list must still render:\n{view}"
    );
}

// Go: internal/ui/model_test.go:1825 TestTabbedPickerColumnAlignment
#[test]
fn test_tabbed_picker_column_alignment() {
    // The list column starts at the SAME screen column on every row, including rows whose
    // preview cell is dense with SGR (ANSI-aware padding).
    let mut s = Surf::open(
        vec![
            Panel::picker(
                "Pick".to_owned(),
                vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
            )
            .with_preview(Box::new(|_, _, max_rows| {
                let row = format!(
                    "\x1b[48;2;10;20;30m\x1b[38;2;40;50;60m{}\x1b[0m",
                    "▀".repeat(12)
                );
                vec![row; max_rows]
            })),
        ],
        100,
        40,
    );

    let plain = strip_sgr(&s.content());
    let mut cols = Vec::new();
    for line in plain.lines() {
        if let Some(i) = line.find('▸')
            && line.contains("alpha")
        {
            cols.push(str_width(&line[..i]));
        }
        for item in ["beta", "gamma"] {
            if let Some(i) = line.find(item) {
                cols.push(str_width(&line[..i - 2]));
            }
        }
    }
    assert_eq!(cols.len(), 3, "expected 3 list rows, measured {cols:?}");
    for c in &cols[1..] {
        assert_eq!(*c, cols[0], "list column drifts across rows: {cols:?}");
    }
}

/// The preview budget is clamped against the terminal height so the inline frame keeps
/// room for the composer and the status row (tabbed.go:797-799); an unknown height (the
/// model before its first resize) skips the clamp, exactly as Go's `m.height > 0` does.
#[test]
fn preview_height_is_clamped_by_the_terminal() {
    let geo: Arc<Mutex<Vec<usize>>> = Arc::default();
    let sink = Arc::clone(&geo);
    let panel = || Panel::picker("Pick".to_owned(), vec!["a".to_owned()]);
    let mut p = panel();
    let sink2 = Arc::clone(&sink);
    p = p.with_preview(Box::new(move |_, _, rows| {
        sink2.lock().expect("geo").push(rows);
        vec!["x".to_owned()]
    }));
    // A 20-row terminal leaves 8 rows for the preview.
    let _ = Surf::open(vec![p], 100, 20).content();
    // …and an unknown height keeps the default 14.
    let mut p = panel();
    let sink3 = Arc::clone(&sink);
    p = p.with_preview(Box::new(move |_, _, rows| {
        sink3.lock().expect("geo").push(rows);
        vec!["x".to_owned()]
    }));
    let _ = Surf::open(vec![p], 100, 0).content();
    assert_eq!(*geo.lock().expect("geo"), vec![8, 14]);
}

/// The picker's hint row is byte-exact and `/` filters the rows like a List does
/// (search.go:336, tabbed.go:924).
#[test]
fn picker_hint_and_search_join_the_row_panels() {
    let items: Vec<String> = (0..30).map(|i| format!("row-{i}")).collect();
    let mut s = Surf::open(
        vec![Panel::picker("Pick".to_owned(), items).with_search(true)],
        100,
        40,
    );
    let plain = strip_sgr(&s.content());
    assert!(
        plain.contains("↑↓ move · ←→ page · Enter confirm · q/Esc cancel · / search"),
        "hint row wrong:\n{plain}"
    );
    // The scroll percentage joins the row panels too (tabbed.go:954).
    assert!(plain.contains("· 0%"), "scroll percent missing:\n{plain}");

    s.press(key(KeyCode::Char('/')));
    for c in "row-17".chars() {
        s.press(key(KeyCode::Char(c)));
    }
    s.press(key(KeyCode::Enter));
    let plain = strip_sgr(&s.content());
    assert!(plain.contains("row-17"), "{plain}");
    assert!(
        !plain.contains("row-18"),
        "the filter must hide the rest:\n{plain}"
    );
    // The commit returns the ORIGINAL index, not the filtered row (the engine's contract).
    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(r.panels[0].cursor, 17);
}

/// A Picker without a preview closure degrades to a plain single-select list.
#[test]
fn picker_without_a_preview_is_a_plain_list() {
    let mut s = Surf::open(
        vec![Panel::picker(
            "Pick".to_owned(),
            vec!["one".to_owned(), "two".to_owned()],
        )],
        100,
        40,
    );
    let plain = strip_sgr(&s.content());
    let rows: Vec<&str> = plain.lines().collect();
    assert!(rows.iter().any(|l| l.trim_end() == "▸ one"), "{rows:?}");
    assert!(rows.iter().any(|l| l.trim_end() == "  two"), "{rows:?}");
}
