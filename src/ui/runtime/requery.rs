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
//! ST terminator). [`ReplyMatcher`] swallows that run while a reply is expected; every other
//! event passes through in the order it came, which is the whole input-safety argument: no byte
//! is read anywhere but in crossterm, so there is no second reader to take a key. What it costs:
//! an `Alt+]` typed while a reply is expected (that chord has no binding), and the keys right
//! after it that could belong to a reply, are dropped.
//!
//! One residual risk is accepted: a reply whose first byte, `ESC`, is the last byte of a read
//! reaches the loop as a lone `Esc` key and then `]11;rgb:…` as typing. The matcher never
//! swallows a lone `Esc` — it is the user's cancel key — so that `Esc` cancels whatever is
//! innermost and the rest lands in the composer. It needs a terminal (or a transport) that cuts
//! the reply exactly after its first byte.
//!
//! [`Requery`] is the policy around it: when to ask (when the window regains focus, and when the
//! REPL asks, between turns), how long to wait, that a terminal which does not answer is never
//! asked again, and that one whose host answers for it is never asked at all.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use super::osc;

/// How long after a miss a late reply is still looked for: within it the reply is swallowed
/// instead of typed; past it nothing is matched for the rest of the session.
pub(crate) const MISS_GRACE: Duration = Duration::from_secs(1);

/// How a reply begins, as bytes: `ESC ] 1 1 ;`.
const PREFIX: &[u8] = b"\x1b]11;";

/// More reply bytes than any colour takes (`rgba:ffff/ffff/ffff/ffff` is 24): past it the run is
/// not a reply.
const MAX_REPLY: usize = 64;

/// What [`ReplyMatcher::feed`] made of one event.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Fed {
    /// The event, when it is to be handled; `None` when it was swallowed as part of a reply.
    pub(crate) pass: Option<Event>,
    /// A whole reply was taken out of the stream.
    pub(crate) reply: Option<Reply>,
}

/// The terminal's reply, taken whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    /// Its colour, as a tone.
    Tone {
        /// Whether the background is dark.
        dark: bool,
    },
    /// A reply whose colour did not parse.
    Unreadable,
}

impl Reply {
    /// The tone it names, when it names one.
    pub(crate) fn dark(self) -> Option<bool> {
        match self {
            Self::Tone { dark } => Some(dark),
            Self::Unreadable => None,
        }
    }
}

/// Finds the OSC 11 reply among the key events crossterm makes of it (the module doc).
///
/// Armed by [`Self::expect`] until one instant — the question's deadline plus [`MISS_GRACE`] —
/// or until a reply has been taken: a session that never asks, or whose terminal never answers,
/// is not matched against outside that window.
#[derive(Debug, Default)]
pub(crate) struct ReplyMatcher {
    /// Until when a reply is looked for.
    until: Option<Instant>,
    /// The bytes of the keys swallowed so far as the reply.
    bytes: Vec<u8>,
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

/// Whether `bytes` can begin a reply. A lone `ESC` cannot: it is the user's `Esc` far more often
/// than half a split reply.
fn starts(bytes: &[u8]) -> bool {
    bytes != [0x1b] && PREFIX.starts_with(bytes)
}

impl ReplyMatcher {
    /// A query went out: its reply is to be taken from the stream until `until`.
    pub(crate) fn expect(&mut self, until: Instant) {
        self.until = Some(until);
        self.bytes.clear();
    }

    /// The reply is whole: its colour, and the matcher idle again.
    fn complete(&mut self) -> Reply {
        let reply = osc::parse_osc11_dark(&self.bytes)
            .map_or(Reply::Unreadable, |dark| Reply::Tone { dark });
        self.bytes.clear();
        self.until = None;
        reply
    }

    /// One event from the terminal at `now`: passed on, swallowed, or the end of a reply.
    pub(crate) fn feed(&mut self, ev: Event, now: Instant) -> Fed {
        if self.until.is_none_or(|at| now > at) {
            self.until = None;
            self.bytes.clear();
            return Fed {
                pass: Some(ev),
                reply: None,
            };
        }
        // A resize, a mouse report, a paste or a focus change is no part of a reply and breaks
        // none: the reply's keys are one run of the terminal's bytes, and a resize is a signal
        // that may land between two reads of it.
        if !matches!(ev, Event::Key(_)) {
            return Fed {
                pass: Some(ev),
                reply: None,
            };
        }
        let b = bytes_of(&ev);
        if self.bytes.len() >= PREFIX.len() {
            // A lone ESC was swallowed as the first half of an ST; whatever follows ends the
            // reply, and anything but the `\` is the user's.
            if self.bytes.last() == Some(&0x1b) {
                let st = b.as_deref() == Some(b"\\");
                return Fed {
                    pass: (!st).then_some(ev),
                    reply: Some(self.complete()),
                };
            }
            match b.as_deref() {
                Some([0x07] | [0x1b, b'\\']) => {
                    return Fed {
                        pass: None,
                        reply: Some(self.complete()),
                    };
                }
                Some([c])
                    if (*c == 0x1b || in_colour(*c))
                        && self.bytes.len() < PREFIX.len() + MAX_REPLY =>
                {
                    self.bytes.push(*c);
                    return Fed::default();
                }
                _ => {}
            }
        } else if let Some(b) = b {
            // The start: `Alt+]` as ONE event, then `1`, `1`, `;`. A key that does not continue a
            // start may begin one.
            if !starts(&[&self.bytes[..], &b].concat()) {
                self.bytes.clear();
            }
            if starts(&[&self.bytes[..], &b].concat()) {
                self.bytes.extend_from_slice(&b);
                return Fed::default();
            }
        }
        // A key that cannot belong to a reply ends the run, and is the user's.
        self.bytes.clear();
        Fed {
            pass: Some(ev),
            reply: None,
        }
    }
}

/// When the loop asks the terminal for its background again, and what became of the question.
#[derive(Debug)]
pub(crate) struct Requery {
    matcher: ReplyMatcher,
    /// A question went unanswered (or this platform cannot ask): never again this session.
    off: bool,
    /// When the question that is out was written.
    sent: Option<Instant>,
    /// A question to write this iteration.
    wanted: bool,
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
            sent: None,
            wanted: false,
            waiters: Vec::new(),
        }
    }
}

impl Requery {
    /// The window regained focus: the terminal is asked this iteration, unless a question is out.
    pub(crate) fn focus_gained(&mut self) {
        self.wanted |= !self.off && self.sent.is_none();
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

    /// The reply deadline: a question out for [`osc::REPLY_TIMEOUT`] is a miss.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self
            .sent
            .is_some_and(|at| now.saturating_duration_since(at) >= osc::REPLY_TIMEOUT)
        {
            self.miss();
        }
    }

    /// A host answers for the background (cmux, asked by the REPL before every turn): the
    /// terminal is not asked at all, so the two can never disagree on screen — a focus check
    /// flipping the frame one way and the next turn flipping it back.
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

    /// One event from the terminal ([`ReplyMatcher::feed`]).
    pub(crate) fn feed(&mut self, ev: Event, now: Instant) -> Fed {
        self.matcher.feed(ev, now)
    }

    /// A reply was taken from the stream: the tone to adopt, when it is one. A reply that comes
    /// after its question was given up on is removed and ignored — the REPL was already told
    /// "no answer", and the frame must not move without it.
    pub(crate) fn answer(&mut self, reply: Reply) -> Option<bool> {
        if self.off {
            return None;
        }
        let Some(dark) = reply.dark() else {
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
        self.sent = None;
        self.wanted = false;
        for w in self.waiters.drain(..) {
            let _ = w.send(None);
        }
    }

    /// How long until [`Self::tick`] has something to do: the reply deadline, while a question is
    /// out.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Duration> {
        self.sent
            .map(|at| (at + osc::REPLY_TIMEOUT).saturating_duration_since(now))
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{MISS_GRACE, Reply, ReplyMatcher, Requery};
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

    /// A matcher expecting a reply for the next minute.
    fn expecting() -> ReplyMatcher {
        let mut m = ReplyMatcher::default();
        m.expect(Instant::now() + Duration::from_secs(60));
        m
    }

    /// Feeds `events` and returns everything passed on and every reply taken.
    fn run(m: &mut ReplyMatcher, events: Vec<Event>) -> (Vec<Event>, Vec<Option<bool>>) {
        let (mut pass, mut replies) = (Vec::new(), Vec::new());
        for ev in events {
            let fed = m.feed(ev, Instant::now());
            pass.extend(fed.pass);
            replies.extend(fed.reply.map(Reply::dark));
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
            let mut m = expecting();
            let mut events = vec![key('h'), key('i')];
            events.extend(reply_events(colour, end));
            events.push(key('!'));
            let (pass, replies) = run(&mut m, events);
            assert_eq!(pass, [key('h'), key('i'), key('!')], "{colour}");
            assert_eq!(replies, [want], "{colour}");
            // One question, one reply: the next such run is typing again.
            assert_eq!(m.feed(alt(']'), Instant::now()).pass, Some(alt(']')));
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
            let mut m = expecting();
            let mut events = body.clone();
            events.push(last);
            let (pass, replies) = run(&mut m, events);
            assert_eq!(pass, passed);
            assert_eq!(replies, [Some(true)]);
        }
    }

    /// A run that only begins like a reply is swallowed up to the first key that cannot belong,
    /// which passes; a lone `Esc` is never swallowed, and an event that is no key passes without
    /// breaking a reply.
    #[test]
    fn a_false_start_is_swallowed_up_to_the_key_that_cannot_belong() {
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut m = expecting();
        let (pass, _) = run(&mut m, vec![esc.clone(), alt(']'), key('1'), key('z')]);
        assert_eq!(pass, [esc, key('z')]);
        // A start right behind a false start is still a start, and a paste inside it passes.
        let mut events = vec![alt(']'), alt(']')];
        events.extend(reply_events("rgb:ffff/ffff/ffff", "\x07").split_off(1));
        events.insert(8, Event::Paste("p".to_owned()));
        let (pass, replies) = run(&mut m, events);
        assert_eq!(pass, [Event::Paste("p".to_owned())]);
        assert_eq!(replies, [Some(false)]);
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
        r.tick(t0 + (REPLY_TIMEOUT / 2));
        assert!(rx.try_recv().is_err(), "still waiting");
        r.tick(t0 + REPLY_TIMEOUT);
        assert_eq!(rx.try_recv().unwrap(), None);
        assert_eq!(r.next_wake(t0), None, "nothing left to wake for");

        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert_eq!(rx.try_recv().unwrap(), None);
        r.focus_gained();
        assert!(!r.take_query(t0));
        assert_eq!(r.answer(Reply::Tone { dark: false }), None);
    }

    /// After a miss the reply is still looked for, for [`MISS_GRACE`]: one that comes late is
    /// taken out of the stream (and ignored). Past the grace the same keys are the user's again.
    #[test]
    fn a_late_reply_is_removed_within_the_grace_and_typing_after_it() {
        let reply = || reply_events("rgb:ffff/ffff/ffff", "\x07");
        let t0 = Instant::now();
        let missed = t0 + REPLY_TIMEOUT;
        for (at, typed) in [
            (missed + MISS_GRACE, false),
            (missed + MISS_GRACE * 2, true),
        ] {
            let mut r = Requery::default();
            r.focus_gained();
            assert!(r.take_query(t0));
            r.tick(missed);
            let (mut passed, mut taken) = (Vec::new(), None);
            for ev in reply() {
                let fed = r.feed(ev, at);
                passed.extend(fed.pass);
                taken = taken.or(fed.reply);
            }
            if typed {
                assert_eq!(passed, reply(), "past the grace the keys are the user's");
            } else {
                assert!(passed.is_empty(), "the late reply was typed");
                assert_eq!(r.answer(taken.unwrap()), None, "and it is not obeyed");
            }
        }
    }

    /// A host that knows the background takes the question over: focus asks nothing, the REPL's
    /// call hears `None` at once.
    #[test]
    fn a_host_that_knows_ends_the_asking() {
        let t0 = Instant::now();
        let mut r = Requery::default();
        r.leave_to_host();
        r.focus_gained();
        assert!(!r.take_query(t0));
        let (tx, mut rx) = oneshot::channel();
        r.ask(tx);
        assert_eq!(rx.try_recv().unwrap(), None);
        assert_eq!(r.next_wake(t0), None);
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
        assert_eq!(r.answer(Reply::Tone { dark: false }), Some(false));
        assert_eq!(rx1.try_recv().unwrap(), Some(false));
        assert_eq!(rx2.try_recv().unwrap(), Some(false));
        // Answered, so the next focus asks again.
        r.focus_gained();
        assert!(r.take_query(t0));
    }
}
