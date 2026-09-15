//! `render_diff` (chat/diff.go): unified hunks become an annotated listing — a dim
//! line-number gutter (the `@@` headers are translated away), background blocks marking
//! additions and deletions, `⋮` rows between hunks, budget truncation with a
//! `"… +N more lines"` tail, overwide rows TRUNCATED never wrapped, tabs expanded to four
//! spaces, and a plain `"NNN + code"` no-color form. The code inside the blocks is
//! highlighted through the SHARED `crate::markdown::highlight::active()` seam — the same
//! pipeline the fenced-code renderer runs, as Go shared its exported `markdown.Highlight`
//! between the two (WP55).

use crate::markdown::CodeTheme;
use crate::text::ansi::{ansi_width, truncate_ansi};

use crate::repl::styles::{diff_code_theme, diff_shades, dim};

/// One display row parsed from unified hunk lines (chat/diff.go `diffRow`): the marker
/// kind (`'+'`, `'-'`, `' '`), the line number it carries (new-file numbering for
/// additions and context, old-file for deletions), and the code text without the marker.
/// A gap row separates hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffRow {
    pub(crate) kind: char,
    pub(crate) num: usize,
    pub(crate) gap: bool,
    pub(crate) text: String,
}

/// Walks unified hunk lines, translating `@@` headers into running line counters
/// (chat/diff.go:37-70). `"\ No newline at end of file"` markers are display noise and
/// drop; anything unrecognized counts as context so a foreign artifact degrades instead
/// of derailing. Tabs expand to spaces — the padding and truncation math needs real
/// display widths.
pub(crate) fn parse_diff_rows(lines: &[&str]) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut old_n, mut new_n) = (1_usize, 1_usize);
    let mut seen_hunk = false;
    let expand = |s: &str| s.replace('\t', "    ");
    for ln in lines {
        if let Some(rest) = ln.strip_prefix("@@") {
            if let Some((o, n)) = parse_hunk_header(rest) {
                old_n = o;
                new_n = n;
            }
            if seen_hunk {
                rows.push(DiffRow {
                    kind: ' ',
                    num: 0,
                    gap: true,
                    text: String::new(),
                });
            }
            seen_hunk = true;
        } else if let Some(rest) = ln.strip_prefix('+') {
            rows.push(DiffRow {
                kind: '+',
                num: new_n,
                gap: false,
                text: expand(rest),
            });
            new_n += 1;
        } else if let Some(rest) = ln.strip_prefix('-') {
            rows.push(DiffRow {
                kind: '-',
                num: old_n,
                gap: false,
                text: expand(rest),
            });
            old_n += 1;
        } else if ln.starts_with('\\') {
            // "\ No newline at end of file"
        } else {
            rows.push(DiffRow {
                kind: ' ',
                num: new_n,
                gap: false,
                text: expand(ln.strip_prefix(' ').unwrap_or(ln)),
            });
            old_n += 1;
            new_n += 1;
        }
    }
    rows
}

/// Extracts the old and new start lines from the rest of an `"@@ -a[,b] +c[,d] @@ …"`
/// header (chat/diff.go:73-93). A zero start (a fresh file's `-0,0`) normalizes to 1 —
/// the first emitted row IS line 1 of its side.
fn parse_hunk_header(rest: &str) -> Option<(usize, usize)> {
    let mut fields = rest.split_whitespace();
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    let start = |s: &str| -> Option<usize> { s.split(',').next()?.parse::<usize>().ok() };
    let o = start(old)?;
    let n = start(new)?;
    Some((o.max(1), n.max(1)))
}

/// Names the grammar for the artifact's file path (chat/diff.go:110-118 `diffLexer`;
/// `""` = no highlighting — plain text inside the blocks). Go asked chroma to match the
/// base name; the shared seam resolves a bare file name itself, so the base name IS the
/// token handed over.
fn diff_lang(title: &str) -> &str {
    title.rsplit(['/', '\\']).next().unwrap_or(title)
}

/// Syntax-highlights one code line through the SHARED highlighter seam and re-arms the
/// given background after every SGR reset, so the block color survives token styling
/// (chat/diff.go:120-131). Go exported `markdown.Highlight` for exactly this so both
/// paths run the same pipeline; [`crate::markdown::highlight::active`] is that export.
///
/// Syntect's foreground-only output arrives here: an `\x1b[48;` from the highlighter
/// would tear a hole in the block, which is the invariant `crate::markdown`'s highlight
/// suite pins.
///
/// Highlighting one line at a time means a construct spanning lines may shade differently
/// than a whole-file pass — acceptable for a diff, and exactly what Go did.
fn highlight_diff_line(text: &str, lang: &str, theme: CodeTheme, bg: &str) -> String {
    let armed = format!("\x1b[0m{bg}");
    if lang.is_empty() {
        return text.replace("\x1b[0m", &armed);
    }
    let highlighted = crate::markdown::highlight::active().highlight(text, lang, theme);
    highlighted
        .trim_end_matches('\n')
        .replace("\x1b[0m", &armed)
}

/// Renders unified hunk lines for the scrollback (chat/diff.go:136-199). `body` is the
/// hunk lines joined with `'\n'` (an `Artifact` diff payload). Rows beyond `budget`
/// collapse into a `"… +N more lines"` tail, and overwide code TRUNCATES (wrapping would
/// wreck the alignment). `width <= 3` (startup, tests) skips width handling; with
/// `color: false` the listing is the plain `"NNN + code"` form.
///
/// `title` is the artifact's file path: its base name names the grammar the code lines
/// are highlighted with (chat/diff.go `diffLexer`), and an empty one means no
/// highlighting at all.
pub fn render_diff(
    title: &str,
    body: &str,
    budget: usize,
    width: usize,
    color: bool,
    dark: bool,
) -> Vec<String> {
    let lines: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let rows = parse_diff_rows(&lines);
    let (shown, extra) = if rows.len() > budget {
        let n = budget.saturating_sub(1);
        (&rows[..n], rows.len() - n)
    } else {
        (&rows[..], 0)
    };

    let max_n = shown.iter().map(|r| r.num).max().unwrap_or(0).max(1);
    let gw = max_n.to_string().len();
    // "  " indent + gutter + space + "+ " marker; the code gets the rest.
    let code_w = width.saturating_sub(2 + gw + 3);
    let (bg_add, bg_del, fg_add, fg_del) = diff_shades(dark);
    let lang = diff_lang(title);
    let theme = diff_code_theme(dark);

    let mut out = Vec::with_capacity(shown.len() + 1);
    for r in shown {
        if r.gap {
            out.push(format!("  {}", dim(&format!("{} ⋮", " ".repeat(gw)))));
            continue;
        }
        let mut text = r.text.clone();
        if width > 3 && code_w > 8 && ansi_width(&text) > code_w {
            text = truncate_ansi(&text, code_w - 1, "…");
        }
        let row = if !color {
            format!("  {:>gw$} {} {}", r.num, r.kind, text)
        } else if r.kind == ' ' {
            format!(
                "  {} {}",
                dim(&format!("{:>gw$}", r.num)),
                dim(&format!("  {text}"))
            )
        } else {
            // The block covers the gutter too: background from the line number through
            // the padded end, with the number and marker in the row's accent color (the
            // code keeps its own foregrounds).
            let (bg, fg) = if r.kind == '-' {
                (bg_del, fg_del)
            } else {
                (bg_add, fg_add)
            };
            let pad = if code_w > 8 {
                " ".repeat(code_w.saturating_sub(ansi_width(&text)))
            } else {
                String::new()
            };
            format!(
                "  {bg}{fg}{:>gw$} {} \x1b[39m{}{pad}\x1b[0m",
                r.num,
                r.kind,
                highlight_diff_line(&text, lang, theme, bg)
            )
        };
        out.push(row);
    }
    if extra > 0 {
        out.push(dim(&format!("  … +{extra} more lines")));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! The parser units and the `render_diff` goldens (the latter formerly `tests/repl/diff.rs`, reached
    //! through a hidden `pub use` re-export; moved in-file 2026-09-15).

    use super::{diff_lang, highlight_diff_line, parse_diff_rows, render_diff};
    use crate::repl::styles::diff_code_theme;
    use crate::text::ansi::{ansi_width, strip_sgr};
    use pretty_assertions::assert_eq;

    // Hunk headers translate into the
    // line-number gutter: additions and context carry new-file numbers, deletions
    // old-file numbers, and a "⋮" row marks the boundary between hunks.
    #[test]
    fn hunk_headers_number_the_gutter_and_a_boundary_row_marks_each_hunk() {
        let rows = parse_diff_rows(&[
            "@@ -3,2 +3,3 @@",
            " ctx",
            "-gone",
            "+new-a",
            "+new-b",
            "@@ -20,1 +21,1 @@",
            "-old",
            "+fresh",
            "\\ No newline at end of file",
        ]);
        let wants: [(char, usize, bool); 7] = [
            (' ', 3, false),
            ('-', 4, false),
            ('+', 4, false),
            ('+', 5, false),
            (' ', 0, true), // hunk boundary
            ('-', 20, false),
            ('+', 21, false),
        ];
        assert_eq!(rows.len(), wants.len(), "{rows:?}");
        for (i, (kind, num, gap)) in wants.into_iter().enumerate() {
            assert_eq!(rows[i].gap, gap, "row {i}: {rows:?}");
            if !gap {
                assert_eq!((rows[i].kind, rows[i].num), (kind, num), "row {i}");
            }
        }
    }

    // A fresh file's "-0,0" normalizes to 1; junk headers keep
    // the running counters.
    #[test]
    fn a_fresh_files_hunk_header_numbers_from_one() {
        let rows = parse_diff_rows(&["@@ -0,0 +1,2 @@", "+a", "+b"]);
        assert_eq!(rows[0].num, 1);
        assert_eq!(rows[1].num, 2);
        let rows = parse_diff_rows(&["@@ junk @@", "+a"]);
        assert_eq!(rows[0].num, 1, "unparseable header keeps counters");
    }

    // The grammar token comes from the artifact
    // title's BASE name (a path prefix must not be part of it), and an empty title means
    // no highlighting at all.
    #[test]
    fn diff_lang_uses_the_base_name() {
        assert_eq!(diff_lang(""), "");
        assert_eq!(diff_lang("main.go"), "main.go");
        assert_eq!(diff_lang("src/a/b/main.rs"), "main.rs");
        assert_eq!(diff_lang("C:\\proj\\main.py"), "main.py");
        assert_eq!(diff_lang("Makefile"), "Makefile");
    }

    // New (T-09, the shared-seam law): a ± row's code is the SHARED
    // `crate::markdown::highlight::active()` output with every reset re-armed to the block
    // background — the same pipeline Go exported `markdown.Highlight` for (syntect's
    // foreground-only bytes), which is exactly what makes it a seam test.
    #[test]
    fn diff_rows_render_through_the_shared_highlight_seam() {
        const BG_ADD: &str = "\x1b[48;5;22m";
        let rows = render_diff(
            "main.rs",
            "@@ -0,0 +1,1 @@\n+let x = 1;",
            24,
            100,
            true,
            true,
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        let want = highlight_diff_line("let x = 1;", "main.rs", diff_code_theme(true), BG_ADD);
        assert!(
            rows[0].contains(&want),
            "the row does not carry the seam's output:\nwant {want:?}\ngot  {:?}",
            rows[0]
        );
        // Whatever the seam emitted, no interior reset may leave the block unarmed and no
        // foreign background may appear.
        let inner = rows[0].strip_suffix("\x1b[0m").unwrap_or(&rows[0]);
        assert!(
            !inner.replace(BG_ADD, "").contains("\x1b[48;"),
            "alien background in the row: {:?}",
            rows[0]
        );
        for (i, part) in inner.split("\x1b[0m").enumerate().skip(1) {
            assert!(
                part.starts_with(BG_ADD),
                "reset {i} did not re-arm the block background: {:?}",
                rows[0]
            );
        }
    }

    // New: an untitled artifact takes the no-highlight path — the code stays verbatim, so
    // a foreign diff never picks up a grammar guessed from nothing.
    #[test]
    fn an_untitled_diff_is_never_highlighted() {
        let rows = render_diff("", "@@ -0,0 +1,1 @@\n+let x = 1;", 24, 100, true, true);
        assert!(
            rows[0].contains("let x = 1;"),
            "code not verbatim: {:?}",
            rows[0]
        );
    }

    // --- the render_diff goldens ---------------------------------------------------

    /// The dark-background shades (chat/diff.go:98-107; the default — WP50 flips light).
    const BG_ADD: &str = "\x1b[48;5;22m";
    const BG_DEL: &str = "\x1b[48;5;52m";
    const FG_ADD: &str = "\x1b[38;5;114m";
    const FG_DEL: &str = "\x1b[38;5;210m";

    // Overwide diff rows TRUNCATE to the screen width; wrapping would wreck the column alignment diffs
    // live by. The plain form is the explicit `color: false`.
    #[test]
    fn overwide_diff_rows_truncate_to_the_screen_width() {
        let body = format!("+{}", "x".repeat(200));
        let rows = render_diff("", &body, 24, 40, false, true);
        assert_eq!(rows.len(), 1);
        let w = ansi_width(&rows[0]);
        assert!(w <= 39, "row width = {w}, must stay under the screen width");
    }

    // With color on, ± rows carry their background blocks, end SGR-self-contained, and a fresh-file
    // "-0,0" hunk numbers from 1.
    #[test]
    fn coloured_diff_rows_carry_their_background_and_end_self_contained() {
        let rows = render_diff(
            "main.go",
            "@@ -0,0 +1,2 @@\n+package main\n+var x = 1",
            24,
            100,
            true,
            true,
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
        for (i, row) in rows.iter().enumerate() {
            assert!(
                row.contains(BG_ADD),
                "row {i} missing the addition background: {row:?}"
            );
            assert!(
                row.ends_with("\x1b[0m"),
                "row {i} must end SGR-self-contained: {row:?}"
            );
        }
        assert!(
            strip_sgr(&rows[0]).contains("1 + package main"),
            "gutter numbering wrong: {:?}",
            strip_sgr(&rows[0])
        );
        assert!(
            strip_sgr(&rows[1]).contains("2 + var x = 1"),
            "gutter numbering wrong: {:?}",
            strip_sgr(&rows[1])
        );
        // Any interior reset must re-arm the background so token styling can't cut the
        // block short (vacuous in T1's plain rows; the WP55 highlighted upgrade rides it).
        for row in &rows {
            let inner = row.strip_suffix("\x1b[0m").unwrap();
            if inner.contains("\x1b[0m") {
                assert!(
                    inner.contains(&format!("\x1b[0m{BG_ADD}")),
                    "token reset not re-armed with the background: {row:?}"
                );
            }
        }
    }

    // A diff row whose content a lexer cannot parse must carry ONLY the block's own background: no
    // alien `\x1b[48;` survives.
    #[test]
    fn a_diff_row_carries_no_alien_background() {
        let rows = render_diff(
            "prompt.js",
            "@@ -1,1 +1,1 @@\n+  重要: 这是发给**开发者**的推荐语（clarity、naturalness）",
            24,
            200,
            true,
            true,
        );
        let stripped = rows[0].replace(BG_ADD, "");
        assert!(
            !stripped.contains("\x1b[48;"),
            "alien background survived in diff row:\n{:?}",
            rows[0]
        );
    }

    // The ± block covers the line-number gutter: the row starts with the block background right after
    // the indent, and the number + marker wear the row's accent color before the code's own foregrounds
    // take over.
    #[test]
    fn the_gutter_sits_inside_the_coloured_block() {
        let rows = render_diff(
            "main.go",
            "@@ -1,1 +1,2 @@\n+package main\n-package old",
            24,
            100,
            true,
            true,
        );
        assert!(
            rows[0].starts_with(&format!("  {BG_ADD}{FG_ADD}")),
            "add row must open with block bg + accent fg over the gutter:\n{:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with(&format!("  {BG_DEL}{FG_DEL}")),
            "del row must open with block bg + accent fg over the gutter:\n{:?}",
            rows[1]
        );
        // The accent yields to the code's own foregrounds after the marker.
        assert!(
            rows[0].contains("\x1b[39m"),
            "accent fg must reset before the code:\n{:?}",
            rows[0]
        );
        assert!(
            strip_sgr(&rows[0]).contains("1 + package main"),
            "gutter layout changed: {:?}",
            strip_sgr(&rows[0])
        );
    }

    // The hunk gap row and the budget tail, byte for byte.
    #[test]
    fn the_hunk_gap_row_and_the_budget_tail_are_byte_exact() {
        let body = "@@ -3,1 +3,1 @@\n-a\n+A\n@@ -20,1 +20,1 @@\n-b\n+B";
        let rows = render_diff("", body, 24, 80, false, true);
        // Rows: -a, +A, gap, -b, +B — the gap renders as the dim "⋮" marker row.
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert_eq!(rows[2], format!("  \x1b[2m{} ⋮\x1b[0m", " ".repeat(2)));

        let mut long = String::from("@@ -0,0 +1,10 @@");
        for i in 0..10 {
            use std::fmt::Write as _;
            let _ = write!(long, "\n+row-{i}");
        }
        let rows = render_diff("", &long, 5, 80, false, true);
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert_eq!(*rows.last().unwrap(), "\x1b[2m  … +6 more lines\x1b[0m");
    }

    // The differ's goldens (go-udiff's byte shape) — the rows the producer feeds into the artifact.
    #[test]
    fn the_unified_differ_keeps_its_goldens() {
        use crate::tool::code::udiff::unified;

        // Equal inputs → the empty string (postDiff posts nothing).
        assert_eq!(unified("a.txt", "a.txt", "same\n", "same\n"), "");

        // A one-line replacement with context, the Go header/count shape.
        assert_eq!(
            unified("a.txt", "a.txt", "one\ntwo\nthree\n", "one\n2\nthree\n"),
            "--- a.txt\n+++ a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );

        // A fresh file diffs against empty content: the odd GNU "-0,0" form.
        assert_eq!(
            unified("n.txt", "n.txt", "", "alpha\nbeta\n"),
            "--- n.txt\n+++ n.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
        );

        // An unterminated final line carries the "\ No newline" marker.
        assert_eq!(
            unified("a", "a", "x\n", "x\ny"),
            "--- a\n+++ a\n@@ -1 +1,2 @@\n x\n+y\n\\ No newline at end of file\n"
        );
    }

    // With colour off the rows carry no escape at all — the shape `render_diff` takes when its caller
    // (the group renderer) feeds it the process decision under `NO_COLOR`.
    #[test]
    fn with_colour_off_the_rows_carry_no_escape() {
        let rows = render_diff(
            "main.go",
            "@@ -0,0 +1,2 @@\n+package main\n+var x = 1",
            24,
            100,
            false,
            true,
        );
        for row in &rows {
            assert!(
                !row.contains("\x1b[") && !row.contains("\x1b]"),
                "an escape sequence with colour off: {row:?}"
            );
        }
        assert_eq!(rows[0].trim(), "1 + package main");
    }
}
