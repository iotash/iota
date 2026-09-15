#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP54 Slider-panel suite (internal/ui/`model_test.go` ports).
//!
//! The T2 panel kind's two laws: the integer STEP-INDEX arithmetic with its
//! `None` = "default" state below `Min` (no float accumulation, `g`/`G` shortcuts,
//! `Max` clamp — tabbed.go:409-428), and the RENDER geometry that makes the state flip
//! readable — a focused-chip title, a `%7s` right-aligned value label so the bar's
//! origin cannot shift between "default" and a number, and blank rows padding the lone
//! bar row (tabbed.go:548-566).
//!
//! T-27 (divergence): charm's gradient + partial-block bar is NOT ported — the bar is a
//! plain `█`/`░` fill. The Go test's `IndexAny(l, "█▌░")` probe is kept verbatim, so the
//! assertions pin exactly what the divergence row promises (origin stability, padding,
//! glyph presence) and nothing about the gradient.
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/slider.rs` of the terminal crate; merged 2026-09-02).

use crate::text::ansi::strip_sgr;
use crate::ui::facade::Panel;
use crossterm::event::KeyCode;

use crate::ui::testutil::{Surf, ch, closed, key};

use crate::ui::theme::{RESET, REV_ON};

fn slider(min: f64, max: f64, step: f64) -> Panel {
    Panel::slider("T".to_owned(), min, max, step, None)
}

/// Go `barCol`: the column the bar starts at, on the first plain row that carries one of
/// the bar glyphs. `-1` when nothing is drawn.
fn bar_col(plain: &str) -> Option<usize> {
    plain
        .lines()
        .find_map(|l| l.char_indices().find(|(_, c)| "█▌░".contains(*c)))
        .map(|(i, _)| i)
}

// --- the step machine -------------------------------------------------------

// Below Min falls back
// to default; G resets to Max.
#[test]
fn a_slider_falls_back_to_default_below_min_and_g_resets_to_max() {
    let mut s = Surf::open(vec![slider(0.0, 1.0, 0.5)]);
    assert_eq!(s.ps(0).value, None, "a slider starts on its default");

    s.tap(key(KeyCode::Right)); // default → Min
    assert_eq!(s.ps(0).value, Some(0.0));
    s.tap(key(KeyCode::Left)); // Min − one step is below Min → back to default
    assert_eq!(s.ps(0).value, None);
    s.tap(ch('G')); // G jumps to Max

    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled);
    assert_eq!(
        r.panels[0].value,
        Some(1.0),
        "G must commit the Max value, not the default"
    );
}

/// The step arithmetic is INDEX-based, so repeated stepping cannot accumulate float
/// error, and `Max` clamps rather than overshooting (tabbed.go:409-428).
#[test]
fn slider_steps_by_index_and_clamps_at_max() {
    let mut s = Surf::open(vec![slider(0.0, 2.0, 0.1)]);
    s.tap(key(KeyCode::Right)); // default → 0.0
    for _ in 0..7 {
        s.tap(key(KeyCode::Right));
    }
    // 0.1 accumulated seven times in floats is 0.7000000000000001; the index form is 0.7.
    assert_eq!(s.ps(0).value, Some(0.7));

    for _ in 0..100 {
        s.tap(key(KeyCode::Right));
    }
    assert_eq!(s.ps(0).value, Some(2.0), "stepping past Max must clamp");

    // 'g' is the way back to "omit the parameter"; 'h'/'l' mirror ←/→.
    s.tap(ch('g'));
    assert_eq!(s.ps(0).value, None);
    s.tap(ch('l'));
    assert_eq!(s.ps(0).value, Some(0.0));
    s.tap(ch('h'));
    assert_eq!(s.ps(0).value, None);
}

// --- the rendered geometry --------------------------------------------------

// A single-panel
// surface titles itself with the focused CHIP (not faint dashes), the bar's origin is
// identical in the default and value states, and the bar row is padded by blank rows.
//
// T-27: the bar is a plain filled bar here; the Go probe (`█▌░`) and every assertion it
// carries port verbatim, because none of them describes the gradient.
#[test]
fn the_slider_bar_keeps_its_origin_and_the_chip_titles_the_surface() {
    let mut s = Surf::open(vec![Panel::slider(
        "Temperature".to_owned(),
        0.0,
        2.0,
        0.1,
        None,
    )]);

    let c = s.content();
    assert!(
        !c.contains("── Temperature"),
        "single-panel title still uses faint dashes:\n{c:?}"
    );
    assert!(
        c.contains(&format!("{REV_ON} Temperature {RESET}")),
        "single-panel title missing the focused-chip style:\n{c:?}"
    );

    let col0 = bar_col(&s.plain()).expect("slider progress bar not rendered");
    assert!(
        s.plain().contains("default"),
        "default state label missing:\n{:?}",
        s.plain()
    );

    s.tap(key(KeyCode::Right)); // default → Min
    s.tap(key(KeyCode::Right)); // step up
    let c = s.content();
    assert!(
        c.contains('█'),
        "bar has no filled cells after stepping up:\n{c:?}"
    );
    assert_eq!(
        bar_col(&strip_sgr(&c)),
        Some(col0),
        "bar origin shifted between the default and value states"
    );

    // The lone bar row is padded above and below, so it reads as a control rather than
    // as another line of text.
    let plain = strip_sgr(&c);
    let rows: Vec<&str> = plain.lines().collect();
    let bar_row = rows
        .iter()
        .rposition(|l| l.contains('█') || l.contains('░'))
        .expect("no bar row");
    assert!(bar_row >= 1, "bar row has nothing above it:\n{plain:?}");
    assert!(
        rows[bar_row - 1].trim().is_empty() && rows[bar_row + 1].trim().is_empty(),
        "bar row not padded by blank rows:\n{plain:?}"
    );

    // ESC closes the surface — the Go test drains its reply channel here.
    assert!(closed(s.press(key(KeyCode::Esc))).cancelled);
}

/// The value label is right-aligned to `"default"`'s width, which is WHY the origin is
/// stable: every state renders the same seven columns before the two-space gutter.
#[test]
fn slider_value_label_is_right_aligned_to_the_default_width() {
    let mut s = Surf::open(vec![slider(0.0, 2.0, 0.1)]);
    let plain_default = s.plain();
    s.tap(key(KeyCode::Right));
    let plain_value = s.plain();

    let row = |p: &str| -> String {
        p.lines()
            .find(|l| l.contains('█') || l.contains('░'))
            .map(str::to_owned)
            .expect("bar row")
    };
    assert!(row(&plain_default).starts_with("  default  "));
    assert!(row(&plain_value).starts_with("      0.0  "));
}

/// The per-kind hint row is byte-exact (tabbed.go:916-947; `·` = U+00B7).
#[test]
fn slider_hint_row_is_byte_exact() {
    let mut s = Surf::open(vec![slider(0.0, 2.0, 0.1)]);
    let plain = s.plain();
    assert!(
        plain.contains("←→ adjust · g default · G max · Enter confirm · q/Esc cancel"),
        "hint row wrong:\n{plain:?}"
    );
}
