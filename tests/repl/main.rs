//! Integration tests of the interactive chat loop over the scripted `Ui` facade (chat/run.go and friends) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod artifact;
mod commands;
mod compact;
mod compose;
mod debug;
mod diff;
mod edit;
mod errors;
mod export;
mod file;
mod host;
mod images;
mod interrupt;
// Spawns real background `bash` children through `Jobs` — Unix-only, like the toolset that starts them.
#[cfg(unix)]
mod jobs;
mod settings;
mod skills;
mod tokens;
mod toolfmt;
mod turn;
