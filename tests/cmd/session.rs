//! `--resume` end to end: the real `iota` binary resumes a bundle that the ORIGINAL implementation's writer
//! produced, drives one turn against `wiremock`, and appends it to that log.
//!
//! The fixtures under `tests/fixtures/sessions/` are a frozen corpus of the v1 on-disk format, written by the
//! original Go implementation (tag `go-final`) and verified through its loader; every id, prefix and expected
//! count is read from their `manifest.json`, so no random id is ever hardcoded here. The last two-implementation
//! round trip (that binary reading what this one wrote) is archived in `docs/history/ROUNDTRIP-FINAL.md`.
//!
//! Discipline (phase-1 bar): every child runs with a CLEARED environment (`common::cleared_env`) in a temp
//! working directory, HTTP goes to `wiremock`, and nothing reads or mutates this process's environment.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use crate::common::cleared_env;
use iota::cmd::Declared;
use iota::provider::ProviderKind;
use iota::provider::model::{RawContent, Role};
use iota::session::{
    Param, ParamSources, SESSION_SCHEMA_VERSION, SessionMeta, SessionRecord, SessionStore,
    SessionToolCall,
};
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::method};

// ---------------------------------------------------------------- the fixture corpus

/// The checked-in Go-written bundles.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sessions")
}

/// `manifest.json` — the only source of ids, prefixes and expected counts.
fn manifest() -> serde_json::Value {
    let raw = fs::read_to_string(fixtures_dir().join("manifest.json")).expect("read manifest.json");
    serde_json::from_str(&raw).expect("manifest.json is JSON")
}

/// One `fixtures[]` entry by name.
fn fixture(name: &str) -> serde_json::Value {
    manifest()["fixtures"]
        .as_array()
        .expect("fixtures array")
        .iter()
        .find(|f| f["name"] == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
        .clone()
}

/// A throwaway `$HOME` holding a private copy of every Go-written bundle at `<home>/.iota/sessions/`.
fn home_with_fixtures() -> TempDir {
    let home = TempDir::new().expect("temp home");
    let sessions = home.path().join(".iota").join("sessions");
    fs::create_dir_all(&sessions).expect("mkdir sessions");
    copy_dir(&fixtures_dir(), &sessions);
    home
}

/// Recursive directory copy (the fixtures are a handful of small files).
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// `fs::canonicalize` minus the `\\?\` verbatim prefix Windows adds to it — the spelling a plain
/// `current_dir()` reports back, and so the only one a run's own `project_root` can be compared with.
/// A no-op on unix, where the prefix does not exist.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        // `\\?\UNC\server\share` is not a drive path: dropping the prefix there would leave a RELATIVE
        // path, so only the `\\?\C:\…` form is unwrapped.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// `<home>/.iota/sessions`.
fn sessions_root(home: &Path) -> PathBuf {
    home.join(".iota").join("sessions")
}

// ---------------------------------------------------------------- the child process

/// `iota …` with a cleared environment (`common::cleared_env`), a temp cwd and the fixture home (the
/// `tests/cmd/cli.rs` discipline).
fn iota(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, home)
        .current_dir(cwd)
        .stdin(Stdio::null());
    cmd
}

/// Writes `<cwd>/.iota.yaml`: a gemini endpoint (its key, and the mock server's URL when one is serving)
/// driven by `agents.default`. `model` may be `""` — then the agent sets no `model:` and its choices are
/// the `p:*` wildcard, which is how a run reaches the resume stage with no model of its own and takes the
/// bundle's (D-52).
///
/// Since `-k` and `-u` were retired, a test points at its mock server the way a user points at an endpoint:
/// in the `providers:` layer (brain page `cli-surface-agent-first`).
fn write_config(cwd: &Path, url: &str, model: &str, workspace: bool) {
    let url = if url.is_empty() {
        String::new()
    } else {
        format!(", url: {url}")
    };
    let start = if model.is_empty() {
        "choices: [\"p:*\"]".to_owned()
    } else {
        format!("model: \"p:{model}\"")
    };
    let workspace = if workspace {
        "\n    workspace: true"
    } else {
        ""
    };
    fs::write(
        cwd.join(".iota.yaml"),
        format!(
            "providers:\n  p: {{type: gemini, key: x{url}}}\nagents:\n  default:\n    {start}{workspace}\n"
        ),
    )
    .expect("write config");
}

/// Runs `cmd` off the async runtime (wiremock needs its worker threads to keep serving).
async fn output(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.output().expect("run iota"))
        .await
        .expect("child join")
}

/// stdout as UTF-8.
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// stderr as UTF-8.
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Asserts the run failed with exit 1 and printed exactly `Error: {message}` (DIVERGENCES I-04).
fn assert_error(o: &Output, message: &str) {
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(o));
    assert_eq!(err(o), format!("Error: {message}\n"));
    assert!(out(o).is_empty(), "a failed run printed to stdout");
}

// ---------------------------------------------------------------- the model responder

/// The reply every stub returns.
const REPLY: &str = "the resumed reply";
/// `promptTokenCount` of the stub's usage block.
const IN_TOKENS: u64 = 12;
/// `candidatesTokenCount` of the stub's usage block.
const OUT_TOKENS: u64 = 4;
/// `totalTokenCount` of the stub's usage block.
const TOTAL_TOKENS: u64 = 16;
/// The bytes the image stub returns as an `inlineData` part.
const IMAGE_BYTES: &[u8] = b"PNG-FAKE-IMAGE";
/// `base64(IMAGE_BYTES)`.
const IMAGE_B64: &str = "UE5HLUZBS0UtSU1BR0U=";
/// `sha256(IMAGE_BYTES)`, lowercase hex — the `data_ref` the attachment store must derive.
const IMAGE_SHA256: &str = "e2bc867b9a8a1c5ff9aff0f61ac7fcfa4a95453913180f3de1d6f813983238ee";

/// A minimal Google responder (the shape the original fixture server answered with): one candidate carrying the
/// reply text — plus an `inlineData` image part when `image` is set — and a fixed usage block. It answers
/// `:generateContent` with the JSON body and `:streamGenerateContent` with the same body as one SSE event, so
/// the test does not depend on which of the two the run picks.
struct GoogleStub {
    image: bool,
}

impl GoogleStub {
    /// The `generateContent` body (also one stream chunk).
    fn body(&self) -> String {
        let image = if self.image {
            format!(r#",{{"inlineData":{{"mimeType":"image/png","data":"{IMAGE_B64}"}}}}"#)
        } else {
            String::new()
        };
        format!(
            r#"{{"candidates":[{{"content":{{"parts":[{{"text":"{REPLY}"}}{image}],"role":"model"}},"finishReason":"STOP"}}],"usageMetadata":{{"promptTokenCount":{IN_TOKENS},"candidatesTokenCount":{OUT_TOKENS},"totalTokenCount":{TOTAL_TOKENS}}}}}"#
        )
    }
}

impl Respond for GoogleStub {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let path = req.url.path().to_owned();
        if path.ends_with(":streamGenerateContent") {
            ResponseTemplate::new(200)
                .set_body_raw(format!("data: {}\n\n", self.body()), "text/event-stream")
        } else {
            ResponseTemplate::new(200).set_body_raw(self.body(), "application/json")
        }
    }
}

/// Mounts the responder on every POST the run can make.
async fn google_stub(server: &MockServer, image: bool) {
    Mock::given(method("POST"))
        .respond_with(GoogleStub { image })
        .mount(server)
        .await;
}

// ---------------------------------------------------------------- planting a bundle by hand

/// Writes a bundle (meta + log) at `dir` with a CHOSEN id, so a test can pin resolution order. The bytes go
/// through `iota_session`'s own serde types — the same writer shapes, just without the random id
/// `SessionStore::create` mints.
fn plant_bundle(dir: &Path, id: &str, provider: &str, model: &str, records: &[SessionRecord]) {
    fs::create_dir_all(dir.join("attachments")).expect("mkdir bundle");
    let mut log = String::new();
    for rec in records {
        log.push_str(&serde_json::to_string(rec).expect("record"));
        log.push('\n');
    }
    fs::write(dir.join("messages.jsonl"), log).expect("write log");
    let mut meta = SessionMeta {
        version: SESSION_SCHEMA_VERSION,
        id: id.to_owned(),
        created_at: iota::session::now_rfc3339(),
        updated_at: iota::session::now_rfc3339(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        message_count: i64::try_from(records.len()).expect("count"),
        ..SessionMeta::default()
    };
    meta.write(dir).expect("write meta");
}

/// `{role, content}` — the shape most planted records need.
fn rec(role: &str, content: &str) -> SessionRecord {
    SessionRecord {
        role: role.to_owned(),
        content: content.to_owned(),
        ..SessionRecord::default()
    }
}

// ---------------------------------------------------------------- tier A: the acceptance test

/// `SESSIONS_DESIGN` §8.2 tier A. The Rust binary resumes a bundle the REAL Go writer produced — by a unique
/// PREFIX and with NO `-M`, so both the resolver and the meta model replay are exercised — sends the resumed
/// history to the model, and appends exactly the new turn to Go's log without disturbing a byte of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_go_bundle_appends_one_turn() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let f = fixture("go-gemini-rich");
    let id = f["id"].as_str().expect("id").to_owned();
    let prefix = f["unique_prefix"].as_str().expect("prefix").to_owned();
    let view_count = f["expect"]["view_count"].as_u64().expect("view_count");
    let bundle = sessions_root(home.path()).join(f["dir"].as_str().expect("dir"));

    let log_before = fs::read(bundle.join("messages.jsonl")).expect("read log");
    let meta_before: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("meta.json")).expect("read meta"))
            .expect("meta is JSON");

    write_config(cwd.path(), &server.uri(), "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", &prefix, "-m", "second question"]);
    let o = output(cmd).await;

    // Exit 0, stdout is the reply ALONE, and the banner is the only thing on stderr (DIVERGENCES D-44).
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("{REPLY}\n"));
    assert_eq!(
        err(&o),
        format!("Resumed session {id} ({view_count} messages)\n")
    );

    // The BUNDLE drove the model: the resumed history precedes the new user message, and the session's own
    // system record became the system instruction (chat/run.go:69-74 — `-s` was not even given).
    let reqs = server.received_requests().await.expect("recorded requests");
    assert_eq!(reqs.len(), 1, "one unary turn");
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).expect("request is JSON");
    // The model came from meta, not from a flag (root.go:323-325).
    assert!(
        reqs[0].url.path().contains("gemini-2.5-pro"),
        "the meta model drives the URL: {}",
        reqs[0].url
    );
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
    let contents = body["contents"].as_array().expect("contents");
    // Five resumed conversation messages (the system record left for `systemInstruction`) + the new user turn.
    assert_eq!(contents.len(), 6);
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[0]["parts"][1]["text"], "hi");
    // The stored attachment was rehydrated out of `attachments/<sha256>` and replayed with it.
    assert_eq!(
        contents[0]["parts"][0]["inlineData"]["mimeType"],
        "text/plain"
    );
    assert_eq!(contents[0]["parts"][0]["inlineData"]["data"], "aGVsbG8=");
    assert_eq!(
        contents[1]["parts"][1]["functionCall"]["name"], "f",
        "the stored gemini raw blob was replayed verbatim"
    );
    assert_eq!(
        contents[1]["parts"][0]["thoughtSignature"], "AQID",
        "the blob's thought signature survived the round trip"
    );
    assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "f");
    assert_eq!(contents[3]["parts"][0]["text"], "done");
    assert_eq!(contents[4]["parts"][0]["text"], "cut");
    assert_eq!(contents[5]["role"], "user");
    assert_eq!(contents[5]["parts"][0]["text"], "second question");
    // The session's tuning was replayed onto the live provider (chat/session.go:422-455): no `-t` and no
    // `--context-window` were given, and effort has no flag at all, so meta's values are what went out.
    assert_eq!(body["generationConfig"]["temperature"], 0.7);
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "HIGH"
    );

    // The log GREW by exactly the turn: every pre-existing byte is untouched (lines are never rewritten).
    let log_after = fs::read(bundle.join("messages.jsonl")).expect("read log");
    assert!(
        log_after.starts_with(&log_before),
        "the Go-written lines were rewritten"
    );
    let appended = String::from_utf8(log_after[log_before.len()..].to_vec()).expect("utf-8");
    assert_eq!(
        appended,
        format!(
            "{{\"role\":\"user\",\"content\":\"second question\"}}\n\
             {{\"role\":\"assistant\",\"content\":\"{REPLY}\",\"usage\":{{\"in\":{IN_TOKENS},\"out\":{OUT_TOKENS},\"total\":{TOTAL_TOKENS}}}}}\n"
        ),
        "the appended lines are Go's own record shape (DIVERGENCES D-55: usage rides the assistant)"
    );

    // meta.json: `future_key` survived a Rust rewrite (D-46), the count grew by the delta, the stamp moved,
    // and NO key Go does not model was invented.
    let meta_after: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("meta.json")).expect("read meta"))
            .expect("meta is JSON");
    assert_eq!(meta_after["future_key"], meta_before["future_key"]);
    assert_eq!(
        meta_after["message_count"].as_i64().unwrap(),
        meta_before["message_count"].as_i64().unwrap() + 2
    );
    assert_ne!(meta_after["updated_at"], meta_before["updated_at"]);
    assert_eq!(meta_after["created_at"], meta_before["created_at"]);
    let keys_before: Vec<&String> = meta_before.as_object().unwrap().keys().collect();
    let keys_after: Vec<&String> = meta_after.as_object().unwrap().keys().collect();
    assert_eq!(
        keys_after, keys_before,
        "the rewrite neither dropped nor invented a meta key"
    );
    // The atomic write left no debris behind (D-45).
    assert!(!bundle.join("meta.json.tmp").exists());

    // And the extended bundle re-loads: the new turn is last, the pre-existing raw blob still restores, and
    // the cumulative usage is the whole log summed.
    let store = SessionStore::new(sessions_root(home.path()));
    let sess = store.load(&id, ProviderKind::Gemini).expect("reload");
    assert_eq!(sess.messages.len() as u64, view_count + 2);
    let last = sess.messages.last().unwrap();
    assert_eq!(last.role(), Role::Assistant);
    assert_eq!(last.content, REPLY);
    assert_eq!(last.usage().expect("usage persisted").output, OUT_TOKENS);
    assert_eq!(sess.messages[sess.messages.len() - 2].role(), Role::User);
    assert_eq!(
        sess.messages[sess.messages.len() - 2].content,
        "second question"
    );
    assert!(
        matches!(sess.messages[2].raw_content(), Some(RawContent::Google(_))),
        "the Go-written gemini blob survived the append"
    );
    assert!(
        sess.messages[5].interrupted(),
        "the interrupted flag survived"
    );
    let expect_in = f["expect"]["usage"]["in"].as_u64().unwrap();
    assert_eq!(sess.usage.input, expect_in + IN_TOKENS);
    assert_eq!(sess.meta.model, "gemini-2.5-pro");
}

/// cmd/root.go:323-333 (design §3.5): `-M` is the ONE thing that wins over the session's meta — it supplies
/// the model and does NOT rewrite `meta.model`. Everything else the bundle recorded replays, because the
/// flags that used to suppress temperature and the window (`-t`, `--context-window`) are gone: a resumed run
/// is the run it resumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_tuning_precedence_explicit_model_wins() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let f = fixture("go-gemini-rich");
    let bundle = sessions_root(home.path()).join(f["dir"].as_str().unwrap());

    write_config(cwd.path(), &server.uri(), "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args([
        "resume",
        f["unique_prefix"].as_str().unwrap(),
        "-M",
        "gemini-flash-latest",
        "-m",
        "second question",
    ]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));

    let reqs = server.received_requests().await.expect("requests");
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).expect("JSON");
    assert!(
        reqs[0].url.path().contains("gemini-flash-latest"),
        "-M wins over meta.model: {}",
        reqs[0].url
    );
    assert_eq!(
        body["generationConfig"]["temperature"], 0.7,
        "the session's temperature replays: no flag suppresses it any more"
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"], "HIGH",
        "effort has no flag, so the session's value still applies"
    );

    // root.go:323-325: Go never calls `SetModel` on the writer, so an explicit `-M` leaves meta.model alone.
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("meta.json")).expect("meta")).expect("JSON");
    assert_eq!(meta["model"], "gemini-2.5-pro");
    assert_eq!(meta["temperature"], 0.7);
    assert_eq!(meta["context_window"], 200_000);
}

/// A bundle written BEFORE the layering existed — one of the frozen Go-corpus fixtures, which carries a
/// temperature, a window and an effort and no `param_sources` key at all — resumes with every value intact
/// and every source read as the user's own, so a later `/model` switch onto a model that declares nothing
/// KEEPS them instead of dropping them (brain page `model-param-layering`).
///
/// This is the compatibility pin: the fixture is not regenerable, so the assertion is against a real file
/// the original implementation wrote, not against one this build could have shaped to suit itself.
#[test]
fn a_pre_layering_bundle_keeps_its_tuning() {
    let f = fixture("go-gemini-rich");
    let dir = fixtures_dir().join(f["dir"].as_str().unwrap());
    let raw = fs::read_to_string(dir.join("meta.json")).expect("read the fixture meta");
    assert!(
        !raw.contains("param_sources"),
        "the fixture must predate the key: {raw}"
    );

    let meta = SessionMeta::read(&dir).expect("decode the fixture meta");
    assert_eq!(meta.temperature, Some(0.7));
    assert_eq!(meta.context_window, 200_000);
    assert_eq!(meta.effort, "high");
    assert_eq!(meta.top_p, None, "the key did not exist yet");
    assert_eq!(meta.param_sources, None, "the key did not exist yet");
    assert_eq!(
        meta.sources(),
        ParamSources::LEGACY,
        "an absent key is the conservative reading: every value is the user's own"
    );
    assert!(
        !meta.records_params(),
        "so its silences are gaps, not choices"
    );

    // Resume: the three recorded values come back as they are, the one it never recorded is evaluated.
    let params = Declared::default().resume(&meta, true);
    assert_eq!(params.context_window, Param::user(200_000));
    assert_eq!(params.effort, Param::user("high".to_owned()));
    assert_eq!(params.temperature, Param::user(Some(0.7)));

    // ...and a model switch onto a model that declares nothing keeps all three.
    let after = Declared::default().evaluate(&params);
    assert_eq!(after.context_window, Param::user(200_000));
    assert_eq!(after.effort, Param::user("high".to_owned()));
    assert_eq!(after.temperature, Param::user(Some(0.7)));
}

// ---------------------------------------------------------------- resolution failures

/// chat/session.go:257,274 — an id fragment nothing matches, with Go's own text for THIS fixture set.
#[test]
fn resume_unknown_fragment() {
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let m = manifest();
    let fragment = m["resolution"]["unknown_fragment"].as_str().unwrap();
    let expected = m["resolution"]["unknown_error"].as_str().unwrap();
    write_config(cwd.path(), "", "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", fragment, "-m", "hi"]);
    assert_error(&cmd.output().expect("run"), expected);
}

/// chat/session.go:278 — a fragment several sessions share, candidates joined in LISTING order (`updated_at`
/// descending), byte-equal to what the Go binary prints for the same corpus.
#[test]
fn resume_ambiguous_fragment() {
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let m = manifest();
    let fragment = m["resolution"]["ambiguous_fragment"].as_str().unwrap();
    let expected = m["resolution"]["ambiguous_error"].as_str().unwrap();
    write_config(cwd.path(), "", "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", fragment, "-m", "hi"]);
    assert_error(&cmd.output().expect("run"), expected);
}

/// chat/session.go:208 — an exact id with no bundle behind it. Nothing is created for it either.
#[test]
fn resume_missing_bundle_is_not_found() {
    let cwd = TempDir::new().expect("temp cwd");
    let home = TempDir::new().expect("temp home");
    fs::create_dir_all(sessions_root(home.path())).expect("mkdir sessions");
    write_config(cwd.path(), "", "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", "nosuchsession", "-m", "hi"]);
    assert_error(
        &cmd.output().expect("run"),
        "no session matches \"nosuchsession\"",
    );
    assert!(!sessions_root(home.path()).join("nosuchsession").exists());
}

/// DIVERGENCES D-52: a bundle recorded under ANOTHER provider replays neither its model nor its tuning, so the
/// deferred `no model chosen …` is re-raised byte-identically (X-48 reworded Go's `--model/-M is required …`).
#[test]
fn resume_provider_mismatch_re_raises_model_required() {
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let f = fixture("go-openai-compaction"); // provider "openai", resumed here as gemini
    let prefix = f["unique_prefix"].as_str().unwrap();
    write_config(cwd.path(), "", "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", prefix, "-m", "hi"]);
    assert_error(
        &cmd.output().expect("run"),
        "no model chosen: set agents.default.model or pass -M",
    );
}

// ---------------------------------------------------------------- scoped (agent-mode) resolution

/// chat/session.go:288-305 through the CLI: agent mode resolves against the project's OWN bucket first, so a
/// fragment that is ambiguous across the flat root is unique inside it. The same fragment, the same corpus and
/// the same binary — only the agent's `workspace: true` differs (the `--agent` flag that used to say the same
/// thing is gone).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_agent_mode_prefers_the_project_bucket() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    // `.git` pins `project_root` to this directory, so the bucket slug is computable from here — but
    // only from the spelling the CHILD will see. `project_root` never canonicalises (agents/mod.rs), so
    // the run's own root is whatever `current_dir()` reports, and a temp path is not that spelling on
    // either platform: macOS resolves `/var` → `/private/var`, Windows hands out 8.3 components
    // (`RUNNER~1`). So the canonical root is what the child is GIVEN as its cwd, and the same value
    // computes the bucket — `strip_verbatim` because `fs::canonicalize` is the one call that adds a
    // `\\?\` prefix, which `current_dir()` would not report back.
    fs::create_dir_all(cwd.path().join(".git")).expect("mkdir .git");
    let root = strip_verbatim(&fs::canonicalize(cwd.path()).expect("canonical cwd"));
    let home = home_with_fixtures();
    let m = manifest();
    let fragment = m["resolution"]["ambiguous_fragment"].as_str().unwrap();

    // One bundle in THIS project's bucket whose id starts with the ambiguous fragment.
    let scoped_id = format!("{fragment}pkt1abcdefg");
    let bucket = sessions_root(home.path())
        .join("projects")
        .join(SessionStore::project_slug(&root));
    plant_bundle(
        &bucket.join(&scoped_id),
        &scoped_id,
        "gemini",
        "gemini-2.5-pro",
        &[rec("system", "scoped sys"), rec("user", "scoped question")],
    );

    // Without `workspace:` the flat view is the mode's own view: the fragment is ambiguous there (and the
    // scoped bundle is invisible to it).
    write_config(&root, &server.uri(), "", false);
    let mut cmd = iota(&root, home.path());
    cmd.args(["resume", fragment, "-m", "hi"]);
    assert_error(
        &cmd.output().expect("run"),
        m["resolution"]["ambiguous_error"].as_str().unwrap(),
    );

    // With it, the bucket answers first, unambiguously, and the turn lands in the bucketed bundle.
    write_config(&root, &server.uri(), "", true);
    let mut cmd = iota(&root, home.path());
    cmd.args(["resume", fragment, "-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        err(&o).contains(&format!("Resumed session {scoped_id} (2 messages)")),
        "stderr: {}",
        err(&o)
    );
    let store = SessionStore::new(sessions_root(home.path()));
    let sess = store
        .load(&scoped_id, ProviderKind::Gemini)
        .expect("reload");
    assert_eq!(sess.messages.len(), 4);
    assert_eq!(sess.messages.last().unwrap().content, REPLY);
}

/// The other half of chat/session.go:288-305: a `NoMatch` in the mode's own view WIDENS to every bucket, so an
/// explicit id keeps working from anywhere — here, a flat bundle resumed from inside an agent-mode project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_agent_mode_widens_to_the_flat_root() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    fs::create_dir_all(cwd.path().join(".git")).expect("mkdir .git");
    let home = home_with_fixtures();
    let f = fixture("go-gemini-rich");
    let id = f["id"].as_str().unwrap().to_owned();

    write_config(cwd.path(), &server.uri(), "", true);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", &id, "-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        err(&o).contains(&format!("Resumed session {id} (")),
        "stderr: {}",
        err(&o)
    );
}

// ---------------------------------------------------------------- awkward bundle shapes

/// DIVERGENCES D-43: Go's interrupt path can leave a log whose last records are TOOL RESULTS. That bundle is
/// never written headlessly, but it must still load and produce a valid request — the loader and the request
/// builder both have to accept a history that ends mid tool round.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_a_log_ending_in_tool_results() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = TempDir::new().expect("temp home");
    let id = "toolt41ended";
    plant_bundle(
        &sessions_root(home.path()).join(id),
        id,
        "gemini",
        "gemini-2.5-pro",
        &[
            rec("system", "sys"),
            rec("user", "call the tool"),
            SessionRecord {
                role: "assistant".to_owned(),
                tool_calls: vec![SessionToolCall {
                    id: "c9".to_owned(),
                    name: "f".to_owned(),
                    arguments: serde_json::Map::new(),
                }],
                ..SessionRecord::default()
            },
            SessionRecord {
                role: "tool".to_owned(),
                content: "tool said ok".to_owned(),
                tool_call_id: "c9".to_owned(),
                tool_call_name: "f".to_owned(),
                ..SessionRecord::default()
            },
        ],
    );

    write_config(cwd.path(), &server.uri(), "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["resume", id, "-m", "carry on"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(err(&o), format!("Resumed session {id} (4 messages)\n"));

    let reqs = server.received_requests().await.expect("requests");
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).expect("JSON");
    let contents = body["contents"].as_array().expect("contents");
    // user / model(functionCall) / user(functionResponse + the new text) — the dangling round replayed intact,
    // with the dialect's own merge of the trailing tool result into the new user turn.
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "f");
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["name"], "f",
        "the trailing tool result reached the request"
    );
    assert_eq!(contents[2]["parts"][1]["text"], "carry on");
}

/// A run that FAILS persists nothing (DIVERGENCES D-43): the log and the meta are byte-identical afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_resumed_turn_persists_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let f = fixture("go-gemini-rich");
    let bundle = sessions_root(home.path()).join(f["dir"].as_str().unwrap());
    let log_before = fs::read(bundle.join("messages.jsonl")).expect("log");
    let meta_before = fs::read(bundle.join("meta.json")).expect("meta");

    write_config(cwd.path(), &server.uri(), "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args([
        "resume",
        f["unique_prefix"].as_str().unwrap(),
        "-m",
        "second question",
    ]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(1), "stderr: {}", err(&o));
    assert_eq!(
        fs::read(bundle.join("messages.jsonl")).expect("log"),
        log_before
    );
    assert_eq!(
        fs::read(bundle.join("meta.json")).expect("meta"),
        meta_before
    );
}

// ---------------------------------------------------------------- generated images (D-53)

/// DIVERGENCES D-53: a resumed run saves its generated images INSIDE the bundle and persists exactly
/// `collectImages`' subset — the images whose save SUCCEEDED, each `filename` rewritten to the saved file's
/// basename, the bytes content-addressed under `attachments/`. The persisted record is pinned as a golden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_image_turn_persists_the_saved_attachment() {
    let server = MockServer::start().await;
    google_stub(&server, true).await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = home_with_fixtures();
    let f = fixture("go-gemini-rich");
    let id = f["id"].as_str().unwrap().to_owned();
    let bundle = sessions_root(home.path()).join(f["dir"].as_str().unwrap());
    let log_before = fs::read(bundle.join("messages.jsonl")).expect("log");

    write_config(cwd.path(), &server.uri(), "", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args([
        "resume",
        f["unique_prefix"].as_str().unwrap(),
        "-m",
        "draw something",
    ]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));

    // The image landed in the BUNDLE's images/, not in ~/.iota/images.
    assert!(
        !home.path().join(".iota").join("images").exists(),
        "a resumed run must not fall back to the global images directory"
    );
    let images: Vec<PathBuf> = fs::read_dir(bundle.join("images"))
        .expect("images dir")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(images.len(), 1, "one saved image");
    let saved = &images[0];
    assert_eq!(fs::read(saved).expect("image bytes"), IMAGE_BYTES);
    let basename = saved
        .file_name()
        .and_then(|n| n.to_str())
        .expect("basename")
        .to_owned();
    assert!(
        basename.ends_with("-0.png"),
        "images.go:52 name: {basename}"
    );
    assert_eq!(out(&o), format!("{REPLY}\n🖼 saved: {}\n", saved.display()));

    // The persisted assistant record — the golden (images.go:131: `filename` is the SAVED file's basename,
    // the mime and bytes are the generated attachment's own).
    let appended = String::from_utf8(
        fs::read(bundle.join("messages.jsonl")).expect("log")[log_before.len()..].to_vec(),
    )
    .expect("utf-8");
    let assistant_line = appended.lines().last().expect("assistant line");
    assert_eq!(
        assistant_line,
        format!(
            "{{\"role\":\"assistant\",\"content\":\"{REPLY}\",\
             \"attachments\":[{{\"filename\":\"{basename}\",\"mime\":\"image/png\",\
             \"data_ref\":\"sha256:{IMAGE_SHA256}\"}}],\
             \"usage\":{{\"in\":{IN_TOKENS},\"out\":{OUT_TOKENS},\"total\":{TOTAL_TOKENS}}}}}"
        )
    );
    // The bytes are content-addressed beside the log, so a later resume can still read them.
    assert_eq!(
        fs::read(bundle.join("attachments").join(IMAGE_SHA256)).expect("blob"),
        IMAGE_BYTES
    );

    // And they come back as an attachment on the reloaded message.
    let store = SessionStore::new(sessions_root(home.path()));
    let sess = store.load(&id, ProviderKind::Gemini).expect("reload");
    let last = sess.messages.last().unwrap();
    assert_eq!(last.attachments.len(), 1);
    assert_eq!(last.attachments[0].filename, basename);
    assert_eq!(last.attachments[0].mime_type, "image/png");
    assert_eq!(last.attachments[0].data, IMAGE_BYTES);
}

// ---------------------------------------------------------------- the stateless path is untouched

/// DIVERGENCES D-41 / design §3.1: Go's `-m` branch returns before any session code, so a bare `-m` run
/// persists NOTHING. Pinned here because `--resume` is the first headless code that could break it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_m_run_writes_nothing_under_home() {
    let server = MockServer::start().await;
    google_stub(&server, false).await;
    let cwd = TempDir::new().expect("temp cwd");
    let home = TempDir::new().expect("temp home");

    write_config(cwd.path(), &server.uri(), "gemini-2.5-pro", false);
    let mut cmd = iota(cwd.path(), home.path());
    cmd.args(["-m", "hi"]);
    let o = output(cmd).await;
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("{REPLY}\n"));
    assert!(
        !home.path().join(".iota").exists(),
        "a stateless -m run created {}",
        home.path().join(".iota").display()
    );
}
