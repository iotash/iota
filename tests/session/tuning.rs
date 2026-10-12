//! The resume-time tuning replay (`chat/session_test.go:277`, `:574`, `:591`).

use iota::provider::{
    Effort, ImageEditJsonTunable, ImageGenOptions, ImageGenParams, ImageGenTunable, ImageTunable,
    ProviderKind, Tunable,
};
use iota::session::{Overrides, SessionMeta, apply_session_tuning};
use iota::testing::FakeProvider;
use pretty_assertions::assert_eq;

const KIND: ProviderKind = ProviderKind::OpenAi;

/// A provider of `KIND` (model `m1`) implementing every optional capability `apply_session_tuning`
/// reaches for, nothing tuned yet — the `stubProvider` + `tunableStub` + `imageGenStub` +
/// `jsonEditStub` family as ONE configurable provider.
fn stub() -> FakeProvider {
    FakeProvider::new()
        .with_kind(KIND)
        .with_model("m1")
        .with_models(&["m1"])
        .tunable()
        .with_image_output(false)
        .with_image_gen(ImageGenOptions::default(), ImageGenParams::default())
        .with_json_edits(false)
}

/// The `sessionMeta{Provider: "stub", Temperature: &0.5, Effort: "high", ContextWindow: 200_000}` the Go
/// table drives every subtest with.
fn tuned_meta() -> SessionMeta {
    SessionMeta {
        provider: "openai".to_owned(),
        temperature: Some(0.5),
        effort: "high".to_owned(),
        context_window: 200_000,
        ..SessionMeta::default()
    }
}

/// Collects the warnings a replay emitted.
fn replay(
    meta: &SessionMeta,
    provider: &mut FakeProvider,
    skip_temperature: bool,
    skip_window: bool,
) -> (Option<u64>, Vec<String>) {
    let mut warnings = Vec::new();
    let window = apply_session_tuning(
        meta,
        provider,
        KIND.as_str(),
        &Overrides {
            temperature: skip_temperature,
            window: skip_window,
            ..Overrides::default()
        },
        &mut |w| warnings.push(w),
    );
    (window, warnings)
}

#[test]
fn apply_session_tuning_applies_all_recorded_knobs() {
    let mut provider = stub();
    let (window, warnings) = replay(&tuned_meta(), &mut provider, false, false);
    assert_eq!(provider.temperature(), Some(0.5));
    assert_eq!(provider.effort(), Some(Effort::High));
    assert_eq!(window, Some(200_000));
    assert!(warnings.is_empty());
}

#[test]
fn apply_session_tuning_explicit_temperature_flag_wins() {
    let mut provider = stub().with_temperature(Some(0.9));
    let (_window, _) = replay(&tuned_meta(), &mut provider, true, false);
    assert_eq!(
        provider.temperature(),
        Some(0.9),
        "flag temperature overridden"
    );
    assert_eq!(
        provider.effort(),
        Some(Effort::High),
        "effort has no flag and must still apply"
    );
}

#[test]
fn apply_session_tuning_explicit_window_flag_wins() {
    let mut provider = stub();
    let (window, _) = replay(&tuned_meta(), &mut provider, false, true);
    assert_eq!(window, None, "flag window overridden");
}

/// A bundle recorded on another provider replays nothing — and says so, once, naming both.
#[test]
fn apply_session_tuning_provider_mismatch_applies_nothing() {
    let mut provider = stub();
    let mut meta = tuned_meta();
    meta.provider = "different".to_owned();
    meta.image = true;
    meta.json_edits = true;
    meta.aspect_ratio = "3:2".to_owned();
    let (window, warnings) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.temperature(), None);
    assert_eq!(provider.effort(), None);
    assert!(!provider.image_output());
    assert!(!provider.json_edits());
    assert_eq!(*provider.image_gen_params(), ImageGenParams::default());
    assert_eq!(window, None, "the window is gated on the provider too");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("Warning: session was recorded on a different endpoint")
            && warnings[0].ends_with("its model and tuning are not replayed"),
        "{warnings:?}"
    );
}

#[test]
fn apply_session_tuning_unset_values_leave_current_tuning() {
    let mut provider = stub()
        .with_temperature(Some(0.9))
        .with_effort(Some(Effort::Low));
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    let (window, _) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.temperature(), Some(0.9));
    assert_eq!(provider.effort(), Some(Effort::Low));
    assert_eq!(window, None);
}

/// The recorded image knobs replay; an empty meta must not clobber the config defaults.
#[test]
fn apply_session_tuning_image_params() {
    let mut provider = stub();
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        aspect_ratio: "3:2".to_owned(),
        negative_prompt: "blurry".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert_eq!(
        *provider.image_gen_params(),
        ImageGenParams {
            aspect_ratio: Some("3:2".to_owned()),
            image_size: None,
            negative_prompt: Some("blurry".to_owned()),
        }
    );

    // An empty meta must not clobber the config defaults.
    let config = ImageGenParams {
        aspect_ratio: Some("1:1".to_owned()),
        ..ImageGenParams::default()
    };
    let mut provider = stub().with_image_gen(ImageGenOptions::default(), config.clone());
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert_eq!(*provider.image_gen_params(), config);
}

/// The JSON-edits switch replays only when recorded.
#[test]
fn apply_session_tuning_json_edits() {
    let mut provider = stub();
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        json_edits: true,
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(provider.json_edits(), "recorded json_edits not replayed");

    let mut provider = stub();
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(
        !provider.json_edits(),
        "absent meta must not flip the switch"
    );
}

/// The image-output switch replays only when the session recorded it.
#[test]
fn image_output_replays_only_when_recorded() {
    let mut provider = stub();
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        image: true,
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(provider.image_output());

    let mut provider = stub().with_image_output(true);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(
        provider.image_output(),
        "an absent meta never switches it OFF"
    );
}

/// An effort this build cannot parse leaves the current setting and emits ONE frozen warning (D-48).
#[test]
fn unparsable_effort_warns_once_and_keeps_the_current_setting() {
    let mut provider = stub().with_effort(Some(Effort::Low));
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        effort: "ultra".to_owned(),
        ..SessionMeta::default()
    };
    let (_window, warnings) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.effort(), Some(Effort::Low));
    assert_eq!(
        warnings,
        ["Warning: session effort \"ultra\" is not recognised; keeping the current setting"]
    );
}

/// A provider with no optional capability at all is left alone, and the window still comes back.
#[test]
fn provider_without_capabilities_still_yields_the_window() {
    let mut provider = FakeProvider::new().with_kind(KIND).with_model("m1");
    let mut warnings = Vec::new();
    let window = apply_session_tuning(
        &tuned_meta(),
        &mut provider,
        KIND.as_str(),
        &Overrides::default(),
        &mut |w| warnings.push(w),
    );
    assert_eq!(window, Some(200_000));
    assert!(warnings.is_empty());
}

/// A zero or negative recorded window yields nothing (Go's `> 0` guard).
#[test]
fn non_positive_window_is_not_returned() {
    let mut provider = stub();
    for window in [0, -5] {
        let meta = SessionMeta {
            provider: "openai".to_owned(),
            context_window: window,
            ..SessionMeta::default()
        };
        let (got, _) = replay(&meta, &mut provider, false, false);
        assert_eq!(got, None, "window {window}");
    }
}

/// docs/design/bot-mode.md §2.2: a bot's config owns the model, the window, effort, temperature and
/// `top_p` — none of them replays from the meta — while the rest of the bundle's tuning still does.
#[test]
fn a_bot_replays_nothing_the_config_owns() {
    let mut provider = stub().with_temperature(Some(0.9));
    let meta = SessionMeta {
        model: "m-old".to_owned(),
        image: true,
        json_edits: true,
        ..tuned_meta()
    };
    let window = iota::session::replay_session_settings(
        &meta,
        &mut provider,
        KIND.as_str(),
        &Overrides {
            config_wins: true,
            ..Overrides::default()
        },
        &mut |w| panic!("unexpected warning: {w}"),
    );
    assert_eq!(window, None);
    assert_eq!(iota::provider::Provider::model(&provider), "m1");
    assert_eq!(provider.temperature(), Some(0.9));
    assert_eq!(provider.effort(), None);
    assert!(provider.image_output(), "image output still replays");
    assert!(provider.json_edits(), "json edits still replay");
}

/// `max_tokens` (DIVERGENCES X-68) replays like `top_p`: a recorded ceiling goes back onto a dialect that
/// sends one, a bundle with none leaves the dialect's default, and a bot's config owns it.
#[test]
fn a_recorded_max_tokens_replays_unless_the_config_owns_it() {
    use iota::provider::MaxTokensTunable;
    let anthropic = || {
        iota::provider::anthropic::AnthropicProvider::new(
            "k",
            "",
            "glm-5.3",
            None,
            reqwest::Client::new(),
        )
    };
    let meta = SessionMeta {
        provider: "anthropic".to_owned(),
        max_tokens: Some(64_000),
        ..SessionMeta::default()
    };
    let mut warn = |w: String| panic!("unexpected warning: {w}");

    let mut p = anthropic();
    apply_session_tuning(&meta, &mut p, "anthropic", &Overrides::default(), &mut warn);
    assert_eq!(p.max_tokens(), Some(64_000));

    let mut p = anthropic();
    let silent = SessionMeta {
        max_tokens: None,
        ..meta.clone()
    };
    apply_session_tuning(
        &silent,
        &mut p,
        "anthropic",
        &Overrides::default(),
        &mut warn,
    );
    assert_eq!(
        p.max_tokens(),
        None,
        "no record: the dialect's default stands"
    );

    let mut p = anthropic();
    let bot = Overrides {
        config_wins: true,
        ..Overrides::default()
    };
    apply_session_tuning(&meta, &mut p, "anthropic", &bot, &mut warn);
    assert_eq!(p.max_tokens(), None, "a bot's config owns the ceiling");
}

/// M2/M3: startup has already installed the CURRENT config's ceiling when the replay runs. A bundle whose
/// source map carries `max_tokens` restores what it says — its recorded default clears the installed
/// value; one whose map predates the key (main's four keys, decoded from the bytes main writes) leaves the
/// installed value standing; a bot keeps its config's either way.
#[test]
fn a_recorded_default_clears_the_installed_ceiling_and_a_four_key_map_does_not() {
    use iota::provider::MaxTokensTunable;
    let installed = || {
        let mut p = iota::provider::anthropic::AnthropicProvider::new(
            "k",
            "",
            "glm-5.3",
            None,
            reqwest::Client::new(),
        );
        p.set_max_tokens(Some(1234)); // what `cmd::tuning::apply` put there from the config
        p
    };
    let mut warn = |w: String| panic!("unexpected warning: {w}");
    let meta = |sources: &str| -> SessionMeta {
        serde_json::from_str(&format!(
            r#"{{"v":2,"provider":"anthropic","param_sources":{sources}}}"#
        ))
        .expect("meta")
    };
    let five = meta(
        r#"{"context_window":"builtin","effort":"builtin","temperature":"builtin","top_p":"builtin","max_tokens":"builtin"}"#,
    );
    let four = meta(
        r#"{"context_window":"builtin","effort":"builtin","temperature":"builtin","top_p":"builtin"}"#,
    );
    assert!(five.records_max_tokens());
    assert!(!four.records_max_tokens(), "main's map never knew the key");

    let mut p = installed();
    apply_session_tuning(&five, &mut p, "anthropic", &Overrides::default(), &mut warn);
    assert_eq!(
        p.max_tokens(),
        None,
        "a recorded default clears the config's 1234"
    );

    let mut p = installed();
    apply_session_tuning(&four, &mut p, "anthropic", &Overrides::default(), &mut warn);
    assert_eq!(
        p.max_tokens(),
        Some(1234),
        "nothing recorded: the config's value stands"
    );

    let bot = Overrides {
        config_wins: true,
        ..Overrides::default()
    };
    let mut p = installed();
    apply_session_tuning(&five, &mut p, "anthropic", &bot, &mut warn);
    assert_eq!(
        p.max_tokens(),
        Some(1234),
        "a bot's config owns the ceiling"
    );
}
