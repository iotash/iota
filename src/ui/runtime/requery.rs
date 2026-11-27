//! The terminal's background, asked again while the loop runs (DIVERGENCES X-108).
//!
//! The startup detect (`super::osc::detect_background`) reads the tty itself, which is safe only
//! while nothing else does: once the loop has the terminal, a second reader would take keys typed
//! before the reply, and crossterm cannot be handed them back. So the loop asks without reading:
//! it writes [`super::osc::OSC11_QUERY`], and the answer arrives in crossterm's own events.
//!
//! crossterm 0.29 has no event for an OSC reply: it parses `ESC ] 1 1 ; rgb:… BEL` as `Alt+]`, one
//! character key a byte, and `Ctrl+G` (`Alt+\` for an ST). [`ReplyMatcher`] swallows that run
//! while a reply is expected and passes every other event in the order it came — no byte is read
//! anywhere but in crossterm, so no key is lost. The cost: a hand-typed `Alt+]` (no binding) and
//! the keys right after it that could belong to a reply are dropped while a reply is expected.
//!
//! A run is only *started* inside the window a question opens; one that has got past `ESC ] 1 1 ;`
//! is followed to its terminator even once the window has passed, so a reply straddling the
//! window's end never has its `rgb:…` typed nor its `BEL` read as `Ctrl+G` (the editor key). The
//! same accepted cost as inside the window: a hand-typed `Alt+] 1 1 ;` begun in the window has
//! the keys after it that fit a reply swallowed past the window too, up to the first that cannot.
//!
//! One risk is accepted: a reply cut right after its first byte reaches the loop as a lone `Esc`
//! and then `]11;rgb:…` as typing. A lone `Esc` is never swallowed — it is the user's cancel key.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use super::osc;

/// How long after a miss a late reply may still start; past it no reply is started again (one
/// already started is followed to its end).
const MISS_GRACE: Duration = Duration::from_secs(1);

/// How a reply begins, as bytes: `ESC ] 1 1 ;`.
const PREFIX: &[u8] = b"\x1b]11;";

/// More colour bytes than any reply has (`rgba:ffff/ffff/ffff/ffff` is 24).
const MAX_REPLY: usize = 64;

/// The bytes a key stands for in a reply: a character, `ESC` + one (`Alt`), `BEL` (`Ctrl+G`), ESC.
fn bytes_of(ev: &Event) -> Vec<u8> {
    let Event::Key(k) = ev else { return vec![] };
    let shiftless = k.modifiers - KeyModifiers::SHIFT;
    match k.code {
        _ if k.kind != KeyEventKind::Press => vec![],
        KeyCode::Esc if k.modifiers.is_empty() => vec![0x1b],
        KeyCode::Char(c) if !c.is_ascii() => vec![],
        KeyCode::Char(c) if shiftless.is_empty() => vec![c as u8],
        KeyCode::Char(c) if shiftless == KeyModifiers::ALT => vec![0x1b, c as u8],
        KeyCode::Char('g') if k.modifiers == KeyModifiers::CONTROL => vec![0x07],
        _ => vec![],
    }
}

/// Whether `bytes` can begin a reply. A lone `ESC` cannot: it is far more often the user's `Esc`.
fn starts(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes != [0x1b] && PREFIX.starts_with(bytes)
}

/// Swallows the reply's keys (the module doc) from [`Self::expect`] until a reply is taken or an
/// instant passes — the deadline plus [`MISS_GRACE`], compared in [`Self::feed`]: no timer.
#[derive(Debug, Default)]
struct ReplyMatcher {
    until: Option<Instant>,
    /// The bytes of the keys swallowed so far.
    bytes: Vec<u8>,
}

impl ReplyMatcher {
    fn expect(&mut self, until: Instant) {
        self.until = Some(until);
        self.bytes.clear();
    }

    fn complete(&mut self) -> Vec<u8> {
        self.until = None;
        std::mem::take(&mut self.bytes)
    }

    /// One event: itself unless swallowed, and the reply's bytes when it ended one. An event that
    /// is no key passes and breaks no reply (a resize may land between two reads of one).
    fn feed(&mut self, ev: Event, now: Instant) -> (Option<Event>, Option<Vec<u8>>) {
        if !matches!(ev, Event::Key(_)) {
            return (Some(ev), None);
        }
        let open = self.until.is_some_and(|at| now <= at);
        let body = self.bytes.len() >= PREFIX.len();
        if !open && !body {
            // Past the window only a run already started is followed (the module doc).
            self.bytes.clear();
            return (Some(ev), None);
        }
        let b = bytes_of(&ev);
        if body && self.bytes.ends_with(&[0x1b]) {
            // The first half of an ST: a `\` ends the reply, any other key ends it and passes.
            return ((b != b"\\").then_some(ev), Some(self.complete()));
        }
        if body && (b == [0x07] || b == b"\x1b\\") {
            return (None, Some(self.complete()));
        }
        let fits = if body {
            matches!(b[..], [c] if c == 0x1b || c.is_ascii_hexdigit() || b"rg:/".contains(&c))
                && self.bytes.len() < PREFIX.len() + MAX_REPLY
        } else {
            !b.is_empty() && starts(&[&self.bytes[..], &b].concat())
        };
        if !fits {
            // A key that cannot belong ends the run; it may begin another, or it is the user's.
            self.bytes.clear();
        }
        if fits || (open && starts(&b)) {
            self.bytes.extend_from_slice(&b);
            return (None, None);
        }
        (Some(ev), None)
    }
}

/// When the terminal is asked (once on the session's first key, on focus, and when the REPL asks
/// between turns): one that does not answer is not asked again, one whose host answers for it
/// never.
#[derive(Debug, Default)]
pub(crate) struct Requery {
    matcher: ReplyMatcher,
    /// A question went unanswered, or a host answers: never again this session.
    missed: bool,
    /// When the question that is out was written.
    sent: Option<Instant>,
    /// A question to write this iteration.
    wanted: bool,
    /// The REPL's call waiting for the answer (`run.rs` asks once per turn, in order).
    waiter: Option<oneshot::Sender<Option<bool>>>,
    /// A key has come: [`Self::key_seen`] asks nothing again.
    keyed: bool,
}

impl Requery {
    /// Whether asking is over. Off unix it never starts: the console's input carries no reply.
    fn off(&self) -> bool {
        self.missed || !cfg!(unix)
    }

    /// The window regained focus: the terminal is asked this iteration, unless a question is out.
    pub(crate) fn focus_gained(&mut self) {
        self.wanted |= !self.off() && self.sent.is_none();
    }

    /// A key the user typed: the first of the session asks the terminal this iteration — a
    /// multiplexer answers for itself until its viewer is in the path (herdr-core: its own dark
    /// default), and a keystroke is the first evidence that someone is looking. ONE ask for the
    /// session, never when asking is off; a question already out stands for it.
    pub(crate) fn key_seen(&mut self) {
        if !std::mem::replace(&mut self.keyed, true) && !self.off() {
            tracing::debug!("theme requery: first-key question");
            self.wanted |= self.sent.is_none();
        }
    }

    /// Whether a question is out and its answer not yet in: the terminal is not lent meanwhile.
    pub(crate) fn awaiting(&self) -> bool {
        self.sent.is_some()
    }

    /// The REPL asks: `None` at once when asking is off, otherwise the reply — to the question
    /// already out, if there is one.
    pub(crate) fn ask(&mut self, reply: oneshot::Sender<Option<bool>>) {
        if self.off() {
            let _ = reply.send(None);
            return;
        }
        self.waiter = Some(reply);
        self.wanted = self.sent.is_none();
    }

    /// The reply deadline: a question out for [`osc::REPLY_TIMEOUT`] is a miss.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.sent.is_some_and(|at| now >= at + osc::REPLY_TIMEOUT) {
            self.miss();
        }
    }

    /// A host answers for the background (cmux): the two must never flip the frame in turn.
    pub(crate) fn leave_to_host(&mut self) {
        self.miss();
    }

    /// Whether the question is to be written now; the caller writes it.
    pub(crate) fn take_query(&mut self, now: Instant) -> bool {
        if !std::mem::take(&mut self.wanted) {
            return false;
        }
        self.sent = Some(now);
        self.matcher.expect(now + osc::REPLY_TIMEOUT + MISS_GRACE);
        true
    }

    /// One event: itself unless it was the reply's, and the tone to adopt when it completed one. A
    /// late reply is ignored (the REPL heard "no answer"); one that does not parse is a miss.
    pub(crate) fn feed(&mut self, ev: Event, now: Instant) -> (Option<Event>, Option<bool>) {
        let (pass, reply) = self.matcher.feed(ev, now);
        let Some(reply) = reply.filter(|_| !self.off()) else {
            return (pass, None);
        };
        let after = self.sent.map(|at| at.elapsed());
        let Some(dark) = osc::parse_osc11_dark(&reply) else {
            tracing::debug!(reply = %String::from_utf8_lossy(&reply).escape_debug(), ?after, "theme requery: unparsable reply, miss");
            self.miss();
            return (pass, None);
        };
        tracing::debug!(
            tone = if dark { "dark" } else { "light" },
            reply = %String::from_utf8_lossy(&reply).escape_debug(),
            ?after,
            "theme requery: answer"
        );
        self.sent = None;
        if let Some(w) = self.waiter.take() {
            let _ = w.send(Some(dark));
        }
        (pass, Some(dark))
    }

    /// No answer: the waiter is told so, and nothing is asked again.
    fn miss(&mut self) {
        if !self.missed {
            tracing::debug!(after = ?self.sent.map(|at| at.elapsed()), "theme requery: miss");
        }
        self.missed = true;
        self.sent = None;
        self.wanted = false;
        if let Some(w) = self.waiter.take() {
            let _ = w.send(None);
        }
    }

    /// How long until [`Self::tick`] has something to do: the reply deadline.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Duration> {
        let at = self.sent? + osc::REPLY_TIMEOUT;
        Some(at.saturating_duration_since(now))
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{MISS_GRACE, Requery};
    use crate::ui::runtime::osc::REPLY_TIMEOUT;
    use crate::ui::testutil::reply_events;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::time::Instant;
    use tokio::sync::oneshot;

    fn ch(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn esc() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    /// One plain key a character, `\x1b` standing for `Esc`.
    fn keys(s: &str) -> Vec<Event> {
        s.chars()
            .map(|c| if c == '\x1b' { esc() } else { ch(c) })
            .collect()
    }

    /// A policy with a question out since `t0`.
    fn asked(t0: Instant) -> Requery {
        let mut r = Requery::default();
        r.focus_gained();
        assert!(r.take_query(t0));
        r
    }

    /// Feeds `events` at `now`: everything passed on, and every tone adopted.
    fn feed(r: &mut Requery, events: Vec<Event>, now: Instant) -> (Vec<Event>, Vec<bool>) {
        let (mut pass, mut tones) = (Vec::new(), Vec::new());
        for ev in events {
            let (p, t) = r.feed(ev, now);
            pass.extend(p);
            tones.extend(t);
        }
        (pass, tones)
    }

    /// Without a question out nothing is matched: the very keys of a reply are the user's.
    #[test]
    fn nothing_is_matched_before_a_question() {
        let events = reply_events("rgb:ffff/ffff/ffff", "\x07");
        let got = feed(&mut Requery::default(), events.clone(), Instant::now());
        assert_eq!(got, (events, vec![]));
    }

    /// The reply is swallowed whole in each form a terminal ends it — a split ST too, and an
    /// `Esc` followed by another key, which passes — while the keys and events around it and an
    /// event inside it pass in order. One question takes one reply.
    #[test]
    fn a_reply_is_swallowed_and_the_keys_around_it_pass() {
        for (colour, end, tail, passed, want) in [
            ("rgb:ffff/ffff/ffff", "\x07", "!", "!", Some(false)),
            ("rgb:1e1e/1e1e/1e1e", "\x1b\\", "!", "!", Some(true)),
            ("rgba:FFFF/FFFF/FFFF/ffff", "\x07", "", "", Some(false)),
            ("rgb:0000/0000/0000", "", "\x1b\\!", "!", Some(true)),
            ("rgb:0000/0000/0000", "", "\x1bx", "x", Some(true)),
            ("", "\x07", "!", "!", None),
        ] {
            let t0 = Instant::now();
            let mut r = asked(t0);
            let mut events = vec![ch('h'), Event::Paste("p".to_owned())];
            events.extend(reply_events(colour, end));
            events.insert(6, Event::FocusLost); // after `ESC ] 1 1 ;`
            events.extend(keys(tail));
            let mut want_pass = vec![ch('h'), Event::Paste("p".to_owned()), Event::FocusLost];
            want_pass.extend(keys(passed));
            let want = (want_pass, want.into_iter().collect());
            assert_eq!(feed(&mut r, events, t0), want, "{colour}");
            let again = reply_events("rgb:ffff/ffff/ffff", "\x07");
            assert_eq!(
                feed(&mut r, again.clone(), t0).0,
                again,
                "one question, one reply"
            );
        }
    }

    /// A run that only begins like a reply is swallowed up to the first key that cannot belong,
    /// which passes; a lone `Esc` is never swallowed, and a new start inside a run is a start.
    #[test]
    fn a_false_start_is_swallowed_up_to_the_key_that_cannot_belong() {
        let alt = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
        let t0 = Instant::now();
        let mut r = asked(t0);
        let events = vec![esc(), alt.clone(), ch('1'), ch('z'), alt.clone()];
        assert_eq!(feed(&mut r, events, t0), (vec![esc(), ch('z')], vec![]));
        // A key that stands for no byte (Enter here) ends a start too, and is the user's.
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let (pass, _) = feed(&mut r, vec![enter.clone(), alt.clone(), alt], t0);
        assert_eq!(pass, [enter]);
        let rest = reply_events("rgb:ffff/ffff/ffff", "\x07").split_off(1);
        assert_eq!(feed(&mut r, rest, t0), (vec![], vec![false]));
    }

    /// A question nobody answers is the last one: the waiter hears `None`, later calls hear it at
    /// once, focus asks nothing, nothing wakes the loop. A late reply is swallowed and ignored
    /// up to [`MISS_GRACE`] after the deadline; past it the same keys are the user's.
    #[test]
    fn a_miss_is_final() {
        let t0 = Instant::now();
        let mut r = asked(t0);
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        r.tick(t0 + REPLY_TIMEOUT / 2);
        assert!(rx.try_recv().is_err(), "still waiting");
        r.tick(t0 + REPLY_TIMEOUT);
        assert_eq!(rx.try_recv().unwrap(), None);
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert_eq!(rx.try_recv().unwrap(), None);
        r.focus_gained();
        assert!(!r.take_query(t0));
        assert_eq!(r.next_wake(t0), None);

        let late = reply_events("rgb:ffff/ffff/ffff", "\x07");
        let grace = t0 + REPLY_TIMEOUT + MISS_GRACE;
        assert_eq!(feed(&mut r, late.clone(), grace), (vec![], vec![]));
        let mut r = asked(t0);
        r.tick(t0 + REPLY_TIMEOUT);
        let past = grace + REPLY_TIMEOUT;
        assert_eq!(
            feed(&mut r, late.clone(), past),
            (late, vec![]),
            "the user's"
        );
    }

    /// A host that knows the background ends the asking: focus asks nothing, the REPL's call
    /// hears `None` at once.
    #[test]
    fn a_host_that_knows_ends_the_asking() {
        let mut r = Requery::default();
        r.leave_to_host();
        r.focus_gained();
        assert!(!r.take_query(Instant::now()));
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert_eq!(rx.try_recv().unwrap(), None);
    }

    /// The REPL's call is answered with the reply; a call made while the question is out asks
    /// nothing more, and a terminal that answered is asked again.
    #[test]
    fn the_reply_answers_the_call() {
        let t0 = Instant::now();
        let mut r = asked(t0);
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert!(!r.take_query(t0), "the question is already out");
        feed(&mut r, reply_events("rgb:ffff/ffff/ffff", "\x07"), t0);
        assert_eq!(rx.try_recv().unwrap(), Some(false));
        assert_eq!(r.next_wake(t0), None);
        r.focus_gained();
        assert!(r.take_query(t0));
    }

    /// The session's first key asks ONE question, in the iteration it came: its reply is taken
    /// like any other, and no later key asks again. A question already out stands for it.
    #[test]
    fn the_first_key_asks_once() {
        let t0 = Instant::now();
        let mut r = Requery::default();
        assert!(!r.take_query(t0), "nothing before a key");
        r.key_seen();
        assert!(r.take_query(t0), "the first key's question");
        assert_eq!(r.next_wake(t0), Some(REPLY_TIMEOUT));
        let got = feed(&mut r, reply_events("rgb:ffff/ffff/ffff", "\x07"), t0);
        assert_eq!(got, (vec![], vec![false]), "applied like any reply");
        assert_eq!(r.next_wake(t0), None);
        r.key_seen();
        assert!(!r.take_query(t0), "one ask for the session");

        let mut r = asked(t0);
        r.key_seen();
        assert!(!r.take_query(t0), "the question out stands for it");
        feed(&mut r, reply_events("rgb:ffff/ffff/ffff", "\x07"), t0);
        r.key_seen();
        assert!(!r.take_query(t0));
    }

    /// With the asking off — a host that answers, or a miss — the first key asks nothing.
    #[test]
    fn the_first_key_asks_nothing_when_asking_is_off() {
        let t0 = Instant::now();
        let mut host = Requery::default();
        host.leave_to_host();
        let mut missed = asked(t0);
        missed.tick(t0 + REPLY_TIMEOUT);
        for mut r in [host, missed] {
            r.key_seen();
            assert!(!r.take_query(t0));
            assert_eq!(r.next_wake(t0), None);
        }
    }

    /// A reply that started inside the window is followed to its terminator past it: none of its
    /// keys — its `BEL`, `Ctrl+G`, least of all — reaches the user's, and it is ignored after a
    /// miss. Past the window a lone `Esc` passes, and so does a start begun only by `Alt+]`.
    #[test]
    fn a_reply_started_in_the_window_is_swallowed_past_it() {
        let t0 = Instant::now();
        let grace = t0 + REPLY_TIMEOUT + MISS_GRACE;
        let past = grace + REPLY_TIMEOUT;
        for missed in [true, false] {
            let mut r = asked(t0);
            if missed {
                r.tick(t0 + REPLY_TIMEOUT);
            }
            let mut late = reply_events("rgb:ffff/ffff/ffff", "\x07");
            let tail = late.split_off(8); // `ESC ] 1 1 ; rgb:` in the window, the rest past it
            assert_eq!(feed(&mut r, late, grace), (vec![], vec![]));
            let want = if missed { vec![] } else { vec![false] };
            assert_eq!(feed(&mut r, tail, past), (vec![], want), "missed {missed}");
            assert_eq!(feed(&mut r, vec![esc()], past).0, [esc()], "a lone Esc");
        }
        let mut r = asked(t0);
        r.tick(t0 + REPLY_TIMEOUT);
        let mut late = reply_events("rgb:ffff/ffff/ffff", "\x07");
        let tail = late.split_off(1);
        assert_eq!(feed(&mut r, late, grace), (vec![], vec![]));
        assert_eq!(
            feed(&mut r, tail.clone(), past),
            (tail, vec![]),
            "not started"
        );
    }
}
