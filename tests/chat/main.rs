//! Integration tests of the headless run loop (chat/chat.go and friends) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod approval;
mod jobs;
mod output;
mod parallel;
mod toolloop;
mod turns;
