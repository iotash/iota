//! The display-math block (`TUI_CONTRACTS` §3.5; T-08/T-15 CLOSED): its fence recognizers,
//! its buffering state ([`MathBlock`]) and its render.
//!
//! The delimiter logic is load-bearing string parsing: display fences `"$$"` / `"\["` open,
//! `"$$"` / `"\]"` close, one-line forms need `len > 4` + a non-empty inner
//! (internal/mathtext/delim.go:181-213) — those two recognizers LIVE in
//! [`crate::mathtext::delim`] and are re-exported here, so the writer's state machine and its
//! unit tests are untouched (DESIGN D16). The inline scanner, `find_inline_math`, sits in
//! `markdown::inline` next to its one caller: it is the rune-index twin of
//! `mathtext::find_inline` written against markdown.go:740-801, and the brain's "two
//! implementations in lockstep" rule keeps a test on each side.
//!
//! The body transform is a plain call into `mathtext`: [`crate::mathtext::render_2d`]
//! (`MathBlock::render`; the inline half is [`crate::mathtext::approx_inline`] in `inline.rs`).
//! The T1 raw-LaTeX stand-in is gone — both hooks render for real — and `mathtext` never names
//! this module back (Phase 5 PR-4: the `MathRenderer` trait and its one ZST impl are deleted,
//! so `mathtext` is a leaf).

use crate::markdown::PreviewHandle;
use crate::markdown::blocks::close_view;

/// The display-fence recognizers, now owned by [`crate::mathtext::delim`] (DESIGN D16): Go keeps
/// `DisplayOpen`/`IsDisplayClose` in `delim.go:181-219` and the markdown writer calls across.
pub(crate) use crate::mathtext::delim::{display_open, is_display_close};

/// The uniform left margin of rendered display-math rows — the same two-space rule as
/// code blocks, so formulas and code sit on one left rule (markdown.go mathIndent).
const MATH_INDENT: &str = "  ";

/// A display-math block: the raw source lines between the `$$` / `\[` fences (the
/// one-line form is a `MathBlock` of one line that renders at once, no preview).
pub(crate) struct MathBlock {
    lines: Vec<String>,
    view: Option<Box<dyn PreviewHandle>>,
}

impl MathBlock {
    /// The live preview's label while the block buffers.
    pub(crate) const LABEL: &'static str = "rendering math…";

    /// An empty block just opened by a bare fence, with the preview the Writer opened for it.
    pub(crate) fn open(view: Option<Box<dyn PreviewHandle>>) -> Self {
        Self {
            lines: Vec::new(),
            view,
        }
    }

    /// The complete one-line form (`$$…$$` / `\[…\]`): its body, rendered at once, no preview.
    pub(crate) fn one_line(body: String) -> Self {
        Self {
            lines: vec![body],
            view: None,
        }
    }

    pub(crate) fn append(&mut self, line: &str) {
        self.lines.push(line.to_owned());
        if let Some(v) = &mut self.view {
            v.write_raw_line(line);
        }
    }

    /// Renders the buffered display-math block (markdown.go:1433-1455): a
    /// whitespace-only source renders NOTHING (the paid gap credit may remain
    /// consumed); otherwise every row is prefixed by the two-space `MATH_INDENT` and the
    /// block rides `begin_block`/`end_block`. The body transform is the mathtext 2D layout
    /// (markdown.go:1443 `mathtext.Render2D`; DESIGN D16 step 2), which degrades to the cleaned
    /// linear source when the formula cannot be laid out; either way the rows print in normal
    /// color (never dim: dim is decoration-only).
    pub(crate) fn render(mut self, width: usize) -> Option<String> {
        close_view(&mut self.view);
        let src = self.lines.join("\n");
        if src.trim().is_empty() {
            return None; // an empty $$ block renders nothing (mirrors the quote)
        }
        let width = width.saturating_sub(MATH_INDENT.len());
        let (block, _ok) = crate::mathtext::render_2d(&src, width);
        let mut out = String::new();
        for r in block.split('\n') {
            out.push_str(MATH_INDENT);
            out.push_str(r);
            out.push('\n');
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{display_open, is_display_close};

    // Go: internal/mathtext/delim_test.go (DisplayOpen/IsDisplayClose shapes)
    #[test]
    fn display_fence_forms() {
        assert_eq!(display_open("$$"), Some((String::new(), false)));
        assert_eq!(display_open("\\["), Some((String::new(), false)));
        assert_eq!(display_open("$$x^2$$"), Some(("x^2".to_owned(), true)));
        assert_eq!(display_open("\\[ a+b \\]"), Some(("a+b".to_owned(), true)));
        assert_eq!(display_open("$$$$"), None); // empty one-line form
        assert_eq!(display_open("$$ $$"), None); // whitespace-only inner
        assert_eq!(display_open("\\]"), None); // a close is not an opener
        assert_eq!(display_open("text"), None);
        assert!(is_display_close("$$"));
        assert!(is_display_close("  \\]"));
        assert!(!is_display_close("\\["));
        assert!(!is_display_close("$$x$$"));
    }
}
