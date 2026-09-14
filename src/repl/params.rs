//! The chat's half of the layered parameters (brain page `model-param-layering`): reading what the session
//! currently runs under, re-evaluating it when `/model` switches models, and writing the result to the
//! provider, the budget and the bundle.
//!
//! The VALUES are never kept here. `context_window` lives in [`ContextBudget`](crate::repl::meter::ContextBudget)
//! and the three tunables live on the provider, so this module assembles a [`LayeredParams`] on demand from
//! those two and the one piece of state that has nowhere else to live: [`Repl::param_sources`], which says
//! where each of them came from. A second copy of the values would be a second thing to keep in step with a
//! provider every tab in `/model` can already write to.

use std::sync::PoisonError;

use crate::provider::Effort;
use crate::session::{LayeredParams, Param, ParamSources, SessionMeta};

use crate::repl::commands::settings::{effort_label, float_ptr_equal, format_temperature};
use crate::repl::run::Repl;
use crate::repl::tokens::DEFAULT_CONTEXT_WINDOW;

/// What the chat is running under right now, read from where each value actually lives.
pub(crate) fn current(repl: &mut Repl) -> LayeredParams {
    let sources = repl.param_sources;
    let window = repl.budget.window();
    let (effort, temperature) = repl.provider.as_tunable().map_or_else(
        || (String::new(), None),
        |t| {
            (
                t.effort().map_or("", Effort::as_str).to_owned(),
                t.temperature(),
            )
        },
    );
    let top_p = repl.provider.as_top_p_tunable().and_then(|t| t.top_p());
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
    }
}

/// Re-evaluates the four parameters for the model the chat has just switched to, and applies whatever moved.
///
/// This is the second (and last) evaluating moment: `agents:` → the NEW model's `models:` entry → the
/// session's current value, with a value that came from a declaration dropped rather than carried onto a
/// model that declares none. Returns whether anything changed, and prints one dim notice per knob that did.
///
/// A `context_window:` on the model being switched TO that does not parse is a warning here, not a failure:
/// the chat is already running, and the honest answer to a bad value is to leave that key silent and say so.
pub(crate) fn switch_model(repl: &mut Repl, id: &str) -> bool {
    let declared = repl.layers.declared(id, |decl| {
        match crate::cmd::window::parse_window_size(decl.raw) {
            Ok(window) => Some(window),
            Err(e) => {
                repl.tr
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
        ..declared
    };
    // The one thing a model declares that a switch canNOT hand over: the deferring wrapper was built around
    // the MCP dispatcher at startup and there is no rebuild seam, so the mode is announced instead of
    // silently ignored (brain page `config-three-layers`, 2026-09-09 — changing it means a new session).
    if let Some((running, wanted)) = repl.layers.defer_mode_drift(id) {
        repl.tr.notice(&format!(
            "Note: {id} asks for defer_mode {:?}; this session keeps {:?} — deferred MCP tools are mounted once, at startup.",
            wanted.name(),
            running.name(),
        ));
    }
    let next = declared.evaluate(&current(repl));
    apply(repl, &next)
}

/// Whether `v` is inside `[lo, hi]`, warning with `label` when it is not.
fn in_range(repl: &Repl, v: f64, lo: f64, hi: f64, label: &str) -> bool {
    if (lo..=hi).contains(&v) {
        return true;
    }
    repl.tr
        .error(&format!("Warning: {label}: want {lo:.1}-{hi:.1} (ignored)"));
    false
}

/// Applies every parameter whose value moved to the provider, the budget and the bundle, one dim notice
/// each, and adopts the source of each one it applied.
///
/// A parameter the provider cannot act on is left alone ENTIRELY — value, notice and source. The capability
/// accessor is the same gate the startup tuning uses (which warns once and moves on there); a chat must not
/// record a `top_p` for a dialect that has none, or a window for one that counts no tokens, because a resume
/// would hand it back as though the session had been running under it.
///
/// Every write carries the source map with it. A value and where it came from are one fact: writing the
/// value alone would leave a bundle saying a window the config supplied is the user's own, and the next
/// switch would then keep a window it was supposed to drop.
pub(crate) fn apply(repl: &mut Repl, next: &LayeredParams) -> bool {
    let now = current(repl);
    let counts_tokens = repl.provider.reports_usage();
    let tunable = repl.provider.as_tunable().is_some();
    let nucleus = repl.provider.as_top_p_tunable().is_some();

    // What the chat ends up running under: `next` for every parameter that lands, `now` for the rest.
    let mut landed = now.clone();
    // An evaluation that dropped a declared window leaves `0` behind, which is the SPELLING of "no window
    // configured"; the budget refuses a zero (it must never clobber a real window), so the built-in default
    // is named explicitly here.
    let window = if next.context_window.value == 0 {
        DEFAULT_CONTEXT_WINDOW
    } else {
        next.context_window.value
    };
    if counts_tokens {
        landed.context_window = Param {
            value: window,
            source: next.context_window.source,
        };
    }
    if tunable {
        landed.effort = next.effort.clone();
        landed.temperature = next.temperature.clone();
    }
    if nucleus {
        landed.top_p = next.top_p.clone();
    }
    let sources = landed.sources();
    let mut changed = false;

    if counts_tokens && window != now.context_window.value {
        repl.budget.set_window(window);
        commit(repl, sources, |m| m.set_context_window(window));
        repl.tr
            .notice(&format!("Context window: {}", repl.budget.status()));
        changed = true;
    }

    if tunable {
        if landed.effort.value != now.effort.value
            && let Ok(effort) = Effort::optional(&landed.effort.value)
        {
            if let Some(t) = repl.provider.as_tunable() {
                t.set_effort(effort);
            }
            let level = landed.effort.value.clone();
            commit(repl, sources, move |m| level.clone_into(&mut m.effort));
            repl.tr
                .notice(&format!("Effort: {}", effort_label(&landed.effort.value)));
            changed = true;
        }
        if !float_ptr_equal(landed.temperature.value, now.temperature.value) {
            if let Some(t) = repl.provider.as_tunable() {
                t.set_temperature(landed.temperature.value);
            }
            let temperature = landed.temperature.value;
            commit(repl, sources, move |m| m.temperature = temperature);
            repl.tr.notice(&format!(
                "Temperature: {}",
                format_temperature(landed.temperature.value)
            ));
            changed = true;
        }
    }

    if nucleus && !float_ptr_equal(landed.top_p.value, now.top_p.value) {
        if let Some(t) = repl.provider.as_top_p_tunable() {
            t.set_top_p(landed.top_p.value);
        }
        let top_p = landed.top_p.value;
        commit(repl, sources, move |m| m.top_p = top_p);
        repl.tr.notice(&format!(
            "Top-p: {}",
            format_temperature(landed.top_p.value)
        ));
        changed = true;
    }

    // A source can move without its value doing so — a declaration that merely CONFIRMS what the session was
    // already running under still takes ownership of it, and the next switch must know that.
    if !changed && repl.param_sources != sources {
        commit(repl, sources, |_| {});
    }
    repl.param_sources = sources;
    changed
}

/// What a session STARTING writes into a bundle it has just created: the four values it is running under
/// and the source of each, so a resume finds the session as it was rather than re-deriving it from a config
/// that may have moved since.
///
/// Only a fresh bundle is stamped. A resumed one already says what it was running under, and where it says
/// nothing the evaluated value lives in memory until something changes it — rewriting its meta on the way in
/// would restate a config the user may be about to edit.
///
/// `window` is `None` for a provider with no token accounting: a session that cannot count tokens has no
/// window to record. The other three come from [`current`], i.e. from the provider itself, so a knob the
/// dialect cannot act on is recorded as the nothing it is.
pub(crate) fn stamp_bundle(repl: &Repl, window: Option<u64>, params: &LayeredParams) {
    update_meta(repl, |m| stamp(m, window, params));
}

/// [`stamp_bundle`] on a meta record — the shape `/save` needs, which mints its bundle and stamps it in one
/// write.
pub(crate) fn stamp(meta: &mut SessionMeta, window: Option<u64>, params: &LayeredParams) {
    if let Some(window) = window {
        meta.set_context_window(window);
    }
    params.effort.value.clone_into(&mut meta.effort);
    meta.temperature = params.temperature.value;
    meta.top_p = params.top_p.value;
    meta.param_sources = Some(params.sources());
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
    if let Some(w) = repl
        .writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        let _ = w.update_meta(f);
    }
}
