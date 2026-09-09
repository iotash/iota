//! Process execution (internal/shell): one `bash -c` child in its own process group with a capped, combined
//! output pipe (`exec`), sandboxed with Seatbelt on macOS / bwrap on Linux when available (`sandbox_*`). The
//! mechanism layer only — the `bash` tool's policy (approval, config, result formatting) is `crate::tool::shell`.

pub mod exec;
#[cfg(target_os = "macos")]
pub(crate) mod sandbox_darwin;
#[cfg(target_os = "linux")]
pub(crate) mod sandbox_linux;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) mod sandbox_other;
