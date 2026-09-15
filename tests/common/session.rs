//! Shared fixtures of the session test binary: the capability-less provider of the "mismatch applies
//! nothing" legs, plus the bundle fabricator that makes locator/scope tests deterministic.
//!
//! Nothing here reads or mutates the process environment: every store is rooted in a `tempfile::TempDir`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use iota::BoxFuture;
use iota::provider::error::ProviderError;
use iota::provider::model::Message;
use iota::provider::{CancellationToken, ChatResult, Provider, ProviderKind};
use iota::session::{SESSION_SCHEMA_VERSION, SessionMeta, SessionStore, now_rfc3339};

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
