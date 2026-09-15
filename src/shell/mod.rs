//! Process execution (internal/shell): one interpreter child in its own process group with a capped, combined
//! output pipe (`exec`), sandboxed with Seatbelt on macOS / bwrap on Linux when available (`sandbox`), plus
//! the run's registry of children that outlive their round (`jobs`). WHICH interpreter — `bash -c` on Unix,
//! Git Bash / PowerShell / `cmd.exe` on Windows — is `interp`. The mechanism layer only: the `shell` tool's
//! policy (approval, config, result formatting) is `crate::tool::shell`.

pub mod exec;
pub mod interp;
pub mod jobs;
pub(crate) mod sandbox;
