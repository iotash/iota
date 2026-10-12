//! A session that moves between providers (`iota resume -M other:model`, a `/model` move before the first
//! reply): what the persistence layer and the config layers do with it. Born as the probes of evaluation
//! `xprov-fable` (2026-10-09), which pinned the debt — a meta that kept naming the old provider, a writer
//! that silently dropped the new dialect's payload, a resume that replayed nothing and said nothing, layers
//! that looked ids up on the startup provider — and flipped when the move was made honest.

use crate::common::{log_lines, temp_store};
use iota::cmd::Declared;
use iota::provider::model::{JsonObject, Message, Raw, RawContent, ToolCall};
use iota::provider::{Provider, ProviderKind};
use iota::session::{
    ImageSettings, LayeredParams, Moved, NewSession, Overrides, Param, ParamSource, ParamSources,
    SESSION_SCHEMA_VERSION, SessionMeta, replay_session_settings,
};
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

/// A move to `provider:model` of type `kind`, with nothing else to record.
fn moved(kind: ProviderKind, provider: &str, model: &str) -> Moved {
    Moved {
        kind,
        provider: provider.to_owned(),
        model: model.to_owned(),
        params: LayeredParams::default(),
        window: None,
        image: ImageSettings::default(),
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
            "1 earlier reply carries anthropic-specific payload (reasoning, provider-side tool state) that openresponses does not replay; the model sees their text and tool calls only."
        )
    );
    w2.move_to(&moved(ProviderKind::OpenResponses, "resp", "gpt-x"))
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
    w.move_to(&moved(ProviderKind::OpenResponses, "resp", "gpt-x"))
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

/// The resume gate: a bundle recorded on `anthropic` resumed on another provider (`-M`) is a MOVE. The replay
/// still hands the provider nothing — not the model, not one knob — but that is no longer the end of it: the
/// move evaluates the bundle as a `/model` move evaluates the chat, so the effort the user set comes back on
/// `resp`, while the temperature and the window the old endpoint's config supplied stay behind. The warning
/// says exactly that, naming both endpoints.
#[test]
fn a_resume_on_another_provider_keeps_what_the_user_set_and_says_so() {
    let meta = SessionMeta {
        version: SESSION_SCHEMA_VERSION,
        provider: "anthropic".to_owned(),
        model: "claude-x".to_owned(),
        effort: "high".to_owned(),
        temperature: Some(0.3),
        context_window: 200_000,
        param_sources: Some(ParamSources {
            context_window: ParamSource::Config,
            effort: ParamSource::User,
            temperature: ParamSource::Config,
            top_p: ParamSource::Builtin,
            max_tokens: Some(ParamSource::Builtin),
        }),
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
    assert_eq!(t.effort(), None, "the replay itself applies nothing");
    assert_eq!(
        warnings,
        [
            "Warning: session was recorded on anthropic:claude-x; this run is on resp:gpt-x — its model is not replayed, and of its tuning only what was set by hand carries over, unless this run's config sets it"
        ]
    );

    // `resp`'s config is silent on all four: what the session runs under there.
    let kept = Declared::default().resume(&meta, false);
    assert_eq!(
        kept.effort,
        Param::user("high".to_owned()),
        "the user's own"
    );
    assert_eq!(kept.temperature, Param::default(), "anthropic's config's");
    assert_eq!(kept.context_window, Param::default(), "likewise");
    assert_eq!(kept.top_p, Param::default());
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

/// `SessionWriter::move_to` is ONE write: on an existing bundle whose meta cannot be written it fails and the
/// writer is exactly what it was — the meta in memory, and the type the next payload is tagged with — and once
/// the write goes through, the bundle is v2 and says everything the move carries.
#[test]
fn a_move_is_written_whole_or_not_at_all() {
    let (_home, store) = temp_store();
    let mut w = store
        .create(NewSession::new(ProviderKind::Anthropic, "claude-x"))
        .unwrap();
    w.append_messages(&[Message::user("q")]).unwrap();
    let dir = w.dir().to_path_buf();
    let before = w.meta().clone();

    std::fs::create_dir(dir.join(iota::session::META_TMP_FILE)).unwrap();
    let mut to = moved(ProviderKind::OpenResponses, "resp", "gpt-x");
    to.params.temperature = iota::session::Param::config(Some(0.2));
    assert!(w.move_to(&to).is_err());
    assert_eq!(*w.meta(), before, "nothing changed in memory");
    let c = call("call_1");
    w.append_messages(&[Message::assistant_with_calls(
        "",
        vec![c],
        Some(RawContent::OpenResponses(vec![
            raw(REASONING),
            raw(FUNCTION_CALL),
        ])),
    )])
    .unwrap_or_else(|e| {
        assert!(
            matches!(e, iota::session::SessionError::MetaNotSaved(_)),
            "{e}"
        );
    });
    assert!(
        !log_lines(&dir)[1].contains("raw"),
        "the writer still encodes for anthropic"
    );

    std::fs::remove_dir(dir.join(iota::session::META_TMP_FILE)).unwrap();
    w.move_to(&to).unwrap();
    let meta = SessionMeta::read(&dir).unwrap();
    assert_eq!(meta.version, SESSION_SCHEMA_VERSION);
    assert_eq!(
        (meta.provider.as_str(), meta.model.as_str()),
        ("resp", "gpt-x")
    );
    assert_eq!(meta.temperature, Some(0.2));
    assert!(meta.param_sources.is_some());
}
