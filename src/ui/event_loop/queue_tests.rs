#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP46 queue-lifecycle suite: FIFO drain, ↑ pop-back (LIFO), the hint laws at the
//! model level, the steering take, and the interrupt fold-back (`model_test.go`).
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
//! in-file (formerly a `#[path]`-mounted `tests/queue.rs` of the terminal crate; merged 2026-09-02).

use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex};

use crate::text::ansi::strip_sgr;
use crate::ui::event_loop::{LoopShared, Model};
use crate::ui::msgs::UiMsg;
use crate::ui::region::{Emit, Region};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio_util::sync::CancellationToken;

/// A loop model over the test-seam region at 80×24 (Go `newTestModel`).
fn test_model() -> Model {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    let region = Arc::new(Mutex::new(Region::new(
        Emit::Test(Box::new(|_, _| {})),
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    Model::new(LoopShared {
        width,
        height,
        region,
    })
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn type_text(m: &mut Model, s: &str) {
    for ch in s.chars() {
        m.handle_key(key(KeyCode::Char(ch)));
    }
}

fn enter(m: &mut Model) {
    m.handle_key(key(KeyCode::Enter));
}

fn up(m: &mut Model) {
    m.handle_key(key(KeyCode::Up));
}

fn ctrl_c(m: &mut Model) {
    m.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
}

/// SGR-stripped frame rows (the Go `stripSGR(content(m))` instrument).
fn plain(m: &mut Model) -> Vec<String> {
    m.frame_view().rows.iter().map(|r| strip_sgr(r)).collect()
}

fn find(rows: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
    rows.iter().position(|r| pred(r))
}

/// Submits with no waiter queue (visible as `»` rows above the separator); the next
/// read request drains the queue in order (FIFO).
// Go: model_test.go:82
#[test]
fn test_queue_then_drain() {
    let mut m = test_model();
    type_text(&mut m, "first");
    enter(&mut m);
    type_text(&mut m, "second");
    enter(&mut m);
    assert_eq!(m.queue, vec!["first", "second"], "queue = {:?}", m.queue);
    let rows = plain(&mut m);
    let first = find(&rows, |r| r.contains("» first")).expect("queue row » first missing");
    assert!(
        rows.iter().any(|r| r.contains("» second")),
        "queue row » second missing:\n{rows:#?}"
    );
    let sep = find(&rows, |r| r.starts_with("───")).expect("separator missing");
    assert!(first < sep, "queue not above the separator:\n{rows:#?}");

    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::ReadReq { id: 1, reply: tx });
    let drained = rx
        .try_recv()
        .expect("read not served")
        .expect("read failed");
    assert_eq!(drained.text, "first", "drained {drained:?}, want first");
    assert_eq!(m.queue, vec!["second"], "queue after drain = {:?}", m.queue);
}

/// ↑ on an empty composer pops the NEWEST queued item (LIFO, one per press); with
/// text present ↑ is history navigation and the queue is untouched; the bottom queue
/// row advertises `· ↑ edit`; a resubmit re-queues at the tail.
// Go: model_test.go:1970
#[test]
fn test_queue_pop_with_up_arrow() {
    let mut m = test_model();
    type_text(&mut m, "first");
    enter(&mut m);
    type_text(&mut m, "second");
    enter(&mut m);

    let joined = plain(&mut m).join("\n");
    assert!(
        joined.contains("» second · ↑ edit"),
        "bottom queue row must advertise ↑ edit:\n{joined}"
    );

    up(&mut m); // empty composer → pops "second"
    assert_eq!(
        m.composer.value(),
        "second",
        "composer must hold the popped newest item"
    );
    assert_eq!(m.queue, vec!["first"], "queue = {:?}", m.queue);

    up(&mut m); // composer non-empty → history navigation, queue untouched
    assert_eq!(
        m.queue.len(),
        1,
        "history nav must not pop the queue: {:?}",
        m.queue
    );

    // Clear and pop the remaining item too (Go: setDraft("") + histIdx reset).
    m.composer.set_value("");
    m.composer.end_history_nav();
    up(&mut m);
    assert_eq!(m.composer.value(), "first", "second pop");
    assert!(m.queue.is_empty(), "queue = {:?}", m.queue);

    // Resubmitting re-queues at the tail.
    enter(&mut m);
    assert_eq!(m.queue, vec!["first"], "requeue = {:?}", m.queue);
}

/// The overflow row (`"+N more"`) carries the hint when newest items are hidden —
/// never a visible row.
// Go: model_test.go:2011
#[test]
fn test_queue_hint_on_overflow_row() {
    let mut m = test_model();
    for s in ["a", "b", "c", "d", "e"] {
        type_text(&mut m, s);
        enter(&mut m);
    }
    let joined = plain(&mut m).join("\n");
    assert!(
        joined.contains("+2 more · ↑ edit"),
        "overflow row must advertise ↑ edit:\n{joined}"
    );
    assert!(
        !joined.contains("» c · ↑ edit"),
        "hidden-newest case must not hint on a visible row:\n{joined}"
    );
}

/// Steering drain: the contiguous non-command prefix is taken; a slash command stops
/// the take; command-headed and empty queues take nothing; queue rows re-render.
// Go: model_test.go:1867
#[test]
fn test_take_queued_messages() {
    let mut m = test_model();
    for s in ["first", "second", "/model", "third"] {
        type_text(&mut m, s);
        enter(&mut m);
    }

    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TakeQueued { reply: tx });
    let taken = rx.try_recv().expect("take not served");
    assert_eq!(taken.len(), 2, "taken = {taken:?}, want [first second]");
    assert_eq!(taken[0].text, "first");
    assert_eq!(taken[1].text, "second");
    assert_eq!(
        m.queue,
        vec!["/model", "third"],
        "queue after take = {:?}",
        m.queue
    );
    let joined = plain(&mut m).join("\n");
    assert!(
        !joined.contains("first") && joined.contains("/model"),
        "queue rows must reflect the take:\n{joined}"
    );

    // Command at the head: nothing to take.
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TakeQueued { reply: tx });
    let taken = rx.try_recv().expect("take not served");
    assert!(taken.is_empty(), "command-headed queue must take nothing");

    // Empty queue: nothing.
    m.queue.clear();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TakeQueued { reply: tx });
    let taken = rx.try_recv().expect("take not served");
    assert!(taken.is_empty(), "empty queue must take nothing");
}

/// Ctrl+C with an active scope fires the TURN cancel and folds queued submits (plus
/// the half-typed draft) into a multi-line composer draft, atomically.
// Go: model_test.go:154
#[test]
fn test_interrupt_restores_queue_to_draft() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    type_text(&mut m, "queued-A");
    enter(&mut m);
    type_text(&mut m, "queued-B");
    enter(&mut m);
    type_text(&mut m, "half");

    ctrl_c(&mut m);
    assert!(turn.is_cancelled(), "turn cancel not fired");
    assert!(
        m.queue.is_empty() && m.cancels.is_empty(),
        "queue/cancels not cleared: {:?} {}",
        m.queue,
        m.cancels.len()
    );
    assert_eq!(
        m.composer.value(),
        "queued-A\nqueued-B\nhalf",
        "draft mismatch"
    );
    assert_eq!(
        m.composer.rows(80).len(),
        3,
        "draft height = {}, want 3",
        m.composer.rows(80).len()
    );
}

/// ESC fires only the TOP (tool) scope, leaving the turn scope in place.
// Go: model_test.go:195
#[test]
fn test_esc_cancels_innermost_scope() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    let tool = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    m.apply(UiMsg::ScopePush(tool.clone()));
    m.handle_key(key(KeyCode::Esc));
    assert!(
        tool.is_cancelled() && !turn.is_cancelled(),
        "esc fired turn={} tool={}, want tool only",
        turn.is_cancelled(),
        tool.is_cancelled()
    );
    assert_eq!(m.cancels.len(), 1, "turn scope must survive");
}
