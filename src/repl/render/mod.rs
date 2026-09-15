//! What the interactive loop draws (chat/transcript.go and friends): the transcript and its tool-call
//! groups, the markdown sink, the styles, the diff and banner renderers, the resume replay and the MCP
//! startup report — everything that turns loop state into lines on the facade.

pub(crate) mod banner;
pub(crate) mod diff;
pub(crate) mod group;
pub(crate) mod mcpreport;
pub(crate) mod replay;
pub(crate) mod styles;
pub(crate) mod transcript;
pub(crate) mod uisink;
