//! The loop's runtime (Phase 5 PR-14): the handle the facade is served through, its mailbox,
//! the event loop on the `"iota-tui"` thread, the terminal writer, the OSC probes and the
//! one-shot pre-REPL surface.

pub(crate) mod event_loop;
pub(crate) mod handle;
pub(crate) mod msgs;
pub(crate) mod oneshot;
pub(crate) mod osc;
pub(crate) mod term;

// Transitional aliases while the import points move (next commit): the files here still name
// their former flat siblings under `ui/` as `super::<name>`.
#[allow(unused_imports)]
pub(crate) use crate::ui::{
    Tui, TuiOptions, composer, facade, frame, keys, paste, region, sink, spans, suggest, surface,
    theme,
};
