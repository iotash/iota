//! Integration tests of the streaming markdown→ANSI renderer (internal/markdown); `harness` is the shared render helper — one binary per area (docs/MERGE-PLAN.md §2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod ansi;
mod code;
mod harness;
mod highlight;
mod inline;
mod list;
mod math_corpus;
mod math_display;
mod quote;
mod spacing;
mod table;
