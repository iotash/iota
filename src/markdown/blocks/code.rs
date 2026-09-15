//! Code-block renderer: the highlightCode twin (markdown.go:1477-1543) routed through
//! the [`crate::markdown::highlight::CodeHighlighter`] seam (T-09). The laws: no-color mode is
//! escape-free by construction; a bare fence (no language) NEVER carries color
//! sequences — Go's plaintext lexer would paint the whole block the style's Text
//! color (monokai near-white, invisible on a light background) while the terminal's
//! default foreground is readable by definition; every rendered line (blank ones
//! included) gets the uniform 2-space indent.

use crate::markdown::RenderOptions;

/// The uniform left margin of every rendered code-block line, matching the buffering
/// preview's indent (markdown.go codeIndent) — the same two-space rule as math.
pub(crate) const CODE_INDENT: &str = "  ";

/// highlightCode twin (markdown.go:1477-1496): no-color and bare-fence inputs take
/// the plain 2-space-indent path with zero escapes; everything else routes through
/// the active [`crate::markdown::highlight::CodeHighlighter`] (syntect, `highlight::active`) and
/// is then indented (T-09).
pub(crate) fn render_code(code: &str, lang: &str, opts: RenderOptions) -> String {
    if !opts.color {
        return indent_code(code);
    }
    if lang.is_empty() {
        return indent_code(code); // a bare fence NEVER carries color sequences
    }
    indent_code(&crate::markdown::highlight::active().highlight(code, lang, opts.code_theme))
}

/// indentCode twin (markdown.go:1536-1543): strip one trailing newline, prefix EVERY
/// line (blank included) with the 2-space indent, rejoin + `"\n"`. Highlighted output
/// resets per token, so the plain indent never inherits a color.
pub(crate) fn indent_code(code: &str) -> String {
    let s = code.strip_suffix('\n').unwrap_or(code);
    let mut out = String::with_capacity(s.len() + 8);
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(CODE_INDENT);
        out.push_str(line);
    }
    out.push('\n');
    out
}
