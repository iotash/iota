//! Integration tests of the on-disk session bundle store (chat/session.go) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixtures this binary uses (`tests/common/`), and no others.
#[path = "../common/session.rs"]
mod common;

mod golden;
mod loader;
mod meta;
mod rawcodec;
mod record;
mod roundtrip;
mod store;
mod tuning;
mod writer;
