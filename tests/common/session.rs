//! Shared fixtures of the session test binary: a temp store and the bundle fabricator that makes
//! locator/scope tests deterministic.
//!
//! Nothing here reads or mutates the process environment: every store is rooted in a `tempfile::TempDir`.

use std::path::{Path, PathBuf};

use iota::session::{SESSION_SCHEMA_VERSION, SessionMeta, SessionStore, now_rfc3339};

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
