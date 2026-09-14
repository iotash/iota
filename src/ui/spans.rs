//! `ansi_to_spans` — the ONE ANSI→ratatui parser (T-05).
//!
//! The region stores committed lines as raw pre-styled ANSI strings end-to-end (the
//! byte-exact-strings bar; the clip/wrap/re-emit theme law). This module converts them
//! at exactly two boundaries: the `insert_before` buffer and the frame rows. Coverage:
//! SGR 0/1/2/3/4/7/21–24/27/39/49, named colors 30–37/90–97 (+ their 40–47/100–107
//! backgrounds), 256-color 38;5/48;5 and truecolor 38;2/48;2 — round-trip pinned by the
//! WP44 goldens.
//!
//! OSC sequences (OSC 8 hyperlinks included) are passed OVER zero-width: their text
//! renders, the escapes contribute nothing, and parsing never desyncs. A terminal cell
//! cannot carry an OSC 8 wrapper, so the link itself does not survive the cell grid —
//! recorded against T-05 in the deviations log.
//!
//! This boundary is also where the frame honours `NO_COLOR` (DIVERGENCES X-28): under
//! [`crate::color::ColorMode::Off`] a parsed foreground or background never reaches the
//! cell, while bold/faint/italic/underline/reverse do. Everything the terminal receives —
//! the frame rows, the committed scrollback, the surfaces — passes through here, so one
//! gate covers the whole frame side, and `ui::theme` stays what it is: the palette, not
//! the decision to use it. Crossterm has a `NO_COLOR` gate of its own, but it fires
//! INSIDE a `\x1b[…m` it has already opened, so every suppressed color used to arrive as
//! a bare `\x1b[m` — a full reset — and the attributes around it were lost.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Accumulated SGR state between escape sequences.
#[derive(Clone, Copy, Default)]
struct SgrState {
    fg: Option<Color>,
    bg: Option<Color>,
    mods: Modifier,
}

impl SgrState {
    /// The ratatui style for a span emitted under this state; with `color` off the
    /// foreground and background stay out of it and only the modifiers reach the cell.
    fn style(self, color: bool) -> Style {
        let mut st = Style::new();
        if color {
            if let Some(fg) = self.fg {
                st = st.fg(fg);
            }
            if let Some(bg) = self.bg {
                st = st.bg(bg);
            }
        }
        st.add_modifier(self.mods)
    }

    /// Applies one SGR parameter list (the bytes between `\x1b[` and `m`).
    fn apply(&mut self, params: &str) {
        let ps: Vec<u16> = params
            .split(';')
            .map(|p| {
                if p.is_empty() {
                    0
                } else {
                    p.parse().unwrap_or(0)
                }
            })
            .collect();
        let mut i = 0;
        while i < ps.len() {
            match ps[i] {
                0 => *self = Self::default(),
                1 => self.mods |= Modifier::BOLD,
                2 => self.mods |= Modifier::DIM,
                3 => self.mods |= Modifier::ITALIC,
                4 => self.mods |= Modifier::UNDERLINED,
                7 => self.mods |= Modifier::REVERSED,
                // 21 doubles as "bold off" in the wrap/carry pipeline; 22 is the
                // canonical normal-intensity reset (bold AND faint off).
                21 | 22 => self.mods &= !(Modifier::BOLD | Modifier::DIM),
                23 => self.mods &= !Modifier::ITALIC,
                24 => self.mods &= !Modifier::UNDERLINED,
                27 => self.mods &= !Modifier::REVERSED,
                30..=37 => self.fg = Some(named(ps[i] - 30, false)),
                39 => self.fg = None,
                40..=47 => self.bg = Some(named(ps[i] - 40, false)),
                49 => self.bg = None,
                90..=97 => self.fg = Some(named(ps[i] - 90, true)),
                100..=107 => self.bg = Some(named(ps[i] - 100, true)),
                38 | 48 => {
                    let (color, consumed) = extended(&ps[i + 1..]);
                    if let Some(c) = color {
                        if ps[i] == 38 {
                            self.fg = Some(c);
                        } else {
                            self.bg = Some(c);
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// The 8 named colors, base (30–37) or bright (90–97), in SGR order.
fn named(idx: u16, bright: bool) -> Color {
    if bright {
        match idx {
            0 => Color::DarkGray,
            1 => Color::LightRed,
            2 => Color::LightGreen,
            3 => Color::LightYellow,
            4 => Color::LightBlue,
            5 => Color::LightMagenta,
            6 => Color::LightCyan,
            _ => Color::White,
        }
    } else {
        match idx {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            _ => Color::Gray,
        }
    }
}

/// Parses the extended-color tail after a 38/48: `5;N` (indexed) or `2;R;G;B`
/// (truecolor). Returns the color and how many parameters were consumed.
fn extended(rest: &[u16]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => {
            let c = rest
                .get(1)
                .map(|n| Color::Indexed(u8::try_from(*n).unwrap_or(u8::MAX)));
            (c, 2)
        }
        Some(2) => {
            let c = match (rest.get(1), rest.get(2), rest.get(3)) {
                (Some(r), Some(g), Some(b)) => Some(Color::Rgb(
                    u8::try_from(*r).unwrap_or(u8::MAX),
                    u8::try_from(*g).unwrap_or(u8::MAX),
                    u8::try_from(*b).unwrap_or(u8::MAX),
                )),
                _ => None,
            };
            (c, 4)
        }
        // Malformed tail: swallow everything so later params cannot misparse.
        _ => (None, rest.len()),
    }
}

/// Parses one raw ANSI row into a styled ratatui [`Line`] under the process's color
/// decision ([`crate::color::enabled`]).
///
/// Only SGR sequences change state; every other CSI is skipped, OSC sequences (BEL- or
/// ST-terminated) are passed over zero-width, and a bare two-byte escape is dropped.
/// The text between escapes lands in spans carrying the accumulated style.
pub(crate) fn ansi_to_spans(s: &str) -> Line<'static> {
    ansi_to_spans_with(s, crate::color::enabled())
}

/// [`ansi_to_spans`] with the color decision made explicit: `false` keeps every parsed
/// foreground and background out of the cells (the `NO_COLOR` frame).
pub(crate) fn ansi_to_spans_with(s: &str, color: bool) -> Line<'static> {
    let bytes = s.as_bytes();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut state = SgrState::default();
    let mut text = String::new();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == 0x1b {
            match bytes.get(i + 1) {
                Some(b'[') => {
                    // CSI: parameters/intermediates up to a final byte 0x40..=0x7e.
                    let mut j = i + 2;
                    while j < s.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j >= s.len() {
                        break; // unterminated escape: drop the tail
                    }
                    if bytes[j] == b'm'
                        && bytes[i + 2..j]
                            .iter()
                            .all(|c| c.is_ascii_digit() || *c == b';')
                    {
                        flush(&mut spans, &mut text, state, color);
                        state.apply(&s[i + 2..j]);
                    }
                    i = j + 1;
                }
                Some(b']') => {
                    // OSC: BEL- or ST-terminated; zero-width passthrough.
                    let mut j = i + 2;
                    let mut end = s.len();
                    while j < s.len() {
                        if bytes[j] == 0x07 {
                            end = j + 1;
                            break;
                        }
                        if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                            end = j + 2;
                            break;
                        }
                        j += 1;
                    }
                    i = end;
                }
                Some(_) => i += 2, // other escape: ESC + one byte
                None => break,
            }
            continue;
        }
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        text.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut spans, &mut text, state, color);
    Line::from(spans)
}

/// Emits the pending text as one span under `state` (no-op for empty text).
fn flush(spans: &mut Vec<Span<'static>>, text: &mut String, state: SgrState, color: bool) {
    if text.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(text), state.style(color)));
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};

    use super::ansi_to_spans_with;

    fn spans(s: &str, color: bool) -> Vec<(String, Style)> {
        ansi_to_spans_with(s, color)
            .spans
            .into_iter()
            .map(|sp| (sp.content.into_owned(), sp.style))
            .collect()
    }

    /// The row every `NO_COLOR` frame assertion is about: a named color, a 256-color
    /// background, a truecolor foreground and three attributes in one line.
    const ROW: &str = "\x1b[36m❯ \x1b[0m\x1b[48;5;236mfield\x1b[0m \x1b[2;38;2;1;2;3mhint\x1b[0m \x1b[1;4;7mtab\x1b[0m";

    // X-28: with color off, no cell carries a foreground or a background — and every
    // attribute the same row carried is still there.
    #[test]
    fn no_color_keeps_attributes_and_drops_every_color() {
        let got = spans(ROW, false);
        for (text, style) in &got {
            assert_eq!(style.fg, None, "{text:?} kept a foreground");
            assert_eq!(style.bg, None, "{text:?} kept a background");
        }
        let hint = got.iter().find(|(t, _)| t == "hint").expect("hint span");
        assert!(hint.1.add_modifier.contains(Modifier::DIM));
        let tab = got.iter().find(|(t, _)| t == "tab").expect("tab span");
        assert!(tab.1.add_modifier.contains(Modifier::BOLD));
        assert!(tab.1.add_modifier.contains(Modifier::UNDERLINED));
        assert!(tab.1.add_modifier.contains(Modifier::REVERSED));
        // The text itself is untouched: the gate is on the style, never on the glyphs.
        let text: String = got.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "❯ field hint tab");
    }

    // The same row with color on is the control: the colors the gate removes are there.
    #[test]
    fn color_on_carries_every_color() {
        let got = spans(ROW, true);
        assert_eq!(got[0].1.fg, Some(Color::Cyan));
        let field = got.iter().find(|(t, _)| t == "field").expect("field span");
        assert_eq!(field.1.bg, Some(Color::Indexed(236)));
        let hint = got.iter().find(|(t, _)| t == "hint").expect("hint span");
        assert_eq!(hint.1.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(hint.1.add_modifier.contains(Modifier::DIM));
    }
}
