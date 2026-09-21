//! THE ruler — the two-rulers law lives in this ONE file (`TUI_CONTRACTS` §3.1).
//!
//! Whole strings measure by GRAPHEME CLUSTERS (unicode-segmentation) with widths summed
//! per cluster (unicode-width), plus the VS16 rule for uniseg parity; single runes use
//! the per-rune tables — the seam for components that walk text one rune at a time
//! (Go internal/textwidth twin). Never hand-roll CJK/emoji ranges and never mix rulers.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of a string by GRAPHEME CLUSTERS: unicode-segmentation clusters,
/// unicode-width sums, plus the VS16 rule — a cluster containing U+FE0F measures 2
/// (uniseg parity; `"⚖️"` = 2 cols). SGR/OSC NOT skipped here (see `ansi::ansi_width`).
pub fn str_width(s: &str) -> usize {
    UnicodeSegmentation::graphemes(s, true)
        .map(cluster_width)
        .sum()
}

/// One grapheme cluster's display width: 2 for any cluster carrying the VS16 emoji
/// presentation selector (the uniseg rule terminals follow), unicode-width otherwise.
pub(crate) fn cluster_width(g: &str) -> usize {
    if g.contains('\u{FE0F}') {
        2
    } else {
        UnicodeWidthStr::width(g)
    }
}

/// Per-rune seam for cursor walks (go-runewidth twin). CJK = 2; control runes = 0.
pub fn rune_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Grapheme iterator re-export point (composer/table cell walks use THIS, never bytes).
pub fn graphemes(s: &str) -> impl Iterator<Item = &str> {
    UnicodeSegmentation::graphemes(s, true)
}

/// Truncate to `max` display columns + `"…"` (grapheme-boundary safe, ANSI-blind).
/// A string already fitting `max` passes through unchanged; otherwise the longest
/// grapheme prefix of at most `max`−1 columns is kept and `…` (1 col) appended
/// (chat/debug.go truncateWidth shape).
pub fn truncate_cols(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out = String::new();
    let mut col = 0;
    for g in UnicodeSegmentation::graphemes(s, true) {
        let w = cluster_width(g);
        if col + w > keep {
            break;
        }
        out.push_str(g);
        col += w;
    }
    out.push('…');
    out
}

/// Truncate to `max` display columns by cutting the MIDDLE out: the head and the tail are
/// kept and joined with `"…"` (1 col), the tail getting the odd column — the shape for a
/// path, whose two ends say the most. Grapheme-boundary safe, ANSI-blind; a string already
/// fitting `max` passes through unchanged, and `max` 0 keeps nothing.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let head_cols = (max - 1) / 2;
    let tail_cols = max - 1 - head_cols;
    let mut head = String::new();
    let mut col = 0;
    for g in UnicodeSegmentation::graphemes(s, true) {
        let w = cluster_width(g);
        if col + w > head_cols {
            break;
        }
        head.push_str(g);
        col += w;
    }
    let mut tail: Vec<&str> = Vec::new();
    col = 0;
    for g in UnicodeSegmentation::graphemes(s, true).rev() {
        let w = cluster_width(g);
        if col + w > tail_cols {
            break;
        }
        tail.push(g);
        col += w;
    }
    head.push('…');
    head.extend(tail.into_iter().rev());
    head
}
