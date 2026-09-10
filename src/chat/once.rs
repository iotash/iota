//! The headless entry point (chat/chat.go:35-68): one message in, the reply (or the JSON report) out.

use std::{io::Write, path::PathBuf, sync::Arc};

use crate::chat::turns::{RunCtx, TurnBudget};
use crate::provider::Provider;
use crate::provider::model::Message;
use crate::tool::Dispatcher;
use tokio_util::sync::CancellationToken;

use crate::chat::error::ChatError;
use crate::chat::report::write_report;
use crate::chat::run::{QuietHost, RunRequest, install_tool_searcher, run_once};
use crate::chat::{AgentOptions, OutputFormat};

/// Everything `once` needs beyond the provider and dispatcher.
pub struct OnceOptions {
    /// The user message.
    pub message: String,
    /// The system prompt (`""` = none).
    pub system: String,
    /// Agent-mode overlay settings.
    pub agent: AgentOptions,
    /// `--max-turns`; `None` = unlimited (no `TurnBudget`).
    pub max_turns: Option<std::num::NonZeroU32>,
    /// `--output-format`.
    pub format: OutputFormat,
    /// Where generated images are saved; `None` = `$HOME` unknown (every save fails with `HOME_NOT_DEFINED`).
    /// A resumed run points it at the session bundle's `images/` (DIVERGENCES D-53).
    pub images_dir: Option<PathBuf>,
    /// The resumed session's view, replayed ahead of the new user message. A NON-EMPTY history WINS over
    /// `system` (chat/run.go:69-74); empty (the default) is the stateless single-shot run.
    pub history: Vec<Message>,
}

/// What one `once` produced beyond its output: the turn's message delta, for the caller to persist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnceOutcome {
    /// Everything the turn appended past the imported history — Go's `history[persisted:]` (chat/run.go:221-229).
    pub delta: Vec<Message>,
}

/// Prefix of each saved-image line in text mode (chat.go:62: `🖼 saved: %s\n`).
pub(crate) const IMAGE_SAVED_PREFIX: &str = "🖼 saved: ";

/// JSON: ALWAYS writes the report (also on error/cancel) then returns the run error (a write error wins).
/// Text: on error writes nothing; else `reply\n` when non-empty, then `🖼 saved: {path}\n` per image, then each
/// image error line + "\n".
/// If the token is cancelled and the run failed, the error becomes `ChatError::Interrupted`.
/// `out` is `dyn Write + Send` so the future (and `crate::cmd::run`) stays `Send`; `Streams` already stores
/// `Box<dyn Write + Send>`.
/// On success the turn's delta travels out in `OnceOutcome` so a resumed run can persist it; a failure persists
/// nothing (DIVERGENCES D-43).
pub async fn once(
    cancel: CancellationToken,
    provider: &mut dyn Provider,
    dispatch: Arc<dyn Dispatcher>,
    opts: OnceOptions,
    out: &mut (dyn Write + Send),
) -> Result<OnceOutcome, ChatError> {
    // --max-turns is the RUN's budget, not the parent loop's: it travels in the context, and the local
    // per-loop cap is left off so the two cannot double-count (chat.go:36-47).
    let cx = RunCtx {
        cancel,
        budget: opts.max_turns.map(TurnBudget::new),
        ..RunCtx::default()
    };
    install_tool_searcher(provider, &dispatch);
    let mut host = QuietHost::new();
    let req = RunRequest {
        message: opts.message,
        system: opts.system,
        agent: opts.agent,
        history: opts.history,
    };
    let result = run_once(
        &cx,
        &*provider,
        &req,
        dispatch,
        None,
        &mut host,
        opts.images_dir.as_deref(),
    )
    .await;
    let result = match result {
        Err(_) if cx.cancel.is_cancelled() => Err(ChatError::Interrupted),
        other => other,
    };

    match opts.format {
        OutputFormat::Json => {
            // The report IS the output whether or not the run succeeded: the rounds that completed were
            // billed. The error still travels, so the exit status keeps its meaning (chat.go:49-54).
            let (reply, images, image_errors) = match &result {
                Ok(o) => (o.reply.as_str(), o.images.clone(), o.image_errors.clone()),
                Err(_) => ("", Vec::new(), Vec::new()),
            };
            let err = result.as_ref().err().map(|e| e as &dyn std::fmt::Display);
            let report = host.rec.report(
                provider.kind(),
                provider.model(),
                reply,
                images,
                image_errors,
                err,
            );
            write_report(out, &report)?;
            result.map(|o| OnceOutcome { delta: o.delta })
        }
        OutputFormat::Text => {
            let outcome = result?;
            if !outcome.reply.is_empty() {
                writeln!(out, "{}", outcome.reply)?;
            }
            for path in &outcome.images {
                writeln!(out, "{IMAGE_SAVED_PREFIX}{path}")?;
            }
            for msg in &outcome.image_errors {
                writeln!(out, "{msg}")?;
            }
            Ok(OnceOutcome {
                delta: outcome.delta,
            })
        }
    }
}
