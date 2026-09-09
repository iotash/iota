//! `wrap_ansi`/`sgr_carry`/`clip_line` goldens + CJK width-ruler vectors
//! (internal/ui/clip.go, `internal/ui/model_test.go`, internal/textwidth, spike G3).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::markdown::hyperlink;
use iota::text::ansi::{
    ansi_len, ansi_width, clip_line, sgr_carry, strip_sgr, truncate_ansi, wrap_ansi, wrap_by_width,
};
use iota::text::width::{graphemes, rune_width, str_width, truncate_cols};

// Go: internal/ui/model_test.go:1264 — rows produced by wrapANSI are self-contained:
// each continuation row re-opens the SGR state its line had at the break.
#[test]
fn test_wrap_ansi_carries_style() {
    let rows = wrap_ansi(
        "\x1b[2mfaint methods list that wraps across rows\x1b[0m",
        16,
    );
    assert!(rows.len() >= 3, "expected >=3 rows, got {rows:?}");
    for (i, r) in rows[1..].iter().enumerate() {
        assert!(
            r.starts_with("\x1b[2m"),
            "continuation row {} lost the faint state: {r:?}",
            i + 1
        );
    }

    // Reset mid-line stops the carry; combined params ("0;7") clear-then-set.
    let rows = wrap_ansi("\x1b[7mrev\x1b[0m plain tail that wraps onward", 12);
    for (i, r) in rows[1..].iter().enumerate() {
        assert!(
            !r.contains("\x1b[7m"),
            "row {} re-opened style past its reset: {r:?}",
            i + 1
        );
    }
    assert_eq!(sgr_carry("", "\x1b[0;7mx"), "\x1b[7m");
    assert_eq!(sgr_carry("\x1b[2m", "a\x1b[36mb"), "\x1b[2;36m");

    // CJK-aware: no row exceeds the column budget.
    for r in wrap_ansi(
        "\x1b[2m方法 tools/list tools/call 中文继续中文继续\x1b[0m",
        10,
    ) {
        let w = str_width(&strip_sgr(&r));
        assert!(w <= 10, "row overflows budget: {w} cols {r:?}");
    }
}

// Go: internal/ui/clip.go:59-105 sgrCarry — parameter-order preservation, resets,
// extended sequences intact, non-SGR CSI ignored.
#[test]
fn test_sgr_carry_goldens() {
    assert_eq!(sgr_carry("", "\x1b[1mx\x1b[36my"), "\x1b[1;36m");
    assert_eq!(sgr_carry("\x1b[1m", "x\x1b[0my"), "");
    assert_eq!(sgr_carry("\x1b[1m", "x\x1b[my"), "");
    assert_eq!(sgr_carry("", "\x1b[38;2;10;20;30mx"), "\x1b[38;2;10;20;30m");
    assert_eq!(sgr_carry("\x1b[1m", "x\x1b[2Ky"), "\x1b[1m"); // CSI but not SGR
    assert_eq!(sgr_carry("", "plain"), "");
}

// Go: internal/ui/clip.go:12-37 clipLine — visible cols [start, start+width), escapes
// always kept, a wide rune straddling a boundary dropped.
#[test]
fn test_clip_line_goldens() {
    assert_eq!(clip_line("abcdef", 1, 3), "bcd");
    assert_eq!(clip_line("a中b", 0, 2), "a"); // 中 straddles the right boundary
    assert_eq!(clip_line("中文", 2, 2), "文");
    assert_eq!(clip_line("中文", 1, 2), ""); // both runes straddle a bound
    assert_eq!(clip_line("\x1b[2mabc\x1b[0m", 1, 1), "\x1b[2mb\x1b[0m");
    assert_eq!(clip_line("abc", 0, 0), "");
}

// Go: chat/userblock_test.go:9 — the PLAIN wrapper (SGR-unaware, CJK-aware, embedded
// newlines start a row).
#[test]
fn test_wrap_by_width() {
    let cases: [(&str, usize, &[&str]); 6] = [
        ("", 10, &[""]),
        ("hello", 10, &["hello"]),
        ("hello", 5, &["hello"]),
        ("abcdefg", 3, &["abc", "def", "g"]),
        ("你好吗", 3, &["你", "好", "吗"]),
        ("a你b", 3, &["a你", "b"]),
    ];
    for (input, width, want) in cases {
        assert_eq!(
            wrap_by_width(input, width),
            want,
            "wrap_by_width({input:?}, {width})"
        );
    }
    // Embedded newlines START a row, they are not measured.
    assert_eq!(wrap_by_width("ab\ncd", 10), ["ab", "cd"]);
}

// The ruler vectors: grapheme-cluster string widths with the VS16 rule (uniseg
// parity; internal/textwidth/textwidth.go) and the per-rune seam (spike G3).
#[test]
fn test_width_ruler_vectors() {
    assert_eq!(str_width("hello"), 5);
    assert_eq!(str_width("中文测试"), 8);
    assert_eq!(str_width("• 中文条目"), 10);
    assert_eq!(str_width("⚖\u{FE0F}"), 2); // VS16 cluster measures 2
    assert_eq!(str_width("🌪\u{FE0F}"), 2); // wide base + VS16 stays 2
    assert_eq!(str_width("🌍"), 2);
    assert_eq!(str_width("\u{1F1EA}\u{1F1FA}"), 2); // flag pair = one 2-col cluster
    assert_eq!(str_width("e\u{301}"), 1); // combining mark collapses into the cluster

    assert_eq!(rune_width('中'), 2);
    assert_eq!(rune_width('a'), 1);
    assert_eq!(rune_width('•'), 1);
    assert_eq!(rune_width('⚖'), 1); // a lone rune has no VS16 context
    assert_eq!(rune_width('\n'), 0);

    assert_eq!(graphemes("e\u{301}b").count(), 2);
}

// truncate_cols: display-column truncation + "…", grapheme-safe, ANSI-blind.
#[test]
fn test_truncate_cols() {
    assert_eq!(truncate_cols("hello", 5), "hello");
    assert_eq!(truncate_cols("hello!", 5), "hell…");
    assert_eq!(truncate_cols("你好世界", 6), "你好…");
    assert_eq!(truncate_cols("你好", 3), "你…"); // a wide rune is never split
    assert_eq!(str_width(&truncate_cols("你好世界", 6)), 5);
}

// truncate_ansi: escape-preserving truncation (x/ansi Truncate twin) — content cut to
// max incl. the tail, escapes after the cut still copied.
#[test]
fn test_truncate_ansi() {
    assert_eq!(truncate_ansi("hello", 10, "…"), "hello");
    assert_eq!(truncate_ansi("hello world", 8, "…"), "hello w…");
    assert_eq!(
        truncate_ansi("\x1b[2mhello world\x1b[0m", 8, "…"),
        "\x1b[2mhello w…\x1b[0m"
    );
    assert_eq!(truncate_ansi("你好世界", 5, "…"), "你好…");
    assert_eq!(
        ansi_width(&truncate_ansi("\x1b[2mhello world\x1b[0m", 8, "…")),
        8
    );
}

// ansi_len / ansi_width / strip_sgr: the escape scanners — SGR counted, OSC 8
// zero-width for every ruler, strip_sgr removes SGR ONLY.
#[test]
fn test_escape_scanners() {
    assert_eq!(ansi_len("\x1b[2mx\x1b[0m"), 8);
    assert_eq!(ansi_len("plain"), 0);
    assert_eq!(ansi_width("\x1b[2m中文\x1b[0m"), 4);
    let link = hyperlink("file:///tmp/a.png", "a.png", true);
    assert_eq!(ansi_width(&link), 5); // OSC 8 is zero display width
    assert_eq!(strip_sgr("\x1b[2mx\x1b[0m"), "x");
    let stripped = strip_sgr(&link);
    assert!(
        stripped.contains("\x1b]8;;"),
        "strip_sgr must keep OSC: {stripped:?}"
    );
}
