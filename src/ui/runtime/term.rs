//! Terminal ownership: the full screen ([`Screen`]) and the out-of-band state around it — the
//! window title, the OSC 9;4 progress bar, focus reporting — each emitted on change.
//!
//! The terminal owns the screen (the alternate screen, `docs/design/fullscreen-tui.md`): a resize
//! is a size handed to the screen and a repaint of every cell, a committed row is appended to the
//! conversation buffer, and nothing ever asks the terminal where its cursor is. The headless seam
//! is [`Term::headless`] (test configuration only): the same screen over a byte buffer, at a size
//! the test names, never touching the process's terminal.

use std::io::{self, Write};

use ratatui::layout::Size;

use super::clipboard::{Clipboard, Copied};
use super::osc;
use super::screen::{Prompt, Screen, ViewOp};
use crate::ui::facade::ProgressState;
use crate::ui::render::frame::FrameView;

/// Produces the writers: one for the screen, one out-of-band control writer. For the live build
/// this is `|| io::stdout()` (handles share the one stream); tests hand out clones of a shared
/// byte buffer.
pub(crate) type MakeWriter<W> = Box<dyn FnMut() -> W + Send>;

/// Owns the terminal and the frame's life on it.
pub(crate) struct Term<W: Write> {
    /// The alternate screen (boxed: it holds two frame buffers).
    screen: Box<Screen<W>>,
    /// Out-of-band control writer (same underlying stream): focus reporting on and off.
    ctrl: W,
    /// Last emitted window title (emit-on-change).
    last_title: Option<String>,
    /// Last emitted OSC 9;4 state (emit-on-change; `None` = never emitted).
    last_progress: Option<ProgressState>,
    /// Whether focus reporting (mode 1004) was turned on — [`Drop`] turns it back off.
    focus_on: bool,
    /// Where a selection's text is copied to (the process's environment; a test's records).
    clipboard: Clipboard,
}

impl<W: Write> Term<W> {
    /// The full screen on the tty's size. Nothing is written yet — the first frame enters the
    /// alternate screen.
    /// `mouse`: whether the alternate screen asks for mouse reporting (the config's `mouse:`).
    pub(crate) fn new(make_writer: MakeWriter<W>, mouse: bool) -> io::Result<Self> {
        let (width, rows) = crossterm::terminal::size()?;
        Self::open(
            make_writer,
            Size::new(width, rows),
            true,
            mouse,
            Clipboard::from_env(),
        )
    }

    /// [`Term::new`] over a test's writers at `size`: the screen never claims the process's
    /// terminal (no `restore_terminal` bookkeeping, no job-control stop).
    #[cfg(test)]
    pub(crate) fn headless(
        make_writer: MakeWriter<W>,
        size: Size,
        mouse: bool,
    ) -> io::Result<Self> {
        Self::open(make_writer, size, false, mouse, Clipboard::recording())
    }

    fn open(
        mut make_writer: MakeWriter<W>,
        size: Size,
        live: bool,
        mouse: bool,
        clipboard: Clipboard,
    ) -> io::Result<Self> {
        let ctrl = make_writer();
        let screen = Box::new(Screen::new(make_writer(), size, live, mouse)?);
        Ok(Self {
            screen,
            ctrl,
            last_title: None,
            last_progress: None,
            focus_on: false,
            clipboard,
        })
    }

    /// Takes the terminal's new size: the next draw repaints every cell.
    pub(crate) fn resize(&mut self, size: Size) {
        self.screen.resize(size);
    }

    /// Opens a batch: what the terminal writes until [`Term::end_batch`] is one write — a
    /// synchronized update if it is larger than `SYNC_MIN` (the loop gathers one iteration's
    /// writes).
    pub(crate) fn begin_batch(&mut self) {
        self.screen.begin_batch();
    }

    /// Closes the batch [`Term::begin_batch`] opened and writes it.
    pub(crate) fn end_batch(&mut self) -> io::Result<()> {
        self.screen.end_batch()
    }

    /// Appends committed blocks to the conversation buffer; the next draw lays out and shows them.
    pub(crate) fn push(&mut self, blocks: Vec<crate::ui::block::Block>) {
        self.screen.push(blocks);
    }

    /// Clears the screen and forgets what it showed — the T-32 `tea.ClearScreen` twin for
    /// surface Tab switches: the next draw writes every cell.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.screen.clear()
    }

    /// The size the frame is laid out against (what the last resize took).
    pub(crate) fn size(&self) -> Size {
        self.screen.size()
    }

    /// Job control: hands the terminal back — the main screen, cooked mode, no paste or focus
    /// reporting — stops the process, and on `fg` takes it again ([`Screen::suspend`]). Returns
    /// the size the terminal has then (`None` for a headless screen, which does not stop).
    pub(crate) fn suspend(&mut self) -> io::Result<Option<Size>> {
        self.screen.suspend(self.focus_on)
    }

    /// Whether the view is scrolled back from the tail.
    pub(crate) fn view_paused(&self) -> bool {
        self.screen.paused()
    }

    /// The viewport's height at the last draw — where the frame starts, for the mouse.
    pub(crate) fn view_height(&self) -> u16 {
        self.screen.view_height()
    }

    /// Starts copying what the last release selected, or — nothing new selected — collects the
    /// outcome of the copy under way (the clipboard's programs run on a thread). An OSC 52 sequence
    /// is written here, in order with the frame. `None`: nothing to say yet.
    pub(crate) fn copy_selection(&mut self) -> io::Result<Option<Copied>> {
        let done = match self.screen.take_copied() {
            Some(text) => self.clipboard.start(text),
            None => self.clipboard.poll(),
        };
        let Some((copied, seq)) = done else {
            return Ok(None);
        };
        if let Some(seq) = seq {
            self.write_raw(seq.as_bytes())?;
        }
        Ok(Some(copied))
    }

    /// Whether a copy's programs are still running (the loop then polls soon).
    pub(crate) fn copy_pending(&self) -> bool {
        self.clipboard.pending()
    }

    /// The key that hands a drag to the terminal's own selection while iota has the mouse.
    pub(crate) fn native_modifier(&self) -> &'static str {
        self.clipboard.native
    }

    /// What a headless terminal's clipboard is handed, readable after the terminal has moved.
    #[cfg(test)]
    pub(crate) fn copied(&self) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        self.clipboard.recorded.clone().unwrap_or_default()
    }

    /// Applies one of the view keys (scrolling, the write-out and the way back, the search).
    pub(crate) fn view(&mut self, op: ViewOp) -> io::Result<()> {
        self.screen.view(op)
    }

    /// One frame: the conversation viewport over the frame's rows, the real cursor where the
    /// frame (or the search line) puts it.
    pub(crate) fn draw_frame(
        &mut self,
        view: &FrameView,
        prompt: Option<&Prompt>,
    ) -> io::Result<()> {
        self.screen.draw_frame(view, prompt)
    }

    /// Emits the window title OSC — on change only (the facade already sanitized it).
    /// The never-set + empty combination stays silent.
    pub(crate) fn set_title(&mut self, title: &str) -> io::Result<()> {
        if self.last_title.as_deref() == Some(title)
            || (title.is_empty() && self.last_title.is_none())
        {
            return Ok(());
        }
        let mut seq = Vec::new();
        osc::emit_title(&mut seq, title)?;
        self.write_raw(&seq)?;
        self.last_title = Some(title.to_owned());
        Ok(())
    }

    /// Emits the OSC 9;4 progress sequence — on change only, exactly like the title, so
    /// the dirty branch can call it unconditionally. A `Term` that never showed a bar
    /// stays silent for `None` (nothing to clear).
    pub(crate) fn set_progress(&mut self, s: ProgressState) -> io::Result<()> {
        if self.last_progress == Some(s)
            || (s == ProgressState::None && self.last_progress.is_none())
        {
            return Ok(());
        }
        self.write_raw(osc::progress_seq(s).as_bytes())?;
        self.last_progress = Some(s);
        Ok(())
    }

    /// Writes one attention ping (OSC 9 + a ringing BEL) out of band. The caller decides
    /// WHETHER to ping (the loop gates on focus) and has already sanitized and defused the
    /// text; both sequences are cursor-neutral, so this is safe between draws.
    pub(crate) fn notify(&mut self, text: &str) -> io::Result<()> {
        self.write_raw(osc::notify_seq(text).as_bytes())
    }

    /// Writes an already-encoded, cursor-neutral sequence verbatim (a program-status report) —
    /// in order with the batch around it, like [`Self::notify`].
    pub(crate) fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.screen.write_raw(bytes)
    }

    /// Turns terminal focus reporting on (mode 1004) so the loop learns when the window
    /// blurs — the ONE gate on attention pings. Called by `run_loop` before its first
    /// iteration, NOT by [`Term::new`]: the one-shot picker and the test harnesses build a
    /// `Term` too and must not leave the mode set (T3 design D17).
    pub(crate) fn enable_focus_reporting(&mut self) -> io::Result<()> {
        self.ctrl.write_all(osc::CSI_FOCUS_ON.as_bytes())?;
        self.ctrl.flush()?;
        self.focus_on = true;
        Ok(())
    }

    /// The exit: leaves the alternate screen and writes the conversation into the main screen
    /// ([`Screen::leave`]).
    pub(crate) fn leave(&mut self) -> io::Result<()> {
        self.screen.leave()
    }
}

/// The renderer's exit clean-up (bubbletea `cursed_renderer.go:178-180,212-214`): focus
/// reporting goes back off and a bar that is still showing is reset. `Term` is passed by
/// value into `run_loop` and therefore dropped on BOTH its clean and its error path, which
/// is what makes this the guarantee — `TermGuard` (raw mode, bracketed paste) unwinds later,
/// on the handle's `close()`. Errors are unreportable here and dropped: the process is
/// leaving the terminal either way.
impl<W: Write> Drop for Term<W> {
    fn drop(&mut self) {
        if self.focus_on {
            let _ = self.ctrl.write_all(osc::CSI_FOCUS_OFF.as_bytes());
        }
        if !matches!(self.last_progress, None | Some(ProgressState::None)) {
            let _ = self.ctrl.write_all(osc::OSC_PROGRESS_RESET.as_bytes());
        }
        let _ = self.ctrl.flush();
    }
}
