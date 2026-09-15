//! `/save` — turning an ephemeral chat into a persisted one (chat/run.go:821-848).
//!
//! The command exists ONLY when the chat started without a bundle (`--no-save`), which is
//! the same flag its table row hangs off: on a persisted session `/save` is not a command
//! at all and the text is sent to the model.
//!
//! Minting is late but complete: the bundle records the LIVE tuning (model, temperature,
//! context window, effort — Go reads them off the provider inside the factory), and the whole
//! backlog lands in ONE append because the persist watermark stayed at zero while nothing
//! was saving. An argument settles a user-chosen title that no model pass and no rollback
//! may overwrite; without one the freshly minted bundle catches up with the name the
//! window has been showing since the first send.

use std::sync::PoisonError;

use crate::repl::run::Repl;
use crate::repl::title::{TITLE_CAP, title_from};

/// `/save [title]`.
pub(crate) fn cmd_save(repl: &mut Repl, arg: &str) {
    if repl
        .writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
    {
        repl.tr
            .notice(&format!("Session already saving ({}).", repl.session_id()));
        return;
    }
    let Some(factory) = repl.new_session.as_mut() else {
        return; // unreachable: the command is unregistered without a factory
    };
    let writer = match factory() {
        Ok(w) => w,
        Err(e) => {
            repl.tr.error(&format!("Save failed: {e}"));
            return;
        }
    };
    let window = repl.budget.window();
    // Go's factory takes the LIVE provider (root.go:360-366), so a mid-chat `/model` or a
    // temperature change lands in the freshly minted meta. The Rust factory is minted at
    // wiring time and cannot see the provider any more, so the live tuning is stamped
    // HERE instead: model, plus the four layered parameters and the source of each — this
    // is the same stamp a session that started with a bundle got on the way in, only later
    // (brain page `model-param-layering`).
    let model = repl.provider.model().to_owned();
    let params = crate::repl::liveparams::current(repl);
    {
        let mut slot = repl.writer.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(writer);
        if let Some(w) = slot.as_mut() {
            let _ = w.update_meta(|m| {
                m.model = model;
                crate::repl::liveparams::stamp(m, Some(window), &params);
            });
        }
    }
    // The watermark stayed at 0 while the chat was ephemeral, so this lands the WHOLE
    // conversation in one append.
    repl.persist_turn();
    let name = title_from(arg, TITLE_CAP);
    if name.is_empty() {
        // The chat was named at first send even without a writer; the freshly minted
        // bundle catches up with that name.
        repl.titler.reapply();
    } else {
        repl.titler.adopt_name(&name);
    }
    repl.tr.notice(&format!(
        "Session saved: {} — auto-saving from now on.",
        repl.session_id()
    ));
}
