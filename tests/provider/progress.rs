//! The upload-progress body over wiremock (WP67; `T3_TEST_PLAN` §5).
//!
//! The test itself — `TestRecordingTransportProgress` (`chat/progress_test.go:68`): the 2048-byte
//! body arrives intact under an explicit `Content-Length` and no `Transfer-Encoding`, `uploaded ==
//! 2048`, `sent == 1` — lives in `src/llm/progress.rs::wire_tests`. It has to install handlers on
//! a `TurnProgress` and enter the `TURN_PROGRESS` task-local scope, and both are `pub(crate)`
//! (`llm::progress` is a private module and `T3_CONTRACTS` §12 adds no `pub` for it); moving the
//! test in-file was the alternative to widening the crate's public API for one assertion.
//! See `scratchpad/tui/design/DEVIATIONS3.md`, `- [WP67]`.
