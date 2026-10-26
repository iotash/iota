//! Host integration (internal/host): the terminal or terminal multiplexer the chat runs inside is
//! told what the conversation is doing — a state for its progress indicator, an attention ping when
//! the user is needed, the session it is persisting into, a clean-up on exit — through
//! per-capability traits a [`Host`] may or may not implement. The [`Presenter`] fans one call out
//! to the FIRST host that has the capability (host.go:97-148), deduplicates repeated states — the
//! first one always goes out, so a host learns the chat exists before its first turn — and gates
//! pings on the `notify` config key.
//!
//! The one capability that is NOT first-host-only runs the other way: a host may tell the MODEL
//! where it is. Every [`EnvironmentContributor`] adds its `key: value` facts to the harness prompt's
//! `<environment>` block ([`Presenter::environment`] gathers them all, in detection order), so a
//! multiplexer's pane id reaches the model through the host layer and never as a special case in
//! `agents::harness` (brain page `host-integration`, 2026-09-21).
//!
//! Hosts: the herdr multiplexer (`herdr::HerdrHost`, told the pane's lifecycle over its socket), the
//! cmux multiplexer (`cmux::CmuxHost`, driven through its CLI) — both detected from the environment —
//! and the plain ANSI terminal ([`ansi::AnsiHost`], the fallback the command always appends).
//!
//! **The innermost host is exclusive** (decided 2026-09-21). Hosts nest — herdr runs inside a cmux
//! window — and a pane inherits its environment from the process that spawned it: a herdr pane
//! carries the `CMUX_SURFACE_ID` the herdr SERVER was born with, which may be stale or another
//! window's, and a status row set through it would land on the wrong surface. So iota talks to the
//! innermost host only: `DETECTORS` is ordered inner to outer (herdr before cmux), and
//! [`Presenter::new`] keeps the FIRST detector that matches, then the ANSI fallback — never two
//! detected hosts. A probe carrying both `HERDR_*` and `CMUX_SURFACE_ID` yields herdr alone, and
//! cmux is not even asked. The per-capability fan-out over the remaining pair (the detected host
//! and the terminal) is what Go had: the multiplexer takes the state (and the session), the terminal
//! keeps the OSC 9 ping and the OSC 11 answer, both are closed, and `<environment>` gets the one
//! host's lines — `host: <name>`, then the ids its CLI takes.
//!
//! **A running background job keeps the host at Busy** (decided 2026-09-24). What a host is shown
//! is the turn's state with one rewrite: while the turn is `Idle` and at least one shell job runs
//! (`Presenter::set_jobs`), the host sees `Busy`. A job's result still comes back as a turn of its
//! own — the notice wakes the loop — so the conversation is not done until the job is; herdr's
//! `agent prompt --wait` therefore waits for it too. That is also why a job that leaves the running
//! set still counts until its notice is taken (`Presenter::notice_taken`): the registry drops the
//! job BEFORE it delivers the notice, and the gap between the two must not flash `Idle`. The end
//! arrives as a fact (`Presenter::job_ended`, with the set that no longer lists the job), not as a
//! drop in the count: a count can hide an end behind a start, or a job that never started. `Busy`, `NeedsInput` and `Error` pass through
//! untouched: a chat blocked on an approval is blocked whatever runs behind it. The rewrite lives
//! here, so every host gets it and no REPL call site of `set_state` changes.
//!
//! **The status row's whole-turn layer is derived here too** (docs/design/busy-span.md §3.5, point 8,
//! decided 2026-10-14). The same inputs that give the host its state give the chat's own screen whether a
//! turn is running ([`crate::ui::facade::TurnBusy`], sent to the facade [`Presenter::show_turn_on`]
//! names): the turn's state BEFORE the jobs rewrite — `Busy` runs, `NeedsInput` waits (its clock stands
//! still), `Idle` and `Error` are no turn. So the two can never drift inside a turn, and outside one their
//! difference is this one rule: a job still running holds the host at `Busy`, while the status row shows
//! only its static job segment (point 5, B).

pub mod ansi;
pub(crate) mod background;
pub(crate) mod cmux;
pub(crate) mod herdr;

pub use ansi::AnsiHost;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::BoxFuture;
pub use crate::app::env::ChildEnv;
use crate::app::env::Env;
use crate::sync::lock;
use crate::ui::facade::{TurnBusy, Ui};

/// What the conversation is doing (host.go:22-29).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// Waiting for the user.
    #[default]
    Idle,
    /// A turn is running.
    Busy,
    /// Blocked on the user mid-turn (approval, an interactive tool).
    NeedsInput,
    /// The turn failed.
    Error,
}

/// Why the user is being pinged (host.go:32-38).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The turn is blocked on the user.
    NeedsInput,
    /// The reply landed.
    Done,
    /// The turn failed.
    Failed,
}

/// One attention ping (host.go:43-46).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Why.
    pub kind: Kind,
    /// The banner text (already a one-liner; the ANSI host sanitizes it like a title).
    pub text: String,
}

/// A host the chat runs inside; every capability is optional (Go's optional interfaces become
/// `Option<&dyn …>` accessors with `None` defaults).
pub trait Host: Send + Sync {
    /// The host's name (`"cmux"`, `"terminal"`).
    fn name(&self) -> &str;
    /// The progress-state capability.
    fn as_state_reporter(&self) -> Option<&dyn StateReporter> {
        None
    }
    /// The attention-ping capability.
    fn as_notifier(&self) -> Option<&dyn Notifier> {
        None
    }
    /// The background-colour capability.
    fn as_background(&self) -> Option<&dyn BackgroundReporter> {
        None
    }
    /// The exit clean-up capability.
    fn as_closer(&self) -> Option<&dyn Closer> {
        None
    }
    /// The session-identity capability.
    fn as_session_reporter(&self) -> Option<&dyn SessionReporter> {
        None
    }
    /// The `<environment>` capability.
    fn as_environment(&self) -> Option<&dyn EnvironmentContributor> {
        None
    }
    /// The children's-environment capability.
    fn as_child_env(&self) -> Option<&dyn ChildEnvironment> {
        None
    }
}

/// Shows the conversation's state.
pub trait StateReporter: Send + Sync {
    /// Shows `s` (already deduplicated by the presenter).
    fn set_state(&self, s: State);
}

/// Pings the user.
pub trait Notifier: Send + Sync {
    /// Shows `e`.
    fn notify(&self, e: &Event);
}

/// Knows whether the terminal background is dark.
pub trait BackgroundReporter: Send + Sync {
    /// `Some(dark)` when the host knows, `None` otherwise. Asynchronous: cmux answers through an RPC
    /// child under a deadline.
    fn dark_background(&self) -> BoxFuture<'_, Option<bool>>;
}

/// Cleans up on exit.
pub trait Closer: Send + Sync {
    /// Clears whatever the host shows for this chat; bounded.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// Is told which session the chat persists into.
pub trait SessionReporter: Send + Sync {
    /// Records session `id`, whose bundle is (or will be) the directory `path`. Called when a chat
    /// starts with a bundle, when `/save` mints one, and when `/session` switches; never for an
    /// ephemeral chat.
    fn report_session(&self, id: &str, path: &Path);
}

/// Tells the model where it runs: facts for the harness prompt's `<environment>` block.
pub trait EnvironmentContributor: Send + Sync {
    /// `(key, value)` pairs, each printed as one `key: value` line after the run's own facts. A host
    /// names itself first (`host: cmux`), then what the model may act on (`cmux surface: <id>`).
    fn environment(&self) -> Vec<(String, String)>;
}

/// Knows which of the variables it put in the run's environment no child of the chat may inherit: a
/// variable that would let a program the model starts (a shell command, an MCP stdio server) speak to the
/// host AS the chat. A host implements it when the integrations it ships for other agents read that
/// variable: an agent started from the shell tool would otherwise report into the chat's own pane and take
/// its status over (the herdr host's doc names the variable and the evidence). Which variables, and why,
/// lives in the host's own file; nothing outside `crate::host` names one.
pub trait ChildEnvironment: Send + Sync {
    /// The variable names, removed from every child's environment ([`child_env`], [`ChildEnv::apply`]).
    fn withheld(&self) -> Vec<String>;
}

/// What every child of the chat is spawned without: the names the hosts' [`ChildEnvironment`]s withhold,
/// gathered like the `<environment>` facts — every host given, in order, each name once. The command gives
/// it the host [`detect`] found (the terminal fallback, built once the screen exists, withholds nothing).
/// The [`ChildEnv`] it returns is the mechanism, which lives at the bottom of the graph (`app::env`) so the
/// shell and MCP layers can apply it without knowing a host exists.
pub fn child_env<'a>(hosts: impl IntoIterator<Item = &'a dyn Host>) -> ChildEnv {
    ChildEnv::new(
        hosts
            .into_iter()
            .filter_map(Host::as_child_env)
            .flat_map(ChildEnvironment::withheld),
    )
}

/// `exec.LookPath` as a closure (`None` when not found).
pub type LookPathFn = Box<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// What the detectors read (host.go:71-74): the run's environment (`os.Getenv`) and a `PATH`
/// lookup, both injected by the command so this module never touches the process environment
/// itself.
pub struct Probe {
    /// The run's environment.
    pub env: Env,
    /// `exec.LookPath` (`None` when not found).
    pub look_path: LookPathFn,
}

/// A host detector: `Some(host)` when the environment says the chat runs inside it.
pub type Detector = fn(&Probe) -> Option<Box<dyn Host>>;

/// The detectors, innermost host first (host.go:84 had one). Only the first match is kept: herdr
/// before cmux, because a herdr pane inside a cmux window inherits a `CMUX_SURFACE_ID` that is the
/// server's, not the pane's (the module doc).
pub(crate) const DETECTORS: &[Detector] = &[herdr::detect_herdr, cmux::detect_cmux];

/// The innermost host the environment names, or `None` (the module doc): the first detector that matches.
/// Run it once per process — a detected host spawns its worker task, so it MUST be called inside the
/// tokio runtime — and hand the host to [`Presenter::over`]. Nothing is reported to it until the presenter
/// says so: a run that ends before then drops a host that never spoke.
pub fn detect(probe: &Probe) -> Option<Box<dyn Host>> {
    DETECTORS.iter().find_map(|d| d(probe))
}

/// The capability set a test host advertises (`crate::testing::RecordingHost`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// Implements [`StateReporter`].
    pub state: bool,
    /// Implements [`Notifier`].
    pub notify: bool,
    /// Implements [`BackgroundReporter`].
    pub background: bool,
    /// Implements [`Closer`].
    pub close: bool,
    /// Implements [`SessionReporter`].
    pub session: bool,
    /// Implements [`EnvironmentContributor`].
    pub environment: bool,
}

/// Fans the conversation's signals out to the detected hosts (host.go:90-148): the FIRST host
/// with a capability serves it; states are deduplicated; pings are gated by the `notify` key.
pub struct Presenter {
    hosts: Vec<Box<dyn Host>>,
    /// The name of the host a detector matched — the banner names it; the fallback is not one.
    detected: Option<String>,
    notify_on: bool,
    /// The inputs and the last state reported (see [`Shown`]).
    state: Mutex<Shown>,
    /// The facade the whole-turn layer is shown on, when there is a screen (the module doc).
    turn_view: Option<Arc<dyn Ui>>,
}

/// What the presenter's state output is computed from, and what it last sent.
#[derive(Default)]
struct Shown {
    /// The turn's state (`set_state`'s argument); `None` before the first.
    requested: Option<State>,
    /// How many shell jobs are running (`set_jobs`).
    jobs: usize,
    /// Jobs that ended (`job_ended`) whose notice the loop has not taken yet (`notice_taken`).
    owed: usize,
    /// The last state reported; `None` until the first, which is therefore never deduplicated.
    sent: Option<State>,
    /// The last whole-turn layer shown.
    turn_sent: TurnBusy,
}

impl Shown {
    /// The state a host sees: the turn's, except that an idle turn with jobs running is `Busy`
    /// (the module doc).
    fn effective(&self) -> Option<State> {
        match self.requested {
            Some(State::Idle) if self.jobs + self.owed > 0 => Some(State::Busy),
            r => r,
        }
    }

    /// The status row's whole-turn layer: the turn's own state, before the jobs rewrite (the module doc).
    fn turn(&self) -> TurnBusy {
        match self.requested {
            Some(State::Busy) => TurnBusy::Running,
            Some(State::NeedsInput) => TurnBusy::Waiting,
            Some(State::Idle | State::Error) | None => TurnBusy::Off,
        }
    }
}

impl Presenter {
    /// Runs the detectors until one matches — the innermost host, and no other (the module doc) —
    /// then appends `fallback` (host.go:97-108, minus Go's "every detector" collect). MUST be called
    /// inside the tokio runtime: a detected host spawns its worker task.
    pub fn new(env: &Probe, fallback: Option<Box<dyn Host>>, notify: bool) -> Self {
        Self::over(detect(env), fallback, notify)
    }

    /// [`Self::new`] over a host [`detect`] already found: the command detects once, at assembly — the
    /// children's environment ([`ChildEnv`]) is needed before the presenter exists, by the shell tool and
    /// by the MCP servers that start connecting before the screen does — and hands the host over here.
    pub fn over(
        detected: Option<Box<dyn Host>>,
        fallback: Option<Box<dyn Host>>,
        notify: bool,
    ) -> Self {
        let name = detected.as_ref().map(|h| h.name().to_owned());
        let mut hosts: Vec<Box<dyn Host>> = detected.into_iter().collect();
        if let Some(f) = fallback {
            hosts.push(f);
        }
        Self {
            detected: name,
            ..Self::with_hosts(hosts, notify)
        }
    }

    /// A presenter over an explicit host list (tests; the L3 fixtures use an empty one). None of
    /// them counts as detected.
    pub fn with_hosts(hosts: Vec<Box<dyn Host>>, notify: bool) -> Self {
        Self {
            hosts,
            detected: None,
            notify_on: notify,
            state: Mutex::new(Shown::default()),
            turn_view: None,
        }
    }

    /// Shows the whole-turn layer on `ui`'s status row (the module doc). Without it — headless, and the
    /// fixtures that leave it out — the layer is only computed.
    #[must_use]
    pub fn show_turn_on(mut self, ui: Arc<dyn Ui>) -> Self {
        self.turn_view = Some(ui);
        self
    }

    /// The name of the host a detector matched (`herdr`, `cmux`) — what the banner's mode row
    /// ends with. `None` in a plain terminal: the ANSI fallback is not a detected host.
    pub fn detected_host(&self) -> Option<&str> {
        self.detected.as_deref()
    }

    /// Shows `s` on the first [`StateReporter`]; a repeated state is not re-sent
    /// (host.go:113-124). Unlike Go, the presenter starts with NO state rather than `Idle`: the
    /// loop's own `Idle`, reported once the banner is up, reaches the host — a herdr pane is
    /// listed from that moment, not from the first turn — and only a repeat is dropped. An idle
    /// turn with jobs running is shown as `Busy` (the module doc).
    pub fn set_state(&self, s: State) {
        self.update(|shown| shown.requested = Some(s));
    }

    /// The turn's state as last reported ([`Self::set_state`]'s argument, before the jobs rewrite); `None`
    /// before the first. What a message from another process is refused on (`State::NeedsInput`: the chat
    /// waits on the user).
    pub fn turn_state(&self) -> Option<State> {
        lock(&self.state).requested
    }

    /// Records how many shell jobs are running: an idle chat with one is shown as `Busy`.
    /// Deduplicated like [`Self::set_state`]; before the first `set_state` it only records. It says
    /// nothing about ends — a job that ended arrives as [`Self::job_ended`] — so a drop here (a job
    /// that never started) owes no notice.
    pub fn set_jobs(&self, n: usize) {
        self.update(|shown| shown.jobs = n);
    }

    /// One job ended, and `n` are still running: its notice is on the way and keeps the chat `Busy`
    /// until [`Self::notice_taken`] — the interactive loop delivers one for every job that ends. One
    /// update, so the job leaving the count and its notice being owed never show `Idle` between them.
    pub fn job_ended(&self, n: usize) {
        self.update(|shown| {
            shown.owed += 1;
            shown.jobs = n;
        });
    }

    /// The loop took a job's notice into a turn: it no longer holds the chat busy by itself. Sends
    /// nothing — the turn it joined reports its own state, which is what settles the host.
    pub fn notice_taken(&self) {
        let mut shown = lock(&self.state);
        shown.owed = shown.owed.saturating_sub(1);
    }

    /// The loop put a job's notice aside instead of taking it into a turn (DIVERGENCES X-84): it no longer
    /// holds the chat busy, and — no turn being there to report — the state that settles the host is sent
    /// now.
    pub fn notice_held(&self) {
        self.update(|shown| shown.owed = shown.owed.saturating_sub(1));
    }

    /// Applies `change` to the inputs, then sends whichever of the two outputs moved: the host's state
    /// ([`Shown::effective`]) and the whole-turn layer ([`Shown::turn`]).
    fn update(&self, change: impl FnOnce(&mut Shown)) {
        let (host, turn) = {
            let mut shown = lock(&self.state);
            change(&mut shown);
            let turn = shown.turn();
            let turn = (shown.turn_sent != turn).then(|| {
                shown.turn_sent = turn;
                turn
            });
            let host = shown
                .effective()
                .filter(|s| shown.sent != Some(*s))
                .inspect(|s| shown.sent = Some(*s));
            (host, turn)
        };
        if let (Some(t), Some(ui)) = (turn, &self.turn_view) {
            ui.turn_busy(t);
        }
        if let Some(s) = host
            && let Some(r) = self.hosts.iter().find_map(|h| h.as_state_reporter())
        {
            r.set_state(s);
        }
    }

    /// Pings the first [`Notifier`], unless `notify` is off (host.go:128-138).
    // By value, not by reference: every call site builds its `Event` inline at a turn anchor
    // (`pres.notify(Event { kind, text })`), which a `&Event` parameter would turn into a
    // named temporary at each of the five. The clone clippy sees avoided is one `String` move
    // per turn, and `Notifier` itself takes `&Event`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn notify(&self, e: Event) {
        if !self.notify_on {
            return;
        }
        if let Some(n) = self.hosts.iter().find_map(|h| h.as_notifier()) {
            n.notify(&e);
        }
    }

    /// Tells the first [`SessionReporter`] which session the chat persists into.
    pub fn set_session(&self, id: &str, path: &Path) {
        if let Some(r) = self.hosts.iter().find_map(|h| h.as_session_reporter()) {
            r.report_session(id, path);
        }
    }

    /// What EVERY host tells the model, in host order — the one capability that is gathered rather
    /// than served by the first host: a fact one host knows is not made false by another host also
    /// knowing something. Empty when no host contributes.
    pub fn environment(&self) -> Vec<(String, String)> {
        self.hosts
            .iter()
            .filter_map(|h| h.as_environment())
            .flat_map(EnvironmentContributor::environment)
            .collect()
    }

    /// Runs every [`Closer`], last host first (host.go:142-148).
    pub async fn close(&self) {
        for h in self.hosts.iter().rev() {
            if let Some(c) = h.as_closer() {
                c.close().await;
            }
        }
    }

    /// The first host that KNOWS the background tone (background.go:42-51), asked in order; the
    /// cmux probe is an RPC child under a one-second deadline.
    pub async fn dark_background(&self) -> Option<bool> {
        for reporter in self.hosts.iter().filter_map(|h| h.as_background()) {
            if let Some(dark) = reporter.dark_background().await {
                return Some(dark);
            }
        }
        None
    }

    /// The detected hosts' names, in order.
    pub fn host_names(&self) -> Vec<&str> {
        self.hosts.iter().map(|h| h.name()).collect()
    }
}

/// The pre-loop background probe (background.go:30-37): the host probes (cmux) first, then
/// `fallback` — the terminal's own OSC 11 answer, supplied by the command as a future (it is a
/// blocking tty round-trip the command puts on a blocking thread).
pub async fn detect_background(
    probe: &Probe,
    fallback: impl std::future::Future<Output = bool>,
) -> bool {
    let query: background::CmuxQuery = std::sync::Arc::new(background::cmux_query_exec);
    background::detect_background_with(probe, &query, fallback).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Caps, Event, Kind, Presenter, Probe, State};
    use crate::app::env::Env;
    use crate::testing::RecordingHost;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A host advertising only `caps`, sharing `RecordingHost`'s logs.
    fn host(name: &'static str, caps: Caps) -> RecordingHost {
        RecordingHost {
            caps,
            ..RecordingHost::new(name)
        }
    }

    const STATE_ONLY: Caps = Caps {
        state: true,
        notify: false,
        background: false,
        close: false,
        session: false,
        environment: false,
    };
    const FULL: Caps = Caps {
        state: true,
        notify: true,
        background: false,
        close: false,
        session: false,
        environment: false,
    };

    // Go: internal/host/host_test.go:25 TestPresenterPerCapabilityFallback — the design's core
    // rule: resolution happens PER CAPABILITY, not per host. A host owning state does not steal
    // notifications; those fall through to the next host that implements them.
    #[test]
    fn test_presenter_per_capability_fallback() {
        let so = host("state-only", STATE_ONLY);
        let fb = host("full", FULL);
        let (so_log, fb_states, fb_events) = (
            Arc::clone(&so.states),
            Arc::clone(&fb.states),
            Arc::clone(&fb.events),
        );
        let p = Presenter::with_hosts(vec![Box::new(so), Box::new(fb)], true);

        p.set_state(State::Busy);
        p.notify(Event {
            kind: Kind::Done,
            text: "iota: done".to_owned(),
        });

        assert_eq!(*so_log.lock().unwrap(), vec![State::Busy]);
        assert!(
            fb_states.lock().unwrap().is_empty(),
            "the fallback got states although a host owns the capability"
        );
        let events = fb_events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "iota: done");
    }

    // Go: internal/host/host_test.go:45 TestPresenterDedupsStates — command dispatch re-asserts
    // Idle liberally and a host may pay per update (cmux spawns a process), so repeats are
    // dropped. Unlike Go, whose presenter started AT Idle, the first report always goes out: the
    // loop's own Idle after the banner is what lists the chat with its host (herdr) before the
    // first turn.
    #[test]
    fn test_presenter_dedups_states() {
        let fb = host("full", FULL);
        let states = Arc::clone(&fb.states);
        let p = Presenter::with_hosts(vec![Box::new(fb)], true);

        p.set_state(State::Idle); // the first report: sent
        p.set_state(State::Idle); // a repeat: dropped
        p.set_state(State::Busy);
        p.set_state(State::Busy);
        p.set_state(State::Idle);

        assert_eq!(
            *states.lock().unwrap(),
            vec![State::Idle, State::Busy, State::Idle]
        );
    }

    // Go: internal/host/host_test.go:61 TestPresenterNotifySwitch — config `notify: false`
    // silences every host; states are unaffected.
    #[test]
    fn test_presenter_notify_switch() {
        let fb = host("full", FULL);
        let (events, states) = (Arc::clone(&fb.events), Arc::clone(&fb.states));
        let p = Presenter::with_hosts(vec![Box::new(fb)], false);
        p.notify(Event {
            kind: Kind::Failed,
            text: "x".to_owned(),
        });
        p.set_state(State::Busy);
        assert!(
            events.lock().unwrap().is_empty(),
            "notify off but delivered"
        );
        assert_eq!(*states.lock().unwrap(), vec![State::Busy]);
    }

    // Go: internal/host/host_test.go:82 TestPresenterCloseReverseOrder — teardown unwinds
    // detection order.
    #[tokio::test]
    async fn test_presenter_close_reverse_order() {
        let caps = Caps {
            close: true,
            ..Caps::default()
        };
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
        let a = RecordingHost {
            caps,
            closed: Arc::clone(&order),
            ..RecordingHost::new("a")
        };
        let b = RecordingHost {
            caps,
            closed: Arc::clone(&order),
            ..RecordingHost::new("b")
        };
        let p = Presenter::with_hosts(vec![Box::new(a), Box::new(b)], true);
        p.close().await;
        assert_eq!(*order.lock().unwrap(), vec!["b", "a"]);
    }

    // Go: internal/host/host_test.go:93 TestNewPresenterDetects — a bare environment leaves the
    // fallback alone.
    #[test]
    fn test_new_presenter_detects_bare_env() {
        let none = Probe {
            env: Env::default(),
            look_path: Box::new(|_| None),
        };
        let p = Presenter::new(&none, Some(Box::new(host("full", FULL))), true);
        assert_eq!(p.host_names(), vec!["full"]);
        assert_eq!(
            p.detected_host(),
            None,
            "the fallback is not a detected host"
        );
    }

    // Go: internal/host/host_test.go:93 TestNewPresenterDetects (cmux half) — the registry
    // contributes detected hosts AHEAD of the fallback, and `close` flushes them. A `#[tokio::test]`
    // because `detect_cmux` spawns the host's worker task (T3 design §5.1 R15).
    #[tokio::test]
    async fn test_new_presenter_detects_cmux() {
        let cmux_env = Probe {
            env: Env::fixed(&[("CMUX_SURFACE_ID", "surface-1")]),
            look_path: Box::new(|_| Some(PathBuf::from("/usr/bin/true"))),
        };
        let p = Presenter::new(&cmux_env, Some(Box::new(host("full", FULL))), true);
        assert_eq!(p.host_names(), vec!["cmux", "full"]);
        assert_eq!(p.detected_host(), Some("cmux"));
        p.close().await; // flushes the worker through /usr/bin/true
    }

    /// The innermost host is exclusive: a probe carrying both `HERDR_*` and `CMUX_SURFACE_ID` (a
    /// herdr pane inside a cmux window) yields herdr and the fallback alone — cmux is not detected,
    /// its `PATH` lookup never runs, and so no `cmux` command ever does.
    #[tokio::test]
    async fn test_new_presenter_keeps_the_innermost_host_only() {
        let cmux_asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asked = Arc::clone(&cmux_asked);
        let both = Probe {
            env: Env::fixed(&[
                ("HERDR_ENV", "1"),
                ("HERDR_PANE_ID", "w1:p2"),
                ("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock"),
                ("HERDR_BIN_PATH", "/nonexistent/herdr"),
                ("CMUX_SURFACE_ID", "surface-1"),
            ]),
            look_path: Box::new(move |_| {
                asked.store(true, std::sync::atomic::Ordering::SeqCst);
                Some(PathBuf::from("/usr/bin/true"))
            }),
        };
        let p = Presenter::new(&both, Some(Box::new(host("full", FULL))), true);
        assert_eq!(p.host_names(), vec!["herdr", "full"]);
        assert_eq!(p.detected_host(), Some("herdr"));
        assert!(
            !cmux_asked.load(std::sync::atomic::Ordering::SeqCst),
            "cmux was looked up although herdr was already detected"
        );
        assert_eq!(
            p.environment(),
            vec![
                ("host".to_owned(), "herdr".to_owned()),
                ("herdr pane".to_owned(), "w1:p2".to_owned()),
                (
                    "herdr note".to_owned(),
                    super::herdr::HERDR_CHILD_NOTE.to_owned()
                ),
            ]
        );
        p.close().await; // the release goes to a socket nobody listens on
    }

    // Go: internal/host/background_test.go:48 TestPresenterDarkBackground — the first host that
    // KNOWS wins; hosts without the capability, or without an answer, fall through.
    #[tokio::test]
    async fn test_presenter_dark_background() {
        let bg = |dark: Option<bool>| RecordingHost {
            dark,
            caps: Caps {
                background: true,
                ..Caps::default()
            },
            ..RecordingHost::new("bg")
        };
        let inert = host("inert", Caps::default());
        let p = Presenter::with_hosts(
            vec![
                Box::new(inert),
                Box::new(bg(None)),
                Box::new(bg(Some(true))),
            ],
            true,
        );
        assert_eq!(p.dark_background().await, Some(true));

        let p = Presenter::with_hosts(vec![Box::new(host("inert", Caps::default()))], true);
        assert_eq!(
            p.dark_background().await,
            None,
            "an inert-only presenter must not know its background"
        );
    }

    /// The environment is GATHERED, not served by the first host: two contributing hosts both land,
    /// in host order, and a host without the capability adds nothing between them.
    #[test]
    fn the_environment_gathers_every_contributing_host_in_order() {
        let pairs = |kv: &[(&str, &str)]| -> Vec<(String, String)> {
            kv.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let contributing = |name: &'static str, kv: &[(&str, &str)]| RecordingHost {
            env: pairs(kv),
            caps: Caps {
                environment: true,
                ..Caps::default()
            },
            ..RecordingHost::new(name)
        };
        let p = Presenter::with_hosts(
            vec![
                Box::new(contributing(
                    "first",
                    &[("host", "first"), ("first pane", "p1")],
                )),
                Box::new(host("inert", Caps::default())),
                Box::new(contributing("second", &[("host", "second")])),
            ],
            true,
        );
        assert_eq!(
            p.environment(),
            pairs(&[("host", "first"), ("first pane", "p1"), ("host", "second")])
        );

        let p = Presenter::with_hosts(vec![Box::new(host("inert", Caps::default()))], true);
        assert!(p.environment().is_empty(), "an inert host contributed");
    }

    /// A host that withholds `names` from children, and nothing else.
    struct Withholding(&'static str, &'static [&'static str]);

    impl super::Host for Withholding {
        fn name(&self) -> &str {
            self.0
        }
        fn as_child_env(&self) -> Option<&dyn super::ChildEnvironment> {
            Some(self)
        }
    }

    impl super::ChildEnvironment for Withholding {
        fn withheld(&self) -> Vec<String> {
            self.1.iter().map(|n| (*n).to_owned()).collect()
        }
    }

    /// Like the environment, what children are spawned without is GATHERED: every withholding host's
    /// names, in host order, each once; a host without the capability adds nothing, and none means none.
    #[test]
    fn the_child_env_gathers_every_withholding_host_once() {
        let first = Withholding("first", &["A_ID", "SHARED"]);
        let inert = host("inert", Caps::default());
        let second = Withholding("second", &["SHARED", "B_ID"]);
        let hosts: [&dyn super::Host; 3] = [&first, &inert, &second];
        assert_eq!(super::child_env(hosts).names(), ["A_ID", "SHARED", "B_ID"]);
        let inert: &dyn super::Host = &host("inert", Caps::default());
        assert!(super::child_env([inert]).names().is_empty());
        assert!(super::child_env(None).names().is_empty());
        assert_eq!(super::ChildEnv::default(), super::child_env(None));
    }

    /// `apply` takes a variable this process HAS out of a real child's environment, and a value the
    /// caller sets after it — an MCP entry's own `env:` — is the caller's and reaches the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn apply_withholds_from_a_real_child_unless_the_caller_sets_it() {
        assert!(std::env::var_os("HOME").is_some(), "the test needs a HOME");
        let echo = |set: Option<&str>| {
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.args(["-c", "printf %s \"${HOME-withheld}\""]);
            let hosts: [&dyn super::Host; 1] = [&Withholding("w", &["HOME"])];
            super::child_env(hosts).apply(&mut cmd);
            if let Some(v) = set {
                cmd.env("HOME", v);
            }
            cmd
        };
        let out = echo(None).output().await.expect("run sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "withheld");
        let out = echo(Some("/set/by/config")).output().await.expect("run sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/set/by/config");
    }

    /// The session goes to the FIRST reporter, like a state; a host without the capability is
    /// skipped, and a presenter with none swallows the call.
    #[test]
    fn the_session_reaches_the_first_reporter() {
        let reporter = |name: &'static str| RecordingHost {
            caps: Caps {
                session: true,
                ..Caps::default()
            },
            ..RecordingHost::new(name)
        };
        let (a, b) = (reporter("a"), reporter("b"));
        let (a_log, b_log) = (Arc::clone(&a.sessions), Arc::clone(&b.sessions));
        let p = Presenter::with_hosts(
            vec![
                Box::new(host("inert", Caps::default())),
                Box::new(a),
                Box::new(b),
            ],
            true,
        );
        p.set_session("s-1", Path::new("/tmp/s-1"));
        assert_eq!(
            *a_log.lock().unwrap(),
            vec![("s-1".to_owned(), PathBuf::from("/tmp/s-1"))]
        );
        assert!(
            b_log.lock().unwrap().is_empty(),
            "the second host got it too"
        );

        Presenter::with_hosts(Vec::new(), true).set_session("s-2", Path::new("/tmp/s-2"));
    }

    /// A presenter recording states on one host, and the log.
    fn recording() -> (Presenter, Arc<Mutex<Vec<State>>>) {
        let fb = host("full", FULL);
        let states = Arc::clone(&fb.states);
        (Presenter::with_hosts(vec![Box::new(fb)], true), states)
    }

    /// An idle chat with a job running is shown as Busy; the jobs ending keep it Busy until their
    /// notices are taken, and the turn those notices run is what shows Idle again.
    #[test]
    fn an_idle_chat_with_a_job_running_is_busy() {
        let (p, states) = recording();
        p.set_state(State::Idle);
        p.set_jobs(1);
        p.set_jobs(2); // still busy: no repeat
        p.job_ended(1);
        p.job_ended(0); // both ended; their notices are on the way
        p.set_state(State::Idle); // the loop wakes for the first notice
        p.notice_taken();
        p.set_state(State::Busy); // its turn
        p.notice_taken(); // the second, drained mid-turn
        p.set_state(State::Idle);
        assert_eq!(
            *states.lock().unwrap(),
            vec![State::Idle, State::Busy, State::Idle]
        );
    }

    /// A notice taken while a notice is still owed keeps the chat Busy; a notice nobody counted
    /// (a job that ended before the loop watched) takes nothing below zero.
    #[test]
    fn notices_are_counted_one_by_one() {
        let (p, states) = recording();
        p.notice_taken(); // nothing owed: stays at zero
        p.set_state(State::Idle);
        p.set_jobs(2);
        p.job_ended(1);
        p.job_ended(0);
        p.notice_taken();
        p.set_state(State::Idle);
        p.notice_taken();
        p.set_state(State::Idle);
        assert_eq!(
            *states.lock().unwrap(),
            vec![State::Idle, State::Busy, State::Idle]
        );
    }

    /// Every end is its own fact: a job ending while another starts leaves the count where it was,
    /// yet both notices are owed, and the chat stays Busy until the second is taken. A job that
    /// leaves the set without ending (it never started) owes nothing.
    #[test]
    fn an_end_masked_by_a_start_is_still_owed() {
        let (p, states) = recording();
        p.set_state(State::Idle);
        p.set_jobs(1); // b1
        p.job_ended(0); // b1 ends…
        p.set_jobs(1); // …as b2 is adopted
        p.job_ended(0); // b2 ends
        p.set_state(State::Idle);
        p.notice_taken(); // b1's
        p.set_state(State::Idle); // b2's is still owed: no Idle
        p.notice_taken(); // b2's
        p.set_state(State::Idle);
        p.set_jobs(1);
        p.set_jobs(0); // a claimed slot whose child never started
        p.set_state(State::Idle); // nothing owed: Idle, where a count's drop would have owed one
        assert_eq!(
            *states.lock().unwrap(),
            vec![
                State::Idle,
                State::Busy,
                State::Idle,
                State::Busy,
                State::Idle
            ]
        );
    }

    /// X-84: a notice the loop put aside instead of taking into a turn settles the host by itself — no turn
    /// follows to report. The state a failed turn left (`Error`) stays; an idle chat owed nothing goes `Idle`.
    #[test]
    fn a_held_notice_settles_the_host_without_a_turn() {
        let (p, states) = recording();
        p.set_state(State::Idle);
        p.set_jobs(1); // a job runs: Busy
        p.job_ended(0); // its notice is owed: still Busy
        p.notice_held(); // put aside: Idle, at once
        p.set_state(State::Error); // a 429 turn
        p.job_ended(0);
        p.notice_held(); // put aside while the error stands: nothing to send
        assert_eq!(
            *states.lock().unwrap(),
            vec![State::Idle, State::Busy, State::Idle, State::Error]
        );
    }

    /// `NeedsInput` and `Error` are not rewritten by a running job, and leaving them for `Idle` with the
    /// job still running shows Busy.
    #[test]
    fn needs_input_and_error_pass_through_while_a_job_runs() {
        let (p, states) = recording();
        p.set_jobs(1);
        p.set_state(State::NeedsInput);
        p.set_state(State::Error);
        p.set_state(State::Idle);
        assert_eq!(
            *states.lock().unwrap(),
            vec![State::NeedsInput, State::Error, State::Busy]
        );
    }

    /// Jobs coming and going inside a turn send nothing: the turn is Busy either way.
    #[test]
    fn jobs_during_a_turn_send_no_repeats() {
        let (p, states) = recording();
        p.set_state(State::Busy);
        p.set_jobs(1);
        p.job_ended(0);
        p.notice_taken();
        p.set_jobs(3);
        assert_eq!(*states.lock().unwrap(), vec![State::Busy]);
    }

    /// Jobs recorded before the first state only record; the first Idle then goes out as Busy,
    /// once.
    #[test]
    fn jobs_before_the_first_state_make_the_first_idle_busy() {
        let (p, states) = recording();
        p.set_jobs(2);
        assert!(states.lock().unwrap().is_empty(), "sent before any state");
        p.set_state(State::Idle);
        p.set_state(State::Idle);
        assert_eq!(*states.lock().unwrap(), vec![State::Busy]);
    }

    /// A host list with no state reporter at all swallows the call (Go: the loop simply ends).
    #[test]
    fn a_presenter_without_a_state_reporter_still_dedups() {
        let p = Presenter::with_hosts(Vec::new(), true);
        p.set_state(State::Busy);
        p.notify(Event {
            kind: Kind::Done,
            text: "nobody".to_owned(),
        });
        assert!(p.host_names().is_empty());
    }

    /// The status row's whole-turn layer comes out of the same update as the host's state (busy-span
    /// point 8): `Busy` runs it, `NeedsInput` pauses it, `Idle` and `Error` end it — and a job still
    /// running after the turn holds the HOST at `Busy` but not the layer (point 5, B).
    #[test]
    fn the_turn_layer_is_the_turn_state_before_the_jobs_rewrite() {
        use crate::testing::{ScriptedUi, UiEvent};
        use crate::ui::facade::{TurnBusy, Ui};
        let ui = ScriptedUi::new(Vec::new());
        let fb = host("full", FULL);
        let states = Arc::clone(&fb.states);
        let p = Presenter::with_hosts(vec![Box::new(fb)], true)
            .show_turn_on(Arc::clone(&ui) as Arc<dyn Ui>);
        p.set_state(State::Idle);
        p.set_state(State::Busy); // the loop accepts a message
        p.set_jobs(1); // a call yields: a job runs
        p.set_state(State::NeedsInput); // an approval
        p.set_state(State::Busy);
        p.set_state(State::Idle); // the turn lands; the job still runs
        p.job_ended(0);
        p.set_state(State::Idle); // the loop wakes for its notice
        p.notice_taken();
        p.set_state(State::Busy); // the notice's turn
        p.set_state(State::Error);
        let turn: Vec<TurnBusy> = ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Turn(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(
            turn,
            vec![
                TurnBusy::Running,
                TurnBusy::Waiting,
                TurnBusy::Running,
                TurnBusy::Off, // the job keeps running: the layer is down all the same
                TurnBusy::Running,
                TurnBusy::Off,
            ]
        );
        assert_eq!(
            *states.lock().unwrap(),
            vec![
                State::Idle,
                State::Busy,
                State::NeedsInput,
                State::Busy, // …while the host stays Busy through the landed turn and the owed notice
                State::Error,
            ]
        );
    }
}
