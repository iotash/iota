//! Integration tests of the tool framework, the built-in toolsets and the AGENTS.md overlay (tool/*, internal/agents, internal/shell) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod agents;
mod code;
mod delegate_tool;
mod framework;
mod shell;
