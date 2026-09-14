//! The layered evaluation of the four parameters `models:` and `agents:` share (brain page
//! `model-param-layering`).
//!
//! `context_window`, `effort`, `temperature` and `top_p` are resolved from three tiers, highest first:
//!
//! 1. what the `agents:` entry declares — a hard override, one level, never a chain;
//! 2. what the model the session is RUNNING declares in its `models:` entry;
//! 3. the session's current value — and only when it is the session's OWN
//!    ([`ParamSource::User`]/[`ParamSource::Builtin`]); one a declaration supplied
//!    ([`ParamSource::Config`]) is dropped instead, so the model a switch left behind cannot leak into the
//!    model it arrived at.
//!
//! Said the other way round: **when the config speaks the config decides; when the config is silent the
//! hand-set value stands.** A `/model` adjustment is the temporary answer for what no declaration covers,
//! not a permanent override of one — that is what editing `agents:` is for.
//!
//! Exactly TWO moments evaluate: a new session starting, and a `/model` model switch. A manual `/model`
//! change writes the session value directly (tagged [`ParamSource::User`]), and a resume RESTORES the
//! bundle's values and tags rather than re-deriving them — a bundle's value is the session's current value
//! after a restart, no more and no less, which is why a config edited in between only reaches an old
//! session when the user switches models inside it.

use std::collections::BTreeMap;

use crate::config::{AgentConfig, Config, ModelConfig, Resolved};
use crate::provider::Effort;
use crate::session::{LayeredParams, Param, ParamSource, SessionMeta};
use crate::tool::DeferMode;

/// One `context_window:` as it was WRITTEN, plus the layer that wrote it.
///
/// The key is a string (`400k`) and parsing it can fail, so the parse stays with the caller: a bad value
/// aborts a run at startup and is a warning the chat continues under when a `/model` switch trips over it.
/// `label` names the layer the way an error message should.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowDecl<'a> {
    /// The value as written.
    pub raw: &'a str,
    /// Which layer wrote it, for the error text (`config context_window` / `agent context_window`).
    pub label: &'static str,
}

/// The `context_window:` error label of a value written under `models:`, kept as Go named it.
const MODEL_WINDOW_LABEL: &str = "config context_window";
/// The `context_window:` error label of an `agents:` override.
const AGENT_WINDOW_LABEL: &str = "agent context_window";

/// What the two config layers declare for ONE (agent, model) pair: the agent's key where it has one, the
/// model's otherwise, per key.
///
/// `None` is a layer that said nothing — which is what tier 3 of the rule exists for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declared {
    /// `context_window:`, already parsed (see [`WindowDecl`]).
    pub context_window: Option<u64>,
    /// `effort:`; a declaration is never the empty string (that spelling IS the absent key).
    pub effort: Option<String>,
    /// `temperature:`.
    pub temperature: Option<f64>,
    /// `top_p:`.
    pub top_p: Option<f64>,
}

impl Declared {
    /// The `context_window:` that applies to this pair, or `None` when neither layer wrote one. The caller
    /// parses it and hands the result to [`Declared::of`].
    pub fn window_decl<'a>(
        agent: &'a AgentConfig,
        model: &'a ModelConfig,
    ) -> Option<WindowDecl<'a>> {
        if !agent.context_window.is_empty() {
            return Some(WindowDecl {
                raw: &agent.context_window,
                label: AGENT_WINDOW_LABEL,
            });
        }
        if !model.context_window.is_empty() {
            return Some(WindowDecl {
                raw: &model.context_window,
                label: MODEL_WINDOW_LABEL,
            });
        }
        None
    }

    /// The declarations of one (agent, model) pair; `window` is [`window_decl`](Declared::window_decl)
    /// parsed (`None` when neither layer declared one, or when the value did not parse and the caller chose
    /// to carry on without it).
    pub fn of(agent: &AgentConfig, model: &ModelConfig, window: Option<u64>) -> Declared {
        Declared {
            context_window: window,
            effort: first_non_empty(&agent.effort, &model.effort),
            temperature: agent.temperature.or(model.temperature),
            top_p: agent.top_p.or(model.top_p),
        }
    }

    /// THE rule, for all four parameters at once: a declaration wins; with none, the session keeps its own
    /// value and drops one that a declaration had supplied.
    ///
    /// Both evaluating moments are this one call — a new session passes [`LayeredParams::default`] as
    /// `current` (there is nothing to keep), a `/model` switch passes what the session runs under.
    pub fn evaluate(&self, current: &LayeredParams) -> LayeredParams {
        LayeredParams {
            context_window: evaluate_one(self.context_window, &current.context_window),
            effort: evaluate_one(self.effort.clone(), &current.effort),
            temperature: evaluate_one(self.temperature.map(Some), &current.temperature),
            top_p: evaluate_one(self.top_p.map(Some), &current.top_p),
        }
    }

    /// A RESUMED session's four parameters. Resume is NOT an evaluating moment: a bundle's value IS the
    /// session's current value written down, so it comes back with the source it was written with, and the
    /// evaluation runs only for a parameter the bundle never recorded.
    ///
    /// The consequence is deliberate (brain page `model-param-layering`): an old session resumed after its
    /// `agents:` entry changed keeps running as it was, and the new declaration reaches it the first time
    /// the user switches models inside it — so a conversation never changes shape halfway through because a
    /// file on disk did.
    ///
    /// `replayed` is the caller's answer to "did this bundle's tuning actually apply?" — a bundle recorded
    /// under another provider type replays NOTHING (`apply_session_tuning`'s own gate), so it has recorded
    /// nothing for this run either.
    pub fn resume(&self, meta: &SessionMeta, replayed: bool) -> LayeredParams {
        let evaluated = self.evaluate(&LayeredParams::default());
        let src = meta.sources();
        // A bundle that carries the source map has said everything: a parameter it records no value for is
        // one the session deliberately runs WITHOUT (an effort the user set back to "default" is a value,
        // not a gap), so its default stands instead of being evaluated. An older bundle only ever recorded
        // a value that is actually there, which is how a resume read one before the sources existed.
        let complete = replayed && meta.records_params();
        LayeredParams {
            context_window: restore_one(
                pick(replayed, complete, meta.recorded_window()),
                src.context_window,
                evaluated.context_window,
            ),
            effort: restore_one(
                pick(
                    replayed,
                    complete,
                    meta.recorded_effort()
                        // A level this build cannot parse was NOT applied to the provider — the replay
                        // warns and keeps the current setting (D-48) — so the session is not running under
                        // it and it is not the session's current value either. Evaluate instead.
                        .filter(|e| Effort::parse(e).is_ok())
                        .map(str::to_owned),
                ),
                src.effort,
                evaluated.effort,
            ),
            temperature: restore_one(
                pick(replayed, complete, meta.recorded_temperature().map(Some)),
                src.temperature,
                evaluated.temperature,
            ),
            top_p: restore_one(
                pick(replayed, complete, meta.recorded_top_p().map(Some)),
                src.top_p,
                evaluated.top_p,
            ),
        }
    }
}

/// One parameter's restore (see [`Declared::resume`]): the bundle's value with the bundle's own tag, or the
/// evaluation when the bundle recorded nothing.
fn restore_one<T>(recorded: Option<T>, source: ParamSource, evaluated: Param<T>) -> Param<T> {
    match recorded {
        Some(value) => Param { value, source },
        None => evaluated,
    }
}

/// What the bundle recorded for one parameter: nothing when its tuning does not apply to this run at all,
/// the value it carries otherwise — and, for a bundle whose record is COMPLETE, the parameter's own default
/// when it carries none (see [`Declared::resume`]).
fn pick<T: Default>(replayed: bool, complete: bool, value: Option<T>) -> Option<T> {
    if !replayed {
        return None;
    }
    value.or_else(|| complete.then(T::default))
}

/// One parameter's evaluation (see [`Declared::evaluate`]).
fn evaluate_one<T: Clone + Default>(declared: Option<T>, current: &Param<T>) -> Param<T> {
    match declared {
        Some(value) => Param::config(value),
        // The declaration this value came from belonged to the model the session is leaving; carrying it
        // onto a model that declares nothing would be that model inheriting a window it never asked for.
        None if current.source == ParamSource::Config => Param::default(),
        None => current.clone(),
    }
}

/// `a` when it is non-empty, else `b` when IT is, else `None` — the `effort:` spelling of "the agent's key
/// over the model's", where an empty string is the absent key.
fn first_non_empty(a: &str, b: &str) -> Option<String> {
    [a, b]
        .into_iter()
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// What a `/model` switch evaluates against: the running agent's declarations, and every `models:` entry,
/// so the model the user just picked can be looked up by the `provider:id` pair it names.
///
/// A chat holds one of these for its whole life. The provider cannot change inside a session (`/model`
/// offers the models of the endpoint the run started on), so the provider name is fixed here too and a
/// lookup is by model id alone.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamLayers {
    /// The `agents:` entry driving the chat: tier 1 of the rule.
    agent: AgentConfig,
    /// The provider every lookup is relative to.
    provider: String,
    /// `models:`, by entry name, with `provider:` anchored so an entry named after its provider still
    /// answers for it.
    models: BTreeMap<String, ModelConfig>,
    /// The `defer_mode:` the run's dispatcher was ASSEMBLED with. It rides along because it is the one other
    /// thing a `models:` entry decides that a switch cannot re-derive: the deferring wrapper is built once,
    /// around the MCP dispatcher, and the modes leave different traces in the history a session replays
    /// (brain page `config-three-layers`, 2026-09-09). A switch onto a model that declares another mode
    /// therefore says so rather than pretending to have applied it.
    defer_mode: DeferMode,
}

impl Default for ParamLayers {
    /// A config that declares nothing: every parameter falls to the built-in default and the dispatcher runs
    /// the default defer mode. `DeferMode` has no `Default` of its own — [`DeferMode::DEFAULT`] is the named
    /// constant, and naming it here keeps the two from ever disagreeing.
    fn default() -> Self {
        Self {
            agent: AgentConfig::default(),
            provider: String::new(),
            models: BTreeMap::new(),
            defer_mode: DeferMode::DEFAULT,
        }
    }
}

impl ParamLayers {
    /// The layers a run resolved to.
    pub fn new(cfg: &Config, resolved: &Resolved) -> Self {
        let models = cfg
            .models
            .iter()
            .map(|(name, m)| {
                let mut m = m.clone();
                m.anchor_provider(name);
                (name.clone(), m)
            })
            .collect();
        Self {
            agent: resolved.agent.clone(),
            provider: resolved.provider_name.clone(),
            models,
            defer_mode: resolved.model.defer_mode(),
        }
    }

    /// The `defer_mode:` the dispatcher runs under, and the one `id` would ask for: a pair, so the caller
    /// can say which model it belongs to. Equal modes are the silent case.
    pub fn defer_mode_drift(&self, id: &str) -> Option<(DeferMode, DeferMode)> {
        let wanted = self
            .model_of(id)
            .map_or(DeferMode::DEFAULT, ModelConfig::defer_mode);
        (wanted != self.defer_mode).then_some((self.defer_mode, wanted))
    }

    /// The `agents:` entry, tier 1 — which applies whatever model the chat is on.
    pub fn agent(&self) -> &AgentConfig {
        &self.agent
    }

    /// The `models:` entry that configures `id` on this run's provider, or `None` when the id is one the
    /// config never mentions (a raw id from the picker, which is the common case).
    ///
    /// Two entries may describe the same `provider:id` with different tunables; the first in name order
    /// answers, so the choice is at least deterministic.
    pub fn model_of(&self, id: &str) -> Option<&ModelConfig> {
        self.models
            .values()
            .find(|m| m.provider == self.provider && m.id == id)
    }

    /// The declarations that apply when the chat runs `id`. `window` parses (and reports) a
    /// `context_window:` the pair declares; returning `None` from it leaves that key silent.
    pub fn declared(
        &self,
        id: &str,
        window: impl FnOnce(WindowDecl<'_>) -> Option<u64>,
    ) -> Declared {
        let silent = ModelConfig::default();
        let model = self.model_of(id).unwrap_or(&silent);
        let raw = Declared::window_decl(&self.agent, model).and_then(window);
        Declared::of(&self.agent, model, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::{Declared, ParamLayers};
    use crate::config::{AgentConfig, Config, ModelConfig, Resolved};
    use crate::session::{LayeredParams, Param, ParamSource, ParamSources, SessionMeta};

    /// An agent declaring all four.
    fn agent() -> AgentConfig {
        AgentConfig {
            context_window: "400k".to_owned(),
            effort: "high".to_owned(),
            temperature: Some(0.4),
            top_p: Some(0.8),
            ..AgentConfig::default()
        }
    }

    /// A model declaring all four, different values.
    fn model() -> ModelConfig {
        ModelConfig {
            context_window: "128k".to_owned(),
            effort: "low".to_owned(),
            temperature: Some(0.9),
            top_p: Some(0.5),
            ..ModelConfig::default()
        }
    }

    /// The `agents:` entry outranks the model, per key: a key only the model declares still lands.
    #[test]
    fn the_agent_overrides_the_model_key_by_key() {
        let d = Declared::of(&agent(), &model(), Some(400_000));
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(d.temperature, Some(0.4));
        assert_eq!(d.top_p, Some(0.8));
        assert_eq!(
            Declared::window_decl(&agent(), &model()).map(|w| (w.raw, w.label)),
            Some(("400k", "agent context_window"))
        );

        // Only the model declares: the model's value is the declaration.
        let d = Declared::of(&AgentConfig::default(), &model(), Some(128_000));
        assert_eq!(d.effort.as_deref(), Some("low"));
        assert_eq!(d.temperature, Some(0.9));
        assert_eq!(d.top_p, Some(0.5));
        assert_eq!(
            Declared::window_decl(&AgentConfig::default(), &model()).map(|w| w.label),
            Some("config context_window")
        );

        // Neither: every tier-1/2 answer is silence.
        let d = Declared::of(&AgentConfig::default(), &ModelConfig::default(), None);
        assert_eq!(d, Declared::default());
        assert_eq!(
            Declared::window_decl(&AgentConfig::default(), &ModelConfig::default()),
            None
        );
    }

    /// MOMENT 1 — a new session: the evaluation with nothing to keep is `agents` → `models` → the built-in
    /// default, and every value it produces is tagged as coming from the config.
    #[test]
    fn a_new_session_evaluates_the_two_config_layers() {
        let d = Declared::of(&agent(), &model(), Some(400_000));
        let p = d.evaluate(&LayeredParams::default());
        assert_eq!(p.context_window, Param::config(400_000));
        assert_eq!(p.effort, Param::config("high".to_owned()));
        assert_eq!(p.temperature, Param::config(Some(0.4)));
        assert_eq!(p.top_p, Param::config(Some(0.8)));

        // With neither layer declaring anything, the four fall to the built-in defaults.
        let p = Declared::default().evaluate(&LayeredParams::default());
        assert_eq!(p, LayeredParams::default());
        assert_eq!(p.effort.source, ParamSource::Builtin);
    }

    /// MOMENT 2a — a `/model` switch onto a model that declares NOTHING: the value the session inherited
    /// from the model it is leaving is dropped, the value the user typed is kept.
    #[test]
    fn a_switch_drops_an_inherited_value_and_keeps_a_typed_one() {
        let current = LayeredParams {
            context_window: Param::config(400_000),
            effort: Param::user("max".to_owned()),
            temperature: Param::config(Some(0.4)),
            top_p: Param::default(),
        };
        let p = Declared::default().evaluate(&current);
        assert_eq!(
            p.context_window,
            Param::default(),
            "a window that came from a declaration must not follow the session onto another model"
        );
        assert_eq!(p.temperature, Param::default());
        assert_eq!(
            p.effort,
            Param::user("max".to_owned()),
            "what the user typed is the session's own and survives config silence"
        );
        assert_eq!(p.top_p, Param::default(), "a built-in default stays one");
    }

    /// MOMENT 2b — a `/model` switch onto a model that DOES declare: the declaration outranks the session's
    /// value whatever put it there, the user's own included.
    #[test]
    fn a_declaration_outranks_the_session_value() {
        let current = LayeredParams {
            context_window: Param::user(64_000),
            effort: Param::user("max".to_owned()),
            temperature: Param::user(Some(1.5)),
            top_p: Param::user(Some(0.1)),
        };
        let p = Declared::of(&AgentConfig::default(), &model(), Some(128_000)).evaluate(&current);
        assert_eq!(p.context_window, Param::config(128_000));
        assert_eq!(p.effort, Param::config("low".to_owned()));
        assert_eq!(p.temperature, Param::config(Some(0.9)));
        assert_eq!(p.top_p, Param::config(Some(0.5)));
    }

    /// MOMENT 4 — a resume: the bundle's values come back with the bundle's own tags, whatever the config
    /// says NOW. Only a parameter the bundle never recorded is evaluated.
    #[test]
    fn a_resume_restores_the_bundle_rather_than_evaluating() {
        // The bundle recorded a window and an effort; the config declares all four, differently.
        let meta = SessionMeta {
            provider: "openai".to_owned(),
            context_window: 64_000,
            effort: "max".to_owned(),
            param_sources: Some(ParamSources {
                context_window: ParamSource::Config,
                effort: ParamSource::User,
                ..ParamSources::default()
            }),
            ..SessionMeta::default()
        };
        let p = Declared::of(&agent(), &model(), Some(400_000)).resume(&meta, true);
        assert_eq!(
            p.context_window,
            Param::config(64_000),
            "a recorded value is the session's current value, config or no config"
        );
        assert_eq!(p.effort, Param::user("max".to_owned()));
        // The two it carries NO value for are not gaps: this bundle knows the layering, so an absent
        // temperature is a session running without one, and the config does not get to fill it in.
        assert_eq!(p.temperature, Param::default());
        assert_eq!(p.top_p, Param::default());

        // And the restored tags are what the NEXT switch acts on: the inherited window goes, the user's
        // effort stays — the same contrast as moment 2a, now across a restart.
        let after = Declared::default().evaluate(&p);
        assert_eq!(after.context_window, Param::default());
        assert_eq!(after.effort, Param::user("max".to_owned()));
    }

    /// A bundle with no `param_sources` key — every session written before the layering — comes back as the
    /// user's own, so a later switch onto a model that declares nothing KEEPS what it was running under.
    #[test]
    fn a_bundle_without_sources_is_read_as_the_users_own() {
        let meta = SessionMeta {
            provider: "openai".to_owned(),
            context_window: 200_000,
            effort: "high".to_owned(),
            temperature: Some(0.7),
            ..SessionMeta::default()
        };
        assert!(!meta.records_params(), "the key is not there at all");
        assert_eq!(
            meta.sources(),
            ParamSources::LEGACY,
            "and its absence reads as the user's own"
        );
        let p = Declared::default().resume(&meta, true);
        assert_eq!(p.context_window, Param::user(200_000));
        assert_eq!(p.effort, Param::user("high".to_owned()));
        assert_eq!(p.temperature, Param::user(Some(0.7)));
        assert_eq!(p.top_p, Param::default(), "the bundle recorded no top_p");

        let after = Declared::default().evaluate(&p);
        assert_eq!(
            after.context_window,
            Param::user(200_000),
            "nothing is lost"
        );
        assert_eq!(after.effort, Param::user("high".to_owned()));
        assert_eq!(after.temperature, Param::user(Some(0.7)));

        // The same bundle, written by a build that knows the layering: its silence about `top_p` is now a
        // statement, so a declaration no longer fills it in.
        let declared = Declared {
            top_p: Some(0.5),
            ..Declared::default()
        };
        let complete = SessionMeta {
            param_sources: Some(ParamSources::default()),
            ..meta.clone()
        };
        assert_eq!(declared.resume(&meta, true).top_p, Param::config(Some(0.5)));
        assert_eq!(declared.resume(&complete, true).top_p, Param::default());
    }

    /// A bundle whose tuning does not apply to this run recorded nothing FOR it: the replay is gated on the
    /// provider type, and the parameters follow the same gate. Likewise an effort spelling this build cannot
    /// parse — the replay never applied it, so the session is not running under it.
    #[test]
    fn a_bundle_that_does_not_replay_records_nothing() {
        let meta = SessionMeta {
            provider: "anthropic".to_owned(),
            context_window: 64_000,
            ..SessionMeta::default()
        };
        let p = Declared::of(&AgentConfig::default(), &model(), Some(128_000)).resume(&meta, false);
        assert_eq!(p.context_window, Param::config(128_000));

        let meta = SessionMeta {
            provider: "openai".to_owned(),
            effort: "turbo".to_owned(),
            ..SessionMeta::default()
        };
        let p = Declared::of(&AgentConfig::default(), &model(), None).resume(&meta, true);
        assert_eq!(p.effort, Param::config("low".to_owned()));
    }

    /// The lookup a switch does: a `models:` entry is found by the `provider:id` it names, an id the config
    /// never mentions leaves the model layer silent, and the agent's override applies either way.
    #[test]
    fn the_models_layer_is_found_by_provider_and_id() {
        let mut cfg = Config::default();
        cfg.models.insert(
            "gpt5".to_owned(),
            ModelConfig {
                provider: "openai".to_owned(),
                id: "gpt-5.2".to_owned(),
                context_window: "400k".to_owned(),
                effort: "high".to_owned(),
                ..ModelConfig::default()
            },
        );
        // An entry named after its provider needs no `provider:` line, and must still answer for it.
        cfg.models.insert(
            "anthropic".to_owned(),
            ModelConfig {
                id: "claude-x".to_owned(),
                effort: "low".to_owned(),
                ..ModelConfig::default()
            },
        );
        let resolved = Resolved {
            provider_name: "openai".to_owned(),
            agent: AgentConfig {
                temperature: Some(0.2),
                ..AgentConfig::default()
            },
            ..Resolved::default()
        };
        let layers = ParamLayers::new(&cfg, &resolved);

        let d = layers.declared("gpt-5.2", |w| {
            assert_eq!(w.raw, "400k");
            Some(400_000)
        });
        assert_eq!(d.context_window, Some(400_000));
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(
            d.temperature,
            Some(0.2),
            "the agent's override still applies"
        );

        // The same id on another provider is another model: this run's provider is `openai`.
        assert!(layers.model_of("claude-x").is_none());
        // A raw id the config never mentions: nothing but the agent's override is declared.
        let d = layers.declared("gpt-4o-mini", |_| panic!("no window is declared"));
        assert_eq!(d.context_window, None);
        assert_eq!(d.effort, None);
        assert_eq!(d.temperature, Some(0.2));
    }
}
