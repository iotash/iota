//! Integration tests of the command: CLI, config, run resolution and the checked-in Go-written session bundles (cmd/, config/) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixtures this binary uses (`tests/common/`), and no others: a mounted file that
// nothing here calls would be dead code. Mounted at the root because `#[path]` inside an inline
// module resolves through a directory that does not exist.
#[path = "../common/child.rs"]
mod common_child;
#[path = "../common/herdr_mock.rs"]
mod common_herdr;
#[path = "../common/oauth_mock.rs"]
mod common_oauth;
#[path = "../common/project.rs"]
mod common_project;
#[path = "../common/transcript.rs"]
mod common_transcript;

mod common {
    pub(crate) use crate::common_child::cleared_env;
    #[cfg(unix)]
    pub(crate) use crate::common_herdr::HerdrMock;
    pub(crate) use crate::common_project::temp_project;
    pub(crate) use crate::common_transcript as transcript;
}

mod cli;
mod config;
mod interactive_cli;
mod mcp;
mod resolve;
mod session;
