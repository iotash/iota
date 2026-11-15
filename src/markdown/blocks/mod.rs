//! The five buffering block types of the streaming renderer — fenced code, table, list,
//! quote and display math — one file each (Phase 5 PR-19): the block's line recognizers,
//! its buffering state (the `*Block` struct the Writer's `enum Block` wraps) and its
//! renderer, together.

pub(crate) mod code;
pub(crate) mod list;
pub(crate) mod math;
pub(crate) mod quote;
pub(crate) mod table;
