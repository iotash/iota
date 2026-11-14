//! Integration tests of the host layer over a stand-in herdr (`tests/common/herdr_mock.rs`), and of the
//! herdr-core host inside a real, isolated hc core (`hc.rs`) — one
//! binary per area (docs/MERGE-PLAN.md §2). Unix only, like the herdr transport: on any other
//! platform this binary is empty.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixture this binary uses (`tests/common/`), mounted at the root because `#[path]`
// inside an inline module resolves through a directory that does not exist.
#[path = "../common/herdr_mock.rs"]
mod common_herdr;

mod common {
    pub(crate) use crate::common_herdr::HerdrMock;
}

// The L4 SSE mock provider (`tests/ui_tmux/mock.rs`), which the herdr-core run drives by message.
#[path = "../ui_tmux/mock.rs"]
mod mock;

mod hc;
mod herdr;
