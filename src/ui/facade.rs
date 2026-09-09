//! The interactive UI facade (`TUI_CONTRACTS` §2). Only input-acquiring
//! calls await; everything else is a fire-and-forget mailbox send. Implemented by
//! `crate::ui`; consumed by `crate::repl`; doubled by `crate::testing::ScriptedUi`.

use crate::BoxFuture;
use tokio_util::sync::CancellationToken;

/// Facade errors. Display: `"ui: closed"` / `"ui: interrupted"` (Go ui.go:27,31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UiError {
    /// The UI loop has shut down; the call (and every later one) cannot be served.
    #[error("ui: closed")]
    Closed,
    /// An idle Ctrl+C / Ctrl+D interrupted the parked call.
    #[error("ui: interrupted")]
    Interrupted,
}

/// One submitted input. `display` = paste tags bounded (for the user block);
/// `text` = tags fully expanded (for sending).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// The bounded form echoed into the transcript's user block.
    pub display: String,
    /// The fully expanded form handed to the model.
    pub text: String,
}

/// Status-row data (model.go `StatusData`). Token/ctx fields render only under WP53.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusData {
    /// Current model label (`"—"` placeholder when empty).
    pub model: String,
    /// Estimated context tokens used.
    pub ctx_used: u64,
    /// Context window size.
    pub ctx_window: u64,
    /// Whether `ctx_used` is an estimate (renders the `≈` prefix).
    pub estimated: bool,
    /// Input tokens of the last call.
    pub in_tokens: u64,
    /// Output tokens of the last call.
    pub out_tokens: u64,
    /// Cache-hit share of the input tokens, in percent.
    pub cache_hit_pct: f64,
    /// Whether the `debug` marker is appended (survives truncation).
    pub debug: bool,
}

/// One slash-completion candidate (ui.go `Suggestion`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestion {
    /// What completing the row puts in the composer.
    pub value: String,
    /// What the row shows (the already-typed prefix is not repeated).
    pub label: String,
    /// The dim right column; `""` for entries that explain themselves.
    pub desc: String,
}

/// Panel kinds (tabbed.go:21-32, Go iota order).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelKind {
    /// Single-select row list.
    #[default]
    List,
    /// Multi-select row list (Space toggles).
    Multi,
    /// Numeric slider (WP54).
    Slider,
    /// On/off switch (WP54).
    Switch,
    /// One-line text input.
    Input,
    /// Directory browser (WP54).
    Browser,
    /// Row list with a per-row preview pane (the `/edit` image picker — T3, WP64).
    Picker,
    /// Read-only line viewer.
    View,
}

/// Live-refresh closure; runs on the UI loop thread at tick time.
pub(crate) type RefreshFn = Box<dyn FnMut() -> Vec<String> + Send>;

/// Preview closure of a `Picker` panel (tabbed.go:35-41 `Panel.Preview`): `(index, max_cols,
/// max_rows) -> rows`; runs on the UI loop thread when the selection or the pane geometry changes.
/// It owns its cache (the decoded images), hence `FnMut` + `Send`, not `Sync`.
pub type PreviewFn = Box<dyn FnMut(usize, usize, usize) -> Vec<String> + Send>;

/// The terminal's native progress indicator (ui.go:66-79; OSC 9;4 on the ANSI host).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProgressState {
    /// No indicator.
    #[default]
    None,
    /// Indeterminate (a turn is running).
    Busy,
    /// Paused, waiting for the user.
    Input,
    /// Error.
    Error,
}

/// One surface panel (tabbed.go:35-103): what every kind shares, and the kind's own data in
/// [`PanelBody`]. `items`/`lines`/`details` may carry raw SGR; the engine matches/truncates/highlights
/// through escapes.
#[derive(Default)]
pub struct Panel {
    /// Chip title.
    pub title: String,
    /// Prompt line above the panel body.
    pub prompt: String,
    /// Whether the row-filter search is available (`'/'` gated on overflow).
    pub search: bool,
    /// Visible rows; `0` = the per-kind default (tabbed.go:431-446).
    pub height: usize,
    /// Live refresh of the rows; runs on the UI loop thread at tick time.
    pub refresh: Option<RefreshFn>,
    /// The kind and its data.
    pub body: PanelBody,
}

/// A panel's kind together with the data only that kind has.
pub enum PanelBody {
    /// Single-select row list — the default.
    List(ListBody),
    /// Multi-select row list (Space toggles).
    Multi(ListBody),
    /// Numeric slider.
    Slider(SliderBody),
    /// On/off switch.
    Switch {
        /// The state the panel opens on.
        on: bool,
    },
    /// One-line text input.
    Input(InputBody),
    /// Directory browser.
    Browser {
        /// Start directory (empty = the current directory).
        dir: std::path::PathBuf,
    },
    /// Row list with a per-row preview pane (the `/edit` image picker).
    Picker(PickerBody),
    /// Read-only line viewer.
    View(ViewBody),
}

impl Default for PanelBody {
    fn default() -> Self {
        Self::List(ListBody::default())
    }
}

impl PanelBody {
    /// The empty body of `kind` — what a fixture fills in afterwards.
    pub fn empty(kind: PanelKind) -> Self {
        match kind {
            PanelKind::List => Self::List(ListBody::default()),
            PanelKind::Multi => Self::Multi(ListBody::default()),
            PanelKind::Slider => Self::Slider(SliderBody::default()),
            PanelKind::Switch => Self::Switch { on: false },
            PanelKind::Input => Self::Input(InputBody::default()),
            PanelKind::Browser => Self::Browser {
                dir: std::path::PathBuf::new(),
            },
            PanelKind::Picker => Self::Picker(PickerBody::default()),
            PanelKind::View => Self::View(ViewBody::default()),
        }
    }
}

/// A row list's data (`List` and `Multi`).
#[derive(Default)]
pub struct ListBody {
    /// Rows (may carry raw SGR).
    pub items: Vec<String>,
    /// Initial cursor row (an index into `items`).
    pub cursor: usize,
    /// Initially checked rows of a `Multi` panel (indices into `items`).
    pub checked: Vec<usize>,
    /// Whether the panel appends the `"Other…"` inline Custom editor row.
    pub custom: bool,
    /// Dim per-row detail column.
    pub details: Vec<String>,
}

/// A picker's data: a row list plus its preview renderer.
#[derive(Default)]
pub struct PickerBody {
    /// Rows (may carry raw SGR).
    pub items: Vec<String>,
    /// Initial cursor row.
    pub cursor: usize,
    /// Dim per-row detail column.
    pub details: Vec<String>,
    /// Preview renderer: `(index, max_cols, max_rows) -> rows`; `None` = plain list.
    pub preview: Option<PreviewFn>,
}

/// A slider's range and value.
#[derive(Default)]
pub struct SliderBody {
    /// Minimum.
    pub min: f64,
    /// Maximum.
    pub max: f64,
    /// Step.
    pub step: f64,
    /// Value; `None` = default.
    pub value: Option<f64>,
}

/// A text input's data.
#[derive(Default)]
pub struct InputBody {
    /// Initial text.
    pub text: String,
    /// Placeholder.
    pub placeholder: String,
    /// Width; `0` = the default 40, clamped to `[4, w−6]`.
    pub width: usize,
}

/// A viewer's data.
#[derive(Default)]
pub struct ViewBody {
    /// Rows (may carry raw SGR).
    pub lines: Vec<String>,
    /// Whether long lines wrap (unwrapped panels pan with h/l).
    pub wrap: bool,
}

impl Panel {
    /// A single-select row list.
    pub fn list(title: impl Into<String>, items: Vec<String>) -> Self {
        Self::of(
            title,
            PanelBody::List(ListBody {
                items,
                ..ListBody::default()
            }),
        )
    }

    /// A multi-select row list.
    pub fn multi(title: impl Into<String>, items: Vec<String>) -> Self {
        Self::of(
            title,
            PanelBody::Multi(ListBody {
                items,
                ..ListBody::default()
            }),
        )
    }

    /// A row list with a preview pane.
    pub fn picker(title: impl Into<String>, items: Vec<String>) -> Self {
        Self::of(
            title,
            PanelBody::Picker(PickerBody {
                items,
                ..PickerBody::default()
            }),
        )
    }

    /// A numeric slider.
    pub fn slider(
        title: impl Into<String>,
        min: f64,
        max: f64,
        step: f64,
        value: Option<f64>,
    ) -> Self {
        Self::of(
            title,
            PanelBody::Slider(SliderBody {
                min,
                max,
                step,
                value,
            }),
        )
    }

    /// An on/off switch.
    pub fn switch(title: impl Into<String>, on: bool) -> Self {
        Self::of(title, PanelBody::Switch { on })
    }

    /// A one-line text input.
    pub fn input(
        title: impl Into<String>,
        text: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self::of(
            title,
            PanelBody::Input(InputBody {
                text: text.into(),
                placeholder: placeholder.into(),
                width: 0,
            }),
        )
    }

    /// A read-only line viewer.
    pub fn view(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self::of(title, PanelBody::View(ViewBody { lines, wrap: false }))
    }

    /// A directory browser.
    pub fn browser(title: impl Into<String>, dir: impl Into<std::path::PathBuf>) -> Self {
        Self::of(title, PanelBody::Browser { dir: dir.into() })
    }

    /// A panel of `body` titled `title`.
    pub fn of(title: impl Into<String>, body: PanelBody) -> Self {
        Self {
            title: title.into(),
            body,
            ..Self::default()
        }
    }

    /// With the prompt line above the body.
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    /// With the row-filter search enabled or not.
    #[must_use]
    pub fn with_search(mut self, search: bool) -> Self {
        self.search = search;
        self
    }

    /// With a fixed visible row count.
    #[must_use]
    pub fn with_height(mut self, height: usize) -> Self {
        self.height = height;
        self
    }

    /// With a live refresh of the rows.
    #[must_use]
    pub fn with_refresh(mut self, refresh: RefreshFn) -> Self {
        self.refresh = Some(refresh);
        self
    }

    /// With the initial cursor row (row panels; a no-op elsewhere).
    #[must_use]
    pub fn with_cursor(mut self, cursor: usize) -> Self {
        match &mut self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => l.cursor = cursor,
            PanelBody::Picker(p) => p.cursor = cursor,
            _ => {}
        }
        self
    }

    /// With the initially checked rows (`Multi`; a no-op elsewhere).
    #[must_use]
    pub fn with_checked(mut self, checked: Vec<usize>) -> Self {
        if let PanelBody::List(l) | PanelBody::Multi(l) = &mut self.body {
            l.checked = checked;
        }
        self
    }

    /// With the `"Other…"` Custom row (`List`/`Multi`; a no-op elsewhere).
    #[must_use]
    pub fn with_custom(mut self, custom: bool) -> Self {
        if let PanelBody::List(l) | PanelBody::Multi(l) = &mut self.body {
            l.custom = custom;
        }
        self
    }

    /// With the dim per-row detail column (row panels; a no-op elsewhere).
    #[must_use]
    pub fn with_details(mut self, details: Vec<String>) -> Self {
        match &mut self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => l.details = details,
            PanelBody::Picker(p) => p.details = details,
            _ => {}
        }
        self
    }

    /// With the opening state (`Switch`; a no-op elsewhere).
    #[must_use]
    pub fn with_on(mut self, on: bool) -> Self {
        if let PanelBody::Switch { on: state } = &mut self.body {
            *state = on;
        }
        self
    }

    /// With long lines wrapping (`View`; a no-op elsewhere).
    #[must_use]
    pub fn with_wrap(mut self, wrap: bool) -> Self {
        if let PanelBody::View(v) = &mut self.body {
            v.wrap = wrap;
        }
        self
    }

    /// With the field width (`Input`; a no-op elsewhere).
    #[must_use]
    pub fn with_input_width(mut self, width: usize) -> Self {
        if let PanelBody::Input(i) = &mut self.body {
            i.width = width;
        }
        self
    }

    /// With the preview renderer (`Picker`; a no-op elsewhere).
    #[must_use]
    pub fn with_preview(mut self, preview: PreviewFn) -> Self {
        if let PanelBody::Picker(p) = &mut self.body {
            p.preview = Some(preview);
        }
        self
    }

    /// The kind, as the engine and the test doubles name it.
    pub fn kind(&self) -> PanelKind {
        match self.body {
            PanelBody::List(_) => PanelKind::List,
            PanelBody::Multi(_) => PanelKind::Multi,
            PanelBody::Slider(_) => PanelKind::Slider,
            PanelBody::Switch { .. } => PanelKind::Switch,
            PanelBody::Input(_) => PanelKind::Input,
            PanelBody::Browser { .. } => PanelKind::Browser,
            PanelBody::Picker(_) => PanelKind::Picker,
            PanelBody::View(_) => PanelKind::View,
        }
    }

    /// The rows of a row panel (`List`/`Multi`/`Picker`); empty elsewhere.
    pub fn items(&self) -> &[String] {
        match &self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => &l.items,
            PanelBody::Picker(p) => &p.items,
            _ => &[],
        }
    }

    /// The initial cursor row of a row panel; `0` elsewhere.
    pub fn cursor(&self) -> usize {
        match &self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => l.cursor,
            PanelBody::Picker(p) => p.cursor,
            _ => 0,
        }
    }

    /// The initially checked rows of a `Multi` panel; empty elsewhere.
    pub fn checked(&self) -> &[usize] {
        match &self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => &l.checked,
            _ => &[],
        }
    }

    /// Whether a `List`/`Multi` panel appends the Custom row.
    pub fn custom(&self) -> bool {
        matches!(&self.body, PanelBody::List(l) | PanelBody::Multi(l) if l.custom)
    }

    /// The dim per-row detail column of a row panel; empty elsewhere.
    pub fn details(&self) -> &[String] {
        match &self.body {
            PanelBody::List(l) | PanelBody::Multi(l) => &l.details,
            PanelBody::Picker(p) => &p.details,
            _ => &[],
        }
    }

    /// A `View` panel's rows; empty elsewhere.
    pub fn lines(&self) -> &[String] {
        match &self.body {
            PanelBody::View(v) => &v.lines,
            _ => &[],
        }
    }

    /// Whether a `View` panel wraps long lines.
    pub fn wrap(&self) -> bool {
        matches!(&self.body, PanelBody::View(v) if v.wrap)
    }

    /// A `Slider` panel's range and value.
    pub fn as_slider(&self) -> Option<&SliderBody> {
        match &self.body {
            PanelBody::Slider(s) => Some(s),
            _ => None,
        }
    }

    /// A `Switch` panel's opening state.
    pub fn on(&self) -> bool {
        matches!(self.body, PanelBody::Switch { on: true })
    }

    /// An `Input` panel's data.
    pub fn as_input(&self) -> Option<&InputBody> {
        match &self.body {
            PanelBody::Input(i) => Some(i),
            _ => None,
        }
    }

    /// A `Browser` panel's start directory; empty elsewhere.
    pub fn dir(&self) -> &std::path::Path {
        match &self.body {
            PanelBody::Browser { dir } => dir,
            _ => std::path::Path::new(""),
        }
    }

    /// Whether a `Picker` panel carries a preview renderer.
    pub fn has_preview(&self) -> bool {
        matches!(&self.body, PanelBody::Picker(p) if p.preview.is_some())
    }

    /// Takes a `Picker` panel's preview renderer out (the surface state owns it while open).
    pub fn take_preview(&mut self) -> Option<PreviewFn> {
        match &mut self.body {
            PanelBody::Picker(p) => p.preview.take(),
            _ => None,
        }
    }
}

/// Multi-tab spec (tabbed.go:107-116). `enter_advances` = wizard (ask); /model keeps false.
#[derive(Default)]
pub struct TabbedSpec {
    /// The panels, in tab order.
    pub panels: Vec<Panel>,
    /// Refresh period for panels with a `refresh` closure; `0` = never.
    pub refresh_every_ms: u64,
    /// Enter on a non-last panel advances instead of committing (the ask wizard).
    pub enter_advances: bool,
}

/// Per-panel commit (tabbed.go:118-134). `cursor`/`checked` ALWAYS index the ORIGINAL items
/// (search filters never renumber). With `Panel::custom`: `custom` = trimmed input, `text` = `""`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelResult {
    /// Committed cursor row (an index into the ORIGINAL items).
    pub cursor: usize,
    /// Committed checked rows (indices into the ORIGINAL items).
    pub checked: Vec<usize>,
    /// Committed slider value; `None` = default.
    pub value: Option<f64>,
    /// Committed switch state.
    pub on: bool,
    /// Committed browser path.
    pub path: String,
    /// Committed input text.
    pub text: String,
    /// Committed Custom-editor text (trimmed); `""` when a listed row was chosen.
    pub custom: String,
}

/// Commit of a whole tabbed surface.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabbedResult {
    /// Whether the surface was cancelled (ESC/q/Ctrl+C).
    pub cancelled: bool,
    /// The focused panel at commit time.
    pub focused: usize,
    /// Per-panel commits, in tab order.
    pub panels: Vec<PanelResult>,
}

/// Spec of the single-select sugar ([`Ui::select`]).
#[derive(Debug, Clone, Default)]
pub struct SelectSpec {
    /// Surface title.
    pub title: String,
    /// The rows.
    pub items: Vec<String>,
    /// Initial cursor row.
    pub cursor: usize,
}

/// Result of the single-select sugar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectResult {
    /// The chosen row (an index into the ORIGINAL items).
    pub index: usize,
    /// Whether the select was cancelled.
    pub cancelled: bool,
}

/// Spec of the read-only viewer sugar ([`Ui::view`]).
#[derive(Debug, Clone, Default)]
pub struct ViewSpec {
    /// Surface title.
    pub title: String,
    /// The lines (may carry raw SGR).
    pub lines: Vec<String>,
    /// Visible rows; `0` = `min(lines.len(), 15)` (applied by the panel layer, ui.go:388-395).
    pub height: usize,
}

/// RAII busy-stop handle. Drop = stop exactly once; [`BusyGuard::stop`] is the readable
/// explicit form.
pub struct BusyGuard(Option<Box<dyn FnOnce() + Send>>);

impl BusyGuard {
    /// Wraps the stop closure; it runs exactly once, on `stop()` or on `Drop`.
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(f)))
    }

    /// Ends the busy phase now.
    pub fn stop(mut self) {
        self.fire();
    }

    fn fire(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.fire();
    }
}

/// RAII cancel-scope handle. Drop = pop exactly once; [`ScopeGuard::pop`] is the readable
/// explicit form.
pub struct ScopeGuard(Option<Box<dyn FnOnce() + Send>>);

impl ScopeGuard {
    /// Wraps the pop closure; it runs exactly once, on `pop()` or on `Drop`.
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(f)))
    }

    /// Pops the scope now.
    pub fn pop(mut self) {
        self.fire();
    }

    fn fire(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.fire();
    }
}

/// Metered block-preview handle (sink.go `previewWriter` contract): the writer COUNTS raw
/// source lines; one row `"label · N lines"`; 150ms throttle, FIRST tick delayed a full
/// period; close is deferred (the row stays until the rendered block morphs it). Drop = close.
/// The markdown renderer consumes the same trait (`crate::markdown::sink` re-exports it).
pub trait PreviewHandle: Send {
    /// Counts one raw source line into the metered row.
    fn write_raw_line(&mut self, line: &str);
    /// Deferred close: the row stays until the rendered block morphs it.
    fn close(&mut self);
}

/// Turn-scoped stream handle (ui.go `StreamSink`). `done()` drops a leaked preview and pops
/// the turn cancel scope.
pub trait UiStreamSink: Send + Sync {
    /// Opens a metered one-row block preview labelled `label`.
    fn block_preview(&self, label: &str) -> Box<dyn PreviewHandle>;
    /// Ends the turn's streaming: drops a leaked preview and pops the turn cancel scope.
    fn done(&self);
}

/// The facade. Blocking calls three-way-select {reply, cancel→revoke msg + cancelled shape,
/// ui-done→`Err(Closed)`}; shutdown fails ALL outstanding waiters. Fire-and-forget calls
/// never block (unbounded mailbox; ordering = region lock + mailbox FIFO).
pub trait Ui: Send + Sync {
    // ---- BLOCKING ----

    /// Blocks for the next submitted input (or a queued one).
    fn read_input<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Input, UiError>>;

    /// Opens a tabbed surface below the composer and blocks until commit or cancel.
    fn tabbed<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        spec: TabbedSpec,
    ) -> BoxFuture<'a, Result<TabbedResult, UiError>>;

    /// Steering drain: pops the contiguous non-`'/'` prefix of the queue.
    /// (The contract's `<'a>(&'a self) -> BoxFuture<'a, _>` shape, lifetime-elided.)
    fn take_queued_messages(&self) -> BoxFuture<'_, Vec<Input>>;

    /// Flush staging tail → quit → join the loop thread; returns its stored io error.
    /// (The contract's `<'a>(&'a self) -> BoxFuture<'a, _>` shape, lifetime-elided.)
    fn close(&self) -> BoxFuture<'_, std::io::Result<()>>;

    // ---- provided sugar (object-safe default bodies over `tabbed`) ----

    /// One `PanelKind::List`; `search: true` UNCONDITIONALLY (ui.go:370-386).
    fn select<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        spec: SelectSpec,
    ) -> BoxFuture<'a, Result<SelectResult, UiError>> {
        Box::pin(async move {
            let panel = Panel::list(spec.title, spec.items)
                .with_search(true)
                .with_cursor(spec.cursor);
            let r = self
                .tabbed(
                    cancel,
                    TabbedSpec {
                        panels: vec![panel],
                        ..TabbedSpec::default()
                    },
                )
                .await?;
            if r.cancelled {
                return Ok(SelectResult {
                    index: 0,
                    cancelled: true,
                });
            }
            Ok(SelectResult {
                index: r.panels.first().map_or(0, |p| p.cursor),
                cancelled: false,
            })
        })
    }

    /// One `PanelKind::View`; height 0 => `min(lines.len(), 15)` at the panel layer
    /// (ui.go:388-395).
    fn view<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        spec: ViewSpec,
    ) -> BoxFuture<'a, Result<(), UiError>> {
        Box::pin(async move {
            let panel = Panel::view(spec.title, spec.lines).with_height(spec.height);
            self.tabbed(
                cancel,
                TabbedSpec {
                    panels: vec![panel],
                    ..TabbedSpec::default()
                },
            )
            .await?;
            Ok(())
        })
    }

    /// Two-item select; `Ok(true)` iff `!cancelled && index == 0` (ui.go:415-421).
    fn confirm<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        title: &'a str,
        yes: &'a str,
        no: &'a str,
    ) -> BoxFuture<'a, Result<bool, UiError>> {
        Box::pin(async move {
            let r = self
                .select(
                    cancel,
                    SelectSpec {
                        title: title.to_owned(),
                        items: vec![yes.to_owned(), no.to_owned()],
                        cursor: 0,
                    },
                )
                .await?;
            Ok(!r.cancelled && r.index == 0)
        })
    }

    // ---- FIRE-AND-FORGET ----

    /// Commits rendered lines into scrollback (in order, batched).
    fn print_lines(&self, lines: Vec<String>);

    /// Renders the reverse-video user block for a submitted input.
    fn user_block(&self, display: &str);

    /// Pushes the TURN cancel scope (index 0) and returns the turn's stream handle.
    fn start_stream(&self, cancel: CancellationToken) -> Box<dyn UiStreamSink>;

    /// Starts the busy phase; the UI appends elapsed (≥2s) + `" (ESC to cancel)"` itself
    /// (ui-architecture.md:77).
    fn busy(&self, label: &str) -> BusyGuard;

    /// Updates the busy detail only — never resets the phase clock.
    fn busy_detail(&self, detail: &str);

    /// Pushes a cancel scope; the guard pops it.
    fn push_cancel_scope(&self, cancel: CancellationToken) -> ScopeGuard;

    /// Replaces the status-row data.
    fn set_status(&self, s: StatusData);

    /// Sets the terminal title. Sanitized here (`sanitize_window_title`) — a SECURITY
    /// property; emitted on change only.
    fn set_title(&self, title: &str);

    /// Installs the slash-command table backing completion and the suggestion row.
    fn set_slash_commands(&self, cmds: Vec<Suggestion>);

    // widget verbs (activity groups):

    /// ENSURE semantics: an existing call preview relabels in place keeping clock+detail.
    fn call_preview(&self, label: &str);

    /// Updates the call widget's detail segment.
    fn call_detail(&self, detail: &str);

    /// Appends one event row under the call widget.
    fn call_line(&self, line: &str);

    /// Deferred close: `open = false` only (the row stays until morphed).
    fn close_preview(&self);

    /// Pauses the call widget's clock (idempotent).
    fn pause_clock(&self);

    /// Resumes the call widget's clock (idempotent).
    fn resume_clock(&self);

    /// Replaces the call widget's body rows wholesale (progressive image frames; ui.go:123).
    fn call_body(&self, rows: Vec<String>);

    // terminal signals (ui.go:287,295; host integration — T3):

    /// Sets the OSC 9;4 progress state; emitted on change only.
    fn set_progress(&self, s: ProgressState);

    /// Attention ping: OSC 9 + BEL while the terminal is UNFOCUSED. Sanitized here with
    /// `sanitize_window_title` (the same 60-rune/control-strip sanitizer as the title).
    fn notify(&self, text: &str);

    /// Re-shades the input rows for a between-turn background flip (chat/theme.go
    /// `applyCodeTheme`).
    fn set_dark_background(&self, dark: bool);

    // ---- cross-thread reads (atomics) ----

    /// Terminal width; starts 80.
    fn width(&self) -> u16;

    /// Terminal height; starts 24 — "24 until the first resize event".
    fn height(&self) -> u16;

    /// Cancelled when the UI loop thread exits (the MCP reporter selects on it).
    fn done(&self) -> CancellationToken;
}

/// Strip runes < 0x20 and 0x7f, trim, cap 60 runes + `'…'` (61 total). ui.go:301-313;
/// vector `"a\x1b]0;evil\x07b\nc"` → `"a]0;evilbc"`. Also guards any future notify text.
pub(crate) fn sanitize_window_title(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|&c| c >= '\u{20}' && c != '\u{7f}')
        .collect();
    let trimmed = cleaned.trim();
    let mut runes = trimmed.chars();
    let head: String = runes.by_ref().take(60).collect();
    if runes.next().is_some() {
        head + "…"
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{BusyGuard, ScopeGuard, UiError, sanitize_window_title};

    // Go: internal/ui/title_test.go:14
    #[test]
    fn test_sanitize_window_title() {
        let tests = [
            ("plain CJK", "二次方程求根公式", "二次方程求根公式"),
            ("ascii", "My chat", "My chat"),
            ("trimmed", "  hello  ", "hello"),
            ("injection stripped", "a\x1b]0;evil\x07b\nc", "a]0;evilbc"),
        ];
        for (name, input, want) in tests {
            assert_eq!(sanitize_window_title(input), want, "{name}");
        }
    }

    // Go: internal/ui/title_test.go:30
    #[test]
    fn test_sanitize_window_title_truncates() {
        let got = sanitize_window_title(&"字".repeat(100));
        let runes: Vec<char> = got.chars().collect();
        assert_eq!(runes.len(), 61, "60 runes + ellipsis: {got:?}");
        assert_eq!(runes[60], '…');
        // Exactly 60 runes stays untouched (no ellipsis).
        let exact = "字".repeat(60);
        assert_eq!(sanitize_window_title(&exact), exact);
    }

    #[test]
    fn error_display_is_byte_exact() {
        // Go: internal/ui/ui.go:27,31
        assert_eq!(UiError::Closed.to_string(), "ui: closed");
        assert_eq!(UiError::Interrupted.to_string(), "ui: interrupted");
    }

    #[test]
    fn guards_fire_exactly_once() {
        let n = Arc::new(AtomicUsize::new(0));
        {
            let n = Arc::clone(&n);
            let g = BusyGuard::new(move || {
                n.fetch_add(1, Ordering::SeqCst);
            });
            g.stop(); // explicit stop; the following drop must not double-fire
        }
        assert_eq!(n.load(Ordering::SeqCst), 1);

        let n = Arc::new(AtomicUsize::new(0));
        {
            let n = Arc::clone(&n);
            let _g = ScopeGuard::new(move || {
                n.fetch_add(1, Ordering::SeqCst);
            });
            // dropped without pop(): Drop fires once
        }
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
