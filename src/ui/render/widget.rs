//! The call widget — the running tool call's lifecycle block, as the screen draws it: the spinner
//! and the header label, the body rows newest first (X-118), and the `⎿ detail · elapsed` status
//! row, its clock frozen while the call waits on the user.
//!
//! It is a [`Layout`]: the spinner frame and the elapsed figure are taken when it is built (from
//! the loop's counter and clock), and its rows are laid out at the width of the draw — every row
//! cut to `width − 4` columns, `…` closing a cut row.

use std::time::Instant;

use crate::text;
use crate::text::ansi::truncate_ansi;
use crate::ui::block::Layout;

use super::frame::SPINNER_FRAMES;
use super::region::RegionSnapshot;
use super::theme::{CYAN, FAINT, RESET};

/// The columns a body or status row leaves of the width: the two-space indent and a margin.
const RESERVE: usize = 4;

/// The widget as it stands at one moment (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallWidget {
    /// The spinner frame.
    spinner: &'static str,
    /// The header label, as supplied (callers pre-style it).
    label: String,
    /// The body rows, in the order drawn (newest first).
    body: Vec<String>,
    /// The status row's text (`detail · elapsed`), while the call has a clock.
    status: Option<String>,
}

impl CallWidget {
    /// The widget `snap` describes at spinner count `spin` and time `now`; `None` while no call
    /// preview is up (an empty label).
    pub(crate) fn new(snap: &RegionSnapshot, spin: usize, now: Instant) -> Option<Self> {
        if snap.label.is_empty() {
            return None;
        }
        let status = snap.since.map(|since| {
            let mut status = String::from("⎿ ");
            if !snap.detail.is_empty() {
                status.push_str(&snap.detail);
                status.push_str(" · ");
            }
            let elapsed = snap.paused_at.unwrap_or(now).duration_since(since);
            status.push_str(&text::elapsed(elapsed));
            status
        });
        Some(Self {
            spinner: SPINNER_FRAMES[spin % SPINNER_FRAMES.len()],
            label: snap.label.clone(),
            body: snap.preview_tail.clone(),
            status,
        })
    }
}

impl Layout for CallWidget {
    fn lines(&self, width: usize) -> Vec<String> {
        // The label renders as supplied, so a streamed tool call's header looks exactly like its
        // final committed form (model.go:954). The cancel hint is the status line's alone
        // (docs/design/busy-span.md §3.4, point 6): one place, where it never moves.
        let room = width.saturating_sub(RESERVE).max(4);
        let row = |t: &str| format!("{FAINT}  {}{RESET}", truncate_ansi(t, room, "…"));
        let mut rows = Vec::with_capacity(self.body.len() + 2);
        rows.push(format!(
            "{CYAN}{}{RESET} {}{RESET}",
            self.spinner, self.label
        ));
        rows.extend(self.body.iter().map(|l| row(l)));
        rows.extend(self.status.as_deref().map(row));
        rows
    }
}

#[cfg(test)]
mod tests;
