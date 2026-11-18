//! The `"iota-tui"` loop thread — wart W10 `IDLE_WAKE`; mailbox drain, post-update jobs,
//! timers, the cancel-scope stack, the parked waiter, the spinner chain (`TUI_DESIGN` §4).
//!
//! - **W10** `IDLE_WAKE`: the poll deadline is ALWAYS finite —
//!   `min(IDLE_POLL_MAX 50ms, active timers)`. The mailbox is a plain `std::sync::mpsc`
//!   with no wake fd (Go's `tea.Program` woke on `Send`); an infinite idle
//!   `event::poll` would starve fire-and-forget messages (`⚠ MCP %s failed`, an async
//!   `set_title`, `set_status`, a `ReadReq` posted between polls) and DEADLOCK
//!   `close()` at idle (`UiMsg::Quit` undrained while the loop sleeps). Worst-case
//!   idle latency: one deadline (50 ms, invisible); ~0 CPU.
//! - **One write an iteration**: the committed rows land in the conversation buffer and the
//!   frame is drawn in the same iteration, as one batched write (X-55's sync rule). A resize is
//!   a size taken and every cell repainted (`docs/design/fullscreen-tui.md`); no byte depends on
//!   where the terminal's cursor is, and the terminal is never asked.
//!
//! All model state lives on this thread (zero locks inside the engine); the facade
//! reaches in only via [`UiMsg`]. The composer/keys/paste/suggest/surface modules are
//! the WP46/WP47 seams — this file's wiring is final, their bodies are provisional.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, mpsc};

use crate::ui::block::Block;
use std::time::{Duration, Instant};

use crate::ui::facade::{
    Delivered, Input, InputKind, ProgressState, StatusData, Suggestion, TURN_LABEL, TabbedResult,
    TabbedSpec, TurnBusy, UiError,
};
use crossterm::event::{Event, KeyEvent};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::clipboard::Copied;
use super::msgs::UiMsg;
use super::osc;
use super::requery::Requery;
use super::screen::{Prompt, SelectOp, ViewOp};
use super::term::Term;
use crate::ui::input::composer::Composer;
use crate::ui::input::keys;
use crate::ui::input::keys::SearchField;
use crate::ui::input::mouse::{self, MouseState};
use crate::ui::input::paste;
use crate::ui::input::suggest::{self, ListView};
use crate::ui::render::frame::{
    BottomZone, BusyView, FrameInput, FrameMap, FrameView, LIST_GAP, SHED_ORDER, TurnClock,
    build_frame,
};
use crate::ui::render::region::RegionSnapshot;
use crate::ui::surface::tabbed::SURFACE_MIN_ROWS;
use crate::ui::surface::{self, SurfaceEffect};

/// W10 `IDLE_WAKE`: the loop's poll deadline is never longer than this
/// (`TUI_CONTRACTS` §8 — a design constant; tea.Program woke on `Send`).
pub(crate) const IDLE_POLL_MAX: Duration = Duration::from_millis(50);

/// How long the terminal must have been quiet — no further resize — before the loop applies
/// the last one, writing nothing (no draw) until then. A pure throttle: a burst of `SIGWINCH`es
/// is one repaint, not one per event. Shorter than a hand's pause between drag steps, longer
/// than a burst of `SIGWINCH`es.
pub(crate) const RESIZE_QUIET: Duration = Duration::from_millis(50);

/// How often the loop looks for a running copy's outcome.
pub(crate) const COPY_POLL: Duration = Duration::from_millis(10);

/// Spinner cadence (model.go:25 — 120ms tick).
pub(crate) const SPINNER_TICK: Duration = Duration::from_millis(120);

/// The status row's job clock cadence: one repaint a second while a background job runs, and none
/// otherwise — an idle iota with no job paints nothing (scenario 14's stale-frame budget stands on
/// that).
pub(crate) const JOB_TICK: Duration = Duration::from_secs(1);

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
}

/// The loop's input-event seam: crossterm live, scripted in tests.
pub(crate) trait EventSource {
    /// Waits up to `timeout` for an event (the W10 deadline is always finite).
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    /// Reads the next event; called only after `poll` returned `true`.
    fn read(&mut self) -> io::Result<Event>;
}

/// The live [`EventSource`] over crossterm's global event stream.
///
/// crossterm is never let near a terminal that has hung up. Its default unix reader (0.29, mio)
/// left its read loop only on `WouldBlock` or a parsed event, so the EOF a hung-up tty answers with
/// (macOS: `Ok(0)`; Linux: `EIO`, which it swallows) kept it reading forever — one core at 100%,
/// the loop never back to its mailbox, `close()` waiting on it for good, and the process an orphan
/// nothing but SIGKILL ends (2026-10-02: fifty of them, left by `tmux kill-server`). The reader in
/// use now (`use-dev-tty`, Cargo.toml — the mio one also stranded a key sent with a resize) does not
/// report a hangup either. So [`HangupWatch`] waits on the terminal itself first and turns a hangup
/// into the loop's error exit; crossterm is then asked for [`CROSSTERM_SLICE`], which hands over a
/// parsed event, a buffered one, or a `SIGWINCH` that arrived meanwhile. Not a zero timeout: that
/// reader does not even look at its own buffer at zero, so a second event read in the same batch
/// would wait for the next input.
pub(crate) struct CrosstermEvents {
    /// The terminal crossterm reads (`None`: no terminal to open — crossterm then fails on its own).
    #[cfg(unix)]
    watch: Option<HangupWatch>,
}

impl CrosstermEvents {
    /// Watches the terminal crossterm itself will read (unix; elsewhere there is no such reader).
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(unix)]
            watch: HangupWatch::open(),
        }
    }
}

impl EventSource for CrosstermEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        #[cfg(unix)]
        if let Some(watch) = &self.watch {
            watch.wait(timeout)?;
            return crossterm::event::poll(CROSSTERM_SLICE);
        }
        crossterm::event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        crossterm::event::read()
    }
}

/// How long crossterm is asked once [`HangupWatch`] has waited (see [`CrosstermEvents`]): the
/// shortest wait its reader reads at all.
#[cfg(unix)]
const CROSSTERM_SLICE: Duration = Duration::from_millis(1);

/// The terminal crossterm reads, watched for a hangup before crossterm is (see [`CrosstermEvents`]).
#[cfg(unix)]
enum HangupWatch {
    /// stdin is a terminal: crossterm's `tty_fd()` reads it.
    Stdin,
    /// stdin is not: crossterm opens `/dev/tty`, and so does this side — the same device.
    DevTty(std::fs::File),
}

#[cfg(unix)]
impl HangupWatch {
    /// The descriptor crossterm reads; `None` when there is no terminal to open.
    fn open() -> Option<Self> {
        use std::io::IsTerminal as _;
        if io::stdin().is_terminal() {
            Some(Self::Stdin)
        } else {
            std::fs::File::open("/dev/tty").ok().map(Self::DevTty)
        }
    }

    /// Waits up to `timeout` for the terminal to have input; `Err` once it has hung up (or the
    /// descriptor is gone). Readable, timed out or cut short by a signal are all `Ok` — the caller
    /// asks crossterm next. The wait and the hang-up test are [`super::tty`]'s: `poll(2)` on
    /// `/dev/tty` answers `POLLNVAL` on macOS whatever the terminal's state, which once ended the
    /// loop at startup whenever stdin was not a terminal.
    fn wait(&self, timeout: Duration) -> io::Result<()> {
        use super::tty::{hung_up, readable};
        use std::os::fd::AsFd as _;
        let stdin = io::stdin();
        let fd = match self {
            Self::Stdin => stdin.as_fd(),
            Self::DevTty(f) => f.as_fd(),
        };
        match readable(fd, timeout) {
            Ok(true) if hung_up(fd)? => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the terminal hung up",
            )),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(all(test, unix))]
mod hangup_tests;

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

/// The loop model (Go `model`): every field mutates on this thread only.
pub(crate) struct Model {
    /// Terminal width (mirrors the shared atomic).
    pub(crate) width: u16,
    /// The last resize recorded and not yet applied (applied once the terminal has been quiet
    /// for `RESIZE_QUIET`).
    pub(crate) pending_resize: Option<(u16, u16)>,
    /// When the last resize event arrived.
    pub(crate) resize_seen: Instant,
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
    /// The whole-turn layer's clock while a turn runs (`UiMsg::Turn`): the busy segment's clock then, and
    /// its label [`TURN_LABEL`] between phases.
    pub(crate) turn: Option<TurnClock>,
    /// The region snapshot (the call preview) the frame renders.
    pub(crate) region_snap: RegionSnapshot,
    /// The open surface, if any.
    pub(crate) surface: Option<SurfaceOpen>,
    /// Live surface generation (the stale-tick guard's counter).
    pub(crate) surface_gen: u64,
    /// The cancel-scope stack (bottom index 0 = the turn).
    pub(crate) cancels: Vec<CancellationToken>,
    /// The slash-command table (completion + suggestion row — WP46).
    pub(crate) commands: Vec<Suggestion>,
    /// The running background jobs (the status row's job segment), oldest first.
    pub(crate) running_jobs: Vec<crate::shell::jobs::JobInfo>,
    /// When the job clock last repainted (the [`JOB_TICK`] chain, live while `running_jobs` is not
    /// empty).
    pub(crate) last_job_tick: Instant,
    /// The composer (WP46's seam; provisional body).
    pub(crate) composer: Composer,
    /// Stored multi-line pastes (`[#N …]` tags — WP46).
    pub(crate) pastes: Vec<String>,
    /// The shared spinner counter (one counter drives header AND busy glyph).
    pub(crate) spin: usize,
    /// Whether the self-stopping spinner chain is running.
    pub(crate) spin_ticking: bool,
    last_spin: Instant,
    /// A queued full clear before the next draw (T-32 surface Tab switch).
    force_clear: bool,
    /// The terminal's background tone (`UiMsg::DarkBackground`, or the terminal's own answer to
    /// [`Self::requery`]; dark is the safe default).
    dark: bool,
    /// The background asked again while the loop runs (X-108).
    pub(crate) requery: Requery,
    /// Whether the frame must be redrawn this iteration (pub(crate): the W10
    /// deadline units clear it to model a fully idle loop).
    pub(crate) dirty: bool,
    /// Set by [`UiMsg::Quit`]; the loop exits after landing pending inserts and painting
    /// the frame without the rows they came from (see `run_loop`).
    quit: bool,
    /// The terminal's progress indicator state (model.go:215); the dirty branch emits it
    /// on change through `term.set_progress`.
    pub(crate) progress: ProgressState,
    /// Attention pings not yet written (model.go:217); [`drain_notify`] writes them.
    pub(crate) notify: Vec<String>,
    /// Program-status reports not yet written (`UiMsg::ProgramStatus`), oldest first; the dirty
    /// branch writes them all, unconditionally — unlike a ping, a report is not gated on focus.
    pub(crate) program_status: Vec<String>,
    /// Whether the terminal window has focus (model.go:118). Starts TRUE: a terminal that
    /// never reports focus never pings, which is Go's behaviour — whoever is already
    /// watching needs no bell.
    pub(crate) focused: bool,
    /// View-key requests (scrolling, the dump, the way back) the next draw applies, in order.
    pub(crate) view_ops: Vec<ViewOp>,
    /// Whether the conversation is written out on the main screen (`Ctrl+O`): the next key only
    /// comes back from it.
    pub(crate) dumped: bool,
    /// The conversation search's query field while a search is on (the full screen; `keys.rs`).
    pub(crate) search: Option<SearchField>,
    /// Whether the full screen's view was scrolled back at the last draw (`/` opens a search then).
    pub(crate) view_paused: bool,
    /// A job-control stop asked for (Ctrl+Z, or SIGTSTP from outside — the full screen): the loop
    /// hands the terminal back and stops after this iteration's frame (`Term::suspend`). Unix
    /// only: a Windows console has no job control.
    #[cfg(unix)]
    pub(crate) suspend: bool,
    /// An idle Ctrl+D that found no reader parked — the REPL between a turn's end and its next
    /// `read_input` — answers that next read (tui-plan S4).
    pub(crate) pending_eof: bool,
    /// The mouse between reports (`input/mouse.rs`).
    pub(crate) mouse: MouseState,
    /// The conversation rows visible at the last draw: the screen rows above the frame (the
    /// screen's own `view_h` is the conversation's whole rectangle, the area floats over).
    pub(crate) vis_h: u16,
    /// Where the last frame's composer and surface rows were.
    pub(crate) frame_map: FrameMap,
    /// The match each completion-list row of the last frame belongs to (where a click lands).
    pub(crate) list_items: Vec<usize>,
    /// What the status row says for a moment after a selection was copied, and since when.
    pub(crate) copied: Option<(String, Instant)>,
    /// The key that hands a drag to the terminal's own selection (`Shift`; `Option` in iTerm2).
    pub(crate) native: &'static str,
    /// Whether a copy's programs are still running (the poll deadline stays short for their
    /// outcome).
    pub(crate) copy_pending: bool,
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
            pending_resize: None,
            resize_seen: Instant::now(),
            shared,
            status: StatusData::default(),
            title: String::new(),
            queue: Vec::new(),
            waiter: None,
            busy: None,
            turn: None,
            region_snap: RegionSnapshot::default(),
            surface: None,
            surface_gen: 0,
            cancels: Vec::new(),
            commands: Vec::new(),
            running_jobs: Vec::new(),
            last_job_tick: Instant::now(),
            composer: Composer::new(),
            pastes: Vec::new(),
            spin: 0,
            spin_ticking: false,
            last_spin: Instant::now(),
            force_clear: false,
            dark: true,
            requery: Requery::default(),
            dirty: true, // the first iteration paints the initial frame
            quit: false,
            progress: ProgressState::None,
            notify: Vec::new(),
            program_status: Vec::new(),
            focused: true,
            view_ops: Vec::new(),
            dumped: false,
            search: None,
            view_paused: false,
            pending_eof: false,
            #[cfg(unix)]
            suspend: false,
            mouse: MouseState::default(),
            vis_h: 0,
            frame_map: FrameMap::default(),
            list_items: Vec::new(),
            copied: None,
            native: "Shift",
            copy_pending: false,
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
        if !self.running_jobs.is_empty() {
            let next = JOB_TICK.saturating_sub(self.last_job_tick.elapsed());
            d = d.min(next.max(Duration::from_millis(1)));
        }
        if let Some(s) = &self.surface
            && s.refresh_every > Duration::ZERO
        {
            let next = s.refresh_every.saturating_sub(s.last_refresh.elapsed());
            d = d.min(next.max(Duration::from_millis(1)));
        }
        if self.pending_resize.is_some() {
            let next = RESIZE_QUIET.saturating_sub(self.resize_seen.elapsed());
            d = d.min(next.max(Duration::from_millis(1)));
        }
        let now = Instant::now();
        if let Some(next) = mouse::edge_due(self, now) {
            d = d.min(next.max(Duration::from_millis(1)));
        }
        if let Some(next) = self.requery.next_wake(now) {
            d = d.min(next.max(Duration::from_millis(1)));
        }
        if self.copy_pending {
            d = d.min(COPY_POLL);
        }
        if let Some((_, at)) = &self.copied {
            let next = mouse::COPIED_SHOWN.saturating_sub(now.saturating_duration_since(*at));
            d = d.min(next.max(Duration::from_millis(1)));
        }
        d
    }

    /// What a copy became, for the status row: done (by a tool or by OSC 52 — the user is told
    /// the same either way, the way is no word of theirs), or too large for the one way left.
    pub(crate) fn copy_done(&mut self, c: Copied) {
        let n = |chars: usize| {
            let what = if chars == 1 {
                "character"
            } else {
                "characters"
            };
            format!("{chars} {what}")
        };
        let text = match c {
            Copied::Done { chars, .. } => format!(
                "copied {} · {}+drag selects in the terminal",
                n(chars),
                self.native
            ),
            Copied::TooLarge { chars } => {
                format!("not copied: {} is too large — use Ctrl+O", n(chars))
            }
        };
        self.copied = Some((text, Instant::now()));
        self.copy_pending = false;
        self.dirty = true;
    }

    /// The mouse's timers: a drag held at the viewport's edge scrolls on, and the copied notice
    /// goes once it has been shown long enough.
    pub(crate) fn tick_mouse(&mut self) {
        let now = Instant::now();
        mouse::tick_edge(self, now);
        if self
            .copied
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= mouse::COPIED_SHOWN)
        {
            self.copied = None;
            self.dirty = true;
        }
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
        if self.busy.is_none() && self.turn.is_none() && self.region_snap.label.is_empty() {
            self.spin_ticking = false;
            return;
        }
        if self.last_spin.elapsed() >= SPINNER_TICK {
            self.spin = self.spin.wrapping_add(1);
            self.last_spin = Instant::now();
            self.dirty = true;
        }
    }

    /// One job-clock repaint when due; nothing at all with no job running, so the chain costs an idle
    /// loop nothing.
    pub(crate) fn tick_jobs(&mut self) {
        if self.running_jobs.is_empty() {
            return;
        }
        if self.last_job_tick.elapsed() >= JOB_TICK {
            self.last_job_tick = Instant::now();
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
                // Every snapshot changes the rendered preview; a labelled one
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
            UiMsg::Jobs(jobs) => {
                // A fresh set restarts the clock chain from now, so the first tick is a full second out.
                self.running_jobs = jobs;
                self.last_job_tick = Instant::now();
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
            UiMsg::ProgramStatus(seq) => {
                self.program_status.push(seq);
                self.dirty = true;
            }
            UiMsg::DarkBackground(dark) => {
                self.dark = dark;
                self.dirty = true;
            }
            // Nothing to draw: the query is written by this iteration, the answer is an event.
            UiMsg::RequeryBackground(reply) => self.requery.ask(reply),
            UiMsg::BackgroundFromHost => self.requery.leave_to_host(),
            #[cfg(unix)]
            UiMsg::Suspend => {
                self.suspend = true;
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
            UiMsg::Turn(state) => {
                let now = Instant::now();
                match (state, self.turn.as_mut()) {
                    (TurnBusy::Off, _) => self.turn = None,
                    (TurnBusy::Running, Some(t)) => t.resume(now),
                    (TurnBusy::Waiting, Some(t)) => t.pause(now),
                    (TurnBusy::Running | TurnBusy::Waiting, None) => {
                        let mut t = TurnClock::start(now);
                        if state == TurnBusy::Waiting {
                            t.pause(now);
                        }
                        self.turn = Some(t);
                        self.ensure_spin();
                    }
                }
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
                // The queue drains FIFO before a waiter parks (model.go:280-288); a Ctrl+D pressed
                // while no reader was parked ends the read after it.
                if self.queue.is_empty() && std::mem::take(&mut self.pending_eof) {
                    let _ = reply.send(Err(UiError::Interrupted));
                } else if self.queue.is_empty() {
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
            UiMsg::Deliver { input, cap, reply } => {
                // `Enqueue`'s two branches, reported — plus the cap, counted over the messages alone: a
                // flood from outside must never crowd out what the user typed or a job's notice.
                let state = if let Some(w) = self.waiter.take() {
                    let _ = w.reply.send(Ok(input));
                    Delivered::Woken
                } else if self.queued_messages() >= cap {
                    Delivered::Full
                } else {
                    let ahead = self.queue.len();
                    self.queue.push(Queued::Notice(input));
                    Delivered::Queued { ahead }
                };
                let _ = reply.send(state);
                self.dirty = true;
            }
            UiMsg::ReadCancel { id, ack } => {
                // Revoke only the SAME waiter (model.go:302-306). The ack goes out after: an input served
                // to the waiter before this message is already on its reply channel (X-84).
                if self.waiter.as_ref().is_some_and(|w| w.id == id) {
                    self.waiter = None;
                }
                let _ = ack.send(());
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

    /// Handles one crossterm event. The terminal's answer to the background question arrives
    /// here too, as the keys crossterm makes of it (`requery.rs`): it is taken out first, and a flip
    /// is ONE ordinary dirty frame — the shades are read at draw time, so nothing is cleared.
    pub(crate) fn handle_event(&mut self, ev: Event) {
        let fed = self.requery.feed(ev, Instant::now());
        if let Some(ev) = fed.pass {
            self.dispatch_event(ev);
        }
        if let Some(reply) = fed.reply
            && let Some(dark) = self.requery.answer(reply)
            && dark != self.dark
        {
            self.dark = dark;
            self.dirty = true;
        }
    }

    /// Handles one event that is the user's (or the window's), not the terminal's reply.
    fn dispatch_event(&mut self, ev: Event) {
        match ev {
            Event::Resize(w, h) => {
                // A resize is only RECORDED here; `apply_resize` runs once the terminal has
                // been quiet for `RESIZE_QUIET` (the loop writes nothing meanwhile).
                self.pending_resize = Some((w, h));
                self.resize_seen = Instant::now();
                // A drag across a resize is over: the layout under it is going (the screen drops
                // the selection with the old width).
                mouse::end_drag(self, SelectOp::Release);
            }
            Event::Key(k) => {
                self.handle_key(k);
                self.dirty = true;
            }
            Event::Paste(data) => {
                self.route_paste(&data);
                self.dirty = true;
            }
            // Focus gates the attention ping (model.go:226-228), and coming back is when the
            // terminal is asked for its background again: the OS may have switched its appearance
            // while the window was away (X-108). No redraw — nothing on screen depends on focus.
            Event::FocusGained => {
                self.focused = true;
                self.requery.focus_gained();
            }
            Event::FocusLost => {
                self.focused = false;
                // The button may come up outside the window and never be reported: a drag held at
                // the viewport's edge would scroll on for good.
                if mouse::end_drag(self, SelectOp::Release) {
                    self.dirty = true;
                }
            }
            Event::Mouse(ev) => {
                mouse::update_mouse(self, ev);
                self.dirty = true;
            }
        }
    }

    /// Routes one key through the composer precedence table (keys.rs — WP46's seam). A key ends a
    /// drag whose release was never reported and drops the mouse's selection first: what is typed
    /// next is not about it.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        if !mouse::end_drag(self, SelectOp::Clear) && std::mem::take(&mut self.mouse.selected) {
            self.view_ops.push(ViewOp::Select(SelectOp::Clear));
        }
        keys::update_key(self, key);
    }

    /// Routes one bracketed paste. A surface owns the input while it is open (a
    /// `/model` manual-input field, an Ask "Other…" editor), exactly as
    /// `oneshot::run_surface` routes it; otherwise the composer's `[#N …]` tag store
    /// takes it.
    pub(crate) fn route_paste(&mut self, data: &str) {
        if let Some(s) = self.surface.as_mut() {
            s.st.paste(data);
        } else if let Some(op) = self.search.as_mut().and_then(|s| s.paste(data)) {
            self.view_ops.push(ViewOp::Search(op));
        } else {
            paste::on_paste(self, data);
        }
    }

    /// Routes a key into the open surface and applies its effect.
    pub(crate) fn route_surface_key(&mut self, key: KeyEvent) {
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        let effect = s.st.key(key);
        self.surface_effect(effect);
    }

    /// Applies what a key or a click did to the open surface.
    pub(crate) fn surface_effect(&mut self, effect: SurfaceEffect) {
        match effect {
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

    /// How many queued entries came from another process (`InputKind::Message`).
    fn queued_messages(&self) -> usize {
        self.queue
            .iter()
            .filter(|q| matches!(q, Queued::Notice(i) if i.kind == InputKind::Message))
            .count()
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
            self.composer.move_to_end();
        }
        self.dirty = true;
    }

    /// Idle Ctrl+C/Ctrl+D: `Err(Interrupted)` to a parked waiter — the caller's cue
    /// to exit (double-Ctrl+C-exits emerges from this two-step). With no waiter parked (the REPL
    /// between a turn's end and its next read) a Ctrl+D is kept for that read: it can only mean the
    /// end of input. A Ctrl+C is not — racing a turn's end, it may have been meant to cancel that
    /// turn, and kept it would end the program instead.
    pub(crate) fn fail_waiter_interrupted(&mut self, eof: bool) {
        if let Some(w) = self.waiter.take() {
            let _ = w.reply.send(Err(UiError::Interrupted));
        } else if eof {
            self.pending_eof = true;
        }
    }

    /// The last recorded resize, once the terminal has been quiet for `RESIZE_QUIET`: the size
    /// taken — the shared atomics, the screen — and the frame left dirty, so the iteration's one
    /// batched draw repaints every cell (`Screen::draw_frame` clears after any resize, the same
    /// size too).
    pub(crate) fn apply_resize<W: Write>(&mut self, term: &mut Term<W>) {
        let Some((w, h)) = self.pending_resize.take() else {
            return;
        };
        self.width = w;
        self.height = h;
        self.shared.width.store(w.max(1), Ordering::Relaxed);
        if h > 0 {
            self.shared.height.store(h, Ordering::Relaxed);
        }
        if w > 0 && h > 0 {
            term.resize(ratatui::layout::Size::new(w, h));
        }
        self.dirty = true;
    }

    /// The search line's query and cursor, while a search is on.
    pub(crate) fn search_prompt(&mut self) -> Option<Prompt> {
        let width = usize::from(self.width.max(2));
        self.search.as_mut().map(|s| s.prompt(width))
    }

    /// Assembles the frame from the model state (the WP46/WP47 slots come through
    /// their seams as plain data).
    pub(crate) fn frame_view(&mut self) -> FrameView {
        let dark = self.dark;
        let fw = self.width.max(1);
        let composer_rows = self.composer.rows(fw);
        let now = Instant::now();
        let busy = self.busy_view(now);
        // An open surface is drawn over the conversation's viewport (C13): it may take the rows
        // the frame's own rows leave — the viewport yields them while it is open — but a panel
        // with no `height` stays under the interaction cap (`interaction_rows`). The completion
        // list takes rows the same way, under the same cap, from the conversation and never from
        // the composer; it is not drawn while a surface owns the keys or a search is on.
        let room = if self.surface.is_some() {
            let others =
                self.rows_besides(&composer_rows, BottomZone::Surface(&[]), busy.as_ref(), now);
            // A frame taller than the screen still leaves the surface its minimum: the screen
            // keeps the frame's bottom then, and the rows above the surface yield.
            self.height
                .saturating_sub(u16::try_from(others).unwrap_or(u16::MAX))
                .max(SURFACE_MIN_ROWS.min(self.height))
        } else {
            0
        };
        let mut shed = 0;
        // No list during a search either: the search line and its hit need the rows the area
        // would cover.
        let list = if self.surface.is_none()
            && self.search.is_none()
            && !suggest::matches_for(&self.composer, &self.commands).is_empty()
        {
            let others = self.rows_besides(&composer_rows, BottomZone::Status, busy.as_ref(), now);
            // An interaction area never exceeds half the screen or 15 rows: 200 matches scroll
            // inside the cap and the conversation keeps the rest. The gap row under the list
            // comes out of the screen, not out of the cap: the cap bounds the list's own rows.
            let screen = usize::from(self.height);
            let room = screen
                .saturating_sub(others + LIST_GAP)
                .min(crate::ui::interaction_rows(screen));
            let mut list = suggest::frame_list(&mut self.composer, &self.commands, fw, room, dark);
            // A screen too short for the highlighted item alone: the rows above the list go first
            // (the screen cuts them off the top), then the gap and the chrome (`SHED_ORDER`), then
            // the item's description rows from the bottom — never its label, never the composer.
            // `others` counts the two separators and the status row; the gap is the fourth.
            let above = others - composer_rows.len() - (SHED_ORDER - LIST_GAP);
            let len = list.rows.len();
            let over = (others + LIST_GAP + len)
                .saturating_sub(screen)
                .saturating_sub(above);
            shed = over.min(SHED_ORDER);
            let keep = len.saturating_sub(over - shed).max(1);
            list.rows.truncate(keep);
            list.items.truncate(keep);
            list
        } else {
            ListView::default()
        };
        let surface = self.surface.as_mut().map(|s| {
            s.st.set_room(room, self.height);
            s.st.set_dark(dark);
            s.st.render(fw)
        });
        let queue_rows = self.queue_rows();
        let cursor = match &surface {
            // The composer's real cursor is suppressed while a surface is open; an
            // input field may export its own, in surface-block coordinates. The frame
            // builder offsets a cursor by `rows_above` (the composer block's first
            // row), so the surface block's own offset below it is the composer's
            // rows + the lower separator (no list is drawn under a surface).
            Some(rendered) => rendered.cursor.map(|(x, y)| {
                let below = composer_rows.len() + 1;
                (
                    x,
                    u16::try_from(below).unwrap_or(u16::MAX).saturating_add(y),
                )
            }),
            None => Some(self.composer.cursor_pos(fw)),
        };
        let bottom = match &surface {
            Some(rendered) => BottomZone::Surface(&rendered.rows),
            None => BottomZone::Status,
        };
        let notice = self.copied.as_ref().map(|(n, _)| n.as_str());
        let view = build_frame(&FrameInput {
            width: fw,
            region: &self.region_snap,
            spin: self.spin,
            scopes_active: !self.cancels.is_empty(),
            queue: &queue_rows,
            queue_editable: self.newest_typed().is_some(),
            composer_rows: &composer_rows,
            composer_cursor: cursor,
            list: &list.rows,
            bottom,
            status: &self.status,
            busy: busy.as_ref(),
            jobs: &self.running_jobs,
            now,
            notice,
            shed,
        });
        self.frame_map = view.map;
        self.list_items = list.items;
        view
    }

    /// How many rows the frame has without the completion list, `bottom` in its bottom zone.
    fn rows_besides(
        &self,
        composer_rows: &[String],
        bottom: BottomZone<'_>,
        busy: Option<&BusyView>,
        now: Instant,
    ) -> usize {
        let queue_rows = self.queue_rows();
        build_frame(&FrameInput {
            width: self.width.max(1),
            region: &self.region_snap,
            spin: self.spin,
            scopes_active: !self.cancels.is_empty(),
            queue: &queue_rows,
            queue_editable: self.newest_typed().is_some(),
            composer_rows,
            composer_cursor: None,
            list: &[],
            bottom,
            status: &self.status,
            busy,
            jobs: &self.running_jobs,
            now,
            notice: None,
            shed: 0,
        })
        .rows
        .len()
    }

    /// The status row's busy segment at `now` (docs/design/busy-span.md §3.1): inside a turn, the phase's
    /// label and detail — [`TURN_LABEL`] between phases — over the TURN's clock, so a phase change only
    /// relabels the segment; outside one, the phase with its own clock, as before.
    pub(crate) fn busy_view(&self, now: Instant) -> Option<BusyView> {
        let Some(turn) = &self.turn else {
            return self.busy.clone();
        };
        let (label, detail) = self.busy.as_ref().map_or_else(
            || (TURN_LABEL.to_owned(), String::new()),
            |b| (b.label.clone(), b.detail.clone()),
        );
        Some(BusyView {
            label,
            detail,
            since: now.checked_sub(turn.elapsed(now)).unwrap_or(now),
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
    m.native = term.native_modifier();
    let mut inserts: Vec<Vec<Block>> = Vec::new();
    // The loop owns the terminal from here: focus reporting goes on so the attention ping
    // can tell "watching" from "away". NOT in `Term::new` — the one-shot picker and the
    // test harnesses build a `Term` without running this loop (T3 design D17).
    term.enable_focus_reporting()?;
    loop {
        // W10: the deadline is ALWAYS finite.
        let deadline = m.poll_deadline();
        if events.poll(deadline)? {
            // A run of resizes in one batch is one size: only the last is taken (the terminal is
            // already there).
            let mut pending: Option<Event> = None;
            loop {
                let ev = events.read()?;
                if let Some(prev) = pending.take()
                    && !(matches!(prev, Event::Resize(..)) && matches!(ev, Event::Resize(..)))
                {
                    m.handle_event(prev);
                }
                pending = Some(ev);
                if !events.poll(Duration::ZERO)? {
                    break;
                }
            }
            if let Some(ev) = pending {
                m.handle_event(ev);
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
        // A recorded resize is applied once the terminal is quiet; until then nothing below
        // writes a byte (the geometry may be moving under it).
        if m.pending_resize.is_some() && (m.resize_seen.elapsed() >= RESIZE_QUIET || m.quit) {
            m.apply_resize(&mut term);
        }
        let resizing = m.pending_resize.is_some();
        // One iteration's screen writes — the title and progress, the frame — go out as ONE
        // write, a DEC 2026 synchronized update when it is larger than a transport's read block
        // (X-55): a split write is never shown half-drawn by an emulator that knows the mode.
        if !resizing {
            term.begin_batch();
        }
        m.tick_spin();
        m.tick_jobs();
        m.tick_surface_refresh();
        m.tick_mouse();
        m.requery.tick(Instant::now());
        // The background question, when one is due: written here, read nowhere — its answer is an
        // event of a later iteration.
        if !resizing && m.requery.take_query(Instant::now()) {
            term.write_raw(osc::OSC11_QUERY)?;
        }
        // A copy whose programs were running: their outcome is the status row's to say.
        if !resizing
            && term.copy_pending()
            && let Some(c) = term.copy_selection()?
        {
            m.copy_done(c);
        }
        // The committed rows join the conversation the frame below shows.
        if !resizing {
            for batch in inserts.drain(..) {
                term.push(batch);
            }
        }
        if m.dirty && !resizing {
            if m.force_clear {
                term.clear()?;
                m.force_clear = false;
            }
            for op in std::mem::take(&mut m.view_ops) {
                term.view(op)?;
            }
            if let Some(c) = term.copy_selection()? {
                m.copy_done(c);
            }
            let view = m.frame_view();
            term.set_title(&m.title)?;
            term.set_progress(m.progress)?; // emit-on-change, like the title
            drain_notify(&mut m, &mut term)?;
            for seq in std::mem::take(&mut m.program_status) {
                term.write_raw(seq.as_bytes())?;
            }
            let prompt = m.search_prompt();
            // The same iteration as the rows above; the rows it left out at the top of a frame
            // taller than the screen are what a click on the frame counts from.
            m.frame_map.cut = term.draw_frame(&view, prompt.as_ref())?;
            m.view_paused = term.view_paused();
            m.vis_h = term.view_height();
            m.copy_pending = term.copy_pending();
            m.dirty = false;
        }
        if !resizing {
            term.end_batch()?;
        }
        // Job control: the terminal goes back to the shell and the process stops; `fg` comes back
        // here. The size may have changed meanwhile, and nothing on the screen is ours any more: the
        // next frame re-enters the alternate screen and paints every cell.
        #[cfg(unix)]
        if std::mem::take(&mut m.suspend) && !m.quit {
            if let Some(size) = term.suspend()? {
                m.pending_resize = Some((size.width, size.height));
                m.resize_seen = Instant::now()
                    .checked_sub(RESIZE_QUIET)
                    .unwrap_or_else(Instant::now);
            }
            m.dirty = true;
        }
        if m.quit {
            break;
        }
    }
    // close() flushed the region BEFORE sending Quit, so its rows already landed; take any
    // stragglers, then leave the screen with the conversation written out.
    while let Ok(msg) = rx.try_recv() {
        if let UiMsg::Scrollback(rows) = msg {
            term.push(rows);
        }
    }
    term.leave()?;
    Ok(())
}

#[cfg(test)]
mod queue_tests;

#[cfg(test)]
mod vt100_tests;
