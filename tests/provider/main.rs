//! Integration tests of the provider adapters and the wire layer (provider/*, internal/llm) — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../common/mod.rs"]
mod common;

mod anthropic;
mod google;
mod imagen;
mod images;
mod openai;
mod openresponses;
mod progress;
mod reqlog;
mod strings;
mod think;
mod tool_delta;
mod usage_capability;
mod usage_conv;
mod wire;
