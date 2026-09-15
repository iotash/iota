//! The terminal's title stack (chat/run.go:122-123): pushed on plain stdout before the event loop, popped
//! after the facade has released the terminal — the two OSCs and the raw write they go out through.

use std::io::Write as _;

/// chat/run.go:122 — push the terminal's title stack, on plain stdout, before the event loop.
pub(super) const TITLE_STACK_PUSH: &str = "\x1b[22;0t";

/// chat/run.go:123 — pop it, deferred until after the facade has released the terminal.
pub(super) const TITLE_STACK_POP: &str = "\x1b[23;0t";

/// The title-stack OSCs go to plain stdout: they are emitted before the facade exists and after it is gone.
pub(super) fn write_raw(seq: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_two_oscs_are_byte_exact() {
        assert_eq!(super::TITLE_STACK_PUSH, "\u{1b}[22;0t");
        assert_eq!(super::TITLE_STACK_POP, "\u{1b}[23;0t");
    }
}
