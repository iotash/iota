//! The sessions root: slugs, the two layouts, id minting and collision checks, the mode-isolated listing
//! views and `--resume` fragment resolution (`chat/session_test.go`, `chat/session_project_test.go`).

use std::path::Path;

use iota::app::HostDirs;
use iota::provider::ProviderKind;
use iota::provider::model::{Message, ToolCall};
use iota::session::{
    BOT_POINTER_FILE, BotPointer, INTERRUPTED_RESULT, LOCK_FILE, MAX_LOG_LINE, META_TMP_FILE,
    NewSession, PROJECTS_DIR_NAME, SESSION_ID_ALPHABET, SESSION_ID_LENGTH, SessionError,
    SessionInfo, SessionStore, resolve_in,
};
use pretty_assertions::assert_eq;

use crate::common::{bucket_dir, log_lines, temp_store, write_bundle};

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

/// A Windows root opens with a drive, and the slug becomes a DIRECTORY name: every character the
/// platform bars from one has to be gone, or `resume --workspace` cannot create its bucket at all
/// (`ERROR_INVALID_NAME`). Windows-only because `:` is a legal file-name character on unix and the
/// Go rule there leaves it alone.
#[cfg(windows)]
#[test]
fn project_slug_is_a_legal_windows_directory_name() {
    assert_eq!(
        SessionStore::project_slug(Path::new(r"C:\Users\x\proj")),
        "C--Users-x-proj"
    );
    let slug = SessionStore::project_slug(Path::new(r"\\?\C:\Users\x\my-app"));
    assert!(
        !slug.contains([':', '*', '?', '"', '<', '>', '|', '\\', '/']),
        "{slug:?} still carries a character no Windows directory name may hold"
    );
}
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
#[test]
fn project_session_writer() {
    let (_home, store) = temp_store();
    let root = "/work/myproj";

    let mut writer = store
        .create(NewSession {
            cwd: root.to_owned(),
            project: true,
            ..NewSession::new(KIND, "m1")
        })
        .unwrap();
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
        .create(NewSession {
            cwd: "/somewhere/else".to_owned(),
            ..NewSession::new(KIND, "m1")
        })
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
    let no_cwd = store
        .create(NewSession {
            project: true,
            ..NewSession::new(KIND, "m1")
        })
        .unwrap();
    assert_eq!(no_cwd.dir().parent(), Some(store.root()));
}
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
    assert_eq!(
        SessionStore::from_dirs(&dirs).unwrap().bots_dir(),
        Some(Path::new("/tmp/iota-test-home/.iota/bots"))
    );
    assert_eq!(SessionStore::new("/x").bots_dir(), None);
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

// ---------------------------------------------------------------- the bundle lock (bot-mode.md §2.3)

/// The text of a refusal naming this process as the holder.
fn locked_text(id: &str) -> String {
    format!(
        "session {id} is open in another iota process (pid {})",
        std::process::id()
    )
}

/// A bundle with one exchange in it, its writer already dropped.
fn saved_session(store: &SessionStore) -> String {
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    w.append_messages(&[Message::user("q"), Message::assistant("a")])
        .unwrap();
    w.id().to_owned()
}

/// Two stores over the same root stand in for two iota processes: while one holds the bundle, the
/// other's resume is refused with `Locked` (naming the holder's pid); once the holder drops, the
/// bundle can be resumed again.
#[test]
fn resume_is_refused_while_another_store_holds_the_bundle() {
    let (home, store) = temp_store();
    let other = SessionStore::new(store.root());
    let id = saved_session(&store);

    let (held, _) = store.resume(&id, KIND).unwrap();
    let err = other.resume(&id, KIND).expect_err("second writer refused");
    assert!(
        matches!(&err, SessionError::Locked { pid: Some(p), .. } if *p == std::process::id()),
        "{err:?}"
    );
    assert_eq!(err.to_string(), locked_text(&id));
    // Refused BEFORE anything was touched: the log is exactly what it was.
    assert_eq!(log_lines(&store.dir(&id).unwrap()).len(), 2);

    drop(held);
    let (again, session) = other.resume(&id, KIND).expect("re-entry after drop");
    assert_eq!(session.messages.len(), 2);
    drop(again);
    drop(home);
}

/// A fresh session takes the lock when its bundle is materialised (`ensure_created`), not before: a
/// pending writer holds nothing, a materialised one refuses a resume from elsewhere.
#[test]
fn a_new_bundle_is_locked_from_its_first_append() {
    let (_home, store) = temp_store();
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    assert!(
        !w.dir().join(LOCK_FILE).exists(),
        "a pending writer takes no lock"
    );
    w.append_messages(&[Message::user("q")]).unwrap();
    assert!(w.dir().join(LOCK_FILE).exists());
    let id = w.id().to_owned();
    let other = SessionStore::new(store.root());
    let err = other.resume(&id, KIND).expect_err("refused");
    assert_eq!(err.to_string(), locked_text(&id));
    drop(w);
    assert!(other.resume(&id, KIND).is_ok());
}

/// A first open of the log that fails lets the bundle lock go with it: once the obstacle is gone, the SAME
/// writer takes the lock again and appends, instead of being refused by its own guard (review R4).
#[test]
fn a_failed_first_open_leaves_the_lock_to_the_retry() {
    let (_home, store) = temp_store();
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    let log = w.dir().join("messages.jsonl");
    std::fs::create_dir_all(&log).unwrap();
    w.append_messages(&[Message::user("q")])
        .expect_err("the log is a directory");
    std::fs::remove_dir(&log).unwrap();
    w.append_messages(&[Message::user("q")])
        .expect("the same writer tries again");
    assert_eq!(log_lines(w.dir()).len(), 1);
    // And it holds the lock now, as any materialised writer does.
    let err = SessionStore::new(store.root())
        .resume(w.id(), KIND)
        .expect_err("held");
    assert_eq!(err.to_string(), locked_text(w.id()));
}

/// A batch is all or nothing for the log (review R1): one that fails partway is cut back to where it began,
/// so retrying it appends it once; one whose meta rewrite alone fails IS in the log — `MetaNotSaved`, not to
/// be appended again — and the counts carry on from it.
#[test]
fn a_failed_batch_is_all_or_nothing_and_a_failed_meta_is_not_a_failed_batch() {
    let (_home, store) = temp_store();
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    let id = w.id().to_owned();
    w.append_messages(&[Message::user("seed")]).unwrap();
    let dir = w.dir().to_path_buf();

    // Partway: the second record cannot be written at all.
    let mut huge = call("c1");
    huge.arguments.insert(
        "blob".to_owned(),
        serde_json::Value::String("x".repeat(MAX_LOG_LINE)),
    );
    let err = w
        .append_messages(&[
            Message::user("q"),
            Message::assistant("").with_tool_calls(vec![huge]),
        ])
        .expect_err("a record over the line cap");
    assert!(!matches!(err, SessionError::MetaNotSaved(_)), "{err:?}");
    assert_eq!(log_lines(&dir).len(), 1, "cut back to the batch's start");
    assert_eq!(w.meta().message_count, 1);

    // The meta rewrite alone fails: the batch is in the log.
    std::fs::create_dir(dir.join(META_TMP_FILE)).unwrap();
    let err = w
        .append_messages(&[Message::user("q"), Message::assistant("a")])
        .expect_err("meta.json cannot be rewritten");
    assert!(matches!(err, SessionError::MetaNotSaved(_)), "{err:?}");
    assert_eq!(log_lines(&dir).len(), 3);
    std::fs::remove_dir(dir.join(META_TMP_FILE)).unwrap();

    // What a caller does with `MetaNotSaved`: goes on with what comes next, and nothing is doubled.
    w.append_messages(&[Message::user("more")]).unwrap();
    assert_eq!(w.meta().message_count, 4);
    drop(w);
    let (_w, session) = store.resume(&id, KIND).unwrap();
    assert_eq!(session.meta.message_count, 4);
    let texts: Vec<&str> = session
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(texts, ["seed", "q", "a", "more"]);
}

/// `delete` refuses a bundle that is held open, and removes it once the holder is gone.
#[test]
fn delete_is_refused_while_the_bundle_is_held() {
    let (_home, store) = temp_store();
    let id = saved_session(&store);
    let dir = store.dir(&id).unwrap();
    let (held, _) = store.resume(&id, KIND).unwrap();
    let other = SessionStore::new(store.root());
    let err = other.delete(&id).expect_err("refused");
    assert!(matches!(err, SessionError::Locked { .. }), "{err:?}");
    assert!(dir.exists());
    drop(held);
    other.delete(&id).unwrap();
    assert!(!dir.exists());
}

// ---------------------------------------------------------------- bots (bot-mode.md §2.2, §2.7)

/// `bot_owner` answers from the pointers; `delete` refuses a pointed-at session whether or not the bot is
/// running (nothing holds its lock here), and removes it once the pointer is gone.
#[test]
fn a_bot_session_is_protected_from_delete() {
    let (home, store) = temp_store();
    let bots = home.path().join("bots");
    let store = store.with_bots(&bots);
    let id = saved_session(&store);
    let free = saved_session(&store);
    assert_eq!(store.bot_owner(&id).unwrap(), None);
    BotPointer::new(&id).write(&bots.join("coder")).unwrap();
    assert_eq!(store.bot_owner(&id).unwrap().as_deref(), Some("coder"));
    assert_eq!(store.bot_owner(&free).unwrap(), None);
    assert_eq!(store.bot_sessions(), vec![id.clone()]);

    let err = store.delete(&id).expect_err("a bot's body");
    assert!(matches!(&err, SessionError::BotOwned { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!("session {id} belongs to bot coder; run iota run coder")
    );
    assert!(store.find_dir(&id).is_some(), "still there");
    store.delete(&free).expect("an ordinary session goes");

    std::fs::remove_file(bots.join("coder").join(iota::session::BOT_POINTER_FILE)).unwrap();
    store.delete(&id).expect("no longer pointed at");
}

/// A pointer that cannot be read is "cannot tell", never "no owner" (review R6): while it is broken the
/// gate refuses every session — the pointer may name any of them — and `delete` removes none; the listing
/// only leaves it out. Once it reads again, the ordinary rules are back.
#[test]
fn an_unreadable_pointer_blocks_the_gate_and_delete() {
    let (home, store) = temp_store();
    let bots = home.path().join("bots");
    let store = store.with_bots(&bots);
    let id = saved_session(&store);
    let free = saved_session(&store);
    BotPointer::new(&id).write(&bots.join("coder")).unwrap();
    let err = store.delete(&id).expect_err("a bot's body");
    assert!(matches!(&err, SessionError::BotOwned { .. }), "{err:?}");

    std::fs::write(bots.join("coder").join(BOT_POINTER_FILE), "{oops").unwrap();
    for s in [&id, &free] {
        let err = store.check_not_bot_owned(s).expect_err("cannot tell");
        assert!(
            matches!(&err, SessionError::BotOwnerUnknown { .. }),
            "{err:?}"
        );
        let pointer = bots.join("coder").join(BOT_POINTER_FILE);
        assert!(
            err.to_string().starts_with(&format!(
                "cannot tell whether session {s} belongs to a bot: "
            )) && err.to_string().ends_with(&format!(
                "; repair or delete {} (without it the bot starts a new session; memory is kept)",
                pointer.display()
            )),
            "{err}"
        );
        let err = store.delete(s).expect_err("not deleted");
        assert!(
            matches!(&err, SessionError::BotOwnerUnknown { .. }),
            "{err:?}"
        );
        assert!(store.find_dir(s).is_some(), "still there");
    }
    assert!(store.bot_sessions().is_empty(), "the listing skips it");

    BotPointer::new(&id).write(&bots.join("coder")).unwrap();
    assert!(matches!(
        store.delete(&id),
        Err(SessionError::BotOwned { .. })
    ));
    store
        .delete(&free)
        .expect("readable again: an ordinary session goes");
}

/// A store without a bots root knows no owners, so nothing is refused for that reason.
#[test]
fn a_store_without_bots_knows_no_owner() {
    let (home, store) = temp_store();
    let id = saved_session(&store);
    BotPointer::new(&id)
        .write(&home.path().join("bots").join("coder"))
        .unwrap();
    assert_eq!(store.bot_owner(&id).unwrap(), None);
}

// ---------------------------------------------------------------- repairing the tail (bot-mode.md §2.7)

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "shell".to_owned(),
        ..ToolCall::default()
    }
}

/// A batch cut off after the assistant's tool calls (one answered, one not) comes back with the
/// missing result synthesised as an interrupted error, appended to the log so a second resume finds
/// nothing left to repair.
#[test]
fn resume_answers_the_tool_calls_the_log_left_open() {
    let (_home, store) = temp_store();
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    let id = w.id().to_owned();
    let calls = vec![call("c1"), call("c2")];
    w.append_messages(&[
        Message::user("run both"),
        Message::assistant("").with_tool_calls(calls.clone()),
        Message::tool_result(&calls[0], "ok", false),
    ])
    .unwrap();
    drop(w);

    let (writer, session) = store.resume(&id, KIND).unwrap();
    assert_eq!(session.repaired, 1);
    assert_eq!(
        session.repair_notice().as_deref(),
        Some("Recovered 1 tool call(s) with no recorded result; they are marked as interrupted.")
    );
    let last = session.messages.last().unwrap();
    assert_eq!(last.tool_call_id(), "c2");
    assert_eq!(last.tool_call_name(), "shell");
    assert_eq!(last.content, INTERRUPTED_RESULT);
    assert!(last.is_error());
    assert_eq!(
        session.meta.message_count, 4,
        "the synthesised result is counted"
    );
    drop(writer);

    let (_w, again) = store.resume(&id, KIND).unwrap();
    assert_eq!(again.repaired, 0);
    assert_eq!(again.repair_notice(), None);
    assert_eq!(again.messages, session.messages, "the repair was persisted");
}

/// A torn final line (a crash mid-write, no newline) must not swallow what the next append writes:
/// resume terminates it first, so the repair lands as a record of its own.
#[test]
fn resume_terminates_a_torn_last_line_before_appending() {
    let (_home, store) = temp_store();
    let mut w = store.create(NewSession::new(KIND, "m1")).unwrap();
    let id = w.id().to_owned();
    let dir = w.dir().to_path_buf();
    w.append_messages(&[
        Message::user("go"),
        Message::assistant("").with_tool_calls(vec![call("c1")]),
    ])
    .unwrap();
    drop(w);
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("messages.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut log, br#"{"role":"tool","content":"half"#).unwrap();
    drop(log);

    let (w, session) = store.resume(&id, KIND).unwrap();
    assert_eq!(session.repaired, 1);
    drop(w);
    let (_w, again) = store.resume(&id, KIND).unwrap();
    assert_eq!(
        again.repaired, 0,
        "the synthesised record survived the reload"
    );
    assert_eq!(again.messages.last().unwrap().content, INTERRUPTED_RESULT);
}
