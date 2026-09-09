//! Integration tests of the on-disk session bundle store (chat/session.go) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod golden;
mod loader;
mod meta;
mod rawcodec;
mod record;
mod store;
mod tuning;
mod writer;
