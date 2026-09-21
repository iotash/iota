//! Integration tests of the host layer over a stand-in herdr (`tests/common/herdr_mock.rs`) — one
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

mod herdr;
