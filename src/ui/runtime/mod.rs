//! The loop's runtime (Phase 5 PR-14): the handle the facade is served through, its mailbox,
//! the event loop on the `"iota-tui"` thread, the terminal writer, the OSC probes and the
//! one-shot pre-REPL surface.

pub(crate) mod event_loop;
pub(crate) mod handle;
pub(crate) mod inline_term;
pub(crate) mod msgs;
pub(crate) mod oneshot;
pub(crate) mod osc;
pub(crate) mod term;
