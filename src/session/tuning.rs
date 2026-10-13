//! Replaying a resumed session's persisted tuning onto the live provider (`ApplySessionTuning`,
//! chat/session.go:414-455).

use crate::provider::Provider;

use crate::session::meta::SessionMeta;

/// Announces an agent a resumed bundle names that the config no longer defines.
///
/// `meta.agent` records how the session was STARTED; a resume reassembles from the current config, so an
/// agent that is still there needs nothing done to it. One that has been deleted since would otherwise
/// vanish silently, and the run falls back to the provider and the model the meta carries — exactly the
/// behaviour every session had before the key existed (decision of 2026-09-10).
///
/// `configured` is the caller's answer to "does `cfg.agents` still have this name?".
pub fn warn_if_session_agent_is_gone(
    meta: &SessionMeta,
    configured: bool,
    warn: &mut dyn FnMut(String),
) {
    if meta.agent.is_empty() || configured {
        return;
    }
    warn(format!(
        "Warning: session agent {:?} is no longer configured; using the provider and model from the session",
        meta.agent
    ));
}

/// What the caller already fixed from flags or config; a resumed session's stored value yields to each
/// (root.go:317-331: explicit flags win).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `-M` chose the model, so the session's must not overwrite it.
    pub model: bool,
    /// The caller fixed a temperature. No CLI flag does since `-t` was retired, so a resumed session's own
    /// value now always wins over the config default — which is what resuming a chat means.
    pub temperature: bool,
    /// The caller fixed a context window (likewise: `--context-window` is gone).
    pub window: bool,
    /// A bot (docs/design/bot-mode.md §2.2): the config owns the model, the context window, effort,
    /// temperature, `top_p` and `max_tokens`, so the session's record of them replays NOTHING — a config edit takes effect
    /// on the next start instead of being frozen at the session's first day. The rest of the bundle's tuning
    /// (image output and generation, json edits) still replays.
    pub config_wins: bool,
}

/// Replays a resumed session onto the live provider: its model (only when the caller did not pick one, and
/// only for a bundle recorded on this provider — [`SessionMeta::recorded_on`]), then its tuning
/// through [`apply_session_tuning`]. Returns the replayed context window.
pub fn replay_session_settings(
    meta: &SessionMeta,
    provider: &mut dyn Provider,
    on: &str,
    overrides: &Overrides,
    warn: &mut dyn FnMut(String),
) -> Option<u64> {
    if !overrides.model
        && !overrides.config_wins
        && meta.recorded_on(on, provider.kind())
        && !meta.model.is_empty()
    {
        provider.set_model(meta.model.clone());
    }
    apply_session_tuning(meta, provider, on, overrides, warn)
}

/// Replays the knobs `meta` recorded. Like the model replay, the WHOLE replay is gated on
/// [`SessionMeta::recorded_on`] (`on` is the provider name the run is on; a v1 bundle matches on the type) —
/// a mismatch applies NOTHING, the context window included, and says so in ONE warning, unless
/// `overrides.config_wins` (a bot replays none of it anyway). Such a resume is a move: of the five layered
/// parameters, the caller keeps the ones the user set where the config here is silent
/// ([`Declared::resume`](crate::config::Declared::resume)), and the warning says that too.
///
/// - **temperature**: only when the caller did not fix one (`overrides.temperature`) and the session recorded one;
/// - **effort**: whenever the session recorded one — Go has no effort flag, so there is no skip;
/// - **`top_p`**: whenever the session recorded one (no flag, no skip);
/// - **`max_tokens`**: likewise (DIVERGENCES X-68);
/// - **image**: `set_image_output(true)` only when `meta.image`;
/// - **image generation**: only when `{aspect_ratio, image_size, negative_prompt}` is non-empty, so an
///   absent meta leaves the config defaults alone (the effort convention);
/// - **json edits**: `set_json_edits(true)` only when `meta.json_edits`.
///
/// With `overrides.config_wins` (a bot) temperature, effort, `top_p`, `max_tokens` and the window are skipped: the config
/// owns them.
///
/// The context window is RETURNED rather than pushed through Go's `setWindow` callback — headless has no
/// context budget to route it into. `Some(n)` when the caller did not fix one and `meta.context_window > 0`.
///
/// An effort string this build cannot parse leaves the current effort untouched and emits ONE warning
/// (D-48): `Warning: session effort {e:?} is not recognised; keeping the current setting`.
pub fn apply_session_tuning(
    meta: &SessionMeta,
    provider: &mut dyn Provider,
    on: &str,
    overrides: &Overrides,
    warn: &mut dyn FnMut(String),
) -> Option<u64> {
    if !meta.recorded_on(on, provider.kind()) {
        if !overrides.config_wins {
            warn(format!(
                "Warning: session was recorded on {}; this run is on {on}:{} — its model is not replayed, and of its tuning only what was set by hand carries over, unless this run's config sets it",
                meta.recorded_where(),
                provider.model()
            ));
            if meta.effort().is_err() {
                warn(format!(
                    "Warning: session effort {:?} is not recognised; keeping the current setting",
                    meta.effort
                ));
            }
        }
        return None;
    }
    if !overrides.config_wins
        && let Some(tunable) = provider.as_tunable()
    {
        if !overrides.temperature && meta.temperature.is_some() {
            tunable.set_temperature(meta.temperature);
        }
        match meta.effort() {
            Ok(None) => {}
            Ok(effort) => tunable.set_effort(effort),
            Err(_) => warn(format!(
                "Warning: session effort {:?} is not recognised; keeping the current setting",
                meta.effort
            )),
        }
    }
    // `top_p` has no flag and no `/model` tab; a bundle carries it only because the layering made it one of
    // the four a session runs under, and a resume is where that record is handed back to the provider.
    if !overrides.config_wins
        && let Some(top_p) = meta.top_p
        && let Some(tunable) = provider.as_top_p_tunable()
    {
        tunable.set_top_p(Some(top_p));
    }
    // `max_tokens` is the same kind of record: config-only, carried so a resume runs under the ceiling the
    // session was started with. A bundle that records the key restores what it says, the DEFAULT included —
    // startup already installed the current config's ceiling, and a recorded default must clear it. One
    // that never knew the key leaves that ceiling standing (X-68; the same reading as `Declared::resume`).
    if !overrides.config_wins
        && let Some(tunable) = provider.as_max_tokens_tunable()
    {
        // A hand-edited `0` is neither (`records_max_tokens`/`recorded_max_tokens`): nothing is set.
        if meta.records_max_tokens() {
            tunable.set_max_tokens(meta.recorded_max_tokens());
        } else if let Some(n) = meta.recorded_max_tokens() {
            tunable.set_max_tokens(Some(n));
        }
    }
    if meta.image
        && let Some(image) = provider.as_image_tunable()
    {
        image.set_image_output(true);
    }
    let params = meta.image_gen_params();
    if !params.is_empty()
        && let Some(image_gen) = provider.as_image_gen_tunable()
    {
        image_gen.set_image_gen_params(params);
    }
    if meta.json_edits
        && let Some(edits) = provider.as_image_edit_json_tunable()
    {
        edits.set_json_edits(true);
    }
    if overrides.window || overrides.config_wins {
        return None;
    }
    u64::try_from(meta.context_window).ok().filter(|w| *w > 0)
}
