//! SIGINT/SIGTERM/SIGHUP → `CancellationToken` (DIVERGENCES I-03): the run is cancelled, MCP servers are closed,
//! and the process exits 130 (text mode prints nothing; JSON mode prints the report with `"error": "interrupted"`).
//!
//! Interactive runs disarm the SIGINT half (`TUI_DESIGN` §8.4 step 7): raw mode owns Ctrl+C — the terminal
//! delivers it as a key event that the composer's cancel ladder answers — so a process-level handler would race
//! the loop. SIGTERM keeps cancelling the root token, which fails every facade waiter and lets the interrupt
//! table persist what the turn produced. SIGHUP takes the same path (docs/design/bot-mode.md §2.7): closing the
//! pane or terminal a run lives in would otherwise kill it outright, and the rounds it had finished would never
//! reach the log.
//!
//! The listener outlives the first signal. Once tokio has installed its handlers a signal nobody listens for is
//! simply swallowed, so a run whose wind-down stalled could not be stopped by anything short of SIGKILL
//! (2026-10-02: orphans of `tmux kill-server`, SIGTERM answered with nothing). A SECOND SIGTERM — or SIGINT,
//! while armed — is the user insisting: the terminal is restored and the process exits 130 at once. What it
//! skips is the wind-down of the turn in flight, not what is already saved: the session writer syncs every
//! batch it appends. A second SIGHUP is not insisting — one hangup can arrive more than once (the pty closing,
//! then the pane's shell passing it on) — so it never forces.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

/// Whether SIGINT must be swallowed rather than cancel the run. One-way and process-wide: a process hosts at
/// most one interactive run, and the terminal it takes over is never handed back to a headless path.
static SIGINT_IGNORED: AtomicBool = AtomicBool::new(false);

/// `tokio::signal::ctrl_c` + unix SIGTERM and SIGHUP → `cancel()`; a second SIGTERM or armed SIGINT → exit 130.
/// Spawned once.
///
/// Spawns the listener task onto the current runtime, so it must be called from inside `block_on` (main.rs does).
pub fn install(cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut signals = Signals::new();
        signals.next().await;
        cancel.cancel();
        while signals.next().await == Got::Hangup {}
        crate::ui::restore_terminal();
        std::process::exit(130);
    });
}

/// Disarms the SIGINT half of the listener installed by [`install`] — the interactive branch calls it before it
/// takes the terminal, and from then on only SIGTERM and SIGHUP cancel the run (`TUI_DESIGN` §8.4 step 7).
///
/// The already-spawned listener cannot be un-spawned, so it consults this flag instead: a SIGINT that arrives
/// while it is set is dropped and the listener re-arms. There is no way back — nothing re-enables it.
pub(crate) fn ignore_sigint() {
    SIGINT_IGNORED.store(true, Ordering::Relaxed);
}

/// Whether [`ignore_sigint`] has disarmed the SIGINT half.
fn sigint_ignored() -> bool {
    SIGINT_IGNORED.load(Ordering::Relaxed)
}

/// Which kind of signal [`Signals::next`] saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Got {
    /// SIGHUP: the terminal went away.
    Hangup,
    /// SIGTERM, or SIGINT while armed: somebody asked the run to stop.
    Stop,
}

/// The process's signal streams, registered once and kept for the whole run, so a signal that arrives between
/// two [`Signals::next`] calls is still delivered to the second.
struct Signals {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hup: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    /// Registers the SIGTERM and SIGHUP streams; one that cannot be registered is skipped rather than fatal.
    fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                term: signal(SignalKind::terminate()).ok(),
                hup: signal(SignalKind::hangup()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Resolves when a signal that must stop the run arrives: SIGTERM or SIGHUP (unix), or SIGINT while it is
    /// still armed. A failed Ctrl-C registration simply never fires.
    async fn next(&mut self) -> Got {
        #[cfg(unix)]
        {
            if self.term.is_none() && self.hup.is_none() {
                wait_for_ctrl_c().await;
                return Got::Stop;
            }
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        if !sigint_ignored() {
                            return Got::Stop;
                        }
                    }
                    () = recv(self.term.as_mut()) => return Got::Stop,
                    () = recv(self.hup.as_mut()) => return Got::Hangup,
                }
            }
        }
        #[cfg(not(unix))]
        {
            wait_for_ctrl_c().await;
            Got::Stop
        }
    }
}

/// Resolves when `sig` delivers; never, for a listener that was not registered or whose stream has closed (a
/// closed stream would otherwise resolve on every poll).
#[cfg(unix)]
async fn recv(sig: Option<&mut tokio::signal::unix::Signal>) {
    match sig {
        Some(sig) => {
            if sig.recv().await.is_none() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// The Ctrl-C-only fallback (no SIGTERM listener, or a non-unix host): re-arms while SIGINT is disarmed, so it
/// never resolves once the interactive branch owns the terminal.
async fn wait_for_ctrl_c() {
    loop {
        if tokio::signal::ctrl_c().await.is_err() {
            // The handler could not be registered; nothing will ever fire, so park forever rather than spin.
            std::future::pending::<()>().await;
        }
        if !sigint_ignored() {
            return;
        }
    }
}
