//! The activity-group state machine (chat/transcript.go:75-719): the run of thinking
//! segments and tool calls between two content boundaries shares ONE lifecycle widget —
//! completed events scroll through its body — and settles into a single summary line.
//! Degenerate groups keep the classic forms (a lone tool call = header + result lines,
//! thinking only = the `"◇ thought for Ns"` marker), which is also exactly what verbose
//! mode produces by settling after every event. Failed calls always break out as their
//! own red rows — aggregation never swallows an error.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::text::{elapsed, tokens};
use crate::tool::{Artifact, ArtifactKind};

use crate::repl::render::styles::{cyan, dim, red, truncate_runes};
use crate::repl::render::transcript::{BlockKind, Inner, Transcript};

/// Aggregation state for one activity group (transcript.go `activityGroup`). The group
/// owns the lifecycle widget for its whole lifetime; a content boundary (or
/// `reset_turn`) settles it. A group that recorded no events yet (a composing call whose
/// text spills) survives content — collapsing it would erase a call still in flight.
#[derive(Default)]
pub struct ActivityGroup {
    /// Widget raised (the group's separator is paid).
    pub up: bool,
    /// A thinking segment owns the widget label.
    pub thinking_up: bool,
    /// Current widget header (restored after a pause).
    pub label: String,
    /// Clock frozen while the user is consulted.
    pub paused: bool,
    /// Settled thinking segments (a count — a segment can measure 0ns).
    pub thinks: u32,
    /// Σ settled thinking segments.
    pub think_dur: Duration,
    /// Σ reasoning tokens (meter estimate, live detail).
    pub think_tokens: u64,
    /// Completed tool calls.
    pub tools: u32,
    /// Failed tool calls.
    pub fails: u32,
    /// Σ tool execution time (human waits excluded).
    pub tools_dur: Duration,
    /// The lone call's classic form, valid while `tools == 1`.
    pub first_header: String,
    /// The lone call's result.
    pub first_result: String,
    /// Whether the lone call failed.
    pub first_err: bool,
    /// The lone call's user-only trailing detail.
    pub first_note: String,
    /// Red breakout rows appended under the summary.
    pub fail_lines: Vec<String>,
}

impl ActivityGroup {
    /// Whether anything settled into the group — the guard between "a group worth
    /// summarizing" and "a raised widget still composing".
    pub fn has_events(&self) -> bool {
        self.tools > 0 || self.thinks > 0
    }
}

/// Raises the group's lifecycle widget (paying the group's one separator) or relabels it
/// in place — the clock keeps running (transcript.go `ensureWidgetLocked`).
pub(crate) fn ensure_widget(inner: &mut Inner, label: &str) {
    if !inner.grp.up {
        inner.grp.up = true;
        inner.begin(BlockKind::Activity);
    }
    label.clone_into(&mut inner.grp.label);
    inner.u.call_preview(label);
}

/// Folds the group into its settled scrollback form and resets it (transcript.go
/// `settleGroupLocked`). Groups without recorded events keep their widget; a group whose
/// widget was already dropped (turn teardown) commits its summary as plain lines.
pub(crate) fn settle_group(inner: &mut Inner) {
    if !inner.grp.has_events() {
        return;
    }
    let lines = group_lines(&inner.grp);
    let was_up = inner.grp.up;
    inner.grp = ActivityGroup::default();
    if inner.last != BlockKind::Activity {
        // An async block interleaved since the raise: re-open so the summary doesn't
        // glue to the stranger.
        inner.begin(BlockKind::Activity);
    }
    if was_up {
        inner.u.close_preview();
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    inner.push_lines(&refs);
}

/// Raises a tool call deferred while the thinking widget or a streaming content block
/// owned the slot, in lifecycle order (transcript.go `raisePendingLocked`). A no-op while
/// the other guard is still up.
pub(crate) fn raise_pending(inner: &mut Inner) {
    if inner.pending_call.is_empty() || inner.grp.thinking_up || inner.content_open {
        return;
    }
    let label = std::mem::take(&mut inner.pending_call);
    ensure_widget(inner, &label);
}

/// The group's settled form (transcript.go `groupLinesLocked`):
///
/// - thinking only        → `"◇ thought for 15s"` (the classic marker)
/// - a lone unthought call → the classic header + result lines
/// - anything else        → one summary line + red breakout rows per failure
fn group_lines(g: &ActivityGroup) -> Vec<String> {
    if g.tools == 0 {
        return vec![dim(&format!("◇ thought for {}", elapsed(g.think_dur)))];
    }
    if g.tools == 1 && g.thinks == 0 {
        // The classic block keeps the note too: a lone call that folds away would
        // otherwise be the one case where a call's note is visible while it runs and
        // gone once it finishes.
        let mut head = g.first_header.clone();
        if !g.first_note.is_empty() {
            head.push_str(&dim(&format!(" · {}", g.first_note)));
        }
        let mut lines = vec![head];
        lines.extend(classic_result(&g.first_result, g.first_err));
        return lines;
    }
    let ran = format!(
        "ran {} {} in {}",
        g.tools,
        plural_tools(g.tools),
        elapsed(g.tools_dur)
    );
    let mut line = if g.thinks > 0 {
        dim(&format!("◇ thought for {} · {ran}", elapsed(g.think_dur)))
    } else {
        dim(&format!("◇ {ran}"))
    };
    if g.fails > 0 {
        line.push_str(&red(&format!(" · {} failed", g.fails)));
    }
    let mut lines = vec![line];
    lines.extend(g.fail_lines.iter().cloned());
    lines
}

/// A result rendered the way the settle path shows it: the unstyled
/// [`crate::tool::fmt::print_tool_result_lines`] rows, each styled dim (red on error)
/// self-contained. (Go produced the same visible rows through fatih + the lineCommitter
/// glue; the Rust rows are SGR-self-contained by construction — deviation noted.)
pub(crate) fn classic_result(result: &str, is_error: bool) -> Vec<String> {
    let style: fn(&str) -> String = if is_error { red } else { dim };
    crate::tool::fmt::print_tool_result_lines(result, is_error)
        .iter()
        .map(|row| style(row))
        .collect()
}

/// A completed call's body row inside the widget (transcript.go `eventLine`): a glyph,
/// the header, and a snippet of the first result line.
pub(crate) fn event_line(header: &str, result: &str, is_error: bool, note: &str) -> String {
    let glyph = if is_error { red("✗") } else { dim("✓") };
    let mut line = format!("{glyph} {header}");
    let first = first_result_line(result);
    if !first.is_empty() {
        line.push_str(&dim(&format!(" · {}", truncate_runes(&first, 48))));
    }
    if !note.is_empty() {
        line.push_str(&dim(&format!(" · {note}")));
    }
    line
}

/// A failed call's red breakout row under the group summary (transcript.go `failLine`).
pub(crate) fn fail_line(header: &str, result: &str) -> String {
    let mut line = format!("{} {header}", red("✗"));
    let first = first_result_line(result);
    if !first.is_empty() {
        line.push_str(&red(&format!(" · {}", truncate_runes(&first, 64))));
    }
    line
}

/// The first non-blank line of a tool result (transcript.go `firstResultLine`).
fn first_result_line(result: &str) -> String {
    result
        .split('\n')
        .map(str::trim)
        .find(|ln| !ln.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn plural_tools(n: u32) -> &'static str {
    if n == 1 { "tool" } else { "tools" }
}

/// Refreshes the widget's live status-row prefix from the group's counters
/// (`"3 tools · 1.2k tokens"`; transcript.go `callDetailLocked`).
fn call_detail(inner: &mut Inner) {
    let mut parts = Vec::new();
    if inner.grp.tools > 0 {
        parts.push(format!(
            "{} {}",
            inner.grp.tools,
            plural_tools(inner.grp.tools)
        ));
    }
    if inner.grp.think_tokens > 0 {
        parts.push(format!("{} tokens", tokens(inner.grp.think_tokens)));
    }
    inner.u.call_detail(&parts.join(" · "));
}

/// Renders an interactive tool's outcome as the `"?"` record block's styled lines —
/// shared between the live [`Transcript::ask_record`] and the resume replay so the two
/// never drift (transcript.go `askRecordLines`).
pub(crate) fn ask_record_lines(result: &str, is_error: bool) -> Vec<String> {
    let style: fn(&str) -> String = if is_error { red } else { dim };
    let result = result.trim_end_matches('\n');
    let result = if result.trim().is_empty() {
        "(no answer)"
    } else {
        result
    };
    result
        .split('\n')
        .enumerate()
        .map(|(i, ln)| {
            let prefix = if i == 0 { "? " } else { "  " };
            style(&format!("{prefix}{ln}"))
        })
        .collect()
}

impl Transcript {
    /// Raises the tool-call lifecycle widget into the current group, or relabels it in
    /// place (transcript.go `openCall`). While the thinking widget owns the slot or
    /// content is streaming, the call is only remembered; `settle_thinking` /
    /// `close_content` raises it.
    pub fn open_call(&self, label: &str) {
        let mut inner = self.lock();
        if inner.grp.thinking_up || inner.content_open {
            label.clone_into(&mut inner.pending_call);
            return;
        }
        ensure_widget(&mut inner, label);
    }

    /// Records a completed tool call into the group (transcript.go `finishCall`):
    /// counters, a body row scrolling through the widget, the failure breakout, and —
    /// while it is the group's only call — the material for the classic degenerate form.
    /// In verbose mode the group settles immediately. `note` is an optional trailing
    /// detail for the event row (`""` = none) — a fact about the call the user should
    /// see and the model should not be billed for.
    pub fn finish_call(
        &self,
        header: &str,
        result: &str,
        is_error: bool,
        dur: Duration,
        note: &str,
    ) {
        let mut inner = self.lock();
        if !inner.grp.up {
            // Defensive: a finish without a raise (never in practice).
            ensure_widget(&mut inner, header);
        }
        inner.grp.tools += 1;
        inner.grp.tools_dur += dur;
        if inner.grp.tools == 1 {
            header.clone_into(&mut inner.grp.first_header);
            result.clone_into(&mut inner.grp.first_result);
            inner.grp.first_err = is_error;
            note.clone_into(&mut inner.grp.first_note);
        }
        if is_error {
            inner.grp.fails += 1;
            let fl = fail_line(header, result);
            inner.grp.fail_lines.push(fl);
        }
        if inner.verbose_on() {
            settle_group(&mut inner);
            return;
        }
        inner
            .u
            .call_line(&event_line(header, result, is_error, note));
        ensure_widget(&mut inner, &dim("Working…"));
        call_detail(&mut inner);
    }

    /// Raises an expanded call's standalone widget (transcript.go `openShowcase`;
    /// `PresentExpanded`: file mutations showing their diff). An expanded call is a group
    /// boundary like content — the running group settles first, and whatever follows
    /// opens a fresh group.
    pub fn open_showcase(&self, header: &str) {
        let mut inner = self.lock();
        inner.pending_call.clear();
        settle_group(&mut inner);
        ensure_widget(&mut inner, header);
    }

    /// Morphs the showcase widget into its expanded block (transcript.go
    /// `settleShowcase`): the header (with ±row counts) over the rendered diff — or the
    /// classic result form when the call posted no diff artifact (errors, declines,
    /// tools that had nothing to show).
    pub fn settle_showcase(
        &self,
        header: &str,
        art: Option<&Artifact>,
        result: &str,
        is_error: bool,
    ) {
        let mut inner = self.lock();
        let lines = match art {
            Some(a) if a.kind == ArtifactKind::Diff && !a.lines.is_empty() && !is_error => {
                let adds = a.lines.iter().filter(|l| l.starts_with('+')).count();
                let dels = a.lines.iter().filter(|l| l.starts_with('-')).count();
                // The dynamic diff row budget: the live screen height, floored at 24
                // (`diffMinRows`) — a taller terminal earns a fuller diff.
                let budget = usize::from(inner.u.height()).max(24);
                let width = usize::from(inner.u.width());
                let mut lines = vec![format!("{header}{}", dim(&format!("  +{adds} -{dels}")))];
                lines.extend(crate::repl::render::diff::render_diff(
                    &a.title,
                    &a.lines.join("\n"),
                    budget,
                    width,
                    crate::app::color::enabled(),
                    inner.dark,
                ));
                lines
            }
            _ => {
                let mut lines = vec![header.to_owned()];
                lines.extend(classic_result(result, is_error));
                lines
            }
        };
        inner.grp = ActivityGroup::default();
        if inner.last != BlockKind::Activity {
            inner.begin(BlockKind::Activity);
        }
        inner.u.close_preview();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
    }

    /// Raises the thinking segment into the group's widget (transcript.go
    /// `openThinking`) and returns the meter that feeds its token count.
    pub fn open_thinking(self: &Arc<Self>) -> ThinkingMeter {
        let base = {
            let mut inner = self.lock();
            inner.grp.thinking_up = true;
            ensure_widget(&mut inner, &dim("Thinking"));
            inner.grp.think_tokens
        };
        ThinkingMeter {
            tr: Arc::clone(self),
            base,
            n: 0,
            last: None,
        }
    }

    /// Completes the thinking segment (transcript.go `settleThinking`): the group
    /// accumulates its duration and shows the `"◇ thought Ns"` body row; in verbose mode
    /// the group settles immediately. A tool call announced during the thinking stream is
    /// raised here, in lifecycle order.
    pub fn settle_thinking(&self, start: Instant) {
        let mut inner = self.lock();
        inner.grp.thinking_up = false;
        let d = start.elapsed();
        inner.grp.thinks += 1;
        inner.grp.think_dur += d;
        if inner.verbose_on() {
            settle_group(&mut inner);
        } else {
            inner
                .u
                .call_line(&dim(&format!("◇ thought {}", elapsed(d))));
            if inner.pending_call.is_empty() {
                ensure_widget(&mut inner, &dim("Working…"));
            }
            call_detail(&mut inner);
        }
        raise_pending(&mut inner);
    }

    /// Commits an interactive tool's outcome — the user's own answers — as a `"?"` record
    /// block (transcript.go `askRecord`). Interactive calls never enter the activity
    /// group.
    pub fn ask_record(&self, result: &str, is_error: bool) {
        let mut inner = self.lock();
        inner.begin(BlockKind::Ask);
        let lines = ask_record_lines(result, is_error);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        inner.push_lines(&refs);
    }

    /// Marks the turn as waiting on the user (transcript.go `pauseForInput`): the widget
    /// relabels to the reason and the elapsed clock freezes — human deliberation must not
    /// inflate the group's timings. A no-op without a live widget.
    pub fn pause_for_input(&self, reason: &str) {
        let mut inner = self.lock();
        if !inner.grp.up || inner.grp.paused {
            return;
        }
        inner.grp.paused = true;
        inner.u.call_preview(&dim(&format!("⏸ {reason}")));
        inner.u.pause_clock();
    }

    /// Restores the widget label and restarts the clock (transcript.go
    /// `resumeFromInput`).
    pub fn resume_from_input(&self) {
        let mut inner = self.lock();
        if !inner.grp.paused {
            return;
        }
        inner.grp.paused = false;
        inner.u.resume_clock();
        let label = inner.grp.label.clone();
        inner.u.call_preview(&label);
    }
}

/// Counts streamed reasoning into the widget's status row (transcript.go
/// `thinkingMeter`): tokens estimated per delta, updates throttled to 150ms so
/// high-frequency deltas don't flood the mailbox. `base` carries the group's tokens from
/// earlier segments, so the detail row keeps counting up across thinking rounds.
pub struct ThinkingMeter {
    tr: Arc<Transcript>,
    base: u64,
    n: u64,
    last: Option<Instant>,
}

impl ThinkingMeter {
    /// Feeds one reasoning delta into the meter.
    pub fn add(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        {
            let inner = self.tr.lock();
            if let Some(est) = &inner.tokens {
                self.n += est(s);
            }
        }
        if let Some(last) = self.last
            && last.elapsed() < Duration::from_millis(150)
        {
            return;
        }
        self.last = Some(Instant::now());
        let mut inner = self.tr.lock();
        inner.grp.think_tokens = self.base + self.n;
        call_detail(&mut inner);
    }
}

/// The streaming header, rendered exactly like the final committed tool-call header
/// (`"[name …]"`, `CodeStyle`) so the widget settles into the collapsed form without
/// changing its look (chat/run.go:1247-1253 `composingLabel`; `"tool"` when nameless).
pub(crate) fn composing_label(name: &str) -> String {
    let n = if name.is_empty() {
        "tool".to_owned()
    } else {
        crate::tool::fmt::display_tool_name(name)
    };
    cyan(&format!("[{n} …]"))
}
