#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP47 surface suite (internal/ui/`{model_test,tabbed_test}`.go ports): the pure
//! `surface_key` ladder, the T1 panel kinds (List/Multi/View/Input), commit-all + the
//! `enter_advances` wizard + the inline `"Other…"` editor, the shared `input_field`
//! pan window, the live-refresh pass, the byte-exact hint rows
//! and chips, and `run_surface`'s one-shot contract.
//!
//! Layers 1–2 only: keys go through `surface_key` exactly as the loop routes them and
//! rows come from `render_surface` at 80 columns (Go's `content(m)` narrowed to the
//! surface block — the frame around it belongs to WP44/WP46, and the facade's
//! reply/ESC-never-fires-a-scope halves to WP45's `tests/facade.rs`).
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/surface.rs` of the terminal crate; merged 2026-09-02).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::text::ansi::{ansi_width, strip_sgr};
use crate::text::width::str_width;
use crate::ui::facade::{
    ListBody, Panel, PanelBody, PanelKind, PickerBody, RefreshFn, TabbedResult, TabbedSpec,
    ViewBody,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::ui::surface::field::{Field, input_field};
use crate::ui::surface::tabbed::{
    PanelState, base_hint, panel_height, scroll_percent, surface_hint,
};
use crate::ui::surface::{SurfaceEffect, SurfaceState};
use crate::ui::theme::{CYAN, FAINT, GREEN, RESET, REV_ON, input_bg};

// --- harness ----------------------------------------------------------------

/// One open surface driven exactly as the loop drives it.
struct Surf {
    st: SurfaceState,
}

impl Surf {
    fn open(panels: Vec<Panel>) -> Self {
        Self::wizard(false, panels)
    }

    /// `enter_advances` = the ask-wizard shape.
    fn wizard(enter_advances: bool, panels: Vec<Panel>) -> Self {
        let st = SurfaceState::new(enter_advances, panels);
        Self { st }
    }

    fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    /// Presses a key that must leave the surface open.
    fn tap(&mut self, k: KeyEvent) {
        assert!(
            !matches!(self.press(k), SurfaceEffect::Close(_)),
            "key closed the surface unexpectedly"
        );
    }

    fn typed(&mut self, s: &str) {
        for c in s.chars() {
            self.tap(ch(c));
        }
    }

    fn ps(&self, i: usize) -> &PanelState {
        &self.st.slots[i].state
    }

    fn rows(&mut self) -> Vec<String> {
        self.st.render(80).rows
    }

    fn content(&mut self) -> String {
        self.rows().join("\n")
    }

    fn plain(&mut self) -> String {
        strip_sgr(&self.content())
    }

    /// The trailing hint row (or the query field that replaces it).
    fn hint(&mut self) -> String {
        strip_sgr(&self.rows().pop().unwrap_or_default())
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A text key as crossterm delivers it (uppercase carries SHIFT).
fn ch(c: char) -> KeyEvent {
    let m = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), m)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

fn list(title: &str, items: &[&str]) -> Panel {
    Panel::list(
        title.to_owned(),
        items.iter().map(|s| (*s).to_owned()).collect(),
    )
}

fn rows_of(n: usize, f: impl Fn(usize) -> String) -> Vec<String> {
    (0..n).map(f).collect()
}

// --- inputField: the pan window (tabbed_test.go, the file's whole reason) ------

/// A focused real-cursor field, styled like the panels build them (no prompt, no
/// internal SGR) — Go `newTestInput`.
fn new_test_input(value: &str, cursor: usize) -> Field {
    let mut f = Field::new();
    f.set_value(value);
    f.set_cursor_runes(cursor);
    f
}

/// A value that fits leaves the window alone: no pan, cursor column == the value's
/// display width up to the cursor.
// Go: internal/ui/tabbed_test.go:26 TestInputFieldFits
#[test]
fn test_input_field_fits() {
    const BOX_W: usize = 20;
    let f = new_test_input("short", 5);
    let mut off = 0;
    let (view, cur) = input_field(&f, &mut off, BOX_W);
    assert_eq!(off, 0, "value fits: the window must not pan");
    assert_eq!(cur, 5);
    // The field draws a blank cell where the cursor sits, so the rendered field runs
    // one column past the value.
    assert_eq!(strip_sgr(&view), "short ");
}

/// The regression this file exists for: with a value longer than the box the cursor
/// must stay INSIDE the box (the field's own scrolling would leave it at the value's
/// absolute column — far right of the field, or past the end of the row).
// Go: internal/ui/tabbed_test.go:48 TestInputFieldLongValueKeepsCursorInsideBox
#[test]
fn test_input_field_long_value_keeps_cursor_inside_box() {
    const BOX_W: usize = 10;
    let value = "abcdefghij".repeat(5); // 50 columns
    let f = new_test_input(&value, value.chars().count());
    let mut off = 0;
    let (view, cur) = input_field(&f, &mut off, BOX_W);

    assert!(cur < BOX_W, "cursor col {cur} outside the box");
    assert!(
        ansi_width(&view) <= BOX_W,
        "view is {} columns wide",
        ansi_width(&view)
    );
    // The window shows the tail of the value plus the cursor's own cell.
    let want = format!("{} ", &value[value.len() - (BOX_W - 1)..]);
    assert_eq!(strip_sgr(&view), want);
}

/// Panning is two-way: walking the cursor back to the start scrolls the window home
/// again (the old code could only ever look right).
// Go: internal/ui/tabbed_test.go:69 TestInputFieldPansBothWays
#[test]
fn test_input_field_pans_both_ways() {
    const BOX_W: usize = 10;
    let value = "abcdefghij".repeat(5);
    let mut f = new_test_input(&value, value.chars().count());
    let mut off = 0;
    input_field(&f, &mut off, BOX_W);
    assert_ne!(off, 0, "window never panned right on a long value");

    f.set_cursor_runes(0);
    let (view, cur) = input_field(&f, &mut off, BOX_W);
    assert_eq!((off, cur), (0, 0), "cursor returned home");
    assert_eq!(strip_sgr(&view), value[..BOX_W]);

    // Mid-value: the cursor stays visible and the window shows what surrounds it.
    f.set_cursor_runes(25);
    let (_, cur) = input_field(&f, &mut off, BOX_W);
    assert!(cur < BOX_W, "mid-value cursor col {cur}");
    assert_eq!(
        off + cur,
        25,
        "window start + cursor col must be the absolute column"
    );
}

/// Wide (CJK) runes count as two columns on both sides of the arithmetic: the window
/// start and the cursor column.
// Go: internal/ui/tabbed_test.go:101 TestInputFieldWideRunes
#[test]
fn test_input_field_wide_runes() {
    const BOX_W: usize = 10;
    let value = "宽字".repeat(8); // 32 columns, 16 runes
    let f = new_test_input(&value, 16);
    let mut off = 0;
    let (view, cur) = input_field(&f, &mut off, BOX_W);

    assert!(
        ansi_width(&view) <= BOX_W,
        "view is {} columns wide",
        ansi_width(&view)
    );
    assert!(cur < BOX_W, "cursor col {cur} outside the box");
    assert_eq!(
        off + cur,
        32,
        "window start + cursor col must be the value's 32 columns"
    );
}

/// The window never pans past the end: a value that shrinks (backspace) must not leave
/// blank columns inside the box while text sits off to the left.
// Go: internal/ui/tabbed_test.go:121 TestInputFieldNeverPansPastTheEnd
#[test]
fn test_input_field_never_pans_past_the_end() {
    const BOX_W: usize = 10;
    let mut f = new_test_input(&"x".repeat(40), 40);
    let mut off = 0;
    input_field(&f, &mut off, BOX_W);

    f.set_value(&"x".repeat(12)); // set_value parks the cursor at the end
    let (view, cur) = input_field(&f, &mut off, BOX_W);
    assert_eq!(
        off,
        12 + 1 - BOX_W,
        "off must pin to the shortened value's end"
    );
    assert_eq!(ansi_width(&view), BOX_W, "want a full box");
    assert_eq!(off + cur, 12);
}

// --- commit-all, the wizard, and the inline Custom editor ---------------------

/// Three tabs, Tab navigation, and a single Enter commits ALL of them.
// Go: internal/ui/model_test.go:789 TestTabbedCommitAll
#[test]
fn test_tabbed_commit_all() {
    let mut s = Surf::open(vec![
        list("Model", &["a", "b"]),
        Panel::multi(
            "Flags".to_owned(),
            vec!["x".to_owned(), "y".to_owned(), "z".to_owned()],
        ),
        Panel::slider("Temp".to_owned(), 0.0, 2.0, 0.1, None),
    ]);

    s.tap(key(KeyCode::Down)); // Model → b
    s.tap(key(KeyCode::Tab)); // → Flags
    s.tap(key(KeyCode::Down)); // cursor y
    s.tap(ch(' '));
    s.tap(key(KeyCode::Tab)); // → Temp
    s.tap(key(KeyCode::Right)); // default → 0.0
    s.tap(key(KeyCode::Right)); // 0.1

    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled);
    assert_eq!(r.focused, 2);
    assert_eq!(r.panels[0].cursor, 1);
    assert_eq!(r.panels[1].checked, vec![1]);
    assert_eq!(r.panels[2].value, Some(0.1));
}

/// `enter_advances`: Enter on a non-last tab moves to the NEXT tab instead of
/// committing — unvisited questions must not be silently submitted with defaults; only
/// the last tab's Enter commits all.
// Go: internal/ui/model_test.go:1558 TestTabbedEnterAdvances
#[test]
fn test_tabbed_enter_advances() {
    let mut s = Surf::wizard(
        true,
        vec![
            list("Q1", &["a", "b"]),
            Panel::input("Q2".to_owned(), String::new(), String::new()),
        ],
    );

    s.tap(key(KeyCode::Down)); // pick "b"
    s.tap(key(KeyCode::Enter)); // NOT the last tab: advance, no commit
    assert_eq!(s.st.focus, 1, "Enter on a non-last tab must advance");

    s.typed("custom");
    let r = closed(s.press(key(KeyCode::Enter))); // last tab: commits all
    assert!(!r.cancelled);
    assert_eq!(r.panels[0].cursor, 1);
    assert_eq!(r.panels[1].text, "custom");
}

/// The inline Custom editor: Enter on `"Other…"` opens it IN PLACE; ESC closes just the
/// editor (the ask survives); Enter with text proceeds — a single-select advances the
/// wizard with the custom answer, a Multi checks the row and stays for more toggles.
// Go: internal/ui/model_test.go:1589 TestTabbedInlineCustom
#[test]
fn test_tabbed_inline_custom() {
    let mut s = Surf::wizard(
        true,
        vec![
            list("Q1", &["a", "b"]).with_custom(true),
            Panel::multi("Q2".to_owned(), vec!["x".to_owned(), "y".to_owned()]).with_custom(true),
        ],
    );

    // Q1: cursor to "Other…" (index 2), Enter opens the editor.
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Enter));
    assert!(
        s.ps(0).editing,
        "Enter on Other must open the inline editor"
    );
    // The editor renders in place, with its own hint row.
    assert!(s.plain().contains("your answer"), "{}", s.plain());
    assert_eq!(s.hint(), "Tab switch · Enter confirm · Esc back to options");

    // ESC closes JUST the editor.
    s.tap(key(KeyCode::Esc));
    assert!(
        !s.ps(0).editing,
        "ESC in the editor must return to the options, not decline the ask"
    );

    // Reopen, type, Enter → the wizard advances to Q2 with the custom answer.
    s.tap(key(KeyCode::Enter));
    s.typed("zig");
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.st.focus, 1, "custom confirm on a non-last tab advances");

    // Q2 (multi): Space on "Other…" opens the editor; Enter checks the row and stays.
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    s.tap(ch(' '));
    assert!(
        s.ps(1).editing,
        "Space on an unchecked Other must open the editor"
    );
    s.typed("bird");
    s.tap(key(KeyCode::Enter));
    assert!(
        s.ps(1).checked.contains(&2),
        "a confirmed custom must check the Other row"
    );

    // Enter on the Other row WITH text behaves like a normal row: commit (last tab)
    // instead of reopening the editor — "type, Enter, Enter" is the natural flow.
    s.tap(key(KeyCode::Up));
    s.tap(key(KeyCode::Up));
    s.tap(ch(' ')); // check "x"
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled);
    assert_eq!(r.panels[0].cursor, 2);
    assert_eq!(r.panels[0].custom, "zig");
    assert_eq!(r.panels[0].text, "", "a Custom panel forces Text empty");
    assert_eq!(r.panels[1].custom, "bird");
    assert_eq!(r.panels[1].checked, vec![0, 2]);
}

/// Multi + Custom Space semantics: Space on a CHECKED Other unchecks it and keeps the
/// draft text (check-by-editing runs only from the unchecked state).
// Go: internal/ui/model.go:761-788 (Space arm, Multi + Custom)
#[test]
fn multi_custom_space_unchecks_and_keeps_the_draft() {
    let mut s = Surf::open(vec![
        Panel::multi("Q".to_owned(), vec!["x".to_owned()]).with_custom(true),
    ]);
    s.tap(key(KeyCode::Down)); // → Other…
    s.tap(ch(' ')); // opens the editor
    s.typed("owl");
    s.tap(key(KeyCode::Enter)); // checks the row
    assert!(s.ps(0).checked.contains(&1));

    s.tap(ch(' ')); // unchecks; the draft stays
    assert!(!s.ps(0).checked.contains(&1));
    assert!(s.plain().contains("Other: owl"));
    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(r.panels[0].custom, "owl");
    assert!(r.panels[0].checked.is_empty());
}

// --- the Input panel ---------------------------------------------------------

/// Letter keys type (q must not cancel, g/j/k must not navigate), the real cursor sits
/// in the field, a paste flattens to one line, the value survives a Tab round trip
/// while list keys resume on the other tab, long content scrolls inside the box, and
/// Enter commits every tab alongside the text.
// Go: internal/ui/model_test.go:1438 TestInputPanel
#[test]
fn test_input_panel() {
    let mut s = Surf::open(vec![
        Panel::input("Model".to_owned(), String::new(), "model name".to_owned())
            .with_input_width(10),
        list("Other", &["a", "b"]),
    ]);

    s.typed("qgjk");
    assert_eq!(s.ps(0).input.value(), "qgjk", "letters must type");

    // The real cursor is inside the field (the composer's is suppressed while a
    // surface is open). The render records it in SURFACE-BLOCK coordinates, exactly as
    // Go's View() does; `event_loop::frame_view` adds the composer block's own height
    // (asserted there by `surface_field_cursor_offset_follows_the_composer_block`).
    let (_x, y) =
        s.st.render(80)
            .cursor
            .expect("no real cursor in the input field");
    assert!(y > 0, "cursor parked on the tab bar row");

    // Paste flattens to a single line.
    s.st.paste("-multi\nline");
    assert_eq!(s.ps(0).input.value(), "qgjk-multi line");

    // Tab away and back keeps the value; list keys navigate again meanwhile.
    s.tap(key(KeyCode::Tab));
    s.tap(ch('j'));
    assert_eq!(s.ps(1).cursor, 1, "j must navigate the list");
    s.tap(key(KeyCode::Tab));
    assert_eq!(s.ps(0).input.value(), "qgjk-multi line");

    // Long content scrolls horizontally: the box row stays bounded.
    s.typed("-0123456789abcdef");
    let mut saw = false;
    for l in s.plain().lines() {
        if l.contains("def") {
            saw = true;
            assert!(
                str_width(l) <= 10 + 6,
                "input row wider than the box: {l:?}"
            );
        }
    }
    assert!(saw, "the field tail is not visible:\n{}", s.plain());
    assert!(
        s.content().contains(input_bg(true)),
        "input row lost its background styling"
    );

    // Enter commits ALL tabs; the text lands in PanelResult.text.
    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled);
    assert_eq!(r.panels[0].text, "qgjk-multi line-0123456789abcdef");
    assert_eq!(r.panels[1].cursor, 1, "the list tab commits alongside");
}

/// Esc (and Ctrl+C) still cancel while typing.
// Go: internal/ui/model_test.go:1500 TestInputPanelEscCancels
#[test]
fn test_input_panel_esc_cancels() {
    let panel = || Panel::input("Model".to_owned(), String::new(), String::new());
    let mut s = Surf::open(vec![panel()]);
    s.typed("abc");
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);

    let mut s = Surf::open(vec![panel()]);
    s.typed("abc");
    assert!(closed(s.press(ctrl('c'))).cancelled);
}

/// The field's colour contract: an adaptive background shade (per detected terminal
/// tone), typed text in the DEFAULT foreground (no reverse video, no fg recolor), and
/// the placeholder faint on the same background.
// Go: internal/ui/model_test.go:1517 TestInputPanelColors
#[test]
fn test_input_panel_colors() {
    let mut s = Surf::open(vec![
        Panel::input("Model".to_owned(), String::new(), "hint".to_owned()).with_input_width(12),
    ]);

    // Placeholder: faint, on the dark-side shade by default.
    let c = s.content();
    assert!(
        c.contains("\x1b[48;5;236m"),
        "dark-side shade missing: {c:?}"
    );
    assert!(
        c.contains(&format!("{FAINT}hint")),
        "placeholder not faint: {c:?}"
    );

    // Typed text: bare runes right after the background + pad — no reverse, no fg
    // colour, no faint.
    s.typed("abc");
    let c = s.content();
    assert!(
        c.contains("\x1b[48;5;236m abc"),
        "typed text not default-foreground on the shade: {c:?}"
    );
    assert!(!c.contains(&format!("{REV_ON} abc")), "{c:?}");
    assert!(!c.contains(&format!("{FAINT}abc")), "{c:?}");

    // Light backgrounds flip to the light-side shade.
    s.st.set_dark(false);
    assert!(
        s.content().contains("\x1b[48;5;254m"),
        "light-side shade missing"
    );
}

// --- the View panel ----------------------------------------------------------

/// A Height-5 viewer windows lines `[0,5)`; Down scrolls by one; `q` closes.
// Go: internal/ui/model_test.go:557 TestViewerScrolls
#[test]
fn test_viewer_scrolls() {
    let lines = rows_of(30, |i| {
        format!("xxx{}", char::from(b'A' + u8::try_from(i % 26).unwrap()))
    });
    let mut s = Surf::open(vec![
        Panel::view("/status".to_owned(), lines.clone()).with_height(5),
    ]);

    let c = s.plain();
    assert!(
        c.contains(&lines[0]) && !c.contains(&lines[6]),
        "window:\n{c}"
    );
    s.tap(key(KeyCode::Down));
    assert!(s.plain().contains(&lines[5]), "viewer did not scroll");
    assert!(closed(s.press(ch('q'))).cancelled);
}

/// `G` jumps to the bottom (offset, not cursor), `g` returns to the top, ←→ and
/// Ctrl+B/F page, Space pages forward, `h`/`l` pan a non-wrap view — and `c` on a
/// non-View panel must NOT cancel the surface (regression).
// Go: internal/ui/model_test.go:903 TestViewKeysV1Parity
#[test]
fn test_view_keys_v1_parity() {
    let lines = rows_of(40, |i| {
        format!("row-{i:02} with some very long tail content {i}")
    });
    let mut s = Surf::open(vec![Panel::view("v".to_owned(), lines).with_height(5)]);

    s.tap(ch('G'));
    assert!(s.plain().contains("row-39"), "G did not reach the bottom");
    s.tap(ch('g'));
    assert!(s.plain().contains("row-00"), "g did not return to the top");

    s.tap(key(KeyCode::Right));
    let c = s.plain();
    assert!(
        c.contains("row-05") && !c.contains("row-00"),
        "→ page:\n{c}"
    );
    s.tap(ctrl('b'));
    assert!(s.plain().contains("row-00"), "Ctrl+B did not page back");
    s.tap(ctrl('f'));
    assert!(s.plain().contains("row-05"), "Ctrl+F did not page forward");
    s.tap(ch('b'));
    assert!(s.plain().contains("row-00"), "b did not page back");
    s.tap(ch(' '));
    assert!(s.plain().contains("row-05"), "Space did not page forward");

    // h/l pan a non-wrap view horizontally.
    s.tap(ch('l'));
    assert!(
        s.rows()[1].starts_with("ow-05"),
        "l did not pan right: {:?}",
        s.rows()[1]
    );
    s.tap(ch('h'));
    assert!(s.rows()[1].starts_with("row-05"), "h did not pan back");
    assert!(closed(s.press(ch('q'))).cancelled);

    // Regression: "c" on a List panel must not cancel.
    let mut s = Surf::open(vec![list("t", &["x", "y"])]);
    assert!(
        !matches!(s.press(ch('c')), SurfaceEffect::Close(_)),
        "'c' on a list cancelled the surface"
    );
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);
}

/// A Wrap view scrolled so only continuation rows show still renders them faint (the
/// `/tools` MCP-tab bug: `wrap_ansi` re-opens the carried SGR on every row).
// Go: internal/ui/model_test.go:1300 TestViewScrollKeepsWrappedStyle
#[test]
fn test_view_scroll_keeps_wrapped_style() {
    let long = format!("{FAINT}{}{RESET}", "methods word ".repeat(30));
    let mut s = Surf::open(vec![
        Panel::view("MCP".to_owned(), vec![long, "tail".to_owned()])
            .with_height(3)
            .with_wrap(true),
    ]);

    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    let body: Vec<String> = s
        .rows()
        .into_iter()
        .filter(|l| l.contains("methods word"))
        .collect();
    assert!(
        !body.is_empty(),
        "no wrapped rows visible:\n{}",
        s.content()
    );
    for l in &body {
        assert!(
            l.contains(FAINT),
            "visible continuation row lost faint: {l:?}"
        );
    }
}

// --- row panels: paging, highlight, chips ------------------------------------

/// ←→ page a list by its visible height.
// Go: internal/ui/model_test.go:956 TestListPagingParity
#[test]
fn test_list_paging_parity() {
    let items = rows_of(30, |i| format!("item-{i:02}"));
    let mut s = Surf::open(vec![Panel::list("l".to_owned(), items).with_height(6)]);
    let _ = s.rows(); // render once to record the page size
    s.tap(key(KeyCode::Right));
    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(r.panels[0].cursor, 6, "→ moves the cursor one visible page");
}

/// A plain cursor row is recoloured cyan; a row carrying its own SGR keeps it (marker
/// only); the tab-bar width is identical across focus switches.
// Go: internal/ui/model_test.go:1164 TestCursorRowHighlight
#[test]
fn test_cursor_row_highlight() {
    let mut s = Surf::open(vec![
        Panel::list(
            "A".to_owned(),
            vec![
                "plain-row".to_owned(),
                "\x1b[32mstyled\x1b[0m-row".to_owned(),
            ],
        ),
        Panel::view("B".to_owned(), vec!["x".to_owned()]),
    ]);

    let c = s.content();
    assert!(
        c.contains(&format!("{CYAN}plain-row{RESET}")),
        "plain cursor row not highlighted:\n{c:?}"
    );
    s.tap(key(KeyCode::Down));
    assert!(
        !s.content().contains(&format!("{CYAN}\x1b[32mstyled")),
        "a styled row must keep its own colours, marker only"
    );
    assert!(
        s.content().contains(&format!("{CYAN}▸ {RESET}")),
        "the cursor marker is missing"
    );

    // The tab-bar width is focus-invariant (both chip states carry " title " padding).
    let bar_width = |s: &mut Surf| str_width(&strip_sgr(&s.rows()[0]));
    let w1 = bar_width(&mut s);
    s.tap(key(KeyCode::Tab));
    assert_eq!(
        bar_width(&mut s),
        w1,
        "tab bar width changed on focus switch"
    );
    assert!(w1 > 0);
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);
}

/// Chips and checkboxes, byte-exact (tabbed.go:460-472, 493-499): a multi-panel bar
/// joins `" Title "` chips with the faint `" │ "`, the focused one reverse-video; a
/// single-panel surface renders its title as a lone FOCUSED chip (no faint dashes);
/// Multi rows carry faint `"[ ] "` / green `"[x] "`.
// Go: internal/ui/model_test.go:1204 TestSliderProgressBarAndChipTitle (chip half) +
// tabbed.go:454-499 markers
#[test]
fn chips_and_checkbox_glyphs_are_byte_exact() {
    let mut s = Surf::open(vec![list("Temperature", &["a"])]);
    assert_eq!(s.rows()[0], format!("{REV_ON} Temperature {RESET}"));
    assert!(!s.plain().contains("── Temperature"));

    let mut s = Surf::open(vec![
        Panel::multi("Flags".to_owned(), vec!["x".to_owned(), "y".to_owned()]),
        list("B", &["b"]),
    ]);
    assert_eq!(
        s.rows()[0],
        format!("{REV_ON} Flags {RESET}{FAINT} │ {RESET}{FAINT} B {RESET}")
    );
    assert!(s.content().contains(&format!("{FAINT}[ ] {RESET}")));
    s.tap(ch(' '));
    assert!(s.content().contains(&format!("{GREEN}[x] {RESET}")));
    s.tap(key(KeyCode::Tab));
    assert_eq!(
        s.rows()[0],
        format!("{FAINT} Flags {RESET}{FAINT} │ {RESET}{REV_ON} B {RESET}")
    );
}

// --- live refresh -------------------------------------------------------------

/// Panels with a `refresh` closure update their rows on every tick; a panel without
/// one is untouched. The STALE-generation half of the Go test lives in the loop
/// (`event_loop::tick_surface_refresh` — WP44's file guards `surface.generation ==
/// surface_gen` before ever calling here), so it is not reachable from this layer.
// Go: internal/ui/model_test.go:873 TestSurfaceLiveRefresh
#[test]
fn test_surface_live_refresh() {
    let n = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&n);
    let refresh: RefreshFn = Box::new(move || {
        let k = counter.fetch_add(1, Ordering::Relaxed) + 1;
        vec![format!("tick {k}")]
    });
    let mut s = Surf::open(vec![
        Panel::view("Live".to_owned(), Vec::new()).with_refresh(refresh),
        Panel::view("Static".to_owned(), vec!["frozen".to_owned()]),
    ]);

    s.st.tick();
    assert!(s.plain().contains("tick 1"), "refresh not applied");
    s.st.tick();
    assert!(s.plain().contains("tick 2"), "second refresh not applied");
    assert_eq!(n.load(Ordering::Relaxed), 2);
    assert_eq!(
        s.ps(1).items,
        vec!["frozen".to_owned()],
        "a panel without a refresh closure must not be touched"
    );
}

/// A refresh that shortens the list clamps the cursor and re-filters against the new
/// content (rows grown under an applied query are judged by it too).
// Go: internal/ui/tabbed.go:318-341 surfTickMsg (clamp + rebuildView + syncCursor)
#[test]
fn refresh_clamps_the_cursor_and_refilters() {
    let live = Arc::new(Mutex::new(rows_of(40, |i| format!("item-{i:02}"))));
    let src = Arc::clone(&live);
    let refresh: RefreshFn =
        Box::new(move || src.lock().unwrap_or_else(PoisonError::into_inner).clone());
    let mut s = Surf::open(vec![
        Panel::list("Live".to_owned(), rows_of(40, |i| format!("item-{i:02}")))
            .with_search(true)
            .with_refresh(refresh),
    ]);

    s.tap(ch('/'));
    s.typed("item-3");
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.ps(0).view.len(), 10);

    // The list shrinks under an applied filter: the view narrows with it and the
    // cursor is pulled back onto a visible row.
    *live.lock().unwrap_or_else(PoisonError::into_inner) = rows_of(32, |i| format!("item-{i:02}"));
    s.st.tick();
    assert_eq!(s.ps(0).items.len(), 32);
    assert_eq!(s.ps(0).view, vec![30, 31]);
    assert!(s.ps(0).view.contains(&s.ps(0).cursor));
}

// --- the surface_key ladder ---------------------------------------------------

/// The strict-precedence ladder (`TUI_CONTRACTS` §6): an open Custom editor and an open
/// query field own the keyboard (letters TYPE, only Ctrl+C escapes); an Input panel
/// keeps only Ctrl+C/Tab/Esc/Enter; the Ctrl chords mirror ↑↓/←→; Tab wraps the focus
/// and always forces a repaint (T-32); Esc/q cancel.
// Go: internal/ui/model.go:587-876 surfaceKey (ladder rows 1-7)
#[test]
fn surface_key_ladder_precedence() {
    // Row 7 (the baseline): q and Esc cancel a row panel.
    let mut s = Surf::open(vec![list("A", &["x", "y"])]);
    assert!(closed(s.press(ch('q'))).cancelled);
    let mut s = Surf::open(vec![list("A", &["x", "y"])]);
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);
    let mut s = Surf::open(vec![list("A", &["x", "y"])]);
    assert!(closed(s.press(ctrl('c'))).cancelled);

    // Row 1: the editor swallows q/Esc-as-text; Ctrl+C still cancels the surface.
    let custom = || list("A", &["x"]).with_custom(true);
    let mut s = Surf::open(vec![custom()]);
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Enter)); // open the editor
    s.typed("q");
    assert_eq!(s.ps(0).input.value(), "q", "q must type inside the editor");
    assert!(closed(s.press(ctrl('c'))).cancelled);

    // Row 2: the query field swallows q; Ctrl+C cancels the surface.
    let searchable =
        || Panel::list("A".to_owned(), rows_of(40, |i| format!("item-{i:02}"))).with_search(true);
    let mut s = Surf::open(vec![searchable()]);
    s.tap(ch('/'));
    s.typed("q");
    assert_eq!(s.ps(0).search.input.value(), "q");
    assert!(closed(s.press(ctrl('c'))).cancelled);

    // Row 5: Ctrl+P/N mirror ↑↓ on a row panel.
    let mut s = Surf::open(vec![list("A", &["x", "y", "z"])]);
    s.tap(ctrl('n'));
    s.tap(ctrl('n'));
    assert_eq!(s.ps(0).cursor, 2);
    s.tap(ctrl('p'));
    assert_eq!(s.ps(0).cursor, 1);

    // Row 6: Tab wraps AND always forces a full repaint (the T-32 CJK insurance).
    let mut s = Surf::open(vec![list("A", &["x"]), list("B", &["y"])]);
    assert!(matches!(
        s.press(key(KeyCode::Tab)),
        SurfaceEffect::ForceRedraw
    ));
    assert_eq!(s.st.focus, 1);
    assert!(matches!(
        s.press(key(KeyCode::Tab)),
        SurfaceEffect::ForceRedraw
    ));
    assert_eq!(s.st.focus, 0, "Tab wraps");
}

/// Only `Press` events route; a key repeat/release is inert. An empty spec closes
/// immediately rather than indexing a panel that is not there.
// Go: internal/ui/model.go:377-393 updateKey (bubbletea delivers presses only)
#[test]
fn surface_key_ignores_non_press_and_empty_specs() {
    let mut s = Surf::open(vec![list("A", &["x", "y"])]);
    let release = KeyEvent::new_with_kind_and_state(
        KeyCode::Esc,
        KeyModifiers::NONE,
        KeyEventKind::Release,
        KeyEventState::NONE,
    );
    assert!(matches!(s.press(release), SurfaceEffect::None));
    assert_eq!(s.ps(0).cursor, 0);

    let mut empty = Surf::open(Vec::new());
    assert!(closed(empty.press(ch('x'))).cancelled);
}

// --- geometry + hint rows ------------------------------------------------------

/// `panel_height`: the `height` override, else 10 (View 15), clamped to the item count,
/// minimum 1.
// Go: internal/ui/tabbed.go:431-446 panelHeight
#[test]
fn panel_height_defaults_and_clamps() {
    let l = list("A", &[]);
    assert_eq!(panel_height(&l, 40), 10);
    assert_eq!(panel_height(&l, 3), 3);
    assert_eq!(panel_height(&l, 0), 1);
    let v = Panel::view("", Vec::new());
    assert_eq!(panel_height(&v, 40), 15);
    let h = list("A", &[]).with_height(6);
    assert_eq!(panel_height(&h, 40), 6);
    assert_eq!(panel_height(&h, 2), 2);
}

/// The 12 base hint rows, byte-exact (tabbed.go:916-947; `·` = U+00B7), plus the
/// composition around them: the multi-panel `"Tab switch · "` prefix, the `" · N%"`
/// scroll suffix, and the `" · / search"` affordance.
// Go: internal/ui/tabbed.go:883-947 surfaceHint/baseHint (+ TUI_CONTRACTS §10)
#[test]
fn hint_rows_are_byte_exact() {
    let hint = |p: &Panel, f: &dyn Fn(&mut PanelState)| -> String {
        let mut st = SurfaceState::new(false, vec![clone_shape(p)]);
        f(&mut st.slots[0].state);
        let slot = &st.slots[0];
        base_hint(&slot.spec, &slot.state)
    };
    let noop = |_: &mut PanelState| {};

    let kinds: &[(PanelKind, &str)] = &[
        (
            PanelKind::Slider,
            "←→ adjust · g default · G max · Enter confirm · q/Esc cancel",
        ),
        (
            PanelKind::Switch,
            "Space toggle · ←→ off/on · Enter confirm · q/Esc cancel",
        ),
        (PanelKind::Input, "←→ move · Enter confirm · Esc cancel"),
        (
            PanelKind::Multi,
            "↑↓ move · ←→ page · Space toggle · Enter confirm · q/Esc cancel",
        ),
        (
            PanelKind::List,
            "↑↓ move · ←→ page · Enter confirm · q/Esc cancel",
        ),
        (
            PanelKind::View,
            "↑↓ scroll · ←→ page · h/l pan · g/G top/bottom · c copy · q/Esc close",
        ),
        (
            PanelKind::Browser,
            "↑↓ move · ←→ page · Enter open/choose · g/G top/bottom · q/Esc cancel",
        ),
    ];
    for (kind, want) in kinds {
        let p = Panel::of("", PanelBody::empty(*kind));
        assert_eq!(&hint(&p, &noop), want, "{kind:?}");
    }
    // The state-dependent rows.
    let l = Panel::list("", Vec::new());
    assert_eq!(
        hint(&l, &|ps: &mut PanelState| ps.editing = true),
        "Enter confirm · Esc back to options"
    );
    let wrapped = Panel::view("", Vec::new()).with_wrap(true);
    assert_eq!(
        hint(&wrapped, &noop),
        "↑↓ scroll · ←→ page · g/G top/bottom · c copy · q/Esc close"
    );
    let v = Panel::view("", Vec::new());
    assert_eq!(
        hint(&v, &|ps: &mut PanelState| ps.copied = true),
        "✓ copied to clipboard"
    );

    // The affordance suffix appears exactly when '/' is live.
    let overflowing = Panel::list("", rows_of(40, |i| format!("item-{i:02}"))).with_search(true);
    let st = SurfaceState::new(false, vec![overflowing]);
    assert_eq!(
        surface_hint(&st.slots[0].spec, &st.slots[0].state),
        "↑↓ move · ←→ page · Enter confirm · q/Esc cancel · / search"
    );

    // The composition: multi-panel prefix + scroll percent, in that order.
    let mut s = Surf::open(vec![
        Panel::list("A".to_owned(), rows_of(40, |i| format!("item-{i:02}"))),
        list("B", &["y"]),
    ]);
    let _ = s.rows(); // the percent reads the rendered geometry
    assert_eq!(
        s.hint(),
        "Tab switch · ↑↓ move · ←→ page · Enter confirm · q/Esc cancel · 0%"
    );
    assert_eq!(scroll_percent(&s.st.slots[0].spec, s.ps(0)), Some(0));
}

/// `scroll_percent`: row panels measure progress through what is VISIBLE, a View
/// through its (wrapped) rows; a panel that fits shows nothing.
// Go: internal/ui/tabbed.go:952-971 scrollPercent
#[test]
fn scroll_percent_tracks_the_visible_list() {
    let mut s = Surf::open(vec![Panel::list(
        "A".to_owned(),
        rows_of(11, |i| format!("item-{i:02}")),
    )]);
    let _ = s.rows();
    assert_eq!(scroll_percent(&s.st.slots[0].spec, s.ps(0)), Some(0));
    s.tap(ch('G'));
    assert_eq!(scroll_percent(&s.st.slots[0].spec, s.ps(0)), Some(100));

    // A list that fits its window has nothing to report.
    let short = Surf::open(vec![list("A", &["x", "y"])]);
    assert_eq!(scroll_percent(&short.st.slots[0].spec, short.ps(0)), None);
}

// --- the one-shot surface -----------------------------------------------------

/// `run_surface`'s spec contract: an empty spec resolves CANCELLED without ever
/// touching the terminal (raw mode, the Inline viewport and the key loop are the
/// `--resume` picker's live path, proven by the L4 tmux layer — WP52).
// Go: internal/ui/ui.go:398-412 RunSurface (default result is Cancelled)
#[test]
fn run_surface_empty_spec_is_cancelled_without_a_terminal() {
    let r = crate::ui::run_surface(TabbedSpec::default(), true).expect("empty spec must not fail");
    assert!(r.cancelled);
    assert!(r.panels.is_empty());
}

// --- helpers ------------------------------------------------------------------

/// `Panel` is deliberately not `Clone` (it may own closures); the hint table only needs
/// the shape fields.
fn clone_shape(p: &Panel) -> Panel {
    let body = match &p.body {
        PanelBody::List(l) => PanelBody::List(ListBody {
            items: l.items.clone(),
            custom: l.custom,
            ..ListBody::default()
        }),
        PanelBody::Multi(l) => PanelBody::Multi(ListBody {
            items: l.items.clone(),
            custom: l.custom,
            ..ListBody::default()
        }),
        PanelBody::Picker(pk) => PanelBody::Picker(PickerBody {
            items: pk.items.clone(),
            ..PickerBody::default()
        }),
        PanelBody::View(v) => PanelBody::View(ViewBody {
            lines: v.lines.clone(),
            wrap: v.wrap,
        }),
        other => PanelBody::empty(Panel::of("", PanelBody::empty(kind_of(other))).kind()),
    };
    Panel {
        title: p.title.clone(),
        search: p.search,
        height: p.height,
        body,
        ..Panel::default()
    }
}

/// The kind of a body (the closure-free kinds `clone_shape` copies by kind alone).
fn kind_of(body: &PanelBody) -> PanelKind {
    match body {
        PanelBody::List(_) => PanelKind::List,
        PanelBody::Multi(_) => PanelKind::Multi,
        PanelBody::Slider(_) => PanelKind::Slider,
        PanelBody::Switch { .. } => PanelKind::Switch,
        PanelBody::Input(_) => PanelKind::Input,
        PanelBody::Browser { .. } => PanelKind::Browser,
        PanelBody::Picker(_) => PanelKind::Picker,
        PanelBody::View(_) => PanelKind::View,
    }
}
