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
    use super::{diff_lang, highlight_diff_line, parse_diff_rows, render_diff};
    use crate::repl::styles::diff_code_theme;

    // Go: chat/compose_test.go:787 TestParseDiffRows — hunk headers translate into the
    // line-number gutter: additions and context carry new-file numbers, deletions
    // old-file numbers, and a "⋮" row marks the boundary between hunks.
    #[test]
    fn test_parse_diff_rows() {
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

    // Go: chat/diff.go:73-93 — a fresh file's "-0,0" normalizes to 1; junk headers keep
    // the running counters.
    #[test]
    fn test_parse_hunk_header_normalizes_zero() {
        let rows = parse_diff_rows(&["@@ -0,0 +1,2 @@", "+a", "+b"]);
        assert_eq!(rows[0].num, 1);
        assert_eq!(rows[1].num, 2);
        let rows = parse_diff_rows(&["@@ junk @@", "+a"]);
        assert_eq!(rows[0].num, 1, "unparseable header keeps counters");
    }

    // Go: chat/diff.go:110-118 `diffLexer` — the grammar token comes from the artifact
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
}
