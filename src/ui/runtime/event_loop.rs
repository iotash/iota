//! The `"iota-tui"` loop thread — warts W4 `DRAW_WITH_INSERTS`, W5 `RESIZE_PASS_FIRST`,
//! W10 `IDLE_WAKE`; mailbox drain, post-update jobs, timers, the cancel-scope stack,
//! the parked waiter, the spinner chain (`TUI_DESIGN` §4).
//!
//! - **W10** `IDLE_WAKE`: the poll deadline is ALWAYS finite —
//!   `min(IDLE_POLL_MAX 50ms, active timers)`. The mailbox is a plain `std::sync::mpsc`
//!   with no wake fd (Go's `tea.Program` woke on `Send`); an infinite idle
//!   `event::poll` would starve fire-and-forget messages (`⚠ MCP %s failed`, an async
//!   `set_title`, `set_status`, a `ReadReq` posted between polls) and DEADLOCK
//!   `close()` at idle (`UiMsg::Quit` undrained while the loop sleeps). Worst-case
//!   idle latency: one deadline (50 ms, invisible); ~0 CPU.
//! - **W4** `DRAW_WITH_INSERTS`: inserts move the physical cursor; the real cursor
//!   returns only at the next draw — so every iteration containing an insert batch
//!   ends with `draw` + `set_cursor_position`, and inserts and draws stay on this one
//!   thread. (This replaces Go's cursor-bump/`viewEquals` defeat — T-03; the idle
//!   frame still carries no spinner glyph.)
//! - **W5** `RESIZE_PASS_FIRST`: on `Event::Resize` — store the atomics →
//!   `autoresize()` → `clear()` → draw → DSR resync of the tracked top; only THEN does
//!   a WIDTH change schedule `region.flush_tail()` as a post-update job (never
//!   re-entrant — it takes the region lock and sends into this mailbox). Height-only
//!   changes flush nothing; reflow orphans are accepted; storm mode stays dead.
//!
//! All model state lives on this thread (zero locks inside the engine); the facade
//! reaches in only via [`UiMsg`]. The composer/keys/paste/suggest/surface modules are
//! the WP46/WP47 seams — this file's wiring is final, their bodies are provisional.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::sync::lock;
use std::time::{Duration, Instant};

use crate::ui::facade::{
    Input, ProgressState, StatusData, Suggestion, TabbedResult, TabbedSpec, UiError,
};
use crossterm::event::{Event, KeyEvent};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::msgs::UiMsg;
use super::osc;
use super::term::Term;
use crate::ui::input::composer::Composer;
use crate::ui::input::keys;
use crate::ui::input::paste;
use crate::ui::input::suggest;
use crate::ui::render::frame::{BottomZone, BusyView, FrameInput, FrameView, build_frame};
use crate::ui::render::region::{Region, RegionSnapshot};
use crate::ui::surface::{self, SurfaceEffect};

/// W10 `IDLE_WAKE`: the loop's poll deadline is never longer than this
/// (`TUI_CONTRACTS` §8 — a design constant; tea.Program woke on `Send`).
pub(crate) const IDLE_POLL_MAX: Duration = Duration::from_millis(50);

/// Spinner cadence (model.go:25 — 120ms tick).
pub(crate) const SPINNER_TICK: Duration = Duration::from_millis(120);

/// Poll cap while a preview is streaming (the spike's hot-loop cadence).
pub(crate) const STREAM_POLL_CAP: Duration = Duration::from_millis(25);

/// Composer growth cap (model.go:29; `TUI_CONTRACTS` §8).
pub(crate) const MAX_COMPOSER_ROWS: usize = 5;

/// State shared between the facade handle (WP45) and this loop thread.
pub(crate) struct LoopShared {
    /// Terminal width in columns (starts 80; resize stores).
    pub(crate) width: Arc<AtomicU16>,
    /// Terminal height in rows (starts 24 — "24 until the first resize event").
    pub(crate) height: Arc<AtomicU16>,
    /// The staging window; the loop takes this lock only inside post-update jobs
    /// (the W5 flush), never inside a message handler.
    pub(crate) region: Arc<Mutex<Region>>,
}

/// The loop's input-event seam: crossterm live, scripted in tests.
pub(crate) trait EventSource {
    /// Waits up to `timeout` for an event (the W10 deadline is always finite).
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    /// Reads the next event; called only after `poll` returned `true`.
    fn read(&mut self) -> io::Result<Event>;
}

/// The live [`EventSource`] over crossterm's global event stream.
pub(crate) struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        crossterm::event::read()
    }
}

/// A parked `read_input` caller (model.go `waiter`): replaced wholesale by a newer
/// request; revoked eagerly by [`UiMsg::ReadCancel`] with the same id.
pub(crate) struct Waiter {
    id: u64,
    reply: oneshot::Sender<Result<Input, UiError>>,
}

/// An open tabbed surface parked below the composer.
pub(crate) struct SurfaceOpen {
    /// The transition-fn state (WP47's seam).
    pub(crate) st: surface::SurfaceState,
    /// Refresh cadence; zero = never (tabbed.go `RefreshEvery`).
    refresh_every: Duration,
    /// Generation stamp — the stale-tick guard (tabbed.go surfTickMsg.gen): a
    /// refresh may only run when this matches the model's live counter.
    generation: u64,
    last_refresh: Instant,
    reply: Option<oneshot::Sender<TabbedResult>>,
}

/// Deferred side effects that must run OUTSIDE the message/event handlers
/// (Go `tea.Cmd`): they take the region lock and send into this same mailbox.
enum Job {
    /// `region.flush_tail()` — the W5 width-change (and pre-open) mitigation.
    FlushTail,
    /// `region.retrim()` — the T-40 frame floor re-applied after a HEIGHT change.
    Retrim,
}

/// The loop model (Go `model`): every field mutates on this thread only.
pub(crate) struct Model {
    /// Terminal width (mirrors the shared atomic).
    pub(crate) width: u16,
    /// Terminal height (mirrors the shared atomic; 0 = unknown → 24 fallback).
    pub(crate) height: u16,
    /// The handle-shared state.
    pub(crate) shared: LoopShared,
    /// Status-row data.
    pub(crate) status: StatusData,
    /// Window title (sanitized by the facade; emitted on change).
    pub(crate) title: String,
    /// Type-ahead queue, oldest first — what the user typed AND what the host injected.
    pub(crate) queue: Vec<Queued>,
    /// The parked `read_input` caller, if any.
    pub(crate) waiter: Option<Waiter>,
    /// The live busy phase, if any.
    pub(crate) busy: Option<BusyView>,
    /// The staging-window snapshot the frame renders.
    pub(crate) region_snap: RegionSnapshot,
    /// The open surface, if any.
    pub(crate) surface: Option<SurfaceOpen>,
    /// Live surface generation (the stale-tick guard's counter).
    pub(crate) surface_gen: u64,
    /// The cancel-scope stack (bottom index 0 = the turn).
    pub(crate) cancels: Vec<CancellationToken>,
    /// The slash-command table (completion + suggestion row — WP46).
    pub(crate) commands: Vec<Suggestion>,
    /// The composer (WP46's seam; provisional body).
    pub(crate) composer: Composer,
    /// Stored multi-line pastes (`[#N …]` tags — WP46).
    pub(crate) pastes: Vec<String>,
    /// The shared spinner counter (one counter drives header AND busy glyph).
    pub(crate) spin: usize,
    /// Whether the self-stopping spinner chain is running.
    pub(crate) spin_ticking: bool,
    last_spin: Instant,
    jobs: Vec<Job>,
    /// A queued full clear before the next draw (T-32 surface Tab switch).
    force_clear: bool,
    /// The terminal's background tone (`UiMsg::DarkBackground`; dark is the safe default).
    dark: bool,
    /// Whether the frame must be redrawn this iteration (pub(crate): the W10
    /// deadline units clear it to model a fully idle loop).
    pub(crate) dirty: bool,
    /// Set by [`UiMsg::Quit`]; the loop exits after landing pending inserts.
    quit: bool,
    /// The terminal's progress indicator state (model.go:215); the dirty branch emits it
    /// on change through `term.set_progress`.
    pub(crate) progress: ProgressState,
    /// Attention pings not yet written (model.go:217); [`drain_notify`] writes them.
    pub(crate) notify: Vec<String>,
    /// Whether the terminal window has focus (model.go:118). Starts TRUE: a terminal that
    /// never reports focus never pings, which is Go's behaviour — whoever is already
    /// watching needs no bell.
    pub(crate) focused: bool,
}

/// One entry of the type-ahead queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Queued {
    /// What the user typed, paste tags UNEXPANDED (the store is consulted at drain time, so a re-submit
    /// after an edit re-expands from it).
    Typed(String),
    /// A host notice, already a finished [`Input`]: it has no paste tags and its `display` is the one line
    /// that stands for it.
    Notice(Input),
}

impl Queued {
    /// The finished input, expanding paste tags for a typed entry.
    fn into_input(self, pastes: &[String]) -> Input {
        match self {
            Self::Typed(text) => paste::make_input(pastes, &text),
            Self::Notice(input) => input,
        }
    }

    /// Whether a steering drain may take this entry: a queued slash command stops the take
    /// (model.go:290-300), and a notice — which can never be one — always passes.
    fn is_steerable(&self) -> bool {
        match self {
            Self::Typed(text) => !text.starts_with('/'),
            Self::Notice(_) => true,
        }
    }

    /// The row the queue block shows for this entry.
    fn row(&self) -> &str {
        match self {
            Self::Typed(text) => text,
            Self::Notice(input) => &input.display,
        }
    }

    /// The typed text, when this is the user's own (the ↑ pop and the interrupt fold-back).
    fn typed(&self) -> Option<&str> {
        match self {
            Self::Typed(text) => Some(text),
            Self::Notice(_) => None,
        }
    }
}

impl Model {
    /// A fresh model reading its initial geometry from the shared atomics.
    pub(crate) fn new(shared: LoopShared) -> Self {
        let width = shared.width.load(Ordering::Relaxed);
        let height = shared.height.load(Ordering::Relaxed);
        Self {
            width: if width > 0 { width } else { 80 },
            height,
            shared,
            status: StatusData::default(),
            title: String::new(),
            queue: Vec::new(),
            waiter: None,
            busy: None,
            region_snap: RegionSnapshot::default(),
            surface: None,
            surface_gen: 0,
            cancels: Vec::new(),
            commands: Vec::new(),
            composer: Composer::new(),
            pastes: Vec::new(),
            spin: 0,
            spin_ticking: false,
            last_spin: Instant::now(),
            jobs: Vec::new(),
            force_clear: false,
            dark: true,
            dirty: true, // the first iteration paints the initial frame
            quit: false,
            progress: ProgressState::None,
            notify: Vec::new(),
            focused: true,
        }
    }

    /// The W10 deadline: ALWAYS finite, capped by [`IDLE_POLL_MAX`] and shortened by
    /// the active timers (spinner, surface refresh, streaming cadence). A pending
    /// redraw polls at zero — the frame debt is paid before sleeping.
    pub(crate) fn poll_deadline(&self) -> Duration {
        if self.dirty {
            return Duration::ZERO;
        }
        let mut d = IDLE_POLL_MAX;
        if self.spin_ticking {
            let next = SPINNER_TICK.saturating_sub(self.last_spin.elapsed());
            d = d.min(next.max(Duration::from_millis(1)));
        }
        if !self.region_snap.label.is_empty() {
            d = d.min(STREAM_POLL_CAP);
        }
        if let Some(s) = &self.surface
            && s.refresh_every > Duration::ZERO
        {
            let next = s.refresh_every.saturating_sub(s.last_refresh.elapsed());
            d = d.min(next.max(Duration::from_millis(1)));
        }
        d
    }

    /// Starts the spinner tick chain when an animated header appears
    /// (model.go ensureSpin).
    fn ensure_spin(&mut self) {
        if self.spin_ticking {
            return;
        }
        self.spin_ticking = true;
        self.last_spin = Instant::now();
    }

    /// One spinner step when due; the chain STOPS itself once nothing animates
    /// (model.go spinTickMsg: `busy == nil && region.label == ""`).
    pub(crate) fn tick_spin(&mut self) {
        if !self.spin_ticking {
            return;
        }
        if self.busy.is_none() && self.region_snap.label.is_empty() {
            self.spin_ticking = false;
            return;
        }
        if self.last_spin.elapsed() >= SPINNER_TICK {
            self.spin = self.spin.wrapping_add(1);
            self.last_spin = Instant::now();
            self.dirty = true;
        }
    }

    /// One generation-guarded surface refresh when due (tabbed.go surfTickMsg).
    fn tick_surface_refresh(&mut self) {
        let live = self.surface_gen;
        if let Some(s) = self.surface.as_mut()
            && s.refresh_every > Duration::ZERO
            && s.generation == live
            && s.last_refresh.elapsed() >= s.refresh_every
        {
            s.st.tick();
            s.last_refresh = Instant::now();
            self.dirty = true;
        }
    }

    /// Applies one mailbox message ([`UiMsg::Scrollback`]/[`UiMsg::Quit`] are the
    /// loop's own — never routed here).
    pub(crate) fn apply(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Region(snap) => {
                // Every snapshot changes the rendered staging window; a labelled one
                // also keeps the spinner chain alive (model.go:197-205).
                let animate = !snap.label.is_empty();
                self.region_snap = snap;
                if animate {
                    self.ensure_spin();
                }
                self.dirty = true;
            }
            UiMsg::Status(s) => {
                self.status = s;
                self.dirty = true;
            }
            UiMsg::Title(t) => {
                self.title = t;
                self.dirty = true;
            }
            UiMsg::Commands(c) => {
                self.commands = c;
                self.dirty = true;
            }
            UiMsg::Progress(s) => {
                self.progress = s;
                self.dirty = true;
            }
            UiMsg::Notify(text) => {
                self.notify.push(text);
                self.dirty = true;
            }
            UiMsg::DarkBackground(dark) => {
                self.dark = dark;
                self.dirty = true;
            }
            UiMsg::BusyOn(label) => {
                self.busy = Some(BusyView {
                    label,
                    detail: String::new(),
                    since: Instant::now(),
                });
                self.ensure_spin();
                self.dirty = true;
            }
            UiMsg::BusyDetail(detail) => {
                // Live sub-state on the current phase — the clock keeps running; a
                // detail with no busy phase is dropped (model.go:259-264).
                if let Some(b) = self.busy.as_mut() {
                    b.detail = detail;
                    self.dirty = true;
                }
            }
            UiMsg::BusyOff => {
                self.busy = None;
                self.dirty = true;
            }
            UiMsg::ScopePush(token) => {
                self.cancels.push(token);
                self.dirty = true;
            }
            UiMsg::ScopePop => {
                self.cancels.pop();
                self.dirty = true;
            }
            UiMsg::ReadReq { id, reply } => {
                // The queue drains FIFO before a waiter parks (model.go:280-288).
                if self.queue.is_empty() {
                    self.waiter = Some(Waiter { id, reply });
                } else {
                    let head = self.queue.remove(0);
                    let _ = reply.send(Ok(head.into_input(&self.pastes)));
                    self.dirty = true;
                }
            }
            UiMsg::Enqueue(input) => {
                // Exactly `submit`'s law, minus the composer: a parked waiter is served directly,
                // otherwise it queues behind whatever is already typed ahead.
                if let Some(w) = self.waiter.take() {
                    let _ = w.reply.send(Ok(input));
                } else {
                    self.queue.push(Queued::Notice(input));
                }
                self.dirty = true;
            }
            UiMsg::ReadCancel { id } => {
                // Revoke only the SAME waiter (model.go:302-306).
                if self.waiter.as_ref().is_some_and(|w| w.id == id) {
                    self.waiter = None;
                }
            }
            UiMsg::TakeQueued { reply } => {
                // Steering drain: the contiguous non-command prefix; a slash command
                // stops the take (model.go:290-300).
                let mut taken = Vec::new();
                while self.queue.first().is_some_and(Queued::is_steerable) {
                    let head = self.queue.remove(0);
                    taken.push(head.into_input(&self.pastes));
                }
                let _ = reply.send(taken);
                self.dirty = true;
            }
            UiMsg::TabbedOpen { spec, reply } => {
                self.surface_gen += 1;
                let TabbedSpec {
                    panels,
                    refresh_every_ms,
                    enter_advances,
                } = spec;
                let st = surface::SurfaceState::new(enter_advances, panels);
                self.surface = Some(SurfaceOpen {
                    st,
                    refresh_every: Duration::from_millis(refresh_every_ms),
                    generation: self.surface_gen,
                    last_refresh: Instant::now(),
                    reply: Some(reply),
                });
                self.dirty = true;
            }
            UiMsg::SurfaceCancel => self.close_surface(TabbedResult {
                cancelled: true,
                ..TabbedResult::default()
            }),
            UiMsg::Scrollback(_) | UiMsg::Quit => {}
        }
    }

    /// Handles one crossterm event; resize takes the dedicated W5 pass.
    fn handle_event<W: Write>(&mut self, ev: Event, term: &mut Term<W>) -> io::Result<()> {
        match ev {
            Event::Resize(w, h) => {
                // W5 RESIZE_PASS_FIRST: geometry sync + clear + draw + DSR resync
                // BEFORE any job touches stale geometry (model.go:175-195 + spike #5).
                let width_changed = w != self.width;
                self.width = w;
                self.height = h;
                self.shared.width.store(w, Ordering::Relaxed);
                if h > 0 {
                    self.shared.height.store(h, Ordering::Relaxed);
                }
                term.autoresize()?;
                term.clear()?;
                term.mark_top_dirty();
                let view = self.frame_view();
                term.draw_frame(&view)?;
                // Leave the frame dirty: the next iteration re-ensures the viewport
                // height against the resynced top.
                self.dirty = true;
                if width_changed {
                    // A WIDTH change reflows the screen and ghosts the frame's top
                    // rows; flush the staged tail (content!) into scrollback first.
                    // Height-only changes don't reflow and must not flush.
                    self.jobs.push(Job::FlushTail);
                } else {
                    // A HEIGHT change moves the frame's floor (T-40): re-apply the
                    // staging cap now, or nothing would until the next line of output.
                    self.jobs.push(Job::Retrim);
                }
            }
            Event::Key(k) => {
                self.handle_key(k);
                self.dirty = true;
            }
            Event::Paste(data) => {
                self.route_paste(&data);
                self.dirty = true;
            }
            // Focus exists for ONE reason: gating the attention ping (model.go:226-228).
            // No redraw — nothing on screen depends on it.
            Event::FocusGained => self.focused = true,
            Event::FocusLost => self.focused = false,
            Event::Mouse(_) => {}
        }
        Ok(())
    }

    /// Routes one key through the composer precedence table (keys.rs — WP46's seam).
    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        keys::update_key(self, key);
    }

    /// Routes one bracketed paste. A surface owns the input while it is open (a
    /// `/model` manual-input field, an Ask "Other…" editor), exactly as
    /// `oneshot::run_surface` routes it; otherwise the composer's `[#N …]` tag store
    /// takes it.
    pub(crate) fn route_paste(&mut self, data: &str) {
        if let Some(s) = self.surface.as_mut() {
            s.st.paste(data);
        } else {
            paste::on_paste(self, data);
        }
    }

    /// Routes a key into the open surface and applies its effect.
    pub(crate) fn route_surface_key(&mut self, key: KeyEvent) {
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        match s.st.key(key) {
            SurfaceEffect::None => {}
            SurfaceEffect::ForceRedraw => {
                // T-32: a full sequential repaint sidesteps the cell-diff renderer
                // dropping a wide rune's first cell on Tab switches (doc-mandated).
                self.force_clear = true;
            }
            SurfaceEffect::Close(result) => self.close_surface(result),
        }
        self.dirty = true;
    }

    /// Closes the open surface, replying to the parked caller exactly once.
    pub(crate) fn close_surface(&mut self, result: TabbedResult) {
        if let Some(mut s) = self.surface.take()
            && let Some(reply) = s.reply.take()
        {
            let _ = reply.send(result);
        }
        self.dirty = true;
    }

    /// Enter: trim; empty ignored; a parked waiter gets it directly, else it queues
    /// (type-ahead); the composer collapses to one row (model.go:473-488).
    pub(crate) fn submit(&mut self) {
        let text = self.composer.value().trim().to_owned();
        self.composer.reset();
        self.dirty = true;
        if text.is_empty() {
            return;
        }
        self.composer.push_history(&text);
        if let Some(w) = self.waiter.take() {
            let _ = w.reply.send(Ok(paste::make_input(&self.pastes, &text)));
        } else {
            self.queue.push(Queued::Typed(text));
        }
    }

    /// The queue as the rows the frame shows it — the ONE read the tests assert on.
    pub(crate) fn queue_rows(&self) -> Vec<&str> {
        self.queue.iter().map(Queued::row).collect()
    }

    /// Index of the newest TYPED queue entry, when there is one (the ↑ pop's target).
    pub(crate) fn newest_typed(&self) -> Option<usize> {
        self.queue.iter().rposition(|q| q.typed().is_some())
    }

    /// Removes queue entry `i` and returns its typed text (`None` when it is a notice or out of range).
    pub(crate) fn take_queued_typed(&mut self, i: usize) -> Option<String> {
        if i >= self.queue.len() || self.queue[i].typed().is_none() {
            return None;
        }
        match self.queue.remove(i) {
            Queued::Typed(text) => Some(text),
            Queued::Notice(_) => None,
        }
    }

    /// Fires the cancel at stack index `i` (0 = the turn, top = innermost), cancels
    /// everything above it, and ATOMICALLY folds the queue + any half-typed draft
    /// back into the composer — interrupt means "the situation changed", so nothing
    /// auto-sends (model.go:529-549).
    pub(crate) fn fire_cancel(&mut self, i: usize) {
        if i >= self.cancels.len() {
            return;
        }
        for token in self.cancels.drain(i..) {
            token.cancel();
        }
        // Only what the USER typed folds back — an interrupt means "the situation changed", and a host
        // notice is not the user's draft to edit. Notices stay queued so the next read still delivers them.
        let typed: Vec<String> = self
            .queue
            .extract_if(.., |q| matches!(q, Queued::Typed(_)))
            .map(|q| match q {
                Queued::Typed(t) => t,
                Queued::Notice(i) => i.text,
            })
            .collect();
        if !typed.is_empty() {
            let mut draft = typed.join("\n");
            let cur = self.composer.value().to_owned();
            if !cur.trim().is_empty() {
                draft.push('\n');
                draft.push_str(&cur);
            }
            self.composer.set_value(&draft);
            let rows = self.composer.line_count().min(MAX_COMPOSER_ROWS);
            self.composer.set_height(rows);
            self.composer.move_to_end();
        }
        self.dirty = true;
    }

    /// Idle Ctrl+C/Ctrl+D: `Err(Interrupted)` to a parked waiter — the caller's cue
    /// to exit (double-Ctrl+C-exits emerges from this two-step).
    pub(crate) fn fail_waiter_interrupted(&mut self) {
        if let Some(w) = self.waiter.take() {
            let _ = w.reply.send(Err(UiError::Interrupted));
        }
    }

    /// Assembles the frame from the model state (the WP46/WP47 slots come through
    /// their seams as plain data).
    pub(crate) fn frame_view(&mut self) -> FrameView {
        let dark = self.dark;
        let (candidates, desc) = suggest::frame_slots(&self.composer, &self.commands, self.width);
        let (width, height) = (self.width, self.height);
        let surface = self.surface.as_mut().map(|s| {
            // The Picker's inline preview is clamped against the terminal height.
            s.st.set_term_height(height);
            s.st.set_dark(dark);
            s.st.render(width)
        });
        let queue_rows = self.queue_rows();
        let composer_rows = self.composer.rows(self.width);
        let cursor = match &surface {
            // The composer's real cursor is suppressed while a surface is open; an
            // input field may export its own, in surface-block coordinates. The frame
            // builder offsets a cursor by `rows_above` (the composer block's first
            // row), so the surface block's own offset below it is the composer's
            // rows + the candidates row + the lower separator.
            Some(rendered) => rendered.cursor.map(|(x, y)| {
                let below = composer_rows.len() + usize::from(candidates.is_some()) + 1;
                (
                    x,
                    u16::try_from(below).unwrap_or(u16::MAX).saturating_add(y),
                )
            }),
            None => Some(self.composer.cursor_pos(self.width)),
        };
        let bottom = match (&surface, &desc) {
            (Some(rendered), _) => BottomZone::Surface(&rendered.rows),
            (None, Some(d)) => BottomZone::Desc(d),
            (None, None) => BottomZone::Status,
        };
        build_frame(&FrameInput {
            width: self.width,
            region: &self.region_snap,
            spin: self.spin,
            scopes_active: !self.cancels.is_empty(),
            queue: &queue_rows,
            composer_rows: &composer_rows,
            composer_cursor: cursor,
            candidates: candidates.as_deref(),
            bottom,
            status: &self.status,
            busy: self.busy.as_ref(),
            now: Instant::now(),
        })
    }
}

/// Writes the pending attention pings (model.go:145-162 `notifyCmd`): silent while the
/// terminal is focused — whoever is already watching needs no bell — otherwise one OSC 9 +
/// BEL write per ping, with a `"4;"` payload defused at the mechanism. The write sits
/// OUTSIDE `draw_frame` (both sequences are cursor-neutral), which is also what lets the
/// vt100 units find it as a plain byte substring.
fn drain_notify<W: Write>(m: &mut Model, term: &mut Term<W>) -> io::Result<()> {
    for text in std::mem::take(&mut m.notify) {
        if !m.focused {
            term.notify(&osc::defuse_notify(&text))?;
        }
    }
    Ok(())
}

/// The loop body (the tea.Program equivalent; `TUI_DESIGN` §4 "Loop iteration").
/// Runs on the dedicated `"iota-tui"` OS thread WP45 spawns; returns when
/// [`UiMsg::Quit`] lands or a terminal operation fails (WP45 stores the error and
/// cancels `done`, failing every outstanding waiter).
pub(crate) fn run_loop<W: Write, E: EventSource>(
    rx: &mpsc::Receiver<UiMsg>,
    mut events: E,
    mut term: Term<W>,
    shared: LoopShared,
) -> io::Result<()> {
    let mut m = Model::new(shared);
    let mut inserts: Vec<Vec<String>> = Vec::new();
    // The loop owns the terminal from here: focus reporting goes on so the attention ping
    // can tell "watching" from "away". NOT in `Term::new` — the one-shot picker and the
    // test harnesses build a `Term` without running this loop (T3 design D17).
    term.enable_focus_reporting()?;
    loop {
        // W10: the deadline is ALWAYS finite.
        let deadline = m.poll_deadline();
        if events.poll(deadline)? {
            loop {
                let ev = events.read()?;
                m.handle_event(ev, &mut term)?;
                if !events.poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        // Drain the mailbox: scrollback batches collect IN ORDER; the rest mutates
        // the model; Quit exits after pending inserts land.
        loop {
            match rx.try_recv() {
                Ok(UiMsg::Scrollback(rows)) => {
                    inserts.push(rows);
                    m.dirty = true;
                }
                Ok(UiMsg::Quit) => {
                    m.quit = true;
                    break;
                }
                Ok(msg) => m.apply(msg),
                Err(_) => break, // empty (or the handle side is gone)
            }
        }
        // Post-update jobs (tea.Cmd): region lock + mailbox sends — never re-entrant.
        for job in std::mem::take(&mut m.jobs) {
            match job {
                Job::FlushTail => lock(&m.shared.region).flush_tail(),
                Job::Retrim => lock(&m.shared.region).retrim(),
            }
        }
        m.tick_spin();
        m.tick_surface_refresh();
        // W2/W6: land the insert batches, pushing the tracked top.
        for batch in inserts.drain(..) {
            term.insert_lines(&batch)?;
        }
        if m.quit {
            break;
        }
        if m.dirty {
            if m.force_clear {
                term.clear()?;
                m.force_clear = false;
            }
            let view = m.frame_view();
            let cap = if m.height > 0 { m.height } else { 24 };
            let view_height = u16::try_from(view.rows.len())
                .unwrap_or(u16::MAX)
                .clamp(1, cap.max(1));
            term.ensure_height(view_height)?; // W1 + W3, coalesced to one per iteration
            term.set_title(&m.title)?;
            term.set_progress(m.progress)?; // emit-on-change, like the title
            drain_notify(&mut m, &mut term)?;
            term.draw_frame(&view)?; // W4: same iteration as the inserts above
            m.dirty = false;
        }
    }
    // close() flushed the region BEFORE sending Quit, so its scrollback already
    // landed above; drain any stragglers, then hand the shell a fresh line.
    while let Ok(msg) = rx.try_recv() {
        if let UiMsg::Scrollback(rows) = msg {
            term.insert_lines(&rows)?;
        }
    }
    term.park_cursor()?;
    Ok(())
}

#[cfg(test)]
mod queue_tests;

#[cfg(test)]
mod vt100_tests;
