//! The notice gate (DIVERGENCES X-84, docs/design/rate-limit-and-interrupt.md §3): whether a finished
//! background job's notice may open a turn of its own. A turn the user interrupted, or one that ended in
//! 429, leaves a [`Hold`]; while it holds, the loop puts a notice `read_input` delivers aside here instead
//! of answering it. A typed message carries everything held; a rate-limit hold also lets it out on its own
//! at [`NoticeGate::deadline`] — one probe turn — and a successful turn ends any hold.
//!
//! Pure bookkeeping over an injected clock: the loop owns the I/O (the timer, the transcript, the host).
//! A running turn never asks the gate — its round boundaries still take notices (`Steerer`).

use std::time::Duration;

use tokio::time::Instant;

use crate::ui::facade::Input;

/// The first probe of a rate-limit hold whose 429 named no wait comes this long after it…
pub(crate) const PROBE_FIRST: Duration = Duration::from_mins(1);
/// …and each one that is refused again doubles the wait, up to this.
pub(crate) const PROBE_CAP: Duration = Duration::from_mins(30);

/// Why notices wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The user interrupted a turn: automatic work waits until they speak. No timer.
    Interrupted,
    /// A turn ended in 429: automatic work waits until `next`, when the held notices go out as one probe.
    /// `step` is how long the NEXT refusal without a `retry-after` waits.
    RateLimited { next: Instant, step: Duration },
}

/// How a turn ended, as far as the gate cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The model answered.
    Answered,
    /// The model was refused with 429 after the ladder; `typed` when the user's own message drove the turn — or
    /// another process's (X-85): anything but a job's notice the gate let through.
    RateLimited {
        retry_after: Option<Duration>,
        typed: bool,
    },
    /// Any other failure.
    Failed,
    /// ESC or Ctrl+C.
    Interrupted,
}

/// The hold and the notices it put aside, in the order their jobs ended.
#[derive(Debug, Default)]
pub(crate) struct NoticeGate {
    hold: Option<Hold>,
    held: Vec<Input>,
}

impl NoticeGate {
    /// The current hold.
    pub(crate) fn hold(&self) -> Option<Hold> {
        self.hold
    }

    /// Whether a notice arriving at `now` waits. A rate-limit hold past its `next` lets it through: that
    /// notice is itself the probe.
    pub(crate) fn holds(&self, now: Instant) -> bool {
        match self.hold {
            None => false,
            Some(Hold::Interrupted) => true,
            Some(Hold::RateLimited { next, .. }) => now < next,
        }
    }

    /// Whether a turn starting now runs under a rate-limit hold — its 429 then gets no ladder.
    pub(crate) fn rate_limited(&self) -> bool {
        matches!(self.hold, Some(Hold::RateLimited { .. }))
    }

    /// When the next automatic try is due, under a rate-limit hold.
    pub(crate) fn next_try(&self) -> Option<Instant> {
        match self.hold {
            Some(Hold::RateLimited { next, .. }) => Some(next),
            _ => None,
        }
    }

    /// When the held notices go out on their own: a rate-limit hold with something to release. Nothing
    /// held, no timer; an interrupt never has one.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next_try().filter(|_| !self.held.is_empty())
    }

    /// Puts a notice aside.
    pub(crate) fn put_aside(&mut self, notice: Input) {
        self.held.push(notice);
    }

    /// How many notices wait.
    pub(crate) fn held_len(&self) -> usize {
        self.held.len()
    }

    /// Takes every held notice, oldest first — a turn is about to carry them.
    pub(crate) fn take_held(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.held)
    }

    /// Returns notices a turn carried and did not keep, AHEAD of anything held since: the order is the
    /// order their jobs ended.
    pub(crate) fn put_back(&mut self, mut notices: Vec<Input>) {
        notices.append(&mut self.held);
        self.held = notices;
    }

    /// The transition a finished turn makes (§3.2 of the design).
    pub(crate) fn settle(&mut self, outcome: Outcome, now: Instant) {
        self.hold = match outcome {
            Outcome::Answered | Outcome::Failed => None,
            Outcome::Interrupted => Some(Hold::Interrupted),
            Outcome::RateLimited { retry_after, typed } => Some(match self.hold {
                // The user's own try does not move the schedule: a later `retry-after` may push it out.
                Some(Hold::RateLimited { next, step }) if typed => Hold::RateLimited {
                    next: retry_after.map_or(next, |d| next.max(now + d)),
                    step,
                },
                // A probe refused again: wait longer.
                Some(Hold::RateLimited { step, .. }) => Hold::RateLimited {
                    next: now + retry_after.unwrap_or(step),
                    step: (step * 2).min(PROBE_CAP),
                },
                // The first refusal.
                Some(Hold::Interrupted) | None => Hold::RateLimited {
                    next: now + retry_after.unwrap_or(PROBE_FIRST),
                    step: PROBE_FIRST * 2,
                },
            }),
        };
    }

    /// `/model` moved the chat to another model: a rate-limit hold tries at once, on that model, and the
    /// schedule starts over. An interrupt's hold is the user's, not the model's: untouched.
    pub(crate) fn model_switched(&mut self, now: Instant) {
        if let Some(Hold::RateLimited { .. }) = self.hold {
            self.hold = Some(Hold::RateLimited {
                next: now,
                step: PROBE_FIRST,
            });
        }
    }
}

/// The wall-clock time `at` falls on, as `HH:MM` — what the transcript says a hold waits until.
pub(crate) fn clock_at(at: Instant) -> String {
    let ahead = at.saturating_duration_since(Instant::now());
    let now = jiff::Zoned::now();
    jiff::SignedDuration::try_from(ahead)
        .ok()
        .and_then(|d| now.checked_add(d).ok())
        .unwrap_or(now)
        .strftime("%H:%M")
        .to_string()
}

/// The dim line a notice put aside leaves: its own headline, then what it waits for.
pub(crate) fn held_line(display: &str, hold: Hold, clock: impl Fn(Instant) -> String) -> String {
    match hold {
        Hold::Interrupted => format!("⏸ {display} — held; it goes with your next message"),
        Hold::RateLimited { next, .. } => format!("⏸ {display} — held until {}", clock(next)),
    }
}

/// The dim line a probe the provider refused again leaves, instead of another error block.
pub(crate) fn still_limited_line(headline: &str, held: usize, next: &str) -> String {
    format!(
        "⏸ Still {} — {held} notice(s) held; next try {next}",
        lower_first(headline)
    )
}

/// The dim line a probe starts with.
pub(crate) fn probe_line(n: usize) -> String {
    format!("▶ Trying again with {n} held notice(s)")
}

/// `Rate limited (429)` → `rate limited (429)`, for the middle of a sentence.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_lowercase().chain(chars).collect::<String>()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ui::facade::InputKind;

    fn notice(n: u32) -> Input {
        Input {
            display: format!("[background job b{n} finished]"),
            text: format!("[background job b{n} finished]\nout"),
            kind: InputKind::Notice,
        }
    }

    fn names(inputs: &[Input]) -> Vec<String> {
        inputs.iter().map(|i| i.display.clone()).collect()
    }

    fn limited(retry_after: Option<Duration>, typed: bool) -> Outcome {
        Outcome::RateLimited { retry_after, typed }
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn nothing_holds_until_a_turn_says_so() {
        let g = NoticeGate::default();
        let now = Instant::now();
        assert!(!g.holds(now));
        assert!(!g.rate_limited());
        assert_eq!(g.deadline(), None);
    }

    // The first 429 waits a minute, or what the response asked for; each refused probe doubles the wait
    // up to the cap; any answer ends the hold.
    #[test]
    fn a_rate_limit_holds_until_next_and_backs_off_to_the_cap() {
        let mut g = NoticeGate::default();
        let t0 = Instant::now();
        g.settle(limited(None, false), t0);
        assert_eq!(
            g.hold(),
            Some(Hold::RateLimited {
                next: t0 + MIN,
                step: 2 * MIN
            })
        );
        assert!(g.holds(t0) && g.holds(t0 + MIN - Duration::from_millis(1)));
        assert!(!g.holds(t0 + MIN), "past next a notice is the probe");
        assert!(g.rate_limited());

        let mut waits = Vec::new();
        let mut now = t0;
        for _ in 0..8 {
            now += Duration::from_secs(1);
            g.settle(limited(None, false), now);
            waits.push((g.next_try().unwrap() - now).as_secs() / 60);
        }
        assert_eq!(waits, [2, 4, 8, 16, 30, 30, 30, 30]);

        g.settle(Outcome::Answered, now);
        assert_eq!(g.hold(), None);
    }

    #[test]
    fn a_retry_after_sets_the_next_try() {
        let mut g = NoticeGate::default();
        let t0 = Instant::now();
        g.settle(limited(Some(Duration::from_hours(5)), false), t0);
        assert_eq!(g.next_try(), Some(t0 + Duration::from_hours(5)));
        // A refused probe with a retry-after waits exactly that, and still steps the schedule.
        g.settle(limited(Some(Duration::from_secs(90)), false), t0);
        assert_eq!(
            g.hold(),
            Some(Hold::RateLimited {
                next: t0 + Duration::from_secs(90),
                step: 4 * MIN
            })
        );
    }

    // The user trying by hand never brings the next automatic try closer, nor steps the schedule.
    #[test]
    fn a_typed_refusal_never_moves_the_schedule_earlier() {
        let mut g = NoticeGate::default();
        let t0 = Instant::now();
        g.settle(limited(None, false), t0);
        let before = g.hold();
        g.settle(limited(None, true), t0 + Duration::from_secs(10));
        assert_eq!(g.hold(), before);
        g.settle(limited(Some(Duration::from_secs(5)), true), t0);
        assert_eq!(g.hold(), before, "an earlier retry-after is ignored");
        g.settle(limited(Some(Duration::from_secs(600)), true), t0);
        assert_eq!(
            g.hold(),
            Some(Hold::RateLimited {
                next: t0 + Duration::from_secs(600),
                step: 2 * MIN
            }),
            "a later one pushes it out"
        );
        // From no hold, a typed refusal is the first refusal.
        let mut fresh = NoticeGate::default();
        fresh.settle(limited(None, true), t0);
        assert_eq!(fresh.next_try(), Some(t0 + MIN));
    }

    #[test]
    fn an_interrupt_holds_from_any_state_with_no_timer() {
        let t0 = Instant::now();
        for start in [None, Some(limited(None, false))] {
            let mut g = NoticeGate::default();
            if let Some(o) = start {
                g.settle(o, t0);
            }
            g.settle(Outcome::Interrupted, t0);
            assert_eq!(g.hold(), Some(Hold::Interrupted));
            g.put_aside(notice(1));
            assert!(g.holds(t0 + Duration::from_hours(24)));
            assert_eq!(g.deadline(), None);
            assert!(!g.rate_limited());
        }
        let mut g = NoticeGate::default();
        g.settle(Outcome::Interrupted, t0);
        g.settle(Outcome::Failed, t0);
        assert_eq!(g.hold(), None, "any other end of a turn lifts it");
    }

    #[test]
    fn the_deadline_needs_something_to_release() {
        let mut g = NoticeGate::default();
        let t0 = Instant::now();
        g.settle(limited(None, false), t0);
        assert_eq!(g.deadline(), None);
        g.put_aside(notice(1));
        assert_eq!(g.deadline(), Some(t0 + MIN));
        assert_eq!(names(&g.take_held()), ["[background job b1 finished]"]);
        assert_eq!(g.deadline(), None);
    }

    // What a turn carried and gave back goes ahead of what arrived meanwhile.
    #[test]
    fn put_back_keeps_the_order_the_jobs_ended_in() {
        let mut g = NoticeGate::default();
        g.put_aside(notice(3));
        g.put_back(vec![notice(1), notice(2)]);
        g.put_aside(notice(4));
        assert_eq!(g.held_len(), 4);
        assert_eq!(
            names(&g.take_held()),
            [1, 2, 3, 4].map(|n| format!("[background job b{n} finished]"))
        );
    }

    #[test]
    fn a_model_switch_tries_at_once_and_restarts_the_schedule() {
        let mut g = NoticeGate::default();
        let t0 = Instant::now();
        g.settle(limited(None, false), t0);
        g.settle(limited(None, false), t0);
        let now = t0 + Duration::from_secs(30);
        g.model_switched(now);
        assert_eq!(
            g.hold(),
            Some(Hold::RateLimited {
                next: now,
                step: MIN
            })
        );
        assert!(!g.holds(now));
        g.settle(limited(None, false), now);
        assert_eq!(
            g.next_try(),
            Some(now + MIN),
            "the new model's first refusal"
        );

        let mut i = NoticeGate::default();
        i.settle(Outcome::Interrupted, t0);
        i.model_switched(t0);
        assert_eq!(i.hold(), Some(Hold::Interrupted));
        let mut n = NoticeGate::default();
        n.model_switched(t0);
        assert_eq!(n.hold(), None);
    }

    #[test]
    fn the_lines_say_what_waits_and_until_when() {
        let t0 = Instant::now();
        let at = |_: Instant| "17:42".to_owned();
        assert_eq!(
            held_line("[background job b2 finished]", Hold::Interrupted, at),
            "⏸ [background job b2 finished] — held; it goes with your next message"
        );
        assert_eq!(
            held_line(
                "[background job b2 finished]",
                Hold::RateLimited {
                    next: t0,
                    step: MIN
                },
                at
            ),
            "⏸ [background job b2 finished] — held until 17:42"
        );
        assert_eq!(
            still_limited_line("Rate limited (429)", 2, "18:12"),
            "⏸ Still rate limited (429) — 2 notice(s) held; next try 18:12"
        );
        assert_eq!(probe_line(3), "▶ Trying again with 3 held notice(s)");
        let shown = clock_at(t0 + Duration::from_hours(1));
        assert_eq!(shown.len(), 5, "{shown}");
        assert_eq!(shown.as_bytes()[2], b':', "{shown}");
    }
}
