//! The turn engine (chat/run.go:1004-1107, 1184-1407, 1696-1839): the streaming state
//! machine one round writes into, the busy-phase controller above it, the per-round image
//! collection (T-39), and the turn-dispatch rule (T-36).
//!
//! **Streaming without pipes.** Go handed the provider two `io.Writer`s and choreographed
//! the reader with `io.Pipe` + blocking reads (reasoning pipe first, content pipe second,
//! a tee into the history buffer, the pipe cut at the first tool delta). Rust providers
//! already take a sink, so all of that collapses into [`RenderSink`] — an explicit state
//! machine owned by the round:
//!
//! ```text
//! Waiting ──reasoning()──▶ Thinking ──reasoning_done()──▶ Waiting
//!    │                                                       │
//!    └──────────────── content() ────────────────────────────┘
//!                          │
//!                          ▼
//!                       Content ──tool_delta()──▶ Composing (content spills)
//! ```
//!
//! The sink runs INSIDE the awaited provider call, so every happens-before subtlety Go
//! documented (mark-content before a racing observer; observer installed before the stream
//! goroutine) is plain call order here.
//!
//! The `Composing`/spill half engages when a dialect emits `StreamSink::tool_delta`
//! (openai, anthropic and openresponses do). A dialect that does not (google) takes the
//! atomic path: the call widget rises at the tool walk instead.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::agents::compose_send_history;
use crate::chat::ChatError;
use crate::chat::images::{IMAGE_INDENT_COLS, IMAGE_MAX_COLS, IMAGE_MAX_ROWS, save_image};
use crate::host::{Presenter, State};
use crate::llm::progress::{TURN_PROGRESS, TurnProgress};
use crate::markdown::{CodeTheme, RenderOptions, Writer, hyperlink};
use crate::provider::model::{Attachment, Message};
use crate::provider::sink::StreamSink;
use crate::provider::{Provider, RoundResult, ToolProvider};
use crate::tool::Dispatcher;
use crate::tool::Presentation;
use crate::ui::facade::{Ui, UiError, UiStreamSink};
use tokio_util::sync::CancellationToken;

use crate::repl::approval::ApprovalGate;
use crate::repl::group::{ThinkingMeter, composing_label};
use crate::repl::meter::CtxMeter;
use crate::repl::phases::watch_phases;
use crate::repl::steer::Steerer;
use crate::repl::toolloop::tool_loop;
use crate::repl::transcript::{ContentCommitter, Transcript};
use crate::repl::uisink::UiMdSink;

// The busy-phase controller moved to `phases.rs` (T3 design D9); the names stay reachable here.
pub(crate) use crate::repl::phases::{PHASE_WAITING, Phases};

/// The progressive frame's widget geometry (chat/images.go:200 `watchImagePartials`).
pub(crate) const PARTIAL_COLS: usize = 64;
/// See [`PARTIAL_COLS`].
pub(crate) const PARTIAL_ROWS: usize = 12;

/// Resolves the session's image directory lazily — only when an image actually needs
/// saving, so the bundle's lazy-creation contract survives image-less turns
/// (chat/images.go:115).
pub(crate) type ImagesDir = Arc<dyn Fn() -> Option<std::path::PathBuf> + Send + Sync>;

/// Everything a turn needs that does not change between turns (built once by the run
/// loop). The per-turn cancel token and stream handle are minted by [`run_turn`].
pub(crate) struct TurnCtx {
    /// The facade.
    pub(crate) ui: Arc<dyn Ui>,
    /// The single writer to the chat area.
    pub(crate) tr: Arc<Transcript>,
    /// The tool dispatcher (LIVE — never cached across rounds).
    pub(crate) dispatch: Arc<dyn Dispatcher>,
    /// The conversation's ONE approval gate.
    pub(crate) gate: Arc<ApprovalGate>,
    /// The agent-mode overlay woven into every send.
    pub(crate) overlay: String,
    /// Where generated images are saved (T-39).
    pub(crate) images_dir: ImagesDir,
    /// `false` for dedicated image providers: every attempt bills, so a round is never
    /// auto-retried (chat/run.go:1428).
    pub(crate) can_retry: bool,
    /// Code-block theme of the detected terminal background; refreshed BETWEEN turns only,
    /// so a theme flip never lands inside a streaming block (chat/run.go:1007).
    pub(crate) code_theme: CodeTheme,
    /// The host presenter (progress state + attention pings — T3).
    pub(crate) pres: Arc<Presenter>,
}

/// One turn's live scaffolding: the turn cancel scope, the stream handle
/// `Ui::start_stream` pushed it onto, and the upload-progress reporter every round's provider
/// call is scoped under.
pub(crate) struct Turn<'a> {
    pub(crate) cx: &'a TurnCtx,
    pub(crate) sink: Arc<dyn UiStreamSink>,
    pub(crate) cancel: CancellationToken,
    pub(crate) progress: Arc<TurnProgress>,
}

/// What a completed turn produced (Go's `reply, thinking` plus the terminating round's
/// images, which `Run`'s success path attaches to the assistant message).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct TurnOutput {
    /// The final reply text.
    pub(crate) content: String,
    /// The final reasoning text.
    pub(crate) reasoning: String,
    /// Images the TERMINATING round generated (earlier rounds' images are already
    /// attached to their own assistant message — T-39).
    pub(crate) images: Vec<Attachment>,
    /// What the TERMINATING round's API call billed (`None` = the provider reported
    /// nothing).
    ///
    /// Go read it back off the provider (`LastUsageFull`) after the turn; the Rust
    /// dialects hand each call's figure to their caller instead (D-55), so the terminating
    /// round has to carry it out — mid-loop rounds already stamp their own message. The
    /// run loop stamps it onto the assistant message it builds, which is what a resumed
    /// session recomputes its cumulative totals from.
    pub(crate) usage: Option<crate::provider::usage::Usage>,
    /// The TERMINATING round's raw blocks. A thinking block must go back on every later
    /// request that replays this turn, and a turn ending in text is the common case, not
    /// the tool-round one; mid-loop rounds already stamp their own message
    /// (chat/run.go:1092-1099).
    pub(crate) raw_content: Option<crate::provider::model::RawContent>,
}

/// Why a turn ended badly. `Chat` is the turn's own failure (provider, cap, or the user's
/// interrupt); `Ui` means the facade closed under the turn, which ends the whole loop.
#[derive(Debug)]
pub(crate) enum TurnFailure {
    /// The turn failed.
    Chat(ChatError),
    /// The facade closed (`ReplError::Ui` for the run loop).
    Ui(UiError),
}

impl From<ChatError> for TurnFailure {
    fn from(e: ChatError) -> Self {
        Self::Chat(e)
    }
}

/// What a turn hands back to the run loop: the outcome, the bookkeeping the turn-level
/// retry reads, and the partials the user actually saw. The partials travel outside the
/// outcome because `ChatError::Interrupted` only signals the class: `finalize_interrupt`
/// needs the streamed text to decide whether the turn is kept (chat/run.go:1327-1332).
#[derive(Debug)]
pub(crate) struct TurnReport {
    /// The turn's result, or why it ended badly.
    pub(crate) outcome: Result<TurnOutput, TurnFailure>,
    /// Whether the turn ran the tool loop over a non-empty tool set — Go's `usedTools`,
    /// the first clause of the turn-level replay guard (chat/run.go:1032-1036).
    pub(crate) used_tools: bool,
    /// Executed calls the parallel gate does not vouch for as read-only — what a
    /// whole-turn replay would run AGAIN (chat/run.go:1030-1031).
    pub(crate) side_fx: u32,
    /// Content streamed before the user interrupted (EMPTY on every other outcome — Go's
    /// `fail()` returns the buffers only on cancellation).
    pub(crate) partial: String,
    /// Reasoning streamed before the interrupt.
    pub(crate) partial_reasoning: String,
}

impl TurnReport {
    /// Whether the user interrupted (the finalize path, not the error path).
    pub(crate) fn is_interrupted(&self) -> bool {
        matches!(self.outcome, Err(TurnFailure::Chat(ChatError::Interrupted)))
    }

    /// A completed turn; `used_tools` is stamped by `run_turn`.
    pub(crate) fn success(out: TurnOutput, side_fx: u32) -> Self {
        Self {
            outcome: Ok(out),
            used_tools: false,
            side_fx,
            partial: String::new(),
            partial_reasoning: String::new(),
        }
    }

    /// A failure carrying no partials.
    pub(crate) fn failed(err: impl Into<TurnFailure>, side_fx: u32) -> Self {
        Self {
            outcome: Err(err.into()),
            used_tools: false,
            side_fx,
            partial: String::new(),
            partial_reasoning: String::new(),
        }
    }

    /// A failure carrying neither partials nor bookkeeping.
    pub(crate) fn bare(err: impl Into<TurnFailure>) -> Self {
        Self::failed(err, 0)
    }
}

/// What one streamed round leaves behind: the round, and the partials the sink
/// accumulated. On a user interrupt those ARE the answer so far, and the caller carries
/// them out of the turn; a non-interrupt failure leaves them empty (Go's `fail()` shape) —
/// a half-streamed answer under a provider error is not something the history should keep.
pub(crate) struct RoundOutcome {
    /// The round, or why it failed.
    pub(crate) result: Result<RoundResult, ChatError>,
    /// Content streamed before a user interrupt.
    pub(crate) partial: String,
    /// Reasoning streamed before a user interrupt.
    pub(crate) partial_reasoning: String,
}

impl RoundOutcome {
    /// A result with no partials.
    fn of(result: Result<RoundResult, ChatError>) -> Self {
        Self {
            result,
            partial: String::new(),
            partial_reasoning: String::new(),
        }
    }
}

/// The turn-dispatch rule (T-36; `TUI_DESIGN` §8.3).
///
/// A provider that can call tools routes EVERY turn through the tool loop — with an EMPTY
/// tools slice when none are configured, because every wire dialect omits an empty `tools`
/// array, so a no-tools turn still streams markdown, still meters previews and the
/// thinking widget, and ESC still yields partials. Providers WITHOUT tool support (the
/// dedicated image dialects) take the unary [`stream_turn`] fallback: no streaming there,
/// and an interrupt takes the zero-yield finalize path by construction.
///
/// The turn's cancel scope and stream handle are minted here and torn down in the pinned
/// order `sink.done()` → `tr.reset_turn()` → `cancel()` (chat/run.go:1069-1071) on EVERY
/// exit path: `done` drops a leaked preview and pops the turn scope, `reset_turn` reclaims
/// a dropped widget's separator, and only then does the token fire.
pub(crate) async fn run_turn(
    cx: &TurnCtx,
    parent_cancel: &CancellationToken,
    provider: &dyn Provider,
    history: &mut Vec<Message>,
    ctxm: &mut CtxMeter,
    steer: &mut Steerer,
) -> TurnReport {
    let cancel = parent_cancel.child_token();
    let sink: Arc<dyn UiStreamSink> = Arc::from(cx.ui.start_stream(cancel.clone()));
    cx.pres.set_state(State::Busy);
    let turn = Turn {
        cx,
        sink,
        cancel: cancel.clone(),
        progress: TurnProgress::new(),
    };
    let tools = cx.dispatch.tools();
    // Go's `usedTools` (run.go:1032): a turn that advertised tools is never replayed
    // whole — the tool loop already retried its own failing calls with the completed
    // rounds, and their side effects, kept in place.
    let used_tools = provider.as_tool_provider().is_some() && !tools.is_empty();
    let mut report = match provider.as_tool_provider() {
        Some(tp) => tool_loop(&turn, tp, history, ctxm, steer, tools).await,
        None => stream_turn(&turn, provider, history).await,
    };
    turn.sink.done();
    cx.tr.reset_turn();
    cancel.cancel();
    report.used_tools = used_tools;
    report
}

/// The UNARY fallback for a provider without tool support (T-36): one `Provider::chat`
/// call under the turn's sink scaffolding, its reply rendered as one markdown block.
///
/// Go's `streamTurn` streamed here through `Provider.StreamChat`; that seam is not ported
/// (D-20), so this path has no text partials — an interrupt yields nothing and
/// `finalize_interrupt` takes its rollback branch, which is exactly what an image turn
/// wants (the /edit canvas comes back as a pending attachment). A provider with the
/// progressive-frame capability is called through `chat_observed` instead, its frames
/// reaching the [`PartialWatcher`] (D4; interactive only — the headless body stays unary).
pub(crate) async fn stream_turn(
    t: &Turn<'_>,
    provider: &dyn Provider,
    history: &[Message],
) -> TurnReport {
    t.cx.tr.begin_round();
    let phases = Phases::new(Arc::clone(&t.cx.ui));
    let send = compose_send_history(history, &t.cx.overlay);
    let res = TURN_PROGRESS
        .scope(Arc::clone(&t.progress), async {
            let _watch = watch_phases(&t.progress, phases.clone());
            match provider.as_image_partial_provider() {
                Some(ip) => {
                    let mut w = PartialWatcher::new(Arc::clone(&t.cx.ui), Arc::clone(&t.cx.tr));
                    ip.chat_observed(&t.cancel, &send, &mut |f| w.frame(f))
                        .await
                }
                None => provider.chat(&t.cancel, &send).await,
            }
        })
        .await;
    phases.end();
    let result = match res {
        Ok(r) => r,
        Err(e) => {
            if t.cancel.is_cancelled() {
                return TurnReport::bare(ChatError::Interrupted);
            }
            return TurnReport::bare(ChatError::Provider(e));
        }
    };
    if !result.text.is_empty() {
        let mut block = Scaffold::of(t).block(t.cx.tr.content_block());
        block.write(&result.text);
        block.finish();
    }
    TurnReport::success(
        TurnOutput {
            content: result.text,
            reasoning: String::new(),
            images: result.images,
            usage: result.usage,
            // The unary path exposes no raw blocks (chat.go:116 sends the plain history).
            raw_content: None,
        },
        0,
    )
}

/// Streams ONE tool round and renders it (chat/run.go:1696-1839 `streamToolRound`).
///
/// Returns the round beside the partials the sink accumulated (see [`RoundOutcome`]).
pub(crate) async fn stream_round(
    t: &Turn<'_>,
    tp: &dyn ToolProvider,
    send: &[Message],
    tools: &[crate::provider::model::ToolDef],
) -> RoundOutcome {
    // A round that died mid-stream may have leaked the streaming guards; clearing them
    // makes re-entry (a retried attempt) safe.
    t.cx.tr.begin_round();
    let mut sink = RenderSink::new(t);
    let res = TURN_PROGRESS
        .scope(Arc::clone(&t.progress), async {
            let _watch = watch_phases(&t.progress, sink.phases.clone());
            tp.stream_chat_with_tools(&t.cancel, send, tools, &mut sink)
                .await
        })
        .await;
    // Whatever streamed is committed before anything else happens: the trailing partial
    // line lands, the content block closes, and a tool call deferred behind it is raised.
    sink.close_block();
    let interrupted = t.cancel.is_cancelled();
    match res {
        Err(e) => {
            let (partial, reasoning) = sink.finish_partials();
            if interrupted {
                RoundOutcome {
                    result: Err(ChatError::Interrupted),
                    partial,
                    partial_reasoning: reasoning,
                }
            } else {
                RoundOutcome::of(Err(ChatError::Provider(e)))
            }
        }
        Ok(_) if interrupted => {
            let (partial, reasoning) = sink.finish_partials();
            RoundOutcome {
                result: Err(ChatError::Interrupted),
                partial,
                partial_reasoning: reasoning,
            }
        }
        Ok(round) => {
            // Reasoning-only response: the model thought and said nothing else, so the
            // reasoning IS the answer — rendered as content rather than reported as an
            // empty turn (run.go:1811-1817). Images make a text-less round valid on their
            // own, so they suppress the rule.
            if round.tool_calls.is_empty()
                && round.content.is_empty()
                && round.images.is_empty()
                && !round.reasoning.is_empty()
            {
                sink.render_answer(&round.reasoning);
                sink.finish();
                return RoundOutcome::of(Ok(RoundResult {
                    content: round.reasoning.clone(),
                    ..round
                }));
            }
            sink.render_spill();
            sink.finish();
            RoundOutcome::of(Ok(round))
        }
    }
}

/// Saves a round's generated images, attaches the SAVED subset to the message they belong
/// to, and renders each one as a half-block block (T-39 + T-16; chat/images.go:117-146).
///
/// Reuses the headless `save_images_for_turn` so an interactive bundle and a `-m` one
/// write the same files with the same names. `width` is the terminal width the picture is
/// fitted to: `min(72, width - 2 - IMAGE_INDENT_COLS)` columns × 14 rows. A decode failure
/// still saved the file — the caption line with the reason appended IS the fallback
/// rendering (`"{caption} ({why})"`, the `"decode image: "` prefix stripped).
pub(crate) fn collect_images(
    tr: &Transcript,
    width: usize,
    dir: Option<&Path>,
    images: &[Attachment],
    msg: &mut Message,
) {
    if images.is_empty() {
        return;
    }
    let max_cols = IMAGE_MAX_COLS.min(width.saturating_sub(2 + IMAGE_INDENT_COLS));
    for (i, att) in images.iter().enumerate() {
        // Go saves, attaches and renders ONE image at a time, so a failure lands between its
        // neighbours' pictures rather than ahead of them (images.go:126-145). The naming rules
        // are `save_image`'s — the same call the headless `-m` tail makes.
        let path = match save_image(att, dir, i) {
            Ok(p) => p,
            Err(e) => {
                tr.error(&format!("Saving image failed: {e}"));
                continue;
            }
        };
        let path = path.to_string_lossy().into_owned();
        msg.attachments.push(Attachment {
            // `filepath.Base` of the saved path: the link from the persisted record to the file.
            filename: Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            mime_type: att.mime_type.clone(),
            data: att.data.clone(),
        });
        let caption = format!(
            "🖼 saved: {}",
            hyperlink(&format!("file://{path}"), &path, crate::color::enabled())
        );
        match crate::imgterm::render(&att.data, max_cols, IMAGE_MAX_ROWS) {
            Ok(rows) => tr.image(&rows, &caption),
            // A decode failure still saved the file: the caption line, with the reason appended,
            // IS the fallback rendering (images.go:143).
            Err(e) => {
                let why = e.to_string();
                let why = why.strip_prefix("decode image: ").unwrap_or(&why);
                tr.notice(&format!("{caption} ({why})"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Progressive image frames
// ---------------------------------------------------------------------------

/// Feeds a streaming image generation's partial frames into the call widget
/// (chat/images.go:187-208 `watchImagePartials`): the FIRST frame raises the `image` widget
/// (`Transcript::image_widget`), every frame is rasterised at [`PARTIAL_COLS`] × [`PARTIAL_ROWS`]
/// and replaces the widget's body through `Ui::call_body`; a frame that fails to decode is
/// skipped silently.
///
/// Partial frames are full-resolution, low-DETAIL pictures, so they render at about half the
/// final block's size: the composition is visible while it refines, and frame GROWTH is safe —
/// only a shrinking widget bounces the composer.
///
/// Runs on the stream task; the facade is safe for concurrent use.
pub(crate) struct PartialWatcher {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    raised: bool,
}

impl PartialWatcher {
    /// A watcher with no widget raised yet.
    pub(crate) fn new(ui: Arc<dyn Ui>, tr: Arc<Transcript>) -> Self {
        Self {
            ui,
            tr,
            raised: false,
        }
    }

    /// One partial frame (decoded bytes).
    ///
    /// The FIRST frame raises the widget: a dedicated image provider's turn has no tool-call
    /// widget to fill, so it opens one (which the final `Transcript::image` then morphs in
    /// place). Tool-driven generation already has one up, and the raise is a no-op there.
    pub(crate) fn frame(&mut self, data: &[u8]) {
        if !self.raised {
            self.raised = true;
            self.tr.image_widget();
        }
        let Ok(rows) = crate::imgterm::render(data, PARTIAL_COLS, PARTIAL_ROWS) else {
            return; // an undecodable frame is simply skipped
        };
        self.ui.call_body(rows);
    }
}

// ---------------------------------------------------------------------------
// The stream state machine
// ---------------------------------------------------------------------------

/// Where a round's stream currently is (`TUI_DESIGN` §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SinkState {
    /// Nothing streamed yet (or a thinking segment just settled).
    Waiting,
    /// A reasoning segment owns the widget.
    Thinking,
    /// A markdown content block is open.
    Content,
    /// The model started composing a tool call: the render pipe is CUT and later content
    /// spills (entered on the first `StreamSink::tool_delta`).
    Composing,
}

/// One open markdown content block: the renderer and the sink whose trailing partial line
/// only `flush` commits.
struct ContentBlock {
    mdw: Writer,
    sink: UiMdSink,
}

impl ContentBlock {
    fn write(&mut self, delta: &str) {
        self.mdw.write(delta.as_bytes());
    }

    /// Flushes the renderer, then commits the trailing partial line.
    fn finish(mut self) {
        self.mdw.flush();
        self.sink.flush();
    }
}

/// The `StreamSink` a round hands the provider: the reasoning meter, the markdown content
/// block, the history buffers and the composing observer, all as ONE state machine (see
/// the module docs).
pub(crate) struct RenderSink {
    tr: Arc<Transcript>,
    dispatch: Arc<dyn Dispatcher>,
    phases: Phases,
    state: SinkState,
    /// Every content byte the provider emitted — the history buffer AND the interrupt
    /// partial (Go's `contentBuf`).
    content: String,
    /// Every reasoning byte (Go's `reasonBuf`).
    reasoning: String,
    /// Content streamed AFTER the composing cut; rendered as its own block after the
    /// stream — misordered but VISIBLE, never silently dropped (run.go:1668-1691).
    spill: String,
    /// `tr.mark_content` fires exactly once, on the first content byte.
    marked: bool,
    thinking_start: Option<Instant>,
    meter: Option<ThinkingMeter>,
    block: Option<ContentBlock>,
    /// A block was opened and closed: `close_block` must not re-raise the deferred widget.
    block_closed: bool,
    composing_seen: bool,
    composing_label: String,
    /// Everything the block needs to be re-opened after the cut (a turn-lifetime clone of
    /// the scaffolding, so the sink does not borrow the `Turn`).
    scaffold: Scaffold,
    /// Progressive image frames from a tool-driven generation (`image_partial`).
    partials: PartialWatcher,
}

/// The turn-lifetime handles a content block needs (a `Turn` cannot be borrowed by the
/// sink: the provider call borrows the sink mutably for its whole duration).
#[derive(Clone)]
struct Scaffold {
    ui: Arc<dyn Ui>,
    tr: Arc<Transcript>,
    stream: Arc<dyn UiStreamSink>,
    code_theme: CodeTheme,
}

impl Scaffold {
    fn of(t: &Turn<'_>) -> Self {
        Self {
            ui: Arc::clone(&t.cx.ui),
            tr: Arc::clone(&t.cx.tr),
            stream: Arc::clone(&t.sink),
            code_theme: t.cx.code_theme,
        }
    }

    /// Opens a block committing through `committer`, metering previews on the turn's
    /// stream handle and materialising the transcript's latched separator before a
    /// preview claims the next row.
    fn block(&self, committer: ContentCommitter) -> ContentBlock {
        let stream = Arc::clone(&self.stream);
        let ui = Arc::clone(&self.ui);
        let tr = Arc::clone(&self.tr);
        let mut committer = committer;
        let sink = UiMdSink::new(
            Box::new(move |label| stream.block_preview(label)),
            Box::new(move |lines| committer.push(&lines)),
            Box::new(move || usize::from(ui.width())),
            Some(Box::new(move || tr.flush_pending())),
        );
        ContentBlock {
            mdw: Writer::new(
                Box::new(sink.clone()),
                RenderOptions {
                    color: crate::color::enabled(),
                    code_theme: self.code_theme,
                },
            ),
            sink,
        }
    }
}

impl RenderSink {
    /// A sink for one round of `t`.
    pub(crate) fn new(t: &Turn<'_>) -> Self {
        Self {
            tr: Arc::clone(&t.cx.tr),
            dispatch: Arc::clone(&t.cx.dispatch),
            phases: {
                let p = Phases::new(Arc::clone(&t.cx.ui));
                p.set(PHASE_WAITING);
                p
            },
            state: SinkState::Waiting,
            content: String::new(),
            reasoning: String::new(),
            spill: String::new(),
            marked: false,
            thinking_start: None,
            meter: None,
            block: None,
            block_closed: false,
            composing_seen: false,
            composing_label: String::new(),
            scaffold: Scaffold::of(t),
            partials: PartialWatcher::new(Arc::clone(&t.cx.ui), Arc::clone(&t.cx.tr)),
        }
    }

    /// Commits the open content block and raises a tool call deferred behind it. A no-op
    /// once the composing cut already closed the block.
    pub(crate) fn close_block(&mut self) {
        self.settle_thinking();
        if let Some(block) = self.block.take() {
            block.finish();
            self.block_closed = true;
            self.tr.close_content();
        }
    }

    /// Renders `text` as a fresh content block — the reasoning-only answer
    /// (run.go:1811-1817).
    pub(crate) fn render_answer(&mut self, text: &str) {
        let mut block = self.scaffold.block(self.tr.open_content());
        block.write(text);
        block.finish();
        self.tr.close_content();
    }

    /// Commits what the model streamed after the composing cut (run.go:1710-1720
    /// `renderSpill`). It lands BELOW the raised widget — misordered like Go's
    /// pre-deferral behavior, but visible.
    pub(crate) fn render_spill(&mut self) {
        if self.spill.is_empty() {
            return;
        }
        let spill = std::mem::take(&mut self.spill);
        let mut block = self.scaffold.block(self.tr.content_block());
        block.write(&spill);
        block.finish();
    }

    /// Ends the round's busy phase (Go's `defer phases.end()`).
    pub(crate) fn finish(&mut self) {
        self.phases.end();
    }

    /// [`Self::finish`] plus the accumulated partials, in that order.
    pub(crate) fn finish_partials(mut self) -> (String, String) {
        self.finish();
        (self.content, self.reasoning)
    }

    /// Settles a live thinking segment (idempotent).
    fn settle_thinking(&mut self) {
        self.meter = None;
        if let Some(start) = self.thinking_start.take() {
            self.tr.settle_thinking(start);
        }
        if self.state == SinkState::Thinking {
            self.state = SinkState::Waiting;
        }
    }
}

impl StreamSink for RenderSink {
    /// The first content byte marks the transcript's content block open — so a tool call
    /// announced by the observer in the same network read still defers — and opens the
    /// markdown block. Every byte lands in the history buffer whatever the state; past the
    /// composing cut the bytes spill instead of rendering.
    fn content(&mut self, delta: &str) {
        if !self.marked {
            self.marked = true;
            self.tr.mark_content();
        }
        self.content.push_str(delta);
        if self.state == SinkState::Composing {
            self.spill.push_str(delta);
            return;
        }
        if self.state != SinkState::Content {
            // Defensive: dialects close reasoning through `ReasoningGate`, but a sink that
            // is written to directly must not leave the thinking widget up.
            self.settle_thinking();
            self.phases.end(); // streaming output is its own progress from here
            self.block = Some(self.scaffold.block(self.tr.open_content()));
            self.block_closed = false;
            self.state = SinkState::Content;
        }
        if let Some(block) = &mut self.block {
            block.write(delta);
        }
    }

    /// Opens the thinking widget on the first reasoning byte and feeds its metered token
    /// row (throttled to 150 ms inside the meter).
    fn reasoning(&mut self, delta: &str) {
        self.reasoning.push_str(delta);
        if self.state != SinkState::Thinking {
            self.phases.end(); // the thinking widget takes over from the status spinner
            self.thinking_start = Some(Instant::now());
            self.meter = Some(self.tr.open_thinking());
            self.state = SinkState::Thinking;
        }
        if let Some(m) = &mut self.meter {
            m.add(delta);
        }
    }

    /// Settles the thinking segment; idempotent, and a no-op when no reasoning streamed.
    fn reasoning_done(&mut self) {
        self.settle_thinking();
    }

    /// One progressive image frame from a streaming generation: the first raises the `image`
    /// widget, every frame repaints its body (chat/images.go:187-208 `watchImagePartials`).
    fn image_partial(&mut self, frame: &[u8]) {
        self.partials.frame(frame);
    }

    /// The composing observer (chat/run.go:1266-1296 `watchToolComposing`), reachable only
    /// once a dialect emits named argument deltas (T-11).
    ///
    /// The FIRST delta ends the content stream for this round — models emit text before
    /// tool calls, so closing the block now makes buffered blocks flush and the deferred
    /// widget rise promptly instead of after the whole argument stream. Empty deltas
    /// (atomic backends) are ignored and an anonymous one never raises a widget: the
    /// zombie-spinner rule. Interactive tools are skipped too — their call never enters
    /// the activity panel, so a widget raised for one would sit behind the tool's own
    /// surface.
    fn tool_delta(&mut self, name: Option<&str>, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if !self.composing_seen {
            self.composing_seen = true;
            self.close_block();
            self.state = SinkState::Composing;
        }
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            return;
        };
        if self.dispatch.presentation(name) == Presentation::Surface {
            return;
        }
        let label = composing_label(name);
        if label == self.composing_label {
            return;
        }
        self.composing_label.clone_from(&label);
        self.phases.end(); // the widget takes over from the status spinner
        self.tr.open_call(&label);
    }
}
