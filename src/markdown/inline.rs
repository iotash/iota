//! Inline renderer with STYLE-CONTEXT PASSING (markdown.go:479-801; `TUI_DESIGN` §7).
//!
//! Container constructs (bold, italic, link text, heading) recurse with their attribute
//! COMPOSED onto `base`, and every atomic segment renders self-contained with the full
//! composed style — nesting escape sequences instead would let an inner reset cut the
//! outer style mid-line. Leaves stay literal: a code span's content is never re-parsed,
//! and precedence is scan order — link, code, `\$`, `$$` atomic skip, inline math,
//! `***`, `**`/`__`, `*`/`_`, plain. `styled == false` + a plain base emits plain runs
//! verbatim, keeping top-level output byte-identical.

use crate::markdown::link::hyperlink;
use crate::markdown::math;
use crate::markdown::style::Style;

/// Faint decoration style (Go mdDim): quote bars, bullets, rules, URLs.
pub(crate) const DIM: Style = Style {
    bold: false,
    italic: false,
    underline: false,
    faint: true,
    fg: None,
};

/// Heading style by level: H1 bold+underline, every other level plain bold.
/// (Bold+faint "level differentiation" for H3+ was tried and REVERTED: terminals render
/// the 1;2 combination faint-dominant — whole documents looked washed out.)
fn heading_style(level: usize) -> Style {
    if level == 1 {
        Style::default().bold().underline()
    } else {
        Style::default().bold()
    }
}

/// highlightLine twin (markdown.go:411-445): styles one line by its markdown shape —
/// heading (markers dropped, styled by level, inline constructs composed on top),
/// horizontal rule (dim, original untrimmed line), stray list-marker fallback, else
/// inline highlighting.
pub(crate) fn highlight_line(line: &str, color: bool) -> String {
    let trimmed = line.trim();

    // Heading: ## Title → drop the # markers, style the text by level.
    if trimmed.starts_with('#') {
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        let rest = &trimmed[level..];
        if let Some(text) = rest.strip_prefix(' ') {
            return render_inline(text.trim(), heading_style(level), true, color);
        }
    }

    // Horizontal rule: --- or *** or ___ (dim wraps the ORIGINAL untrimmed line).
    if is_horizontal_rule(trimmed) {
        return DIM.render(line, color);
    }

    // Stray list-marker line (Flush fallback only — whole blocks go through the
    // buffered path): normalize the bullet and dim it, style the rest.
    if let Some((marker, rest)) = split_list_marker(line) {
        return format!(
            "{}{}",
            render_list_marker(marker, color),
            highlight_inline(rest, color)
        );
    }

    highlight_inline(line, color)
}

/// highlightInline twin (markdown.go:474-477): inline styling under a plain base.
pub(crate) fn highlight_inline(line: &str, color: bool) -> String {
    render_inline(line, Style::default(), false, color)
}

/// renderInline twin (markdown.go:490-635). See the module doc for the composition and
/// precedence laws; `styled` says whether `base` carries attributes (a plain run under
/// a plain base is emitted verbatim).
pub(crate) fn render_inline(line: &str, base: Style, styled: bool, color: bool) -> String {
    fn flush(out: &mut String, plain: &mut String, base: Style, styled: bool, color: bool) {
        if plain.is_empty() {
            return;
        }
        if styled {
            out.push_str(&base.render(plain, color));
        } else {
            out.push_str(plain);
        }
        plain.clear();
    }

    let runes: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut plain = String::new();
    let mut i = 0;

    while i < runes.len() {
        // Link: [text](url) → styled text, brackets hidden, URL dimmed.
        if runes[i] == '['
            && let Some(text_end) = find_close(&runes, i + 1, ']')
            && text_end > i + 1
            && text_end + 1 < runes.len()
            && runes[text_end + 1] == '('
            && let Some(url_end) = find_close(&runes, text_end + 2, ')')
        {
            let text: String = runes[i + 1..text_end].iter().collect();
            let url: String = runes[text_end + 2..url_end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&hyperlink(
                &url,
                &render_inline(&text, base.fg(6).underline(), true, color),
                color,
            ));
            out.push_str(&base.faint().render(&format!(" ({url})"), color));
            i = url_end + 1;
            continue;
        }

        // Inline code: `code` → styled, backticks hidden. Runs BEFORE the inline-math
        // branch so a code span wins: "`$x$`" stays literal code.
        if runes[i] == '`'
            && let Some(end) = find_close(&runes, i + 1, '`')
        {
            let content: String = runes[i + 1..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&base.fg(6).render(&content, color));
            i = end + 1;
            continue;
        }

        // Escaped dollar: "\$" is a literal "$", never a math opener; the emitted "$"
        // can no longer close a span a later "$" might try to form.
        if runes[i] == '\\' && runes.get(i + 1) == Some(&'$') {
            plain.push('$');
            i += 2;
            continue;
        }

        // A "$$" run is a display fence, never inline math: emit both dollars
        // atomically and skip past them (advancing by one would mis-scan a one-line
        // "$$x$$" into inline "$x$").
        if runes[i] == '$' && runes.get(i + 1) == Some(&'$') {
            plain.push_str("$$");
            i += 2;
            continue;
        }

        // Inline math: $...$ or \(...\), delimiters hidden, body styled like inline
        // code (cyan). The body transform is a plain call into the math engine
        // (markdown.go:572 `mathtext.ApproxInline(body)`; DESIGN D16 step 1).
        if let Some((body, end)) = math::find_inline_math(&runes, i) {
            flush(&mut out, &mut plain, base, styled, color);
            let approx = crate::mathtext::approx_inline(&body);
            out.push_str(&base.fg(6).render(&approx, color));
            i = end;
            continue;
        }

        // Bold italic: ***text*** → both attributes composed. Without this branch the
        // ** pair would match greedily inside the *** run and orphan the third star.
        if i + 2 < runes.len()
            && runes[i] == '*'
            && runes[i + 1] == '*'
            && runes[i + 2] == '*'
            && let Some(end) = find_triple_close(&runes, i + 3)
        {
            let inner: String = runes[i + 3..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&render_inline(&inner, base.bold().italic(), true, color));
            i = end + 3;
            continue;
        }

        // Bold: **text** / __text__ → bold, markers hidden.
        if runes[i] == '*'
            && runes.get(i + 1) == Some(&'*')
            && let Some(end) = find_double_close(&runes, i + 2, '*')
        {
            let inner: String = runes[i + 2..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&render_inline(&inner, base.bold(), true, color));
            i = end + 2;
            continue;
        }
        if runes[i] == '_'
            && runes.get(i + 1) == Some(&'_')
            && let Some(end) = find_double_close(&runes, i + 2, '_')
        {
            let inner: String = runes[i + 2..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&render_inline(&inner, base.bold(), true, color));
            i = end + 2;
            continue;
        }

        // Italic: *text* / _text_ → italic, markers hidden. Avoid matching list
        // bullets and horizontal rules; "_" only opens at line start or after space.
        if runes[i] == '*'
            && i + 1 < runes.len()
            && runes[i + 1] != '*'
            && runes[i + 1] != ' '
            && let Some(end) = find_close(&runes, i + 1, '*')
            && end > i + 1
        {
            let inner: String = runes[i + 1..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&render_inline(&inner, base.italic(), true, color));
            i = end + 1;
            continue;
        }
        if runes[i] == '_'
            && i + 1 < runes.len()
            && runes[i + 1] != '_'
            && runes[i + 1] != ' '
            && (i == 0 || runes[i - 1].is_whitespace())
            && let Some(end) = find_close(&runes, i + 1, '_')
            && end > i + 1
        {
            let inner: String = runes[i + 1..end].iter().collect();
            flush(&mut out, &mut plain, base, styled, color);
            out.push_str(&render_inline(&inner, base.italic(), true, color));
            i = end + 1;
            continue;
        }

        plain.push(runes[i]);
        i += 1;
    }
    flush(&mut out, &mut plain, base, styled, color);

    out
}

/// stripInlineMarkdown twin (markdown.go:639-691): removes `` ` ``, `**`, `__`, `*`,
/// `_` pairs (same close-finding rules as the renderer, no styling) — used for table
/// header cells and cell width measurement.
pub(crate) fn strip_inline_markdown(line: &str) -> String {
    let runes: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < runes.len() {
        if runes[i] == '`'
            && let Some(end) = find_close(&runes, i + 1, '`')
        {
            out.extend(&runes[i + 1..end]);
            i = end + 1;
            continue;
        }
        if runes[i] == '*'
            && runes.get(i + 1) == Some(&'*')
            && let Some(end) = find_double_close(&runes, i + 2, '*')
        {
            out.extend(&runes[i + 2..end]);
            i = end + 2;
            continue;
        }
        if runes[i] == '_'
            && runes.get(i + 1) == Some(&'_')
            && let Some(end) = find_double_close(&runes, i + 2, '_')
        {
            out.extend(&runes[i + 2..end]);
            i = end + 2;
            continue;
        }
        if runes[i] == '*'
            && i + 1 < runes.len()
            && runes[i + 1] != '*'
            && runes[i + 1] != ' '
            && let Some(end) = find_close(&runes, i + 1, '*')
            && end > i + 1
        {
            out.extend(&runes[i + 1..end]);
            i = end + 1;
            continue;
        }
        if runes[i] == '_'
            && i + 1 < runes.len()
            && runes[i + 1] != '_'
            && runes[i + 1] != ' '
            && (i == 0 || runes[i - 1].is_whitespace())
            && let Some(end) = find_close(&runes, i + 1, '_')
            && end > i + 1
        {
            out.extend(&runes[i + 1..end]);
            i = end + 1;
            continue;
        }
        out.push(runes[i]);
        i += 1;
    }
    out
}

/// First `delim` at or after `start` (Go findClose; `None` = -1).
fn find_close(runes: &[char], start: usize, delim: char) -> Option<usize> {
    (start..runes.len()).find(|&i| runes[i] == delim)
}

/// First doubled `delim` at or after `start` (Go findDoubleClose).
fn find_double_close(runes: &[char], start: usize, delim: char) -> Option<usize> {
    (start..runes.len().saturating_sub(1)).find(|&i| runes[i] == delim && runes[i + 1] == delim)
}

/// First `***` run at or after `start` (Go findTripleClose).
fn find_triple_close(runes: &[char], start: usize) -> Option<usize> {
    (start..runes.len().saturating_sub(2))
        .find(|&i| runes[i] == '*' && runes[i + 1] == '*' && runes[i + 2] == '*')
}

/// splitListMarker twin — the hand parser for Go's `^(\s*(?:[-*+]|\d+[.)]) )` regex:
/// optional whitespace, a bullet (`-`/`*`/`+`) or an ordered token (digits then `.` or
/// `)`), then EXACTLY one required trailing space (part of the marker).
pub(crate) fn split_list_marker(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\x0c' | b'\r') {
        i += 1;
    }
    let end = match bytes.get(i)? {
        b'-' | b'*' | b'+' => i + 1,
        b'0'..=b'9' => {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if !matches!(bytes.get(j), Some(b'.' | b')')) {
                return None;
            }
            j + 1
        }
        _ => return None,
    };
    if bytes.get(end) != Some(&b' ') {
        return None;
    }
    Some((&line[..=end], &line[end + 1..]))
}

/// renderListMarker twin (markdown.go:819-828): unordered bullets become a dim `"• "`,
/// ordered markers are kept but dimmed (trailing space inside the dim span, as Go's
/// `mdDim.Render(bullet)` had it); leading indentation is preserved.
pub(crate) fn render_list_marker(marker: &str, color: bool) -> String {
    let bullet = marker.trim_start_matches([' ', '\t']);
    let indent = &marker[..marker.len() - bullet.len()];
    if bullet.starts_with("- ") || bullet.starts_with("* ") || bullet.starts_with("+ ") {
        format!("{indent}{}", DIM.render("• ", color))
    } else {
        format!("{indent}{}", DIM.render(bullet, color))
    }
}

/// isHorizontalRule twin (markdown.go:1267-1282): trimmed, at least 3 bytes, first
/// byte in `{-,*,_}`, every rune either that character or a space (`"- - -"` counts).
pub(crate) fn is_horizontal_rule(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 {
        return false;
    }
    let ch = s.as_bytes()[0] as char;
    if ch != '-' && ch != '*' && ch != '_' {
        return false;
    }
    s.chars().all(|r| r == ch || r == ' ')
}

/// isHeadingLine twin (markdown.go:450-460): one or more `#` then a space and text.
pub(crate) fn is_heading_line(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return false;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    trimmed[level..].starts_with(' ')
}

/// isBlockLine twin (markdown.go:466-469): a plain-path line that is a block-level
/// element bounded by one blank above and below — a heading or a horizontal rule.
pub(crate) fn is_block_line(line: &str) -> bool {
    is_heading_line(line) || is_horizontal_rule(line.trim())
}

/// isListLine twin (markdown.go:842-847): the marker shape, excluding horizontal rules
/// (`"- - -"` matches the marker regex too).
pub(crate) fn is_list_line(line: &str) -> bool {
    if is_horizontal_rule(line.trim()) {
        return false;
    }
    split_list_marker(line).is_some()
}

#[cfg(test)]
mod tests {
    use super::{highlight_inline, highlight_line, is_list_line, split_list_marker};
    use crate::text::ansi::strip_sgr;

    // Go: internal/markdown/markdown_test.go:370
    #[test]
    fn test_highlight_line_hides_markers() {
        let cases = [
            ("# Title", "Title"),
            ("### Deep heading", "Deep heading"),
            ("- item", "• item"),
            ("* item", "• item"),
            ("1. first", "1. first"),
            ("  - nested", "  • nested"),
        ];
        for (input, want) in cases {
            let got = strip_sgr(&highlight_line(input, true));
            assert_eq!(got, want, "highlight_line({input:?}) visible");
        }
    }

    // Go: internal/markdown/markdown_test.go:1041 — the inline path never eats a "$$"
    // display fence (the atomic-skip guard behind one-line "$$x$$").
    #[test]
    fn test_inline_math_leaves_display_fence() {
        for input in ["$$x$$", "$$x^2$$", "$$a + b$$", "$$"] {
            let got = highlight_inline(input, false);
            assert!(
                got.contains("$$"),
                "inline path ate the display fence: {input:?} -> {got:?}"
            );
        }
    }

    // Hand-parser accept/reject pins for Go's listMarkerRe `^(\s*(?:[-*+]|\d+[.)]) )`
    // (markdown spec §regexes — the marker requires ONE trailing space; ordered
    // accepts '.' and ')').
    #[test]
    fn split_list_marker_pins() {
        assert_eq!(split_list_marker("- item"), Some(("- ", "item")));
        assert_eq!(split_list_marker("  * x"), Some(("  * ", "x")));
        assert_eq!(split_list_marker("\t+ x"), Some(("\t+ ", "x")));
        assert_eq!(split_list_marker("3. third"), Some(("3. ", "third")));
        assert_eq!(split_list_marker("12) x"), Some(("12) ", "x")));
        assert_eq!(split_list_marker("-item"), None); // no trailing space
        assert_eq!(split_list_marker("3.x"), None);
        assert_eq!(split_list_marker("3 x"), None); // digit without . or )
        assert_eq!(split_list_marker(". x"), None);
        assert_eq!(split_list_marker(""), None);
        assert_eq!(split_list_marker("- "), Some(("- ", ""))); // empty rest is fine
        assert!(is_list_line("- item"));
        assert!(!is_list_line("- - -")); // horizontal rule wins
    }
}
