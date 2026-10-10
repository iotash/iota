//! A session that moves between providers (`iota resume -M other:model`, a `/model` move before the first
//! reply): what the persistence layer and the config layers do with it. Born as the probes of evaluation
//! `xprov-fable` (2026-10-09), which pinned the debt — a meta that kept naming the old provider, a writer
//! that silently dropped the new dialect's payload, a resume that replayed nothing and said nothing, layers
//! that looked ids up on the startup provider — and flipped when the move was made honest.

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

/// A bundle created under `anthropic`, resumed under `openresponses` (what `resume -M` does): the anthropic
/// blob is dropped on load — and SAID to be ([`Session::foreign_notice`]) — while text / tool calls /
/// reasoning text survive; the records the moved writer appends are tagged `openresponses`, and the meta
/// names the provider and the model the session now runs on. The log is per-record tagged, so each type
/// restores exactly its own records.
#[test]
fn a_bundle_that_moves_holds_both_dialects_and_its_meta_follows() {
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
    assert_eq!(resumed.foreign, ["anthropic"]);
    assert_eq!(
        resumed
            .foreign_notice(ProviderKind::OpenResponses)
            .as_deref(),
        Some(
            "1 earlier repl(ies) carry anthropic reasoning that openresponses does not replay; the model sees their text and tool calls only."
        )
    );
    w2.move_to(ProviderKind::OpenResponses, "resp", "gpt-x")
        .unwrap();
    assert_eq!(SessionMeta::read(w2.dir()).unwrap().provider, "resp");
    assert_eq!(SessionMeta::read(w2.dir()).unwrap().model, "gpt-x");

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
    assert_eq!(SessionMeta::read(&dir).unwrap().provider, "resp");
    drop(w2);

    let under_a = store.load(&id, ProviderKind::Anthropic).unwrap();
    assert!(under_a.messages[1].raw_content().is_some());
    assert!(under_a.messages[5].raw_content().is_none());
    assert_eq!(under_a.foreign, ["openresponses"]);
    let under_r = store.load(&id, ProviderKind::OpenResponses).unwrap();
    assert!(under_r.messages[1].raw_content().is_none());
    assert!(under_r.messages[5].raw_content().is_some());
    assert_eq!(under_r.foreign, ["anthropic"]);
}

/// A writer encodes the payloads of ITS type only: one created under `anthropic` and handed the responses
/// dialect's payload would write the record without it. A chat that moves therefore moves its writer
/// ([`SessionWriter::move_to`]) — and the payload the new dialect produces reaches the disk, tagged with it,
/// for a later resume under that type to replay. Identity point 3 of the evaluation.
#[test]
fn a_moved_writer_keeps_the_new_dialects_payload() {
    let (_home, store) = temp_store();
    let mut w = store
        .create(NewSession::new(ProviderKind::Anthropic, "claude-x"))
        .unwrap();
    let id = w.id().to_owned();
    w.move_to(ProviderKind::OpenResponses, "resp", "gpt-x")
        .unwrap();
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
        lines[1].contains(r#""raw":{"provider":"openresponses","#),
        "the payload reached the disk under its own tag: {}",
        lines[1]
    );
    assert_eq!(SessionMeta::read(&dir).unwrap().provider, "resp");
    let under_r = store.load(&id, ProviderKind::OpenResponses).unwrap();
    assert!(under_r.messages[1].raw_content().is_some());
    assert!(under_r.foreign.is_empty());
}

/// The resume gate: a bundle recorded on `anthropic` resumed on another provider (`-M`) replays NOTHING — not
/// the model, not the effort, not the temperature, not the window — and SAYS so, naming both.
#[test]
fn a_resume_on_another_provider_replays_nothing_and_says_so() {
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
    let window = replay_session_settings(&meta, &mut p, "resp", &Overrides::default(), &mut |w| {
        warnings.push(w);
    });
    assert_eq!(window, None);
    assert_eq!(p.model(), "gpt-x");
    let t = p.as_tunable().expect("tunable");
    assert_eq!(t.effort(), None);
    assert_eq!(t.temperature(), None);
    assert_eq!(
        warnings,
        [
            "Warning: session was recorded on anthropic:claude-x; this run is on resp:gpt-x — its model and tuning are not replayed"
        ]
    );
}

/// `ParamLayers` resolves a model id on the provider it was built for (identity point 6): layers built for
/// `anth` would land the ANTHROPIC entry's knobs on `resp:shared-id`, and declare nothing for an id only
/// `resp` serves. So a move builds them again for the run moved to the new provider — as `-M resp:…` would
/// have resolved it — and they answer for `resp`.
#[test]
fn layers_built_for_a_moved_run_look_ids_up_on_the_new_provider() {
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
    let mut resolved = cfg.resolve_agent("default").expect("the agent resolves");
    assert_eq!(resolved.provider_name, "anth");
    let startup = iota::cmd::ParamLayers::new(&cfg, &resolved);
    assert_eq!(
        startup.declared("shared-id", |_| Some(1)).effort.as_deref(),
        Some("low"),
        "built for anth, the anthropic entry answers"
    );

    resolved.move_to_provider(&cfg, "resp");
    let layers = iota::cmd::ParamLayers::new(&cfg, &resolved);
    let d = layers.declared("shared-id", |_| Some(1));
    assert_eq!(d.effort.as_deref(), Some("high"), "the resp entry answers");
    assert_eq!(
        layers.model_of("shared-id").map(|m| m.provider.as_str()),
        Some("resp")
    );

    let mut asked = false;
    let d = layers.declared("only-on-resp", |_| {
        asked = true;
        Some(1_000_000)
    });
    assert!(asked, "the responses-only id's window is declared");
    assert_eq!(d.context_window, Some(1_000_000));
    assert!(layers.model_of("only-on-resp").is_some());
}
