//! Escape-aware string utilities (`TUI_CONTRACTS` §3.2; internal/ui/clip.go). Pure;
//! golden-tested; consumed by `crate::ui` and `crate::repl`.
//!
//! Two scanner shapes are used deliberately: `csi_len_at` recognizes ONLY CSI
//! sequences (`\x1b[` … final byte), the exact Go `ansiLen` twin driving
//! `clip_line`/`sgr_carry`; the width/strip family additionally skips OSC sequences
//! (OSC 8 hyperlinks are zero-width for every ruler).

use crate::text::width::{cluster_width, graphemes, rune_width, str_width};

/// Byte length of the ANSI CSI escape starting at `b[i]`, or 0 (clip.go ansiLen).
fn csi_len_at(b: &[u8], i: usize) -> usize {
    if i + 1 >= b.len() || b[i] != 0x1b || b[i + 1] != b'[' {
        return 0;
    }
    let mut j = i + 2;
    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
        j += 1;
    }
    if j < b.len() {
        j += 1; // include the final byte
    }
    j - i
}

/// Byte length of the CSI or OSC escape starting at `b[i]`, or 0. OSC runs to BEL
/// (`\x07`) or ST (`\x1b\\`); an unterminated sequence extends to the end.
fn escape_len_at(b: &[u8], i: usize) -> usize {
    let n = csi_len_at(b, i);
    if n > 0 {
        return n;
    }
    if i + 1 >= b.len() || b[i] != 0x1b || b[i + 1] != b']' {
        return 0;
    }
    let mut j = i + 2;
    while j < b.len() {
        if b[j] == 0x07 {
            return j + 1 - i;
        }
        if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
            return j + 2 - i;
        }
        j += 1;
    }
    b.len() - i
}

/// Removes every CSI and OSC escape (SGR and OSC 8 hyperlinks alike), leaving the text
/// the user actually sees. Crate-internal companion of [`ansi_width`].
pub(crate) fn strip_all(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let n = escape_len_at(b, i);
        if n > 0 {
            i += n;
            continue;
        }
        // Advance one char (safe: i is always on a char boundary here).
        let ch_len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// CSI/OSC scanner: the total byte count occupied by ANSI escape sequences in `s`.
pub fn ansi_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut total = 0;
    let mut i = 0;
    while i < s.len() {
        let n = escape_len_at(b, i);
        if n > 0 {
            total += n;
            i += n;
        } else {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    total
}

/// `width::str_width` skipping SGR + OSC 8 sequences — the ruler
/// for committed (already styled) lines.
pub fn ansi_width(s: &str) -> usize {
    str_width(&strip_all(s))
}

/// Removes `\x1b[..m` (SGR) sequences only — digits and `;` params, `m` final, the Go
/// `ansiRe` shape. Other escapes (OSC 8 hyperlinks) are kept.
pub fn strip_sgr(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let n = csi_len_at(b, i);
        if n > 0
            && b[i + n - 1] == b'm'
            && b[i + 2..i + n - 1]
                .iter()
                .all(|c| c.is_ascii_digit() || *c == b';')
        {
            i += n;
            continue;
        }
        let ch_len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Escape-preserving truncation (x/ansi Truncate twin): a string whose visible width
/// fits `max` passes through unchanged; otherwise visible content is cut so that
/// content + `tail` fit `max` columns, `tail` is appended at the cut, and every escape
/// sequence after the cut is still copied (a trailing reset survives).
pub fn truncate_ansi(s: &str, max: usize, tail: &str) -> String {
    if ansi_width(s) <= max {
        return s.to_owned();
    }
    let Some(budget) = max.checked_sub(str_width(tail)) else {
        return String::new();
    };
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    let mut ignoring = false;
    let mut i = 0;
    while i < s.len() {
        let esc = escape_len_at(bytes, i);
        if esc > 0 {
            out.push_str(&s[i..i + esc]);
            i += esc;
            continue;
        }
        // Visible run up to the next escape, walked by grapheme cluster.
        let mut end = i;
        while end < s.len() && escape_len_at(bytes, end) == 0 {
            end += s[end..].chars().next().map_or(1, char::len_utf8);
        }
        for cluster in graphemes(&s[i..end]) {
            if ignoring {
                continue;
            }
            let cw = cluster_width(cluster);
            if col + cw > budget {
                ignoring = true;
                out.push_str(tail);
                continue;
            }
            out.push_str(cluster);
            col += cw;
        }
        i = end;
    }
    out
}

/// Visible cols `[start, start+width)`: escapes always kept; a wide rune straddling a
/// boundary dropped (clip.go clipLine — the v1 promptui viewer implementation, ported
/// for the View panel's horizontal pan).
pub fn clip_line(s: &str, start: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut col = 0;
    let mut i = 0;
    while i < s.len() {
        let esc = csi_len_at(bytes, i);
        if esc > 0 {
            out.push_str(&s[i..i + esc]);
            i += esc;
            continue;
        }
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        let rw = rune_width(ch);
        if col >= start && col + rw <= start + width {
            out.push(ch);
        }
        col += rw;
        i += ch.len_utf8();
    }
    out
}

/// Hard wrap (ANSI+CJK-aware, x/ansi Hardwrap twin with `preserveSpace`) then re-emit
/// the SGR state active at each row boundary so every row renders correctly in
/// isolation (clip.go wrapANSI): a viewport clipped mid-line must not lose the line's
/// styling when the row that opened it scrolls out of view.
pub fn wrap_ansi(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = hardwrap(s, width);
    let mut state = String::new();
    for (i, row) in rows.iter_mut().enumerate() {
        // The carry folds the ORIGINAL row (before prefixing), exactly like Go's
        // range-copy semantics — otherwise the re-emitted prefix would double.
        let original = row.clone();
        if i > 0 && !state.is_empty() {
            *row = format!("{state}{original}");
        }
        state = sgr_carry(&state, &original);
    }
    rows
}

/// Hard-wraps into rows of at most `width` columns, breaking word boundaries,
/// preserving escapes (zero width) and leading spaces, grapheme-cluster measured —
/// the x/ansi `Hardwrap(s, width, true)` twin restricted to what wrapANSI feeds it.
fn hardwrap(s: &str, width: usize) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut col = 0;
    let mut i = 0;
    while i < s.len() {
        let esc = escape_len_at(bytes, i);
        if esc > 0 {
            row.push_str(&s[i..i + esc]);
            i += esc;
            continue;
        }
        let mut end = i;
        while end < s.len() && escape_len_at(bytes, end) == 0 {
            end += s[end..].chars().next().map_or(1, char::len_utf8);
        }
        for cluster in graphemes(&s[i..end]) {
            if cluster == "\n" {
                rows.push(std::mem::take(&mut row));
                col = 0;
                continue;
            }
            let cw = cluster_width(cluster);
            if col + cw > width {
                rows.push(std::mem::take(&mut row));
                col = 0;
            }
            row.push_str(cluster);
            col += cw;
        }
        i = end;
    }
    rows.push(row);
    rows
}

/// Merge a row's SGR params into the accumulated open-state string (one merged SGR
/// sequence, or `""` when reset) in order; `""`/`"0"` resets; a leading `"0;rest"`
/// clears then applies; extended sequences (`38;2;r;g;b`, `38;5;n`) survive intact
/// (clip.go sgrCarry).
pub fn sgr_carry(state: &str, row: &str) -> String {
    let mut state = state.to_owned();
    let b = row.as_bytes();
    let mut i = 0;
    while i < row.len() {
        let n = csi_len_at(b, i);
        if n == 0 {
            i += row[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let seq = &row[i..i + n];
        i += n;
        if !seq.ends_with('m') {
            continue; // CSI but not SGR
        }
        let mut params = &seq[2..seq.len() - 1];
        if params.is_empty() || params == "0" {
            state.clear();
            continue;
        }
        // A leading reset clears the state before the rest applies ("0;7").
        if let Some(rest) = params.strip_prefix("0;") {
            state.clear();
            params = rest;
        }
        if state.is_empty() {
            state = format!("\x1b[{params}m");
        } else {
            state = format!("{};{params}m", &state[..state.len() - 1]);
        }
    }
    state
}

/// PLAIN wrapper (SGR-unaware; `UserBlock` + composer echo only — never conflate with
/// [`wrap_ansi`]). Hard-wraps on rune boundaries, CJK-aware (a wide rune is never
/// split); embedded newlines START a row, they are not measured
/// (chat/replay.go wrapByWidth).
pub fn wrap_by_width(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in s.split('\n') {
        let mut row = String::new();
        let mut cur = 0;
        for r in line.chars() {
            let rw = rune_width(r);
            if cur + rw > width {
                rows.push(std::mem::take(&mut row));
                cur = 0;
            }
            row.push(r);
            cur += rw;
        }
        rows.push(row);
    }
    rows
}
