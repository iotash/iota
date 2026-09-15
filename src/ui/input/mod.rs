//! What the user types into (Phase 5 PR-14): the composer, the key ladder that routes into it,
//! bracketed-paste handling, in-composer completion — and the editing core the composer shares
//! with the surface's input fields (`editor`, PR-15).

pub(crate) mod composer;
pub(crate) mod editor;
pub(crate) mod keys;
pub(crate) mod paste;
pub(crate) mod suggest;
