//! What the process learns ONCE at its edge and injects everywhere: program identity and the process-level
//! directories (internal/app/app.go plus the `os.UserCacheDir`/`os.TempDir` rules) here, the environment seams
//! and `${var}` expansion in `env`, the color decision in `color`, the `IOTA_LOG` tap in `diag`, the atomic file
//! writer in `fs`, and the lexical path helpers in `paths`.

pub mod color;
pub mod diag;
pub mod env;
pub mod fs;
pub(crate) mod paths;

use std::path::{Path, PathBuf};

/// The command and brand name.
pub(crate) const NAME: &str = "iota";
/// The per-user directory under `$HOME` (`~/.iota`).
pub(crate) const DOT_DIR: &str = ".iota";
/// The config file stem: `~/.iota.yaml`, `./.iota.yml`, ….
pub(crate) const CONFIG_BASE: &str = ".iota";
/// Config file extensions in lookup order.
pub(crate) const CONFIG_EXTS: [&str; 2] = [".yaml", ".yml"];
/// The version `--version` and `iota version` print (and every other place that names the release: the
/// `IOTA_LOG` hello, the MCP `clientInfo`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Process-level directories, resolved ONCE at the binary edge and injected everywhere (tests build them by hand).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostDirs {
    /// `os.UserHomeDir`: `$HOME` (unix) / `%USERPROFILE%` (windows), None when unset or empty. NEVER getpwuid.
    pub home: Option<PathBuf>,
    /// `os.Getwd`; None when unavailable.
    pub cwd: Option<PathBuf>,
    /// `os.TempDir`: `$TMPDIR` else `/tmp` (verbatim, trailing slash kept).
    pub temp: PathBuf,
    /// `os.UserCacheDir`: macOS `home/Library/Caches`; linux `$XDG_CACHE_HOME` if absolute else `home/.cache`; None
    /// without home.
    pub cache: Option<PathBuf>,
    /// The running binary, `std::env::current_exe()` canonicalised; None when the OS could not say. Not a
    /// directory, but the one other process fact resolved at this edge for the same reason: the `shell` set's
    /// rule that iota itself runs outside the sandbox compares a command's first word against it, and the
    /// harness prompt names it — both from here, never from a second probe.
    pub exe: Option<PathBuf>,
}

impl HostDirs {
    /// Resolves every directory from the process environment.
    pub fn from_env() -> Self {
        let home = user_home();
        Self {
            cache: cache_dir(home.as_deref()),
            home,
            cwd: std::env::current_dir().ok(),
            temp: temp_dir(),
            exe: std::env::current_exe().ok().and_then(|p| canonical(&p)),
        }
    }

    /// `<home>/.iota` (`app.Home()`).
    pub fn app_home(&self) -> Option<PathBuf> {
        self.home.as_ref().map(|h| h.join(DOT_DIR))
    }

    /// `<home>/.iota/images`.
    pub fn images_dir(&self) -> Option<PathBuf> {
        self.app_home().map(|h| h.join("images"))
    }
}

/// `std::fs::canonicalize` as a comparison key that is also fit to show: symlinks resolved, and on Windows
/// the `\\?\` verbatim prefix `canonicalize` puts on a drive path dropped again, so the result reads like a
/// path the user would write (and two canonical paths compare by their bytes). `None` when the path does not
/// exist or cannot be resolved.
pub(crate) fn canonical(p: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(p).ok()?;
    if cfg!(windows)
        && let Some(s) = resolved.to_str()
        && let Some(rest) = s.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return Some(PathBuf::from(rest));
    }
    Some(resolved)
}

/// `os.UserHomeDir` rule: `$HOME` (`%USERPROFILE%` on windows), None when unset or empty.
pub(crate) fn user_home() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `os.UserCacheDir` rule for the current platform, given the home directory: macOS `home/Library/Caches`; other
/// unix `$XDG_CACHE_HOME` when set and absolute, else `home/.cache`; None without a home (and on non-unix).
pub(crate) fn cache_dir(home: Option<&Path>) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library").join("Caches"))
    } else if cfg!(unix) {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".cache")))
    } else {
        None
    }
}

/// `os.TempDir` rule: `$TMPDIR` (verbatim) else `/tmp` on unix; the platform default elsewhere.
pub(crate) fn temp_dir() -> PathBuf {
    if cfg!(unix) {
        std::env::var_os("TMPDIR")
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
    } else {
        std::env::temp_dir()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{CONFIG_BASE, CONFIG_EXTS, DOT_DIR, HostDirs, NAME, cache_dir};

    // Go: internal/app/app_test.go:8
    #[test]
    fn test_home() {
        let dirs = HostDirs {
            home: Some(PathBuf::from("/tmp/iota-test-home")),
            ..HostDirs::default()
        };
        let got = dirs.app_home().expect("home is set");
        assert_eq!(
            got.file_name().and_then(|n| n.to_str()),
            Some(".iota"),
            "Home() = {got:?}, want …/.iota"
        );
        assert_eq!(got, Path::new("/tmp/iota-test-home/.iota"));
        assert_eq!(
            dirs.images_dir(),
            Some(PathBuf::from("/tmp/iota-test-home/.iota/images"))
        );
        // Without a home there is no app home (Go: the UserHomeDir error).
        let none = HostDirs::default();
        assert!(none.app_home().is_none());
        assert!(none.images_dir().is_none());
        // Identity constants.
        assert_eq!(NAME, "iota");
        assert_eq!(DOT_DIR, ".iota");
        assert_eq!(CONFIG_BASE, ".iota");
        assert_eq!(CONFIG_EXTS, [".yaml", ".yml"]);
    }

    #[test]
    fn cache_dir_rules_per_os() {
        let home = Path::new("/h");
        if cfg!(target_os = "macos") {
            assert_eq!(
                cache_dir(Some(home)),
                Some(PathBuf::from("/h/Library/Caches"))
            );
            assert_eq!(cache_dir(None), None);
        } else if cfg!(unix) {
            // Read (never mutate) the process environment to compute the expectation.
            let xdg = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute());
            if let Some(p) = xdg {
                assert_eq!(cache_dir(Some(home)), Some(p.clone()));
                assert_eq!(cache_dir(None), Some(p));
            } else {
                assert_eq!(cache_dir(Some(home)), Some(PathBuf::from("/h/.cache")));
                assert_eq!(cache_dir(None), None);
            }
        } else {
            assert_eq!(cache_dir(Some(home)), None);
        }
        // from_env agrees with the free functions it composes.
        let dirs = HostDirs::from_env();
        assert_eq!(dirs.cache, cache_dir(dirs.home.as_deref()));
        assert_eq!(dirs.temp, super::temp_dir());
        assert!(!dirs.temp.as_os_str().is_empty());
        // The executable is canonical: what a symlink on PATH resolves to, so a comparison against it needs
        // no second canonicalisation of this side — and it carries no `\\?\` prefix on Windows.
        let exe = dirs.exe.expect("the test binary knows its own path");
        assert_eq!(Some(exe.clone()), super::canonical(&exe));
        assert!(!exe.to_string_lossy().starts_with(r"\\?\"), "{exe:?}");
    }
}
