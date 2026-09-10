//! `parse_window_size` (chat/tokens.go:20-43) — the one place `models.<name>.context_window` (and a session
//! meta's recorded window) is read. It has no headless effect: compaction is interactive-only.

/// Why a window size did not parse (chat/tokens.go:23,36,40 — the three texts, verbatim).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowSizeError {
    /// Nothing but whitespace.
    #[error("empty size")]
    Empty,
    /// The numeric part (after the unit suffix) is not a positive number; carries it as Go's `%q`.
    #[error("invalid context window size: {0:?}")]
    NotANumber(String),
    /// The value truncates to zero or does not fit.
    #[error("invalid context window size")]
    OutOfRange,
}

/// lowercase+trim; suffix k|m|b = 1e3|1e6|1e9; `ParseFloat`; see [`WindowSizeError`] for the failures.
pub fn parse_window_size(s: &str) -> Result<u64, WindowSizeError> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Err(WindowSizeError::Empty);
    }
    // tokens.go:26-33: one optional decimal unit suffix; the numeric part keeps any inner whitespace for the
    // error text (`%q` of the post-suffix string) and is trimmed only for the parse.
    let (mult, num) = match s.as_bytes()[s.len() - 1] {
        b'k' => (1e3, &s[..s.len() - 1]),
        b'm' => (1e6, &s[..s.len() - 1]),
        b'b' => (1e9, &s[..s.len() - 1]),
        _ => (1.0, s.as_str()),
    };
    let v = match num.trim().parse::<f64>() {
        Ok(v) if v > 0.0 => v,
        _ => return Err(WindowSizeError::NotANumber(num.to_owned())),
    };
    // tokens.go:38-41: `int(v * mult)` truncates; a non-positive (or, on amd64, overflowed/non-finite) result is
    // the bare error. The truncating/sign-losing casts are guarded by the `> 0` check and the finite bound.
    let n = (v * mult).trunc();
    #[allow(clippy::cast_precision_loss)]
    let max = i64::MAX as f64;
    if !n.is_finite() || n < 1.0 || n >= max {
        return Err(WindowSizeError::OutOfRange);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let tokens = n as u64;
    Ok(tokens)
}
