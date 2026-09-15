//! Integration tests of the tool framework, the built-in toolsets and the AGENTS.md overlay (tool/*, internal/agents, internal/shell) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixtures this binary uses (`tests/common/`), and no others.
#[path = "../common/project.rs"]
mod common;

mod agents;
mod code;
mod framework;
// Both drive REAL children through the shell toolset, on every platform: the toolset now resolves an
// interpreter on Windows too (`src/shell/interp.rs`). The command lines are POSIX, so the tests that run one
// ask `shell::skip_unless_posix` first — on a Windows machine with Git Bash they all run, and on one without
// it they print a SKIP line rather than feeding a bash script to PowerShell.
mod jobs;
mod shell;
