//! `run_surface` — the one-shot pre-REPL surface (ui.go:398-412 RunSurface): a
//! short-lived raw-mode + Inline terminal sized to the surface, rendering ONLY the
//! surface (no composer chrome), quitting when the surface closes, and fully released
//! (raw off, cursor shown, paste off, flushed) BEFORE return — the `--resume` session
//! picker's terminal must be handed back before the REPL's own `Tui::start`.

use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use crate::ui::facade::{TabbedResult, TabbedSpec};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::{cursor, execute, terminal};

use super::frame::FrameView;
use super::surface::{SurfaceEffect, SurfaceState};
use super::term::Term;

/// The W10 idle-wake shape: the poll deadline is always finite.
const ONESHOT_POLL_MAX: Duration = Duration::from_millis(50);

/// Restores the terminal on every exit path (raw off, paste off, cursor shown).
struct RawGuard;

impl RawGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = execute!(out, DisableBracketedPaste, cursor::Show);
        let _ = terminal::disable_raw_mode();
        let _ = out.flush();
    }
}

/// The one-shot surface loop (ui.go `RunSurface`). Init arms the refresh tick when
/// `refresh_every_ms > 0`; a key that closes the surface ends the program; the result
/// (default cancelled) is returned after the terminal is released.
pub(crate) fn run_surface(spec: TabbedSpec, dark: bool) -> io::Result<TabbedResult> {
    let TabbedSpec {
        panels,
        refresh_every_ms,
        enter_advances,
    } = spec;
    if panels.is_empty() {
        return Ok(TabbedResult {
            cancelled: true,
            ..TabbedResult::default()
        });
    }
    let mut st = SurfaceState::new(enter_advances, panels);
    st.set_dark(dark);
    let _guard = RawGuard::new()?;
    let (_, start_row) = cursor::position()?;
    let (width, height) = terminal::size()?;
    st.set_term_height(height);
    let rows = st.render(width).rows;
    let h = u16::try_from(rows.len()).unwrap_or(u16::MAX).max(1);
    let mut term: Term<io::Stdout> = Term::new(Box::new(io::stdout), h, start_row, None)?;

    let refresh = Duration::from_millis(refresh_every_ms);
    let mut last_refresh = Instant::now();
    let mut dirty = true;
    loop {
        let mut deadline = if dirty {
            Duration::ZERO
        } else {
            ONESHOT_POLL_MAX
        };
        if refresh > Duration::ZERO {
            let next = refresh.saturating_sub(last_refresh.elapsed());
            deadline = deadline.min(next.max(Duration::from_millis(1)));
        }
        if event::poll(deadline)? {
            match event::read()? {
                Event::Key(k) => {
                    match st.key(k) {
                        SurfaceEffect::Close(result) => {
                            term.park_cursor()?;
                            return Ok(result); // the guard releases the terminal
                        }
                        SurfaceEffect::ForceRedraw => term.clear()?, // T-32
                        SurfaceEffect::None => {}
                    }
                    dirty = true;
                }
                Event::Paste(data) => {
                    st.paste(&data);
                    dirty = true;
                }
                Event::Resize(..) => {
                    term.autoresize()?;
                    term.clear()?;
                    term.mark_top_dirty();
                    dirty = true;
                }
                _ => {}
            }
        }
        if refresh > Duration::ZERO && last_refresh.elapsed() >= refresh {
            st.tick();
            last_refresh = Instant::now();
            dirty = true;
        }
        if dirty {
            let size = term.size()?;
            st.set_term_height(size.height);
            let rendered = st.render(size.width);
            let view_height = u16::try_from(rendered.rows.len())
                .unwrap_or(u16::MAX)
                .clamp(1, size.height.max(1));
            term.ensure_height(view_height)?;
            // One-shot mode renders only the surface, so the cursor target is the
            // surface-block coordinate itself (no composer offset).
            let view = FrameView {
                rows: rendered.rows,
                cursor: rendered.cursor,
            };
            term.draw_frame(&view)?;
            dirty = false;
        }
    }
}
