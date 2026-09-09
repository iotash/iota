//! Integration tests of the mathtext engine (internal/mathtext) over the reader-generated goldens
//! under `tests/fixtures/mathtext/` — one binary per area (docs/MERGE-PLAN.md §2; `T3_TEST_PLAN` §1).
//! `goldens_inline`/`widths` are WP61's, `goldens_2d` is WP62's; this file only checks the fixtures
//! are where the runners expect them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod goldens_2d;
mod goldens_inline;
mod widths;

/// The fixture directory the three runners read.
pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/mathtext/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// The three golden files are checked in verbatim (`T3_CONTRACTS` §2.9) and non-empty.
#[test]
fn fixtures_are_checked_in() {
    for (name, marker) in [
        ("goldens-2d.txt", "=== IN: "),
        ("goldens-inline.txt", "=== IN: "),
        ("glyph-widths.txt", " w="),
    ] {
        let body = fixture(name);
        assert!(body.contains(marker), "{name} lacks its {marker:?} rows");
    }
}
