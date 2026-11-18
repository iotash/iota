//! The loop's runtime (Phase 5 PR-14): the handle the facade is served through, its mailbox,
//! the event loop on the `"iota-tui"` thread, the terminal writer, the wait on the terminal's
//! descriptor, the OSC probes, the background asked again while the loop runs, the one-shot
//! pre-REPL surface, and where a selection's text is copied to.

pub(crate) mod clipboard;
pub(crate) mod event_loop;
pub(crate) mod handle;
pub(crate) mod msgs;
pub(crate) mod oneshot;
pub(crate) mod osc;
pub(crate) mod requery;
pub(crate) mod screen;
pub(crate) mod term;
#[cfg(unix)]
pub(crate) mod tty;
