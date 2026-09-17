#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP44 L2 suite — `TestBackend` frame goldens with an EMPTY composer (stacking order,
//! exactly 2 separators, bottom-slot swap), the status-line suite (exact SGR bytes),
//! the call-widget render, and the `ansi_to_spans` round-trip goldens (T-05).
//!
//! The composer-complete crown jewel (`TestWrappedComposerLayout`) lives WHOLLY in
//! WP46's tests/composer.rs — this file never renders composer content.
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
//! in-file (formerly a `#[path]`-mounted `tests/frame_goldens.rs` of the terminal crate; merged 2026-09-02).

use std::time::{Duration, Instant};

use crate::text::ansi::strip_sgr;
use crate::text::width::str_width;
use crate::ui::facade::StatusData;
use crate::ui::render::frame::{
    BottomZone, BusyView, FrameInput, FrameView, build_frame, status_line,
};
use crate::ui::render::region::RegionSnapshot;
use crate::ui::render::theme::{CYAN, FAINT, GREEN, RED, RESET, YELLOW};
use crate::ui::testutil::SPINNER_GLYPHS;
use ratatui::style::{Color, Modifier};

/// One frame case; defaults = idle 80-col frame with an EMPTY composer.
struct Case {
    width: u16,
    region: RegionSnapshot,
    spin: usize,
    scopes_active: bool,
    queue: Vec<String>,
    status: StatusData,
    busy: Option<BusyView>,
    candidates: Option<String>,
    surface: Option<Vec<String>>,
    desc: Option<String>,
    now: Instant,
}

impl Default for Case {
    fn default() -> Self {
        Self {
            width: 80,
            region: RegionSnapshot::default(),
            spin: 0,
            scopes_active: false,
            queue: Vec::new(),
            status: StatusData::default(),
            busy: None,
            candidates: None,
            surface: None,
            desc: None,
            now: Instant::now(),
        }
    }
}

/// Builds the frame for a case with the EMPTY composer (prompt row only, cursor at
/// column 2 — never any composer content in this file).
fn view(c: &Case) -> FrameView {
    let composer_rows = vec![format!("{CYAN}❯ {RESET}")];
    let bottom = if let Some(s) = &c.surface {
        BottomZone::Surface(s)
    } else if let Some(d) = &c.desc {
        BottomZone::Desc(d)
    } else {
        BottomZone::Status
    };
    build_frame(&FrameInput {
        width: c.width,
        region: &c.region,
        spin: c.spin,
        scopes_active: c.scopes_active,
        queue: &c.queue.iter().map(String::as_str).collect::<Vec<_>>(),
        composer_rows: &composer_rows,
        composer_cursor: if c.surface.is_some() {
            None
        } else {
            Some((2, 0))
        },
        candidates: c.candidates.as_deref(),
        bottom,
        status: &c.status,
        busy: c.busy.as_ref(),
        now: c.now,
    })
}

/// SGR-stripped frame rows (the Go `stripSGR(content(m))` instrument).
fn plain(v: &FrameView) -> Vec<String> {
    v.rows.iter().map(|r| strip_sgr(r)).collect()
}

/// Renders raw ANSI rows through `ansi_to_spans` into a ratatui `TestBackend` and
/// reads the cell grid back as plain strings — the L2 geometry instrument.
fn render_plain(rows: &[String], w: u16, h: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let lines: Vec<ratatui::text::Line<'static>> = rows
        .iter()
        .map(|r| crate::ui::render::spans::ansi_to_spans(r))
        .collect();
    term.draw(|f| {
        f.render_widget(
            ratatui::widgets::Paragraph::new(ratatui::text::Text::from(lines)),
            f.area(),
        );
    })
    .unwrap();
    let buf = term.backend().buffer();
    let width = usize::from(buf.area.width);
    buf.content
        .chunks(width)
        .map(|cells| {
            let mut s = String::new();
            let mut skip = 0usize;
            for c in cells {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = c.symbol();
                s.push_str(sym);
                skip = str_width(sym).saturating_sub(1);
            }
            s.trim_end().to_owned()
        })
        .collect()
}

fn find(rows: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
    rows.iter().position(|r| pred(r))
}

fn separator_indices(rows: &[String]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with(&super::SEPARATOR_GLYPH.repeat(3)))
        .map(|(i, _)| i)
        .collect()
}

/// The fixed stacking order with an EMPTY composer, exactly 2 separators, the
/// bottom-slot swap (surface > desc > status), the cursor offset math, and the
/// idle-frame no-spinner-glyph invariant — through BOTH the raw rows and the
/// `TestBackend` cell grid.
// Composer content is the composer suite's own; the frame order around an empty one is pinned here.
#[test]
fn frame_order_with_empty_composer_and_slot_swap() {
    let case = Case {
        status: StatusData {
            model: "gpt-4o".to_owned(),
            ctx_used: 1,
            ctx_window: 100,
            ..StatusData::default()
        },
        queue: vec!["queued".to_owned()],
        region: RegionSnapshot {
            tail: vec!["line-A".to_owned()],
            ..RegionSnapshot::default()
        },
        ..Case::default()
    };
    let v = view(&case);
    let rows = plain(&v);

    let seps = separator_indices(&rows);
    assert_eq!(seps.len(), 2, "want exactly 2 separators:\n{rows:#?}");
    let tail = find(&rows, |r| r == "line-A").expect("tail row");
    let queue = find(&rows, |r| r.contains("» queued")).expect("queue row");
    let composer = find(&rows, |r| r.contains('❯')).expect("composer row");
    let status = find(&rows, |r| r.contains("gpt-4o · ")).expect("status row");
    assert!(
        tail < queue
            && queue < seps[0]
            && seps[0] < composer
            && composer < seps[1]
            && seps[1] < status,
        "layout order wrong (tail={tail} queue={queue} sep={seps:?} composer={composer} status={status}):\n{rows:#?}"
    );
    assert_eq!(
        status,
        rows.len() - 1,
        "status must be the frame's last row"
    );
    // Spacer sits between the content side and the queue.
    assert_eq!(
        rows[queue - 1].trim(),
        "",
        "no blank spacer above the queue"
    );
    // The real cursor: prompt column 2, offset by every row above the composer.
    assert_eq!(
        v.cursor,
        Some((2, u16::try_from(composer).unwrap())),
        "cursor must sit at prompt col 2 on the composer row"
    );
    // The idle frame carries no spinner glyph (T-03's surviving half).
    let joined = rows.join("\n");
    assert!(
        !joined.chars().any(|c| SPINNER_GLYPHS.contains(c)),
        "idle frame must carry no spinner glyph:\n{joined}"
    );

    // The TestBackend cell grid agrees on the order.
    let h = u16::try_from(v.rows.len()).unwrap();
    let grid = render_plain(&v.rows, 80, h);
    let g_composer = find(&grid, |r| r.contains('❯')).expect("composer in grid");
    let g_status = find(&grid, |r| r.contains("gpt-4o")).expect("status in grid");
    assert!(g_composer < g_status, "grid order wrong:\n{grid:#?}");

    // A surface replaces the status row, below the composer.
    let case = Case {
        surface: Some(vec!["── /model ──".to_owned(), "▸ a".to_owned()]),
        ..case
    };
    let v = view(&case);
    let rows = plain(&v);
    assert_eq!(separator_indices(&rows).len(), 2);
    assert!(
        !rows.iter().any(|r| r.contains("gpt-4o · ")),
        "status visible while a surface is open:\n{rows:#?}"
    );
    let composer = find(&rows, |r| r.contains('❯')).expect("composer row");
    let surface = find(&rows, |r| r.contains("/model")).expect("surface row");
    assert!(composer < surface, "surface not below the composer");
    assert_eq!(
        v.cursor, None,
        "composer cursor must hide while a surface is open"
    );

    // A selected candidate's description takes the status slot; the candidates row
    // sits INSIDE the composer block (above the lower separator).
    let case = Case {
        surface: None,
        candidates: Some(format!("  {FAINT}⎿ {RESET}{CYAN}model{RESET}")),
        desc: Some(format!("  {FAINT}Pick a model{RESET}")),
        ..case
    };
    let v = view(&case);
    let rows = plain(&v);
    let seps = separator_indices(&rows);
    assert_eq!(seps.len(), 2);
    let cand = find(&rows, |r| r.contains("⎿ model")).expect("candidates row");
    let desc = find(&rows, |r| r.contains("Pick a model")).expect("desc row");
    assert!(
        cand < seps[1] && seps[1] < desc,
        "candidates must sit inside the composer block, desc below (cand={cand} sep={seps:?} desc={desc}):\n{rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("gpt-4o · ")),
        "status must yield its slot to the description"
    );
}

/// One CONSTANT blank row separates the content side from everything input-side —
/// directly above the queue when one shows, directly above the top separator otherwise.
#[test]
fn spacer_above_input_zone() {
    let v = view(&Case::default());
    let rows = plain(&v);
    let sep = separator_indices(&rows)[0];
    assert!(sep >= 1, "no room for a spacer:\n{rows:#?}");
    assert_eq!(
        rows[sep - 1].trim(),
        "",
        "no blank spacer above the top separator"
    );

    let v = view(&Case {
        queue: vec!["queued".to_owned()],
        ..Case::default()
    });
    let rows = plain(&v);
    let queue = find(&rows, |r| r.contains("» queued")).expect("queue row");
    assert!(queue >= 1);
    assert_eq!(
        rows[queue - 1].trim(),
        "",
        "no blank spacer above the queue row"
    );
}

/// Residue rows hold their height but render BLANK — stale widget chrome next to an
/// injected user block read as leaked tool output.
#[test]
fn residue_renders_blank() {
    let v = view(&Case {
        region: RegionSnapshot {
            tail: vec!["◇ ran 2 tools in 2s".to_owned()],
            residue: vec![
                "✓ [shell …] · stale".to_owned(),
                "⎿ 2 tools · 8s".to_owned(),
            ],
            ..RegionSnapshot::default()
        },
        ..Case::default()
    });
    let rows = plain(&v);
    let joined = rows.join("\n");
    assert!(
        !joined.contains("stale") && !joined.contains("2 tools · 8s"),
        "residue text leaked into the frame:\n{joined}"
    );
    let tail = find(&rows, |r| r.contains("ran 2 tools")).expect("tail row");
    assert_eq!(rows[tail + 1].trim(), "", "residue row 1 must render blank");
    assert_eq!(rows[tail + 2].trim(), "", "residue row 2 must render blank");
    // Height kept: the grid renders the same number of rows either way.
    let h = u16::try_from(v.rows.len()).unwrap();
    let grid = render_plain(&v.rows, 80, h);
    assert_eq!(grid.len(), rows.len());
}

/// Busy toggling on/off changes ZERO frame rows; the label lives on the SAME row as
/// the model/context status (the height invariant that killed the composer bounce).
#[test]
fn busy_in_status_line() {
    let status = StatusData {
        model: "gpt-4o".to_owned(),
        ctx_used: 1000,
        ctx_window: 128_000,
        ..StatusData::default()
    };
    let base = Case {
        status: status.clone(),
        ..Case::default()
    };
    let before = plain(&view(&base));

    let during_case = Case {
        status: status.clone(),
        busy: Some(BusyView {
            label: "Compacting context…".to_owned(),
            detail: String::new(),
            since: Instant::now(),
        }),
        ..Case::default()
    };
    let during = plain(&view(&during_case));
    assert_eq!(
        during.len(),
        before.len(),
        "busy ON changed the frame height:\n{during:#?}"
    );
    let row = during
        .iter()
        .find(|r| r.contains("Compacting context…"))
        .expect("busy label not in the frame");
    assert!(
        row.contains("gpt-4o"),
        "busy not on the status row: {row:?}"
    );

    let after = plain(&view(&Case {
        status,
        ..Case::default()
    }));
    assert_eq!(
        after.len(),
        before.len(),
        "busy OFF changed the frame height"
    );
    assert!(
        !after.join("\n").contains("Compacting context…"),
        "busy label not cleared"
    );
}

/// The live sub-state renders after the label (`"label · detail"`); the state-machine
/// halves (clock kept, phase clears detail, idle drop) live in the loop units.
#[test]
fn busy_detail_renders_after_label() {
    let v = view(&Case {
        busy: Some(BusyView {
            label: "Composing tool call — write_file".to_owned(),
            detail: "4.2 KB".to_owned(),
            since: Instant::now(),
        }),
        ..Case::default()
    });
    let joined = plain(&v).join("\n");
    assert!(
        joined.contains("Composing tool call — write_file · 4.2 KB"),
        "detail not rendered after the label:\n{joined}"
    );
}

/// Fields render; a narrow width truncates to a single row.
#[test]
fn status_line_renders_fields_and_truncates() {
    let s = StatusData {
        model: "gpt-4o".to_owned(),
        ctx_used: 12_000,
        ctx_window: 128_000,
        estimated: true,
        in_tokens: 148_000,
        out_tokens: 3_400,
        ..StatusData::default()
    };
    let line = strip_sgr(&status_line(&s, None, 0, false, 80, Instant::now()));
    for want in ["gpt-4o", "↑ 148k", "↓ 3.4k", "≈9% / 128k"] {
        assert!(line.contains(want), "status missing {want:?}: {line:?}");
    }
    let narrow = strip_sgr(&status_line(&s, None, 0, false, 20, Instant::now()));
    assert!(
        str_width(&narrow) <= 20,
        "narrow status overflows: {narrow:?}"
    );
}

/// Exact SGR bytes per segment: model cyan+faint, tokens green+faint, ctx hue+faint;
/// the em-dash placeholder keeps the model field visible.
#[test]
fn status_line_field_hues() {
    let s = StatusData {
        model: "gpt-4o".to_owned(),
        ctx_used: 1000,
        ctx_window: 128_000,
        estimated: true,
        in_tokens: 1000,
        out_tokens: 500,
        ..StatusData::default()
    };
    let line = status_line(&s, None, 0, false, 80, Instant::now());
    assert!(
        line.contains(&format!("{CYAN}{FAINT}gpt-4o{RESET}")),
        "model segment not cyan+faint:\n{line:?}"
    );
    assert!(
        line.contains(&format!("{GREEN}{FAINT}↑ 1k ↓ 500{RESET}")),
        "token segment not green+faint (or format drifted):\n{line:?}"
    );
    assert!(
        line.contains(&format!("{GREEN}{FAINT}≈0% / 128k{RESET}")),
        "context segment not green+faint (or format drifted):\n{line:?}"
    );

    let line = status_line(&StatusData::default(), None, 0, false, 80, Instant::now());
    assert!(
        strip_sgr(&line).contains('—'),
        "missing em-dash placeholder:\n{line:?}"
    );
}

/// The context figure warms as the window fills: green roomy, yellow past 70%, red
/// past 90%.
#[test]
fn status_line_context_hues() {
    for (used, hue, name) in [
        (50_000_u64, GREEN, "roomy"),
        (100_000, YELLOW, "past 70%"),
        (120_000, RED, "past 90%"),
    ] {
        let s = StatusData {
            model: "gpt-4o".to_owned(),
            ctx_used: used,
            ctx_window: 128_000,
            ..StatusData::default()
        };
        let line = status_line(&s, None, 0, false, 80, Instant::now());
        assert!(
            line.contains(&format!("{hue}{FAINT}")),
            "{name}: context segment missing its hue:\n{line:?}"
        );
    }
}

/// A token-less provider drops both figure segments instead of rendering zeros.
#[test]
fn status_line_without_token_accounting() {
    let s = StatusData {
        model: "imagen-4".to_owned(),
        ..StatusData::default()
    };
    let line = strip_sgr(&status_line(&s, None, 0, false, 80, Instant::now()));
    assert!(
        !line.contains(['↑', '↓', '%']),
        "token-less status should carry no figures: {line:?}"
    );
}

/// Without token figures the ctx segment hides too (the T1 default shape).
#[test]
fn status_line_hides_ctx_without_tokens() {
    let s = StatusData {
        model: "seedream-5.0-pro".to_owned(),
        ..StatusData::default()
    };
    let line = strip_sgr(&status_line(&s, None, 0, false, 80, Instant::now()));
    assert!(line.contains("seedream-5.0-pro"), "model missing: {line:?}");
    assert!(!line.contains('%'), "ctx segment must hide: {line:?}");
}

/// The cache share QUALIFIES the input figure (`"↑ 148k (77% cached) ↓ 22k"`) and
/// disappears when nothing was cached.
#[test]
fn status_line_cache_share() {
    let s = StatusData {
        model: "gpt-4o".to_owned(),
        ctx_used: 12_000,
        ctx_window: 128_000,
        in_tokens: 148_000,
        out_tokens: 22_000,
        cache_hit_pct: 76.8,
        ..StatusData::default()
    };
    let line = strip_sgr(&status_line(&s, None, 0, false, 120, Instant::now()));
    assert!(
        line.contains("↑ 148k (77% cached) ↓ 22k"),
        "cache share missing or misplaced: {line:?}"
    );

    let s = StatusData {
        cache_hit_pct: 0.0,
        model: "claude".to_owned(),
        ..s
    };
    let line = strip_sgr(&status_line(&s, None, 0, false, 120, Instant::now()));
    assert!(
        !line.contains("cached"),
        "cache share without activity: {line:?}"
    );
}

/// The `debug` marker renders yellow+faint and SURVIVES truncation (re-appended) —
/// a mode that rewrites the layout must not vanish on narrow terminals.
#[test]
fn status_line_debug_marker() {
    let off = StatusData {
        model: "gpt-4o".to_owned(),
        ctx_used: 1000,
        ctx_window: 128_000,
        ..StatusData::default()
    };
    let line = strip_sgr(&status_line(&off, None, 0, false, 80, Instant::now()));
    assert!(!line.contains("debug"), "marker shown while off: {line:?}");

    let on = StatusData { debug: true, ..off };
    let line = status_line(&on, None, 0, false, 80, Instant::now());
    assert!(
        line.contains(&format!("{YELLOW}{FAINT}debug{RESET}")),
        "marker missing or not yellow+faint:\n{line:?}"
    );

    for width in [28_u16, 24] {
        let narrow = strip_sgr(&status_line(&on, None, 0, false, width, Instant::now()));
        assert!(
            narrow.contains("debug"),
            "marker lost to truncation at {width}: {narrow:?}"
        );
        assert!(
            str_width(&narrow) <= usize::from(width),
            "status overflows {width}: {narrow:?}"
        );
    }
}

/// The call widget: spinner header over the live `"⎿ elapsed"` row; the cancel hint
/// only with an active scope; the detail rides ahead of the elapsed figure.
#[test]
fn call_preview_rendering() {
    let base = Instant::now();
    let mut case = Case {
        region: RegionSnapshot {
            label: "[shell …]".to_owned(),
            since: Some(base),
            ..RegionSnapshot::default()
        },
        now: base + Duration::from_secs(3),
        ..Case::default()
    };
    let rows = plain(&view(&case));
    let joined = rows.join("\n");
    assert!(
        joined.contains("[shell …]"),
        "widget header missing:\n{joined}"
    );
    assert!(
        joined.contains("⎿ 3s"),
        "elapsed status row missing:\n{joined}"
    );
    assert!(
        !joined.contains("ESC to cancel"),
        "cancel hint without a cancel scope:\n{joined}"
    );

    case.scopes_active = true;
    let joined = plain(&view(&case)).join("\n");
    assert!(
        joined.contains("⎿ 3s · ESC to cancel"),
        "cancel hint missing with an active scope:\n{joined}"
    );

    case.region.detail = "1.2k tokens".to_owned();
    let joined = plain(&view(&case)).join("\n");
    assert!(
        joined.contains("⎿ 1.2k tokens · 3s · ESC to cancel"),
        "detail missing from the status row:\n{joined}"
    );

    // A paused clock freezes the figure where it stopped.
    case.region.detail = String::new();
    case.region.paused_at = Some(base + Duration::from_secs(1));
    case.now = base + Duration::from_secs(5);
    let joined = plain(&view(&case)).join("\n");
    assert!(
        joined.contains("⎿ 1s"),
        "paused clock must freeze the elapsed figure:\n{joined}"
    );

    // The TestBackend grid renders the header + status row too.
    case.region.paused_at = None;
    let v = view(&case);
    let h = u16::try_from(v.rows.len()).unwrap();
    let grid = render_plain(&v.rows, 80, h);
    assert!(
        grid.iter().any(|r| r.contains("[shell …]")),
        "grid header:\n{grid:#?}"
    );
}

/// The staging window renders above the separator — tail as-is, preview rolling
/// source dim under the spinner header.
#[test]
fn region_rendering() {
    let v = view(&Case {
        region: RegionSnapshot {
            tail: vec!["line-A".to_owned()],
            label: "rendering table…".to_owned(),
            preview_tail: vec!["|src|".to_owned()],
            ..RegionSnapshot::default()
        },
        ..Case::default()
    });
    let rows = plain(&v);
    let tail = find(&rows, |r| r.contains("line-A")).expect("tail row");
    let header = find(&rows, |r| r.contains("rendering table…")).expect("preview header");
    let preview_tail = find(&rows, |r| r.contains("|src|")).expect("preview_tail row");
    let sep = separator_indices(&rows)[0];
    assert!(
        tail < header && header < preview_tail && preview_tail < sep,
        "staging window order wrong (tail={tail} header={header} preview_tail={preview_tail} sep={sep}):\n{rows:#?}"
    );
}

/// Queue rendering laws at the frame level: the hint rides the last fully-visible row
/// only when nothing is hidden; the overflow row carries it otherwise; slash items
/// re-wrap green.
// The render laws; the model-driven queue suite is queue_tests.rs.
#[test]
fn queue_rows_hint_laws() {
    let full = Case {
        queue: vec!["a", "b", "c", "d", "e"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ..Case::default()
    };
    let joined = plain(&view(&full)).join("\n");
    assert!(
        joined.contains("  … +2 more · ↑ edit"),
        "overflow row must advertise ↑ edit:\n{joined}"
    );
    assert!(
        !joined.contains("» c · ↑ edit"),
        "hidden-newest case must not hint on a visible row:\n{joined}"
    );

    let exact = Case {
        queue: vec!["/model".to_owned(), "b".to_owned()],
        ..Case::default()
    };
    let v = view(&exact);
    let joined = plain(&v).join("\n");
    assert!(
        joined.contains("» b · ↑ edit"),
        "fully-visible queue must hint on its last row:\n{joined}"
    );
    // The slash item re-wraps green inside the faint row (raw bytes).
    assert!(
        v.rows
            .iter()
            .any(|r| r.contains(&format!("{GREEN}/model{RESET}{FAINT}"))),
        "slash queue item not green:\n{:#?}",
        v.rows
    );
}

// ---------------------------------------------------------------------------
// ansi_to_spans round-trip goldens (T-05)
// ---------------------------------------------------------------------------

fn spans_of(s: &str) -> Vec<(String, ratatui::style::Style)> {
    crate::ui::render::spans::ansi_to_spans(s)
        .spans
        .into_iter()
        .map(|sp| (sp.content.into_owned(), sp.style))
        .collect()
}

// T-05 round-trip: 16-color + bold pin ("\x1b[1mb\x1b[0m" — markdown_test.go's byte pin).
#[test]
fn spans_roundtrip_bold_and_named() {
    let got = spans_of("\x1b[1mb\x1b[0m plain \x1b[36mcyan\x1b[0m");
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0].0, "b");
    assert!(got[0].1.add_modifier.contains(Modifier::BOLD));
    assert_eq!(got[1].0, " plain ");
    assert_eq!(got[1].1, ratatui::style::Style::new());
    assert_eq!(got[2].0, "cyan");
    assert_eq!(got[2].1.fg, Some(Color::Cyan));
}

// T-05 round-trip: 256-color foreground/background (the diff-shade palette class).
#[test]
fn spans_roundtrip_256_color() {
    let got = spans_of("\x1b[38;5;114m+add\x1b[0m\x1b[48;5;52mdel\x1b[0m");
    assert_eq!(got[0].1.fg, Some(Color::Indexed(114)));
    assert_eq!(got[1].1.bg, Some(Color::Indexed(52)));
    assert_eq!(got[1].1.fg, None, "reset must clear the foreground");
}

// T-05 round-trip: truecolor.
#[test]
fn spans_roundtrip_truecolor() {
    let got = spans_of("\x1b[38;2;1;2;3mfg\x1b[48;2;9;8;7mbg\x1b[0m");
    assert_eq!(got[0].1.fg, Some(Color::Rgb(1, 2, 3)));
    assert_eq!(got[1].1.bg, Some(Color::Rgb(9, 8, 7)));
    assert_eq!(
        got[1].1.fg,
        Some(Color::Rgb(1, 2, 3)),
        "fg carries until reset"
    );
}

// T-05 round-trip: reverse + faint, the merged sgr_carry form "\x1b[2;36m", and the
// clear-then-apply "0;7" law.
#[test]
fn spans_roundtrip_reverse_faint_and_merged() {
    let got = spans_of("\x1b[7mrev\x1b[0m");
    assert!(got[0].1.add_modifier.contains(Modifier::REVERSED));

    let got = spans_of("\x1b[2;36mdim-cyan\x1b[0m");
    assert!(got[0].1.add_modifier.contains(Modifier::DIM));
    assert_eq!(got[0].1.fg, Some(Color::Cyan));

    let got = spans_of("\x1b[1;31mx\x1b[0;7my\x1b[0m");
    assert!(got[1].1.add_modifier.contains(Modifier::REVERSED));
    assert!(
        !got[1].1.add_modifier.contains(Modifier::BOLD),
        "0; must clear first"
    );
    assert_eq!(got[1].1.fg, None);
}

// T-05: the removal codes 21-24/27/39/49.
#[test]
fn spans_removal_codes() {
    let got = spans_of("\x1b[1;2;3;4;7;31;41ma\x1b[22;23;24;27;39;49mb");
    let a = &got[0].1;
    assert!(a.add_modifier.contains(Modifier::BOLD | Modifier::DIM));
    let b = &got[1].1;
    assert_eq!(
        b.add_modifier,
        Modifier::empty(),
        "removals must clear: {b:?}"
    );
    assert_eq!(b.fg, None);
    assert_eq!(b.bg, None);
}

// T-05: bright named colors 90-97.
#[test]
fn spans_bright_named_colors() {
    let got = spans_of("\x1b[90ma\x1b[97mb\x1b[92mc");
    assert_eq!(got[0].1.fg, Some(Color::DarkGray));
    assert_eq!(got[1].1.fg, Some(Color::White));
    assert_eq!(got[2].1.fg, Some(Color::LightGreen));
}

// T-05: OSC 8 hyperlinks pass through ZERO-WIDTH — the text renders, the escapes
// contribute nothing, parsing never desyncs (BEL- and ST-terminated forms).
#[test]
fn spans_osc8_zero_width_passthrough() {
    let st = "\x1b]8;;http://e\x1b\\link\x1b]8;;\x1b\\";
    let got = spans_of(st);
    let text: String = got.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(text, "link");
    assert_eq!(str_width(&text), 4);

    let bel = "\x1b]8;;http://e\x07link\x1b]8;;\x07 tail";
    let text: String = spans_of(bel).iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(text, "link tail");

    // Styling survives around the link.
    let styled = format!("{FAINT}\u{1b}]8;;http://e\u{1b}\\x\u{1b}]8;;\u{1b}\\y{RESET}");
    let got = spans_of(&styled);
    assert!(
        got.iter()
            .all(|(_, st)| st.add_modifier.contains(Modifier::DIM))
    );
}
