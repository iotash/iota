//! `/model` and the lazy model picker (chat/run.go:513-716, :1112-1157).
//!
//! The Model tab is a COMBO (`ui::facade::ListBody::combo`): a permanent input row over a list of
//! candidates. It replaced the old `manual` fork — list OR field, decided by whether one listing
//! succeeded — for a reason that is a property of the world and not of the code: a provider that
//! cannot list its models is usually a provider that never implements the endpoint (relays, most
//! of them), not one that had a bad minute. An input that appears only after something FAILED is
//! the wrong shape for that; the input is always there, and the list is completion for it
//! (brain page `config-three-layers`).
//!
//! The rows are the agent's candidate set — `models:` entries, inline `provider:id`s and every
//! `provider:*` wildcard, the wildcards asked CONCURRENTLY (`repl::catalog`). A source that fails
//! costs its own rows and nothing else: it contributes one line to the panel's prompt, and every
//! other source is listed as if it had never been asked.
//!
//! **The fetch-cancel idiom** (run.go:1112-1121) is the subtle part and now lives in the catalog:
//! the listings run under ONE cancel scope so ESC aborts all of them immediately instead of
//! leaving the user hostage to an HTTP timeout behind an unresponsive spinner — and the
//! cancellation state must be READ BEFORE the scope's own token fires, or the check would be
//! unconditionally true. A user-cancelled fetch abandons the picker silently.

use std::sync::Arc;

use crate::provider::Provider;
use crate::sync::lock;
use crate::ui::facade::{Panel, PanelResult, TabbedSpec, Ui};
use tokio_util::sync::CancellationToken;

use crate::repl::catalog::{Candidate, ModelCatalog, Pick};
use crate::repl::commands::settings::Extras;
use crate::repl::run::Repl;
use crate::repl::title::WriterSlot;

/// The combo field's placeholder (run.go:536 — it was the manual field's).
const MODEL_PLACEHOLDER: &str = "model name (e.g. gpt-4o)";

/// The Model tab's rows (chat/settings.go:104-131 `modelRows`): the candidate set, with the
/// CURRENT model marked in place — or PREPENDED when the candidates do not carry it (a delisted
/// model, a model reached with `-M` from outside the set), so submitting a tab the user never
/// visited changes nothing. An unset model becomes the `"(not selected)"` row, and committing it
/// is a no-op (the caller's `!= ""` guard).
///
/// `session` is the endpoint the chat is on; it decides how a row reads (`Candidate::label`).
///
/// Returns `(values, labels, cursor)`: `values` are what a commit reads, `labels` what the panel
/// shows.
pub(crate) fn model_rows(
    current: &Candidate,
    models: &[Candidate],
    session: &str,
) -> (Vec<Candidate>, Vec<String>, usize) {
    let mut values: Vec<Candidate> = models.to_vec();
    if !values.iter().any(|v| v == current) {
        values.insert(0, current.clone());
    }
    let mut cursor = 0;
    let labels = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let label = v.label(session);
            if v == current {
                cursor = i;
                if label.is_empty() {
                    return "(not selected)".to_owned();
                }
                return format!("{label} (current)");
            }
            label
        })
        .collect();
    (values, labels, cursor)
}

/// The Model tab: the panel to show and the candidates its cursor indexes.
struct ModelTab {
    /// One per row, parallel to the panel's items.
    values: Vec<Candidate>,
    /// The panel itself.
    panel: Panel,
    /// The user pressed ESC while the listings were in flight: abandon the command, quietly.
    cancelled: bool,
}

/// What a commit of the Model tab chose: a listed row, or the text the combo's field carries
/// (`cursor == values.len()` IS the `use "…" as typed` row — `ui::facade::PanelResult`).
/// `None` = the cursor was past the rows and nothing was typed.
fn chosen(values: &[Candidate], catalog: &ModelCatalog, r: &PanelResult) -> Option<Candidate> {
    if let Some(c) = values.get(r.cursor) {
        return Some(c.clone());
    }
    let typed = r.text.trim();
    (!typed.is_empty()).then(|| catalog.parse_typed(typed))
}

/// Builds the Model tab: the candidate set expanded (one concurrent round of listings), the rows
/// laid out around the current model, and whatever could not answer put in the prompt row.
async fn model_tab(
    ui: &Arc<dyn Ui>,
    catalog: &ModelCatalog,
    live: &dyn Provider,
    title: &str,
    current: Option<&Candidate>,
    cancel: &CancellationToken,
) -> ModelTab {
    let expanded = catalog.expand(ui, live, cancel).await;
    if expanded.cancelled {
        return ModelTab {
            values: Vec::new(),
            panel: Panel::default(),
            cancelled: true,
        };
    }
    let session = catalog.session_provider();
    // `/model` lays the rows out around the model the chat is running, with the cursor on it; the
    // startup pick has no current model to preserve, so the candidates stand alone.
    let (values, labels, cursor) = if let Some(current) = current {
        model_rows(current, &expanded.rows, session)
    } else {
        let labels = expanded.rows.iter().map(|c| c.label(session)).collect();
        (expanded.rows, labels, 0)
    };
    let mut panel = Panel::list(title.to_owned(), labels)
        .with_combo(MODEL_PLACEHOLDER)
        .with_cursor(cursor);
    // What could not answer goes in the prompt row — a subtitle, not a list row and not a line
    // printed over the transcript before the surface opens: a source that never implements
    // `list_models` is a fact about the list, not an event.
    if !expanded.notes.is_empty() {
        panel = panel.with_prompt(expanded.notes.join(" · "));
    }
    ModelTab {
        values,
        panel,
        cancelled: false,
    }
}

/// Commits a chosen model to the provider AND the session bundle: one call site, so the
/// live model and what a resumed session replays cannot drift.
fn commit(provider: &mut dyn Provider, writer: &WriterSlot, name: &str) {
    provider.set_model(name.to_owned());
    if let Some(w) = lock(writer).as_mut() {
        let _ = w.update_meta(|m| name.clone_into(&mut m.model));
    }
}

/// Reports a candidate this session cannot take: a chat stays on the endpoint it started on.
///
/// The history it replays is that dialect's own (signed thinking blocks, response items), the
/// bundle records the provider type it was created under, and the dispatcher was assembled for it
/// — so a provider move is a new run, and saying so is the same answer `/model` already gives for
/// a `defer_mode` it cannot hand over (`repl::liveparams::switch_model`).
fn report_elsewhere(repl: &Repl, catalog: &ModelCatalog, provider: &str, id: &str) {
    let agent = catalog.agent();
    let agent = if agent.is_empty() { "<agent>" } else { agent };
    repl.handles.tr.notice(&format!(
        "{provider}:{id} runs on another provider; a session keeps the endpoint it started on \
         (its history is that dialect's). Start one there: iota run {agent} -M {provider}:{id}"
    ));
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
    let ui = Arc::clone(&repl.handles.ui);
    let tab = model_tab(
        &ui,
        &repl.conv.catalog,
        &*repl.conv.provider,
        "Select a model",
        None,
        cancel,
    )
    .await;
    if tab.cancelled {
        return false;
    }
    let ModelTab { values, panel, .. } = tab;
    let spec = TabbedSpec {
        panels: vec![panel],
        ..TabbedSpec::default()
    };
    let chosen = match ui.tabbed(cancel, spec).await {
        Ok(r) if !r.cancelled => r
            .panels
            .first()
            .and_then(|p| chosen(&values, &repl.conv.catalog, p)),
        _ => None,
    };
    let Some(chosen) = chosen else { return false };
    let name = match repl.conv.catalog.pick(&chosen) {
        Pick::Here(id) => id,
        Pick::Elsewhere(provider, id) => {
            report_elsewhere(repl, &repl.conv.catalog, &provider, &id);
            return false;
        }
    };
    if name.is_empty() {
        return false;
    }
    commit(&mut *repl.conv.provider, &repl.session.writer, &name);
    repl.handles.tr.notice(&format!("Using model: {name}"));
    crate::repl::liveparams::switch_model(repl, &name);
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
    let cancel = &repl.handles.cancel.clone();
    // The read-only System tab shows the prompt AS SENT, so it needs the same overlay the
    // message path composes with (chat/run.go:624).
    let overlay = repl
        .conv
        .overlay
        .as_ref()
        .map_or_else(String::new, crate::agents::Overlay::content);
    let current = Candidate {
        provider: repl.conv.catalog.session_provider().to_owned(),
        id: repl.conv.provider.model().to_owned(),
    };
    let ui = Arc::clone(&repl.handles.ui);
    let tab = model_tab(
        &ui,
        &repl.conv.catalog,
        &*repl.conv.provider,
        "Model",
        Some(&current),
        cancel,
    )
    .await;
    if tab.cancelled {
        return;
    }
    // The Model tab is index 0; every capability tab records its own index as it lands.
    let ModelTab { values, panel, .. } = tab;
    let mut panels = vec![panel];
    let window = repl.conv.budget.window();
    let history = std::mem::take(&mut repl.conv.history);
    let extras = Extras::assemble(
        &mut *repl.conv.provider,
        window,
        &history,
        &overlay,
        &mut panels,
    );
    repl.conv.history = history;
    let Ok(r) = repl
        .handles
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
    let chosen = r
        .panels
        .first()
        .and_then(|p| chosen(&values, &repl.conv.catalog, p));
    // Picking the "(not selected)" row, or leaving the field empty, is a no-op.
    let mut changed = false;
    match chosen.as_ref().map(|c| repl.conv.catalog.pick(c)) {
        Some(Pick::Here(id)) if !id.is_empty() && id != current.id => {
            commit(&mut *repl.conv.provider, &repl.session.writer, &id);
            repl.handles.tr.notice(&format!("Model switched to {id}"));
            // The model decides the four layered parameters again: `agents:` → the NEW model's
            // `models:` entry → what the session is already running under, minus whatever the
            // model just left had declared (brain page `model-param-layering`). It runs BEFORE
            // the tabs are read back, because a knob the user moved in this same surface is the
            // intent they have just expressed and must win over the re-evaluation — which it
            // does by construction: every tab commits against the value it OPENED on.
            crate::repl::liveparams::switch_model(repl, &id);
            changed = true;
        }
        Some(Pick::Elsewhere(provider, id)) => {
            report_elsewhere(repl, &repl.conv.catalog, &provider, &id);
        }
        Some(Pick::Here(_)) | None => {}
    }
    changed |= extras.apply(&r, repl);
    if !changed {
        repl.handles.tr.notice("No changes.");
    }
}

#[cfg(test)]
mod tests {
    use super::model_rows;
    use crate::repl::catalog::Candidate;

    fn here(id: &str) -> Candidate {
        Candidate::here(id)
    }

    fn on(provider: &str, id: &str) -> Candidate {
        Candidate {
            provider: provider.to_owned(),
            id: id.to_owned(),
        }
    }

    // Go: chat/settings_test.go:52 TestModelRows — the untouched-tab-is-a-no-op law:
    // every row list carries the CURRENT value with the cursor on it.
    #[test]
    fn test_model_rows() {
        let models = vec![here("a-model"), here("b-model")];

        // Current model present in the list: marked in place, no growth.
        let (values, labels, cur) = model_rows(&here("b-model"), &models, "");
        assert_eq!(values.len(), 2);
        assert_eq!(values[cur], here("b-model"));
        assert_eq!(labels[cur], "b-model (current)");

        // Missing current (a delisted model, a different naming scheme): PREPENDED, so
        // submitting a tab the user never visited changes nothing.
        let (values, labels, cur) = model_rows(&here("models/c"), &models, "");
        assert_eq!(cur, 0);
        assert_eq!(values[0], here("models/c"));
        assert_eq!(labels[0], "models/c (current)");
        assert_eq!(values.len(), 3, "a missing current must grow the list");

        // No model selected yet: a "(not selected)" row representing "" is prepended;
        // committing it is a no-op (the caller's non-empty guard).
        let (values, labels, cur) = model_rows(&here(""), &models, "");
        assert_eq!(cur, 0);
        assert_eq!(values[0], here(""));
        assert_eq!(labels[0], "(not selected)");
    }

    /// A mixed list says where each row comes from: the session's own endpoint is implicit, every
    /// other one is written `provider:id` — which is also what makes the same id on two providers
    /// two distinguishable rows.
    #[test]
    fn rows_from_other_providers_carry_their_provider() {
        let models = vec![
            on("anthropic", "claude-x"),
            on("relay", "claude-x"),
            on("relay", "vendor/y"),
        ];
        let (values, labels, cur) = model_rows(&on("anthropic", "claude-x"), &models, "anthropic");
        assert_eq!(values.len(), 3, "the current model is already in the set");
        assert_eq!(cur, 0);
        assert_eq!(
            labels,
            ["claude-x (current)", "relay:claude-x", "relay:vendor/y"]
        );
    }
}
