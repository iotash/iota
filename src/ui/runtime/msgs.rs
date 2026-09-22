//! `UiMsg` — the handle→loop mailbox vocabulary (`TUI_CONTRACTS` §5), plus the region's
//! live-publish seam implemented over it.
//!
//! The mailbox is a plain unbounded `std::sync::mpsc` channel: producers are human- and
//! turn-paced, the one bulk producer (region overflow) is already chunked below the
//! screen height, and no send toward the loop may ever block (`TUI_DESIGN` §3 — the
//! documented backpressure choice). Global ordering is the Go publish law's Rust shape:
//! writers serialize on the region `Mutex`, and this FIFO preserves their arrival order
//! for the single consumer. Reply channels are `tokio::sync::oneshot` senders stored in
//! the message; the loop's sends ignore the result (a dropped receiver IS a revoked
//! waiter), and the explicit [`UiMsg::ReadCancel`] still clears a parked waiter eagerly.

use std::sync::mpsc;

use crate::ui::facade::{
    Input, ProgressState, StatusData, Suggestion, TabbedResult, TabbedSpec, UiError,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::ui::render::region::{LivePublish, RegionSnapshot};

/// One message from the facade handle (WP45) to the `"iota-tui"` loop thread.
pub(crate) enum UiMsg {
    /// One pre-chunked scrollback batch to insert above the frame, in arrival order
    /// (region overflow, ≤ `max(2, screen_height/2)` rows per batch — T-06).
    Scrollback(Vec<String>),
    /// A fresh staging-window snapshot; always follows its mutation's overflow batches.
    Region(RegionSnapshot),
    /// Replaces the status-row data.
    Status(StatusData),
    /// Replaces the window title (already sanitized by the facade — a security
    /// property; the loop emits the OSC only on change).
    Title(String),
    /// Installs the slash-command table (completion + suggestion row).
    Commands(Vec<Suggestion>),
    /// Replaces the running background jobs (the status row's job segment, and its one-second tick).
    Jobs(Vec<crate::shell::jobs::JobInfo>),
    /// Starts a busy phase: the label mounts on the status row, the phase clock
    /// restarts, any previous detail clears (model.go busyOnMsg).
    BusyOn(String),
    /// Updates the busy detail only — never the phase clock (model.go busyDetailMsg).
    BusyDetail(String),
    /// Ends the busy phase.
    BusyOff,
    /// Pushes a cancel scope (bottom index 0 = the turn).
    ScopePush(CancellationToken),
    /// Pops the top cancel scope.
    ScopePop,
    /// Parks a `read_input` waiter — or serves it straight from the queue head
    /// (FIFO type-ahead drain, model.go readReqMsg).
    ReadReq {
        /// Caller-unique id; [`UiMsg::ReadCancel`] revokes only the SAME id.
        id: u64,
        /// Receives the input (or `Err(Interrupted)` on an idle Ctrl+C/Ctrl+D).
        reply: oneshot::Sender<Result<Input, UiError>>,
    },
    /// Revokes the parked waiter with this id (the caller gave up — eager clear;
    /// a different id means a newer waiter already replaced it).
    ReadCancel {
        /// The id passed with the matching [`UiMsg::ReadReq`].
        id: u64,
    },
    /// Host-injected input: served straight to a parked waiter, else appended to the
    /// type-ahead queue (the composer's draft is never touched).
    Enqueue(Input),
    /// Steering drain: pops the contiguous non-`'/'` prefix of the queue
    /// (a slash command stops the take — model.go takeQueuedMsg).
    TakeQueued {
        /// Receives the drained inputs (empty when the head is a command or none).
        reply: oneshot::Sender<Vec<Input>>,
    },
    /// Opens a tabbed surface below the composer (the handle flushed the staging
    /// tail FIRST — mailbox FIFO lands the flush before this open).
    TabbedOpen {
        /// The surface spec.
        spec: TabbedSpec,
        /// Receives the commit/cancel result when the surface closes.
        reply: oneshot::Sender<TabbedResult>,
    },
    /// Closes whatever surface is open as cancelled (the blocking caller's
    /// cancel token fired).
    SurfaceCancel,
    /// Replaces the progress state (model.go:215 progressMsg).
    Progress(ProgressState),
    /// Attention ping text, already sanitized (model.go:217 notifyMsg).
    Notify(String),
    /// Re-shade the composer for a background flip.
    DarkBackground(bool),
    /// Terminates the loop thread. `close()` flushes the region first, so the
    /// mailbox FIFO lands the flushed scrollback ahead of this (ui.go Close).
    Quit,
}

/// The [`LivePublish`] impl over the loop mailbox (the WP43 seam): one
/// [`UiMsg::Scrollback`] per chunk, then the [`UiMsg::Region`] snapshot — all sent while
/// the caller holds the region `Mutex`, preserving Go's publish-under-the-lock ordering.
pub(crate) struct MailboxPublish(pub(crate) mpsc::Sender<UiMsg>);

impl LivePublish for MailboxPublish {
    fn scrollback(&self, rows: Vec<String>) {
        // A closed loop drops the message; the producer must never block or fail.
        let _ = self.0.send(UiMsg::Scrollback(rows));
    }

    fn region(&self, snap: RegionSnapshot) {
        let _ = self.0.send(UiMsg::Region(snap));
    }
}
