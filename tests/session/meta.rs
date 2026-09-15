//! `meta.json`: the tuning round trip, unknown-key preservation, legacy bundles and the atomic rewrite.

use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::session::{META_FILE, META_TMP_FILE, NewSession, ParamSource, ParamSources, SessionMeta};
use pretty_assertions::assert_eq;

use crate::common::temp_store;

const KIND: ProviderKind = ProviderKind::OpenAi;
#[test]
fn session_meta_tuning_round_trip() {
    let (_home, store) = temp_store();
    let mut writer = store.create(NewSession::new(KIND, "m1")).unwrap();
    let id = writer.id().to_owned();

    // Setters before the bundle exists are flushed by the first append.
    writer
        .update_meta(|meta| {
            meta.temperature = Some(0.7);
            meta.effort = "high".to_owned();
            meta.context_window = 200_000;
        })
        .unwrap();
    writer.append_messages(&[Message::user("hi")]).unwrap();
    drop(writer);

    let sess = store.load(&id, KIND).unwrap();
    assert_eq!(sess.meta.temperature, Some(0.7));
    assert_eq!(sess.meta.effort, "high");
    assert_eq!(sess.meta.context_window, 200_000);

    // Setters on a resumed (already-created) session write through immediately; a None temperature
    // drops the field.
    let (mut writer, _) = store.resume(&id, KIND).unwrap();
    writer
        .update_meta(|meta| {
            meta.temperature = None;
            meta.effort = "max".to_owned();
            meta.context_window = 32_000;
        })
        .unwrap();
    drop(writer);

    let sess = store.load(&id, KIND).unwrap();
    assert_eq!(sess.meta.temperature, None, "temperature not cleared");
    assert_eq!(sess.meta.effort, "max");
    assert_eq!(sess.meta.context_window, 32_000);
    let text = std::fs::read_to_string(store.dir(&id).unwrap().join(META_FILE)).unwrap();
    assert!(
        !text.contains("temperature"),
        "an unset temperature must be omitted: {text}"
    );
}

/// A key a future Go build wrote survives a Rust rewrite (D-46), and Rust never invents one of its own.
#[test]
fn unknown_meta_keys_survive_a_rust_rewrite() {
    let (_home, store) = temp_store();
    let mut writer = store.create(NewSession::new(KIND, "m1")).unwrap();
    let id = writer.id().to_owned();
    writer.append_messages(&[Message::user("hi")]).unwrap();
    let dir = writer.dir().to_path_buf();
    drop(writer);

    // Inject the unknown member textually, exactly as the Go fixture generator does, so the file stays
    // byte-authentic writer output.
    let text = std::fs::read_to_string(dir.join(META_FILE)).unwrap();
    let injected = text.replace("\n}", ",\n  \"future_key\": {\n    \"kept\": true\n  }\n}");
    std::fs::write(dir.join(META_FILE), &injected).unwrap();

    let meta = SessionMeta::read(&dir).unwrap();
    assert_eq!(
        meta.extra.get("future_key"),
        Some(&serde_json::json!({"kept": true})),
        "unknown key not captured"
    );
    assert_eq!(meta.extra.len(), 1, "only the unknown key lands in `extra`");

    // A Rust rewrite (another append) keeps it.
    let (mut writer, _) = store.resume(&id, KIND).unwrap();
    writer.append_messages(&[Message::assistant("ok")]).unwrap();
    drop(writer);

    let after = SessionMeta::read(&dir).unwrap();
    assert_eq!(
        after.extra.get("future_key"),
        Some(&serde_json::json!({"kept": true}))
    );
    assert_eq!(after.message_count, 2);
    // The key set is Go's modelled keys plus the pre-existing unknown one, nothing invented.
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(META_FILE)).unwrap()).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "created_at",
            "future_key",
            "id",
            "message_count",
            "model",
            "provider",
            "updated_at",
            "v",
        ]
    );
}

/// `param_sources:` and `top_p:` — the layering's half of the meta (brain page `model-param-layering`):
/// both are omitted from a bundle that has nothing to say, both survive a rewrite, and a bundle without the
/// sources key reads back as the user's own so a later model switch keeps its values.
#[test]
fn meta_records_where_each_layered_parameter_came_from() {
    let (_home, store) = temp_store();
    let mut writer = store.create(NewSession::new(KIND, "m")).unwrap();
    let id = writer.id().to_owned();
    writer.append_messages(&[Message::user("hi")]).unwrap();
    let dir = writer.dir().to_path_buf();

    // Nothing layered: the file keeps the byte shape every bundle written before the key had.
    let text = std::fs::read_to_string(dir.join(META_FILE)).unwrap();
    assert!(!text.contains("param_sources"), "{text}");
    assert!(!text.contains("top_p"), "{text}");
    // ...and it reads back as the conservative legacy answer, which is what that absence MEANS.
    let legacy = SessionMeta::read(&dir).unwrap();
    assert_eq!(legacy.param_sources, None, "the key is not there at all");
    assert_eq!(legacy.sources(), ParamSources::LEGACY);
    assert_eq!(legacy.sources().effort, ParamSource::User);
    assert_eq!(legacy.top_p, None);

    // A session that ran under declarations records them, and the pair round-trips through a rewrite.
    writer
        .update_meta(|m| {
            m.top_p = Some(0.9);
            m.effort = "high".to_owned();
            m.param_sources = Some(ParamSources {
                context_window: ParamSource::Builtin,
                effort: ParamSource::Config,
                temperature: ParamSource::Builtin,
                top_p: ParamSource::Config,
            });
        })
        .unwrap();
    drop(writer);
    let text = std::fs::read_to_string(dir.join(META_FILE)).unwrap();
    assert!(text.contains("\"top_p\": 0.9"), "{text}");
    assert!(text.contains("\"effort\": \"config\""), "{text}");

    let (mut writer, _) = store.resume(&id, KIND).unwrap();
    writer.append_messages(&[Message::assistant("ok")]).unwrap();
    drop(writer);
    let after = store.load(&id, KIND).unwrap().meta;
    assert_eq!(after.top_p, Some(0.9));
    assert_eq!(after.sources().effort, ParamSource::Config);
    assert_eq!(after.sources().context_window, ParamSource::Builtin);
}

/// `agent:` records how a session was STARTED. It is omitted when the run named no agent — so every bundle
/// written before the three-layer config keeps the byte shape it had — and it survives a rewrite.
#[test]
fn meta_records_the_agent_the_session_ran_under() {
    let (_home, store) = temp_store();

    // No agent: the key is absent from the file, exactly as it was before the key existed.
    let mut writer = store.create(NewSession::new(KIND, "m")).unwrap();
    writer
        .append_messages(&[Message::user("hi".to_owned())])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    let text = std::fs::read_to_string(dir.join(META_FILE)).unwrap();
    assert!(!text.contains("\"agent\""), "{text}");
    assert_eq!(SessionMeta::read(&dir).unwrap().agent, "");

    // Under an agent: recorded, and still there after the meta is rewritten.
    let mut writer = store
        .create(NewSession {
            agent: "reviewer".to_owned(),
            ..NewSession::new(KIND, "m")
        })
        .unwrap();
    writer
        .append_messages(&[Message::user("hi".to_owned())])
        .unwrap();
    let dir = writer.dir().to_path_buf();
    assert_eq!(SessionMeta::read(&dir).unwrap().agent, "reviewer");
    writer
        .append_messages(&[Message::user("again".to_owned())])
        .unwrap();
    assert_eq!(SessionMeta::read(&dir).unwrap().agent, "reviewer");
}

/// A resumed bundle whose agent has been deleted from the config says so once, and the run carries on with
/// the provider and model the meta holds — the behaviour every session had before the key existed.
#[test]
fn a_deleted_session_agent_is_announced_once() {
    let mut meta = SessionMeta {
        agent: "reviewer".to_owned(),
        ..SessionMeta::default()
    };

    let mut warnings = Vec::new();
    iota::session::warn_if_session_agent_is_gone(&meta, false, &mut |w| warnings.push(w));
    assert_eq!(
        warnings,
        vec![
            "Warning: session agent \"reviewer\" is no longer configured; using the provider and model from the session"
                .to_owned()
        ]
    );

    // Still configured: nothing to say.
    let mut warnings = Vec::new();
    iota::session::warn_if_session_agent_is_gone(&meta, true, &mut |w| warnings.push(w));
    assert!(warnings.is_empty(), "{warnings:?}");

    // A bundle that names no agent is silent either way.
    meta.agent.clear();
    let mut warnings = Vec::new();
    iota::session::warn_if_session_agent_is_gone(&meta, false, &mut |w| warnings.push(w));
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// The minimal legacy bundle Go can have written loads with everything else defaulted.
#[test]
fn legacy_meta_loads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(META_FILE), br#"{"v":1,"id":"x"}"#).unwrap();
    let meta = SessionMeta::read(dir.path()).unwrap();
    assert_eq!(meta.version, 1);
    assert_eq!(meta.id, "x");
    assert_eq!(meta.message_count, 0);
    assert_eq!(meta.provider, "");
    assert_eq!(meta.temperature, None);
    assert!(meta.extra.is_empty());
    // An entirely empty object is legal too.
    std::fs::write(dir.path().join(META_FILE), b"{}").unwrap();
    assert_eq!(
        SessionMeta::read(dir.path()).unwrap(),
        SessionMeta::default()
    );
}

/// The rewrite is temp+rename (D-45) and leaves no stray file behind on success.
#[test]
fn meta_write_is_atomic_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut meta = SessionMeta {
        version: 1,
        id: "k7qz3xv9m2ht".to_owned(),
        ..SessionMeta::default()
    };
    meta.write(dir.path()).unwrap();
    assert!(dir.path().join(META_FILE).is_file());
    assert!(
        !dir.path().join(META_TMP_FILE).exists(),
        "the temp file must be renamed away"
    );
    // `updated_at` is restamped by every write.
    assert!(!meta.updated_at.is_empty());
    let first = meta.updated_at.clone();
    meta.write(dir.path()).unwrap();
    assert!(meta.updated_at >= first);
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, [META_FILE]);
}
