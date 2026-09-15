//! Shared fixtures of every integration-test binary (`tests/<area>/main.rs` mounts this directory with
//! `#[path = "../common/mod.rs"]`): the temp-project and session-store helpers, the fake dispatchers and
//! providers, the wiremock SSE/JSON helpers and the recorded transcript. Each binary uses a subset, hence
//! `dead_code` and `unused_imports` (the re-exports below) are allowed here and nowhere else.
#![allow(dead_code, unused_imports)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod child;
pub mod fake_mcp;
pub mod project;
pub mod session;
pub mod stub;
pub mod transcript;
pub mod wire;

pub use child::cleared_env;
pub use fake_mcp::{FakeMcp, prefix_for, static_prefix};

/// The checked-in 2×2 PNG (top row red, bottom row blue; 8-bit RGBA) every image test renders:
/// `tests/fixtures/images/rb-2x2.png` (`T3_CONTRACTS` §8).
pub const RB_2X2_PNG: &[u8] = include_bytes!("../fixtures/images/rb-2x2.png");
pub use project::temp_project;
pub use session::*;
pub use stub::stub_tool;
pub use wire::*;
