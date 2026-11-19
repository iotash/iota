#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP54 Browser-panel suite (internal/ui/`model_test.go:841` + `search_test.go:354`).
//!
//! The T2 panel kind's laws: the listing shape (`"../"` first, then name-sorted
//! directories, then files; dot-hidden entries skipped — tabbed.go:259-289), Enter as
//! descend-vs-commit (a directory reloads the panel and does NOT close the surface; a
//! file records its path and falls through to commit-all), and the descend-clears-the-
//! filter rule — the query was aimed at the directory being left, so carrying it into
//! the new one would silently hide half of it.
//!
//! The engine is crate-private by design (`TUI_CONTRACTS` §5), so these tests live in-file
//! (formerly a `#[path]`-mounted `tests/browser.rs` of the terminal crate; merged 2026-09-02).

use std::path::{Path, PathBuf};

use crate::text::ansi::{ansi_width, strip_sgr};
use crate::ui::facade::Panel;
use crate::ui::render::theme::{FAINT, RESET};
use crossterm::event::KeyCode;

use crate::ui::testutil::{Surf, ch, closed, key};

use crate::ui::surface::search::SearchMode;

fn browser(dir: &Path, search: bool) -> Panel {
    Panel::browser("Add".to_owned(), dir.to_path_buf()).with_search(search)
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write fixture file");
}

// --- descend vs commit ------------------------------------------------------

// Enter on a directory
// descends (no commit); Enter on a file commits with the chosen path.
#[test]
fn enter_descends_into_a_directory_and_commits_a_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    write(&sub.join("pick.txt"), "x");

    let mut s = Surf::open(vec![browser(root.path(), false)]);
    assert_eq!(s.names(), ["../", "sub/"]);

    // entries: ../, sub/ — move to sub/ and descend.
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Enter)); // `tap` asserts the descend did NOT commit
    assert_eq!(s.names(), ["../", "pick.txt"]);

    // entries now: ../, pick.txt — choose the file.
    s.tap(key(KeyCode::Down));
    let r = closed(s.press(key(KeyCode::Enter)));
    assert!(!r.cancelled);
    assert_eq!(
        PathBuf::from(&r.panels[0].path),
        sub.join("pick.txt"),
        "chosen = {r:?}"
    );
}

/// A directory picker (`Pick::Dir`, `/allow`'s `Add` tab) lists `./` (the directory shown), `../` and the
/// directories — no file. Enter only ever descends (`./` stays put); Space chooses the highlighted directory
/// and commits, `./` choosing the one shown. The hint names both keys. The file mode is untouched by this
/// (`enter_descends_into_a_directory_and_commits_a_file`).
#[test]
fn a_directory_picker_descends_on_enter_and_chooses_on_space() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir_all(sub.join("deep")).expect("mkdir");
    write(&root.path().join("a.txt"), "");
    let picker = |dir: &Path| Panel::dir_browser("Add", dir.to_path_buf());

    let mut s = Surf::open(vec![picker(root.path())]);
    assert_eq!(s.names(), ["./", "../", "sub/"]);
    assert_eq!(
        s.hint(),
        "↑↓ move · Enter open · Space choose · q/Esc cancel"
    );

    // Enter on `./` reloads where it is; Enter on `sub/` descends — neither commits.
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.names(), ["./", "../", "sub/"]);
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.names(), ["./", "../", "deep/"]);
    assert_eq!(s.ps(0).dir, sub);

    // Space on `deep/` chooses it without entering it.
    s.tap(key(KeyCode::Down));
    s.tap(key(KeyCode::Down));
    let r = closed(s.press(ch(' ')));
    assert!(!r.cancelled);
    assert_eq!(PathBuf::from(&r.panels[0].path), sub.join("deep"));

    // Space on `./` chooses the directory shown.
    let mut s = Surf::open(vec![picker(&sub)]);
    let r = closed(s.press(ch(' ')));
    assert_eq!(PathBuf::from(&r.panels[0].path), sub);

    // Space in the file browser chooses nothing and keeps the surface open.
    let mut s = Surf::open(vec![browser(root.path(), false)]);
    s.tap(ch(' '));
    assert!(s.ps(0).chosen.is_empty());
}

/// The listing rule: `"../"` first, then name-sorted directories, then name-sorted files;
/// dot-hidden entries never appear (tabbed.go:259-289).
#[test]
fn browser_lists_parent_then_dirs_then_files_skipping_dotfiles() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("zeta")).expect("mkdir");
    std::fs::create_dir(root.path().join("alpha")).expect("mkdir");
    std::fs::create_dir(root.path().join(".hidden")).expect("mkdir");
    write(&root.path().join("b.txt"), "");
    write(&root.path().join("a.txt"), "");
    write(&root.path().join(".secret"), "");

    let mut s = Surf::open(vec![browser(root.path(), false)]);
    assert_eq!(s.names(), ["../", "alpha/", "zeta/", "a.txt", "b.txt"]);

    // The dir header row and the byte-exact hint row (tabbed.go:916-947).
    let plain = s.plain();
    assert!(plain.contains(&root.path().to_string_lossy().into_owned()));
    assert!(
        plain.contains("↑↓ move · ←→ page · Enter open/choose · g/G top/bottom · q/Esc cancel"),
        "hint row wrong:\n{plain:?}"
    );
}

/// The cursor row is the current row of every list: tinted to the edge, the cyan `▸`, the name
/// bold — a directory's faint is for the rows off the cursor only, so the current one is never
/// bold-faint; a file is plain off the cursor.
#[test]
fn the_browser_cursor_row_is_tinted_and_bold_and_never_faint() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("sub")).expect("mkdir");
    write(&root.path().join("a.txt"), "");
    let mut s = Surf::open(vec![browser(root.path(), false)]);
    let rows = s.rows();
    let at = |name: &str| {
        rows.iter()
            .find(|r| strip_sgr(r).trim_end().ends_with(name))
            .cloned()
            .expect(name)
    };
    let cur = at("../");
    assert!(cur.starts_with("\x1b[48;5;237m\x1b[36m"), "{cur:?}");
    assert!(cur.contains("\x1b[1m"), "{cur:?}");
    assert!(!cur.contains(FAINT), "{cur:?}");
    assert_eq!(ansi_width(&cur), 80, "{cur:?}");
    assert_eq!(at("sub/"), format!("  {FAINT}sub/{RESET}"));
    assert_eq!(at("a.txt"), "  a.txt");
}

/// An unreadable directory keeps the panel where it was and renders the OS message —
/// a browser that blanked itself on a permission error would look like an empty folder.
#[test]
fn browser_read_error_keeps_the_directory_and_shows_the_message() {
    let root = tempfile::tempdir().expect("tempdir");
    write(&root.path().join("keep.txt"), "");
    let mut s = Surf::open(vec![browser(root.path(), false)]);
    let before = s.names();

    let missing = root.path().join("gone");
    let slot = &mut s.st.slots[0];
    slot.state.set_dir(&slot.spec, &missing);
    assert_eq!(
        s.names(),
        before,
        "a failed read must not clear the listing"
    );
    assert!(!s.ps(0).error_text.is_empty(), "no error message recorded");
    assert!(
        s.plain().contains(&s.ps(0).error_text),
        "error row not rendered"
    );
}

// --- search + descend -------------------------------------------------------

// The query was
// aimed at the directory being left, so it must not silently hide half of the new one.
#[test]
fn descending_clears_the_filter() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("alpha");
    std::fs::create_dir(&sub).expect("mkdir");
    for i in 0..20 {
        write(&root.path().join(format!("file-{i:02}.txt")), "");
        write(&sub.join(format!("inner-{i:02}.txt")), "");
    }

    let mut s = Surf::open(vec![browser(root.path(), true)]);
    s.tap(ch('/'));
    s.typed("alpha");
    s.tap(key(KeyCode::Enter)); // apply: only the subdirectory remains
    assert_eq!(
        s.ps(0).view.len(),
        1,
        "filter kept the wrong number of rows"
    );

    s.tap(key(KeyCode::Enter)); // descend into it
    assert_eq!(
        s.ps(0).search.mode,
        SearchMode::Off,
        "descending kept the filter"
    );
    assert_eq!(
        s.ps(0).view.len(),
        s.ps(0).entries.len(),
        "the new directory is still filtered"
    );
    assert_eq!(s.ps(0).entries.len(), 21, "../ plus the 20 inner files");
}

/// `'/'` is gated on the UNFILTERED overflow and on the panel's opt-in flag, exactly as
/// on the other row panels (search.go:328-346) — a short listing has nothing to search.
#[test]
fn browser_search_is_gated_by_the_flag_and_by_overflow() {
    let root = tempfile::tempdir().expect("tempdir");
    for i in 0..20 {
        write(&root.path().join(format!("file-{i:02}.txt")), "");
    }
    let mut off = Surf::open(vec![browser(root.path(), false)]);
    off.tap(ch('/'));
    assert_eq!(
        off.ps(0).search.mode,
        SearchMode::Off,
        "'/' opened without the opt-in flag"
    );

    let small = tempfile::tempdir().expect("tempdir");
    write(&small.path().join("only.txt"), "");
    let mut short = Surf::open(vec![browser(small.path(), true)]);
    short.tap(ch('/'));
    assert_eq!(
        short.ps(0).search.mode,
        SearchMode::Off,
        "'/' opened on a listing that fits"
    );

    let mut live = Surf::open(vec![browser(root.path(), true)]);
    live.tap(ch('/'));
    assert_eq!(live.ps(0).search.mode, SearchMode::Typing);
}

/// A filtered browser still commits the UNDERLYING entry: the cursor indexes `entries`,
/// never the filtered view, so the path handed back is the row the user is looking at.
#[test]
fn browser_commits_the_underlying_entry_under_a_filter() {
    let root = tempfile::tempdir().expect("tempdir");
    for i in 0..20 {
        write(&root.path().join(format!("file-{i:02}.txt")), "");
    }
    write(&root.path().join("target.md"), "");

    let mut s = Surf::open(vec![browser(root.path(), true)]);
    s.tap(ch('/'));
    s.typed("target");
    s.tap(key(KeyCode::Enter));
    assert_eq!(s.ps(0).view.len(), 1);
    assert!(
        s.ps(0).cursor > 0,
        "the cursor must keep its original index"
    );

    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(
        PathBuf::from(&r.panels[0].path),
        root.path().join("target.md")
    );
}
