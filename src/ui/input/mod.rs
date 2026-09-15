//! What the user types into (Phase 5 PR-14): the composer, the key ladder that routes into it,
//! bracketed-paste handling and in-composer completion.

pub(crate) mod composer;
pub(crate) mod keys;
pub(crate) mod paste;
pub(crate) mod suggest;

// Transitional aliases while the import points move (next commit): the files here still name
// their former flat siblings under `ui/` as `super::<name>`.
#[allow(unused_imports)]
pub(crate) use crate::ui::{event_loop, theme};
