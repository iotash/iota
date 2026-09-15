//! The fenced code block: its buffering state ([`CodeBlock`], markdown.go:219-249) and the
//! highlightCode twin (markdown.go:1477-1543) routed through the
//! [`crate::markdown::highlight::CodeHighlighter`] seam (T-09). The laws: no-color mode is
//! escape-free by construction; a bare fence (no language) NEVER carries color
//! sequences — Go's plaintext lexer would paint the whole block the style's Text
//! color (monokai near-white, invisible on a light background) while the terminal's
//! default foreground is readable by definition; every rendered line (blank ones
//! included) gets the uniform 2-space indent.

use crate::markdown::blocks::close_view;
use crate::markdown::blocks::math::MathBlock;
use crate::markdown::{PreviewHandle, RenderOptions};

/// The buffering code preview label (markdown.go:1372-1377) — U+2026 ellipsis.
pub(crate) fn code_label(lang: &str) -> String {
    if lang.is_empty() {
        "rendering code…".to_owned()
    } else {
        format!("rendering code ({lang})…")
    }
}

/// A fenced code block (markdown.go:219-249): the language tag of the opening fence and
/// the raw lines up to the closing one.
pub(crate) struct CodeBlock {
    lang: String,
    lines: Vec<String>,
    view: Option<Box<dyn PreviewHandle>>,
    /// The display-math block a fence line interrupted. Go's dispatch checks the fence
    /// FIRST and opens the code block without closing an open `$$` block, so the formula
    /// is open again once the fence closes — including at the end of input, where
    /// markdown.go:379-399 renders the fence alone and leaves `inMath` set. Kept as written.
    interrupted: Option<MathBlock>,
}

impl CodeBlock {
    /// The language tag of an opening fence line: what follows the backticks, trimmed.
    pub(crate) fn lang_of(fence: &str) -> String {
        fence
            .trim()
            .strip_prefix("```")
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    /// An empty block just opened by a fence tagged `lang`, with the preview the Writer
    /// opened for it and the display-math block the fence interrupted, if any.
    pub(crate) fn new(
        lang: String,
        view: Option<Box<dyn PreviewHandle>>,
        interrupted: Option<MathBlock>,
    ) -> Self {
        Self {
            lang,
            lines: Vec::new(),
            view,
            interrupted,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        self.lines.push(line.to_owned());
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    /// Takes back the display-math block the fence interrupted, if any.
    pub(crate) fn take_interrupted(&mut self) -> Option<MathBlock> {
        self.interrupted.take()
    }

    /// Closes the preview and renders the block; the output already carries its
    /// trailing newline (the indentCode shape).
    pub(crate) fn render(mut self, opts: RenderOptions) -> String {
        close_view(&mut self.view);
        let code = self.lines.join("\n");
        render_code(&code, &self.lang, opts)
    }
}

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
