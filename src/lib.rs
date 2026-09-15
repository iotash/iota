#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! iota — an AI chat CLI for the terminal. ONE package: the library holds every module and
//! `src/main.rs` is the thin binary (`docs/ARCHITECTURE.md` §1).

pub mod agents;
pub mod app;
pub mod cmd;
pub(crate) mod config;
pub mod headless;
pub mod host;
pub mod imgterm;
pub mod llm;
pub mod markdown;
pub mod mathtext;
pub mod mcp;
pub mod provider;
pub mod repl;
pub mod session;
pub mod shell;
pub(crate) mod sync;
#[cfg(feature = "testing")]
pub mod testing;
pub mod text;
pub mod tool;
pub mod ui;

/// Boxed, `Send` future used by every object-safe async trait in the crate.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// A boxed, thread-safe error.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
