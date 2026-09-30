//! The bot branch of the session wiring (docs/design/bot-mode.md §2.2): `iota run <bot>` opens the bot's
//! ONE session — resumed when it exists, created under the pointer's id when it does not — and makes the
//! config, not the session's first day, decide the system prompt and the model parameters.
//!
//! Kept apart from `wire_session` so the whole branch runs in a unit test: everything here is the store, a
//! provider and plain values, nothing that needs a terminal.

use std::path::Path;

use crate::provider::Provider;
use crate::provider::model::{Message, Role};
use crate::session::{
    BotOpen, NewSession, Overrides, SessionError, SessionMeta, SessionStore, SessionWriter,
    replay_session_settings,
};

/// The notice a config-edited system prompt leaves in the transcript.
pub(crate) const SYSTEM_UPDATED: &str = "system prompt updated from config";

/// The notice for a pointer whose bundle never reached the disk.
pub(crate) const NEVER_SAVED: &str =
    "The bot's last run ended before anything was saved; its session starts empty.";

/// What the bot branch hands back to `wire_session`.
pub(crate) struct BotSession {
    /// The bot's writer (it holds the bot lock for the rest of the run).
    pub(crate) writer: SessionWriter,
    /// The resumed view (empty for a fresh session), with the config's system prompt first.
    pub(crate) history: Vec<Message>,
    /// The session was resumed (not created).
    pub(crate) resumed: bool,
    /// Dim transcript lines for the chat's opening.
    pub(crate) notices: Vec<String>,
    /// On a resume, the meta as the last run left it — before this run stamped its model and directory
    /// over it. [`resume_notices`] reads it.
    pub(crate) previous: Option<SessionMeta>,
    /// The tail repair's announcement, when the resume had to repair anything.
    pub(crate) repair_notice: Option<String>,
}

/// Refuses a provider a bot cannot run on (bot-mode.md §4.1): one that does not report token usage (the
/// compaction threshold would be a guess) or cannot call tools (no `remember`). Image providers fail both.
pub(crate) fn check_bot_provider(
    name: &str,
    provider: &dyn Provider,
) -> Result<(), crate::cmd::error::SetupError> {
    if provider.reports_usage() && provider.as_tool_provider().is_some() {
        Ok(())
    } else {
        Err(crate::cmd::error::SetupError::BotProvider(name.to_owned()))
    }
}

/// The lines a resumed bot's model is told first (bot-mode.md §2.5, review M2/M3) — across a restart its only
/// sense of time and place, since the view carries no timestamps: how long ago the session was last written
/// (`meta.updated_at`, read in `now`'s time zone) and, when `cwd` is not the directory it last ran in
/// (`meta.cwd`), that the project changed. An unparsable `updated_at` says nothing about time; an empty
/// `meta.cwd` (a bundle older than the key) nothing about place.
pub(crate) fn resume_notices(meta: &SessionMeta, cwd: &str, now: &jiff::Zoned) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(last) = meta.updated_at.parse::<jiff::Timestamp>() {
        let secs = (now.timestamp().as_second() - last.as_second()).max(0);
        let at = last.to_zoned(now.time_zone().clone());
        out.push(format!(
            "Resumed after {} (last message {})",
            elapsed(secs),
            at.strftime("%Y-%m-%d %H:%M")
        ));
    }
    if !meta.cwd.is_empty() && meta.cwd != cwd {
        out.push(format!(
            "Resumed in a different project: {} → {cwd}",
            meta.cwd
        ));
    }
    out
}

/// `secs` in the largest whole unit: `3 days`, `1 hour`, `12 minutes`, `less than a minute`.
fn elapsed(secs: i64) -> String {
    let (n, unit) = match secs {
        s if s >= 86_400 => (s / 86_400, "day"),
        s if s >= 3_600 => (s / 3_600, "hour"),
        s if s >= 60 => (s / 60, "minute"),
        _ => return "less than a minute".to_owned(),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Opens bot `name`'s session from `<bots>/<name>`. `fresh` describes the bundle a first launch creates;
/// `system` is the config's (trimmed) system prompt. On a resume the session's own model and parameters
/// are NOT replayed — the provider keeps what the config (or `-M`) gave it, and the meta is told the
/// model it now runs; the loop stamps the other four the same way it stamps a new bundle. The meta is also
/// told the directory this run is in, so the next resume's [`resume_notices`] compare against the last
/// run's, not the first one's.
pub(crate) fn open_bot_session(
    store: &SessionStore,
    bots: &Path,
    name: &str,
    fresh: NewSession,
    system: &str,
    provider: &mut dyn Provider,
    warn: &mut dyn FnMut(String),
) -> Result<BotSession, SessionError> {
    let kind = fresh.kind;
    let cwd = fresh.cwd.clone();
    match store.open_bot(&bots.join(name), fresh, kind)? {
        BotOpen::Fresh {
            mut writer,
            never_saved,
        } => {
            // No titler for a bot: its one session is named after it (§2.2). Pending, so nothing is written
            // until the first message lands.
            writer.update_meta(|m| name.clone_into(&mut m.title))?;
            Ok(BotSession {
                writer,
                history: Vec::new(),
                resumed: false,
                notices: never_saved
                    .then(|| NEVER_SAVED.to_owned())
                    .into_iter()
                    .collect(),
                previous: None,
                repair_notice: None,
            })
        }
        BotOpen::Resumed(mut writer, session) => {
            // What the bundle carries besides the five knobs the config owns (image output, json edits)
            // still replays.
            replay_session_settings(
                &session.meta,
                provider,
                kind,
                &Overrides {
                    config_wins: true,
                    ..Overrides::default()
                },
                warn,
            );
            let model = provider.model().to_owned();
            writer.update_meta(|m| {
                m.model = model;
                kind.as_str().clone_into(&mut m.provider);
                m.cwd = cwd;
            })?;
            let repair_notice = session.repair_notice();
            let mut history = session.messages;
            let mut notices = Vec::new();
            if adopt_system(&mut writer, &mut history, system)? {
                notices.push(SYSTEM_UPDATED.to_owned());
            }
            Ok(BotSession {
                writer,
                history,
                resumed: true,
                notices,
                previous: Some(session.meta),
                repair_notice,
            })
        }
    }
}

/// Makes the config's system prompt the session's (§2.2 "配置变更要生效"): when it differs from the view's
/// first message, a new system record is appended — the log's last system record wins on the next load, so
/// the format needs nothing new — and the view's head is replaced. `true` when anything changed. The same
/// prompt changes nothing, so the prompt cache survives a restart.
///
/// An EMPTY config prompt is left alone: the log cannot say "no system prompt" (an empty system record
/// never wins), so a bot whose `system:` was removed keeps the last one it had.
fn adopt_system(
    writer: &mut SessionWriter,
    history: &mut Vec<Message>,
    system: &str,
) -> Result<bool, SessionError> {
    if system.is_empty() {
        return Ok(false);
    }
    let head_is_system = history.first().is_some_and(|m| m.role() == Role::System);
    if head_is_system && history[0].content == system {
        return Ok(false);
    }
    let msg = Message::system(system.to_owned());
    writer.append_messages(std::slice::from_ref(&msg))?;
    if head_is_system {
        history[0] = msg;
    } else {
        history.insert(0, msg);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        NEVER_SAVED, SYSTEM_UPDATED, check_bot_provider, open_bot_session, resume_notices,
    };
    use crate::provider::model::{Message, Role};
    use crate::provider::{Provider, ProviderKind};
    use crate::session::{
        BOT_POINTER_FILE, BotPointer, NewSession, SessionError, SessionMeta, SessionStore, load_log,
    };
    use crate::testing::FakeProvider;

    /// bot-mode.md §4.1: a bot needs a provider that reports usage AND calls tools; either missing is a setup
    /// error in the document's words.
    #[test]
    fn a_bot_needs_usage_and_tools() {
        let refused =
            r#"bot "coder" needs a chat model that reports token usage and supports tools"#;
        for p in [
            FakeProvider::new(),
            FakeProvider::new().reporting_usage(),
            FakeProvider::new().with_tools(),
        ] {
            let e = check_bot_provider("coder", &p).expect_err("refused");
            assert_eq!(e.to_string(), refused);
        }
        check_bot_provider("coder", &FakeProvider::new().reporting_usage().with_tools())
            .expect("usage and tools are enough");
    }

    /// The bundle a first launch creates.
    fn fresh(kind: ProviderKind, model: &str) -> NewSession {
        NewSession {
            cwd: "/work/proj".to_owned(),
            agent: "coder".to_owned(),
            ..NewSession::new(kind, model)
        }
    }

    /// A temp `~/.iota`: the store and the bots root side by side, as `SessionStore::from_dirs` builds them.
    struct Home {
        _tmp: tempfile::TempDir,
        store: SessionStore,
        bots: std::path::PathBuf,
    }

    fn home() -> Home {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bots = tmp.path().join("bots");
        let store = SessionStore::new(tmp.path().join("sessions")).with_bots(&bots);
        Home {
            _tmp: tmp,
            store,
            bots,
        }
    }

    fn provider(model: &str) -> FakeProvider {
        FakeProvider::new()
            .with_kind(ProviderKind::OpenAi)
            .with_model(model)
            .tunable()
    }

    fn open(
        h: &Home,
        p: &mut FakeProvider,
        system: &str,
    ) -> Result<super::BotSession, SessionError> {
        open_in(h, p, system, "/work/proj")
    }

    /// [`open`] from directory `cwd`.
    fn open_in(
        h: &Home,
        p: &mut FakeProvider,
        system: &str,
        cwd: &str,
    ) -> Result<super::BotSession, SessionError> {
        let model = p.model().to_owned();
        open_bot_session(
            &h.store,
            &h.bots,
            "coder",
            NewSession {
                cwd: cwd.to_owned(),
                ..fresh(ProviderKind::OpenAi, &model)
            },
            system,
            p,
            &mut |w| panic!("unexpected warning: {w}"),
        )
    }

    fn pointer(h: &Home) -> BotPointer {
        BotPointer::read(&h.bots.join("coder"))
            .expect("read pointer")
            .expect("a pointer")
    }

    /// A first launch writes the pointer BEFORE any bundle exists, names the pending bundle after the bot,
    /// keeps it flat, and marks the pointer materialised once the first message lands; the second launch
    /// resumes that same session.
    #[test]
    fn first_launch_then_resume() {
        let h = home();
        let mut p = provider("gpt-4o");
        let mut first = open(&h, &mut p, "be terse").expect("fresh");
        assert!(!first.resumed && first.history.is_empty() && first.notices.is_empty());
        let id = first.writer.id().to_owned();
        assert_eq!(
            pointer(&h),
            BotPointer::new(&id),
            "pointer first, bundle later"
        );
        assert!(!first.writer.on_disk());
        assert_eq!(first.writer.meta().title, "coder");
        assert_eq!(first.writer.dir(), h.store.root().join(&id), "flat layout");

        first
            .writer
            .append_messages(&[Message::system("be terse".to_owned()), Message::user("hi")])
            .expect("first write");
        assert!(
            pointer(&h).materialized,
            "the first write materialises the pointer"
        );
        assert_eq!(
            SessionMeta::read(first.writer.dir()).expect("meta").title,
            "coder"
        );
        drop(first);

        let again = open(&h, &mut p, "be terse").expect("resume");
        assert!(again.resumed);
        assert_eq!(again.writer.id(), id);
        assert_eq!(again.history.len(), 2);
        assert!(
            again.notices.is_empty(),
            "same system prompt: nothing to say"
        );
    }

    /// bot-mode.md §2.5 (review M2/M3): a resume tells the model how long it was away — from the meta's
    /// `updated_at` — and, started in another directory, that the project changed; the meta then records this
    /// run's directory, so the next resume from the same place says nothing about it. A fresh launch says
    /// neither.
    #[test]
    fn a_resume_tells_the_model_how_long_and_where() {
        let h = home();
        let mut p = provider("gpt-4o");
        let now: jiff::Zoned = "2026-09-30T19:30:00+00:00[UTC]".parse().expect("now");
        let mut first = open_in(&h, &mut p, "", "/work/iota").expect("fresh");
        assert!(
            first.previous.is_none(),
            "a fresh launch has nothing to say"
        );
        first
            .writer
            .append_messages(&[Message::user("hi")])
            .expect("first write");
        let dir = first.writer.dir().to_path_buf();
        drop(first);
        // Last written three days and a bit before `now` (the writer stamps the real clock).
        let path = dir.join(crate::session::META_FILE);
        let mut meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("meta")).expect("json");
        meta["updated_at"] = "2026-09-27T18:02:41+00:00".into();
        std::fs::write(&path, meta.to_string()).expect("write meta");

        let again = open_in(&h, &mut p, "", "/work/herdr").expect("resume");
        let previous = again.previous.as_ref().expect("resumed");
        assert_eq!(
            resume_notices(previous, "/work/herdr", &now),
            [
                "Resumed after 3 days (last message 2026-09-27 18:02)",
                "Resumed in a different project: /work/iota → /work/herdr",
            ]
        );
        assert_eq!(again.writer.meta().cwd, "/work/herdr");
        drop(again);

        let third = open_in(&h, &mut p, "", "/work/herdr").expect("resume");
        let notices = resume_notices(
            third.previous.as_ref().expect("resumed"),
            "/work/herdr",
            &jiff::Zoned::now(),
        );
        assert_eq!(
            notices.len(),
            1,
            "same directory as the last run: {notices:?}"
        );
        assert!(
            notices[0].starts_with("Resumed after less than a minute (last message "),
            "{notices:?}"
        );
        // An unparsable stamp and a bundle older than `cwd` say nothing.
        let bare = SessionMeta {
            updated_at: "garbage".to_owned(),
            ..SessionMeta::default()
        };
        assert!(resume_notices(&bare, "/work/herdr", &now).is_empty());
    }

    /// The elapsed time in its largest whole unit, singular or plural.
    #[test]
    fn the_elapsed_time_reads_naturally() {
        assert_eq!(super::elapsed(59), "less than a minute");
        assert_eq!(super::elapsed(60), "1 minute");
        assert_eq!(super::elapsed(2 * 3_600 + 59), "2 hours");
        assert_eq!(super::elapsed(86_400), "1 day");
        assert_eq!(super::elapsed(10 * 86_400 + 3_600), "10 days");
    }

    /// Only one process runs a bot: the second is refused with the bot's sentence — even while the first
    /// has written its pointer and nothing else.
    #[test]
    fn a_running_bot_is_locked() {
        let h = home();
        let mut p = provider("gpt-4o");
        let first = open(&h, &mut p, "").expect("first");
        let Err(err) = open(&h, &mut p, "") else {
            panic!("the second launch must be refused");
        };
        assert_eq!(
            err.to_string(),
            format!("bot coder is already running (pid {})", std::process::id())
        );
        drop(first);
        open(&h, &mut p, "").expect("free once the first run is gone");
    }

    /// A pointer whose bundle never reached the disk: the same id, empty again, with a notice.
    #[test]
    fn an_unsaved_pointer_starts_over_under_the_same_id() {
        let h = home();
        BotPointer::new("k7qz3xv9m2ht")
            .write(&h.bots.join("coder"))
            .expect("pointer");
        let mut p = provider("gpt-4o");
        let s = open(&h, &mut p, "").expect("fresh again");
        assert!(!s.resumed);
        assert_eq!(s.writer.id(), "k7qz3xv9m2ht");
        assert_eq!(s.notices, vec![NEVER_SAVED.to_owned()]);
    }

    /// A materialised pointer whose bundle is gone is a hard error with both ways out — and nothing is
    /// created in its place.
    #[test]
    fn a_missing_materialised_bundle_is_a_hard_error() {
        let h = home();
        let dir = h.bots.join("coder");
        BotPointer {
            materialized: true,
            ..BotPointer::new("k7qz3xv9m2ht")
        }
        .write(&dir)
        .expect("pointer");
        let mut p = provider("gpt-4o");
        let Err(err) = open(&h, &mut p, "") else {
            panic!("must not start a new session");
        };
        assert!(matches!(&err, SessionError::BotMissing { .. }), "{err:?}");
        assert_eq!(
            err.to_string(),
            format!(
                "bot coder's session k7qz3xv9m2ht is missing: restore {}, or delete {} to start over (memory is kept)",
                h.store.root().join("k7qz3xv9m2ht").display(),
                dir.join(BOT_POINTER_FILE).display()
            )
        );
        assert!(!h.store.root().join("k7qz3xv9m2ht").exists());
        assert!(pointer(&h).materialized, "the pointer is left as it was");
    }

    /// An unreadable body is reported as it is, never replaced by a new session.
    #[test]
    fn an_unreadable_bundle_is_reported_not_replaced() {
        let h = home();
        let bundle = h.store.root().join("k7qz3xv9m2ht");
        std::fs::create_dir_all(&bundle).expect("bundle");
        std::fs::write(bundle.join(crate::session::META_FILE), "{oops").expect("meta");
        BotPointer {
            materialized: true,
            ..BotPointer::new("k7qz3xv9m2ht")
        }
        .write(&h.bots.join("coder"))
        .expect("pointer");
        let mut p = provider("gpt-4o");
        let Err(err) = open(&h, &mut p, "") else {
            panic!("a damaged body must not be replaced");
        };
        assert!(matches!(&err, SessionError::CannotRead { .. }), "{err:?}");
        assert_eq!(pointer(&h).session, "k7qz3xv9m2ht");
        assert_eq!(
            std::fs::read_to_string(bundle.join(crate::session::META_FILE)).expect("meta"),
            "{oops"
        );
    }

    /// A pointer left `materialized: false` by a crash between the bundle's creation and the pointer's
    /// rewrite is corrected by the next resume.
    #[test]
    fn a_resume_fixes_a_stale_materialized_flag() {
        let h = home();
        let mut p = provider("gpt-4o");
        let mut s = open(&h, &mut p, "").expect("fresh");
        s.writer
            .append_messages(&[Message::user("hi")])
            .expect("write");
        let id = s.writer.id().to_owned();
        drop(s);
        BotPointer::new(&id)
            .write(&h.bots.join("coder"))
            .expect("stale pointer");
        assert!(open(&h, &mut p, "").expect("resume").resumed);
        assert!(pointer(&h).materialized);
    }

    /// §2.2 "配置变更要生效": a changed `system:` is appended as a new system record (the log's last one wins
    /// on the next load), replaces the view's head, and says so; an unchanged one touches nothing.
    #[test]
    fn a_changed_system_prompt_is_taken_from_the_config() {
        let h = home();
        let mut p = provider("gpt-4o");
        let mut s = open(&h, &mut p, "old prompt").expect("fresh");
        s.writer
            .append_messages(&[
                Message::system("old prompt".to_owned()),
                Message::user("hi"),
            ])
            .expect("write");
        let dir = s.writer.dir().to_path_buf();
        drop(s);

        let s = open(&h, &mut p, "new prompt").expect("resume");
        assert_eq!(s.notices, vec![SYSTEM_UPDATED.to_owned()]);
        assert_eq!(s.history[0].role(), Role::System);
        assert_eq!(s.history[0].content, "new prompt");
        assert_eq!(s.history.len(), 2, "replaced, not added to the view");
        drop(s);
        let log = load_log(&dir, ProviderKind::OpenAi).expect("reload");
        assert_eq!(
            log.view[0].content, "new prompt",
            "the last system record wins"
        );
        let lines = std::fs::read_to_string(dir.join(crate::session::LOG_FILE)).expect("log");
        assert_eq!(lines.lines().count(), 3, "one system record appended");

        let s = open(&h, &mut p, "new prompt").expect("resume again");
        assert!(s.notices.is_empty());
        drop(s);
        let lines = std::fs::read_to_string(dir.join(crate::session::LOG_FILE)).expect("log");
        assert_eq!(
            lines.lines().count(),
            3,
            "an unchanged prompt appends nothing"
        );
    }

    /// A session that never had a system prompt gets the config's put at the head of the view.
    #[test]
    fn a_new_system_prompt_goes_first() {
        let h = home();
        let mut p = provider("gpt-4o");
        let mut s = open(&h, &mut p, "").expect("fresh");
        s.writer
            .append_messages(&[Message::user("hi")])
            .expect("write");
        drop(s);
        let s = open(&h, &mut p, "now with a prompt").expect("resume");
        assert_eq!(s.notices, vec![SYSTEM_UPDATED.to_owned()]);
        assert_eq!(s.history[0].content, "now with a prompt");
        assert_eq!(s.history[1].content, "hi");
    }

    /// The five knobs the config owns are NOT replayed from the meta on a bot's resume, and the model it
    /// runs is written back into the meta.
    #[test]
    fn the_config_wins_over_the_session_meta() {
        let h = home();
        let mut p = provider("gpt-4o").with_temperature(Some(0.2));
        let mut s = open(&h, &mut p, "").expect("fresh");
        s.writer
            .update_meta(|m| {
                "gpt-3.5".clone_into(&mut m.model);
                m.temperature = Some(1.5);
                "high".clone_into(&mut m.effort);
                m.top_p = Some(0.5);
                m.set_context_window(8_000);
            })
            .expect("meta");
        s.writer
            .append_messages(&[Message::user("hi")])
            .expect("write");
        let dir = s.writer.dir().to_path_buf();
        drop(s);

        let s = open(&h, &mut p, "").expect("resume");
        assert_eq!(p.model(), "gpt-4o", "the meta's model is not replayed");
        let tunable = p.as_tunable().expect("tunable");
        assert_eq!(tunable.temperature(), Some(0.2), "nor its temperature");
        assert_eq!(tunable.effort(), None, "nor its effort");
        let meta = SessionMeta::read(&dir).expect("meta");
        assert_eq!(meta.model, "gpt-4o", "the running model is written back");
        assert_eq!(s.writer.meta().model, "gpt-4o");
    }

    /// `iota resume <id>`'s gate: a bot's session is refused with the way to open it; any other passes.
    #[test]
    fn resume_refuses_a_bot_session() {
        let h = home();
        BotPointer::new("k7qz3xv9m2ht")
            .write(&h.bots.join("coder"))
            .expect("pointer");
        let err = h
            .store
            .check_not_bot_owned("k7qz3xv9m2ht")
            .expect_err("owned");
        assert_eq!(
            err.to_string(),
            "session k7qz3xv9m2ht belongs to bot coder; run iota run coder"
        );
        h.store
            .check_not_bot_owned("otherid00000")
            .expect("not a bot's");
    }
}
