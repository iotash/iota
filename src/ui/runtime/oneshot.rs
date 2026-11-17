//! `run_surface` — the one-shot pre-REPL surface (ui.go:398-412 RunSurface): the same full
//! screen the REPL uses ([`Term`]), rendering ONLY the surface (no composer chrome) pinned to its
//! bottom, quitting when the surface closes, and fully released (the alternate screen left, raw
//! off, cursor shown, paste off) BEFORE return — the `iota resume` session picker's terminal must
//! be handed back before the REPL's own `Tui::start`. Its conversation is empty, so leaving
//! writes nothing into the shell's screen.

use std::io;
use std::time::{Duration, Instant};

use crate::ui::facade::{TabbedResult, TabbedSpec};
use crossterm::event::{self, EnableBracketedPaste, Event, MouseButton, MouseEventKind};
use crossterm::{execute, terminal};
use ratatui::layout::Size;

use super::handle::{TermGuard, install_panic_hook};
use super::term::Term;
use crate::ui::render::frame::{FrameMap, FrameView};
use crate::ui::surface::{SurfaceEffect, SurfaceState};

/// The W10 idle-wake shape: the poll deadline is always finite.
const ONESHOT_POLL_MAX: Duration = Duration::from_millis(50);

/// The one-shot surface loop (ui.go `RunSurface`). Init arms the refresh tick when
/// `refresh_every_ms > 0`; a key that closes the surface ends the program; the result
/// (default cancelled) is returned after the terminal is released.
pub(crate) fn run_surface(spec: TabbedSpec, dark: bool, mouse: bool) -> io::Result<TabbedResult> {
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
    terminal::enable_raw_mode()?;
    // Every exit path from here leaves the alternate screen and restores the tty (`restore_terminal`).
    let _guard = TermGuard::new();
    install_panic_hook();
    execute!(io::stdout(), EnableBracketedPaste)?;
    let mut term: Term<io::Stdout> = Term::new(Box::new(io::stdout), mouse)?;

    let refresh = Duration::from_millis(refresh_every_ms);
    let mut last_refresh = Instant::now();
    let mut dirty = true;
    // The surface's first screen row at the last draw (it is pinned to the bottom), for a click.
    let mut top: u16 = 0;
    // A resize is taken at the next draw, which repaints every cell.
    let mut resized: Option<Size> = None;
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
                            term.leave()?;
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
                // The mouse as in the chat's surfaces (`input/mouse.rs`): a click on a row, the wheel.
                Event::Mouse(ev) => {
                    let effect = match ev.kind {
                        MouseEventKind::Down(MouseButton::Left) if ev.row >= top => {
                            st.click(usize::from(ev.row - top), usize::from(ev.column))
                        }
                        MouseEventKind::ScrollUp => {
                            st.wheel(-1);
                            SurfaceEffect::None
                        }
                        MouseEventKind::ScrollDown => {
                            st.wheel(1);
                            SurfaceEffect::None
                        }
                        _ => SurfaceEffect::None,
                    };
                    match effect {
                        SurfaceEffect::Close(result) => {
                            term.leave()?;
                            return Ok(result);
                        }
                        SurfaceEffect::ForceRedraw => term.clear()?,
                        SurfaceEffect::None => {}
                    }
                    dirty = true;
                }
                Event::Resize(width, height) if width > 0 && height > 0 => {
                    resized = Some(Size { width, height });
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
            if let Some(size) = resized.take() {
                term.resize(size);
            }
            let size = term.size();
            // Nothing but the surface is on the screen: all of it is the surface's.
            st.set_room(size.height, size.height);
            let rendered = st.render(size.width);
            // One-shot mode renders only the surface, so the cursor target is the
            // surface-block coordinate itself (no composer offset).
            let view = FrameView {
                float: 0,
                rows: rendered.rows,
                cursor: rendered.cursor,
                map: FrameMap {
                    surface: Some(0),
                    ..FrameMap::default()
                },
            };
            term.draw_frame(&view, None)?;
            top = size
                .height
                .saturating_sub(u16::try_from(view.rows.len()).unwrap_or(u16::MAX));
            dirty = false;
        }
    }
}
