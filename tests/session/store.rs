//! The sessions root: slugs, the two layouts, id minting and collision checks, the mode-isolated listing
//! views and `--resume` fragment resolution (`chat/session_test.go`, `chat/session_project_test.go`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use iota::app::HostDirs;
use iota::provider::ProviderKind;
use iota::provider::model::Message;
use iota::session::{
    PROJECTS_DIR_NAME, SESSION_ID_ALPHABET, SESSION_ID_LENGTH, SessionError, SessionInfo,
    SessionStore, resolve_in,
};
use pretty_assertions::assert_eq;

use crate::common::{bucket_dir, temp_store, write_bundle};

const KIND: ProviderKind = ProviderKind::OpenAi;

fn info(id: &str) -> SessionInfo {
    SessionInfo {
        id: id.to_owned(),
        title: String::new(),
        model: String::new(),
        provider: String::new(),
        updated_at: None,
        message_count: 0,
    }
}

// Go: chat/session_project_test.go:50
#[test]
fn project_slug() {
    for (root, want) in [
        ("/Users/x/proj", "-Users-x-proj"),
        ("/Users/x/proj/", "-Users-x-proj"), // cleaned before encoding
        ("/Users/x/my-app", "-Users-x-my-app"),
        ("/", "-"),
    ] {
        assert_eq!(
            SessionStore::project_slug(Path::new(root)),
            want,
            "project_slug({root:?})"
        );
    }
}

// Go: chat/session_test.go:465
#[test]
fn new_session_id() {
    let (_home, store) = temp_store();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..100 {
        let id = store.new_id();
        assert_eq!(id.len(), SESSION_ID_LENGTH, "id {id:?}");
        assert!(
            id.bytes().all(|b| SESSION_ID_ALPHABET.contains(&b)),
            "id {id:?} contains a symbol outside the alphabet"
        );
        assert!(
            seen.insert(id.clone()),
            "duplicate id {id:?} in a small batch"
        );
    }
}

// Go: chat/session_test.go:485
#[test]
fn resolve_session_id() {
    let infos = [
        info("k7qz3xv9m2ht"),
        info("k7ab00000000"),
        info("01JZXK7QZTXA9WBGVE3M8YC5DN"), // upper-case id: matching is case-insensitive
    ];

    // Exact match wins.
    assert_eq!(resolve_in(&infos, "k7qz3xv9m2ht").unwrap(), "k7qz3xv9m2ht");
    // Unique prefix resolves.
    assert_eq!(resolve_in(&infos, "k7q").unwrap(), "k7qz3xv9m2ht");
    // Prefix matching is case-insensitive.
    assert_eq!(
        resolve_in(&infos, "01jzxk").unwrap(),
        "01JZXK7QZTXA9WBGVE3M8YC5DN"
    );
    // Ambiguous prefix errors and names the candidates in listing order.
    let err = resolve_in(&infos, "k7").unwrap_err();
    assert_eq!(
        err.to_string(),
        "session id \"k7\" is ambiguous: k7qz3xv9m2ht, k7ab00000000"
    );
    assert!(matches!(err, SessionError::Ambiguous(..)));
    // Unknown fragment errors.
    let err = resolve_in(&infos, "zzz").unwrap_err();
    assert_eq!(err.to_string(), "no session matches \"zzz\"");
    assert!(matches!(err, SessionError::NoMatch(_)));
    // An empty fragment is a prefix of everything (Go's `strings.HasPrefix(x, "")`).
    assert!(matches!(
        resolve_in(&infos, "").unwrap_err(),
        SessionError::Ambiguous(..)
    ));
    assert!(matches!(
        resolve_in(&[], "anything").unwrap_err(),
        SessionError::NoMatch(_)
    ));
}

// Go: chat/session_project_test.go:66
#[test]
fn project_session_writer() {
    let (_home, store) = temp_store();
    let root = "/work/myproj";

    let mut writer = store.create(KIND, "m1", None, "", root, true, "").unwrap();
    writer.append_messages(&[Message::user("hi")]).unwrap();
    let id = writer.id().to_owned();
    drop(writer);

    let want = bucket_dir(store.root(), root).join(&id);
    assert!(
        want.join("meta.json").is_file(),
        "bundle not in project bucket {want:?}"
    );
    let sess = store.load(&id, KIND).unwrap();
    assert_eq!(sess.meta.cwd, root, "meta cwd");

    // Normal mode: flat layout, cwd recorded anyway.
    let mut flat_writer = store
        .create(KIND, "m1", None, "", "/somewhere/else", false, "")
        .unwrap();
    flat_writer.append_messages(&[Message::user("hi")]).unwrap();
    let flat_id = flat_writer.id().to_owned();
    drop(flat_writer);
    assert!(store.root().join(&flat_id).join("meta.json").is_file());
    assert_eq!(
        store.load(&flat_id, KIND).unwrap().meta.cwd,
        "/somewhere/else"
    );

    // `project` without a cwd stays flat too (Go: `project && cwd != ""`).
    let no_cwd = store.create(KIND, "m1", None, "", "", true, "").unwrap();
    assert_eq!(no_cwd.dir().parent(), Some(store.root()));
}

// Go: chat/session_project_test.go:117
#[test]
fn session_locator_across_layouts() {
    let (_home, store) = temp_store();
    let root = "/work/p1";
    let flat_id = "aaaa00000000";
    let bucket_id = "aaab00000000";
    write_bundle(&store.root().join(flat_id), flat_id, "/elsewhere");
    write_bundle(
        &bucket_dir(store.root(), root).join(bucket_id),
        bucket_id,
        root,
    );

    // Resume finds both layouts.
    for id in [flat_id, bucket_id] {
        let (_writer, sess) = store.resume(id, KIND).unwrap();
        assert_eq!(sess.messages.len(), 1, "resume({id})");
    }

    // A prefix with no flat match widens to the buckets.
    assert_eq!(store.resolve_id("aaab", None).unwrap(), bucket_id);
    // Mode-first: "aaa" is ambiguous across layouts but UNIQUE within the flat view.
    assert_eq!(store.resolve_id("aaa", None).unwrap(), flat_id);
    // ...and unique within the project bucket (project-first resolution).
    assert_eq!(
        store.resolve_id("aaa", Some(Path::new(root))).unwrap(),
        bucket_id
    );
    // An explicit id outside the bucket still resolves via the global fallback.
    assert_eq!(
        store.resolve_id(flat_id, Some(Path::new(root))).unwrap(),
        flat_id
    );
    // Unknown fragments still error.
    assert_eq!(
        store
            .resolve_id("zzz", Some(Path::new(root)))
            .unwrap_err()
            .to_string(),
        "no session matches \"zzz\""
    );
    // An id that is on disk nowhere is NotFound, not a resolution failure.
    assert_eq!(
        store.dir("nope00000000").unwrap_err().to_string(),
        "session nope00000000 not found"
    );
}

/// Candidate order (and therefore the `Ambiguous` text) is deterministic: the listing views read
/// directories name-sorted, like Go's `os.ReadDir`, and the `updated_at` sort is stable.
#[test]
fn ambiguity_candidates_are_listed_deterministically() {
    let (_home, store) = temp_store();
    let stamped = "2026-01-01T00:00:00+00:00";
    for id in ["ccc300000000", "ccc100000000", "ccc200000000"] {
        let dir = store.root().join(id);
        write_bundle(&dir, id, "");
        let mut meta = iota::session::SessionMeta::read(&dir).unwrap();
        meta.updated_at = stamped.to_owned();
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    }
    assert_eq!(
        store.resolve_id("ccc", None).unwrap_err().to_string(),
        "session id \"ccc\" is ambiguous: ccc100000000, ccc200000000, ccc300000000"
    );
}

/// An ambiguity inside the mode's own view is FINAL: only a no-match widens (chat/session.go:297-299).
#[test]
fn ambiguity_in_the_scoped_view_is_final() {
    let (_home, store) = temp_store();
    let root = "/work/p1";
    let bucket = bucket_dir(store.root(), root);
    write_bundle(&bucket.join("aaaa00000000"), "aaaa00000000", root);
    write_bundle(&bucket.join("aaab00000000"), "aaab00000000", root);
    write_bundle(&store.root().join("bbbb00000000"), "bbbb00000000", "");

    let err = store
        .resolve_id("aaa", Some(Path::new(root)))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("session id \"aaa\" is ambiguous: "),
        "got {err}"
    );
    assert!(err.contains("aaaa00000000") && err.contains("aaab00000000"));
}

// Go: chat/session_project_test.go:171
#[test]
fn list_sessions_scoped() {
    let (_home, store) = temp_store();
    let (root1, root2) = ("/work/p1", "/work/p2");
    write_bundle(
        &store.root().join("aaaa00000000"),
        "aaaa00000000",
        "/elsewhere",
    );
    write_bundle(
        &bucket_dir(store.root(), root1).join("bbbb00000000"),
        "bbbb00000000",
        root1,
    );
    write_bundle(
        &bucket_dir(store.root(), root2).join("cccc00000000"),
        "cccc00000000",
        root2,
    );

    let flat = store.list(None).unwrap();
    assert_eq!(flat.len(), 1, "flat view must hide project buckets");
    assert_eq!(flat[0].id, "aaaa00000000");

    let mut all: Vec<String> = store
        .list_all()
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    all.sort();
    assert_eq!(all, ["aaaa00000000", "bbbb00000000", "cccc00000000"]);

    let scoped = store.list(Some(Path::new(root1))).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, "bbbb00000000");

    // A project with no bucket yet is simply empty, not an error.
    assert!(
        store
            .list(Some(Path::new("/work/never")))
            .unwrap()
            .is_empty()
    );
    // The `projects/` container itself is never listed as a session.
    assert!(!flat.iter().any(|i| i.id == PROJECTS_DIR_NAME));
}

/// Listings are `updated_at` DESC with unparsable timestamps last (chat/session.go:962).
#[test]
fn listing_sorts_newest_first_and_unparsable_last() {
    let (_home, store) = temp_store();
    let stamp = |id: &str, updated: &str| {
        let dir = store.root().join(id);
        write_bundle(&dir, id, "");
        let mut meta = iota::session::SessionMeta::read(&dir).unwrap();
        meta.updated_at = updated.to_owned();
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    };
    stamp("aaaa00000000", "2024-01-01T00:00:00+00:00");
    stamp("bbbb00000000", "2026-01-01T00:00:00+00:00");
    stamp("cccc00000000", "not a timestamp");

    let ids: Vec<String> = store
        .list(None)
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(ids, ["bbbb00000000", "aaaa00000000", "cccc00000000"]);
}

// Go: chat/session_project_test.go:256
#[test]
fn old_session_compat() {
    let (_home, store) = temp_store();
    let id = "dddd00000000";
    write_bundle(&store.root().join(id), id, "");

    let global = store.list(None).unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].id, id);
    // A flat bundle never leaks into a project scope.
    assert!(store.list(Some(Path::new("/work/p1"))).unwrap().is_empty());
    assert_eq!(store.resolve_id("dddd", None).unwrap(), id);
    let (_writer, sess) = store.resume(id, KIND).unwrap();
    assert_eq!(sess.messages.len(), 1);
    // meta without `cwd` loads with an empty cwd, not an error.
    assert_eq!(sess.meta.cwd, "");
}

// Go: chat/session_project_test.go:285
#[test]
fn session_id_taken() {
    let (_home, store) = temp_store();
    let root = "/work/p1";
    // A bare DIRECTORY reserves the id — no meta.json required.
    std::fs::create_dir_all(store.root().join("aaaa00000000")).unwrap();
    std::fs::create_dir_all(bucket_dir(store.root(), root).join("bbbb00000000")).unwrap();

    assert!(store.id_taken("aaaa00000000"), "flat id not seen as taken");
    assert!(
        store.id_taken("bbbb00000000"),
        "bucketed id not seen as taken"
    );
    assert!(!store.id_taken("cccc00000000"), "free id reported taken");
    // ...but a bundle is still only RECOGNISED by its meta.json.
    assert_eq!(store.find_dir("aaaa00000000"), None);
}

/// `from_dirs` composes `<home>/.iota/sessions`, and a host with no home is `$HOME is not defined`.
#[test]
fn store_from_host_dirs() {
    let dirs = HostDirs {
        home: Some(Path::new("/tmp/iota-test-home").to_path_buf()),
        ..HostDirs::default()
    };
    assert_eq!(
        SessionStore::from_dirs(&dirs).unwrap().root(),
        Path::new("/tmp/iota-test-home/.iota/sessions")
    );
    let err = SessionStore::from_dirs(&HostDirs::default()).unwrap_err();
    assert_eq!(err.to_string(), "$HOME is not defined");
    assert!(matches!(err, SessionError::HomeNotDefined));
}

/// A bundle whose `meta.json` is corrupt fails with Go's `cannot read session …` frame, and is skipped
/// (not fatal) by the listing views.
#[test]
fn unreadable_meta_is_cannot_read_and_unlisted() {
    let (_home, store) = temp_store();
    let dir = store.root().join("eeee00000000");
    write_bundle(&dir, "eeee00000000", "");
    std::fs::write(dir.join("meta.json"), b"{not json").unwrap();

    let err = store.load("eeee00000000", KIND).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("cannot read session eeee00000000: "),
        "got {err}"
    );
    assert!(matches!(err, SessionError::CannotRead { .. }));
    assert!(store.list(None).unwrap().is_empty());
}
