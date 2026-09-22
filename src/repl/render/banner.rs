//! The startup banner: a card — the `ι> iota` mark beside the version, the mode row (and,
//! inside a detected host, its name) and the directory, in a rounded frame that hugs the
//! widest of the three. Five rows where the terminal has the room, the three bare rows
//! where it has not, and nothing else.
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
use crate::text::width::{str_width, truncate_middle};

use crate::repl::render::styles::{cyan, dim};

/// The mark that opens the card: the letter, the prompt, the name — cyan, one piece.
const MARK: &str = "ι> iota";

/// This build's version, dim, beside the mark.
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// Between the mark and the version.
const MARK_GAP: &str = "  ";

/// The joiner between a row's segments — dim, so the facts stand out and the dots recede
/// like the frame around them.
const SEP: &str = " · ";

/// The mode row's bundle segment for a chat that started without one — two segments, so
/// the joiner inside it is dim like every other.
const NOT_SAVED: [&str; 2] = ["not saved", "/save keeps it"];

/// What the frame costs a row: the edge and the blank beside it, on both sides.
const FRAME_COLS: usize = 4;

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
    /// The host a detector matched (`host::Presenter::detected_host`: `herdr`, `cmux`), which
    /// the mode row ends with; `None` in a plain terminal — the ANSI fallback is not one.
    pub(crate) host: Option<&'a str>,
}

/// One row of the card: its text, styled, and the columns the text occupies — measured
/// before the styling went on, since an escape has no width and the region does not
/// re-measure what it is handed.
struct Row {
    text: String,
    cols: usize,
}

impl Row {
    /// A row with no styling of its own.
    fn bare(text: String) -> Self {
        let cols = str_width(&text);
        Self { text, cols }
    }
}

/// The banner's rows: the card when `width` allows it — the mark row (`ι> iota`, cyan,
/// then the version, dim), the mode row and the directory row, each padded to the widest,
/// inside a rounded frame (dim) one blank away on either side — else the three rows bare.
/// The directory row is cut to the columns it may have — the middle out, both ends kept —
/// so it never wraps: the terminal's width less the frame's four, or the whole width bare.
///
/// The frame needs its four columns beside the widest row that is never cut — the mode
/// row in every real run (`chat · session <id>` is 27 columns; `agent · not saved · /save
/// keeps it`, the longest, 34; a host adds ` · in <host>`). Narrower, the three rows stand
/// alone.
///
/// `width` is the terminal's, as the facade reports it; `0` (unknown) is read as wide, and
/// nothing is cut.
pub(crate) fn banner_lines(facts: &BannerFacts<'_>, width: u16) -> Vec<String> {
    let mark = Row {
        text: format!("{}{MARK_GAP}{}", cyan(MARK), dim(VERSION)),
        cols: str_width(MARK) + str_width(MARK_GAP) + str_width(VERSION),
    };
    let mode = mode_row(facts);
    let uncut = mark.cols.max(mode.cols);
    let framed = width == 0 || usize::from(width) >= uncut + FRAME_COLS;
    let dir_cols = match (width, framed) {
        (0, _) => usize::MAX,
        (w, true) => usize::from(w).saturating_sub(FRAME_COLS),
        (w, false) => usize::from(w),
    };
    let dir = Row::bare(truncate_middle(&tilde(facts.dir, facts.home), dir_cols));
    let rows = [mark, mode, dir];
    if !framed {
        return rows.into_iter().map(|r| r.text).collect();
    }
    let inner = rows.iter().map(|r| r.cols).max().unwrap_or(0);
    let edge = "─".repeat(inner + 2);
    let mut out = Vec::with_capacity(rows.len() + 2);
    out.push(dim(&format!("╭{edge}╮")));
    out.extend(rows.iter().map(|r| {
        format!(
            "{} {}{} {}",
            dim("│"),
            r.text,
            " ".repeat(inner - r.cols),
            dim("│")
        )
    }));
    out.push(dim(&format!("╰{edge}╯")));
    out
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
/// `/save` hint — then `in <host>` inside a detected host, joined with a dim ` · `. A chat with
/// no bundle and no way to mint one (a test fixture; a real run always has one or the other)
/// skips the middle segment.
fn mode_row(f: &BannerFacts<'_>) -> Row {
    let mut segments = vec![(if f.workspace { "agent" } else { "chat" }).to_owned()];
    match (f.session_id, f.resumed, f.ephemeral) {
        (Some(id), true, _) => segments.push(format!("resumed {id}")),
        (Some(id), false, _) => segments.push(format!("session {id}")),
        (None, _, true) => segments.extend(NOT_SAVED.iter().map(|s| (*s).to_owned())),
        (None, _, false) => {}
    }
    if let Some(host) = f.host {
        segments.push(format!("in {host}"));
    }
    let cols = segments.iter().map(|s| str_width(s)).sum::<usize>()
        + str_width(SEP) * segments.len().saturating_sub(1);
    Row {
        text: segments.join(&dim(SEP)),
        cols,
    }
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

// The NO_COLOR shape — the same rows, the frame included, with no escape at all — is
// `tests/nocolor`'s: the decision is process-wide and made once, which this binary cannot
// do. What is pinned here is that the frame is TEXT the styles wrap, never an escape of its
// own: strip the escapes and the card is still standing.
#[cfg(test)]
mod tests {
    use std::path::{MAIN_SEPARATOR, Path, PathBuf};

    use super::{BannerFacts, banner_lines};
    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;

    const ID: &str = "01hq3z8a9b2c";

    /// The version row's text, bare.
    const VERSION_ROW: &str = concat!("ι> iota  v", env!("CARGO_PKG_VERSION"));

    fn facts<'a>(dir: &'a Path, home: Option<&'a Path>) -> BannerFacts<'a> {
        BannerFacts {
            workspace: false,
            session_id: Some(ID),
            ephemeral: false,
            resumed: false,
            dir,
            home,
            host: None,
        }
    }

    fn plain(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| strip_sgr(l)).collect()
    }

    /// A framed row's content: the edges and the blanks beside them taken off.
    fn inside(row: &str) -> String {
        row.strip_prefix("│ ")
            .and_then(|r| r.strip_suffix(" │"))
            .unwrap_or_else(|| panic!("not a framed row: {row:?}"))
            .trim_end()
            .to_owned()
    }

    /// The mode row's content, bare, at 80 columns.
    fn mode_row(f: &BannerFacts<'_>) -> String {
        inside(&plain(&banner_lines(f, 80))[2])
    }

    /// The directory row's content, bare, at 80 columns.
    fn dir_row(dir: &Path, home: Option<&Path>) -> String {
        inside(&plain(&banner_lines(&facts(dir, home), 80))[3])
    }

    /// Five rows: the frame's top edge, the mark and the version, the mode row, the
    /// directory, the bottom edge — the frame one blank away from the widest row, every row
    /// padded to it; the mark cyan, the version and the frame dim, the directory bare.
    #[test]
    fn a_card_around_the_three_facts() {
        let lines = banner_lines(&facts(Path::new("/srv/app"), None), 80);
        assert_eq!(
            plain(&lines),
            [
                "╭─────────────────────────────╮".to_owned(),
                format!("│ {VERSION_ROW:<27} │"),
                format!("│ chat · session {ID} │"),
                "│ /srv/app                    │".to_owned(),
                "╰─────────────────────────────╯".to_owned(),
            ]
        );
        assert_eq!(
            lines[0],
            format!("\x1b[2m╭{}╮\x1b[0m", "─".repeat(29)),
            "the top edge is dim"
        );
        assert!(lines[1].starts_with("\x1b[2m│\x1b[0m \x1b[36mι> iota\x1b[0m  \x1b[2mv"));
        assert!(
            lines[1].ends_with("\x1b[0m             \x1b[2m│\x1b[0m"),
            "{:?}",
            lines[1]
        );
        assert!(
            lines[3].starts_with("\x1b[2m│\x1b[0m /srv/app "),
            "{:?}",
            lines[3]
        );
        assert_eq!(
            lines[4],
            format!("\x1b[2m╰{}╯\x1b[0m", "─".repeat(29)),
            "the bottom edge is dim"
        );
        // Every row of the card is the same width.
        for row in plain(&lines) {
            assert_eq!(str_width(&row), 31, "{row:?}");
        }
    }

    /// The frame hugs the widest row: a directory longer than the mode row widens the card
    /// to the directory, and the shorter rows are padded to it.
    #[test]
    fn the_frame_hugs_the_widest_row() {
        let dir = Path::new("/srv/app/with/a/longer/path/than/the/mode/row");
        let lines = plain(&banner_lines(&facts(dir, None), 80));
        assert_eq!(lines[3], format!("│ {} │", dir.display()));
        assert_eq!(
            lines[2],
            format!("│ chat · session {ID}                   │")
        );
        for row in &lines {
            assert_eq!(str_width(row), 49, "{row:?}");
        }
    }

    /// The mode row: `agent` or `chat`, then the bundle in one of its three states — or, with
    /// no bundle and no factory, the mode alone.
    #[test]
    fn mode_row_states() {
        let dir = Path::new("/srv/app");
        let f = facts(dir, None);
        assert_eq!(mode_row(&f), format!("chat · session {ID}"));
        let f = BannerFacts {
            resumed: true,
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), format!("chat · resumed {ID}"));
        let f = BannerFacts {
            session_id: None,
            ephemeral: true,
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), "chat · not saved · /save keeps it");
        let f = BannerFacts {
            workspace: true,
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), format!("agent · session {ID}"));
        let f = BannerFacts {
            session_id: None,
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), "chat");
    }

    /// Every ` · ` in the mode row is dim — the one inside `not saved · /save keeps it`
    /// included — and the segments between them carry no style of their own.
    #[test]
    fn the_mode_row_joins_its_segments_with_dim_dots() {
        let f = BannerFacts {
            workspace: true,
            session_id: None,
            ephemeral: true,
            host: Some("herdr"),
            ..facts(Path::new("/srv/app"), None)
        };
        let row = &banner_lines(&f, 80)[2];
        assert!(
            row.contains(
                "agent\x1b[2m · \x1b[0mnot saved\x1b[2m · \x1b[0m/save keeps it\x1b[2m · \x1b[0min herdr"
            ),
            "{row:?}"
        );
        // A bare ` · ` — one outside a dim span — appears nowhere.
        assert!(!strip_sgr(row).contains('\x1b'), "{row:?}");
        assert_eq!(row.matches(" · ").count(), 3);
        assert_eq!(row.matches("\x1b[2m · \x1b[0m").count(), 3);
    }

    /// Inside a detected host the mode row ends with `in <host>` — after the bundle segment,
    /// or right after the mode when there is none; a plain terminal's row is unchanged.
    #[test]
    fn mode_row_names_a_detected_host() {
        let dir = Path::new("/srv/app");
        let f = BannerFacts {
            host: Some("herdr"),
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), format!("chat · session {ID} · in herdr"));
        let f = BannerFacts {
            workspace: true,
            session_id: None,
            ephemeral: true,
            host: Some("cmux"),
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), "agent · not saved · /save keeps it · in cmux");
        let f = BannerFacts {
            session_id: None,
            host: Some("herdr"),
            ..facts(dir, None)
        };
        assert_eq!(mode_row(&f), "chat · in herdr");
        assert!(!mode_row(&facts(dir, None)).contains(" · in "));
    }

    /// `~` stands for the home directory, by components: the home itself is `~`, a sibling
    /// that merely shares the prefix is left alone, and without a home nothing is replaced.
    #[test]
    fn directory_row_shortens_home() {
        let home = PathBuf::from("/Users/me");
        let under = home.join("Work").join("iota");
        assert_eq!(
            dir_row(&under, Some(&home)),
            format!("~{MAIN_SEPARATOR}Work{MAIN_SEPARATOR}iota")
        );
        assert_eq!(dir_row(&home, Some(&home)), "~");
        let sibling = PathBuf::from("/Users/me2").join("Work");
        assert_eq!(
            dir_row(&sibling, Some(&home)),
            sibling.display().to_string()
        );
        assert_eq!(dir_row(&under, None), under.display().to_string());
    }

    /// The frame needs four columns beside the mode row — the one row never cut: 38 for the
    /// longest plain row (`agent · not saved · /save keeps it`, 34 columns), where the card
    /// exactly fits; under that the three rows stand alone, unframed and unpadded, the mark
    /// still cyan and the version still dim; an unknown width (0) is read as wide.
    #[test]
    fn a_narrow_terminal_drops_the_frame() {
        let longest = BannerFacts {
            workspace: true,
            session_id: None,
            ephemeral: true,
            ..facts(Path::new("/srv/app"), None)
        };
        let at_37 = banner_lines(&longest, 37);
        assert_eq!(
            plain(&at_37),
            [
                VERSION_ROW.to_owned(),
                "agent · not saved · /save keeps it".to_owned(),
                "/srv/app".to_owned(),
            ]
        );
        assert!(at_37[0].starts_with("\x1b[36mι> iota\x1b[0m  \x1b[2mv"));
        assert!(!at_37.iter().any(|l| l.contains('│')));
        let at_38 = plain(&banner_lines(&longest, 38));
        assert_eq!(at_38.len(), 5);
        assert!(at_38[0].starts_with('╭') && at_38[4].starts_with('╰'));
        assert!(at_38.iter().all(|row| str_width(row) == 38), "{at_38:?}");
        // A shorter mode row needs fewer columns: `chat · session <id>` is 27, so 31.
        let f = facts(Path::new("/srv/app"), None);
        assert_eq!(plain(&banner_lines(&f, 30)).len(), 3);
        assert_eq!(plain(&banner_lines(&f, 31)).len(), 5);
        assert_eq!(plain(&banner_lines(&f, 0)).len(), 5);
    }

    /// The threshold follows the mode row: a host's name widens the longest row by ` · in
    /// herdr` (11 columns), so the frame needs 49 columns there and goes at 48 — the row
    /// itself is never cut.
    #[test]
    fn a_named_host_widens_the_frame_threshold() {
        let hosted = BannerFacts {
            workspace: true,
            session_id: None,
            ephemeral: true,
            host: Some("herdr"),
            ..facts(Path::new("/srv/app"), None)
        };
        let at_48 = plain(&banner_lines(&hosted, 48));
        assert_eq!(at_48.len(), 3, "{at_48:?}");
        assert_eq!(at_48[1], "agent · not saved · /save keeps it · in herdr");
        let at_49 = plain(&banner_lines(&hosted, 49));
        assert_eq!(at_49.len(), 5, "{at_49:?}");
        assert!(at_49.iter().all(|row| str_width(row) == 49), "{at_49:?}");
        assert_eq!(
            inside(&at_49[2]),
            "agent · not saved · /save keeps it · in herdr"
        );
    }

    /// A path longer than the columns inside the frame is cut in the middle, both ends kept,
    /// so the row never wraps and the card never exceeds the terminal: at 80 columns the
    /// directory gets 76 and the card is exactly 80 wide, at 40 it gets 36. Bare (under the
    /// 31 the mode row needs), it gets the whole width; a wide terminal leaves it whole, and
    /// an unknown width cuts nothing.
    #[test]
    fn a_long_directory_row_is_cut_inside_the_frame() {
        let long = Path::new(
            "/Volumes/build/agents/workspaces/2026-09/a-project-with-a-very-long-name/packages/cli",
        );
        let at_80 = plain(&banner_lines(&facts(long, None), 80));
        assert_eq!(
            at_80[3],
            "│ /Volumes/build/agents/workspaces/2026…ect-with-a-very-long-name/packages/cli │"
        );
        assert!(at_80.iter().all(|row| str_width(row) == 80), "{at_80:?}");
        // A narrower card cuts deeper: the frame's four columns are always kept.
        let at_40 = plain(&banner_lines(&facts(long, None), 40));
        assert_eq!(at_40[3], "│ /Volumes/build/ag…-name/packages/cli │");
        assert!(at_40.iter().all(|row| str_width(row) == 40), "{at_40:?}");
        // Bare: the rows alone, the directory at the terminal's width.
        let at_30 = plain(&banner_lines(&facts(long, None), 30));
        assert_eq!(at_30.len(), 3);
        assert_eq!(at_30[2], "/Volumes/build…me/packages/cli");
        assert_eq!(str_width(&at_30[2]), 30);
        // A path that fits is left whole; an unknown width cuts nothing.
        let whole = |w: u16| plain(&banner_lines(&facts(long, None), w))[3].clone();
        assert!(whole(200).ends_with("/packages/cli │") && !whole(200).contains('…'));
        assert!(!whole(0).contains('…'));
        assert_eq!(str_width(&whole(0)), 85 + 4);
    }

    /// The skill-discovery warnings ride under the banner; with no overlay there are none.
    #[test]
    fn overlay_warnings_need_an_overlay() {
        assert!(super::overlay_warnings(None).is_empty());
    }
}
