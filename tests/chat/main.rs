//! Integration tests of the headless run loop (chat/chat.go and friends) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod approval;
// A headless run over the REAL shell toolset. Its command lines are POSIX, so each test asks
// `jobs::skip_unless_posix` first (see `tests/tool/main.rs`).
mod jobs;
mod output;
mod parallel;
mod toolloop;
mod turns;
