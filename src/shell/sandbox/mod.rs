//! The OS sandbox a `shell` command runs under when the config asks for one and the platform has one
//! (internal/shell/sandbox_*.go): Seatbelt on macOS (`darwin`), bubblewrap on Linux (`linux`), nothing
//! elsewhere (`other`). One backend per target, with the same two entry points: `available`, and `command`,
//! which wraps the interpreter invocation.

/// Why the sandbox wrapper could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    /// The platform has no sandbox (`other`) — the only way `command` fails.
    #[error("sandboxing is not supported on this platform")]
    Unsupported,
}

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod other;

#[cfg(target_os = "macos")]
pub(crate) use darwin::{available, command};
#[cfg(target_os = "linux")]
pub(crate) use linux::{available, command};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) use other::{available, command};
