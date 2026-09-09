//! 16-color SGR style (`TUI_CONTRACTS` §3.3). Param pins: bold=1, italic=3, faint=2,
//! underline=4, cyan fg=36; H1 = `"1;4"`.
//!
//! Emission order mirrors the Go lipgloss stack for the pinned byte cases: bold(1),
//! italic(3), underline(4), faint(2), then the foreground — so `# H1` renders `1;4`
//! and bold+faint renders `1;2` exactly as the Go renderer did.

/// 16-color SGR style; `render()` emits ONE combined `\x1b[..m` + text + `\x1b[0m`,
/// nothing when color is off. Containers compose attributes downward (the inline
/// renderer's style-context passing), so every atomic segment is self-contained and an
/// inner reset can never cut an outer style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// SGR 1.
    pub bold: bool,
    /// SGR 3.
    pub italic: bool,
    /// SGR 4.
    pub underline: bool,
    /// SGR 2.
    pub faint: bool,
    /// ANSI 16-color foreground INDEX (0..=7), emitted as SGR 30+index — cyan is
    /// `fg(6)` → param 36 (the Go `lipgloss.Color("6")` shape).
    pub fg: Option<u8>,
}

impl Style {
    /// This style with bold composed on.
    #[must_use]
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// This style with italic composed on.
    #[must_use]
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// This style with underline composed on.
    #[must_use]
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// This style with faint composed on.
    #[must_use]
    pub fn faint(mut self) -> Self {
        self.faint = true;
        self
    }

    /// This style with the 16-color foreground index set (6 = cyan → SGR 36).
    #[must_use]
    pub fn fg(mut self, index: u8) -> Self {
        self.fg = Some(index);
        self
    }

    /// One combined SGR + `s` + reset; `s` verbatim when `color` is false or the style
    /// carries no attribute; empty input renders empty (no stray escapes).
    pub fn render(&self, s: &str, color: bool) -> String {
        if !color || *self == Self::default() {
            return s.to_owned();
        }
        if s.is_empty() {
            return String::new();
        }
        let mut params = Vec::new();
        if self.bold {
            params.push("1".to_owned());
        }
        if self.italic {
            params.push("3".to_owned());
        }
        if self.underline {
            params.push("4".to_owned());
        }
        if self.faint {
            params.push("2".to_owned());
        }
        if let Some(idx) = self.fg {
            params.push((30 + u16::from(idx)).to_string());
        }
        format!("\x1b[{}m{s}\x1b[0m", params.join(";"))
    }
}
