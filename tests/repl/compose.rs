//! The `compose_test` suite — the transcript + activity-group choreography against the
//! `ScriptedUi`-backed recorder (`chat/compose_test.go` ports; the single highest-value
//! block). Style bytes are the frozen SGR pins (`TUI_CONTRACTS` §9), rebuilt locally the
//! way the Go tests rebuilt them from the style API.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use iota::repl::{Transcript, render_diff};
use iota::testing::{ScriptedUi, UiEvent};
use iota::tool::{Artifact, ArtifactKind};
use pretty_assertions::assert_eq;

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

fn working() -> String {
    dim("Working…")
}

fn truncate_runes(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        head + "…"
    } else {
        head
    }
}

fn first_result_line(result: &str) -> String {
    result
        .split('\n')
        .map(str::trim)
        .find(|ln| !ln.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// `transcript.go` `eventLine` twin for expectations.
fn event_line(header: &str, result: &str, is_error: bool, note: &str) -> String {
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

/// `transcript.go` `failLine` twin for expectations.
fn fail_line(header: &str, result: &str) -> String {
    let mut line = format!("{} {header}", red("✗"));
    let first = first_result_line(result);
    if !first.is_empty() {
        line.push_str(&red(&format!(" · {}", truncate_runes(&first, 64))));
    }
    line
}

/// Renders a result the way the settle path does (Go `classicResult`).
fn classic(result: &str, is_error: bool) -> Vec<String> {
    let style: fn(&str) -> String = if is_error { red } else { dim };
    iota::tool::fmt::print_tool_result_lines(result, is_error)
        .iter()
        .map(|r| style(r))
        .collect()
}

/// Records the transcript's surface calls in order (Go `recSurface`).
/// Records the transcript's facade calls as the `kind:payload` lines the assertions compare
/// (a `ScriptedUi` at 80×30 plus the rendering of its event log).
#[derive(Clone)]
struct Rec(Arc<ScriptedUi>);

impl Default for Rec {
    fn default() -> Self {
        let ui = ScriptedUi::new(Vec::new());
        ui.set_size(80, 30);
        Self(ui)
    }
}

impl Rec {
    /// The facade the transcript under test writes to.
    fn ui(&self) -> Arc<ScriptedUi> {
        Arc::clone(&self.0)
    }

    fn lines(&self) -> Vec<String> {
        self.0
            .events()
            .into_iter()
            .filter_map(|e| {
                Some(match e {
                    UiEvent::Print(lines) => format!("print:{}", lines.join("|")),
                    UiEvent::UserBlock(s) => format!("user:{s}"),
                    UiEvent::CallPreview(l) => format!("call:{l}"),
                    UiEvent::CallDetail(d) => format!("detail:{d}"),
                    UiEvent::CallLine(l) => format!("line:{l}"),
                    UiEvent::ClosePreview => "settle".to_owned(),
                    UiEvent::PauseClock => "pause".to_owned(),
                    UiEvent::ResumeClock => "resume".to_owned(),
                    UiEvent::CallBody(rows) => format!("body:{}", rows.join("|")),
                    _ => return None,
                })
            })
            .collect()
    }

    fn joined(&self) -> String {
        self.lines().join("\n")
    }
}

fn transcript(rec: &Rec) -> Arc<Transcript> {
    Arc::new(Transcript::new(rec.ui(), None))
}

// Go: chat/compose_test.go:51 TestActivityGroupAggregates — thinking and consecutive
// tool calls share ONE widget (one separator, relabels in place, event rows through the
// body) and a content boundary settles them into a single summary line.
#[test]
fn test_activity_group_aggregates() {
    let rec = Rec::default();
    let est = |s: &str| u64::try_from(s.len()).unwrap() / 4;
    let tr = Arc::new(Transcript::new(rec.ui(), Some(Box::new(est))));

    tr.user("hello");
    let start = Instant::now();
    let mut m = tr.open_thinking();
    m.add("some reasoning text");
    tr.settle_thinking(start);
    tr.open_call("[a …]");
    tr.open_call("[a full]"); // header expansion: same widget, no separator
    tr.finish_call("[a full]", "ok", false, Duration::from_secs(2), "");
    tr.open_call("[b]");
    tr.finish_call("[b]", "out", false, Duration::from_secs(3), "");
    let mut content = tr.content_block();
    content.push(&["Done."]);

    let tokens = format!("{} tokens", iota::text::tokens(est("some reasoning text")));
    let want = [
        "user:hello".to_owned(),
        "print:".to_owned(),
        format!("call:{}", dim("Thinking")),
        format!("detail:{tokens}"),
        format!("line:{}", dim("◇ thought <1s")),
        format!("call:{}", working()),
        format!("detail:{tokens}"),
        "call:[a …]".to_owned(),
        "call:[a full]".to_owned(),
        format!("line:{}", event_line("[a full]", "ok", false, "")),
        format!("call:{}", working()),
        format!("detail:1 tool · {tokens}"),
        "call:[b]".to_owned(),
        format!("line:{}", event_line("[b]", "out", false, "")),
        format!("call:{}", working()),
        format!("detail:2 tools · {tokens}"),
        "settle".to_owned(),
        format!("print:{}", dim("◇ thought for <1s · ran 2 tools in 5s")),
        "print:".to_owned(),
        "print:Done.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:99 TestActivityGroupLoneToolClassic — a lone tool call with
// no thinking keeps the classic block: header over the "⎿" result lines.
#[test]
fn test_activity_group_lone_tool_classic() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("x");
    tr.open_call("[read_file path:a]");
    tr.finish_call(
        "[read_file path:a]",
        "line1\nline2",
        false,
        Duration::from_secs(1),
        "",
    );
    let mut content = tr.content_block();
    content.push(&["Answer."]);

    let mut classic_block = vec!["[read_file path:a]".to_owned()];
    classic_block.extend(classic("line1\nline2", false));
    let want = [
        "user:x".to_owned(),
        "print:".to_owned(),
        "call:[read_file path:a]".to_owned(),
        format!(
            "line:{}",
            event_line("[read_file path:a]", "line1\nline2", false, "")
        ),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")),
        "print:".to_owned(),
        "print:Answer.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:127 TestActivityGroupThinkingOnly — thinking with no tool
// calls settles into the classic "◇ thought for Ns" marker at the content boundary.
#[test]
fn test_activity_group_thinking_only() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.open_thinking();
    tr.settle_thinking(Instant::now());
    let mut content = tr.content_block();
    content.push(&["The reply."]);

    let want = [
        format!("call:{}", dim("Thinking")), // first block: no separator
        format!("line:{}", dim("◇ thought <1s")),
        format!("call:{}", working()),
        "detail:".to_owned(),
        "settle".to_owned(),
        format!("print:{}", dim("◇ thought for <1s")),
        "print:".to_owned(),
        "print:The reply.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:152 TestActivityGroupFailBreakout — failed calls are never
// swallowed by aggregation: the summary carries a red failure count and each failed call
// breaks out as its own red row.
#[test]
fn test_activity_group_fail_breakout() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.open_call("[a]");
    tr.finish_call("[a]", "fine", false, Duration::from_secs(1), "");
    tr.open_call("[shell cmd:x]");
    tr.finish_call(
        "[shell cmd:x]",
        "exit 1\ndetail",
        true,
        Duration::from_secs(1),
        "",
    );
    let mut content = tr.content_block();
    content.push(&["So."]);

    let summary = format!("{}{}", dim("◇ ran 2 tools in 2s"), red(" · 1 failed"));
    let want = [
        "call:[a]".to_owned(),
        format!("line:{}", event_line("[a]", "fine", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "call:[shell cmd:x]".to_owned(),
        format!(
            "line:{}",
            event_line("[shell cmd:x]", "exit 1\ndetail", true, "")
        ),
        format!("call:{}", working()),
        "detail:2 tools".to_owned(),
        "settle".to_owned(),
        format!(
            "print:{summary}|{}",
            fail_line("[shell cmd:x]", "exit 1\ndetail")
        ),
        "print:".to_owned(),
        "print:So.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:184 TestVerboseSettlesPerEvent — verbose mode (/debug on)
// settles the group after every event, reproducing the classic per-item blocks (the
// T-18 hook survives as a closure although /debug itself is unregistered this slice).
#[test]
fn test_verbose_settles_per_event() {
    let rec = Rec::default();
    let tr = transcript(&rec);
    tr.set_verbose(Some(Box::new(|| true)));

    tr.user("hello");
    tr.open_thinking();
    tr.settle_thinking(Instant::now());
    let mut content = tr.content_block();
    content.push(&["The reply."]);
    tr.open_call("[a …]");
    tr.open_call("[a full]");
    tr.finish_call("[a full]", "ok", false, Duration::from_secs(1), "");
    tr.open_call("[b]");
    tr.finish_call("[b]", "", false, Duration::from_secs(1), "");
    let mut content2 = tr.content_block();
    content2.push(&["Done."]);

    let mut a_block = vec!["[a full]".to_owned()];
    a_block.extend(classic("ok", false));
    let mut b_block = vec!["[b]".to_owned()];
    b_block.extend(classic("", false));
    let want = [
        "user:hello".to_owned(),
        "print:".to_owned(),
        format!("call:{}", dim("Thinking")),
        "settle".to_owned(),
        format!("print:{}", dim("◇ thought for <1s")),
        "print:".to_owned(),
        "print:The reply.".to_owned(),
        "print:".to_owned(),
        "call:[a …]".to_owned(),
        "call:[a full]".to_owned(),
        "settle".to_owned(),
        format!("print:{}", a_block.join("|")),
        "print:".to_owned(),
        "call:[b]".to_owned(),
        "settle".to_owned(),
        format!("print:{}", b_block.join("|")),
        "print:".to_owned(),
        "print:Done.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:220 TestTranscriptBlankLatch — interior blanks pass through
// once more content follows; trailing blanks are dropped.
#[test]
fn test_transcript_blank_latch() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.echo(&[
        "❯ hi".to_owned(),
        String::new(),
        "line-1".to_owned(),
        String::new(),
        String::new(),
        "line-2".to_owned(),
        String::new(),
        String::new(),
    ]);
    tr.user("next");

    let want = [
        "print:❯ hi||line-1|||line-2", // interior blanks intact, trailing dropped
        "print:",
        "user:next",
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:238 TestTranscriptGrouping — consecutive notices (and errors)
// group into one block; a different kind in between starts fresh; content re-opens after
// an async interleave.
#[test]
fn test_transcript_grouping() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.notice("AGENTS.md reloaded (2 files)");
    tr.notice("Skills reloaded (3 skill(s))");
    tr.error("⚠ MCP srv failed: boom");
    tr.notice("Context compacted");

    let mut content = tr.content_block();
    content.push(&["streaming…"]);
    tr.error("⚠ MCP other failed: late"); // async interleave
    content.push(&["more content"]); // same committer re-opens

    let want = [
        format!("print:{}", dim("AGENTS.md reloaded (2 files)")),
        format!("print:{}", dim("Skills reloaded (3 skill(s))")),
        "print:".to_owned(),
        format!("print:{}", red("⚠ MCP srv failed: boom")),
        "print:".to_owned(),
        format!("print:{}", dim("Context compacted")),
        "print:".to_owned(),
        "print:streaming…".to_owned(),
        "print:".to_owned(),
        format!("print:{}", red("⚠ MCP other failed: late")),
        "print:".to_owned(),
        "print:more content".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:270 TestThinkingComposingInterleave — the observer's openCall
// must not hijack the thinking widget: the call is remembered and raised at settle, in
// lifecycle order, into the same group (last label wins while queued).
#[test]
fn test_thinking_composing_interleave() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("do it");
    tr.open_thinking();
    tr.open_call("[write_file …]"); // observer fires mid-thought: queued
    tr.open_call("[shell …]"); // label change while queued: last wins
    tr.settle_thinking(Instant::now());
    tr.open_call("[shell cmd:ls]"); // the tool walk expands the raised widget
    tr.finish_call("[shell cmd:ls]", "ok", false, Duration::from_secs(1), "");
    let mut content = tr.content_block();
    content.push(&["Done."]);

    let want = [
        "user:do it".to_owned(),
        "print:".to_owned(),
        format!("call:{}", dim("Thinking")),
        format!("line:{}", dim("◇ thought <1s")),
        "detail:".to_owned(),
        "call:[shell …]".to_owned(), // the pending call raised at settle, same widget
        "call:[shell cmd:ls]".to_owned(),
        format!("line:{}", event_line("[shell cmd:ls]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "settle".to_owned(),
        format!("print:{}", dim("◇ thought for <1s · ran 1 tool in 1s")),
        "print:".to_owned(),
        "print:Done.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:311 TestContentComposingInterleave — an openCall arriving
// while content is open defers until closeContent; the group settles at the content's
// first commit and the deferred call opens the NEXT group.
#[test]
fn test_content_composing_interleave() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("table then tool");
    tr.open_thinking();
    tr.settle_thinking(Instant::now());
    let mut content = tr.open_content();
    content.push(&["intro line"]);
    tr.open_call("[shell …]"); // observer fires; the table is still buffered
    content.push(&["| a | b |", "| 1 |"]); // renderer flush: same block, no re-open
    tr.open_call("[shell cmd:pwd]"); // label refresh while deferred: last wins
    tr.close_content(); // content over → the widget raises NOW

    let want = [
        "user:table then tool".to_owned(),
        "print:".to_owned(),
        format!("call:{}", dim("Thinking")),
        format!("line:{}", dim("◇ thought <1s")),
        format!("call:{}", working()), // no pending call yet at settle time
        "detail:".to_owned(),
        "settle".to_owned(),
        format!("print:{}", dim("◇ thought for <1s")), // the group settles at content
        "print:".to_owned(),
        "print:intro line".to_owned(),
        "print:| a | b ||| 1 |".to_owned(), // committed into the SAME content block
        "print:".to_owned(),
        "call:[shell cmd:pwd]".to_owned(), // raised at closeContent: a new group
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:346 TestMarkContentBeatsObserver — markContent runs on the
// stream side at the FIRST content byte, so an openCall arriving before openContent
// still defers.
#[test]
fn test_mark_content_beats_observer() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.begin_round();
    tr.mark_content(); // stream side: first content byte
    tr.open_call("[shell …]"); // observer, before openContent ran: must defer
    let mut content = tr.open_content();
    content.push(&["the text"]);
    tr.close_content();

    let want = [
        "print:the text", // first block: no separator
        "print:",
        "call:[shell …]",
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:368 TestBeginRoundClearsStaleGuards — a round that died
// mid-stream leaks its guards; beginRound clears them so the next round's widget is not
// silently deferred forever.
#[test]
fn test_begin_round_clears_stale_guards() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.mark_content(); // round 1 died after content started
    tr.begin_round(); // round 2 (retry) begins
    tr.open_call("[shell …]");

    assert_eq!(rec.joined(), "call:[shell …]");
}

// Go: chat/compose_test.go:384 TestCloseContentWithoutPendingCall — closeContent with no
// deferred call is a no-op; openCall after the close raises immediately.
#[test]
fn test_close_content_without_pending_call() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    let mut content = tr.open_content();
    content.push(&["reply"]);
    tr.close_content();
    tr.open_call("[shell …]");

    let want = ["print:reply", "print:", "call:[shell …]"];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:405 TestOrphanedWidgetSeparatorReuse — a widget dropped
// before any event settled leaves its separator with nothing under it; the next block
// reuses that orphan instead of stacking a second blank.
#[test]
fn test_orphaned_widget_separator_reuse() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("x");
    tr.open_call("[shell …]"); // separator paid, widget raised
    tr.reset_turn(); // turn died; sink.done dropped the widget
    tr.notice("Interrupted.");
    tr.user("next"); // a normal turn afterwards pays its own separator again

    let want = [
        "user:x".to_owned(),
        "print:".to_owned(),
        "call:[shell …]".to_owned(),
        format!("print:{}", dim("Interrupted.")), // no extra separator
        "print:".to_owned(),
        "user:next".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:429 TestResetTurnSettlesPartialGroup — a group with recorded
// events still settles when the turn dies: the partial summary is its trace (the widget
// itself was already dropped; the summary commits as plain lines).
#[test]
fn test_reset_turn_settles_partial_group() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("x");
    tr.open_call("[a]");
    tr.finish_call("[a]", "ok", false, Duration::from_secs(1), "");
    tr.open_call("[b]");
    tr.finish_call("[b]", "ok", false, Duration::from_secs(1), "");
    tr.reset_turn(); // interrupted before any content boundary
    tr.notice("Interrupted.");

    let want = [
        "user:x".to_owned(),
        "print:".to_owned(),
        "call:[a]".to_owned(),
        format!("line:{}", event_line("[a]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "call:[b]".to_owned(),
        format!("line:{}", event_line("[b]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:2 tools".to_owned(),
        "settle".to_owned(),
        format!("print:{}", dim("◇ ran 2 tools in 2s")),
        "print:".to_owned(),
        format!("print:{}", dim("Interrupted.")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:462 TestSettleReopensAfterInterleave — an async error
// interleaving between the raise and the settle forces the summary to re-open the block
// — it never glues to the stranger.
#[test]
fn test_settle_reopens_after_interleave() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.open_call("[shell …]");
    tr.error("⚠ MCP srv failed: boom"); // async reporter mid-execution
    tr.finish_call("[shell cmd:ls]", "ok", false, Duration::from_secs(1), "");
    let mut content = tr.content_block();
    content.push(&["Done."]);

    let mut classic_block = vec!["[shell cmd:ls]".to_owned()];
    classic_block.extend(classic("ok", false));
    let want = [
        "call:[shell …]".to_owned(),
        "print:".to_owned(),
        format!("print:{}", red("⚠ MCP srv failed: boom")),
        format!("line:{}", event_line("[shell cmd:ls]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "print:".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")),
        "print:".to_owned(),
        "print:Done.".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:864 TestUserSettlesOpenGroup — a mid-turn injected user
// message (steering) is a stronger boundary than content: the running group settles
// first, the ❯ block lands, and the next round's activity opens a fresh group.
#[test]
fn test_user_settles_open_group() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.open_call("[a]");
    tr.finish_call("[a]", "ok", false, Duration::from_secs(1), "");
    tr.user("also check the tests"); // steering injection at the round boundary
    tr.open_call("[b]");

    let mut classic_block = vec!["[a]".to_owned()];
    classic_block.extend(classic("ok", false));
    let want = [
        "call:[a]".to_owned(),
        format!("line:{}", event_line("[a]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")),
        "print:".to_owned(),
        "user:also check the tests".to_owned(),
        "print:".to_owned(),
        "call:[b]".to_owned(), // fresh group, own separator
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:490 TestAskRecord — an interactive tool's outcome lands as
// the "?" record block, outside any activity group.
#[test]
fn test_ask_record() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("pick");
    tr.ask_record("Auth: JWT\nLib: chi", false);
    tr.ask_record("", false);

    let want = [
        "user:pick".to_owned(),
        "print:".to_owned(),
        format!("print:{}|{}", dim("? Auth: JWT"), dim("  Lib: chi")),
        "print:".to_owned(),
        format!("print:{}", dim("? (no answer)")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:511 TestPauseForInput — pauseForInput relabels the live
// widget and freezes its clock; resume restores the group's label. Without a widget both
// are no-ops.
#[test]
fn test_pause_for_input() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.pause_for_input("waiting for your input"); // no widget: nothing
    tr.resume_from_input();

    tr.open_call("[edit_file path:x]");
    tr.pause_for_input("waiting for approval");
    tr.resume_from_input();

    let want = [
        "call:[edit_file path:x]".to_owned(),
        format!("call:{}", dim("⏸ waiting for approval")),
        "pause".to_owned(),
        "resume".to_owned(),
        "call:[edit_file path:x]".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:536 TestTranscriptSplitsEmbeddedNewlines — a provider error
// carries its multi-line JSON body in ONE string; the transcript expands embedded
// newlines so the blank latch works at line granularity, with trailing newlines latched
// away.
#[test]
fn test_transcript_splits_embedded_newlines() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.error("Error: 400 Bad Request {\n  \"message\": \"bad\",\n  \"code\": \"x\"\n}\n");
    tr.user("next");

    let styled = red("Error: 400 Bad Request {\n  \"message\": \"bad\",\n  \"code\": \"x\"\n}\n");
    let rows: Vec<&str> = styled
        .strip_suffix('\n')
        .unwrap_or(&styled)
        .split('\n')
        .collect();
    let want = [
        format!("print:{}", rows.join("|")),
        "print:".to_owned(),
        "user:next".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:697 TestShowcaseSettlesGroupAndExpandsDiff — an expanded call
// (PresentExpanded) is a group boundary: the running group settles first, the showcase
// takes its own widget, and the result expands into the colored diff block under a
// ±count header. Whatever follows opens a fresh group.
#[test]
fn test_showcase_settles_group_and_expands_diff() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("edit it");
    tr.open_call("[read_file path:a]");
    tr.finish_call(
        "[read_file path:a]",
        "ok",
        false,
        Duration::from_secs(1),
        "",
    );
    let art = Artifact {
        kind: ArtifactKind::Diff,
        title: "a".to_owned(),
        lines: vec![
            "@@ -1,3 +1,3 @@".to_owned(),
            " one".to_owned(),
            "-two".to_owned(),
            "+2".to_owned(),
            " three".to_owned(),
        ],
    };
    tr.open_showcase("[edit_file path:a]");
    tr.settle_showcase(
        "[edit_file path:a]",
        Some(&art),
        "1 replacement(s) in a",
        false,
    );
    tr.open_call("[read_file path:b]");

    let mut classic_block = vec!["[read_file path:a]".to_owned()];
    classic_block.extend(classic("ok", false));
    let mut diff_block = vec![format!("[edit_file path:a]{}", dim("  +1 -1"))];
    diff_block.extend(render_diff("a", &art.lines.join("\n"), 30, 80, true, true));
    let want = [
        "user:edit it".to_owned(),
        "print:".to_owned(),
        "call:[read_file path:a]".to_owned(),
        format!("line:{}", event_line("[read_file path:a]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")), // the group settles first
        "print:".to_owned(),
        "call:[edit_file path:a]".to_owned(), // showcase widget: own block
        "settle".to_owned(),
        format!("print:{}", diff_block.join("|")),
        "print:".to_owned(),
        "call:[read_file path:b]".to_owned(), // a fresh group follows
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:731 TestShowcaseDiffBudget — the diff budget follows the live
// screen height (floored at diffMinRows=24): rows beyond it collapse into the
// "… +N more lines" tail.
#[test]
fn test_showcase_diff_budget() {
    let mut lines = vec!["@@ -0,0 +1,40 @@".to_owned()];
    for i in 0..40 {
        lines.push(format!("+row-{i}"));
    }
    let rec = Rec::default(); // Height() = 30 → budget 30: 29 rows + tail
    let tr = transcript(&rec);
    tr.open_showcase("[write_file path:big]");
    tr.settle_showcase(
        "[write_file path:big]",
        Some(&Artifact {
            kind: ArtifactKind::Diff,
            title: String::new(),
            lines,
        }),
        "ok",
        false,
    );

    let out = rec.joined();
    assert!(
        out.contains("… +11 more lines"),
        "missing truncation tail:\n{out}"
    );
    assert!(
        out.contains("row-28") && !out.contains("row-29"),
        "budget must cut after row 28:\n{out}"
    );
    assert!(
        !out.contains("@@"),
        "hunk headers must translate into line numbers, not render:\n{out}"
    );
}

// Go: chat/compose_test.go:756 TestShowcaseFallsBackClassic — a showcase without an
// artifact (declines, errors, tools with nothing to show) falls back to the classic
// header + result form.
#[test]
fn test_showcase_falls_back_classic() {
    let rec = Rec::default();
    let tr = transcript(&rec);
    tr.open_showcase("[edit_file path:x]");
    tr.settle_showcase(
        "[edit_file path:x]",
        None,
        "The user declined this call.",
        true,
    );

    let mut classic_block = vec!["[edit_file path:x]".to_owned()];
    classic_block.extend(classic("The user declined this call.", true));
    let want = [
        "call:[edit_file path:x]".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:622 TestTranscriptImageBlock — an image block is the half-block
// rows plus a dim caption in ONE block, paying one separator like every other block.
#[test]
fn test_transcript_image_block() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    let mut content = tr.content_block();
    content.push(&["Here you go."]);
    tr.close_content();
    tr.image(&["ROW1".to_owned(), "ROW2".to_owned()], "🖼 saved: /x/y.png");

    let want = [
        "print:Here you go.".to_owned(),
        "print:".to_owned(),
        "print:  ROW1|  ROW2".to_owned(), // uniform two-space indent
        format!("print:  {}", dim("🖼 saved: /x/y.png")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:644 TestImageMorphsGenerationWidget — an image-generation widget
// (raised via the composing observer) morphs INTO the image block: the separator was paid at
// the raise, so the image pays no second one.
#[test]
fn test_image_morphs_generation_widget() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("draw");
    tr.open_call("[image_generation …]");
    tr.image(&["ROW".to_owned()], "🖼 saved: /p.png");

    let want = [
        "user:draw".to_owned(),
        "print:".to_owned(),
        "call:[image_generation …]".to_owned(),
        "settle".to_owned(),
        "print:  ROW".to_owned(),
        format!("print:  {}", dim("🖼 saved: /p.png")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/compose_test.go:666 TestImageWidgetSettlesActivityFirst — progressive frames
// arriving over a group with recorded activity settle the group FIRST (frames replace the
// widget body wholesale), and the image then morphs a fresh, dedicated widget.
#[test]
fn test_image_widget_settles_activity_first() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    tr.user("draw with tools");
    tr.open_call("[a]");
    tr.finish_call("[a]", "ok", false, Duration::from_secs(1), "");
    tr.image_widget(); // the first partial frame arrives
    tr.image(&["ROW".to_owned()], "🖼 saved: /p.png");

    let mut classic_block = vec!["[a]".to_owned()];
    classic_block.extend(classic("ok", false));
    let want = [
        "user:draw with tools".to_owned(),
        "print:".to_owned(),
        "call:[a]".to_owned(),
        format!("line:{}", event_line("[a]", "ok", false, "")),
        format!("call:{}", working()),
        "detail:1 tool".to_owned(),
        "settle".to_owned(),
        format!("print:{}", classic_block.join("|")), // the lone call settles classic
        "print:".to_owned(),
        "call:image".to_owned(), // a fresh widget hosts the frames
        "settle".to_owned(),
        "print:  ROW".to_owned(), // …and the image morphs it
        format!("print:  {}", dim("🖼 saved: /p.png")),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

/// New (no Go twin — Go's `imageWidget` deferral is only covered end to end): while a content
/// block streams, the image widget defers exactly like a composing tool call and rises when the
/// block closes, keeping an already-pending call ahead of it.
#[test]
fn image_widget_defers_behind_streaming_content() {
    let rec = Rec::default();
    let tr = transcript(&rec);

    let mut content = tr.open_content();
    content.push(&["thinking out loud"]);
    tr.image_widget(); // deferred: content owns the slot
    assert_eq!(rec.joined(), "print:thinking out loud");
    tr.close_content();

    let want = [
        "print:thinking out loud".to_owned(),
        "print:".to_owned(),
        "call:image".to_owned(),
    ];
    assert_eq!(rec.joined(), want.join("\n"));
}

// Go: chat/userblock_test.go:8 TestWrapByWidth — the PLAIN wrapper the user block and
// composer echo use: CJK runes are width 2, a wide rune never splits across the
// boundary, embedded behavior is pure hard wrap.
#[test]
fn test_wrap_by_width() {
    let tests: [(&str, &str, usize, &[&str]); 6] = [
        ("empty", "", 10, &[""]),
        ("fits", "hello", 10, &["hello"]),
        ("exact", "hello", 5, &["hello"]),
        ("hardwrap", "abcdefg", 3, &["abc", "def", "g"]),
        ("cjk", "你好吗", 3, &["你", "好", "吗"]),
        ("cjk-mixed", "a你b", 3, &["a你", "b"]),
    ];
    for (name, input, width, want) in tests {
        assert_eq!(
            iota::text::ansi::wrap_by_width(input, width),
            want,
            "{name}"
        );
    }
}
