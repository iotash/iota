//! The frame builder — a pure port of Go `model.View` + `model.statusLine`
//! (internal/ui/model.go:914-1193).
//!
//! Builds the pinned bottom frame as raw ANSI rows in the FIXED stacking order
//! `[staging tail][residue (blank)][preview][spacer][queue] ─sep─ [composer
//! (+candidates row)] ─sep─ [surface | suggestion-desc | status]`, tracking `rows_above`
//! for the real-cursor offset. Every appended row is exactly one visual line (the
//! one-entry-one-row law) or the cursor desyncs. Above the composer the frame only ever
//! GROWS; busy is a status-row SEGMENT so toggling it never changes the frame height
//! (`TestBusyInStatusLine`); the bottom zone swaps content on existing rows
//! (surface > suggestion-desc > status).
//!
//! Pure data in, rows out: the loop model assembles a [`FrameInput`] from its state, so
//! layer-2 goldens drive this builder without a terminal and WP46/WP47 feed the
//! composer/candidates/surface slots from their own modules without touching this file.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::text;
use crate::text::ansi::truncate_ansi;
use crate::text::width::str_width;
use crate::ui::facade::StatusData;

use super::region::RegionSnapshot;
use super::theme::{CYAN, FAINT, GREEN, RED, RESET, YELLOW};

/// Braille spinner frames — single column, width-safe (model.go:25). One counter
/// drives both the preview header and the status-line busy glyph.
pub(crate) const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Visible queued lines cap; beyond it a `"+N more"` row (model.go:32).
pub(crate) const QUEUE_SHOWN_MAX: usize = 3;

/// Busy elapsed time renders only past this threshold (model.go:1127).
pub(crate) const BUSY_ELAPSED_MIN: Duration = Duration::from_secs(2);

/// The queue-management hint, appended to the last fully-visible row — or to the
/// overflow row when newest items are hidden (model.go:998).
const QUEUE_HINT: &str = " · ↑ edit";

/// The live busy phase (model.go `busyState`): label + optional detail + phase clock.
#[derive(Debug, Clone)]
pub(crate) struct BusyView {
    /// The phase label (`"Waiting for the model"`).
    pub(crate) label: String,
    /// Live sub-state (`"4.2 KB"`); updates keep the clock (model.go busyDetailMsg).
    pub(crate) detail: String,
    /// Phase start; elapsed renders after [`BUSY_ELAPSED_MIN`].
    pub(crate) since: Instant,
}

/// What occupies the one bottom-zone slot below the second separator
/// (surface > suggestion-desc > status — model.go:1049-1058).
pub(crate) enum BottomZone<'a> {
    /// The status line (the default resident).
    Status,
    /// The selected completion candidate's description row (pre-rendered by suggest).
    Desc(&'a str),
    /// An open interaction surface's rows (pre-rendered by the surface engine).
    Surface(&'a [String]),
}

/// Everything the frame renders, as plain data (assembled by the loop model).
pub(crate) struct FrameInput<'a> {
    /// Terminal width (0/unknown → separators fall back to 80 columns).
    pub(crate) width: u16,
    /// The staging-window snapshot (tail, residue, preview).
    pub(crate) region: &'a RegionSnapshot,
    /// The shared spinner counter.
    pub(crate) spin: usize,
    /// Whether any cancel scope is active (renders the ESC hints).
    pub(crate) scopes_active: bool,
    /// Queued type-ahead submits, oldest first.
    pub(crate) queue: &'a [String],
    /// The composer's styled rows (prompt included), 1..=5.
    pub(crate) composer_rows: &'a [String],
    /// The composer cursor as (column incl. the 2-col prompt, row within the composer
    /// block); `None` while a surface owns (and hides) the cursor.
    pub(crate) composer_cursor: Option<(u16, u16)>,
    /// The completion-candidates row — INSIDE the composer block, above the lower
    /// separator (it belongs to the line being typed; model.go:1042-1045).
    pub(crate) candidates: Option<&'a str>,
    /// The bottom-zone occupant.
    pub(crate) bottom: BottomZone<'a>,
    /// Status-row data.
    pub(crate) status: &'a StatusData,
    /// The live busy phase, if any.
    pub(crate) busy: Option<&'a BusyView>,
    /// "Now" for every elapsed figure (injected so goldens control the clock).
    pub(crate) now: Instant,
}

/// The built frame: one string per visual row (raw ANSI, no trailing newlines) and the
/// real-cursor target relative to the frame origin.
pub(crate) struct FrameView {
    /// Every frame row, top to bottom.
    pub(crate) rows: Vec<String>,
    /// Real-cursor (x, y) within the frame; `None` = hidden (surface open).
    pub(crate) cursor: Option<(u16, u16)>,
}

/// `max(4, width - reserve)` in display columns (Go `maxInt(4, m.width-…)`).
fn budget(width: u16, reserve: usize) -> usize {
    (width as usize).saturating_sub(reserve).max(4)
}

/// Builds the frame rows in the fixed stacking order (model.go View, 914-1076).
pub(crate) fn build_frame(fi: &FrameInput<'_>) -> FrameView {
    let mut rows: Vec<String> = Vec::new();

    // The staging window: committed content, rendered as-is.
    rows.extend(fi.region.tail.iter().cloned());
    // Residue rows keep their HEIGHT but render BLANK — stale widget chrome next to an
    // injected user block read as leaked tool output (model.go:947-950).
    rows.extend(fi.region.residue.iter().map(|_| String::new()));
    if !fi.region.label.is_empty() {
        // The label renders as supplied (callers pre-style it), so a streamed tool
        // call's header looks exactly like its final committed form (model.go:954).
        rows.push(format!(
            "{CYAN}{}{RESET} {}{RESET}",
            SPINNER_FRAMES[fi.spin % SPINNER_FRAMES.len()],
            fi.region.label
        ));
        for line in &fi.region.preview_tail {
            rows.push(format!(
                "{FAINT}  {}{RESET}",
                truncate_ansi(line, budget(fi.width, 4), "…")
            ));
        }
        if let Some(since) = fi.region.since {
            // The call preview's live status row: detail, elapsed (frozen while the
            // clock is paused), and the cancel hint (model.go:966-978).
            let mut status = String::from("⎿ ");
            if !fi.region.detail.is_empty() {
                status.push_str(&fi.region.detail);
                status.push_str(" · ");
            }
            let elapsed = fi
                .region
                .paused_at
                .map_or_else(|| fi.now.duration_since(since), |p| p.duration_since(since));
            status.push_str(&text::elapsed(elapsed));
            if fi.scopes_active {
                status.push_str(" · ESC to cancel");
            }
            rows.push(format!(
                "{FAINT}  {}{RESET}",
                truncate_ansi(&status, budget(fi.width, 4), "…")
            ));
        }
    }

    // Spacer: one CONSTANT blank row between the content side and everything
    // input-side — it can never bounce the frame (model.go:983-988).
    rows.push(String::new());

    // Type-ahead queue: dim "»" rows; the block's bottom row carries the management
    // hint — on the overflow row instead when newest items are hidden
    // (model.go:993-1020; the hint reserve uses BYTE length, Go `len` parity).
    let n = fi.queue.len();
    if n > 0 {
        let shown = n.min(QUEUE_SHOWN_MAX);
        for (i, entry) in fi.queue.iter().enumerate().take(shown) {
            let hinted = i == shown - 1 && n == shown;
            let width = if hinted {
                budget(fi.width, 4 + QUEUE_HINT.len())
            } else {
                budget(fi.width, 4)
            };
            let mut item = truncate_ansi(entry, width, "…");
            if item.starts_with('/') {
                item = format!("{GREEN}{item}{RESET}{FAINT}");
            }
            if hinted {
                item.push_str(QUEUE_HINT);
            }
            rows.push(format!("{FAINT}» {item}{RESET}"));
        }
        if n > shown {
            rows.push(format!("{FAINT}  … +{} more{QUEUE_HINT}{RESET}", n - shown));
        }
    }

    // The composer sits between TWO separators; completion candidates render INSIDE
    // the block, above the lower one (model.go:1022-1046).
    let w = if fi.width < 1 { 80 } else { fi.width as usize };
    let sep = format!("{FAINT}{}{RESET}", "─".repeat(w));
    rows.push(sep.clone());
    let rows_above = rows.len(); // frame rows above the composer = the cursor Y offset
    rows.extend(fi.composer_rows.iter().cloned());
    if let Some(c) = fi.candidates {
        rows.push(c.to_owned());
    }
    rows.push(sep);

    // The bottom zone holds exactly ONE of surface | description | status; swapping is
    // a content change, never a composer move (model.go:1049-1058).
    match fi.bottom {
        BottomZone::Surface(surface_rows) => rows.extend(surface_rows.iter().cloned()),
        BottomZone::Desc(desc) => rows.push(desc.to_owned()),
        BottomZone::Status => rows.push(status_line(
            fi.status,
            fi.busy,
            fi.spin,
            fi.scopes_active,
            fi.width,
            fi.now,
        )),
    }

    let cursor = fi.composer_cursor.map(|(x, y)| {
        let above = u16::try_from(rows_above).unwrap_or(u16::MAX);
        (x, above.saturating_add(y))
    });
    FrameView { rows, cursor }
}

/// Renders the status line (model.go statusLine, 1083-1179): `"  model"` then
/// `" · "`-joined optional segments in order — tokens, ctx, debug, busy — with
/// per-segment hues, truncated to one row with the debug marker re-appended.
pub(crate) fn status_line(
    s: &StatusData,
    busy: Option<&BusyView>,
    spin: usize,
    scopes_active: bool,
    width: u16,
    now: Instant,
) -> String {
    let model = if s.model.is_empty() { "—" } else { &s.model };

    // Session cost: each arrow only once its side is non-zero (token-less providers
    // drop the segment, never zeros); the cache share QUALIFIES ↑ (model.go:1093-1105).
    let mut tokens = String::new();
    if s.in_tokens > 0 {
        tokens = format!("↑ {}", text::tokens(s.in_tokens));
        if s.cache_hit_pct > 0.0 {
            let _ = write!(tokens, " ({:.0}% cached)", s.cache_hit_pct);
        }
    }
    if s.out_tokens > 0 {
        if !tokens.is_empty() {
            tokens.push(' ');
        }
        let _ = write!(tokens, "↓ {}", text::tokens(s.out_tokens));
    }

    // Context fill, warming as the window fills; `≈` marks an estimate
    // (model.go:1109-1118).
    let mut ctx = String::new();
    let mut ctx_hue = GREEN;
    if let Some(pct) = (s.ctx_used * 100).checked_div(s.ctx_window) {
        let approx = if s.estimated { "≈" } else { "" };
        ctx = format!("{approx}{pct}% / {}", text::tokens(s.ctx_window));
        ctx_hue = usage_hue(pct);
    }

    // Busy tail: " label[ · detail][  elapsed][ (ESC to cancel)]" (model.go:1120-1132).
    let mut frame = "";
    let mut tail = String::new();
    if let Some(b) = busy {
        frame = SPINNER_FRAMES[spin % SPINNER_FRAMES.len()];
        tail = format!(" {}", b.label);
        if !b.detail.is_empty() {
            let _ = write!(tail, " · {}", b.detail);
        }
        let elapsed = now.duration_since(b.since);
        if elapsed >= BUSY_ELAPSED_MIN {
            let _ = write!(tail, "  {}", text::elapsed(elapsed));
        }
        if scopes_active {
            tail.push_str(" (ESC to cancel)");
        }
    }

    // Recording is a MODE, not a figure — and the one segment that survives
    // truncation (model.go:1139-1142).
    let dbg = if s.debug { "debug" } else { "" };

    let mut plain = format!("  {model}");
    for seg in [tokens.as_str(), ctx.as_str(), dbg] {
        if !seg.is_empty() {
            let _ = write!(plain, " · {seg}");
        }
    }
    if busy.is_some() {
        let _ = write!(plain, " · {frame}{tail}");
    }

    if str_width(&plain) > width as usize {
        // Truncation eats the tail, where the mode marker sits — re-append it: a
        // narrow terminal is exactly where a silently changed layout is hardest to
        // explain (model.go:1153-1163).
        let mut line = truncate_ansi(&plain, (width as usize).max(4), "…");
        if !dbg.is_empty() {
            let marker = format!(" · {dbg}");
            let room = (width as usize).saturating_sub(str_width(&marker));
            if room > 4 {
                line = truncate_ansi(&plain, room, "…") + &marker;
            }
        }
        return format!("{FAINT}{line}{RESET}");
    }

    let mut out = format!("  {CYAN}{FAINT}{model}{RESET}");
    if !tokens.is_empty() {
        let _ = write!(out, "{FAINT} · {RESET}{GREEN}{FAINT}{tokens}{RESET}");
    }
    if !ctx.is_empty() {
        let _ = write!(out, "{FAINT} · {RESET}{ctx_hue}{FAINT}{ctx}{RESET}");
    }
    if !dbg.is_empty() {
        let _ = write!(out, "{FAINT} · {RESET}{YELLOW}{FAINT}{dbg}{RESET}");
    }
    if busy.is_some() {
        let _ = write!(
            out,
            "{FAINT} · {RESET}{CYAN}{frame}{RESET}{FAINT}{tail}{RESET}"
        );
    }
    out
}

/// Context-fill hue: green while there is room, yellow past 70%, red past 90% — the
/// 80% auto-compaction prompt should never arrive as a surprise (model.go:1184-1193).
pub(crate) fn usage_hue(pct: u64) -> &'static str {
    if pct > 90 {
        RED
    } else if pct > 70 {
        YELLOW
    } else {
        GREEN
    }
}

#[cfg(test)]
mod tests;
