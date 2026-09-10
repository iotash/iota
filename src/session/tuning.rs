//! Replaying a resumed session's persisted tuning onto the live provider (`ApplySessionTuning`,
//! chat/session.go:414-455).

use crate::provider::{Provider, ProviderKind};

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
    /// `-M`/`model:` was given.
    pub model: bool,
    /// `-t` was given.
    pub temperature: bool,
    /// `--context-window` was given.
    pub window: bool,
}

/// Replays a resumed session onto the live provider: its model (only when the caller did not pick one, and
/// only for a bundle recorded under this provider type — an explicit `-M` never rewrites `meta.model`), then
/// its tuning through [`apply_session_tuning`]. Returns the replayed context window.
pub fn replay_session_settings(
    meta: &SessionMeta,
    provider: &mut dyn Provider,
    kind: ProviderKind,
    overrides: &Overrides,
    warn: &mut dyn FnMut(String),
) -> Option<u64> {
    if !overrides.model && meta.provider == kind.as_str() && !meta.model.is_empty() {
        provider.set_model(meta.model.clone());
    }
    apply_session_tuning(meta, provider, kind, overrides, warn)
}

/// Replays the knobs `meta` recorded. Like the model replay, the WHOLE replay is gated on
/// `meta.provider == kind.as_str()` — a mismatch applies NOTHING, the context window included.
///
/// - **temperature**: only when the caller did not fix one (`overrides.temperature`) and the session recorded one;
/// - **effort**: whenever the session recorded one — Go has no effort flag, so there is no skip;
/// - **image**: `set_image_output(true)` only when `meta.image`;
/// - **image generation**: only when `{aspect_ratio, image_size, negative_prompt}` is non-empty, so an
///   absent meta leaves the config defaults alone (the effort convention);
/// - **json edits**: `set_json_edits(true)` only when `meta.json_edits`.
///
/// The context window is RETURNED rather than pushed through Go's `setWindow` callback — headless has no
/// context budget to route it into. `Some(n)` when the caller did not fix one and `meta.context_window > 0`.
///
/// An effort string this build cannot parse leaves the current effort untouched and emits ONE warning
/// (D-48): `Warning: session effort {e:?} is not recognised; keeping the current setting`.
pub fn apply_session_tuning(
    meta: &SessionMeta,
    provider: &mut dyn Provider,
    kind: ProviderKind,
    overrides: &Overrides,
    warn: &mut dyn FnMut(String),
) -> Option<u64> {
    if meta.provider != kind.as_str() {
        return None;
    }
    if let Some(tunable) = provider.as_tunable() {
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
    if overrides.window {
        return None;
    }
    u64::try_from(meta.context_window).ok().filter(|w| *w > 0)
}
