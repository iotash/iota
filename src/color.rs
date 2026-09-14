//! Whether this process paints its output — resolved ONCE at the binary edge (like
//! [`HostDirs`](crate::app::HostDirs)) and read wherever an escape sequence is about to be
//! written.
//!
//! Two layers consult it, differently (DIVERGENCES X-27, X-28):
//!
//! - the **chat side** (`repl::styles`, the markdown `RenderOptions`, `render_diff`, OSC 8
//!   hyperlinks) is TEXT that gets copied, exported and replayed, and under [`ColorMode::Off`]
//!   it emits no escape at all — bold, faint and reverse included. That is fatih/color's
//!   `NoColor` rule, which the Go chat side ran under;
//! - the **frame side** (`ui`) is a live inline TUI that renders through a cell grid, and under
//!   [`ColorMode::Off`] it drops every foreground and background color at the ONE byte→cell
//!   boundary (`ui::spans::ansi_to_spans`) while keeping the attributes: a focused tab, a
//!   cursor row or a separator still has to be tellable from its neighbours, and a picture
//!   is not what `NO_COLOR` asks for. That is lipgloss's `Ascii` color profile, which the Go
//!   frame ran under.
//!
//! The half-block image rasteriser (`imgterm`) is exempt: its truecolor cells ARE the
//! picture, and a monochrome image is not a less decorated one but a missing one.

use std::sync::OnceLock;

use crate::vars::EnvSource;

/// Whether SGR/OSC sequences may be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// Paint: every style helper emits its sequence.
    On,
    /// Plain text on the chat side; attributes only on the frame side.
    Off,
}

impl ColorMode {
    /// Decides from the environment and the nature of stdout. Any ONE of these turns color
    /// off; they are listed in order of authority, which is the order a future override
    /// (`--color=always`, `CLICOLOR_FORCE`) would have to respect:
    ///
    /// 1. `NO_COLOR` is set to a non-empty value — <https://no-color.org>: presence is what
    ///    counts, the value is not inspected (`NO_COLOR=0` disables too), and an EMPTY value
    ///    is the same as unset. The user said so explicitly, so it outranks everything.
    /// 2. `TERM=dumb` — the terminal declared it cannot interpret escapes.
    /// 3. stdout is not a terminal — a pipe or a file gets text, never escapes.
    ///
    /// The same three tests, in the same order, are fatih/color's `NoColor` initialiser —
    /// what the Go chat side ran under.
    pub fn detect(env: &dyn EnvSource, stdout_is_terminal: bool) -> Self {
        // `EnvSource::var` already reads an empty value as unset.
        if env.var("NO_COLOR").is_some() {
            return Self::Off;
        }
        if env.var("TERM").as_deref() == Some("dumb") {
            return Self::Off;
        }
        if !stdout_is_terminal {
            return Self::Off;
        }
        Self::On
    }

    /// [`detect`](Self::detect) over the process environment and the real stdout.
    pub fn from_process() -> Self {
        use std::io::IsTerminal as _;
        Self::detect(&crate::vars::ProcessEnv, std::io::stdout().is_terminal())
    }

    /// `true` for [`ColorMode::On`].
    #[must_use]
    pub fn is_on(self) -> bool {
        self == Self::On
    }
}

/// The process-wide decision. Unset until [`init`] runs, and read as [`ColorMode::On`] then:
/// the library's own byte pins (every SGR golden in the tree) run without a binary edge, and
/// a consumer that never decides gets what a terminal user gets.
static MODE: OnceLock<ColorMode> = OnceLock::new();

/// Records the decision for the rest of the process. The FIRST call wins and later ones are
/// ignored (a process has one stdout), so a test binary that needs [`ColorMode::Off`] calls
/// this before anything renders and every test in it sees the same answer.
pub fn init(mode: ColorMode) -> ColorMode {
    *MODE.get_or_init(|| mode)
}

/// The decision in force ([`ColorMode::On`] until [`init`] says otherwise).
pub fn current() -> ColorMode {
    MODE.get().copied().unwrap_or(ColorMode::On)
}

/// `current().is_on()` — the one-word question every style helper asks.
pub fn enabled() -> bool {
    current().is_on()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::ColorMode;
    use crate::vars::EnvSource;

    /// A map-backed environment (empty values read as unset, like `ProcessEnv`).
    fn env(vars: &[(&str, &str)]) -> impl EnvSource {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| map.get(name).filter(|v| !v.is_empty()).cloned()
    }

    #[test]
    fn a_terminal_with_a_clean_environment_paints() {
        assert_eq!(ColorMode::detect(&env(&[]), true), ColorMode::On);
        assert_eq!(
            ColorMode::detect(&env(&[("TERM", "xterm-256color")]), true),
            ColorMode::On
        );
    }

    #[test]
    fn no_color_disables_whatever_its_value() {
        // no-color.org: presence, not value — `0`, `false` and `no` all disable.
        for value in ["1", "0", "false", "no", "yes", "anything"] {
            assert_eq!(
                ColorMode::detect(&env(&[("NO_COLOR", value)]), true),
                ColorMode::Off,
                "NO_COLOR={value}"
            );
        }
    }

    #[test]
    fn an_empty_no_color_is_unset() {
        assert_eq!(
            ColorMode::detect(&env(&[("NO_COLOR", "")]), true),
            ColorMode::On
        );
    }

    #[test]
    fn a_dumb_terminal_gets_no_escapes() {
        assert_eq!(
            ColorMode::detect(&env(&[("TERM", "dumb")]), true),
            ColorMode::Off
        );
        // Only the exact value: `dumb-emacs-ansi` and friends are not dumb.
        assert_eq!(
            ColorMode::detect(&env(&[("TERM", "dumb-emacs-ansi")]), true),
            ColorMode::On
        );
    }

    #[test]
    fn a_pipe_gets_no_escapes() {
        assert_eq!(ColorMode::detect(&env(&[]), false), ColorMode::Off);
    }

    #[test]
    fn the_unset_default_paints() {
        // `init` is never called in the library's own tests: every SGR golden in the tree
        // depends on this default.
        assert!(super::enabled());
        assert_eq!(super::current(), ColorMode::On);
    }
}
