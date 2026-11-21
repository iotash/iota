//! Test-only paths every module's unit tests share — the lib's half of `tests/common/paths.rs`. A literal Unix
//! path is not the same thing on every platform (paths-audit §2 S5): `/x` is relative on Windows, and nothing
//! is unwritable under `/dev/null` there.

use std::path::{Path, PathBuf};

/// An absolute fixture root that is never touched: a `/proj` would be RELATIVE on Windows (`filepath.IsAbs`'s
/// twin wants a drive letter), and joining onto it there spells a half-Unix `/proj\x` no assertion can be
/// written against.
#[cfg(windows)]
pub(crate) const FIXTURE_ROOT: &str = r"C:\proj";
#[cfg(not(windows))]
pub(crate) const FIXTURE_ROOT: &str = "/proj";

/// A directory path no write reaches on any platform: it is under the crate's `Cargo.toml`, a regular file, so
/// nothing can be created there. (`/dev/null/x` is not that on Windows: there it is `\dev\null\x` on the
/// current drive, which a run can create.)
pub(crate) fn unwritable() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("Cargo.toml")
        .join("unwritable")
}
