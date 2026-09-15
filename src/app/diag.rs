//! Developer diagnostics: `IOTA_LOG=<path>` installs a file subscriber for `tracing` (DIVERGENCES
//! X-29; MIGRATION-ROADMAP §3 #7).
//!
//! `tracing` is the developer's channel, never the user's. rmcp, hyper, process-wrap and this
//! crate's own `debug!`s speak it; nobody hears them unless the variable is set, and nothing a
//! user must see is ever written to it — user-facing warnings go through
//! [`Streams::warning`](crate::cmd::io::Streams::warning) or the transcript. So the switch is not
//! a precondition for seeing warnings, and a run without it is exactly as loud as before.
//!
//! What lands in the file: this crate at `DEBUG` and everything else at `INFO`, one line per
//! event with a UTC timestamp, the level and the target, no SGR (a log file never wants color,
//! and `NO_COLOR` has nothing to say about it). The level, the format and rotation are the
//! roadmap's backlog; this is the pipe.

use std::{fs::File, io, path::Path, sync::Arc};

use tracing_subscriber::{
    filter::Targets, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

use crate::app::env::EnvSource;

/// The variable that names the log file.
pub const ENV_VAR: &str = "IOTA_LOG";

/// Opens `path` for appending and makes it the process's global `tracing` subscriber. Must run
/// before anything emits — in practice, before the tokio runtime is built. A second call fails
/// like a second `set_global_default` does.
pub fn install(path: &Path) -> io::Result<()> {
    let file = Arc::new(File::options().create(true).append(true).open(path)?);
    let filter = Targets::new()
        .with_default(tracing::Level::INFO)
        .with_target("iota", tracing::Level::DEBUG);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false)
        .with_target(true);
    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init()
        .map_err(|e| io::Error::other(e.to_string()))?;
    tracing::info!(version = crate::cmd::VERSION, "iota diagnostics on");
    Ok(())
}

/// [`install`] when `IOTA_LOG` names a path; a file that cannot be opened is one `warn` line and
/// the run goes on without diagnostics — the user asked for a side channel, not for the run to
/// depend on it.
pub fn install_from_env(env: &dyn EnvSource, warn: &mut dyn FnMut(String)) {
    let Some(path) = env.var(ENV_VAR) else { return };
    if let Err(e) = install(Path::new(&path)) {
        warn(format!("Warning: {ENV_VAR}: cannot log to {path}: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::install_from_env;

    /// An unset variable installs nothing and says nothing.
    #[test]
    fn unset_is_silent() {
        let env = |_: &str| None;
        let mut warnings = Vec::new();
        install_from_env(&env, &mut |w| warnings.push(w));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A path that cannot be opened warns once — with the variable, the path and the reason —
    /// and returns. (The success path installs a process-global subscriber, so it is exercised
    /// against the binary in `tests/cmd/cli.rs`, never in this shared test process.)
    #[test]
    fn an_unopenable_path_warns_and_continues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-such-dir").join("iota.log");
        let path_str = path.to_string_lossy().into_owned();
        let env = move |name: &str| (name == "IOTA_LOG").then(|| path_str.clone());
        let mut warnings = Vec::new();
        install_from_env(&env, &mut |w| warnings.push(w));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with(&format!(
                "Warning: IOTA_LOG: cannot log to {}: ",
                path.display()
            )),
            "{}",
            warnings[0]
        );
        assert!(!path.exists());
    }
}
