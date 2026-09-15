//! The five buffering block types of the streaming renderer — fenced code, table, list,
//! quote and display math — one file each (Phase 5 PR-19): the block's line recognizers,
//! its buffering state (the `*Block` struct the Writer's `enum Block` wraps) and its
//! renderer, together.

pub(crate) mod code;
pub(crate) mod list;
pub(crate) mod math;
pub(crate) mod quote;
pub(crate) mod table;

use crate::markdown::PreviewHandle;

/// Closes a block's live preview, if it opened one — always BEFORE the rendered block is
/// written, so the preview row is released to the block that replaces it.
pub(crate) fn close_view(view: &mut Option<Box<dyn PreviewHandle>>) {
    if let Some(mut v) = view.take() {
        v.close();
    }
}
