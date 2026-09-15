//! Hand-ported unified-diff twin of go-udiff v0.4.1 (`Unified` → `Lines` → `toUnified` →
//! `String`), feeding the `edit_file`/`write_file` display artifact (code.go:661-675; the
//! D-19 lift, T-35). Line-level diff (Myers greedy, budget-capped with a whole-replace
//! fallback — go-udiff's lcs carries the same escape hatch), 3 context lines, hunks
//! merged when ≤ 6 unchanged lines apart, and the exact go-udiff header/row byte shape:
//! `@@ -l[,c] +l[,c] @@` with the count elided at 1 and the odd GNU `-0,0`/`+0,0` form
//! for an empty side, `"\ No newline at end of file"` after an unterminated final line.

/// Unchanged context lines around each hunk (go-udiff `DefaultContextLines`).
const CONTEXT_LINES: usize = 3;

/// Myers search budget; past it the trimmed middle collapses to one replace edit.
const MAX_DIFF_COST: usize = 1000;

/// A unified diff of `old` → `new` labelled `old_label`/`new_label` (go-udiff
/// `Unified`). Equal inputs yield the empty string.
pub fn unified(old_label: &str, new_label: &str, old: &str, new: &str) -> String {
    let a = split_keep(old);
    let b = split_keep(new);
    let edits = line_edits(&a, &b);
    render_unified(old_label, new_label, &a, &b, &edits)
}

/// One line-level replacement: `a[start..end]` becomes `b[repl]`.
struct LineEdit {
    start: usize,
    end: usize,
    repl: std::ops::Range<usize>,
}

/// Splits into lines KEEPING each `'\n'`; a final unterminated line is kept bare
/// (go-udiff `splitLines`; `""` → no lines).
fn split_keep(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Del,
    Ins,
}

/// Line-level edit script between `a` and `b`: common prefix/suffix trimmed, Myers on
/// the middle, contiguous non-equal runs merged into replace edits.
#[allow(clippy::many_single_char_names)] // a/b/p/s are the diff literature's names
fn line_edits(a: &[&str], b: &[&str]) -> Vec<LineEdit> {
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    let am = &a[p..a.len() - s];
    let bm = &b[p..b.len() - s];
    if am.is_empty() && bm.is_empty() {
        return Vec::new();
    }
    let Some(ops) = myers(am, bm) else {
        // Budget exceeded: one replace edit over the whole middle.
        return vec![LineEdit {
            start: p,
            end: p + am.len(),
            repl: p..p + bm.len(),
        }];
    };
    let mut edits = Vec::new();
    let (mut ai, mut bi) = (p, p);
    let mut cur: Option<LineEdit> = None;
    for op in ops {
        match op {
            Op::Eq => {
                if let Some(e) = cur.take() {
                    edits.push(e);
                }
                ai += 1;
                bi += 1;
            }
            Op::Del => {
                let e = cur.get_or_insert(LineEdit {
                    start: ai,
                    end: ai,
                    repl: bi..bi,
                });
                e.end += 1;
                ai += 1;
            }
            Op::Ins => {
                let e = cur.get_or_insert(LineEdit {
                    start: ai,
                    end: ai,
                    repl: bi..bi,
                });
                e.repl.end += 1;
                bi += 1;
            }
        }
    }
    if let Some(e) = cur {
        edits.push(e);
    }
    edits
}

/// Greedy Myers over line slices with a search budget; `None` = budget exceeded.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
// the k-diagonal walk is signed by construction; every index is proven in-range before the cast back
#[allow(clippy::many_single_char_names)] // n/m/k/x/y/d ARE the Myers paper's names
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let cap = (a.len() + b.len()).min(MAX_DIFF_COST);
    let off = cap as isize;
    let width = 2 * cap + 1;
    let mut v = vec![0_isize; width];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found: Option<isize> = None;
    'search: for d in 0..=(cap as isize) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let dfin = found?;
    let mut ops_rev: Vec<Op> = Vec::new();
    let (mut x, mut y) = (n, m);
    let mut d = dfin;
    while d > 0 {
        let v = &trace[d as usize];
        let k = x - y;
        let idx = (k + off) as usize;
        let down = k == -d || (k != d && v[idx - 1] < v[idx + 1]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops_rev.push(Op::Eq);
            x -= 1;
            y -= 1;
        }
        if down {
            ops_rev.push(Op::Ins);
            y -= 1;
        } else {
            ops_rev.push(Op::Del);
            x -= 1;
        }
        d -= 1;
    }
    while x > 0 && y > 0 {
        ops_rev.push(Op::Eq);
        x -= 1;
        y -= 1;
    }
    ops_rev.reverse();
    Some(ops_rev)
}

/// One hunk under assembly (go-udiff `hunk`; row kinds `'-'`, `'+'`, `' '`).
struct Hunk {
    from_line: usize,
    to_line: usize,
    lines: Vec<(char, String)>,
}

/// Appends the equal lines `lines[start..end]` (indices past the end stop the walk) and
/// returns how many were added (go-udiff `addEqualLines`; the caller saturates a
/// would-be-negative `start` to 0, which skips the same rows Go's `i < 0` guard did).
fn add_equal(h: &mut Hunk, lines: &[&str], start: usize, end: usize) -> usize {
    let mut delta = 0;
    for i in start..end {
        if i >= lines.len() {
            return delta;
        }
        h.lines.push((' ', lines[i].to_owned()));
        delta += 1;
    }
    delta
}

/// The go-udiff `toUnified` + `String` pipeline, ported literally over line indices.
fn render_unified(
    old_label: &str,
    new_label: &str,
    a: &[&str],
    b: &[&str],
    edits: &[LineEdit],
) -> String {
    if edits.is_empty() {
        return String::new();
    }
    let gap = CONTEXT_LINES * 2;
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut h: Option<Hunk> = None;
    let mut last = 0_usize;
    let mut to_line = 0_usize;
    for e in edits {
        let start = e.start;
        if h.is_some() && start == last {
            // Direct extension.
        } else if h.is_some() && start <= last + gap {
            // Within range of the previous lines: add the joiners.
            if let Some(hh) = h.as_mut() {
                add_equal(hh, a, last, start);
            }
        } else {
            // A new hunk is needed.
            if let Some(mut hh) = h.take() {
                add_equal(&mut hh, a, last, last + CONTEXT_LINES);
                hunks.push(hh);
            }
            to_line += start - last;
            let mut hh = Hunk {
                from_line: start + 1,
                to_line: to_line + 1,
                lines: Vec::new(),
            };
            let delta = add_equal(&mut hh, a, start.saturating_sub(CONTEXT_LINES), start);
            hh.from_line -= delta;
            hh.to_line -= delta;
            h = Some(hh);
        }
        last = start;
        if let Some(hh) = h.as_mut() {
            for line in &a[e.start..e.end] {
                hh.lines.push(('-', (*line).to_owned()));
            }
            last += e.end - e.start;
            for j in e.repl.clone() {
                hh.lines.push(('+', b[j].to_owned()));
                to_line += 1;
            }
        }
    }
    if let Some(mut hh) = h.take() {
        add_equal(&mut hh, a, last, last + CONTEXT_LINES);
        hunks.push(hh);
    }

    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    for hh in &hunks {
        let (mut from_count, mut to_count) = (0_usize, 0_usize);
        for (kind, _) in &hh.lines {
            match kind {
                '-' => from_count += 1,
                '+' => to_count += 1,
                _ => {
                    from_count += 1;
                    to_count += 1;
                }
            }
        }
        out.push_str("@@");
        push_range(&mut out, '-', hh.from_line, from_count);
        push_range(&mut out, '+', hh.to_line, to_count);
        out.push_str(" @@\n");
        for (kind, content) in &hh.lines {
            out.push(*kind);
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

/// One side of a hunk header (go-udiff `String`): count elided at 1, and the odd GNU
/// `-0,0`/`+0,0` form when a side is empty at line 1 (adding to an empty file).
fn push_range(out: &mut String, sign: char, line: usize, count: usize) {
    use std::fmt::Write as _;
    if count > 1 {
        let _ = write!(out, " {sign}{line},{count}");
    } else if line == 1 && count == 0 {
        let _ = write!(out, " {sign}0,0");
    } else {
        let _ = write!(out, " {sign}{line}");
    }
}

#[cfg(test)]
#[allow(clippy::format_collect)] // fixture builders; clarity over the extra allocation
mod tests {
    use super::unified;

    // The go-udiff byte shape the diff renderer and the artifact tests rely on.
    #[test]
    fn test_unified_equal_is_empty() {
        assert_eq!(unified("a", "a", "same\n", "same\n"), "");
        assert_eq!(unified("a", "a", "", ""), "");
    }

    #[test]
    fn test_unified_replace_with_context() {
        let got = unified("a.txt", "a.txt", "one\ntwo\nthree\n", "one\n2\nthree\n");
        assert_eq!(
            got,
            "--- a.txt\n+++ a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
    }

    #[test]
    fn test_unified_new_file_is_gnu_zero_form() {
        let got = unified("n.txt", "n.txt", "", "alpha\nbeta\n");
        assert_eq!(
            got,
            "--- n.txt\n+++ n.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
        );
    }

    #[test]
    fn test_unified_missing_final_newline_marker() {
        let got = unified("a", "a", "x\n", "x\ny");
        assert_eq!(
            got,
            "--- a\n+++ a\n@@ -1 +1,2 @@\n x\n+y\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn test_unified_far_edits_split_hunks() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l19\n", "L19\n");
        let got = unified("f", "f", &old, &new);
        assert_eq!(
            got.matches("@@").count(),
            4,
            "two hunks (2 markers each):\n{got}"
        );
        // First hunk: change at line 2 with 1 line of leading context (the file starts).
        assert!(got.contains("@@ -1,5 +1,5 @@"), "{got}");
        // Second hunk: change at line 19 with trailing context cut by EOF.
        assert!(got.contains("@@ -16,5 +16,5 @@"), "{got}");
    }

    #[test]
    fn test_unified_near_edits_merge_into_one_hunk() {
        let old: String = (1..=12).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l4\n", "L4\n").replace("l8\n", "L8\n");
        let got = unified("f", "f", &old, &new);
        assert_eq!(got.matches("@@").count(), 2, "one merged hunk:\n{got}");
        assert!(got.contains("-l4\n"), "{got}");
        assert!(got.contains("-l8\n"), "{got}");
    }
}
