//! `IOTA_DEBUG_REGION=<path>` op-trace for the staging window (internal/ui/debug.go).
//! Opened once (empty/unset = disabled). Spacing faults are invisible to unit tests when
//! they sit in a producer or in the renderer rather than the region itself — a live op
//! trace against a real provider is the fastest way to localize which layer emitted a
//! stray row. Kept as permanent tooling (the sanctioned live-op localization aid).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;

/// The append-only trace file, opened once from `$IOTA_DEBUG_REGION` (debug.go:15-25).
/// `None` when the variable is unset/empty or the open fails (trace silently disabled).
static REGION_TRACE: OnceLock<Option<File>> = OnceLock::new();

fn trace_file() -> Option<&'static File> {
    REGION_TRACE
        .get_or_init(|| {
            let path = std::env::var("IOTA_DEBUG_REGION").ok()?;
            if path.is_empty() {
                return None;
            }
            OpenOptions::new().append(true).create(true).open(path).ok()
        })
        .as_ref()
}

/// Appends one region-op line to the trace (debug.go:27-33). The message closure runs
/// only when tracing is enabled; write errors are ignored (a trace must never take the
/// UI down).
pub(crate) fn debug_region(msg: impl FnOnce() -> String) {
    if let Some(mut f) = trace_file() {
        let _ = writeln!(f, "{}", msg());
    }
}
