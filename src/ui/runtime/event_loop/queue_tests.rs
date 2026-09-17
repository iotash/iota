#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! WP46 queue-lifecycle suite: FIFO drain, ↑ pop-back (LIFO), the hint laws at the
//! model level, the steering take, and the interrupt fold-back (`model_test.go`).
//!
//! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
//! in-file (formerly a `#[path]`-mounted `tests/queue.rs` of the terminal crate; merged 2026-09-02).

use crate::ui::facade::{Input, InputKind};
use crate::ui::runtime::msgs::UiMsg;
use crate::ui::testutil::{ctrl_c, enter, key, plain, test_model, type_text, up};
use crossterm::event::KeyCode;
use tokio_util::sync::CancellationToken;

fn find(rows: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
    rows.iter().position(|r| pred(r))
}

/// Submits with no waiter queue (visible as `»` rows above the separator); the next
/// read request drains the queue in order (FIFO).
#[test]
fn queued_messages_drain_in_order() {
    let mut m = test_model();
    type_text(&mut m, "first");
    enter(&mut m);
    type_text(&mut m, "second");
    enter(&mut m);
    assert_eq!(
        m.queue_rows(),
        vec!["first", "second"],
        "queue = {:?}",
        m.queue_rows()
    );
    let rows = plain(&mut m);
    let first = find(&rows, |r| r.contains("» first")).expect("queue row » first missing");
    assert!(
        rows.iter().any(|r| r.contains("» second")),
        "queue row » second missing:\n{rows:#?}"
    );
    let sep = find(&rows, |r| {
        r.starts_with(&crate::ui::render::frame::SEPARATOR_GLYPH.repeat(3))
    })
    .expect("separator missing");
    assert!(first < sep, "queue not above the separator:\n{rows:#?}");

    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::ReadReq { id: 1, reply: tx });
    let drained = rx
        .try_recv()
        .expect("read not served")
        .expect("read failed");
    assert_eq!(drained.text, "first", "drained {drained:?}, want first");
    assert_eq!(
        m.queue_rows(),
        vec!["second"],
        "queue after drain = {:?}",
        m.queue_rows()
    );
}

/// ↑ on an empty composer pops the NEWEST queued item (LIFO, one per press); with
/// text present ↑ is history navigation and the queue is untouched; the bottom queue
/// row advertises `· ↑ edit`; a resubmit re-queues at the tail.
#[test]
fn up_pops_the_last_queued_message_back_into_the_draft() {
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
    assert_eq!(
        m.queue_rows(),
        vec!["first"],
        "queue = {:?}",
        m.queue_rows()
    );

    up(&mut m); // composer non-empty → history navigation, queue untouched
    assert_eq!(
        m.queue.len(),
        1,
        "history nav must not pop the queue: {:?}",
        m.queue_rows()
    );

    // Clear and pop the remaining item too (Go: setDraft("") + histIdx reset).
    m.composer.set_value("");
    m.composer.end_history_nav();
    up(&mut m);
    assert_eq!(m.composer.value(), "first", "second pop");
    assert!(m.queue.is_empty(), "queue = {:?}", m.queue_rows());

    // Resubmitting re-queues at the tail.
    enter(&mut m);
    assert_eq!(
        m.queue_rows(),
        vec!["first"],
        "requeue = {:?}",
        m.queue_rows()
    );
}

/// The overflow row (`"+N more"`) carries the hint when newest items are hidden —
/// never a visible row.
#[test]
fn the_queue_overflow_row_carries_the_hint() {
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
#[test]
fn the_steering_take_lifts_the_queued_messages_and_leaves_commands() {
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
        m.queue_rows(),
        vec!["/model", "third"],
        "queue after take = {:?}",
        m.queue_rows()
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
#[test]
fn an_interrupt_folds_the_queue_back_into_the_draft() {
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
        m.queue_rows(),
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
#[test]
fn esc_cancels_the_innermost_scope_only() {
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

/// One host notice, in the shape `repl::run::job_notice` builds.
fn notice(headline: &str, body: &str) -> Input {
    Input {
        display: headline.to_owned(),
        text: format!("{headline}\n{body}"),
        kind: InputKind::Notice,
    }
}

/// New (phase C): an injected notice IS an input — a parked `read_input` gets it at once (this is what
/// wakes an idle loop), and with nobody parked it queues behind what is already typed ahead.
#[test]
fn enqueue_answers_a_parked_reader_else_queues() {
    let mut m = test_model();

    // Nobody parked: it joins the queue and shows its ONE headline as a `»` row.
    m.apply(UiMsg::Enqueue(notice(
        "[background job b1 finished: exit 0 after 2s] make test",
        "all green",
    )));
    assert_eq!(
        m.queue_rows(),
        vec!["[background job b1 finished: exit 0 after 2s] make test"],
        "queue = {:?}",
        m.queue_rows()
    );
    let rows = plain(&mut m);
    assert!(
        find(&rows, |r| r.contains("» [background job b1 finished")).is_some(),
        "the notice row is missing:\n{rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("all green")),
        "the job's output must not reach the frame:\n{rows:#?}"
    );

    // The next read drains it, whole text and all.
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::ReadReq { id: 1, reply: tx });
    let got = rx.try_recv().expect("read not served").expect("input");
    assert_eq!(got.kind, InputKind::Notice);
    assert!(got.text.ends_with("\nall green"), "{:?}", got.text);
    assert!(m.queue.is_empty());

    // A PARKED reader (the idle loop) is answered directly — the wake-up.
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::ReadReq { id: 2, reply: tx });
    assert!(rx.try_recv().is_err(), "the reader should be parked");
    m.apply(UiMsg::Enqueue(notice(
        "[background job b2 finished: exit 1 after 3s] lint",
        "boom",
    )));
    let got = rx
        .try_recv()
        .expect("the parked reader was not woken")
        .expect("input");
    assert_eq!(
        got.display,
        "[background job b2 finished: exit 1 after 3s] lint"
    );
    assert!(m.queue.is_empty(), "a served notice must not also queue");
}

/// New (phase C): the queue is shared, the ownership is not. A notice never becomes the user's draft — ↑
/// skips it, and an interrupt leaves it queued instead of folding it into the composer.
#[test]
fn a_notice_is_never_the_users_draft() {
    let mut m = test_model();
    let turn = CancellationToken::new();
    m.apply(UiMsg::ScopePush(turn.clone()));
    type_text(&mut m, "typed-A");
    enter(&mut m);
    m.apply(UiMsg::Enqueue(notice(
        "[background job b1 finished: exit 0 after 2s] make test",
        "all green",
    )));
    type_text(&mut m, "typed-B");
    enter(&mut m);

    // ↑ on an empty composer pops the newest TYPED entry, stepping over the notice.
    up(&mut m);
    assert_eq!(m.composer.value(), "typed-B");
    assert_eq!(
        m.queue_rows(),
        vec![
            "typed-A",
            "[background job b1 finished: exit 0 after 2s] make test"
        ],
        "the notice must stay put: {:?}",
        m.queue_rows()
    );

    // Ctrl+C folds only what was typed; the notice is still queued and still deliverable.
    ctrl_c(&mut m);
    assert!(turn.is_cancelled());
    assert_eq!(m.composer.value(), "typed-A\ntyped-B");
    assert_eq!(
        m.queue_rows(),
        vec!["[background job b1 finished: exit 0 after 2s] make test"],
        "an interrupt must not discard a notice: {:?}",
        m.queue_rows()
    );
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::ReadReq { id: 1, reply: tx });
    let got = rx.try_recv().expect("read not served").expect("input");
    assert_eq!(got.kind, InputKind::Notice);
}

/// New (phase C): a steering drain takes a notice like any non-command entry — and a queued slash command
/// still stops the take in front of it.
#[test]
fn a_notice_is_taken_by_the_steering_drain() {
    let mut m = test_model();
    m.apply(UiMsg::Enqueue(notice(
        "[background job b1 finished: exit 0 after 2s] make test",
        "all green",
    )));
    type_text(&mut m, "/model");
    enter(&mut m);
    m.apply(UiMsg::Enqueue(notice(
        "[background job b2 finished: exit 0 after 1s] lint",
        "clean",
    )));

    let (tx, mut rx) = tokio::sync::oneshot::channel();
    m.apply(UiMsg::TakeQueued { reply: tx });
    let taken = rx.try_recv().expect("take not served");
    assert_eq!(taken.len(), 1, "the command must stop the take: {taken:?}");
    assert_eq!(taken[0].kind, InputKind::Notice);
    assert_eq!(
        m.queue_rows(),
        vec![
            "/model",
            "[background job b2 finished: exit 0 after 1s] lint"
        ]
    );
}
