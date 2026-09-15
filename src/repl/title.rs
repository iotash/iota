//! The session-title state machine and its async pass (chat/title.go, chat/chat.go:160-256,
//! chat/run.go:218-251).
//!
//! A session is named the moment its first user message exists — BEFORE the turn runs, so
//! a first turn that spends minutes in tool calls is not left poorly named while it works.
//! Three moves, in the order they can occur:
//!
//! - [`SessionTitle::seed`] lands the prompt-derived placeholder and releases that message
//!   (plus a generation number) for the async model pass;
//! - [`SessionTitle::land`] replaces the placeholder when the pass returns;
//! - [`SessionTitle::unseed`] gives the name back when the message it was derived from
//!   rolls back with a failed or discarded turn — and bumps the generation, so a
//!   late-landing pass for that text is dropped instead of naming the session after
//!   something it no longer contains.
//!
//! Nothing here waits for the assistant: both the placeholder and the model pass are
//! derived from the user's message alone. The pass rides a SECOND provider instance (the
//! turn is still streaming on the first, whose per-call state is not safe for a concurrent
//! request) and is joined before any input that could swap or mint the writer.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::provider::Provider;
use crate::provider::model::{Message, Role};
use tokio_util::sync::CancellationToken;

use crate::repl::render::styles::truncate_runes;

/// The async pass's deadline (chat/run.go:245).
pub(crate) const TITLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Rune cap of the prompt-derived placeholder (chat/title.go `seed`).
const PLACEHOLDER_CAP: usize = 40;

/// Rune cap of a settled title (chat/chat.go:203 `sanitizeTitle`, run.go:838 `/save`).
pub(crate) const TITLE_CAP: usize = 80;

/// Rune cap of the message text handed to the model pass (chat/chat.go:176).
const PROMPT_CAP: usize = 500;

/// The session writer as the loop holds it: `None` while the chat is ephemeral, swapped by
/// `/session`, minted late by `/save`. Shared with the title pass, which resolves it at
/// land time — never captures it.
pub(crate) type WriterSlot = Arc<Mutex<Option<crate::session::SessionWriter>>>;

/// Where a name is shown besides the session bundle (the window title, via the facade).
pub(crate) type WindowSink = Box<dyn Fn(&str) + Send + Sync>;

/// The mutable half (the async pass's `land` races the loop's own moves).
#[derive(Default)]
struct TitleState {
    /// A placeholder is on the session.
    seeded: bool,
    /// Settled — resumed or user-chosen; never overwritten.
    titled: bool,
    /// Seed generation; `land` drops a pass an `unseed` outlived.
    generation: u64,
    /// The name as last set — `reapply` hands it to a writer minted later.
    current: String,
}

/// One session's name across a turn's lifecycle (chat/title.go `sessionTitle`).
pub(crate) struct SessionTitle {
    /// Resolved on EVERY call rather than captured: the loop swaps the writer
    /// (`/session`) and mints it late (`/save`), so a captured copy would name the session
    /// the user just left, or decide there is none forever.
    writer: WriterSlot,
    window: WindowSink,
    state: Mutex<TitleState>,
}

impl SessionTitle {
    /// Wires the sinks. A resumed session arrives already named, so it is left alone.
    pub(crate) fn new(writer: WriterSlot, window: WindowSink, resumed: bool) -> Self {
        Self {
            writer,
            window,
            state: Mutex::new(TitleState {
                seeded: resumed,
                titled: resumed,
                ..TitleState::default()
            }),
        }
    }

    /// Lands the placeholder derived from the first user message and, on the ONE call that
    /// newly seeds, releases that message and the generation so the caller can fire the
    /// model pass (chat/title.go `seed`).
    ///
    /// An ephemeral chat seeds too — the window title is worth having even when nothing
    /// persists, and `/save` reapplies the name to the bundle it mints.
    pub(crate) fn seed(&self, history: &[Message]) -> Option<(String, u64)> {
        let mut st = self.lock();
        if st.seeded {
            return None;
        }
        let first_user = first_user_text(history);
        if first_user.is_empty() {
            return None;
        }
        st.seeded = true;
        let placeholder = title_from(&first_user, PLACEHOLDER_CAP);
        self.set(&mut st, &placeholder);
        Some((first_user, st.generation))
    }

    /// Applies the model pass's answer — unless the seed it was derived from rolled back
    /// meanwhile (generation mismatch), the name settled (an explicit `/save` title
    /// outranks the pass), or the pass came back empty (the placeholder stands; there is
    /// no retry).
    pub(crate) fn land(&self, generation: u64, name: &str) {
        let mut st = self.lock();
        if generation != st.generation || st.titled || name.is_empty() {
            return;
        }
        self.set(&mut st, name);
    }

    /// Drops a name whose message is no longer in the history (chat/title.go `unseed`).
    ///
    /// Seeding before the turn means the name can outlive what it was named after, so a
    /// rolled-back turn has to give it back — model-written or not: the text it summarized
    /// is gone either way. A settled title is never touched, and the generation bump
    /// invalidates a pass still in flight.
    pub(crate) fn unseed(&self, history: &[Message]) {
        let mut st = self.lock();
        if st.titled || !st.seeded {
            return;
        }
        if !first_user_text(history).is_empty() {
            return;
        }
        st.seeded = false;
        st.generation += 1;
        self.set(&mut st, "");
    }

    /// Settles the name without touching it — a session resumed mid-chat arrives with its
    /// own (chat/title.go `adopt`).
    pub(crate) fn adopt(&self) {
        let mut st = self.lock();
        st.seeded = true;
        st.titled = true;
    }

    /// Settles an EXPLICIT name: a title the user chose (`/save "…"`) is never overwritten,
    /// and a pass still in flight is dropped by [`SessionTitle::land`]'s settled check.
    pub(crate) fn adopt_name(&self, name: &str) {
        let mut st = self.lock();
        st.seeded = true;
        st.titled = true;
        self.set(&mut st, name);
    }

    /// Hands the current name to the CURRENT writer (chat/title.go `reapply`): `/save`
    /// mints the bundle mid-chat, after the ephemeral session was already named at first
    /// send, so the fresh meta must catch up with what the window shows.
    pub(crate) fn reapply(&self) {
        let st = self.lock();
        if !st.current.is_empty() {
            self.write_title(&st.current);
        }
    }

    /// Writes a name to both the session and the window. An ephemeral chat has no writer
    /// (a no-op) — the window still gets the name, and `current` remembers it for a writer
    /// minted later.
    fn set(&self, st: &mut TitleState, name: &str) {
        name.clone_into(&mut st.current);
        self.write_title(name);
        (self.window)(name);
    }

    fn write_title(&self, name: &str) {
        if let Some(w) = self
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            // A meta write failure is not worth a red block over a title: the name is on
            // the window either way and the next append rewrites meta.
            let _ = w.update_meta(|m| name.clone_into(&mut m.title));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TitleState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a session is named after: the first user message with any text (chat/chat.go:166).
///
/// The assistant reply is deliberately NOT an input — the name must not wait for it (a
/// tool-heavy first turn takes minutes), so an attachment-only opener defers to the next
/// message that carries text.
pub(crate) fn first_user_text(history: &[Message]) -> String {
    history
        .iter()
        .find(|m| m.role() == Role::User && !m.content.is_empty())
        .map_or_else(String::new, |m| m.content.clone())
}

/// Shapes arbitrary text into a session title (chat/chat.go:245): ONE line (a stored
/// newline would break the session picker's row accounting and the window title), control
/// characters dropped, capped on rune boundaries. EVERY title entry point funnels through
/// it — the model's answer, the prompt-derived placeholder, an explicit `/save` argument.
pub(crate) fn title_from(s: &str, max: usize) -> String {
    truncate_runes(&flatten_line(s), max)
}

/// One-line flattening (chat/editpicker.go:57 `flattenLine`): newlines/tabs become spaces,
/// control characters drop, whitespace runs collapse.
pub(crate) fn flatten_line(s: &str) -> String {
    let mapped: String = s
        .chars()
        .filter_map(|r| match r {
            '\n' | '\r' | '\t' => Some(' '),
            c if c < '\u{20}' || c == '\u{7f}' => None,
            c => Some(c),
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shapes the model pass's answer (chat/chat.go:197): `<think>` blocks stripped, the FIRST
/// line kept (a model that explains itself keeps its title line only), surrounding quotes
/// trimmed, then [`title_from`] at 80 runes.
pub(crate) fn sanitize_title(s: &str) -> String {
    let stripped = strip_think(s);
    let s = stripped.trim();
    let s = s.split('\n').next().unwrap_or(s);
    let s = s.trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”' | '「' | '」' | '`' | ' '));
    title_from(s, TITLE_CAP)
}

/// Removes `<think>…</think>` blocks (chat/chat.go:210): reasoning models behind chatcomp
/// relays leak them into plain content, and a chain of thought must never become the
/// session title. An UNCLOSED tag means everything after it is thought.
fn strip_think(s: &str) -> String {
    let mut s = s.to_owned();
    loop {
        let Some(open) = s.find("<think>") else {
            return s;
        };
        let Some(end) = s[open..].find("</think>") else {
            s.truncate(open);
            return s;
        };
        let after = open + end + "</think>".len();
        s = format!("{}{}", &s[..open], &s[after..]);
    }
}

/// The terminal-title text (chat/run.go:1844): a whitespace-only session title falls back
/// to the application name.
pub(crate) fn window_title(title: &str) -> String {
    if title.trim().is_empty() {
        crate::app::NAME.to_owned()
    } else {
        title.to_owned()
    }
}

/// The status line's model field (chat/run.go:1853): the provider type stands in until a
/// model is chosen.
pub(crate) fn status_model_label(model: &str, provider_type: &str) -> String {
    if model.is_empty() {
        provider_type.to_owned()
    } else {
        model.to_owned()
    }
}

/// Whether `input` only opens a READ-ONLY viewer — it never calls the provider and never
/// mutates the session writer, so it need not wait on the background title pass
/// (chat/chat.go:188-195).
///
/// The set is Go's four: `{/debug, /status, /tools, /skills}` (T-31 closed by T3, which
/// registered the two that were missing). It guards the "`/debug` is slow right after the
/// first chat" bug: a viewer that blocked on the ~1 s async title request would feel broken
/// for no reason. The match is the dispatch chain's own rule — the bare command or the
/// command followed by a space — so `/debugx` is a plain message and still waits.
pub fn is_read_only_viewer(input: &str) -> bool {
    ["/debug", "/status", "/tools", "/skills"].iter().any(|c| {
        input == *c
            || input
                .strip_prefix(*c)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

/// Asks the model for a title of `first_user` (chat/chat.go:175-183). A failed or empty
/// pass returns `""`, which [`SessionTitle::land`] drops — the placeholder is a complete
/// fallback on its own and there is no retry.
pub(crate) async fn generate_title_text(
    cancel: &CancellationToken,
    provider: &dyn Provider,
    first_user: &str,
) -> String {
    let prompt = format!(
        "Write a short title (at most 6 words, no quotes, no trailing punctuation) summarizing the user message below, in the same language the message uses. Return only the title itself:\n\n{}",
        truncate_runes(first_user, PROMPT_CAP)
    );
    match provider.chat(cancel, &[Message::user(prompt)]).await {
        Ok(r) => sanitize_title(&r.text),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! The session-title state machine and its helpers (`chat/titlestate_test.go` — all 13,
    //! `chat/title_test.go`, `chat/run_test.go`'s two label pins, and `chat/debug_test.go`'s
    //! read-only set).
    //!
    //! The state machine is crate-private (its only consumer is the run loop), so these tests
    //! live in-file (formerly a `#[path]`-mounted `tests/title.rs`; merged 2026-09-02).

    use std::sync::{Arc, Mutex, PoisonError};

    use crate::provider::ProviderKind;
    use crate::provider::model::{Attachment, Message};
    use crate::repl::title::{
        SessionTitle, WriterSlot, first_user_text, is_read_only_viewer, sanitize_title,
        status_model_label, title_from, window_title,
    };
    use crate::session::{NewSession, SessionStore, SessionWriter};
    use pretty_assertions::assert_eq;

    /// A title state wired to an in-memory writer plus a recording window sink, so a test can
    /// assert BOTH halves of every move (Go's `titleProbe`).
    struct Probe {
        /// Kept alive: the store's root is a temp dir the writers point into.
        _tmp: tempfile::TempDir,
        store: SessionStore,
        writer: WriterSlot,
        window: Arc<Mutex<Vec<String>>>,
        titler: SessionTitle,
    }

    impl Probe {
        fn new(resumed: bool) -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let store = SessionStore::new(tmp.path());
            let writer: WriterSlot = Arc::new(Mutex::new(Some(new_writer(&store))));
            let window = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&window);
            let titler = SessionTitle::new(
                Arc::clone(&writer),
                Box::new(move |s: &str| {
                    sink.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(s.to_owned());
                }),
                resumed,
            );
            Self {
                _tmp: tmp,
                store,
                writer,
                window,
                titler,
            }
        }

        /// The name on the CURRENT session bundle.
        fn name(&self) -> String {
            self.writer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map_or_else(String::new, |w| w.meta().title.clone())
        }

        fn last_window(&self) -> String {
            self.window
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last()
                .cloned()
                .unwrap_or_default()
        }

        fn window_writes(&self) -> usize {
            self.window
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }

        /// Swaps in a different bundle (`/session`) or drops the writer entirely (an ephemeral
        /// chat), returning what was there.
        fn set_writer(&self, w: Option<SessionWriter>) -> Option<SessionWriter> {
            std::mem::replace(
                &mut *self.writer.lock().unwrap_or_else(PoisonError::into_inner),
                w,
            )
        }

        fn mint(&self) -> SessionWriter {
            new_writer(&self.store)
        }
    }

    /// A pending bundle: `create` touches no disk, so the whole suite is in-memory.
    fn new_writer(store: &SessionStore) -> SessionWriter {
        store
            .create(NewSession::new(ProviderKind::OpenAi, "gpt-test"))
            .expect("create session writer")
    }

    fn user_turn(text: &str) -> Vec<Message> {
        vec![Message::user(text)]
    }

    // Naming at SEND time is the
    // whole point: a first turn that spends minutes in tool calls must not be poorly named
    // while it works, so the placeholder comes from the user's message alone and the same call
    // releases that message for the async pass.
    #[test]
    fn title_seeds_before_the_reply() {
        let p = Probe::new(false);
        let seeded = p.titler.seed(&user_turn("refactor the session writer"));
        let (first_user, _) = seeded.expect("seed must release the message for the model pass");
        assert_eq!(first_user, "refactor the session writer");
        assert_eq!(p.name(), "refactor the session writer");
        assert_eq!(p.last_window(), "refactor the session writer");
    }

    // Later messages never rename a
    // session, and the model pass fires only for the seed that newly named it.
    #[test]
    fn title_seed_only_once() {
        let p = Probe::new(false);
        let mut history = user_turn("first question");
        p.titler.seed(&history);
        history.push(Message::assistant("sure"));
        history.push(Message::user("second question"));

        assert!(
            p.titler.seed(&history).is_none(),
            "a second seed released another model pass"
        );
        assert_eq!(p.name(), "first question", "the FIRST message names it");
    }
    #[test]
    fn title_land_upgrades_the_placeholder() {
        let p = Probe::new(false);
        let (_, generation) = p
            .titler
            .seed(&user_turn("how do I profile Go allocations"))
            .expect("seeded");
        p.titler.land(generation, "Profiling Go allocations");
        assert_eq!(p.name(), "Profiling Go allocations");
        assert_eq!(p.last_window(), "Profiling Go allocations");
    }

    // A failed pass
    // changes nothing and there is no retry: the placeholder is a complete fallback.
    #[test]
    fn title_land_empty_keeps_the_placeholder() {
        let p = Probe::new(false);
        let (_, generation) = p.titler.seed(&user_turn("a question")).expect("seeded");
        p.titler.land(generation, "");
        assert_eq!(p.name(), "a question");
    }

    // A resumed bundle
    // arrives named; neither the placeholder nor a pass may take that away.
    #[test]
    fn title_resumed_session_untouched() {
        let p = Probe::new(true);
        p.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .expect("writer")
            .update_meta(|m| m.title = "an earlier chat".to_owned())
            .expect("seed the resumed title");

        assert!(
            p.titler.seed(&user_turn("new question")).is_none(),
            "a resumed session released a model pass"
        );
        assert_eq!(p.name(), "an earlier chat");
        assert_eq!(p.window_writes(), 0, "the window sink must stay untouched");
    }

    // A failed or discarded turn
    // takes its user message out of the history, and the name was derived from nothing else;
    // the pass that was racing the turn must not land late either.
    #[test]
    fn title_unseed_on_rollback() {
        let p = Probe::new(false);
        let history = user_turn("a question that errored");
        let (_, generation) = p.titler.seed(&history).expect("seeded");
        assert!(!p.name().is_empty());

        p.titler.unseed(&[]); // the turn rolled back
        assert_eq!(p.name(), "", "the name must be given back");
        assert_eq!(p.last_window(), "", "the window title must be cleared");

        p.titler.land(generation, "A Question That Errored");
        assert_eq!(p.name(), "", "a stale pass landed after the rollback");

        // And the NEXT message supplies both placeholder and pass.
        let (first_user, gen2) = p
            .titler
            .seed(&user_turn("what actually worked"))
            .expect("re-seed releases a fresh pass");
        assert_eq!(first_user, "what actually worked");
        p.titler.land(gen2, "What actually worked");
        assert_eq!(p.name(), "What actually worked");
    }

    // On a fast pass the
    // model title can arrive before the turn fails; the rollback takes it too, because the text
    // it summarized is gone either way.
    #[test]
    fn title_rollback_reverts_a_landed_title() {
        let p = Probe::new(false);
        let (_, generation) = p.titler.seed(&user_turn("a question")).expect("seeded");
        p.titler.land(generation, "A Model Title");
        p.titler.unseed(&[]);
        assert_eq!(p.name(), "");
    }

    // An interrupt that
    // kept partial output leaves the user message in place, so the name stays.
    #[test]
    fn title_unseed_keeps_a_surviving_turn() {
        let p = Probe::new(false);
        let mut history = user_turn("a question");
        p.titler.seed(&history);
        history.push(Message::assistant("partial…"));
        p.titler.unseed(&history);
        assert_eq!(p.name(), "a question");
    }

    // An explicit /save title outranks
    // the model pass (one in flight is dropped on landing) and no rollback strips it.
    #[test]
    fn title_adopt_name_wins() {
        let p = Probe::new(false);
        let (_, generation) = p.titler.seed(&user_turn("a question")).expect("seeded");
        p.titler.adopt_name("my chosen name");

        p.titler.land(generation, "Model Title");
        assert_eq!(p.name(), "my chosen name");
        p.titler.unseed(&[]);
        assert_eq!(
            p.name(),
            "my chosen name",
            "a chosen title survives rollback"
        );
    }

    // A --no-save chat has no
    // writer, but the window title is worth having; when /save mints a bundle, reapply hands it
    // the name the window already carries.
    #[test]
    fn title_ephemeral_names_the_window() {
        let p = Probe::new(false);
        p.set_writer(None); // ephemeral: nothing is persisting
        let (first_user, generation) = p.titler.seed(&user_turn("a question")).expect("seeded");
        assert_eq!(first_user, "a question");
        assert_eq!(p.last_window(), "a question");

        p.titler.land(generation, "A Model Title");
        assert_eq!(p.last_window(), "A Model Title");

        p.set_writer(Some(p.mint())); // /save mints the bundle mid-chat
        p.titler.reapply();
        assert_eq!(p.name(), "A Model Title");
    }

    // /save before any message
    // has nothing to hand over; the minted writer stays untouched for the next seed to name.
    #[test]
    fn title_reapply_without_a_name() {
        let p = Probe::new(false);
        p.titler.reapply();
        assert_eq!(p.name(), "");
    }

    // /session swaps the writer
    // mid-chat, so the state must read the CURRENT one: a captured handle would name the
    // session the user just left.
    #[test]
    fn title_follows_a_writer_swap() {
        let p = Probe::new(false);
        p.titler.seed(&user_turn("first chat"));
        let first = p.set_writer(Some(p.mint())).expect("the original writer");
        assert_eq!(first.meta().title, "first chat");

        p.titler.adopt(); // /session resumed another bundle
        p.titler.seed(&user_turn("first chat")); // adopted: a no-op either way
        assert_eq!(p.name(), "", "the swapped-in session must not be renamed");
    }

    // `land` arrives from the pass
    // task while the loop seeds and unseeds. Go ran it under -race; the Rust twin drives the
    // same interleaving across two threads against the mutex.
    #[test]
    fn title_land_races_the_loop() {
        let p = Arc::new(Probe::new(false));
        let racer = Arc::clone(&p);
        let done = std::thread::spawn(move || {
            for i in 0..100u64 {
                racer.titler.land(i % 3, "Racing Title");
            }
        });
        let history = user_turn("a question");
        for _ in 0..100 {
            p.titler.seed(&history);
            p.titler.unseed(&[]);
        }
        done.join().expect("racer thread");
    }

    // An attachment-only opener defers to the
    // next message that carries text: the name must never wait for the assistant.
    #[test]
    fn an_attachment_only_opener_defers_to_the_next_text() {
        assert_eq!(first_user_text(&[]), "");
        assert_eq!(first_user_text(&user_turn("draw a cat")), "draw a cat");
        let history = vec![
            Message::system("sys"),
            Message {
                attachments: vec![Attachment {
                    filename: "a.png".to_owned(),
                    ..Attachment::default()
                }],
                ..Message::default()
            },
            Message::assistant("what a nice picture"),
            Message::user("hi"),
        ];
        assert_eq!(first_user_text(&history), "hi");
    }

    // Every title entry point funnels through it: one
    // line, no control characters, capped on RUNE boundaries (a stored newline would break the
    // picker's row accounting and the window title).
    #[test]
    fn every_title_entry_point_flattens_caps_and_strips_controls() {
        for (name, input, want) in [
            (
                "multi-line prompt",
                "draw a cat\nwith a hat\nand boots",
                "draw a cat with a hat and boots",
            ),
            ("tabs and runs", "draw \t a\n\n  cat", "draw a cat"),
            ("control chars", "draw\x00 a\x07 cat", "draw a cat"),
            ("already one line", "draw a cat", "draw a cat"),
            ("blank", "   \n\t ", ""),
        ] {
            assert_eq!(title_from(input, 40), want, "{name}");
        }

        // The cap counts runes, so CJK is never cut mid-character.
        let long = "生成一张图片".repeat(10); // 60 runes
        let got = title_from(&long, 40);
        let runes: Vec<char> = got.chars().collect();
        assert_eq!(runes.len(), 41, "{got}");
        assert_eq!(runes[40], '…');
    }

    // The model's answer keeps its first-line
    // semantics (an explanatory second paragraph is not part of the title) and lands flattened.
    #[test]
    fn sanitize_title_keeps_the_first_line_flattened() {
        assert_eq!(
            sanitize_title("  \"Cat portrait\"  \n\nI chose this because…"),
            "Cat portrait"
        );
        assert_eq!(sanitize_title("Cat\tportrait"), "Cat portrait");
    }

    // Reasoning models behind chatcomp
    // relays leak <think> blocks into plain content; a chain of thought must never become the
    // session title.
    #[test]
    fn sanitize_title_strips_think() {
        for (name, input, want) in [
            (
                "block before title",
                "<think>\nthe user wants…\n</think>\nCat portrait",
                "Cat portrait",
            ),
            ("unclosed tag", "<think>never closed, all thought", ""),
            (
                "multiple blocks",
                "A<think>x</think>B<think>y</think>C",
                "ABC",
            ),
            ("no block", "Cat portrait", "Cat portrait"),
        ] {
            assert_eq!(sanitize_title(input), want, "{name}");
        }
    }

    // A whitespace-only title falls back to
    // the application name rather than blanking the terminal's tab.
    #[test]
    fn window_title_fallback() {
        assert_eq!(window_title("My chat"), "My chat");
        assert_eq!(window_title(""), "iota");
        assert_eq!(window_title("   \t "), "iota");
    }

    // The provider type stands in
    // until a model is chosen.
    #[test]
    fn status_model_label_falls_back_to_type() {
        assert_eq!(status_model_label("gpt-4o", "openai"), "gpt-4o");
        assert_eq!(status_model_label("", "openai"), "openai");
    }

    // A read-only viewer skips the title-pass
    // wait (the "/debug is slow after the first chat" fix). The FULL Go set, restored by T3 when
    // /debug and /skills registered (T-31): provider-touching and mutating commands — and plain
    // messages, and the near-miss `/debugx` prefix matching must not over-match — still wait.
    #[test]
    fn read_only_viewers_skip_the_title_pass_and_nothing_else_does() {
        for input in [
            "/debug",
            "/status",
            "/tools",
            "/skills",
            "/tools foo",
            "/debug on",
            "/skills brain-page do it",
            "/status ",
        ] {
            assert!(is_read_only_viewer(input), "{input} is a read-only viewer");
        }
        for input in [
            "/model",
            "/compact",
            "/session",
            "/file",
            "你好",
            "/debugx",
            "/statusx",
            "/toolsy",
            "/skillset",
            "plain text",
        ] {
            assert!(
                !is_read_only_viewer(input),
                "{input} must wait for the title pass"
            );
        }
    }
}
