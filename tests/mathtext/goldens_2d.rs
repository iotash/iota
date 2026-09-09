//! The 2D goldens runner (WP62): every formula of `goldens-2d.txt` through
//! `iota::mathtext::render_2d(in, 80)` — `ok` flag, every row byte-exact incl. trailing spaces,
//! every row's `str_width == w=`, no combining mark anywhere.
//!
//! The fixture is the RAW output of the real Go package (`internal/mathtext.Render2D`), generated
//! by the T3 reader from a throwaway copy compiled outside the Go tree, so it is the byte-for-byte
//! contract — it covers everything `layout_test.go`/`p3fix_test.go` pin PLUS the untested corners
//! (ragged matrices, nested integrals, scripts on fractions, every fallback quirk).

use iota::mathtext::render_2d;
use iota::text::width::str_width;

/// One row of a golden block: its text and the display width Go measured for it.
struct GoldenRow {
    /// The row verbatim, trailing spaces included.
    text: String,
    /// The `w=` Go's uniseg reported.
    width: usize,
}

/// One `=== IN: <src>` / `ok=<bool>` / `|row|  (w=N)`… record of the fixture.
struct Golden {
    /// 1-based line number of the `=== IN:` row, for the failure report.
    line: usize,
    /// The LaTeX input.
    input: String,
    /// The `ok` flag `Render2D` returned.
    ok: bool,
    /// The block rows, in order.
    rows: Vec<GoldenRow>,
}

impl Golden {
    /// The whole expected block — the rows as `Render2D` joined them.
    fn block(&self) -> String {
        self.rows
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Parses a `|<row>|  (w=N)` line into its text and width.
fn parse_row(line: &str, at: usize) -> GoldenRow {
    let rest = line
        .strip_prefix('|')
        .unwrap_or_else(|| panic!("goldens-2d.txt:{at}: {line:?} does not start with '|'"));
    let cut = rest
        .rfind("|  (w=")
        .unwrap_or_else(|| panic!("goldens-2d.txt:{at}: {line:?} has no '|  (w=N)' tail"));
    let width: usize = rest[cut + "|  (w=".len()..]
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("goldens-2d.txt:{at}: {line:?} does not end with ')'"))
        .parse()
        .unwrap_or_else(|e| panic!("goldens-2d.txt:{at}: bad width in {line:?}: {e}"));
    GoldenRow {
        text: rest[..cut].to_owned(),
        width,
    }
}

/// Parses `goldens-2d.txt`: `=== IN: <src>`, then `ok=<bool>`, then one `|row|  (w=N)` per row of
/// the block, until the next `=== IN:`.
fn goldens() -> Vec<Golden> {
    let body = crate::fixture("goldens-2d.txt");
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<Golden> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let head = lines[i];
        let input = head.strip_prefix("=== IN: ").unwrap_or_else(|| {
            panic!(
                "goldens-2d.txt:{}: {head:?} is not an `=== IN: ` row",
                i + 1
            )
        });
        let flag = lines
            .get(i + 1)
            .unwrap_or_else(|| panic!("goldens-2d.txt:{}: `=== IN:` with no `ok=` row", i + 1));
        let ok = match *flag {
            "ok=true" => true,
            "ok=false" => false,
            other => panic!(
                "goldens-2d.txt:{}: {other:?} is not `ok=true`/`ok=false`",
                i + 2
            ),
        };
        let mut rows = Vec::new();
        let mut j = i + 2;
        while j < lines.len() && !lines[j].starts_with("=== IN: ") {
            rows.push(parse_row(lines[j], j + 1));
            j += 1;
        }
        assert!(
            !rows.is_empty(),
            "goldens-2d.txt:{}: {input:?} has no block rows",
            i + 1
        );
        out.push(Golden {
            line: i + 1,
            input: input.to_owned(),
            ok,
            rows,
        });
        i = j;
    }
    out
}

/// Every fixture record must round-trip byte-exactly through `render_2d(in, 80)` — the `ok` flag
/// and the block alike. The FIRST mismatch is reported with the input, the expected and the actual
/// block, each row fenced in `|…|` so trailing spaces are visible.
#[test]
fn display_goldens_match_go() {
    let cases = goldens();
    assert!(
        cases.len() >= 128,
        "goldens-2d.txt shrank to {} records",
        cases.len()
    );
    let total = cases.len();
    let mut passed = 0usize;
    for g in &cases {
        let (block, ok) = render_2d(&g.input, 80);
        assert!(
            ok == g.ok && block == g.block(),
            "2D golden mismatch at goldens-2d.txt:{}\n  IN:   |{}|\n  want: ok={}\n{}\n  got:  ok={ok}\n{}\n  ({passed}/{total} records passed before this one)",
            g.line,
            g.input,
            g.ok,
            fence(&g.block()),
            fence(&block),
        );
        passed += 1;
    }
    assert_eq!(passed, total, "every golden record must pass");
}

/// Renders a block one `|row|` per line for the failure report, trailing spaces visible.
fn fence(block: &str) -> String {
    block
        .split('\n')
        .map(|r| format!("        |{r}|"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The box invariant across the whole corpus: every row of every block measures exactly the width
/// Go's uniseg reported, and every row of one block measures the same — the equal-row-width rule
/// the layout is built on (`box.go:21-24`), asserted through OUR ruler.
#[test]
fn display_goldens_rows_measure_their_width() {
    for g in goldens() {
        let (block, _) = render_2d(&g.input, 80);
        let got: Vec<&str> = block.split('\n').collect();
        assert_eq!(
            got.len(),
            g.rows.len(),
            "goldens-2d.txt:{}: {:?} rendered {} rows, want {}",
            g.line,
            g.input,
            got.len(),
            g.rows.len()
        );
        for (i, want) in g.rows.iter().enumerate() {
            assert_eq!(
                str_width(got[i]),
                want.width,
                "goldens-2d.txt:{}: {:?} row {i} = |{}| measures {}, want w={}",
                g.line,
                g.input,
                got[i],
                str_width(got[i]),
                want.width
            );
        }
        let widths: Vec<usize> = got.iter().map(|r| str_width(r)).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "goldens-2d.txt:{}: {:?} rows have unequal widths {widths:?}:\n{block}",
            g.line,
            g.input
        );
    }
}

/// No block the engine returns ever carries a combining mark (U+0300..=U+036F) — the hard rule of
/// the whole renderer, asserted over the full corpus rather than the handful of Go inputs that
/// spell it out (`render_test.go:55,146`).
#[test]
fn display_goldens_are_mark_free() {
    for g in goldens() {
        let (block, _) = render_2d(&g.input, 80);
        for c in block.chars() {
            let u = c as u32;
            assert!(
                !(0x0300..=0x036F).contains(&u),
                "goldens-2d.txt:{}: {:?} emitted combining mark U+{u:04X}:\n{block}",
                g.line,
                g.input
            );
        }
    }
}

/// The `width` argument is advisory (`render.go:26`): a narrow budget changes nothing, and a block
/// wider than it is still returned with `ok = true`.
#[test]
fn display_goldens_ignore_the_width_budget() {
    for g in goldens() {
        assert_eq!(
            render_2d(&g.input, 5),
            render_2d(&g.input, 80),
            "goldens-2d.txt:{}: {:?} changed with the width budget",
            g.line,
            g.input
        );
    }
}
