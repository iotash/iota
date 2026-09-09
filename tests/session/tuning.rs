//! The resume-time tuning replay (`chat/session_test.go:277`, `:574`, `:591`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use iota::provider::{Effort, ImageGenParams, ProviderKind};
use iota::session::{Overrides, SessionMeta, apply_session_tuning};
use pretty_assertions::assert_eq;

use crate::common::{PlainProvider, StubProvider};

const KIND: ProviderKind = ProviderKind::OpenAi;

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
    provider: &mut StubProvider,
    skip_temperature: bool,
    skip_window: bool,
) -> (Option<u64>, Vec<String>) {
    let mut warnings = Vec::new();
    let window = apply_session_tuning(
        meta,
        provider,
        KIND,
        &Overrides {
            temperature: skip_temperature,
            window: skip_window,
            ..Overrides::default()
        },
        &mut |w| warnings.push(w),
    );
    (window, warnings)
}

// Go: chat/session_test.go:281 ("applies all recorded knobs")
#[test]
fn apply_session_tuning_applies_all_recorded_knobs() {
    let mut provider = StubProvider::new(KIND);
    let (window, warnings) = replay(&tuned_meta(), &mut provider, false, false);
    assert_eq!(provider.temperature, Some(0.5));
    assert_eq!(provider.effort, Some(Effort::High));
    assert_eq!(window, Some(200_000));
    assert!(warnings.is_empty());
}

// Go: chat/session_test.go:296 ("explicit temperature flag wins")
#[test]
fn apply_session_tuning_explicit_temperature_flag_wins() {
    let mut provider = StubProvider::new(KIND);
    provider.temperature = Some(0.9);
    let (_window, _) = replay(&tuned_meta(), &mut provider, true, false);
    assert_eq!(
        provider.temperature,
        Some(0.9),
        "flag temperature overridden"
    );
    assert_eq!(
        provider.effort,
        Some(Effort::High),
        "effort has no flag and must still apply"
    );
}

// Go: chat/session_test.go:308 ("explicit window flag wins")
#[test]
fn apply_session_tuning_explicit_window_flag_wins() {
    let mut provider = StubProvider::new(KIND);
    let (window, _) = replay(&tuned_meta(), &mut provider, false, true);
    assert_eq!(window, None, "flag window overridden");
}

// Go: chat/session_test.go:317 ("provider type mismatch applies nothing")
#[test]
fn apply_session_tuning_provider_mismatch_applies_nothing() {
    let mut provider = StubProvider::new(KIND);
    let mut meta = tuned_meta();
    meta.provider = "different".to_owned();
    meta.image = true;
    meta.json_edits = true;
    meta.aspect_ratio = "3:2".to_owned();
    let (window, warnings) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.temperature, None);
    assert_eq!(provider.effort, None);
    assert!(!provider.image_output);
    assert!(!provider.json_edits);
    assert_eq!(provider.image_gen, ImageGenParams::default());
    assert_eq!(window, None, "the window is gated on the type too");
    assert!(warnings.is_empty());
}

// Go: chat/session_test.go:328 ("unset values leave current tuning")
#[test]
fn apply_session_tuning_unset_values_leave_current_tuning() {
    let mut provider = StubProvider::new(KIND);
    provider.temperature = Some(0.9);
    provider.effort = Some(Effort::Low);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    let (window, _) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.temperature, Some(0.9));
    assert_eq!(provider.effort, Some(Effort::Low));
    assert_eq!(window, None);
}

// Go: chat/session_test.go:574
#[test]
fn apply_session_tuning_image_params() {
    let mut provider = StubProvider::new(KIND);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        aspect_ratio: "3:2".to_owned(),
        negative_prompt: "blurry".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert_eq!(
        provider.image_gen,
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
    let mut provider = StubProvider::new(KIND);
    provider.image_gen = config.clone();
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert_eq!(provider.image_gen, config);
}

// Go: chat/session_test.go:591
#[test]
fn apply_session_tuning_json_edits() {
    let mut provider = StubProvider::new(KIND);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        json_edits: true,
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(provider.json_edits, "recorded json_edits not replayed");

    let mut provider = StubProvider::new(KIND);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(!provider.json_edits, "absent meta must not flip the switch");
}

/// The image-output switch replays only when the session recorded it.
#[test]
fn image_output_replays_only_when_recorded() {
    let mut provider = StubProvider::new(KIND);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        image: true,
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(provider.image_output);

    let mut provider = StubProvider::new(KIND);
    provider.image_output = true;
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        ..SessionMeta::default()
    };
    replay(&meta, &mut provider, false, false);
    assert!(
        provider.image_output,
        "an absent meta never switches it OFF"
    );
}

/// An effort this build cannot parse leaves the current setting and emits ONE frozen warning (D-48).
#[test]
fn unparsable_effort_warns_once_and_keeps_the_current_setting() {
    let mut provider = StubProvider::new(KIND);
    provider.effort = Some(Effort::Low);
    let meta = SessionMeta {
        provider: "openai".to_owned(),
        effort: "ultra".to_owned(),
        ..SessionMeta::default()
    };
    let (_window, warnings) = replay(&meta, &mut provider, false, false);
    assert_eq!(provider.effort, Some(Effort::Low));
    assert_eq!(
        warnings,
        ["Warning: session effort \"ultra\" is not recognised; keeping the current setting"]
    );
}

/// A provider with no optional capability at all is left alone, and the window still comes back.
#[test]
fn provider_without_capabilities_still_yields_the_window() {
    let mut provider = PlainProvider(KIND);
    let mut warnings = Vec::new();
    let window = apply_session_tuning(
        &tuned_meta(),
        &mut provider,
        KIND,
        &Overrides::default(),
        &mut |w| warnings.push(w),
    );
    assert_eq!(window, Some(200_000));
    assert!(warnings.is_empty());
}

/// A zero or negative recorded window yields nothing (Go's `> 0` guard).
#[test]
fn non_positive_window_is_not_returned() {
    let mut provider = StubProvider::new(KIND);
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
