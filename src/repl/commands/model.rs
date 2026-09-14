//! `/model` and the lazy model picker (chat/run.go:513-716, :1112-1157).
//!
//! T1 ships the Model tab alone: the fetched list as a searchable picker, or a manual
//! name field when the provider cannot list. The capability-assembled tabs (Context,
//! Effort, Temperature, Image, System) are WP54's (T-12), and the recorded-index commit
//! shape below is the one they extend.
//!
//! **The fetch-cancel idiom** (run.go:1112-1121) is the subtle part: the listing runs
//! under its OWN cancel scope so ESC aborts it immediately instead of leaving the user
//! hostage to an HTTP timeout behind an unresponsive spinner — and the cancellation state
//! must be READ BEFORE the scope's own token fires, or the check would be
//! unconditionally true. A user-cancelled fetch abandons the picker silently.

use std::sync::Arc;

use crate::provider::Provider;
use crate::ui::facade::{Panel, SelectSpec, TabbedSpec, Ui};
use tokio_util::sync::CancellationToken;

use crate::repl::commands::settings::Extras;
use crate::repl::run::Repl;
use crate::repl::title::WriterSlot;

/// The manual field's placeholder and width (run.go:536).
const MODEL_PLACEHOLDER: &str = "model name (e.g. gpt-4o)";
const MODEL_INPUT_WIDTH: usize = 40;

/// The Model tab's rows (chat/settings.go:104-131 `modelRows`): the fetched models, with
/// the CURRENT one marked in place — or PREPENDED when the listing does not carry it (a
/// delisted model, a different naming scheme), so submitting a tab the user never visited
/// changes nothing. An unset model becomes the `"(not selected)"` row, and committing it
/// is a no-op (the caller's `!= ""` guard).
///
/// Returns `(values, labels, cursor)`: `values` are what a commit reads, `labels` what the
/// panel shows.
pub(crate) fn model_rows(current: &str, models: &[String]) -> (Vec<String>, Vec<String>, usize) {
    let mut values: Vec<String> = models.to_vec();
    if !values.iter().any(|v| v == current) {
        values.insert(0, current.to_owned());
    }
    let mut cursor = 0;
    let labels = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if v == current {
                cursor = i;
            }
            if v.is_empty() {
                "(not selected)".to_owned()
            } else if v == current {
                format!("{v} (current)")
            } else {
                v.clone()
            }
        })
        .collect();
    (values, labels, cursor)
}

/// What a model listing produced.
struct Fetched {
    models: Vec<String>,
    /// The listing's own failure, when it had one.
    error: Option<String>,
    /// The user pressed ESC on the fetch: abandon the whole command, quietly.
    cancelled: bool,
}

/// Lists the provider's models under a dedicated cancel scope and a busy row
/// (run.go:1112-1121).
async fn fetch_models(
    ui: &Arc<dyn Ui>,
    provider: &dyn Provider,
    parent: &CancellationToken,
) -> Fetched {
    let child = parent.child_token();
    let scope = ui.push_cancel_scope(child.clone());
    let busy = ui.busy("Fetching available models");
    let res = provider.list_models(&child).await;
    busy.stop();
    scope.pop();
    // READ the cancellation state before firing our own token: afterwards `child` is
    // cancelled either way. An app-wide shutdown is not a user cancellation.
    let cancelled = child.is_cancelled() && !parent.is_cancelled();
    child.cancel();
    match res {
        Ok(models) => Fetched {
            models,
            error: None,
            cancelled,
        },
        Err(e) => Fetched {
            models: Vec::new(),
            error: Some(e.to_string()),
            cancelled,
        },
    }
}

/// The manual model-name field (run.go:1133-1141): one Input tab, so a provider without a
/// listing endpoint is still usable.
fn manual_panel(text: &str) -> Panel {
    Panel::input(
        "Model".to_owned(),
        text.to_owned(),
        MODEL_PLACEHOLDER.to_owned(),
    )
    .with_input_width(MODEL_INPUT_WIDTH)
}

/// Commits a chosen model to the provider AND the session bundle: one call site, so the
/// live model and what a resumed session replays cannot drift.
fn commit(provider: &mut dyn Provider, writer: &WriterSlot, name: &str) {
    provider.set_model(name.to_owned());
    if let Some(w) = writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
    {
        let _ = w.update_meta(|m| name.clone_into(&mut m.model));
    }
}

/// The lazy model picker (chat/run.go:1112-1157 `ensureModel`).
///
/// Called at startup when no model is configured — ESC defers, and the first message
/// re-prompts — and again before any send that still has none. `false` means the user
/// cancelled or entered nothing: the caller continues the loop WITHOUT sending.
///
/// Choosing the first model IS a model switch, so the four layered parameters are evaluated
/// against the `models:` entry the chosen id names: a run that started on a `provider:*`
/// wildcard had no entry to read them from until now (brain page `model-param-layering`).
pub(crate) async fn ensure_model(repl: &mut Repl, cancel: &CancellationToken) -> bool {
    let ui = Arc::clone(&repl.ui);
    let fetched = fetch_models(&ui, &*repl.provider, cancel).await;
    if fetched.cancelled {
        return false;
    }
    let name = if fetched.error.is_some() || fetched.models.is_empty() {
        if let Some(e) = &fetched.error {
            repl.tr.error(&format!("Fetching models failed: {e}"));
        }
        let spec = TabbedSpec {
            panels: vec![manual_panel("")],
            ..TabbedSpec::default()
        };
        match ui.tabbed(cancel, spec).await {
            Ok(r) if !r.cancelled => r.panels.first().map(|p| p.text.trim().to_owned()),
            _ => None,
        }
    } else {
        let spec = SelectSpec {
            title: "Select a model".to_owned(),
            items: fetched.models.clone(),
            cursor: 0,
        };
        match ui.select(cancel, spec).await {
            Ok(r) if !r.cancelled => fetched.models.get(r.index).cloned(),
            _ => None,
        }
    };
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return false;
    };
    commit(&mut *repl.provider, &repl.writer, &name);
    repl.tr.notice(&format!("Using model: {name}"));
    crate::repl::params::switch_model(repl, &name);
    true
}

/// The `/model` command (chat/run.go:513-716).
///
/// The Model tab, the recorded-index commit and the per-delta notice are this file's; the
/// capability-assembled tabs beside it (Context / Effort / Temperature / Image /
/// Aspect / Size / Negative / JSON edits / System) are `commands::settings`' — WP54 grew
/// the questionnaire around this code rather than through it, which is why `Extras` both
/// appends the panels and reads them back.
///
/// A facade failure is treated exactly like a cancel (Go's `if serr != nil || r.Cancelled
/// { continue }`): commands never end the loop — a closed facade ends it at the next
/// `read_input`, which is the ONE exit path.
pub(crate) async fn cmd_model(repl: &mut Repl) {
    let cancel = &repl.cancel.clone();
    // The read-only System tab shows the prompt AS SENT, so it needs the same overlay the
    // message path composes with (chat/run.go:624).
    let overlay = repl
        .overlay
        .as_ref()
        .map_or_else(String::new, crate::agents::Overlay::content);
    let fetched = fetch_models(&repl.ui, &*repl.provider, cancel).await;
    if fetched.cancelled {
        return;
    }
    let manual = fetched.error.is_some() || fetched.models.is_empty();
    if let Some(e) = &fetched.error {
        repl.tr.error(&format!(
            "Fetching models failed: {e} — enter a model name manually."
        ));
    }
    let current = repl.provider.model().to_owned();
    let (values, panel) = if manual {
        (Vec::new(), manual_panel(&current))
    } else {
        let (values, labels, cursor) = model_rows(&current, &fetched.models);
        (
            values,
            Panel::list("Model".to_owned(), labels)
                .with_search(true)
                .with_cursor(cursor),
        )
    };
    // The Model tab is index 0; every capability tab records its own index as it lands.
    let mut panels = vec![panel];
    let window = repl.budget.window();
    let history = std::mem::take(&mut repl.history);
    let extras = Extras::assemble(&mut *repl.provider, window, &history, &overlay, &mut panels);
    repl.history = history;
    let Ok(r) = repl
        .ui
        .tabbed(
            cancel,
            TabbedSpec {
                panels,
                ..TabbedSpec::default()
            },
        )
        .await
    else {
        return;
    };
    if r.cancelled {
        return;
    }
    let chosen = r.panels.first().map_or_else(String::new, |p| {
        if manual {
            p.text.trim().to_owned()
        } else {
            values.get(p.cursor).cloned().unwrap_or_default()
        }
    });
    // Picking the "(not selected)" row, or leaving the field empty, is a no-op.
    let mut changed = false;
    if !chosen.is_empty() && chosen != current {
        commit(&mut *repl.provider, &repl.writer, &chosen);
        repl.tr.notice(&format!("Model switched to {chosen}"));
        // The model decides the four layered parameters again: `agents:` → the NEW model's
        // `models:` entry → what the session is already running under, minus whatever the
        // model just left had declared (brain page `model-param-layering`). It runs BEFORE
        // the tabs are read back, because a knob the user moved in this same surface is the
        // intent they have just expressed and must win over the re-evaluation — which it
        // does by construction: every tab commits against the value it OPENED on.
        crate::repl::params::switch_model(repl, &chosen);
        changed = true;
    }
    changed |= extras.apply(&r, repl);
    if !changed {
        repl.tr.notice("No changes.");
    }
}

#[cfg(test)]
mod tests {
    use super::model_rows;

    // Go: chat/settings_test.go:52 TestModelRows — the untouched-tab-is-a-no-op law:
    // every row list carries the CURRENT value with the cursor on it.
    #[test]
    fn test_model_rows() {
        let models = vec!["a-model".to_owned(), "b-model".to_owned()];

        // Current model present in the list: marked in place, no growth.
        let (values, labels, cur) = model_rows("b-model", &models);
        assert_eq!(values.len(), 2);
        assert_eq!(values[cur], "b-model");
        assert_eq!(labels[cur], "b-model (current)");

        // Missing current (a delisted model, a different naming scheme): PREPENDED, so
        // submitting a tab the user never visited changes nothing.
        let (values, labels, cur) = model_rows("models/c", &models);
        assert_eq!(cur, 0);
        assert_eq!(values[0], "models/c");
        assert_eq!(labels[0], "models/c (current)");
        assert_eq!(values.len(), 3, "a missing current must grow the list");

        // No model selected yet: a "(not selected)" row representing "" is prepended;
        // committing it is a no-op (the caller's non-empty guard).
        let (values, labels, cur) = model_rows("", &models);
        assert_eq!(cur, 0);
        assert_eq!(values[0], "");
        assert_eq!(labels[0], "(not selected)");
    }
}
