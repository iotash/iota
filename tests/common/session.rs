//! Shared fixtures of the `iota-session` test crates (WP-S1-owned): the Go tests' `stubProvider` family
//! (`chat/session_test.go:15-36`, `:264-275`, `:561-610`) as ONE configurable provider, plus the bundle
//! fabricator `chat/session_project_test.go:17-48` uses to make locator/scope tests deterministic.
//!
//! Nothing here reads or mutates the process environment: every store is rooted in a `tempfile::TempDir`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use iota::BoxFuture;
use iota::provider::error::ProviderError;
use iota::provider::model::Message;
use iota::provider::{
    CancellationToken, ChatResult, Effort, ImageEditJsonTunable, ImageGenOptions, ImageGenParams,
    ImageTunable, Provider, ProviderKind, Tunable,
};
use iota::session::{SESSION_SCHEMA_VERSION, SessionMeta, SessionStore, now_rfc3339};

/// `stubProvider` + `tunableStub` + `imageGenStub` + `jsonEditStub` in one: a provider that implements
/// every optional capability `apply_session_tuning` reaches for and records what it was given.
///
/// `kind` is settable so a test can load a bundle "under a different provider type" — Go's `otherType`
/// wrapper (`chat/session_test.go:362-364`).
#[derive(Debug)]
pub struct StubProvider {
    pub kind: ProviderKind,
    pub model: String,
    pub temperature: Option<f64>,
    pub effort: Option<Effort>,
    pub image_output: bool,
    pub image_gen: ImageGenParams,
    pub json_edits: bool,
}

impl StubProvider {
    /// A stub of `kind` with model `m1` and no tuning applied yet.
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            model: "m1".to_owned(),
            temperature: None,
            effort: None,
            image_output: false,
            image_gen: ImageGenParams::default(),
            json_edits: false,
        }
    }
}

impl Provider for StubProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async { Ok(vec!["m1".to_owned()]) })
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async { Ok(ChatResult::default()) })
    }

    fn as_tunable(&mut self) -> Option<&mut dyn Tunable> {
        Some(self)
    }

    fn as_image_tunable(&mut self) -> Option<&mut dyn ImageTunable> {
        Some(self)
    }

    fn as_image_gen_tunable(&mut self) -> Option<&mut dyn iota::provider::ImageGenTunable> {
        Some(self)
    }

    fn as_image_edit_json_tunable(&mut self) -> Option<&mut dyn ImageEditJsonTunable> {
        Some(self)
    }
}

impl Tunable for StubProvider {
    fn set_temperature(&mut self, t: Option<f64>) {
        self.temperature = t;
    }

    fn temperature(&self) -> Option<f64> {
        self.temperature
    }

    fn set_effort(&mut self, e: Option<Effort>) {
        self.effort = e;
    }

    fn effort(&self) -> Option<Effort> {
        self.effort
    }
}

impl ImageTunable for StubProvider {
    fn set_image_output(&mut self, on: bool) {
        self.image_output = on;
    }

    fn image_output(&self) -> bool {
        self.image_output
    }
}

impl iota::provider::ImageGenTunable for StubProvider {
    fn set_image_gen_params(&mut self, p: ImageGenParams) {
        self.image_gen = p;
    }

    fn image_gen_params(&self) -> &ImageGenParams {
        &self.image_gen
    }

    fn image_gen_options(&self) -> ImageGenOptions {
        ImageGenOptions::default()
    }
}

impl ImageEditJsonTunable for StubProvider {
    fn set_json_edits(&mut self, on: bool) {
        self.json_edits = on;
    }

    fn json_edits(&self) -> bool {
        self.json_edits
    }
}

/// A provider with NO optional capability at all, for the "mismatch applies nothing" legs.
#[derive(Debug)]
pub struct PlainProvider(pub ProviderKind);

impl Provider for PlainProvider {
    fn kind(&self) -> ProviderKind {
        self.0
    }

    fn model(&self) -> &'static str {
        "m1"
    }

    fn set_model(&mut self, _model: String) {}

    fn list_models<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn chat<'a>(
        &'a self,
        _cancel: &'a CancellationToken,
        _messages: &'a [Message],
    ) -> BoxFuture<'a, Result<ChatResult, ProviderError>> {
        Box::pin(async { Ok(ChatResult::default()) })
    }
}

/// A store rooted at `<temp>/.iota/sessions`, mirroring what `HostDirs` would resolve — the tests own the
/// directory, so nothing touches `$HOME`.
pub fn temp_store() -> (tempfile::TempDir, SessionStore) {
    let home = tempfile::tempdir().expect("temp dir");
    let store = SessionStore::new(home.path().join(".iota").join("sessions"));
    (home, store)
}

/// The on-disk bucket for a project root under a sessions root.
pub fn bucket_dir(root: &Path, project_root: &str) -> PathBuf {
    root.join("projects")
        .join(SessionStore::project_slug(Path::new(project_root)))
}

/// `writeBundle` (chat/session_project_test.go:17-43): fabricates a bundle with a chosen id and a
/// one-line log, so locator/scope tests are deterministic. `cwd == ""` mimics a bundle written before the
/// field existed.
pub fn write_bundle(dir: &Path, id: &str, cwd: &str) {
    std::fs::create_dir_all(dir).expect("mkdir bundle");
    let now = now_rfc3339();
    let meta = SessionMeta {
        version: SESSION_SCHEMA_VERSION,
        id: id.to_owned(),
        created_at: now.clone(),
        updated_at: now,
        provider: "openai".to_owned(),
        model: "m1".to_owned(),
        cwd: cwd.to_owned(),
        message_count: 1,
        ..SessionMeta::default()
    };
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_vec(&meta).expect("marshal meta"),
    )
    .expect("write meta");
    std::fs::write(
        dir.join("messages.jsonl"),
        "{\"role\":\"user\",\"content\":\"hi\"}\n",
    )
    .expect("write log");
}

/// Every line of a bundle's `messages.jsonl`, without the trailing newline.
pub fn log_lines(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("messages.jsonl")).expect("read log");
    text.lines().map(str::to_owned).collect()
}
