//! What the loop draws with (Phase 5 PR-14): the staging region and its snapshot, the frame
//! builder, the ANSI→span parser (the `NO_COLOR` gate), the theme, the streaming sink with its
//! metered previews, and the region debugger.

pub(crate) mod debug;
pub(crate) mod frame;
pub(crate) mod region;
pub(crate) mod sink;
pub(crate) mod spans;
pub(crate) mod theme;
