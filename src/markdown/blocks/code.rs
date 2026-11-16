//! The fenced code block: its buffering state ([`CodeBlock`], markdown.go:219-249) and the
//! highlightCode twin (markdown.go:1477-1543), highlighted a line at a time as the block streams
//! (`highlight::LineHighlighter`, the syntect pipeline behind the T-09 seam). The laws: no-color mode is
//! escape-free by construction; a bare fence (no language) NEVER carries color
//! sequences — Go's plaintext lexer would paint the whole block the style's Text
//! color (monokai near-white, invisible on a light background) while the terminal's
//! default foreground is readable by definition; every rendered line (blank ones
//! included) gets the uniform 2-space indent.

use crate::markdown::RenderOptions;
use crate::markdown::fragment::{Fragment, Source};
use crate::markdown::highlight::LineHighlighter;
use crate::text::ansi::{ansi_width, wrap_ansi};

/// A fenced code block (markdown.go:219-249): the raw lines up to the closing fence, and — when
/// the block is colored and its language known — those lines highlighted as they arrived, so the
/// block can be shown at any point while it streams (tui-plan S7b).
pub(crate) struct CodeBlock {
    lines: Vec<String>,
    /// The highlighter and its output so far; `None` renders the raw lines (no color, a bare
    /// fence, an unknown language, or a grammar that failed on some line).
    hl: Option<(LineHighlighter, String)>,
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

    /// An empty block just opened by a fence tagged `lang`. A bare fence NEVER carries color
    /// sequences.
    pub(crate) fn new(lang: &str, opts: RenderOptions) -> Self {
        let hl = (opts.color && !lang.is_empty())
            .then(|| LineHighlighter::new(lang, opts.code_theme))
            .flatten()
            .map(|h| (h, String::new()));
        Self {
            lines: Vec::new(),
            hl,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        self.lines.push(line.to_owned());
        if let Some((h, out)) = &mut self.hl
            && !h.line(&format!("{line}\n"), out)
        {
            self.hl = None;
        }
    }

    /// The block as it stands — highlighted once, line by line: only its wrap depends on the
    /// width ([`indent_code`] lays it out at one). `None` until its first line, like the other
    /// blocks with nothing to show.
    pub(crate) fn fragment(&self, opts: RenderOptions) -> Option<Fragment> {
        if self.lines.is_empty() {
            return None;
        }
        let code = match &self.hl {
            Some((_, out)) => out.strip_suffix('\n').unwrap_or(out).to_owned(),
            None => self.lines.join("\n"),
        };
        Some(Fragment::new(Source::Code(code), opts))
    }
}

/// The uniform left margin of every rendered code-block line, (markdown.go codeIndent) — the same two-space rule as math.
pub(crate) const CODE_INDENT: &str = "  ";

/// indentCode twin (markdown.go:1536-1543), laid out at `width`: strip one trailing newline,
/// prefix EVERY line (blank included) with the 2-space indent, rejoin + `"\n"`. Highlighted
/// output resets per token, so the plain indent never inherits a color. A line wider than
/// `width` wraps under the same indent — the gutter holds on every row — with its SGR carried
/// across the break (`wrap_ansi`).
pub(crate) fn indent_code(code: &str, width: usize) -> String {
    let s = code.strip_suffix('\n').unwrap_or(code);
    let room = width.saturating_sub(CODE_INDENT.len()).max(1);
    let mut out = String::with_capacity(s.len() + 8);
    for line in s.split('\n') {
        if ansi_width(line) > room {
            for piece in wrap_ansi(line, room) {
                out.push_str(CODE_INDENT);
                out.push_str(&piece);
                out.push('\n');
            }
        } else {
            out.push_str(CODE_INDENT);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
