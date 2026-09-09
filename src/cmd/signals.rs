//! SIGINT/SIGTERM → `CancellationToken` (DIVERGENCES I-03): the run is cancelled, MCP servers are closed, and the
//! process exits 130 (text mode prints nothing; JSON mode prints the report with `"error": "interrupted"`).
//!
//! Interactive runs disarm the SIGINT half (`TUI_DESIGN` §8.4 step 7): raw mode owns Ctrl+C — the terminal
//! delivers it as a key event that the composer's cancel ladder answers — so a process-level handler would race
//! the loop. SIGTERM keeps cancelling the root token, which fails every facade waiter and lets the interrupt
//! table persist what the turn produced.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

/// Whether SIGINT must be swallowed rather than cancel the run. One-way and process-wide: a process hosts at
/// most one interactive run, and the terminal it takes over is never handed back to a headless path.
static SIGINT_IGNORED: AtomicBool = AtomicBool::new(false);

/// `tokio::signal::ctrl_c` + unix SIGTERM → `cancel()`. Spawned once.
///
/// Spawns the listener task onto the current runtime, so it must be called from inside `block_on` (main.rs does).
pub fn install(cancel: CancellationToken) {
    tokio::spawn(async move {
        wait_for_signal().await;
        cancel.cancel();
    });
}

/// Disarms the SIGINT half of the listener installed by [`install`] — the interactive branch calls it before it
/// takes the terminal, and from then on only SIGTERM cancels the run (`TUI_DESIGN` §8.4 step 7).
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

/// Resolves when a signal that must cancel the run arrives: SIGTERM (unix), or SIGINT while it is still armed.
/// A SIGTERM listener that cannot be registered is skipped rather than fatal; a failed Ctrl-C registration
/// simply never fires.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        if !sigint_ignored() {
                            return;
                        }
                    }
                    _ = term.recv() => return,
                }
            },
            Err(_) => wait_for_ctrl_c().await,
        }
    }
    #[cfg(not(unix))]
    {
        wait_for_ctrl_c().await;
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
