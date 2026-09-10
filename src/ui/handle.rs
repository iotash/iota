//! `TuiHandle` — the facade impl over the loop mailbox (`TUI_DESIGN` §3): the blocking
//! calls' oneshot three-way-select discipline with the eager revoke, every
//! fire-and-forget verb, `BusyGuard`/`ScopeGuard` wiring, `start_stream`'s turn-scope
//! push, `close` (flush → quit → join via `spawn_blocking`), and the `TermGuard`
//! raw-mode restore.
//!
//! **Reply-channel discipline** (Go's "buffered(1) + three-way select" law, ui.go:186):
//! each blocking call creates a `tokio::sync::oneshot`, sends its request message, then
//! selects over {the reply; the caller's cancel token → send the revoke message
//! (`ReadCancel`/`SurfaceCancel`) and resolve the cancelled shape; `done` →
//! `Err(Closed)`}. The loop stores reply senders as `Option` and `.take()`s them, so a
//! double send is unrepresentable; a dropped receiver IS a revoked waiter (send results
//! are ignored, never unwrapped). **Shutdown fails all waiters:** when the loop thread
//! exits, its `Model` drops every parked sender (receivers resolve `Err(Closed)`) and
//! `done` is cancelled (pending selects resolve). Liveness at idle is wart W10: the
//! loop's poll deadline is always finite, so a request or `Quit` posted while the loop
//! is fully idle drains within one `IDLE_POLL_MAX`.
//!
//! **Ordering:** fire-and-forget verbs never block — region verbs publish under the
//! shared `Mutex<Region>` (chunked `Scrollback` batches, then the snapshot), pure loop
//! verbs are one mailbox send; the mailbox FIFO preserves arrival order for the single
//! consumer (the Go publish-under-the-lock law's Rust shape).

use std::io::{self, Write};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::sync::lock;
use std::thread;

use crate::BoxFuture;
use crate::text::ansi::wrap_by_width;
use crate::text::width::str_width;
use crate::ui::facade::{
    BusyGuard, Input, ProgressState, ScopeGuard, StatusData, Suggestion, TabbedResult, TabbedSpec,
    Ui, UiError, UiStreamSink, sanitize_window_title,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::event_loop::{CrosstermEvents, EventSource, LoopShared, run_loop};
use super::msgs::{MailboxPublish, UiMsg};
use super::region::{Emit, Region};
use super::sink::StreamSink;
use super::term::Term;
use super::theme::{RESET, REV_ON};
use super::{Tui, TuiOptions};

/// Restores the terminal — bracketed paste off, cursor shown, raw mode off — exactly
/// once, on `Drop`. The loop thread holds it across `run_loop` so EVERY exit path
/// (clean `Quit`, a draw error) restores; `Tui::start`'s failure paths restore through
/// it too. `panic = "abort"` means no unwind-based restore — accepted and documented
/// (`stty sane` recovers a killed session; the no-unwrap/no-panic lint wall makes an
/// in-code panic unreachable).
pub(crate) struct TermGuard(());

impl TermGuard {
    /// Arms the restore; call right after `enable_raw_mode` succeeded.
    pub(crate) fn new() -> Self {
        Self(())
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        // Best-effort: restore errors are unreportable on an exit path.
        let mut out = io::stdout();
        let _ = crossterm::execute!(
            out,
            crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The facade handle (`TUI_CONTRACTS` §2): shared by every consumer, talks to the
/// `"iota-tui"` loop thread through the mailbox and to the staging window through the
/// shared `Mutex<Region>`.
pub(crate) struct TuiHandle {
    /// The loop mailbox (unbounded; producers are human/turn-paced — the documented
    /// backpressure choice).
    tx: mpsc::Sender<UiMsg>,
    /// Terminal width in columns (starts 80; the loop's resize pass stores).
    width: Arc<AtomicU16>,
    /// Terminal height in rows (starts 24 — "24 until the first resize event").
    height: Arc<AtomicU16>,
    /// The staging window; every region verb publishes under this lock.
    region: Arc<Mutex<Region>>,
    /// Cancelled when the loop thread exits (background reporters select on it).
    done: CancellationToken,
    /// The loop's io error, stored on exit; [`Ui::close`] returns it once.
    err: Mutex<Option<io::Error>>,
    /// The loop thread's join handle; [`Ui::close`] takes it exactly once.
    join: Mutex<Option<thread::JoinHandle<()>>>,
    /// `read_input` waiter ids — [`UiMsg::ReadCancel`] revokes only the SAME id.
    next_read_id: AtomicU64,
}

/// Spawns the named `"iota-tui"` loop thread over an already-built terminal and event
/// source, and wires the facade handle around its mailbox. `restore` rides into the
/// thread so the terminal is restored on every exit path (`None` for headless tests).
pub(crate) fn spawn<W, E>(
    term: Term<W>,
    events: E,
    width: Arc<AtomicU16>,
    height: Arc<AtomicU16>,
    restore: Option<TermGuard>,
) -> io::Result<Arc<TuiHandle>>
where
    W: Write + Send + 'static,
    E: EventSource + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let region = Arc::new(Mutex::new(Region::new(
        Emit::Live {
            tx: Box::new(MailboxPublish(tx.clone())),
        },
        Arc::clone(&width),
        Arc::clone(&height),
    )));
    let handle = Arc::new(TuiHandle {
        tx,
        width: Arc::clone(&width),
        height: Arc::clone(&height),
        region: Arc::clone(&region),
        done: CancellationToken::new(),
        err: Mutex::new(None),
        join: Mutex::new(None),
        next_read_id: AtomicU64::new(1),
    });
    let shared = LoopShared {
        width,
        height,
        region,
    };
    let thread_handle = Arc::clone(&handle);
    let join = thread::Builder::new()
        .name("iota-tui".to_owned())
        .spawn(move || {
            let result = run_loop(&rx, events, term, shared);
            // Every exit path: restore the terminal FIRST, then fail the waiters —
            // dropping the loop's Model already dropped every parked reply sender
            // (their receivers resolve Err(Closed)), and cancelling `done` resolves
            // the callers' pending selects.
            drop(restore);
            if let Err(e) = result {
                *lock(&thread_handle.err) = Some(e);
            }
            thread_handle.done.cancel();
        })?;
    *lock(&handle.join) = Some(join);
    Ok(handle)
}

/// `Tui::start` (`TUI_CONTRACTS` §5): enables raw mode + bracketed paste, queries the
/// initial size into the shared atomics (bubbletea sent `WindowSizeMsg` automatically;
/// crossterm is asked once here), anchors the inline viewport at the current cursor
/// row, and spawns the `"iota-tui"` loop thread owning the terminal (one OS thread —
/// `event::poll` blocks, and wart W8 wants a single crossterm owner).
pub(crate) fn start(opts: TuiOptions) -> io::Result<Tui> {
    crossterm::terminal::enable_raw_mode()?;
    // From here on every failure path restores the terminal via the guard.
    let restore = TermGuard::new();
    crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste)?;
    let (w, h) = crossterm::terminal::size()?;
    let width = Arc::new(AtomicU16::new(if w > 0 { w } else { 80 }));
    let height = Arc::new(AtomicU16::new(if h > 0 { h } else { 24 }));
    // The pre-raw-mode cursor row anchors the viewport (raw mode is on, so the DSR
    // round-trip cooperates with crossterm's event reader — wart W8).
    let (_, row) = crossterm::cursor::position()?;
    let term = Term::new(Box::new(io::stdout), 1, row, None)?;
    let handle = spawn(term, CrosstermEvents, width, height, Some(restore))?;
    // The probed tone is the loop's first message: the input shade follows it from frame one.
    handle.set_dark_background(opts.dark);
    Ok(Tui { handle })
}

impl Ui for TuiHandle {
    fn read_input<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Input, UiError>> {
        Box::pin(async move {
            let id = self.next_read_id.fetch_add(1, Ordering::Relaxed);
            let (reply_tx, reply_rx) = oneshot::channel();
            if self
                .tx
                .send(UiMsg::ReadReq {
                    id,
                    reply: reply_tx,
                })
                .is_err()
            {
                return Err(UiError::Closed);
            }
            tokio::select! {
                biased;
                r = reply_rx => r.unwrap_or(Err(UiError::Closed)),
                () = cancel.cancelled() => {
                    // Eager revoke: clear the parked waiter now so a later submit
                    // queues instead of vanishing into a dead channel (the loop
                    // revokes only the SAME id — a newer waiter is untouched).
                    let _ = self.tx.send(UiMsg::ReadCancel { id });
                    Err(UiError::Interrupted)
                }
                () = self.done.cancelled() => Err(UiError::Closed),
            }
        })
    }

    fn tabbed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        spec: TabbedSpec,
    ) -> BoxFuture<'a, Result<TabbedResult, UiError>> {
        Box::pin(async move {
            // Flush the staging tail FIRST (ui.go:334-341): opening a surface grows
            // the frame like a resize reflow, and the ghostable rows must reach real
            // scrollback; the mailbox FIFO lands the flush before the open.
            lock(&self.region).flush_tail();
            let (reply_tx, reply_rx) = oneshot::channel();
            if self
                .tx
                .send(UiMsg::TabbedOpen {
                    spec,
                    reply: reply_tx,
                })
                .is_err()
            {
                return Err(UiError::Closed);
            }
            tokio::select! {
                biased;
                r = reply_rx => r.map_err(|_| UiError::Closed),
                () = cancel.cancelled() => {
                    let _ = self.tx.send(UiMsg::SurfaceCancel);
                    Err(UiError::Interrupted)
                }
                () = self.done.cancelled() => Err(UiError::Closed),
            }
        })
    }

    fn take_queued_messages(&self) -> BoxFuture<'_, Vec<Input>> {
        Box::pin(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            if self.tx.send(UiMsg::TakeQueued { reply: reply_tx }).is_err() {
                return Vec::new();
            }
            tokio::select! {
                biased;
                r = reply_rx => r.unwrap_or_default(),
                // Go returns nil once the Program died (ui.go:207-217).
                () = self.done.cancelled() => Vec::new(),
            }
        })
    }

    fn close(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            // Flush the staged tail into scrollback (the transcript must be
            // complete; preview rows and residue are display-only and drop), THEN
            // send Quit — the mailbox FIFO lands the flushed batches ahead of it,
            // and wart W10 guarantees a fully idle loop drains Quit within one
            // 50ms poll deadline (the close-at-idle deadlock regression).
            lock(&self.region).flush();
            let _ = self.tx.send(UiMsg::Quit);
            self.done.cancelled().await;
            let join = lock(&self.join).take();
            if let Some(handle) = join {
                // Join off the async worker (a thread join is blocking I/O); the
                // thread is already past `done.cancel()`, so this is brief. A
                // panicked loop thread is unreachable under the lint wall — the
                // join result is deliberately ignored (the stored io error is the
                // real outcome).
                let _ = tokio::task::spawn_blocking(move || handle.join()).await;
            }
            // A second close finds the slots empty and returns Ok(()).
            match lock(&self.err).take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        })
    }

    fn enqueue(&self, input: Input) {
        // Fire-and-forget like every other injection: a closed loop drops it, and the caller (a job
        // supervisor task) must never block on the UI.
        let _ = self.tx.send(UiMsg::Enqueue(input));
    }

    fn print_lines(&self, lines: Vec<String>) {
        if lines.is_empty() {
            return; // ui.go:220-222
        }
        lock(&self.region).commit(lines);
    }

    fn user_block(&self, display: &str) {
        // Full-width reversed rows with the "❯ " gutter (ui.go:224-243).
        let w = usize::from(self.width.load(Ordering::Relaxed)).max(8);
        let gutter = 2;
        let styled: Vec<String> = wrap_by_width(display, w - gutter)
            .into_iter()
            .enumerate()
            .map(|(i, row)| {
                let g = if i == 0 { "❯ " } else { "  " };
                let pad = (w - gutter).saturating_sub(str_width(&row));
                format!("{REV_ON}{g}{row}{}{RESET}", " ".repeat(pad))
            })
            .collect();
        lock(&self.region).commit(styled);
    }

    fn start_stream(&self, cancel: CancellationToken) -> Box<dyn UiStreamSink> {
        // The TURN cancel scope (stack index 0) — sink.done() pops it (ui.go:248).
        let _ = self.tx.send(UiMsg::ScopePush(cancel));
        let tx = self.tx.clone();
        Box::new(StreamSink::new(Arc::clone(&self.region), move || {
            let _ = tx.send(UiMsg::ScopePop);
        }))
    }

    fn busy(&self, label: &str) -> BusyGuard {
        let _ = self.tx.send(UiMsg::BusyOn(label.to_owned()));
        let tx = self.tx.clone();
        BusyGuard::new(move || {
            let _ = tx.send(UiMsg::BusyOff);
        })
    }

    fn busy_detail(&self, detail: &str) {
        let _ = self.tx.send(UiMsg::BusyDetail(detail.to_owned()));
    }

    fn push_cancel_scope(&self, cancel: CancellationToken) -> ScopeGuard {
        let _ = self.tx.send(UiMsg::ScopePush(cancel));
        let tx = self.tx.clone();
        ScopeGuard::new(move || {
            let _ = tx.send(UiMsg::ScopePop);
        })
    }

    fn set_status(&self, s: StatusData) {
        let _ = self.tx.send(UiMsg::Status(s));
    }

    fn set_title(&self, title: &str) {
        // Sanitized HERE — a security property (ui.go:297-313): the loop emits the
        // title as a raw OSC sequence, so control bytes must never reach it.
        let _ = self.tx.send(UiMsg::Title(sanitize_window_title(title)));
    }

    fn set_slash_commands(&self, cmds: Vec<Suggestion>) {
        let _ = self.tx.send(UiMsg::Commands(cmds));
    }

    fn call_preview(&self, label: &str) {
        lock(&self.region).open_call_preview(label);
    }

    fn call_detail(&self, detail: &str) {
        lock(&self.region).set_call_detail(detail);
    }

    fn call_line(&self, line: &str) {
        lock(&self.region).preview_line(line);
    }

    fn close_preview(&self) {
        lock(&self.region).close_preview();
    }

    fn pause_clock(&self) {
        lock(&self.region).pause_clock();
    }

    fn resume_clock(&self) {
        lock(&self.region).resume_clock();
    }

    fn call_body(&self, rows: Vec<String>) {
        // Like `call_detail`: straight into the region, no mailbox hop (handle.rs precedent).
        lock(&self.region).set_call_body(rows);
    }

    fn set_progress(&self, s: ProgressState) {
        let _ = self.tx.send(UiMsg::Progress(s));
    }

    fn notify(&self, text: &str) {
        // Sanitized HERE like the title: the loop emits it as a raw OSC 9 sequence.
        let _ = self.tx.send(UiMsg::Notify(sanitize_window_title(text)));
    }

    fn set_dark_background(&self, dark: bool) {
        let _ = self.tx.send(UiMsg::DarkBackground(dark));
    }

    fn width(&self) -> u16 {
        self.width.load(Ordering::Relaxed)
    }

    fn height(&self) -> u16 {
        self.height.load(Ordering::Relaxed)
    }

    fn done(&self) -> CancellationToken {
        self.done.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! WP45 facade-contract suite: the blocking calls' three-way-select discipline over the
    //! REAL loop thread (waiter delivery, eager revoke, idle Ctrl+C, shutdown failing
    //! waiters), the surface reply path, and `close` (flush → quit → join) including the
    //! close-at-idle W10 regression.
    //!
    //! The modules under test are crate-private by design (`TUI_CONTRACTS` §5), so these tests live
    //! in-file (formerly a `#[path]`-mounted `tests/facade.rs` of the terminal crate; merged 2026-09-02).

    use std::io::{self, Write};
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::ui::event_loop::EventSource;
    use crate::ui::facade::{SelectSpec, Ui, UiError};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use tokio_util::sync::CancellationToken;

    /// A cloneable byte sink shared between the terminal stack and the assertions.
    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Scripted [`EventSource`] over a channel (the loop's poll deadline still elapses
    /// for real, so the W10 timing in these tests is genuine).
    struct ChannelEvents {
        rx: mpsc::Receiver<Event>,
        pending: Option<Event>,
    }

    impl EventSource for ChannelEvents {
        fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
            if self.pending.is_some() {
                return Ok(true);
            }
            match self.rx.recv_timeout(timeout) {
                Ok(e) => {
                    self.pending = Some(e);
                    Ok(true)
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(timeout);
                    Ok(false)
                }
            }
        }

        fn read(&mut self) -> io::Result<Event> {
            if let Some(e) = self.pending.take() {
                return Ok(e);
            }
            self.rx
                .recv()
                .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "no scripted event"))
        }
    }

    /// The facade under test: the REAL `crate::ui::handle::spawn` loop thread over a headless
    /// terminal, driven through the `Arc<TuiHandle>` exactly like a consumer.
    struct FacadeHarness {
        ui: Arc<crate::ui::handle::TuiHandle>,
        etx: mpsc::Sender<Event>,
        buf: SharedBuf,
    }

    fn start_facade() -> FacadeHarness {
        let width = Arc::new(AtomicU16::new(80));
        let height = Arc::new(AtomicU16::new(24));
        let geo = crate::ui::term::Geometry::new(80, 24);
        let buf = SharedBuf::default();
        let wtr = buf.clone();
        // start_top 19 mirrors the WP44 loop harness: the viewport starts at the bottom.
        let t =
            crate::ui::term::Term::new(Box::new(move || wtr.clone()), 1, 19, Some(geo)).unwrap();
        let (etx, erx) = mpsc::channel();
        let events = ChannelEvents {
            rx: erx,
            pending: None,
        };
        let ui = crate::ui::handle::spawn(t, events, width, height, None).unwrap();
        FacadeHarness { ui, etx, buf }
    }

    impl FacadeHarness {
        fn contents(&self) -> String {
            let mut p = vt100::Parser::new(24, 80, 500);
            p.process(&self.buf.bytes());
            p.screen().contents()
        }

        fn wait_until(&self, timeout: Duration, pred: impl Fn(&Self) -> bool) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if pred(self) {
                    return true;
                }
                thread::sleep(Duration::from_millis(5));
            }
            pred(self)
        }

        fn wait_frame(&self) {
            assert!(
                self.wait_until(Duration::from_secs(2), |h| h.contents().contains('❯')),
                "initial frame never painted:\n{}",
                self.contents()
            );
        }

        fn key(&self, code: KeyCode, mods: KeyModifiers) {
            self.etx
                .send(Event::Key(KeyEvent::new(code, mods)))
                .unwrap();
        }

        fn type_text(&self, s: &str) {
            for c in s.chars() {
                self.key(KeyCode::Char(c), KeyModifiers::NONE);
            }
        }
    }

    /// With a pending `read_input`, Enter delivers the input directly (no queueing) and
    /// clears the waiter.
    // Go: internal/ui/model_test.go:61
    #[tokio::test(flavor = "multi_thread")]
    async fn test_submit_delivers_to_waiter() {
        let h = start_facade();
        h.wait_frame();
        let ui = Arc::clone(&h.ui);
        let reader = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            ui.read_input(&cancel).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await; // let the waiter park
        h.type_text("hello");
        h.key(KeyCode::Enter, KeyModifiers::NONE);
        let got = reader.await.unwrap().expect("waiter delivered");
        assert_eq!(got.text, "hello");
        assert_eq!(got.display, "hello");
        h.ui.close().await.expect("close");
    }

    /// A `read_input` whose cancel token died revokes its waiter eagerly, so a later
    /// submit queues instead of vanishing into a dead channel — the next `read_input`
    /// drains it.
    // Go: internal/ui/model_test.go:582
    #[tokio::test(flavor = "multi_thread")]
    async fn test_read_cancel_revokes_waiter() {
        let h = start_facade();
        h.wait_frame();
        let cancel = CancellationToken::new();
        let ui = Arc::clone(&h.ui);
        let token = cancel.clone();
        let reader = tokio::spawn(async move { ui.read_input(&token).await });
        tokio::time::sleep(Duration::from_millis(100)).await; // park the waiter
        cancel.cancel();
        assert_eq!(
            reader.await.unwrap().expect_err("cancelled call"),
            UiError::Interrupted,
            "the cancel arm resolves the caller (Go returned ctx.Err())"
        );
        // Let the eager ReadCancel drain (W10 bounds it to one 50ms deadline).
        tokio::time::sleep(Duration::from_millis(200)).await;
        h.type_text("late");
        h.key(KeyCode::Enter, KeyModifiers::NONE);
        // The late submit queued; a fresh read_input drains the queue head.
        let got =
            h.ui.read_input(&CancellationToken::new())
                .await
                .expect("queued late submit drained");
        assert_eq!(got.text, "late");
        h.ui.close().await.expect("close");
    }

    /// With no cancel scopes, Ctrl+C surfaces `Err(Interrupted)` to the pending
    /// `read_input` — the caller's cue to exit (double-Ctrl+C-exits emerges from this).
    // Go: internal/ui/model_test.go:182
    #[tokio::test(flavor = "multi_thread")]
    async fn test_idle_ctrl_c_interrupts() {
        let h = start_facade();
        h.wait_frame();
        let ui = Arc::clone(&h.ui);
        let reader = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            ui.read_input(&cancel).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        h.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            reader.await.unwrap().expect_err("idle interrupt"),
            UiError::Interrupted
        );
        h.ui.close().await.expect("close");
    }

    /// Shutdown fails ALL outstanding waiters: a parked `read_input` resolves
    /// `Err(Closed)` when the loop exits, and every later blocking call fails fast.
    #[tokio::test(flavor = "multi_thread")]
    async fn waiter_fails_on_shutdown() {
        let h = start_facade();
        h.wait_frame();
        let ui = Arc::clone(&h.ui);
        let reader = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            ui.read_input(&cancel).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await; // park the waiter
        h.ui.close().await.expect("close");
        assert_eq!(
            reader
                .await
                .unwrap()
                .expect_err("shutdown fails the waiter"),
            UiError::Closed
        );
        assert_eq!(
            h.ui.read_input(&CancellationToken::new())
                .await
                .expect_err("closed ui fails immediately"),
            UiError::Closed
        );
    }

    /// The select sugar's reply path over the real loop: the surface renders BELOW the
    /// composer line, and ↓ + Enter resolve the blocked caller with the chosen index.
    // Go: internal/ui/model_test.go:110 (TestSelectBelowComposer)
    #[tokio::test(flavor = "multi_thread")]
    async fn select_below_composer_reply_path() {
        let h = start_facade();
        h.wait_frame();
        let ui = Arc::clone(&h.ui);
        let sel = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            ui.select(
                &cancel,
                SelectSpec {
                    title: "/model".to_owned(),
                    items: vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
                    cursor: 0,
                },
            )
            .await
        });
        assert!(
            h.wait_until(Duration::from_secs(2), |h| h.contents().contains("/model")),
            "surface never rendered:\n{}",
            h.contents()
        );
        let c = h.contents();
        let composer = c.find('❯').expect("composer row");
        let surface = c.find("/model").expect("surface title");
        assert!(
            composer < surface,
            "selector must render BELOW the composer:\n{c}"
        );
        h.key(KeyCode::Down, KeyModifiers::NONE);
        h.key(KeyCode::Enter, KeyModifiers::NONE);
        let r = sel.await.unwrap().expect("select reply");
        assert!(!r.cancelled);
        assert_eq!(r.index, 1);
        h.ui.close().await.expect("close");
    }

    /// ESC cancels the surface WITHOUT firing turn scopes: the blocked caller gets the
    /// cancelled shape and the pushed cancel token stays un-fired.
    // Go: internal/ui/model_test.go:136
    #[tokio::test(flavor = "multi_thread")]
    async fn test_select_esc_cancels() {
        let h = start_facade();
        h.wait_frame();
        let token = CancellationToken::new();
        let guard = h.ui.push_cancel_scope(token.clone());
        let ui = Arc::clone(&h.ui);
        let sel = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            ui.select(
                &cancel,
                SelectSpec {
                    title: "t".to_owned(),
                    items: vec!["only-item".to_owned()],
                    cursor: 0,
                },
            )
            .await
        });
        assert!(
            h.wait_until(Duration::from_secs(2), |h| h
                .contents()
                .contains("only-item")),
            "surface never rendered:\n{}",
            h.contents()
        );
        h.key(KeyCode::Esc, KeyModifiers::NONE);
        let r = sel.await.unwrap().expect("select reply");
        assert!(r.cancelled, "esc should cancel the select");
        assert!(
            !token.is_cancelled(),
            "surface ESC must not fire the turn cancel scope"
        );
        guard.pop();
        h.ui.close().await.expect("close");
    }

    /// `close` flushes the staging tail into scrollback BEFORE quitting: the mailbox FIFO
    /// lands the flushed `Scrollback` batches ahead of `Quit`, so the loop inserts them
    /// above the frame on its way out (the transcript is complete in real scrollback).
    #[tokio::test(flavor = "multi_thread")]
    async fn close_flushes_tail_ordering() {
        let h = start_facade();
        h.wait_frame();
        let tail = ["tail-one", "tail-two", "tail-three", "tail-four"];
        h.ui.print_lines(tail.iter().map(|s| (*s).to_owned()).collect());
        assert!(
            h.wait_until(Duration::from_secs(2), |h| h
                .contents()
                .contains("tail-four")),
            "staged tail never rendered:\n{}",
            h.contents()
        );
        h.ui.close().await.expect("close");
        // Characterization of the flush-then-quit shape: the flushed batches are
        // INSERTED above the viewport on the way out while the last-painted frame
        // (whose staging tail still showed the same rows) is left behind un-redrawn —
        // so each line lands on the final grid twice. Without the flush, Quit would
        // strand the rows in the frame only (one copy) and real scrollback would
        // never receive them.
        let c = h.contents();
        for line in tail {
            let n = c.matches(line).count();
            assert!(
                n >= 2,
                "{line}: want the inserted scrollback copy above the stale frame \
             (>= 2 occurrences), got {n}:\n{c}"
            );
        }
    }

    /// The `ui.close()`-at-idle deadlock regression (wart W10): Quit posted while the
    /// loop is FULLY idle terminates it within finite poll deadlines — close returns
    /// promptly, `done` is cancelled, and a second close is a quiet `Ok(())`.
    #[tokio::test(flavor = "multi_thread")]
    async fn close_at_idle_terminates_within_deadline() {
        let h = start_facade();
        h.wait_frame();
        tokio::time::sleep(Duration::from_millis(150)).await; // deep idle: deadlines elapse
        let t0 = Instant::now();
        h.ui.close().await.expect("close at idle");
        let elapsed = t0.elapsed();
        // The 1s bound is 20× the 50ms W10 deadline — generous for CI; an infinite
        // idle poll would hang here forever.
        assert!(
            elapsed < Duration::from_secs(1),
            "close at idle took {elapsed:?} (W10 IDLE_WAKE violated)"
        );
        assert!(
            h.ui.done().is_cancelled(),
            "done cancels when the loop exits"
        );
        h.ui.close().await.expect("second close is Ok(())");
    }

    /// The cross-thread reads: width/height atomics start 80×24 and `done` is live until
    /// the loop exits.
    #[tokio::test(flavor = "multi_thread")]
    async fn size_atomics_and_done_before_close() {
        let h = start_facade();
        h.wait_frame();
        assert_eq!((h.ui.width(), h.ui.height()), (80, 24));
        assert!(!h.ui.done().is_cancelled());
        h.ui.close().await.expect("close");
    }
}
