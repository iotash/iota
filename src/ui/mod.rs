//! The inline terminal engine (internal/ui) — the ONLY module allowed to name ratatui/crossterm
//! (`TUI_CONTRACTS` §1.7; ci.sh grep gate over `src/ui/**`). Event-loop thread owning
//! `Terminal<CrosstermBackend<Stdout>>` + `Viewport::Inline`; implements the [`facade::Ui`] trait,
//! which is the whole surface `crate::ui::repl` talks to. The spike warts W1–W10 are carried as named
//! constraints by the modules below (`TUI_DESIGN` §4).

pub mod facade;

pub use debug::install_region_trace;

pub(crate) mod input;
pub(crate) mod render;
pub(crate) mod runtime;
pub(crate) mod surface;
#[cfg(test)]
pub(crate) mod testutil;

// Transitional aliases while the import points move (next commit): every `crate::ui::<file>`
// path of the flat layout still resolves.
#[allow(unused_imports)]
pub(crate) use input::{composer, keys, paste, suggest};
#[allow(unused_imports)]
pub(crate) use render::{debug, frame, region, sink, spans, theme};
#[allow(unused_imports)]
pub(crate) use runtime::{event_loop, handle, msgs, oneshot, osc, term};

use std::sync::Arc;

use crate::ui::facade::{TabbedResult, TabbedSpec, Ui};

/// Construction options for [`Tui::start`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct TuiOptions {
    /// Whether the terminal background is dark (from [`detect_background`]).
    pub(crate) dark: bool,
}

/// The running interactive terminal UI (`TUI_CONTRACTS` §5). Owns the `"iota-tui"` loop
/// thread; [`Tui::handle`] is the facade every consumer talks through.
pub(crate) struct Tui {
    handle: Arc<handle::TuiHandle>,
}

impl Tui {
    /// Enables raw mode + bracketed paste, queries the initial size, spawns the
    /// `"iota-tui"` loop thread owning `Terminal<CrosstermBackend<Stdout>>` with
    /// `Viewport::Inline`.
    pub(crate) fn start(opts: TuiOptions) -> std::io::Result<Tui> {
        handle::start(opts)
    }

    /// The facade handle (an `Arc` clone per consumer).
    pub(crate) fn handle(&self) -> Arc<dyn Ui> {
        Arc::clone(&self.handle) as Arc<dyn Ui>
    }
}

/// One-shot pre-REPL surface (ui.go RunSurface): short-lived raw mode + Inline terminal
/// sized to the surface, fully released before return. Caller: the `iota resume` picker.
pub(crate) fn run_surface(spec: TabbedSpec, dark: bool) -> std::io::Result<TabbedResult> {
    oneshot::run_surface(spec, dark)
}

/// ONE OSC 11 round-trip on the tty (MUST run before any event loop); 100ms timeout;
/// default true (dark).
pub(crate) fn detect_background() -> bool {
    osc::detect_background()
}
