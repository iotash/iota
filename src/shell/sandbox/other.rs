//! Sandbox stub for platforms without a supported OS sandbox (internal/shell/`sandbox_other.go`).

use std::path::PathBuf;

/// No sandbox on this platform.
pub(crate) fn available() -> bool {
    false
}

/// Always fails: `sandboxing is not supported on this platform`.
pub(crate) fn command(
    _shell: &crate::shell::interp::Interpreter,
    _script: &str,
    _writable: &[PathBuf],
    _network: bool,
) -> Result<tokio::process::Command, super::SandboxError> {
    Err(super::SandboxError::Unsupported)
}
