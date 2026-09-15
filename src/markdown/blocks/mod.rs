//! The five buffering block types of the streaming renderer — fenced code, table, list,
//! quote and display math — one file each (Phase 5 PR-19). Today each file holds the
//! block's renderer; the parsing that feeds it still lives in `markdown::Writer` and moves
//! here block by block.

pub(crate) mod code;
pub(crate) mod list;
pub(crate) mod math;
pub(crate) mod quote;
pub(crate) mod table;
