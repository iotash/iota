//! Integration tests of the MCP manager (mcp/*) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The shared fixtures this binary uses (`tests/common/`): the mock OAuth server and the temp project.
#[path = "../common/oauth_mock.rs"]
mod common_oauth;
#[path = "../common/project.rs"]
mod common_project;

mod manager;
mod naming;
mod oauth;
