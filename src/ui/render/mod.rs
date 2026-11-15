//! What the loop draws with (Phase 5 PR-14): the staging region and its snapshot, the frame
//! builder, the ANSI→span parser (the `NO_COLOR` gate), the theme, the streaming sink with its
//! metered previews, the region debugger, and the in-app selection.

pub(crate) mod debug;
pub(crate) mod frame;
pub(crate) mod region;
pub(crate) mod selection;
pub(crate) mod sink;
pub(crate) mod spans;
pub(crate) mod theme;
