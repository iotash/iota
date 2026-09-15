//! Host integration (internal/host): the terminal or terminal multiplexer the chat runs inside is
//! told what the conversation is doing — a state for its progress indicator, an attention ping when
//! the user is needed, a clean-up on exit — through per-capability traits a [`Host`] may or may
//! not implement. The [`Presenter`] fans one call out to the FIRST host that has the capability
//! (host.go:97-148), deduplicates states, and gates pings on the `notify` config key.
//!
//! Hosts: the cmux multiplexer (`cmux::CmuxHost`, detected from the environment) and the plain
//! ANSI terminal ([`ansi::AnsiHost`], the fallback the command always appends).

pub mod ansi;
pub(crate) mod background;
pub(crate) mod cmux;

pub use ansi::AnsiHost;

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use crate::BoxFuture;
use crate::app::env::Env;

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

/// The detectors, in priority order (host.go:84).
pub(crate) const DETECTORS: &[Detector] = &[cmux::detect_cmux];

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
}

/// Fans the conversation's signals out to the detected hosts (host.go:90-148): the FIRST host
/// with a capability serves it; states are deduplicated; pings are gated by the `notify` key.
pub struct Presenter {
    hosts: Vec<Box<dyn Host>>,
    notify_on: bool,
    state: Mutex<State>,
}

impl Presenter {
    /// Runs the detectors, then appends `fallback` (host.go:97-108). MUST be called inside the
    /// tokio runtime: the cmux detector spawns its worker task.
    pub fn new(env: &Probe, fallback: Option<Box<dyn Host>>, notify: bool) -> Self {
        let mut hosts: Vec<Box<dyn Host>> = DETECTORS.iter().filter_map(|d| d(env)).collect();
        if let Some(f) = fallback {
            hosts.push(f);
        }
        Self::with_hosts(hosts, notify)
    }

    /// A presenter over an explicit host list (tests; the L3 fixtures use an empty one).
    pub fn with_hosts(hosts: Vec<Box<dyn Host>>, notify: bool) -> Self {
        Self {
            hosts,
            notify_on: notify,
            state: Mutex::new(State::Idle),
        }
    }

    /// Shows `s` on the first [`StateReporter`]; a repeated state is not re-sent
    /// (host.go:113-124).
    pub fn set_state(&self, s: State) {
        {
            let mut cur = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if *cur == s {
                return;
            }
            *cur = s;
        }
        if let Some(r) = self.hosts.iter().find_map(|h| h.as_state_reporter()) {
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
    use std::path::PathBuf;
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
    };
    const FULL: Caps = Caps {
        state: true,
        notify: true,
        background: false,
        close: false,
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
    // dropped; the initial state is Idle, which makes a leading `set_state(Idle)` a no-op.
    #[test]
    fn test_presenter_dedups_states() {
        let fb = host("full", FULL);
        let states = Arc::clone(&fb.states);
        let p = Presenter::with_hosts(vec![Box::new(fb)], true);

        p.set_state(State::Idle); // initial state: a no-op
        p.set_state(State::Busy);
        p.set_state(State::Busy);
        p.set_state(State::Idle);

        assert_eq!(*states.lock().unwrap(), vec![State::Busy, State::Idle]);
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
        p.close().await; // flushes the worker through /usr/bin/true
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
}
