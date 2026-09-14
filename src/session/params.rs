//! The four parameters a session runs under, each with the SOURCE that put it there (brain page
//! `model-param-layering`).
//!
//! `context_window`, `effort`, `temperature` and `top_p` are declared in two config layers and can also be
//! set by hand in `/model`. The value alone is not enough to decide what a later model switch should do with
//! it — a window inherited from `models.gpt5` must not survive a switch to a model that declares none, while
//! one the user typed must — so every value carries where it came from ([`ParamSource`]), and the tag is
//! persisted beside the value in `meta.json`.
//!
//! The evaluation itself ([`crate::config::Declared::evaluate`]) lives with the config layers it reads; this
//! module is only the STATE: what the session currently runs under, and how it is written down.

/// Where the session's current value for a layered parameter came from.
///
/// The distinction that matters is [`Config`](ParamSource::Config) versus the other two: a value a
/// declaration supplied belongs to that declaration, so a `/model` switch that lands on a model whose
/// config is silent DROPS it rather than letting the model it left leak into the one it arrived at. A value
/// the user typed, and the built-in default, are the session's own and survive the switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSource {
    /// Nothing declared the parameter and nobody set one: the built-in default (an empty effort, an
    /// omitted temperature, a window of 0).
    #[default]
    Builtin,
    /// A config DECLARATION put it there — the model's `models:` entry, or the `agents:` entry that
    /// overrides it. Both are re-derived at every evaluation, which is why they share one tag: the question
    /// a switch asks is "did a declaration say this?", not "which of the two said it".
    Config,
    /// The user set it by hand in `/model`. It is the session's own value: config silence keeps it.
    User,
}

/// The source of each layered parameter, as `meta.json` records it.
///
/// The key's PRESENCE is itself information: a bundle that carries it was written by a build that knows the
/// layering, so what it says is complete — a parameter it records no value for is one the session
/// deliberately runs without, not a gap to fill from the config. A bundle without the key predates the rule
/// (`meta.param_sources` is `None`), and [`ParamSources::LEGACY`] is how its values are read instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ParamSources {
    /// Where the recorded `context_window` came from.
    pub context_window: ParamSource,
    /// Where the recorded `effort` came from.
    pub effort: ParamSource,
    /// Where the recorded `temperature` came from.
    pub temperature: ParamSource,
    /// Where the recorded `top_p` came from.
    pub top_p: ParamSource,
}

impl ParamSources {
    /// How a bundle written before the key existed is read: every value it carries is treated as the
    /// USER's own.
    ///
    /// It is the conservative choice, and the only one that cannot lose something a person chose: a value
    /// tagged this way is KEPT when a later model switch finds no declaration, where
    /// [`Config`](ParamSource::Config) would have dropped it. Those bundles were written when a `/model`
    /// change was the only way a session's tuning could differ from its config, so "the user set this" is
    /// also the likeliest truth about them.
    pub const LEGACY: ParamSources = ParamSources {
        context_window: ParamSource::User,
        effort: ParamSource::User,
        temperature: ParamSource::User,
        top_p: ParamSource::User,
    };

    /// Every parameter at the same source — the four evaluated at once, for a caller with nothing to
    /// distinguish them by.
    pub const fn all(source: ParamSource) -> ParamSources {
        ParamSources {
            context_window: source,
            effort: source,
            temperature: source,
            top_p: source,
        }
    }
}

/// One layered parameter: the value the session currently runs under, and where it came from.
///
/// `T` is the value AS THE SESSION HOLDS IT, with `T::default()` meaning "unset" — `0` for the window, `""`
/// for the effort, `None` for the two sampling knobs — so one evaluation serves all four.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Param<T> {
    /// The value.
    pub value: T,
    /// Where it came from.
    pub source: ParamSource,
}

impl<T> Param<T> {
    /// A value the user set by hand: it survives a model switch that finds no declaration.
    pub fn user(value: T) -> Self {
        Self {
            value,
            source: ParamSource::User,
        }
    }

    /// A value a config declaration supplied: a switch to a model no declaration covers drops it.
    pub fn config(value: T) -> Self {
        Self {
            value,
            source: ParamSource::Config,
        }
    }
}

/// The four layered parameters as ONE value: what the session runs under right now.
///
/// It is assembled from where each value actually lives — the context budget for the window, the provider
/// for the three tunables — rather than kept as a second copy of them; only the sources are state of their
/// own ([`ParamSources`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayeredParams {
    /// The context window compaction accounts against; `0` = the chat's own default.
    pub context_window: Param<u64>,
    /// The reasoning effort, as the wire spells it; `""` = omit the parameter.
    pub effort: Param<String>,
    /// The sampling temperature; `None` = omit the parameter.
    pub temperature: Param<Option<f64>>,
    /// Nucleus sampling; `None` = omit the parameter.
    pub top_p: Param<Option<f64>>,
}

impl LayeredParams {
    /// The four sources, in the shape `meta.json` records.
    pub fn sources(&self) -> ParamSources {
        ParamSources {
            context_window: self.context_window.source,
            effort: self.effort.source,
            temperature: self.temperature.source,
            top_p: self.top_p.source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ParamSource, ParamSources};

    /// The legacy reading is a named constant, not the `Default`: an absent key means "the user's own",
    /// while a PRESENT one that says nothing about a parameter means the built-in default.
    #[test]
    fn the_legacy_reading_is_not_the_default_one() {
        assert_eq!(ParamSources::LEGACY, ParamSources::all(ParamSource::User));
        assert_eq!(
            ParamSources::default(),
            ParamSources::all(ParamSource::Builtin)
        );

        let partial: ParamSources = serde_json::from_str(r#"{"effort":"config"}"#).expect("decode");
        assert_eq!(partial.effort, ParamSource::Config);
        assert_eq!(partial.context_window, ParamSource::Builtin);
    }

    /// The wire spelling, which `meta.json` is read back through.
    #[test]
    fn the_sources_round_trip_through_json() {
        let sources = ParamSources {
            context_window: ParamSource::Config,
            effort: ParamSource::User,
            ..ParamSources::default()
        };
        let text = serde_json::to_string(&sources).expect("serialise");
        assert_eq!(
            text,
            r#"{"context_window":"config","effort":"user","temperature":"builtin","top_p":"builtin"}"#
        );
        assert_eq!(
            serde_json::from_str::<ParamSources>(&text).expect("decode"),
            sources
        );
    }
}
