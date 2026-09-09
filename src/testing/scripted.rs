//! `ScriptedUi` — the layer-3 facade double (`TUI_CONTRACTS` §2.1; features `testing` +
//! `ui`). Blocking calls return READY futures popping a script of [`Reply`]s in order;
//! every facade call — fire-and-forget included — is recorded as a typed [`UiEvent`] in
//! one log so tests assert exact ordering. The [`ScriptedUi::fail_after`] knob simulates
//! the real loop's shutdown failing every further blocking call.
//!
//! The script, not the caller's cancel token, decides how a blocking call resolves:
//! script [`Reply::Interrupted`] to model an idle Ctrl+C or a fired cancel scope,
//! [`Reply::Closed`] to model shutdown.

use crate::sync::lock;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
};

use tokio_util::sync::CancellationToken;

use crate::ui::facade::{
    BusyGuard, Input, PanelKind, PreviewHandle, ProgressState, ScopeGuard, StatusData, Suggestion,
    TabbedResult, TabbedSpec, Ui, UiError, UiStreamSink, sanitize_window_title,
};
use crate::{BoxFuture, host};

/// Typed record of every facade call, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    /// [`Ui::print_lines`] with the committed lines.
    Print(Vec<String>),
    /// [`Ui::user_block`] with the display text.
    UserBlock(String),
    /// [`Ui::start_stream`] (the turn scope push is implied).
    StreamStart,
    /// [`UiStreamSink::block_preview`] with the label.
    Preview(String),
    /// [`PreviewHandle::write_raw_line`] with the raw source line.
    PreviewLine(String),
    /// [`PreviewHandle::close`] (recorded once; `Drop` defers to it).
    PreviewClose,
    /// [`UiStreamSink::done`] (the turn scope pop is implied).
    Done,
    /// [`Ui::busy`] with the label.
    Busy(String),
    /// The [`BusyGuard`] stopped (explicitly or on `Drop`).
    BusyOff,
    /// [`Ui::busy_detail`] with the detail.
    BusyDetail(String),
    /// [`Ui::set_status`] with the data.
    Status(StatusData),
    /// [`Ui::set_title`] with the SANITIZED title (the facade sanitizes — a
    /// security property the double mirrors).
    Title(String),
    /// [`Ui::set_slash_commands`] with the table.
    Commands(Vec<Suggestion>),
    /// [`Ui::push_cancel_scope`].
    ScopePush,
    /// The [`ScopeGuard`] popped (explicitly or on `Drop`).
    ScopePop,
    /// [`Ui::call_preview`] with the label.
    CallPreview(String),
    /// [`Ui::call_detail`] with the detail.
    CallDetail(String),
    /// [`Ui::call_line`] with the row.
    CallLine(String),
    /// [`Ui::close_preview`].
    ClosePreview,
    /// [`Ui::pause_clock`].
    PauseClock,
    /// [`Ui::resume_clock`].
    ResumeClock,
    /// [`Ui::read_input`] (blocking; pops the script).
    ReadInput,
    /// [`Ui::tabbed`] — or its `select`/`view`/`confirm` sugar — with the
    /// comparable shape of the spec (blocking; pops the script).
    Tabbed(TabbedSummary),
    /// [`Ui::take_queued_messages`] (blocking; pops the script).
    TakeQueued,
    /// [`Ui::close`].
    Close,
    /// [`Ui::call_body`] with the rows.
    CallBody(Vec<String>),
    /// [`Ui::set_progress`].
    Progress(ProgressState),
    /// [`Ui::notify`] with the SANITIZED text.
    Notify(String),
    /// [`Ui::set_dark_background`].
    DarkBackground(bool),
}

/// Shape assertion for one panel of a tabbed/select/view/confirm call — everything L3
/// compares, none of the closures.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelSummary {
    /// Chip title.
    pub title: String,
    /// Panel kind.
    pub kind: PanelKind,
    /// Row panels' items (approval asserts exact items).
    pub items: Vec<String>,
    /// Row panels' dim detail column.
    pub details: Vec<String>,
    /// View panels: `lines.len()`.
    pub line_count: usize,
    /// Initial cursor row.
    pub cursor: usize,
    /// Initially checked rows.
    pub checked: Vec<usize>,
    /// Whether the `"Other…"` Custom row is appended.
    pub custom: bool,
    /// Whether row-filter search is available.
    pub search: bool,
    /// Whether a View panel wraps long lines.
    pub wrap: bool,
    /// Whether the panel carries a live-refresh closure.
    pub has_refresh: bool,
    /// Prompt line above an Input panel.
    pub prompt: String,
    /// Input panel placeholder.
    pub placeholder: String,
    /// Input panel initial text.
    pub text: String,
    /// Visible rows; `0` = the per-kind default.
    pub height: usize,
    /// Input width; `0` = the default 40.
    pub input_width: usize,
    /// Slider value the panel OPENED on; `None` = the provider default.
    pub value: Option<f64>,
    /// Slider range and step (`min`, `max`, `step`) — a provider's own ceiling (the
    /// Anthropic 1.0 temperature cap) is only visible here.
    pub range: (f64, f64, f64),
    /// Switch state the panel opened on.
    pub on: bool,
    /// Whether the panel carries a preview closure (Picker).
    pub has_preview: bool,
}

/// Shape assertion for tabbed/select/view/confirm calls. Built by [`TabbedSummary::of`] at
/// call-record time. Fully specified BEFORE fan-out: WP45 implements the recorder,
/// WP49/WP50 assert against it in parallel lanes.
#[derive(Debug, Clone, PartialEq)]
pub struct TabbedSummary {
    /// Per-panel shapes, in tab order.
    pub panels: Vec<PanelSummary>,
    /// Refresh period for panels with a `refresh` closure; `0` = never.
    pub refresh_every_ms: u64,
    /// Enter on a non-last panel advances instead of committing (the ask wizard).
    pub enter_advances: bool,
}

impl TabbedSummary {
    /// The comparable shape of `spec` (closures reduced to `has_refresh`).
    pub fn of(spec: &TabbedSpec) -> Self {
        Self {
            panels: spec
                .panels
                .iter()
                .map(|p| PanelSummary {
                    title: p.title.clone(),
                    kind: p.kind(),
                    items: p.items().to_vec(),
                    details: p.details().to_vec(),
                    line_count: p.lines().len(),
                    cursor: p.cursor(),
                    checked: p.checked().to_vec(),
                    custom: p.custom(),
                    search: p.search,
                    wrap: p.wrap(),
                    has_refresh: p.refresh.is_some(),
                    prompt: p.prompt.clone(),
                    placeholder: p
                        .as_input()
                        .map_or_else(String::new, |i| i.placeholder.clone()),
                    text: p.as_input().map_or_else(String::new, |i| i.text.clone()),
                    height: p.height,
                    input_width: p.as_input().map_or(0, |i| i.width),
                    value: p.as_slider().and_then(|s| s.value),
                    range: p
                        .as_slider()
                        .map_or((0.0, 0.0, 0.0), |s| (s.min, s.max, s.step)),
                    on: p.on(),
                    has_preview: p.has_preview(),
                })
                .collect(),
            refresh_every_ms: spec.refresh_every_ms,
            enter_advances: spec.enter_advances,
        }
    }
}

/// Next scripted reply for a blocking call; a shape mismatch fails the test.
pub enum Reply {
    /// `read_input` resolves `Ok(input)`.
    Input(Input),
    /// The blocking call resolves `Err(UiError::Interrupted)` (idle Ctrl+C, or a
    /// fired cancel scope).
    Interrupted,
    /// The blocking call resolves `Err(UiError::Closed)` (shutdown).
    Closed,
    /// `tabbed` (or its sugar) resolves `Ok(result)`.
    Tabbed(TabbedResult),
    /// `take_queued_messages` resolves with these inputs.
    Queued(Vec<Input>),
}

/// The scripted facade double: blocking calls return ready futures popping the script;
/// fire-and-forget calls log a [`UiEvent`].
pub struct ScriptedUi {
    script: Mutex<VecDeque<Reply>>,
    /// Shared with the guards/sinks/previews a call hands out, so their later
    /// records land in the same ordered log.
    log: Arc<Mutex<Vec<UiEvent>>>,
    width: AtomicU16,
    height: AtomicU16,
    done: CancellationToken,
    fail_after: AtomicUsize,
    blocking_calls: AtomicUsize,
}

impl ScriptedUi {
    /// A double whose blocking calls consume `script` in order. Size starts 80×24.
    pub fn new(script: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into()),
            log: Arc::new(Mutex::new(Vec::new())),
            width: AtomicU16::new(80),
            height: AtomicU16::new(24),
            done: CancellationToken::new(),
            fail_after: AtomicUsize::new(usize::MAX),
            blocking_calls: AtomicUsize::new(0),
        })
    }

    /// Every recorded facade call, in order.
    pub fn events(&self) -> Vec<UiEvent> {
        lock(&self.log).clone()
    }

    /// Sets the size the `width()`/`height()` atomics report.
    pub fn set_size(&self, w: u16, h: u16) {
        self.width.store(w, Ordering::Relaxed);
        self.height.store(h, Ordering::Relaxed);
    }

    /// After `n` blocking calls every further one resolves `Err(Closed)` (shutdown
    /// simulation — the real loop's exit fails all outstanding waiters).
    pub fn fail_after(&self, n: usize) {
        self.fail_after.store(n, Ordering::Relaxed);
    }

    fn record(&self, ev: UiEvent) {
        lock(&self.log).push(ev);
    }

    /// Counts one blocking call; `true` once the call is past the [`Self::fail_after`]
    /// threshold or the double was closed — it must resolve the shutdown shape.
    fn shut(&self) -> bool {
        let n = self.blocking_calls.fetch_add(1, Ordering::Relaxed) + 1;
        n > self.fail_after.load(Ordering::Relaxed) || self.done.is_cancelled()
    }

    /// Pops the next scripted reply; a missing one fails the test.
    #[allow(clippy::panic)] // test double: a script underrun must fail the test loudly
    fn pop(&self, call: &str) -> Reply {
        lock(&self.script)
            .pop_front()
            .unwrap_or_else(|| panic!("ScriptedUi: no scripted reply left for {call}"))
    }
}

/// The recording stream sink a [`Ui::start_stream`] call hands out.
struct ScriptedStream {
    log: Arc<Mutex<Vec<UiEvent>>>,
}

impl UiStreamSink for ScriptedStream {
    fn block_preview(&self, label: &str) -> Box<dyn PreviewHandle> {
        lock(&self.log).push(UiEvent::Preview(label.to_owned()));
        Box::new(ScriptedPreview {
            log: Arc::clone(&self.log),
            closed: false,
        })
    }

    fn done(&self) {
        lock(&self.log).push(UiEvent::Done);
    }
}

/// The recording preview handle; `Drop` = close, recorded exactly once.
struct ScriptedPreview {
    log: Arc<Mutex<Vec<UiEvent>>>,
    closed: bool,
}

impl PreviewHandle for ScriptedPreview {
    fn write_raw_line(&mut self, line: &str) {
        lock(&self.log).push(UiEvent::PreviewLine(line.to_owned()));
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        lock(&self.log).push(UiEvent::PreviewClose);
    }
}

impl Drop for ScriptedPreview {
    fn drop(&mut self) {
        self.close();
    }
}

#[allow(clippy::panic)] // test double: a scripted-shape mismatch must fail the test loudly
impl Ui for ScriptedUi {
    fn read_input<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Input, UiError>> {
        self.record(UiEvent::ReadInput);
        let r = if self.shut() {
            Err(UiError::Closed)
        } else {
            match self.pop("read_input") {
                Reply::Input(i) => Ok(i),
                Reply::Interrupted => Err(UiError::Interrupted),
                Reply::Closed => Err(UiError::Closed),
                Reply::Tabbed(_) | Reply::Queued(_) => {
                    panic!("ScriptedUi: read_input got a non-input reply")
                }
            }
        };
        Box::pin(std::future::ready(r))
    }

    fn tabbed<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        spec: TabbedSpec,
    ) -> BoxFuture<'a, Result<TabbedResult, UiError>> {
        self.record(UiEvent::Tabbed(TabbedSummary::of(&spec)));
        let r = if self.shut() {
            Err(UiError::Closed)
        } else {
            match self.pop("tabbed") {
                Reply::Tabbed(t) => Ok(t),
                Reply::Interrupted => Err(UiError::Interrupted),
                Reply::Closed => Err(UiError::Closed),
                Reply::Input(_) | Reply::Queued(_) => {
                    panic!("ScriptedUi: tabbed got a non-tabbed reply")
                }
            }
        };
        Box::pin(std::future::ready(r))
    }

    fn take_queued_messages(&self) -> BoxFuture<'_, Vec<Input>> {
        self.record(UiEvent::TakeQueued);
        let r = if self.shut() {
            Vec::new()
        } else {
            match self.pop("take_queued_messages") {
                Reply::Queued(v) => v,
                Reply::Closed => Vec::new(), // Go returns nil once the Program died
                Reply::Input(_) | Reply::Interrupted | Reply::Tabbed(_) => {
                    panic!("ScriptedUi: take_queued_messages got a non-queue reply")
                }
            }
        };
        Box::pin(std::future::ready(r))
    }

    fn close(&self) -> BoxFuture<'_, std::io::Result<()>> {
        self.record(UiEvent::Close);
        self.done.cancel();
        Box::pin(std::future::ready(Ok(())))
    }

    fn print_lines(&self, lines: Vec<String>) {
        self.record(UiEvent::Print(lines));
    }

    fn user_block(&self, display: &str) {
        self.record(UiEvent::UserBlock(display.to_owned()));
    }

    fn start_stream(&self, _cancel: CancellationToken) -> Box<dyn UiStreamSink> {
        self.record(UiEvent::StreamStart);
        Box::new(ScriptedStream {
            log: Arc::clone(&self.log),
        })
    }

    fn busy(&self, label: &str) -> BusyGuard {
        self.record(UiEvent::Busy(label.to_owned()));
        let log = Arc::clone(&self.log);
        BusyGuard::new(move || lock(&log).push(UiEvent::BusyOff))
    }

    fn busy_detail(&self, detail: &str) {
        self.record(UiEvent::BusyDetail(detail.to_owned()));
    }

    fn push_cancel_scope(&self, _cancel: CancellationToken) -> ScopeGuard {
        self.record(UiEvent::ScopePush);
        let log = Arc::clone(&self.log);
        ScopeGuard::new(move || lock(&log).push(UiEvent::ScopePop))
    }

    fn set_status(&self, s: StatusData) {
        self.record(UiEvent::Status(s));
    }

    fn set_title(&self, title: &str) {
        // Sanitized like the real facade (a security property tests may pin).
        self.record(UiEvent::Title(sanitize_window_title(title)));
    }

    fn set_slash_commands(&self, cmds: Vec<Suggestion>) {
        self.record(UiEvent::Commands(cmds));
    }

    fn call_preview(&self, label: &str) {
        self.record(UiEvent::CallPreview(label.to_owned()));
    }

    fn call_detail(&self, detail: &str) {
        self.record(UiEvent::CallDetail(detail.to_owned()));
    }

    fn call_line(&self, line: &str) {
        self.record(UiEvent::CallLine(line.to_owned()));
    }

    fn close_preview(&self) {
        self.record(UiEvent::ClosePreview);
    }

    fn pause_clock(&self) {
        self.record(UiEvent::PauseClock);
    }

    fn resume_clock(&self) {
        self.record(UiEvent::ResumeClock);
    }

    fn call_body(&self, rows: Vec<String>) {
        self.record(UiEvent::CallBody(rows));
    }

    fn set_progress(&self, s: ProgressState) {
        self.record(UiEvent::Progress(s));
    }

    fn notify(&self, text: &str) {
        // Sanitized like the real facade (the same sanitizer as the title).
        self.record(UiEvent::Notify(sanitize_window_title(text)));
    }

    fn set_dark_background(&self, dark: bool) {
        self.record(UiEvent::DarkBackground(dark));
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

/// A recording [`host::Host`] implementing whichever capabilities `caps` enables (the
/// `host_test.go` fakes): every state, event and close lands in shared logs the test reads back;
/// `dark` is what the background capability answers.
pub struct RecordingHost {
    /// The host's name.
    pub name: &'static str,
    /// Every `set_state` call, in order.
    pub states: Arc<Mutex<Vec<host::State>>>,
    /// Every `notify` call, in order.
    pub events: Arc<Mutex<Vec<host::Event>>>,
    /// The names of the hosts closed so far, in close order (shared across hosts so the
    /// reverse-order law is observable).
    pub closed: Arc<Mutex<Vec<&'static str>>>,
    /// What `dark_background` answers.
    pub dark: Option<bool>,
    /// Which capabilities the host advertises.
    pub caps: host::Caps,
}

impl RecordingHost {
    /// A host with every capability, its own logs, and no background answer.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            states: Arc::default(),
            events: Arc::default(),
            closed: Arc::default(),
            dark: None,
            caps: host::Caps {
                state: true,
                notify: true,
                background: true,
                close: true,
            },
        }
    }

    /// The recorded states.
    pub fn states(&self) -> Vec<host::State> {
        lock(&self.states).clone()
    }

    /// The recorded events.
    pub fn events(&self) -> Vec<host::Event> {
        lock(&self.events).clone()
    }

    /// The recorded close order.
    pub fn closed(&self) -> Vec<&'static str> {
        lock(&self.closed).clone()
    }
}

impl host::Host for RecordingHost {
    fn name(&self) -> &'static str {
        self.name
    }

    fn as_state_reporter(&self) -> Option<&dyn host::StateReporter> {
        self.caps.state.then_some(self as &dyn host::StateReporter)
    }

    fn as_notifier(&self) -> Option<&dyn host::Notifier> {
        self.caps.notify.then_some(self as &dyn host::Notifier)
    }

    fn as_background(&self) -> Option<&dyn host::BackgroundReporter> {
        self.caps
            .background
            .then_some(self as &dyn host::BackgroundReporter)
    }

    fn as_closer(&self) -> Option<&dyn host::Closer> {
        self.caps.close.then_some(self as &dyn host::Closer)
    }
}

impl host::StateReporter for RecordingHost {
    fn set_state(&self, s: host::State) {
        lock(&self.states).push(s);
    }
}

impl host::Notifier for RecordingHost {
    fn notify(&self, e: &host::Event) {
        lock(&self.events).push(e.clone());
    }
}

impl host::BackgroundReporter for RecordingHost {
    fn dark_background(&self) -> Option<bool> {
        self.dark
    }
}

impl host::Closer for RecordingHost {
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            lock(&self.closed).push(self.name);
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::{Reply, ScriptedUi, TabbedSummary, UiEvent};
    use crate::ui::facade::{
        Input, Panel, PanelKind, PanelResult, SelectSpec, TabbedResult, TabbedSpec, Ui, UiError,
        ViewSpec,
    };

    #[tokio::test]
    async fn blocking_calls_pop_the_script_in_order() {
        let cancel = CancellationToken::new();
        let ui = ScriptedUi::new(vec![
            Reply::Input(Input {
                display: "hi".to_owned(),
                text: "hi".to_owned(),
            }),
            Reply::Interrupted,
            Reply::Queued(vec![Input {
                display: "q".to_owned(),
                text: "q".to_owned(),
            }]),
        ]);
        assert_eq!(
            ui.read_input(&cancel).await.expect("scripted input").text,
            "hi"
        );
        assert_eq!(
            ui.read_input(&cancel)
                .await
                .expect_err("scripted interrupt"),
            UiError::Interrupted
        );
        let taken = ui.take_queued_messages().await;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].text, "q");
        assert_eq!(
            ui.events(),
            vec![UiEvent::ReadInput, UiEvent::ReadInput, UiEvent::TakeQueued]
        );
    }

    #[tokio::test]
    async fn recorder_orders_fire_and_forget_calls() {
        let cancel = CancellationToken::new();
        let ui = ScriptedUi::new(Vec::new());
        ui.print_lines(vec!["a".to_owned()]);
        ui.user_block("you");
        let sink = ui.start_stream(cancel.clone());
        let mut pv = sink.block_preview("rendering table…");
        pv.write_raw_line("|a|");
        pv.close();
        drop(pv); // Drop after close records nothing more
        sink.done();
        let busy = ui.busy("Waiting for the model");
        ui.busy_detail("1.2 KB");
        busy.stop();
        let scope = ui.push_cancel_scope(cancel.clone());
        scope.pop();
        ui.set_title("a\x1b]0;evil\x07b\nc"); // recorded sanitized
        ui.call_preview("[bash]");
        ui.call_detail("1.2k tokens");
        ui.call_line("✓ ran");
        ui.close_preview();
        ui.pause_clock();
        ui.resume_clock();
        assert_eq!(
            ui.events(),
            vec![
                UiEvent::Print(vec!["a".to_owned()]),
                UiEvent::UserBlock("you".to_owned()),
                UiEvent::StreamStart,
                UiEvent::Preview("rendering table…".to_owned()),
                UiEvent::PreviewLine("|a|".to_owned()),
                UiEvent::PreviewClose,
                UiEvent::Done,
                UiEvent::Busy("Waiting for the model".to_owned()),
                UiEvent::BusyDetail("1.2 KB".to_owned()),
                UiEvent::BusyOff,
                UiEvent::ScopePush,
                UiEvent::ScopePop,
                UiEvent::Title("a]0;evilbc".to_owned()),
                UiEvent::CallPreview("[bash]".to_owned()),
                UiEvent::CallDetail("1.2k tokens".to_owned()),
                UiEvent::CallLine("✓ ran".to_owned()),
                UiEvent::ClosePreview,
                UiEvent::PauseClock,
                UiEvent::ResumeClock,
            ]
        );
    }

    #[tokio::test]
    async fn guards_record_on_drop_and_leaked_preview_closes_once() {
        let ui = ScriptedUi::new(Vec::new());
        {
            let _busy = ui.busy("b");
        } // dropped without stop()
        {
            let _scope = ui.push_cancel_scope(CancellationToken::new());
        } // dropped without pop()
        let sink = ui.start_stream(CancellationToken::new());
        {
            let _pv = sink.block_preview("p");
        } // leaked: Drop closes exactly once
        assert_eq!(
            ui.events(),
            vec![
                UiEvent::Busy("b".to_owned()),
                UiEvent::BusyOff,
                UiEvent::ScopePush,
                UiEvent::ScopePop,
                UiEvent::StreamStart,
                UiEvent::Preview("p".to_owned()),
                UiEvent::PreviewClose,
            ]
        );
    }

    // The sugar defaults route through `tabbed`, so the recorder sees their exact
    // panel shapes (select forces search on — ui.go:370-386; view keeps height;
    // confirm is a two-item select).
    #[tokio::test]
    async fn sugar_records_tabbed_shapes() {
        let cancel = CancellationToken::new();
        let ui = ScriptedUi::new(vec![
            Reply::Tabbed(TabbedResult {
                cancelled: false,
                focused: 0,
                panels: vec![PanelResult {
                    cursor: 1,
                    ..PanelResult::default()
                }],
            }),
            Reply::Tabbed(TabbedResult::default()),
            Reply::Tabbed(TabbedResult {
                cancelled: false,
                focused: 0,
                panels: vec![PanelResult::default()],
            }),
        ]);
        let sel = ui
            .select(
                &cancel,
                SelectSpec {
                    title: "Select a model".to_owned(),
                    items: vec!["a".to_owned(), "b".to_owned()],
                    cursor: 0,
                },
            )
            .await
            .expect("select");
        assert_eq!((sel.index, sel.cancelled), (1, false));
        ui.view(
            &cancel,
            ViewSpec {
                title: "Status".to_owned(),
                lines: vec!["r1".to_owned(), "r2".to_owned()],
                height: 0,
            },
        )
        .await
        .expect("view");
        assert!(
            ui.confirm(&cancel, "Save?", "Yes", "No")
                .await
                .expect("confirm")
        );

        let events = ui.events();
        assert_eq!(events.len(), 3);
        let UiEvent::Tabbed(select_shape) = &events[0] else {
            panic!("select did not record a Tabbed event: {events:?}");
        };
        assert_eq!(select_shape.panels.len(), 1);
        assert_eq!(select_shape.panels[0].kind, PanelKind::List);
        assert!(select_shape.panels[0].search, "select forces search on");
        assert_eq!(select_shape.panels[0].items, ["a", "b"]);
        let UiEvent::Tabbed(view_shape) = &events[1] else {
            panic!("view did not record a Tabbed event: {events:?}");
        };
        assert_eq!(view_shape.panels[0].kind, PanelKind::View);
        assert_eq!(view_shape.panels[0].line_count, 2);
        let UiEvent::Tabbed(confirm_shape) = &events[2] else {
            panic!("confirm did not record a Tabbed event: {events:?}");
        };
        assert_eq!(confirm_shape.panels[0].items, ["Yes", "No"]);
    }

    /// The shutdown knob: after `n` blocking calls every further one resolves the
    /// closed shape without touching the script (waiter-fails-on-shutdown, L3 side).
    #[tokio::test]
    async fn fail_after_fails_further_blocking_calls() {
        let cancel = CancellationToken::new();
        let ui = ScriptedUi::new(vec![Reply::Input(Input::default())]);
        ui.fail_after(1);
        assert!(ui.read_input(&cancel).await.is_ok());
        assert_eq!(
            ui.read_input(&cancel)
                .await
                .expect_err("past the threshold"),
            UiError::Closed
        );
        assert!(ui.take_queued_messages().await.is_empty());
        let r = ui
            .tabbed(&cancel, TabbedSpec::default())
            .await
            .expect_err("tabbed past the threshold");
        assert_eq!(r, UiError::Closed);
    }

    /// `close` cancels `done` and every later blocking call resolves `Err(Closed)`.
    #[tokio::test]
    async fn close_cancels_done_and_fails_blocking_calls() {
        let cancel = CancellationToken::new();
        let ui = ScriptedUi::new(vec![Reply::Input(Input::default())]);
        assert!(!ui.done().is_cancelled());
        ui.close().await.expect("close");
        assert!(ui.done().is_cancelled());
        assert_eq!(
            ui.read_input(&cancel).await.expect_err("closed"),
            UiError::Closed
        );
        assert_eq!(
            ui.events(),
            vec![UiEvent::Close, UiEvent::ReadInput],
            "the script is left untouched after close"
        );
    }

    #[test]
    fn size_atomics_report_set_size() {
        let ui = ScriptedUi::new(Vec::new());
        assert_eq!((ui.width(), ui.height()), (80, 24));
        ui.set_size(100, 28);
        assert_eq!((ui.width(), ui.height()), (100, 28));
    }

    #[test]
    fn tabbed_summary_reduces_closures_to_has_refresh() {
        let spec = TabbedSpec {
            panels: vec![
                Panel::view("Tools".to_owned(), vec!["x".to_owned()])
                    .with_refresh(Box::new(Vec::new)),
            ],
            refresh_every_ms: 500,
            enter_advances: false,
        };
        let s = TabbedSummary::of(&spec);
        assert!(s.panels[0].has_refresh);
        assert_eq!(s.refresh_every_ms, 500);
        assert_eq!(s.panels[0].line_count, 1);
    }
}
