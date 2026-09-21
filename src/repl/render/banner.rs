//! The startup banner: the `iota` wordmark beside three facts — the version, the mode, the
//! directory. Three rows, and nothing else.
//!
//! Go printed "Chat started…", the command table, the session row and the agent-mode counts
//! (chat/run.go:86-105); those went with DIVERGENCES X-46. What the chat can be told is the
//! composer's completion row, which reads the SAME table the dispatch chain reads — the
//! one-table law (`repl::commands`, `TUI_DESIGN` §9) — so a banner listing it again pinned
//! nothing the row does not. What model the chat runs is the status row's.
//!
//! Go printed its rows to plain stdout before the Program claimed the terminal. Here they go
//! through the facade like everything else: the frame engine inserts them above the
//! composer, which is the same scrollback in the same order, and the "nothing writes to
//! the terminal except through `ui`" invariant survives.

use std::path::Path;

use crate::agents::Overlay;
use crate::text::width::truncate_middle;

use crate::repl::render::styles::{cyan, dim};

/// The wordmark: three rows of half blocks, 20 columns each, a one-column lead.
const LOGO: [&str; 3] = [
    " ▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█",
    "  █  █  █   █   █▀▀█",
    " ▀▀▀ ▀▀▀▀   ▀   ▀  ▀",
];

/// The wordmark's display columns (every glyph in it is one column wide).
const LOGO_COLS: usize = 20;

/// Between the wordmark and the facts.
const GUTTER: &str = "   ";

/// Below this many columns the wordmark is dropped and the three facts stand alone.
const LOGO_MIN_WIDTH: u16 = 48;

/// The mode row's second segment for a chat that started without a bundle.
const NOT_SAVED: &str = "not saved · /save keeps it";

/// What the three facts say.
pub(crate) struct BannerFacts<'a> {
    /// Agent mode (`agents.<name>.workspace: true`) — the overlay is on.
    pub(crate) workspace: bool,
    /// The bundle the chat persists into; `None` while it has none.
    pub(crate) session_id: Option<&'a str>,
    /// The chat STARTED without a bundle: a `/save` factory exists.
    pub(crate) ephemeral: bool,
    /// The history came out of a bundle (`iota resume`).
    pub(crate) resumed: bool,
    /// The project root in agent mode, else the working directory.
    pub(crate) dir: &'a Path,
    /// The user's home, which the directory row shortens to `~`.
    pub(crate) home: Option<&'a Path>,
}

/// The banner's three rows: the wordmark (cyan) on the left when `width` allows it, and on
/// the right the version (dim), the mode row and the directory row. The directory row is
/// cut to the columns left of `width` — the middle out, both ends kept — so it never wraps.
///
/// `width` is the terminal's, as the facade reports it; `0` (unknown) is read as wide, and
/// nothing is cut.
pub(crate) fn banner_lines(facts: &BannerFacts<'_>, width: u16) -> Vec<String> {
    let logo = width == 0 || width >= LOGO_MIN_WIDTH;
    let dir_cols = match (width, logo) {
        (0, _) => usize::MAX,
        (w, true) => usize::from(w).saturating_sub(LOGO_COLS + GUTTER.len()),
        (w, false) => usize::from(w),
    };
    let right = [
        dim(concat!("v", env!("CARGO_PKG_VERSION"))),
        mode_row(facts),
        truncate_middle(&tilde(facts.dir, facts.home), dir_cols),
    ];
    if !logo {
        return right.into();
    }
    LOGO.iter()
        .zip(right)
        .map(|(logo, fact)| format!("{}{GUTTER}{fact}", cyan(logo)))
        .collect()
}

/// The skill-discovery warnings, dim, in the overlay's order — printed under the banner in
/// agent mode, as Go did (chat/run.go:103-105).
pub(crate) fn overlay_warnings(overlay: Option<&Overlay>) -> Vec<String> {
    overlay
        .map(Overlay::warnings)
        .unwrap_or_default()
        .iter()
        .map(|w| dim(&format!("⚠ {w}")))
        .collect()
}

/// `agent`/`chat`, then where the chat is being saved — `session <id>`, `resumed <id>`, or the
/// `/save` hint — joined with ` · `. A chat with no bundle and no way to mint one (a test
/// fixture; a real run always has one or the other) names only its mode.
fn mode_row(f: &BannerFacts<'_>) -> String {
    let mode = if f.workspace { "agent" } else { "chat" };
    let bundle = match (f.session_id, f.resumed, f.ephemeral) {
        (Some(id), true, _) => format!("resumed {id}"),
        (Some(id), false, _) => format!("session {id}"),
        (None, _, true) => NOT_SAVED.to_owned(),
        (None, _, false) => return mode.to_owned(),
    };
    format!("{mode} · {bundle}")
}

/// `dir` with a leading `home` replaced by `~` — by path components, so `/home/me2` is not
/// under `/home/me`; the separator stays the platform's.
fn tilde(dir: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => dir.display().to_string(),
    }
}

// The NO_COLOR shape — the same rows with no escape at all — is `tests/nocolor`'s: the
// decision is process-wide and made once, which this binary cannot do.
#[cfg(test)]
mod tests {
    use std::path::{MAIN_SEPARATOR, Path, PathBuf};

    use super::{BannerFacts, LOGO, LOGO_COLS, banner_lines};
    use crate::text::ansi::strip_sgr;

    const ID: &str = "01hq3z8a9b2c";

    fn facts<'a>(dir: &'a Path, home: Option<&'a Path>) -> BannerFacts<'a> {
        BannerFacts {
            workspace: false,
            session_id: Some(ID),
            ephemeral: false,
            resumed: false,
            dir,
            home,
        }
    }

    fn plain(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| strip_sgr(l)).collect()
    }

    /// The mode row, bare, at 80 columns.
    fn mode_row(f: &BannerFacts<'_>) -> String {
        plain(&banner_lines(f, 80))[1].clone()
    }

    /// The directory row, bare, at 80 columns.
    fn dir_row(dir: &Path, home: Option<&Path>) -> String {
        plain(&banner_lines(&facts(dir, home), 80))[2].clone()
    }

    /// Three rows: the wordmark, three spaces, then the version, the mode and the directory.
    #[test]
    fn three_rows_logo_left_facts_right() {
        let lines = banner_lines(&facts(Path::new("/srv/app"), None), 80);
        assert_eq!(
            plain(&lines),
            [
                format!(" ▀█▀ █▀▀█ ▀▀█▀▀ █▀▀█   v{}", env!("CARGO_PKG_VERSION")),
                format!("  █  █  █   █   █▀▀█   chat · session {ID}"),
                " ▀▀▀ ▀▀▀▀   ▀   ▀  ▀   /srv/app".to_owned(),
            ]
        );
        // Every wordmark row is LOGO_COLS columns of single-width glyphs.
        for row in LOGO {
            assert_eq!(crate::text::width::str_width(row), LOGO_COLS, "{row:?}");
        }
        // The wordmark is cyan, the version dim, the two facts below bare.
        assert!(lines[0].starts_with("\x1b[36m ▀█▀") && lines[0].contains("\x1b[2mv"));
        assert!(lines[1].ends_with(&format!("\x1b[0m   chat · session {ID}")));
        assert!(lines[2].ends_with("\x1b[0m   /srv/app"));
    }

    /// The mode row: `agent` or `chat`, then the bundle in one of its three states — or, with
    /// no bundle and no factory, the mode alone.
    #[test]
    fn mode_row_states() {
        let dir = Path::new("/srv/app");
        let f = facts(dir, None);
        assert!(mode_row(&f).ends_with(&format!("   chat · session {ID}")));
        let f = BannerFacts {
            resumed: true,
            ..facts(dir, None)
        };
        assert!(mode_row(&f).ends_with(&format!("   chat · resumed {ID}")));
        let f = BannerFacts {
            session_id: None,
            ephemeral: true,
            ..facts(dir, None)
        };
        assert!(mode_row(&f).ends_with("   chat · not saved · /save keeps it"));
        let f = BannerFacts {
            workspace: true,
            ..facts(dir, None)
        };
        assert!(mode_row(&f).ends_with(&format!("   agent · session {ID}")));
        let f = BannerFacts {
            session_id: None,
            ..facts(dir, None)
        };
        assert!(mode_row(&f).ends_with("█▀▀█   chat"));
    }

    /// `~` stands for the home directory, by components: the home itself is `~`, a sibling
    /// that merely shares the prefix is left alone, and without a home nothing is replaced.
    #[test]
    fn directory_row_shortens_home() {
        let home = PathBuf::from("/Users/me");
        let under = home.join("Work").join("iota");
        assert!(
            dir_row(&under, Some(&home))
                .ends_with(&format!("   ~{MAIN_SEPARATOR}Work{MAIN_SEPARATOR}iota"))
        );
        assert!(dir_row(&home, Some(&home)).ends_with("   ~"));
        let sibling = PathBuf::from("/Users/me2").join("Work");
        assert!(dir_row(&sibling, Some(&home)).ends_with(&format!("   {}", sibling.display())));
        assert!(dir_row(&under, None).ends_with(&format!("   {}", under.display())));
    }

    /// Under 48 columns the wordmark goes and the facts stand alone, unindented; an unknown
    /// width (0) is read as wide.
    #[test]
    fn narrow_terminal_drops_the_logo() {
        let f = facts(Path::new("/srv/app"), None);
        assert_eq!(
            plain(&banner_lines(&f, 47)),
            [
                format!("v{}", env!("CARGO_PKG_VERSION")),
                format!("chat · session {ID}"),
                "/srv/app".to_owned(),
            ]
        );
        assert!(plain(&banner_lines(&f, 48))[0].starts_with(" ▀█▀"));
        assert!(plain(&banner_lines(&f, 0))[0].starts_with(" ▀█▀"));
        // The narrow rows carry no cyan: only the version is styled.
        let narrow = banner_lines(&f, 40);
        assert!(!narrow.iter().any(|l| l.contains("\x1b[36m")));
        assert!(narrow[0].starts_with("\x1b[2mv"));
    }

    /// A path longer than the columns beside the wordmark is cut in the middle, both ends
    /// kept, so the row never wraps: at 80 columns the directory gets 57. Narrow, it gets the
    /// whole width; an unknown width cuts nothing.
    #[test]
    fn a_long_directory_row_is_cut_in_the_middle() {
        let long = Path::new(
            "/Volumes/build/agents/workspaces/2026-09/a-project-with-a-very-long-name/packages/cli",
        );
        let row = |w: u16| plain(&banner_lines(&facts(long, None), w))[2].clone();
        let at_80 = row(80);
        assert_eq!(
            at_80,
            " ▀▀▀ ▀▀▀▀   ▀   ▀  ▀   /Volumes/build/agents/worksp…-very-long-name/packages/cli"
        );
        assert_eq!(crate::text::width::str_width(&at_80), 80);
        // Narrow: the facts alone, the directory at the terminal's width.
        let at_40 = row(40);
        assert_eq!(at_40, "/Volumes/build/agen…ng-name/packages/cli");
        assert_eq!(crate::text::width::str_width(&at_40), 40);
        // A path that fits is left whole; an unknown width cuts nothing.
        assert!(row(200).ends_with("/packages/cli") && !row(200).contains('…'));
        assert!(!row(0).contains('…'));
    }

    /// The skill-discovery warnings ride under the banner; with no overlay there are none.
    #[test]
    fn overlay_warnings_need_an_overlay() {
        assert!(super::overlay_warnings(None).is_empty());
    }
}
