#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP54 Switch-panel suite (internal/ui/`model_test.go:1679` port).
//!
//! The T2 panel kind's one law is GEOMETRIC: Off and On must occupy the same columns, so
//! toggling slides the knob instead of reflowing the row (a right-aligned 3-column label
//! plus a fixed 6-cell track — tabbed.go:567-580). Space toggles; ←/→ SET rather than
//! toggle (`h`/`l` mirror them); Enter commits `On`.
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/switch.rs` of the terminal crate; merged 2026-09-02).

use crate::text::ansi::strip_sgr;
use crate::text::width::str_width;
use crate::ui::facade::{Panel, TabbedResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::surface::tabbed::PanelState;
use crate::ui::surface::{SurfaceEffect, SurfaceState};

// --- harness ----------------------------------------------------------------

struct Surf {
    st: SurfaceState,
}

impl Surf {
    fn open(panels: Vec<Panel>) -> Self {
        let st = SurfaceState::new(false, panels);
        Self { st }
    }

    fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    fn tap(&mut self, k: KeyEvent) {
        assert!(
            !matches!(self.press(k), SurfaceEffect::Close(_)),
            "key closed the surface unexpectedly"
        );
    }

    fn ps(&self) -> &PanelState {
        &self.st.slots[0].state
    }

    fn content(&mut self) -> String {
        self.st.render(80).rows.join("\n")
    }

    /// Go `toggleRow`: the rendered row carrying the knob, stripped and right-trimmed.
    fn toggle_row(&mut self) -> String {
        let c = strip_sgr(&self.content());
        c.lines()
            .find(|l| l.contains('●'))
            .map(|l| l.trim_end().to_owned())
            .expect("no switch row rendered")
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

fn switch_panel() -> Panel {
    Panel::switch("Image".to_owned(), false)
}

// --- the suite --------------------------------------------------------------

// Go: internal/ui/model_test.go:1679 TestTabbedSwitch — Space toggles, ←/→ set, the row
// geometry survives the flip, and Enter commits `On`.
#[test]
fn test_tabbed_switch() {
    let mut s = Surf::open(vec![switch_panel()]);

    let off_row = s.toggle_row();
    assert!(
        off_row.contains("Off"),
        "initial row = {off_row:?}, want the Off state"
    );

    s.tap(ch(' '));
    assert!(s.ps().on, "Space must toggle the switch on");
    let on_row = s.toggle_row();
    assert!(
        on_row.contains("On"),
        "on row = {on_row:?}, want the On state"
    );
    assert_eq!(
        str_width(&off_row),
        str_width(&on_row),
        "the state flip changed the row geometry:\n{off_row:?}\n{on_row:?}"
    );

    s.tap(key(KeyCode::Left));
    assert!(!s.ps().on, "← must set the switch off");
    s.tap(key(KeyCode::Right));
    assert!(s.ps().on, "→ must set the switch on");

    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(
        !r.cancelled && r.panels[0].on,
        "commit = {r:?}, want On=true"
    );
}

/// `Panel::on` seeds the state, so a switch opens showing what the provider already has
/// — an untouched tab commits the same value back (tabbed.go:190-254).
#[test]
fn switch_opens_on_the_current_value() {
    let mut s = Surf::open(vec![switch_panel().with_on(true)]);
    assert!(s.ps().on);
    assert!(s.toggle_row().contains("On"));
    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(r.panels[0].on, "an untouched Switch tab must be a no-op");
}

/// `h`/`l` mirror ←/→ (the readline heritage row of the ladder), and both SET rather
/// than toggle — pressing `l` twice leaves the switch on.
#[test]
fn switch_hl_keys_set_rather_than_toggle() {
    let mut s = Surf::open(vec![switch_panel()]);
    s.tap(ch('l'));
    s.tap(ch('l'));
    assert!(s.ps().on, "l must SET on, not toggle");
    s.tap(ch('h'));
    s.tap(ch('h'));
    assert!(!s.ps().on, "h must SET off, not toggle");
}

/// The per-kind hint row is byte-exact (tabbed.go:916-947; `·` = U+00B7), and the
/// prompt line above it renders the caller's one-line explanation of the knob.
#[test]
fn switch_hint_and_prompt_rows_are_byte_exact() {
    let mut s = Surf::open(vec![switch_panel().with_prompt(
        "Request image generation (modalities / built-in tool)".to_owned(),
    )]);
    let plain = strip_sgr(&s.content());
    assert!(
        plain.contains("Space toggle · ←→ off/on · Enter confirm · q/Esc cancel"),
        "hint row wrong:\n{plain:?}"
    );
    assert!(
        plain.contains(" Request image generation (modalities / built-in tool)"),
        "prompt row missing:\n{plain:?}"
    );
}
