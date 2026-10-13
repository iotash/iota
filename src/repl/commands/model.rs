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
//! The rows are the agent's choices — `models:` entries, inline `provider:id`s and every
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

/// The Model tab's rows (chat/settings.go:104-131 `modelRows`): the choices, with the
/// CURRENT model marked in place — or PREPENDED when the candidates do not carry it (a delisted
/// model, a model reached with `-M` from outside the set), so submitting a tab the user never
/// visited changes nothing. An unset model becomes the `"(not selected)"` row, and committing it
/// is a no-op (the caller's `!= ""` guard).
///
/// `label` is how a row reads (`ModelCatalog::row_label`); this adds the `(current)` mark.
///
/// Returns `(values, labels, cursor)`: `values` are what a commit reads, `labels` what the panel
/// shows.
pub(crate) fn model_rows(
    current: &Candidate,
    models: &[Candidate],
    label: impl Fn(&Candidate) -> String,
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
            let label = label(v);
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
    if typed.is_empty() {
        return None;
    }
    catalog.parse_typed(typed)
}

/// Builds the Model tab: the choices expanded (one concurrent round of listings), the rows
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
    let label = |c: &Candidate| catalog.row_label(c);
    // `/model` lays the rows out around the model the chat is running, with the cursor on it; the
    // startup pick has no current model to preserve, so the candidates stand alone.
    let (values, labels, cursor) = if let Some(current) = current {
        model_rows(current, &expanded.rows, label)
    } else {
        let labels = expanded.rows.iter().map(label).collect();
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

/// Reports a candidate this chat cannot move to — `why` says what keeps it, `next` what the user can run instead.
fn refuse(repl: &Repl, provider: &str, id: &str, why: &str, next: &str) {
    repl.handles.tr.notice(&format!(
        "{provider}:{id} runs on another provider; {why}. Nothing was switched. {next}"
    ));
}

/// What a `/model` pick the chat cannot take leaves the user: THIS chat, continued there in a new run, when it
/// has a bundle to resume — `iota resume <id> -M` moves it — or a NEW session there when it has none (an
/// ephemeral chat, one not on disk yet) or is a bot's, which only its config moves.
fn way_out(repl: &Repl, provider: &str, id: &str) -> String {
    let session = lock(&repl.session.writer)
        .as_ref()
        .filter(|w| w.on_disk() && repl.conv.bot.is_none())
        .map(|w| w.id().to_owned());
    if let Some(session) = session {
        return format!(
            "This chat continues there in a new run: iota resume {session} -M {provider}:{id}"
        );
    }
    let agent = repl.conv.catalog.agent();
    let agent = if agent.is_empty() { "<agent>" } else { agent };
    format!("A NEW session starts there with: iota run {agent} -M {provider}:{id}")
}

/// Moves the chat to `provider:id` — another endpoint — and returns what the chat ran under before and the
/// parameters it runs under there, for the caller's [`liveparams::apply`](crate::repl::liveparams::apply) to
/// announce and record. `None` (reported, nothing changed) when it
/// does not: the chat never moves (a bot), the endpoint cannot be built or is another kind of model
/// ([`open_endpoint`]), a chat a model has replied in cannot save what it holds first ([`saved`]), the user
/// declines ([`consent`]), or the bundle could not be told.
///
/// A chat a model has replied in moves as `iota resume <id> -M provider:id` moves it, in this process: what
/// it holds is saved first, under the type it was said in; the log is read again for the target's type; and
/// the user agrees to what that costs before anything is written ([`consent`]).
///
/// The move is ONE state write, then the swap. Everything is computed on the target before anything is
/// installed: the knobs the chat runs under are re-evaluated against the target's layers and land on the new
/// instance — the third evaluating moment, beside a new session and a `/model` switch (brain page
/// `model-param-layering`) — and the bundle is told the endpoint, the model, the four parameters with their
/// sources and the image settings in one write ([`SessionWriter::move_to`](crate::session::SessionWriter::move_to)).
/// Only when that write succeeded is the endpoint installed, with the history as the log reads for it. What
/// the chat holds beside the history stays: the dispatcher, the tokenizer and the persist watermark.
pub(crate) async fn move_to(
    repl: &mut Repl,
    provider: &str,
    id: &str,
) -> Option<(crate::session::LayeredParams, crate::session::LayeredParams)> {
    let replied = repl.replied();
    if replied && !saved(repl, provider, id) {
        return None;
    }
    let next = way_out(repl, provider, id);
    let mut ep = open_endpoint(repl, provider, id, &next)?;
    let history = if replied {
        Some(consent(repl, ep.provider.kind(), provider, id).await?)
    } else {
        None
    };
    let running = crate::repl::liveparams::current(repl);
    let next = crate::repl::liveparams::reevaluate(repl, &ep.layers, id, &running);
    let landed = crate::repl::liveparams::land(&mut *ep.provider, &next);
    let moved = crate::repl::liveparams::moved(provider, &mut *ep.provider, landed);
    if let Some(w) = lock(&repl.session.writer).as_mut()
        && let Err(e) = w.move_to(&moved)
    {
        repl.handles
            .tr
            .error(&format!("Error: the session could not be moved: {e}"));
        return None;
    }
    install(repl, ep);
    if let Some(history) = history {
        repl.conv.history = history;
        repl.conv.budget.reseed(&repl.conv.history);
    }
    Some((running, next))
}

/// Whether everything a chat a model has replied in holds is in its log, saved under the type it was said in
/// — reported when not. A reply saved after the writer's type changed would lose its payload: the text would
/// be kept, the signed thinking or the reasoning items not. An ephemeral chat has no log to save it to.
fn saved(repl: &mut Repl, provider: &str, id: &str) -> bool {
    if lock(&repl.session.writer).is_none() {
        refuse(
            repl,
            provider,
            id,
            "this chat is not saved, and saved after the move its replies would lose their payload",
            "Save it with /save, then pick the model again.",
        );
        return false;
    }
    if !repl.persist_turn() {
        refuse(
            repl,
            provider,
            id,
            "what this chat holds could not be saved first, and saved after the move its replies would lose their payload",
            "Pick the model again once the session saves.",
        );
        return false;
    }
    true
}

/// The history a chat a model has replied in takes to `provider:id`, of type `kind` — once the user agreed.
/// `None`: reported, or declined.
///
/// It is the log read again for `kind`, as `iota resume -M` reads it. The view this process holds was loaded
/// for the type the chat is on, so a reply the target's own dialect made in an earlier run lost its payload
/// on the way in: the log still has it, and the reload brings it back ([`reloaded`]). The reload also says
/// which replies' payload the target does not replay — the first of the three things the user is told before
/// agreeing, beside where `iota resume` goes afterwards and the way back.
async fn consent(
    repl: &mut Repl,
    kind: crate::provider::ProviderKind,
    provider: &str,
    id: &str,
) -> Option<Vec<crate::provider::model::Message>> {
    let session = repl.session.session_id();
    let loaded = match repl.session.store.load(&session, kind) {
        Ok(loaded) => loaded,
        Err(e) => {
            repl.handles
                .tr
                .error(&format!("Error: the session could not be read again: {e}"));
            return None;
        }
    };
    let lost = loaded.foreign_notice(kind).unwrap_or_else(|| {
        format!("No earlier reply carries payload that {kind} does not replay.")
    });
    let Some(history) = reloaded(&repl.conv.history, loaded.messages) else {
        repl.handles.tr.error(
            "Error: the session's log does not hold this chat as it stands; nothing was switched.",
        );
        return None;
    };
    let here = Candidate {
        provider: repl.conv.catalog.session_provider().to_owned(),
        id: repl.conv.provider.model().to_owned(),
    }
    .label("");
    let tr = &repl.handles.tr;
    tr.notice(&lost);
    tr.notice(&format!(
        "Once moved, iota resume {session} continues this chat on {provider}:{id}."
    ));
    tr.notice(&format!(
        "{here} stays reachable: picking it again moves the chat back, and the replies made there replay their payload again."
    ));
    let ui = Arc::clone(&repl.handles.ui);
    let agreed = ui
        .confirm(
            &repl.handles.cancel,
            &format!("Move this chat to {provider}:{id}?"),
            "Move it",
            &format!("Stay on {here}"),
        )
        .await
        .unwrap_or(false);
    agreed.then_some(history)
}

/// The chat's `history` with every message its log holds replaced by the log's own, as `view` — the log read
/// for another type — has it. A frozen-mode tools mount, which the log never holds, stays where it is. `None`
/// when the two do not line up message for message.
fn reloaded(
    history: &[crate::provider::model::Message],
    view: Vec<crate::provider::model::Message>,
) -> Option<Vec<crate::provider::model::Message>> {
    if history.iter().filter(|m| !m.is_tools_mount()).count() != view.len() {
        return None;
    }
    let mut view = view.into_iter();
    history
        .iter()
        .map(|m| {
            if m.is_tools_mount() {
                return Some(m.clone());
            }
            view.next().filter(|v| v.role() == m.role())
        })
        .collect()
}

/// The endpoint `provider:id` names, ready to take the chat's place — or `None`, reported, with nothing
/// changed: the chat never moves, the endpoint cannot be built (an unknown provider, a missing key), or it is
/// another KIND of model — a refusal ends with `next`, what the user can run instead. What a run settles at startup from its provider's kind — the token meter and
/// `/compact`, the image commands and the retry policy — is not rebuilt, so a move keeps it; nor is the
/// dispatcher, so the target's layers keep the `defer_mode` it was assembled with.
pub(crate) fn open_endpoint(
    repl: &mut Repl,
    provider: &str,
    id: &str,
    next: &str,
) -> Option<crate::repl::Endpoint> {
    let Some(factory) = repl.conv.endpoints.as_ref() else {
        refuse(
            repl,
            provider,
            id,
            "this chat keeps the endpoint it started on",
            next,
        );
        return None;
    };
    let tr = Arc::clone(&repl.handles.tr);
    let mut ep = match factory(provider, id, &mut |w| tr.notice(&w)) {
        Ok(ep) => ep,
        Err(e) => {
            tr.error(&format!("Error: {e}"));
            return None;
        }
    };
    if ep.provider.reports_usage() != repl.conv.provider.reports_usage()
        || ep.provider.as_image_gen_tunable().is_some() != repl.conv.image_provider
    {
        refuse(
            repl,
            provider,
            id,
            "it is another kind of model (token accounting, image generation) than this chat was started for",
            next,
        );
        return None;
    }
    ep.layers = ep.layers.keeping_defer_mode(&repl.conv.layers);
    Some(ep)
}

/// Puts `ep` in the place of the endpoint the chat is on: the provider, the title twin, the declarations and
/// the picker. A title pass still in flight is given up — it runs on the instance being replaced.
pub(crate) fn install(repl: &mut Repl, ep: crate::repl::Endpoint) {
    repl.session.abort_title();
    repl.session.title_provider = ep
        .title_provider
        .map(|p| Arc::new(tokio::sync::Mutex::new(p)));
    repl.conv.layers = ep.layers;
    repl.conv.catalog = ep.catalog;
    repl.conv.provider = ep.provider;
}

/// The lazy model picker (chat/run.go:1112-1157 `ensureModel`).
///
/// Called at startup when no model is configured (`agents.<name>.model` unset, or `-M provider:*`)
/// — ESC defers, and the first message re-prompts — and again before any send that still has
/// none. `false` means the user cancelled or entered nothing: the caller continues the loop
/// WITHOUT sending.
///
/// Choosing the first model IS a model switch, so the four layered parameters are evaluated
/// against the `models:` entry the chosen id names: a run that started with no model had no
/// entry to read them from until now (brain page `model-param-layering`).
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
            let Some((from, next)) = move_to(repl, &provider, &id).await else {
                return false;
            };
            repl.handles
                .tr
                .notice(&format!("Using model: {id} on {provider}"));
            crate::repl::liveparams::apply(repl, &from, &next);
            return true;
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
    // The read-only System tab shows the prompt AS SENT, so it needs the same harness and
    // overlay the message path composes with (chat/run.go:624).
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
    // A bot is not offered a window it would fail in (bot-mode.md §4.1).
    let floor = if repl.conv.bot.is_some() {
        crate::repl::context::tokens::BOT_MIN_WINDOW
    } else {
        0
    };
    let extras = Extras::assemble(
        &mut *repl.conv.provider,
        window,
        floor,
        &history,
        &repl.conv.harness,
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
            if let Some((from, next)) = move_to(repl, &provider, &id).await {
                repl.handles
                    .tr
                    .notice(&format!("Model switched to {id} on {provider}"));
                crate::repl::liveparams::apply(repl, &from, &next);
                changed = true;
            }
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
    use super::{model_rows, reloaded};
    use crate::provider::model::{Message, Raw, RawContent, ToolDef};
    use crate::repl::catalog::Candidate;

    fn here(id: &str) -> Candidate {
        Candidate::here(id)
    }

    /// The name of a row on the session's own endpoint (`Candidate::here`).
    fn bare(c: &Candidate) -> String {
        c.label("")
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
        let (values, labels, cur) = model_rows(&here("b-model"), &models, bare);
        assert_eq!(values.len(), 2);
        assert_eq!(values[cur], here("b-model"));
        assert_eq!(labels[cur], "b-model (current)");

        // Missing current (a delisted model, a different naming scheme): PREPENDED, so
        // submitting a tab the user never visited changes nothing.
        let (values, labels, cur) = model_rows(&here("models/c"), &models, bare);
        assert_eq!(cur, 0);
        assert_eq!(values[0], here("models/c"));
        assert_eq!(labels[0], "models/c (current)");
        assert_eq!(values.len(), 3, "a missing current must grow the list");

        // No model selected yet: a "(not selected)" row representing "" is prepended;
        // committing it is a no-op (the caller's non-empty guard).
        let (values, labels, cur) = model_rows(&here(""), &models, bare);
        assert_eq!(cur, 0);
        assert_eq!(values[0], here(""));
        assert_eq!(labels[0], "(not selected)");
    }

    /// A mixed list says where each row comes from — every row, the current one included — which is also
    /// what makes the same id on two providers two distinguishable rows. The current row is found by its PAIR,
    /// whatever its label says.
    #[test]
    fn rows_from_other_providers_carry_their_provider() {
        let models = vec![
            on("anthropic", "claude-x"),
            on("relay", "claude-x"),
            on("relay", "vendor/y"),
        ];
        // `""`: the set spans two endpoints, so no provider is implicit (`ModelCatalog::implicit_provider`).
        let (values, labels, cur) =
            model_rows(&on("anthropic", "claude-x"), &models, |c| c.label(""));
        assert_eq!(values.len(), 3, "the current model is already in the set");
        assert_eq!(cur, 0);
        assert_eq!(
            labels,
            [
                "anthropic:claude-x (current)",
                "relay:claude-x",
                "relay:vendor/y"
            ]
        );
    }

    /// A move takes the log's copy of every message the log holds — read for the target's type, so with the
    /// payload the chat's own view had lost — and keeps a frozen-mode tools mount, which the log never holds,
    /// where it was. A view that does not line up with the chat is not taken.
    #[test]
    fn a_reload_keeps_the_mounts_and_takes_the_logs_copy() {
        let payload = RawContent::OpenResponses(vec![Raw::from_string("{}".to_owned()).unwrap()]);
        let mount = Message::system_tools(vec![ToolDef {
            name: "late".to_owned(),
            ..ToolDef::default()
        }]);
        let history = vec![Message::user("q"), mount.clone(), Message::assistant("a")];
        let restored = Message::assistant_with_calls("a", Vec::new(), Some(payload));
        let view = vec![Message::user("q"), restored.clone()];
        assert_eq!(
            reloaded(&history, view),
            Some(vec![Message::user("q"), mount, restored])
        );
        assert_eq!(reloaded(&history, vec![Message::user("q")]), None);
        assert_eq!(
            reloaded(&history, vec![Message::user("q"), Message::user("a")]),
            None
        );
    }
}
