//! The loop's runtime (Phase 5 PR-14): the handle the facade is served through, its mailbox,
//! the event loop on the `"iota-tui"` thread, the terminal writer, the OSC probes and the
//! one-shot pre-REPL surface, and where a selection's text is copied to.

pub(crate) mod clipboard;
pub(crate) mod event_loop;
pub(crate) mod handle;
pub(crate) mod msgs;
pub(crate) mod oneshot;
pub(crate) mod osc;
pub(crate) mod screen;
pub(crate) mod term;
