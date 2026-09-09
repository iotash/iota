//! Sandbox stub for platforms without a supported OS sandbox (internal/shell/`sandbox_other.go`).

use std::path::{Path, PathBuf};

/// No sandbox on this platform.
pub(crate) fn available() -> bool {
    false
}

/// Always fails: `sandboxing is not supported on this platform`.
pub(crate) fn command(
    _bash: &Path,
    _script: &str,
    _writable: &[PathBuf],
    _network: bool,
) -> Result<tokio::process::Command, String> {
    Err("sandboxing is not supported on this platform".to_owned())
}
