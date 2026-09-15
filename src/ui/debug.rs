//! `IOTA_DEBUG_REGION=<path>` op-trace for the staging window (internal/ui/debug.go).
//! Opened once, by the binary, from the injected environment (empty/unset = disabled). Spacing faults are invisible to unit tests when
//! they sit in a producer or in the renderer rather than the region itself — a live op
//! trace against a real provider is the fastest way to localize which layer emitted a
//! stray row. Kept as permanent tooling (the sanctioned live-op localization aid).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;

use crate::app::env::Env;

/// The variable that names the trace file.
pub const ENV_VAR: &str = "IOTA_DEBUG_REGION";

/// The append-only trace file, opened once from `$IOTA_DEBUG_REGION` (debug.go:15-25).
/// `None` when the variable is unset/empty or the open fails (trace silently disabled);
/// never set in a process that did not [`install_region_trace`] (a library user, a test).
static REGION_TRACE: OnceLock<Option<File>> = OnceLock::new();

/// Opens the trace `IOTA_DEBUG_REGION` names, once, from the injected environment. The binary
/// calls it before the runtime is built; a second call is a no-op.
pub fn install_region_trace(env: &Env) {
    let file = env
        .var(ENV_VAR)
        .and_then(|path| OpenOptions::new().append(true).create(true).open(path).ok());
    let _ = REGION_TRACE.set(file);
}

fn trace_file() -> Option<&'static File> {
    REGION_TRACE.get().and_then(Option::as_ref)
}

/// Appends one region-op line to the trace (debug.go:27-33). The message closure runs
/// only when tracing is enabled; write errors are ignored (a trace must never take the
/// UI down).
pub(crate) fn debug_region(msg: impl FnOnce() -> String) {
    if let Some(mut f) = trace_file() {
        let _ = writeln!(f, "{}", msg());
    }
}
