//! Integration tests of the command: CLI, config, run resolution, delegation wiring and the checked-in Go-written session bundles (cmd/, config/) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod cli;
mod config;
mod delegate;
mod interactive_cli;
mod resolve;
mod session;
