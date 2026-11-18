//! The terminal's background, asked again while the loop runs (DIVERGENCES X-108).
//!
//! The startup detect (`super::osc::detect_background`) reads the tty itself, which is safe only
//! while nothing else does. Once the loop has the terminal, a second reader would take whatever
//! the user typed before the reply and could not hand it back — crossterm has no way to be fed
//! bytes. So the loop asks without reading: it writes [`super::osc::OSC11_QUERY`] and goes back
//! to its events, and the answer arrives in that same stream, behind everything typed before it.
//!
//! crossterm 0.29 has no event for an OSC reply. It parses `ESC ] 1 1 ; rgb:… BEL` as the keys
//! those bytes would be: `Alt+]`, then one character key a byte, then `Ctrl+G` (or `Alt+\` for an
//! ST terminator). [`ReplyMatcher`] recognizes exactly that run and takes it out of the stream;
//! every other event passes through in the order it came, which is the whole input-safety
//! argument: no byte is read anywhere but in crossterm, so no key can be swallowed, and the only
//! events withheld are the reply's own.
//!
//! [`Requery`] is the policy around it: when to ask (150 ms after the window regains focus, if
//! the user is not typing; and when the REPL asks, between turns), how long to wait, and that a
//! terminal which does not answer is never asked again.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use super::osc;

/// How long after `FocusGained` the terminal is asked: the click that focused the window, and
/// whatever the window system sends with it, are through by then.
pub(crate) const FOCUS_SETTLE: Duration = Duration::from_millis(150);

/// A focus check is skipped when a key or a paste arrived this recently: the user is typing, and
/// the question waits for the next turn.
pub(crate) const KEY_QUIET: Duration = Duration::from_millis(300);

/// How a reply begins, as bytes: `ESC ] 1 1 ;`.
const PREFIX: &[u8] = b"\x1b]11;";

/// More reply bytes than any colour takes (`rgba:ffff/ffff/ffff/ffff` is 24): past it the run is
/// not a reply.
const MAX_REPLY: usize = 64;

/// What [`ReplyMatcher::feed`] made of one event.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Fed {
    /// Events to handle now, oldest first: the event itself, or keys that were held as a possible
    /// reply start and turned out not to be one.
    pub(crate) pass: Vec<Event>,
    /// A whole reply was taken out of the stream: `Some(dark)`, or `None` when its colour did not
    /// parse.
    pub(crate) reply: Option<Option<bool>>,
}

/// Finds the OSC 11 reply among the key events crossterm makes of it (the module doc).
///
/// Idle until [`Self::expect`]: a session that never asks is never matched against. From then on
/// it stays armed until a reply has been taken — also past the deadline, so an answer that comes
/// late is still removed instead of being typed into the composer.
#[derive(Debug, Default)]
pub(crate) struct ReplyMatcher {
    /// Whether a query is out and its reply has not been seen.
    expecting: bool,
    /// The events held as a reply in the making.
    held: Vec<Event>,
    /// The bytes those events stand for.
    bytes: Vec<u8>,
    /// When the first of them arrived.
    since: Option<Instant>,
}

/// The bytes a key event stands for, when it can be part of a reply: a character, `ESC` + a
/// character (crossterm's `Alt`), `BEL` (`Ctrl+G`), or a lone `ESC`.
fn bytes_of(ev: &Event) -> Option<Vec<u8>> {
    let Event::Key(k) = ev else {
        return None;
    };
    if k.kind != KeyEventKind::Press {
        return None;
    }
    let plain = KeyModifiers::NONE;
    match k.code {
        KeyCode::Esc if k.modifiers == plain => Some(vec![0x1b]),
        KeyCode::Char(c) if c.is_ascii() => {
            let b = c as u8;
            let shiftless = k.modifiers - KeyModifiers::SHIFT;
            if shiftless == plain {
                Some(vec![b])
            } else if shiftless == KeyModifiers::ALT {
                Some(vec![0x1b, b])
            } else if k.modifiers == KeyModifiers::CONTROL && c == 'g' {
                Some(vec![0x07])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether `b` can stand inside a reply's colour: `rgb:` / `rgba:`, hex digits, `/`.
fn in_colour(b: u8) -> bool {
    b.is_ascii_hexdigit() || matches!(b, b'r' | b'g' | b':' | b'/')
}

impl ReplyMatcher {
    /// A query went out: its reply is to be taken from the stream.
    pub(crate) fn expect(&mut self) {
        self.expecting = true;
    }

    /// Whether the held events are inside a reply (the prefix matched), not just a possible start.
    fn in_reply(&self) -> bool {
        self.bytes.len() >= PREFIX.len()
    }

    /// Gives up on the held events: a possible start goes back into the stream, the fragment of a
    /// reply is dropped (after `ESC ] 1 1 ;` it is nobody's typing).
    fn give_up(&mut self, pass: &mut Vec<Event>) {
        if self.in_reply() {
            self.held.clear();
        } else {
            pass.append(&mut self.held);
        }
        self.bytes.clear();
        self.since = None;
    }

    /// The reply is whole: its colour, and the matcher idle again.
    fn complete(&mut self) -> Option<bool> {
        let dark = osc::parse_osc11_dark(&self.bytes);
        self.held.clear();
        self.bytes.clear();
        self.since = None;
        self.expecting = false;
        dark
    }

    /// Holds `ev` as the next piece of a reply.
    fn hold(&mut self, ev: Event, bytes: &[u8]) {
        self.since.get_or_insert_with(Instant::now);
        self.held.push(ev);
        self.bytes.extend_from_slice(bytes);
    }

    /// One event from the terminal: passed on, held, or the end of a reply.
    pub(crate) fn feed(&mut self, ev: Event) -> Fed {
        let mut fed = Fed::default();
        if !self.expecting || !matches!(ev, Event::Key(_)) {
            // A resize, a mouse report, a paste or a focus change is no part of a reply and
            // breaks none: it passes, and what is held stays held.
            fed.pass.push(ev);
            return fed;
        }
        let Some(b) = bytes_of(&ev) else {
            self.give_up(&mut fed.pass);
            fed.pass.push(ev);
            return fed;
        };
        if self.in_reply() {
            // A lone ESC is held as the first half of an ST; whatever follows ends the reply.
            let esc_pending = self.bytes.last() == Some(&0x1b);
            if esc_pending && b != b"\\" {
                fed.reply = Some(self.complete());
                fed.pass.push(ev);
            } else if esc_pending || b == [0x07] || b == b"\x1b\\" {
                fed.reply = Some(self.complete());
            } else if (b == [0x1b] || (b.len() == 1 && in_colour(b[0])))
                && self.bytes.len() < PREFIX.len() + MAX_REPLY
            {
                self.hold(ev, &b);
            } else {
                self.give_up(&mut fed.pass);
                fed.pass.push(ev);
            }
            return fed;
        }
        // The start: `Alt+]` as ONE event, then `1`, `1`, `;`. A lone ESC key is never held as a
        // start — it is the user's Esc far more often than half a split reply.
        let fits = |held: &[u8], b: &[u8]| {
            let all = [held, b].concat();
            all != [0x1b] && PREFIX.starts_with(&all)
        };
        if fits(&self.bytes, &b) {
            self.hold(ev, &b);
            return fed;
        }
        self.give_up(&mut fed.pass);
        if fits(&[], &b) {
            self.hold(ev, &b);
        } else {
            fed.pass.push(ev);
        }
        fed
    }

    /// When held events have waited [`osc::REPLY_TIMEOUT`] the rest is not coming: a possible
    /// start is handed back (a real `Alt+]` is delayed by that much, once a query is out), a
    /// reply's fragment is dropped.
    pub(crate) fn stale(&mut self, now: Instant) -> Vec<Event> {
        let mut pass = Vec::new();
        if self
            .since
            .is_some_and(|at| now.saturating_duration_since(at) >= osc::REPLY_TIMEOUT)
        {
            self.give_up(&mut pass);
        }
        pass
    }
}

/// When the loop asks the terminal for its background again, and what became of the question.
#[derive(Debug)]
pub(crate) struct Requery {
    matcher: ReplyMatcher,
    /// A question went unanswered (or this platform cannot ask): never again this session.
    off: bool,
    /// The focus check's time, when one is armed.
    due: Option<Instant>,
    /// When the question that is out was written.
    sent: Option<Instant>,
    /// A question to write this iteration.
    wanted: bool,
    /// When the last key or paste arrived.
    last_input: Option<Instant>,
    /// The REPL's callers waiting for the answer.
    waiters: Vec<oneshot::Sender<Option<bool>>>,
}

impl Default for Requery {
    fn default() -> Self {
        Self {
            matcher: ReplyMatcher::default(),
            // Elsewhere than unix the startup detect is declined too (`osc::round_trip`): the
            // console's input is not a byte stream a reply could arrive in.
            off: !cfg!(unix),
            due: None,
            sent: None,
            wanted: false,
            last_input: None,
            waiters: Vec::new(),
        }
    }
}

impl Requery {
    /// The window regained focus: the terminal is asked once things have settled.
    pub(crate) fn focus_gained(&mut self, now: Instant) {
        if !self.off {
            self.due = Some(now + FOCUS_SETTLE);
        }
    }

    /// A key or a paste arrived.
    pub(crate) fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// The REPL asks (between turns): answered at once with `None` when asking is off, otherwise
    /// with the reply — the one already on its way, if a question is out.
    pub(crate) fn ask(&mut self, reply: oneshot::Sender<Option<bool>>) {
        if self.off {
            let _ = reply.send(None);
            return;
        }
        self.waiters.push(reply);
        self.wanted = self.sent.is_none();
    }

    /// The timers: a due focus check asks unless the user is typing, and a question out for
    /// [`osc::REPLY_TIMEOUT`] is a miss.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.due.is_some_and(|at| now >= at) {
            self.due = None;
            let typing = self
                .last_input
                .is_some_and(|at| now.saturating_duration_since(at) < KEY_QUIET);
            if !typing && self.sent.is_none() {
                self.wanted = true;
            }
        }
        if self
            .sent
            .is_some_and(|at| now.saturating_duration_since(at) >= osc::REPLY_TIMEOUT)
        {
            self.miss();
        }
    }

    /// Whether the question is to be written now; the caller writes it.
    pub(crate) fn take_query(&mut self, now: Instant) -> bool {
        if !std::mem::take(&mut self.wanted) {
            return false;
        }
        self.sent = Some(now);
        self.matcher.expect();
        true
    }

    /// One event from the terminal ([`ReplyMatcher::feed`]).
    pub(crate) fn feed(&mut self, ev: Event) -> Fed {
        self.matcher.feed(ev)
    }

    /// Held events that are no reply after all ([`ReplyMatcher::stale`]).
    pub(crate) fn stale(&mut self, now: Instant) -> Vec<Event> {
        self.matcher.stale(now)
    }

    /// A reply was taken from the stream: the tone to adopt, when it is one. A reply that comes
    /// after its question was given up on is removed and ignored — the REPL was already told
    /// "no answer", and the frame must not move without it.
    pub(crate) fn answer(&mut self, reply: Option<bool>) -> Option<bool> {
        if self.off {
            return None;
        }
        let Some(dark) = reply else {
            self.miss();
            return None;
        };
        self.sent = None;
        for w in self.waiters.drain(..) {
            let _ = w.send(Some(dark));
        }
        Some(dark)
    }

    /// No answer: every waiter is told so, and nothing is asked again.
    fn miss(&mut self) {
        self.off = true;
        self.due = None;
        self.sent = None;
        self.wanted = false;
        for w in self.waiters.drain(..) {
            let _ = w.send(None);
        }
    }

    /// How long until [`Self::tick`] or [`Self::stale`] has something to do, when either does.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Duration> {
        let after = |at: Instant| (at + osc::REPLY_TIMEOUT).saturating_duration_since(now);
        [
            self.due.map(|at| at.saturating_duration_since(now)),
            self.sent.map(after),
            self.matcher.since.map(after),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{FOCUS_SETTLE, Fed, KEY_QUIET, ReplyMatcher, Requery};
    use crate::ui::runtime::osc::REPLY_TIMEOUT;
    use crate::ui::testutil::reply_events;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};
    use tokio::sync::oneshot;

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn alt(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
    }

    /// Feeds `events` and returns everything passed on and every reply taken.
    fn run(m: &mut ReplyMatcher, events: Vec<Event>) -> (Vec<Event>, Vec<Option<bool>>) {
        let (mut pass, mut replies) = (Vec::new(), Vec::new());
        for ev in events {
            let fed = m.feed(ev);
            pass.extend(fed.pass);
            replies.extend(fed.reply);
        }
        (pass, replies)
    }

    /// Without a question out nothing is matched: the very keys of a reply are the user's.
    #[test]
    fn an_idle_matcher_passes_everything() {
        let mut m = ReplyMatcher::default();
        let events = reply_events("rgb:ffff/ffff/ffff", "\x07");
        let (pass, replies) = run(&mut m, events.clone());
        assert_eq!(pass, events);
        assert!(replies.is_empty());
    }

    /// The reply is taken whole, in each form a terminal ends it, and the keys around it — typed
    /// before the terminal answered, and after — pass in order.
    #[test]
    fn a_reply_is_taken_and_the_keys_around_it_pass() {
        for (colour, end, want) in [
            ("rgb:ffff/ffff/ffff", "\x07", Some(false)),
            ("rgb:1e1e/1e1e/1e1e", "\x1b\\", Some(true)),
            ("rgba:FFFF/FFFF/FFFF/ffff", "\x07", Some(false)),
            ("rgb:ff/ff/ff", "\x1b\\", Some(false)),
            ("", "\x07", None),
        ] {
            let mut m = ReplyMatcher::default();
            m.expect();
            let mut events = vec![key('h'), key('i')];
            events.extend(reply_events(colour, end));
            events.push(key('!'));
            let (pass, replies) = run(&mut m, events);
            assert_eq!(pass, [key('h'), key('i'), key('!')], "{colour}");
            assert_eq!(replies, [want], "{colour}");
            // One question, one reply: the next such run is typing again.
            assert_eq!(m.feed(alt(']')).pass, [alt(']')]);
        }
    }

    /// An ST split by the terminal's writes arrives as a lone `Esc` and a `\`: still one reply. An
    /// `Esc` followed by anything else ends the reply and that key passes.
    #[test]
    fn a_split_terminator_ends_the_reply() {
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut body = reply_events("rgb:0000/0000/0000", "");
        body.push(esc.clone());
        for (last, passed) in [(key('\\'), vec![]), (key('x'), vec![key('x')])] {
            let mut m = ReplyMatcher::default();
            m.expect();
            let mut events = body.clone();
            events.push(last);
            let (pass, replies) = run(&mut m, events);
            assert_eq!(pass, passed);
            assert_eq!(replies, [Some(true)]);
        }
    }

    /// Keys that only begin like a reply are the user's: all of them come back, in order, as soon
    /// as one does not fit — and a lone `Esc` is never held at all.
    #[test]
    fn a_false_start_is_handed_back_in_order() {
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut m = ReplyMatcher::default();
        m.expect();
        assert_eq!(m.feed(esc.clone()).pass, [esc]);
        assert_eq!(m.feed(alt(']')), Fed::default());
        assert_eq!(m.feed(key('1')), Fed::default());
        assert_eq!(m.feed(key('z')).pass, [alt(']'), key('1'), key('z')]);
        // A start right behind a false start is still a start.
        assert_eq!(m.feed(alt(']')), Fed::default());
        let (pass, replies) = run(&mut m, reply_events("rgb:ffff/ffff/ffff", "\x07"));
        assert_eq!(pass, [alt(']')]);
        assert_eq!(replies, [Some(false)]);
    }

    /// A held start whose rest never comes goes back into the stream after the reply deadline; a
    /// reply's fragment is dropped instead. Other events pass meanwhile and break nothing.
    #[test]
    fn stale_holds_are_released() {
        let mut m = ReplyMatcher::default();
        m.expect();
        assert_eq!(m.feed(alt(']')), Fed::default());
        assert_eq!(m.feed(Event::FocusLost).pass, [Event::FocusLost]);
        let now = Instant::now();
        assert!(m.stale(now).is_empty());
        assert_eq!(m.stale(now + REPLY_TIMEOUT), [alt(']')]);

        let (pass, replies) = run(&mut m, reply_events("rgb:ffff/ff", ""));
        assert!(pass.is_empty() && replies.is_empty());
        assert!(m.stale(Instant::now() + REPLY_TIMEOUT).is_empty());
        // The fragment is gone, and the matcher still waits for a whole reply.
        let (pass, replies) = run(&mut m, reply_events("rgb:ffff/ffff/ffff", "\x07"));
        assert!(pass.is_empty());
        assert_eq!(replies, [Some(false)]);
    }

    /// The focus check: asked [`FOCUS_SETTLE`] after the focus, not while the user types.
    #[test]
    fn focus_asks_once_settled_and_not_while_typing() {
        let t0 = Instant::now();
        let mut r = Requery::default();
        r.focus_gained(t0);
        r.tick(t0 + FOCUS_SETTLE - Duration::from_millis(1));
        assert!(!r.take_query(t0));
        r.tick(t0 + FOCUS_SETTLE);
        assert!(r.take_query(t0 + FOCUS_SETTLE));
        assert!(!r.take_query(t0 + FOCUS_SETTLE), "one question");

        let mut r = Requery::default();
        r.input(t0);
        r.focus_gained(t0);
        r.tick(t0 + FOCUS_SETTLE);
        assert!(!r.take_query(t0 + FOCUS_SETTLE), "a key 150 ms ago");
        r.focus_gained(t0 + KEY_QUIET);
        r.tick(t0 + KEY_QUIET + FOCUS_SETTLE);
        assert!(r.take_query(t0 + KEY_QUIET + FOCUS_SETTLE));
    }

    /// A question nobody answers is the last one: the waiter hears `None`, later callers hear it
    /// at once, focus arms nothing — and the late reply is ignored.
    #[test]
    fn a_miss_is_final() {
        let t0 = Instant::now();
        let mut r = Requery::default();
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert!(r.take_query(t0));
        r.tick(t0 + REPLY_TIMEOUT - Duration::from_millis(1));
        assert!(rx.try_recv().is_err(), "still waiting");
        r.tick(t0 + REPLY_TIMEOUT);
        assert_eq!(rx.try_recv().unwrap(), None);

        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert_eq!(rx.try_recv().unwrap(), None);
        r.focus_gained(t0);
        r.tick(t0 + Duration::from_secs(1));
        assert!(!r.take_query(t0));
        assert_eq!(r.next_wake(t0), None);
        assert_eq!(r.answer(Some(false)), None);
    }

    /// One question serves every caller that asked while it was out.
    #[test]
    fn an_answer_reaches_every_waiter() {
        let t0 = Instant::now();
        let mut r = Requery::default();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        r.ask(tx1);
        assert!(r.take_query(t0));
        r.ask(tx2);
        assert!(!r.take_query(t0), "the question is already out");
        assert_eq!(r.answer(Some(false)), Some(false));
        assert_eq!(rx1.try_recv().unwrap(), Some(false));
        assert_eq!(rx2.try_recv().unwrap(), Some(false));
        // Answered, so the next focus asks again.
        r.focus_gained(t0);
        r.tick(t0 + FOCUS_SETTLE);
        assert!(r.take_query(t0 + FOCUS_SETTLE));
    }
}
