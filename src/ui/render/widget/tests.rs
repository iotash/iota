#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The call widget's rows: their order, the cut at `width − 4`, the clock and its pause.

use std::time::{Duration, Instant};

use crate::text::ansi::{ansi_width, strip_ansi};
use crate::text::width::str_width;
use crate::ui::block::Layout;
use crate::ui::render::frame::SPINNER_FRAMES;
use crate::ui::render::region::RegionSnapshot;

use super::CallWidget;

fn snap(body: &[&str], since: Option<Instant>) -> RegionSnapshot {
    RegionSnapshot {
        label: "[shell sleep 4]".to_owned(),
        preview_tail: body.iter().map(|s| (*s).to_owned()).collect(),
        since,
        ..RegionSnapshot::default()
    }
}

fn plain(w: &CallWidget, width: usize) -> Vec<String> {
    w.lines(width).iter().map(|l| strip_ansi(l)).collect()
}

/// No label, no widget: the frame draws nothing for it.
#[test]
fn no_widget_without_a_label() {
    let s = RegionSnapshot::default();
    assert_eq!(CallWidget::new(&s, 0, Instant::now()), None);
}

/// Header with the spinner frame of the count, the body newest first as given, the status row last.
#[test]
fn header_then_body_in_order_then_status() {
    let base = Instant::now();
    let w = CallWidget::new(
        &snap(&["newest", "middle", "oldest"], Some(base)),
        3,
        base + Duration::from_secs(3),
    )
    .unwrap();
    let rows = plain(&w, 80);
    assert_eq!(
        rows,
        vec![
            format!("{} [shell sleep 4]", SPINNER_FRAMES[3]),
            "  newest".to_owned(),
            "  middle".to_owned(),
            "  oldest".to_owned(),
            "  ⎿ 3s".to_owned(),
        ]
    );
}

/// The spinner wraps at its frame count.
#[test]
fn spinner_count_wraps() {
    let w = CallWidget::new(&snap(&[], None), SPINNER_FRAMES.len() + 1, Instant::now()).unwrap();
    assert!(plain(&w, 80)[0].starts_with(SPINNER_FRAMES[1]));
}

/// No clock, no status row.
#[test]
fn no_status_row_without_a_clock() {
    let w = CallWidget::new(&snap(&["a"], None), 0, Instant::now()).unwrap();
    assert_eq!(w.lines(80).len(), 2);
}

/// The detail leads the status row; a paused clock freezes where it stopped.
#[test]
fn detail_and_paused_clock() {
    let base = Instant::now();
    let mut s = snap(&[], Some(base));
    s.detail = "4.2 KB".to_owned();
    s.paused_at = Some(base + Duration::from_secs(5));
    let w = CallWidget::new(&s, 0, base + Duration::from_secs(60)).unwrap();
    assert_eq!(plain(&w, 80)[1], "  ⎿ 4.2 KB · 5s");
}

/// Body and status rows are cut to `width − 4` (four columns at the least) at every width; the
/// header is the caller's and is never cut here.
#[test]
fn rows_cut_at_width_less_four() {
    let base = Instant::now();
    let long = "x".repeat(200);
    let mut s = snap(&[&long], Some(base));
    s.detail = "d".repeat(200);
    let w = CallWidget::new(&s, 0, base).unwrap();
    for width in [0usize, 20, 80] {
        let room = width.saturating_sub(4).max(4);
        let rows = plain(&w, width);
        assert_eq!(rows.len(), 3);
        for r in &rows[1..] {
            assert_eq!(str_width(r), 2 + room, "width {width}: {r:?}");
            assert!(r.ends_with('…'), "width {width}: {r:?}");
        }
        assert_eq!(ansi_width(&w.lines(width)[0]), str_width(&rows[0]));
    }
}
