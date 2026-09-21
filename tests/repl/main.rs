//! Integration tests of the interactive chat loop over the scripted `Ui` facade (chat/run.go and friends) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixture this binary uses (`tests/common/`): the stand-in herdr the host anchors are
// asserted against. Mounted at the root because `#[path]` inside an inline module resolves through
// a directory that does not exist.
#[path = "../common/herdr_mock.rs"]
mod common_herdr;

mod common {
    #[cfg(unix)]
    pub(crate) use crate::common_herdr::HerdrMock;
}

mod artifact;
mod commands;
mod compact;
mod debug;
mod edit;
mod export;
mod file;
mod host;
mod images;
// Spawns real background children through `Jobs`; its command lines are POSIX, so each test that runs one
// asks `jobs::skip_unless_posix` first (see `tests/tool/main.rs`).
mod jobs;
mod settings;
mod skills;
mod tokens;
mod toolfmt;
