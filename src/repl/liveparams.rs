//! The chat's half of the layered parameters (brain page `model-param-layering`): reading what the session
//! currently runs under, re-evaluating it when `/model` switches models, and writing the result to the
//! provider, the budget and the bundle.
//!
//! The VALUES are never kept here. `context_window` lives in [`ContextBudget`](crate::repl::context::meter::ContextBudget)
//! and the three tunables live on the provider, so this module assembles a [`LayeredParams`] on demand from
//! those two and the one piece of state that has nowhere else to live: [`Conversation::param_sources`](crate::repl::state::Conversation::param_sources), which says
//! where each of them came from. A second copy of the values would be a second thing to keep in step with a
//! provider every tab in `/model` can already write to.

use crate::provider::{Effort, Provider};
use crate::session::{ImageSettings, LayeredParams, Moved, Param, ParamSources, SessionMeta};
use crate::sync::lock;

use crate::repl::commands::settings::{effort_label, float_ptr_equal, format_temperature};
use crate::repl::context::tokens::DEFAULT_CONTEXT_WINDOW;
use crate::repl::run::Repl;

/// What the chat is running under right now, read from where each value actually lives.
pub(crate) fn current(repl: &mut Repl) -> LayeredParams {
    read(
        &mut *repl.conv.provider,
        repl.conv.budget.window(),
        repl.conv.param_sources,
    )
}

/// What `provider` runs under, with the budget's `window`, each value tagged with where it came from.
fn read(provider: &mut dyn Provider, window: u64, sources: ParamSources) -> LayeredParams {
    let (effort, temperature) = provider.as_tunable().map_or_else(
        || (String::new(), None),
        |t| {
            (
                t.effort().map_or("", Effort::as_str).to_owned(),
                t.temperature(),
            )
        },
    );
    let top_p = provider.as_top_p_tunable().and_then(|t| t.top_p());
    let max_tokens = provider
        .as_max_tokens_tunable()
        .and_then(|t| t.max_tokens());
    LayeredParams {
        context_window: Param {
            value: window,
            source: sources.context_window,
        },
        effort: Param {
            value: effort,
            source: sources.effort,
        },
        temperature: Param {
            value: temperature,
            source: sources.temperature,
        },
        top_p: Param {
            value: top_p,
            source: sources.top_p,
        },
        max_tokens: Param {
            value: max_tokens,
            source: sources.max_tokens.unwrap_or_default(),
        },
    }
}

/// Re-evaluates the five parameters for the model the chat has just switched to, and applies whatever moved.
///
/// This is the second (and last) evaluating moment: `agents:` → the NEW model's `models:` entry → the
/// session's current value, with a value that came from a declaration dropped rather than carried onto a
/// model that declares none. Returns whether anything changed, and prints one dim notice per knob that did.
///
/// A `context_window:` on the model being switched TO that does not parse is a warning here, not a failure:
/// the chat is already running, and the honest answer to a bad value is to leave that key silent and say so.
pub(crate) fn switch_model(repl: &mut Repl, id: &str) -> bool {
    let running = current(repl);
    let r: &Repl = repl;
    let next = reevaluate(r, &r.conv.layers, id, &running);
    apply(repl, &running, &next)
}

/// The five parameters re-evaluated for `id` against `layers`, from what the session is `running` under: the
/// evaluating half of [`switch_model`], warning about a declaration it cannot take and a `defer_mode` it cannot
/// hand over. A move to another endpoint evaluates against THAT endpoint's layers before anything is
/// installed, so what it writes into the bundle is what will land (`commands::model::move_to`).
pub(crate) fn reevaluate(
    repl: &Repl,
    layers: &crate::config::ParamLayers,
    id: &str,
    running: &LayeredParams,
) -> LayeredParams {
    let declared = layers.declared(id, |decl| {
        match crate::config::window::parse_window_size(decl.raw) {
            Ok(window) => Some(window),
            Err(e) => {
                repl.handles
                    .tr
                    .error(&format!("Warning: {}: {e} (ignored)", decl.label));
                None
            }
        }
    });
    // A declaration outside its range would be refused at startup; reached mid-chat it can only come from a
    // `models:` entry the run did not start on, so it is refused the same way and the key stays silent.
    let declared = crate::config::Declared {
        temperature: declared.temperature.filter(|t| {
            in_range(
                repl,
                *t,
                0.0,
                2.0,
                &format!("config temperature {}", crate::text::go_float(*t)),
            )
        }),
        top_p: declared.top_p.filter(|p| {
            in_range(
                repl,
                *p,
                0.0,
                1.0,
                &format!("config top_p {}", crate::text::go_float(*p)),
            )
        }),
        max_tokens: declared.max_tokens.filter(|n| {
            if *n > 0 {
                return true;
            }
            repl.handles
                .tr
                .error("Warning: config max_tokens 0: want 1 or more (ignored)");
            false
        }),
        ..declared
    };
    // The one thing a model declares that a switch canNOT hand over: the deferring wrapper was built around
    // the MCP dispatcher at startup and there is no rebuild seam, so the mode is announced instead of
    // silently ignored (brain page `config-three-layers`, 2026-09-09 — changing it means a new session).
    if let Some((kept, wanted)) = layers.defer_mode_drift(id) {
        repl.handles.tr.notice(&format!(
            "Note: {id} asks for defer_mode {:?}; this session keeps {:?} — deferred MCP tools are mounted once, at startup.",
            wanted.name(),
            kept.name(),
        ));
    }
    declared.evaluate(running)
}

/// Whether `v` is inside `[lo, hi]`, warning with `label` when it is not.
fn in_range(repl: &Repl, v: f64, lo: f64, hi: f64, label: &str) -> bool {
    if (lo..=hi).contains(&v) {
        return true;
    }
    repl.handles
        .tr
        .error(&format!("Warning: {label}: want {lo:.1}-{hi:.1} (ignored)"));
    false
}

/// Applies every parameter whose value moved from `now` — what the chat ran under before the switch — to the
/// provider, the budget and the bundle, one dim notice each, and adopts the source of each one it applied.
///
/// A parameter the provider cannot act on is left alone ENTIRELY — value, notice and source. The capability
/// accessor is the same gate the startup tuning uses (which warns once and moves on there); a chat must not
/// record a `top_p` for a dialect that has none, or a window for one that counts no tokens, because a resume
/// would hand it back as though the session had been running under it.
///
/// Every write carries the source map with it. A value and where it came from are one fact: writing the
/// value alone would leave a bundle saying a window the config supplied is the user's own, and the next
/// switch would then keep a window it was supposed to drop.
pub(crate) fn apply(repl: &mut Repl, now: &LayeredParams, next: &LayeredParams) -> bool {
    let counts_tokens = repl.conv.provider.reports_usage();
    let tunable = repl.conv.provider.as_tunable().is_some();
    let nucleus = repl.conv.provider.as_top_p_tunable().is_some();
    let ceiling = repl.conv.provider.as_max_tokens_tunable().is_some();
    let landed = landing(&mut *repl.conv.provider, now, next);
    let window = landed.context_window.value;
    let sources = landed.sources();
    let mut changed = false;

    if counts_tokens && window != now.context_window.value {
        repl.conv.budget.set_window(window);
        commit(repl, sources, |m| m.set_context_window(window));
        repl.handles
            .tr
            .notice(&format!("Context window: {}", repl.conv.budget.status()));
        changed = true;
    }

    if tunable {
        if landed.effort.value != now.effort.value
            && let Ok(effort) = Effort::optional(&landed.effort.value)
        {
            if let Some(t) = repl.conv.provider.as_tunable() {
                t.set_effort(effort);
            }
            let level = landed.effort.value.clone();
            commit(repl, sources, move |m| level.clone_into(&mut m.effort));
            repl.handles
                .tr
                .notice(&format!("Effort: {}", effort_label(&landed.effort.value)));
            changed = true;
        }
        if !float_ptr_equal(landed.temperature.value, now.temperature.value) {
            if let Some(t) = repl.conv.provider.as_tunable() {
                t.set_temperature(landed.temperature.value);
            }
            let temperature = landed.temperature.value;
            commit(repl, sources, move |m| m.temperature = temperature);
            repl.handles.tr.notice(&format!(
                "Temperature: {}",
                format_temperature(landed.temperature.value)
            ));
            changed = true;
        }
    }

    if nucleus && !float_ptr_equal(landed.top_p.value, now.top_p.value) {
        if let Some(t) = repl.conv.provider.as_top_p_tunable() {
            t.set_top_p(landed.top_p.value);
        }
        let top_p = landed.top_p.value;
        commit(repl, sources, move |m| m.top_p = top_p);
        repl.handles.tr.notice(&format!(
            "Top-p: {}",
            format_temperature(landed.top_p.value)
        ));
        changed = true;
    }

    if ceiling && landed.max_tokens.value != now.max_tokens.value {
        if let Some(t) = repl.conv.provider.as_max_tokens_tunable() {
            t.set_max_tokens(landed.max_tokens.value);
        }
        let max_tokens = landed.max_tokens.value;
        commit(repl, sources, move |m| m.max_tokens = max_tokens);
        repl.handles
            .tr
            .notice(&format!("Max tokens: {}", max_tokens_label(max_tokens)));
        changed = true;
    }

    // A source can move without its value doing so — a declaration that merely CONFIRMS what the session was
    // already running under still takes ownership of it, and the next switch must know that.
    if !changed && repl.conv.param_sources != sources {
        commit(repl, sources, |_| {});
    }
    repl.conv.param_sources = sources;
    changed
}

/// What the chat runs under once `next` is applied to `provider`, whose values are `now`: `next` for every
/// parameter the provider can act on, `now` for the rest (the gates of [`apply`]).
fn landing(
    provider: &mut dyn Provider,
    now: &LayeredParams,
    next: &LayeredParams,
) -> LayeredParams {
    let mut landed = now.clone();
    if provider.reports_usage() {
        // An evaluation that dropped a declared window leaves `0` behind, which is the SPELLING of "no window
        // configured"; the budget refuses a zero (it must never clobber a real window), so the built-in
        // default is named explicitly here.
        landed.context_window = Param {
            value: if next.context_window.value == 0 {
                DEFAULT_CONTEXT_WINDOW
            } else {
                next.context_window.value
            },
            source: next.context_window.source,
        };
    }
    if provider.as_tunable().is_some() {
        landed.effort = next.effort.clone();
        landed.temperature = next.temperature.clone();
    }
    if provider.as_top_p_tunable().is_some() {
        landed.top_p = next.top_p.clone();
    }
    if provider.as_max_tokens_tunable().is_some() {
        landed.max_tokens = next.max_tokens.clone();
    }
    landed
}

/// Puts `next` on `provider` — an instance about to run the session: a resume's, or the one a move installs —
/// as far as its dialect takes it, and returns what the session runs under there: `next` for every knob the
/// provider took, nothing for the rest. The window is not the provider's to take: it stays as evaluated, for
/// the budget and for the warning a provider that counts no tokens gives about one — with the built-in
/// default named where tokens are counted and the evaluation dropped a declared one ([`landing`]).
pub(crate) fn land(provider: &mut dyn Provider, next: &LayeredParams) -> LayeredParams {
    let mut landed = LayeredParams {
        context_window: next.context_window.clone(),
        ..LayeredParams::default()
    };
    if provider.reports_usage() && landed.context_window.value == 0 {
        landed.context_window.value = DEFAULT_CONTEXT_WINDOW;
    }
    if let Some(t) = provider.as_tunable() {
        t.set_effort(Effort::optional(&next.effort.value).ok().flatten());
        t.set_temperature(next.temperature.value);
        landed.effort = next.effort.clone();
        landed.temperature = next.temperature.clone();
    }
    if let Some(t) = provider.as_top_p_tunable() {
        t.set_top_p(next.top_p.value);
        landed.top_p = next.top_p.clone();
    }
    if let Some(t) = provider.as_max_tokens_tunable() {
        t.set_max_tokens(next.max_tokens.value);
        landed.max_tokens = next.max_tokens.clone();
    }
    landed
}

/// Where the session runs once `provider` — named `name` in the config — runs it under `params`: what
/// [`SessionWriter::move_to`](crate::session::SessionWriter::move_to) writes. The window recorded is the one
/// the budget holds (the built-in default when nothing declares one), and none for a provider with no token
/// accounting.
pub(crate) fn moved(name: &str, provider: &mut dyn Provider, params: LayeredParams) -> Moved {
    let window = provider
        .reports_usage()
        .then_some(match params.context_window.value {
            0 => DEFAULT_CONTEXT_WINDOW,
            w => w,
        });
    Moved {
        kind: provider.kind(),
        provider: name.to_owned(),
        model: provider.model().to_owned(),
        params,
        window,
        image: ImageSettings::of(provider),
    }
}

/// Where the chat runs right now ([`moved`]): what a bundle this run created is told when the chat starts,
/// and what `/save` mints one with — so a resume finds the session as it was rather than re-deriving it from
/// a config that may have moved since. The values are read back from the provider and the budget
/// ([`current`]), so a knob the dialect cannot act on is never recorded as though it had applied.
pub(crate) fn moved_now(repl: &mut Repl) -> Moved {
    let params = current(repl);
    let name = repl.conv.catalog.session_provider().to_owned();
    moved(&name, &mut *repl.conv.provider, params)
}

/// How a `Max tokens:` notice names a ceiling: the number, or `default (32000)` for the dialect's own.
fn max_tokens_label(n: Option<u32>) -> String {
    n.map_or_else(
        || format!("default ({})", crate::llm::anthropic::DEFAULT_MAX_TOKENS),
        |n| n.to_string(),
    )
}

/// ONE knob's write: its value and the source map it landed with, together (see [`apply`]).
fn commit(repl: &Repl, sources: ParamSources, value: impl FnOnce(&mut SessionMeta)) {
    update_meta(repl, |m| {
        value(m);
        m.param_sources = Some(sources);
    });
}

/// The ONE session-bundle write of this module (the questionnaire's twin in `commands::settings`).
fn update_meta(repl: &Repl, f: impl FnOnce(&mut SessionMeta)) {
    if let Some(w) = lock(&repl.session.writer).as_mut() {
        let _ = w.update_meta(f);
    }
}
