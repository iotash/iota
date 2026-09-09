#![allow(dead_code)]
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

use crate::text::ansi::strip_sgr;
use crate::ui::facade::{Panel, TabbedResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::surface::search::SearchMode;
use crate::ui::surface::tabbed::PanelState;
use crate::ui::surface::{SurfaceEffect, SurfaceState};

// --- harness ----------------------------------------------------------------

struct Surf {
    st: SurfaceState,
}

impl Surf {
    fn open(panels: Vec<Panel>) -> Self {
        let st = SurfaceState::new(false, panels);
        Self { st }
    }

    fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    fn tap(&mut self, k: KeyEvent) {
        assert!(
            !matches!(self.press(k), SurfaceEffect::Close(_)),
            "key closed the surface unexpectedly"
        );
    }

    fn typed(&mut self, s: &str) {
        for c in s.chars() {
            self.tap(ch(c));
        }
    }

    fn ps(&self) -> &PanelState {
        &self.st.slots[0].state
    }

    fn names(&self) -> Vec<String> {
        self.ps().entries.iter().map(|e| e.name.clone()).collect()
    }

    fn content(&mut self) -> String {
        self.st.render(80).rows.join("\n")
    }

    fn plain(&mut self) -> String {
        strip_sgr(&self.content())
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

fn browser(dir: &Path, search: bool) -> Panel {
    Panel::browser("Add".to_owned(), dir.to_path_buf()).with_search(search)
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write fixture file");
}

// --- descend vs commit ------------------------------------------------------

// Go: internal/ui/model_test.go:841 TestBrowserDescendAndChoose — Enter on a directory
// descends (no commit); Enter on a file commits with the chosen path.
#[test]
fn test_browser_descend_and_choose() {
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
    assert!(!s.ps().error_text.is_empty(), "no error message recorded");
    assert!(
        s.plain().contains(&s.ps().error_text),
        "error row not rendered"
    );
}

// --- search + descend -------------------------------------------------------

// Go: internal/ui/search_test.go:354 TestBrowserDescendClearsFilter — the query was
// aimed at the directory being left, so it must not silently hide half of the new one.
#[test]
fn test_browser_descend_clears_filter() {
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
    assert_eq!(s.ps().view.len(), 1, "filter kept the wrong number of rows");

    s.tap(key(KeyCode::Enter)); // descend into it
    assert_eq!(
        s.ps().search.mode,
        SearchMode::Off,
        "descending kept the filter"
    );
    assert_eq!(
        s.ps().view.len(),
        s.ps().entries.len(),
        "the new directory is still filtered"
    );
    assert_eq!(s.ps().entries.len(), 21, "../ plus the 20 inner files");
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
        off.ps().search.mode,
        SearchMode::Off,
        "'/' opened without the opt-in flag"
    );

    let small = tempfile::tempdir().expect("tempdir");
    write(&small.path().join("only.txt"), "");
    let mut short = Surf::open(vec![browser(small.path(), true)]);
    short.tap(ch('/'));
    assert_eq!(
        short.ps().search.mode,
        SearchMode::Off,
        "'/' opened on a listing that fits"
    );

    let mut live = Surf::open(vec![browser(root.path(), true)]);
    live.tap(ch('/'));
    assert_eq!(live.ps().search.mode, SearchMode::Typing);
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
    assert_eq!(s.ps().view.len(), 1);
    assert!(s.ps().cursor > 0, "the cursor must keep its original index");

    let r = closed(s.press(key(KeyCode::Enter)));
    assert_eq!(
        PathBuf::from(&r.panels[0].path),
        root.path().join("target.md")
    );
}
