//! PROBE (evaluation `xprov-fable`, 2026-10-09): what the persistence layer and the config layers do when
//! a session's provider type is not the one its bundle was created under — the state `iota resume -M
//! other:model` puts a bundle in today, and the state an in-session provider switch would put the live
//! writer and the parameter layers in.
//!
//! Not product code. Each test pins one measured fact the evaluation relies on.

use crate::common::{log_lines, temp_store};
use iota::provider::model::{JsonObject, Message, Raw, RawContent, ToolCall};
use iota::provider::{Provider, ProviderKind};
use iota::session::{NewSession, Overrides, SessionMeta, replay_session_settings};
use iota::testing::FakeProvider;
use pretty_assertions::assert_eq;

fn raw(s: &str) -> Raw {
    Raw::from_string(s.to_owned()).unwrap()
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "f".to_owned(),
        arguments: JsonObject::new(),
    }
}

const THINKING: &str = r#"{"type":"thinking","thinking":"weigh","signature":"SIG"}"#;
const REASONING: &str =
    r#"{"id":"rs_2","type":"reasoning","summary":[],"encrypted_content":"OPAQUE"}"#;
const FUNCTION_CALL: &str =
    r#"{"id":"fc_2","type":"function_call","call_id":"call_2","name":"f","arguments":"{}"}"#;

/// 【实测】A bundle created under `anthropic`, resumed under `openresponses` (what `resume -M` does): the
/// anthropic blob is dropped on load and text / tool calls / reasoning text survive; the records the
/// resumed writer appends are tagged `openresponses`; `meta.provider` is NOT moved and keeps saying
/// `anthropic`. The log is per-record tagged already — each type restores exactly its own records — so the
/// on-disk format needs nothing for a mixed-dialect session; only `meta.provider` is a lie about it.
#[test]
fn a_bundle_resumed_under_another_type_holds_both_dialects_while_meta_names_one() {
    let (_home, store) = temp_store();
    let mut w = store
        .create(NewSession::new(ProviderKind::Anthropic, "claude-x"))
        .unwrap();
    let id = w.id().to_owned();
    let c1 = call("toolu_1");
    w.append_messages(&[
        Message::user("q1"),
        Message::assistant_with_calls(
            "",
            vec![c1.clone()],
            Some(RawContent::Anthropic(vec![raw(THINKING)])),
        )
        .with_reasoning("weigh"),
        Message::tool_result(&c1, "result", false),
        Message::assistant("answer 1"),
    ])
    .unwrap();
    let dir = w.dir().to_path_buf();
    drop(w);

    let (mut w2, resumed) = store.resume(&id, ProviderKind::OpenResponses).unwrap();
    assert_eq!(
        resumed.messages[1].raw_content(),
        None,
        "the foreign blob is dropped"
    );
    assert_eq!(resumed.messages[1].tool_calls().len(), 1);
    assert_eq!(
        resumed.messages[1].reasoning(),
        "weigh",
        "the reasoning text is kept"
    );
    assert_eq!(resumed.meta.provider, "anthropic");
    assert_eq!(resumed.meta.model, "claude-x");

    let c2 = call("call_2");
    w2.append_messages(&[
        Message::user("q2"),
        Message::assistant_with_calls(
            "",
            vec![c2.clone()],
            Some(RawContent::OpenResponses(vec![
                raw(REASONING),
                raw(FUNCTION_CALL),
            ])),
        ),
        Message::tool_result(&c2, "result", false),
        Message::assistant("answer 2"),
    ])
    .unwrap();
    let lines = log_lines(&dir);
    assert!(
        lines[1].contains(r#""raw":{"provider":"anthropic","#),
        "{}",
        lines[1]
    );
    assert!(
        lines[5].contains(r#""raw":{"provider":"openresponses","#),
        "{}",
        lines[5]
    );
    assert_eq!(
        w2.meta().provider,
        "anthropic",
        "the writer appended under openresponses and left meta.provider alone"
    );
    assert_eq!(SessionMeta::read(&dir).unwrap().provider, "anthropic");
    drop(w2);

    let under_a = store.load(&id, ProviderKind::Anthropic).unwrap();
    assert!(under_a.messages[1].raw_content().is_some());
    assert!(under_a.messages[5].raw_content().is_none());
    let under_r = store.load(&id, ProviderKind::OpenResponses).unwrap();
    assert!(under_r.messages[1].raw_content().is_none());
    assert!(under_r.messages[5].raw_content().is_some());
}

/// 【实测】The failure an in-memory provider switch would cause if the live writer were NOT rebuilt: a
/// writer whose `kind` is `anthropic` is handed the responses dialect's payload and writes the record
/// WITHOUT it — no error, no warning — so a later resume under `openresponses` has no reasoning item to
/// replay. This is `SessionWriter.kind`, identity point 3 of the evaluation, measured.
#[test]
fn a_writer_of_one_type_silently_drops_the_other_dialects_payload() {
    let (_home, store) = temp_store();
    let mut w = store
        .create(NewSession::new(ProviderKind::Anthropic, "claude-x"))
        .unwrap();
    let id = w.id().to_owned();
    let c = call("call_1");
    w.append_messages(&[
        Message::user("q"),
        Message::assistant_with_calls(
            "",
            vec![c],
            Some(RawContent::OpenResponses(vec![
                raw(REASONING),
                raw(FUNCTION_CALL),
            ])),
        ),
    ])
    .expect("the append succeeds");
    let dir = w.dir().to_path_buf();
    drop(w);
    let lines = log_lines(&dir);
    assert!(
        !lines[1].contains("raw"),
        "the payload never reached the disk: {}",
        lines[1]
    );
    let under_r = store.load(&id, ProviderKind::OpenResponses).unwrap();
    assert_eq!(under_r.messages[1].raw_content(), None);
    assert_eq!(under_r.messages[1].tool_calls().len(), 1);
}

/// 【实测】The resume gate: a bundle recorded under `anthropic` resumed on an `openresponses` provider
/// replays NOTHING — not the model, not the effort, not the temperature, not the window — and emits no
/// warning while doing so. The consistency debt the two earlier investigations named, pinned.
#[test]
fn a_resume_under_another_type_replays_nothing_and_says_nothing() {
    let meta = SessionMeta {
        provider: "anthropic".to_owned(),
        model: "claude-x".to_owned(),
        effort: "high".to_owned(),
        temperature: Some(0.3),
        context_window: 200_000,
        ..SessionMeta::default()
    };
    let mut p = FakeProvider::new()
        .with_kind(ProviderKind::OpenResponses)
        .with_model("gpt-x")
        .tunable();
    let mut warnings = Vec::new();
    let window = replay_session_settings(
        &meta,
        &mut p,
        ProviderKind::OpenResponses,
        &Overrides::default(),
        &mut |w| warnings.push(w),
    );
    assert_eq!(window, None);
    assert_eq!(p.model(), "gpt-x");
    let t = p.as_tunable().expect("tunable");
    assert_eq!(t.effort(), None);
    assert_eq!(t.temperature(), None);
    assert!(warnings.is_empty(), "nothing told the user: {warnings:?}");
}

/// 【实测】`ParamLayers` resolves a model id on the STARTUP provider only (identity point 6): after a
/// switch to `resp`, picking `shared-id` would land the ANTHROPIC entry's knobs on the responses model, and
/// an id that exists only on `resp` declares nothing at all — its `context_window: 1m` never lands. The
/// layers must move with the provider, or the switch applies the wrong declarations silently.
#[test]
fn the_layers_look_a_model_id_up_on_the_startup_provider_only() {
    const YAML: &str = r"
providers:
  anth: {type: anthropic, key: k}
  resp: {type: openresponses, key: k}
models:
  a: {provider: anth, id: shared-id, effort: low}
  r: {provider: resp, id: shared-id, effort: high}
  r2: {provider: resp, id: only-on-resp, context_window: 1m}
agents:
  default: {model: a, choices: [a, r, r2]}
";
    let cfg =
        iota::cmd::Config::parse(YAML.as_bytes(), &iota::app::env::Env::default(), &mut |w| {
            panic!("unexpected config warning: {w}")
        })
        .expect("the config loads");
    let resolved = cfg.resolve_agent("default").expect("the agent resolves");
    assert_eq!(resolved.provider_name, "anth");
    let layers = iota::cmd::ParamLayers::new(&cfg, &resolved);

    let d = layers.declared("shared-id", |_| Some(1));
    assert_eq!(
        d.effort.as_deref(),
        Some("low"),
        "the anthropic entry answers for an id the user would pick on resp"
    );
    assert_eq!(
        layers.model_of("shared-id").map(|m| m.provider.as_str()),
        Some("anth")
    );

    let mut asked = false;
    let d = layers.declared("only-on-resp", |_| {
        asked = true;
        Some(1_000_000)
    });
    assert_eq!(d.context_window, None);
    assert!(
        !asked,
        "no window declaration was found for the responses-only id"
    );
    assert!(layers.model_of("only-on-resp").is_none());
}
