//! Integration tests of the tool framework, the built-in toolsets and the AGENTS.md overlay (tool/*, internal/agents, internal/shell) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod agents;
mod code;
mod framework;
// Both drive REAL `bash` children through the shell toolset, and Windows has neither yet
// (`src/tool/shell.rs::new_shell_set` builds no tool there) — so they are Unix-only by construction,
// not by accident. `framework.rs` keeps the Windows half of that contract as its own test.
#[cfg(unix)]
mod jobs;
#[cfg(unix)]
mod shell;
