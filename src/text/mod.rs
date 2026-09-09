//! Text helpers (tool/agent.go:240-276), Go format reproductions (`%q`, `%v` on floats, `time.Duration`),
//! and — in `width`/`ansi` — THE grapheme width ruler and its escape-aware companion shared by `markdown`,
//! `ui` and `repl` (`TUI_CONTRACTS` §3).

pub mod ansi;
pub mod width;

/// tool/agent.go:240-249: split on `'\n'`, drop exactly one trailing empty segment; `""` → `[]`.
pub(crate) fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// tool/agent.go:267-276: cut at `max` bytes backing up to a char boundary.
pub(crate) fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// Go `%q` for str (ASCII-identical; `format!("{s:?}")`).
pub(crate) fn go_quote(s: &str) -> String {
    format!("{s:?}")
}

/// Go `%v` for float64 in the ranges the CLI prints (3.5 → `"3.5"`, 2.0 → `"2"`, 0.7 → `"0.7"`).
///
/// Reproduces `strconv.FormatFloat(f, 'g', -1, 64)`: the shortest round-trip digits, `%e` form (`1e+06`, `1e-05`)
/// when the decimal exponent is below -4 or at least 6, plain `%f` form otherwise.
pub(crate) fn go_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    // `{:e}` yields the shortest round-trip mantissa and a bare decimal exponent, e.g. `-1.5e-5`.
    let sci = format!("{f:e}");
    let (neg, sci) = match sci.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, sci.as_str()),
    };
    let (mant, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let nd = i32::try_from(digits.len()).unwrap_or(i32::MAX);

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..6).contains(&exp) {
        let dp = exp + 1; // digits before the decimal point
        if dp <= 0 {
            out.push_str("0.");
            for _ in 0..-dp {
                out.push('0');
            }
            out.push_str(&digits);
        } else if dp >= nd {
            out.push_str(&digits);
            for _ in 0..(dp - nd) {
                out.push('0');
            }
        } else {
            let dp = usize::try_from(dp).unwrap_or(0);
            out.push_str(&digits[..dp]);
            out.push('.');
            out.push_str(&digits[dp..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let magnitude = exp.unsigned_abs();
        if magnitude < 10 {
            out.push('0');
        }
        out.push_str(&magnitude.to_string());
    }
    out
}

/// Go `time.Duration` `String()`: `"30s"`, `"300ms"`, `"10m0s"`, `"1m30s"`, `"2h0m0s"`.
pub(crate) fn go_duration(d: std::time::Duration) -> String {
    const NS_PER_US: u128 = 1_000;
    const NS_PER_MS: u128 = 1_000_000;
    const NS_PER_S: u128 = 1_000_000_000;

    let u = d.as_nanos();
    if u == 0 {
        return "0s".to_owned();
    }
    if u < NS_PER_S {
        // Smaller than a second: smaller units, like 1.2ms.
        let (unit, prec) = if u < NS_PER_US {
            ("ns", 0)
        } else if u < NS_PER_MS {
            ("\u{b5}s", 3)
        } else {
            ("ms", 6)
        };
        let (int, frac) = fmt_frac(u, prec);
        return format!("{int}{frac}{unit}");
    }
    let (secs, frac) = fmt_frac(u, 9);
    let mut out = String::new();
    let mins = secs / 60;
    if mins > 0 {
        let hours = mins / 60;
        if hours > 0 {
            out.push_str(&hours.to_string());
            out.push('h');
        }
        out.push_str(&(mins % 60).to_string());
        out.push('m');
    }
    out.push_str(&(secs % 60).to_string());
    out.push_str(&frac);
    out.push('s');
    out
}

/// Go `fmtFrac`: `v / 10^prec` and the fraction as `".digits"` with trailing zeros dropped (`""` when zero).
fn fmt_frac(v: u128, prec: u32) -> (u128, String) {
    let base = 10u128.pow(prec);
    let int = v / base;
    let frac = v % base;
    if frac == 0 {
        return (int, String::new());
    }
    let width = usize::try_from(prec).unwrap_or(0);
    let mut digits = format!("{frac:0width$}");
    while digits.ends_with('0') {
        digits.pop();
    }
    (int, format!(".{digits}"))
}

/// Elapsed duration in the UI's compact style: `"<1s"` below one second, whole seconds
/// under a minute (`"45s"`), then space-separated carried units with seconds ALWAYS kept
/// (`"3m 45s"`, `"1h 3m 45s"`) — a timer must not degrade to minute granularity just
/// because it ran long. internal/timefmt Elapsed; ONE implementation for every timer.
pub(crate) fn elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        return "<1s".to_owned();
    }
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m {}s", s / 3600, s % 3600 / 60, s % 60)
    }
}

/// Token count with k/m units and one decimal, trailing `.0` trimmed: 842→`"842"`,
/// 1234→`"1.2k"`, 128000→`"128k"`, 1000000→`"1m"`, 1500000→`"1.5m"`. Counts below 1000
/// stay exact. internal/tokfmt Tokens; ONE implementation for every figure.
#[allow(clippy::cast_precision_loss)] // token counts sit far below 2^52; Go used float64 the same way
pub fn tokens(n: u64) -> String {
    fn trim_zero(f: f64) -> String {
        let mut s = format!("{f:.1}");
        if s.ends_with(".0") {
            s.truncate(s.len() - 2);
        }
        s
    }
    if n >= 1_000_000 {
        format!("{}m", trim_zero(n as f64 / 1e6))
    } else if n >= 1_000 {
        format!("{}k", trim_zero(n as f64 / 1e3))
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod ui_format_tests {
    use std::time::Duration;

    use super::{elapsed, tokens};

    // Go: internal/timefmt/timefmt_test.go:8
    #[test]
    fn test_elapsed() {
        let cases: [(Duration, &str); 10] = [
            (Duration::ZERO, "<1s"),
            (Duration::from_millis(999), "<1s"),
            (Duration::from_secs(1), "1s"),
            (Duration::from_secs(59), "59s"),
            (Duration::from_secs(60), "1m 0s"),
            (Duration::from_secs(65), "1m 5s"),
            (Duration::from_secs(59 * 60 + 59), "59m 59s"),
            (Duration::from_secs(3600), "1h 0m 0s"),
            (Duration::from_secs(3600 + 60 + 1), "1h 1m 1s"),
            (Duration::from_secs(2 * 3600 + 5 * 60 + 30), "2h 5m 30s"),
        ];
        for (d, want) in cases {
            assert_eq!(elapsed(d), want, "Elapsed({d:?})");
        }
    }

    // Go: internal/tokfmt/tokfmt_test.go:5
    #[test]
    fn test_tokens() {
        let cases: [(u64, &str); 11] = [
            (0, "0"),
            (842, "842"),
            (900, "900"),
            (1_000, "1k"),
            (1_234, "1.2k"),
            (12_400, "12.4k"),
            (56_400, "56.4k"),
            (128_000, "128k"),
            (1_000_000, "1m"),
            (1_100_000, "1.1m"),
            (1_500_000, "1.5m"),
        ];
        for (n, want) in cases {
            assert_eq!(tokens(n), want, "Tokens({n})");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{go_duration, go_float, go_quote, split_lines, truncate_to_char_boundary};

    #[test]
    fn split_lines_table() {
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("a"), vec!["a"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
        assert_eq!(split_lines("\n"), vec![""]);
        assert_eq!(split_lines("\n\n"), vec!["", ""]);
        assert_eq!(split_lines("a\r\nb\r\n"), vec!["a\r", "b\r"]);
    }

    #[test]
    fn truncate_to_char_boundary_backs_up() {
        assert_eq!(truncate_to_char_boundary("hello", 10), "hello");
        assert_eq!(truncate_to_char_boundary("hello", 5), "hello");
        assert_eq!(truncate_to_char_boundary("hello", 3), "hel");
        assert_eq!(truncate_to_char_boundary("hello", 0), "");
        // "é" is 2 bytes: a cut inside it backs up to the boundary before it.
        assert_eq!(truncate_to_char_boundary("aé", 2), "a");
        assert_eq!(truncate_to_char_boundary("aé", 3), "aé");
        // 4-byte scalar: cuts at 1, 2, 3 all back up to the empty string.
        for max in 1..4 {
            assert_eq!(truncate_to_char_boundary("😀x", max), "");
        }
        assert_eq!(truncate_to_char_boundary("😀x", 4), "😀");
    }

    #[test]
    fn go_quote_ascii() {
        assert_eq!(go_quote("abc"), "\"abc\"");
        assert_eq!(go_quote(""), "\"\"");
        assert_eq!(go_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(go_quote("a\nb\tc\\"), "\"a\\nb\\tc\\\\\"");
    }

    // Expectations generated with Go 1.27 `fmt.Sprintf("%v", f)`.
    #[test]
    fn go_float_table() {
        let cases = [
            (3.5, "3.5"),
            (2.0, "2"),
            (0.7, "0.7"),
            (0.0, "0"),
            (1.0, "1"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (100_000.0, "100000"),
            (1_000_000.0, "1e+06"),
            (123_456_789.0, "1.23456789e+08"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e21, "1e+21"),
            (1e20, "1e+20"),
            (-3.5, "-3.5"),
            (2.05, "2.05"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e-7, "1e-07"),
            (12345.678, "12345.678"),
            (999_999.0, "999999"),
            (0.5, "0.5"),
            (1.0 / 3.0, "0.3333333333333333"),
            (-0.0, "-0"),
            (1e100, "1e+100"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "+Inf"),
            (f64::NEG_INFINITY, "-Inf"),
        ];
        for (f, want) in cases {
            assert_eq!(go_float(f), want, "%v of {f:e}");
        }
    }

    // Expectations generated with Go 1.27 `time.Duration.String()`.
    #[test]
    fn go_duration_table() {
        let cases: [(u64, &str); 21] = [
            (0, "0s"),
            (1, "1ns"),
            (999, "999ns"),
            (1_000, "1µs"),
            (1_500, "1.5µs"),
            (1_000_000, "1ms"),
            (300_000_000, "300ms"),
            (1_500_000_000, "1.5s"),
            (30_000_000_000, "30s"),
            (90_000_000_000, "1m30s"),
            (600_000_000_000, "10m0s"),
            (7_200_000_000_000, "2h0m0s"),
            (90_061_500_000_000, "25h1m1.5s"),
            (1_234_567, "1.234567ms"),
            (123_456_789, "123.456789ms"),
            (1_000_000_000, "1s"),
            (59_000_000_000, "59s"),
            (60_000_000_000, "1m0s"),
            (3_600_000_000_000, "1h0m0s"),
            (360_000_000_000_000, "100h0m0s"),
            (1_500_000, "1.5ms"),
        ];
        for (ns, want) in cases {
            assert_eq!(go_duration(Duration::from_nanos(ns)), want, "{ns}ns");
        }
        assert_eq!(go_duration(Duration::from_secs(30)), "30s");
        assert_eq!(go_duration(Duration::from_millis(300)), "300ms");
    }
}
