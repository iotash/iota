//! The quiet tool loop (`chat/toolloop_test.go`): the local cap, the no-default-cap contract, the per-round tool
//! refresh, the reasoning-only rule and the final round's images — plus the phase-2 seeding/delta contract
//! (`chat/run.go:68-74`, `:221-229`, `:1092-1095`): an imported history in, the turn's delta out.

use std::{path::Path, sync::Arc};

use iota::chat::turns::RunCtx;
use iota::chat::{ChatError, QuietHost, RunRequest, execute_with_tools, run_once};
use iota::provider::RoundResult;
use iota::provider::model::{
    AssistantBody, Attachment, Body, Message, Raw, RawContent, Role, ToolCall,
};
use iota::provider::usage::Usage;
use iota::testing::{FakeProvider, Round, StaticDispatcher, tool_call_with};
use iota::tool::Dispatcher;
use pretty_assertions::assert_eq;

use crate::common::{GrowingDispatcher, call};

#[tokio::test]
async fn the_tool_loop_stops_at_the_opt_in_cap() {
    // Opt-in limit (--max-turns): the loop stops after exactly N rounds.
    const LIMIT: u32 = 7;
    let tp = FakeProvider::looping(1, 0);
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut history = vec![Message::user("go")];
    let mut host = QuietHost::new();
    let cx = RunCtx::default();

    let err = execute_with_tools(
        &cx,
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        std::num::NonZeroU32::new(LIMIT),
        &mut host,
    )
    .await
    .expect_err("the cap must stop the loop");
    assert!(
        matches!(err, ChatError::LocalCap { turns } if turns.get() == LIMIT),
        "err = {err}, want the local cap"
    );
    assert_eq!(
        err.to_string(),
        "tool loop reached the --max-turns limit without a final response (7 turns)"
    );
    assert_eq!(tp.calls(), 7, "model calls, want exactly the limit");
    // Every completed round was recorded; the cap check happens before the eighth call.
    assert_eq!(host.rec.round_count(), 7);

    // History stays well-formed: the user message followed by complete assistant/tool round pairs — every tool
    // call has its matching result.
    let mut want = vec![Message::user("go")];
    for n in 1..=7 {
        let tc = ToolCall {
            id: format!("call-{n}"),
            name: "noop".to_owned(),
            ..ToolCall::default()
        };
        want.push(Message::assistant_with_calls("", vec![tc.clone()], None));
        want.push(Message::tool_result(&tc, "noop:{}", false));
    }
    assert_eq!(history, want);
    assert_eq!(history.len(), 1 + 2 * 7);
    assert_eq!(history[0].role(), Role::User);
    for pair in history[1..].chunks(2) {
        let (a, r) = (&pair[0], &pair[1]);
        assert_eq!(a.role(), Role::Assistant);
        assert_eq!(a.tool_calls().len(), 1);
        assert_eq!(r.role(), Role::Tool);
        assert_eq!(r.tool_call_id(), a.tool_calls()[0].id);
        assert!(!r.is_error());
    }
}

#[tokio::test]
async fn the_tool_loop_is_unlimited_by_default() {
    // The no-default-cap contract: with max_turns 0 the loop runs past any historical cap and ends only when
    // the model stops calling tools.
    let tp = FakeProvider::looping(1, 75);
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut history = vec![Message::user("go")];
    let mut host = QuietHost::new();
    let outcome = execute_with_tools(
        &RunCtx::default(),
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut host,
    )
    .await
    .expect("unlimited loop errored");
    assert_eq!(tp.calls(), 76);
    assert_eq!(outcome.content, "done");
    assert_eq!(host.rec.round_count(), 76);
    assert_eq!(history.len(), 1 + 2 * 75);
}

#[tokio::test]
async fn execute_with_tools_refreshes_the_tool_set_every_round() {
    // The Once loop re-queries the dispatcher every round: a tool loaded by a search_tools call must be
    // advertised in the very next request.
    // Calls `search_tools` in round 1, answers `done` from round 2 on; the log keeps each round's tool set.
    let tp = FakeProvider::new()
        .with_tools()
        .round(Round::calls(vec![tool_call_with(
            "c1",
            "search_tools",
            &[("query", "late")],
        )]))
        .replying("done");
    let dispatch: Arc<GrowingDispatcher> = Arc::new(GrowingDispatcher::default());
    let mut history = vec![Message::user("go")];
    let mut host = QuietHost::new();
    let outcome = execute_with_tools(
        &RunCtx::default(),
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(outcome.content, "done");
    let seen = tp.seen_tools();
    assert_eq!(seen.len(), 2, "rounds = {}, want 2", seen.len());
    assert_eq!(seen[0], ["search_tools"]);
    assert!(
        seen[1].iter().any(|n| n == "late_tool"),
        "round 2 must advertise the searched-in tool, got {:?}",
        seen[1]
    );
    // The round that searched names its tool in the report; the answering round names none.
    assert_eq!(host.rec.rounds()[0].tools, ["search_tools"]);
    assert!(host.rec.rounds()[1].tools.is_empty());
}

// New (chat.go:318-323): a round with no tool calls, no content and some reasoning answers WITH the reasoning.
#[tokio::test]
async fn reasoning_only_reply_is_the_reply() {
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            reasoning: "the answer is 42".to_owned(),
            ..RoundResult::default()
        }],
        "unused",
    );
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut history = vec![Message::user("go")];
    let mut host = QuietHost::new();
    let outcome = execute_with_tools(
        &RunCtx::default(),
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(outcome.content, "the answer is 42");
    assert_eq!(outcome.reasoning, "the answer is 42");
    // Nothing was appended: the terminating round is not stored on history.
    assert_eq!(history, vec![Message::user("go")]);

    // With content present the reasoning stays reasoning.
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            content: "visible".to_owned(),
            reasoning: "hidden".to_owned(),
            ..RoundResult::default()
        }],
        "unused",
    );
    let outcome = execute_with_tools(
        &RunCtx::default(),
        &tp,
        dispatch.clone(),
        &mut history,
        dispatch.tools(),
        "",
        None,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(outcome.content, "visible");
    assert_eq!(outcome.reasoning, "hidden");

    // And run_once surfaces the reasoning-only reply as THE reply.
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            reasoning: "only thought".to_owned(),
            ..RoundResult::default()
        }],
        "unused",
    );
    let out = run_once(
        &RunCtx::default(),
        &tp,
        &RunRequest {
            message: "go".to_owned(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");
    assert_eq!(out.reply, "only thought");
}

// New (chat.go:112 `saveImagesQuiet(tp)` after the loop): only the TERMINATING round's images are saved.
#[tokio::test]
async fn tool_loop_final_round_images_are_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let early = Attachment {
        filename: "early.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: b"EARLY".to_vec(),
    };
    let last = Attachment {
        filename: "last.png".to_owned(),
        mime_type: "image/png".to_owned(),
        data: b"LAST".to_vec(),
    };
    let tp = FakeProvider::scripted(
        vec![
            RoundResult {
                tool_calls: vec![call("c1", "noop")],
                images: vec![early],
                ..RoundResult::default()
            },
            RoundResult {
                content: "done".to_owned(),
                images: vec![last],
                ..RoundResult::default()
            },
        ],
        "done",
    );
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let mut host = QuietHost::new();
    let out = run_once(
        &RunCtx::default(),
        &tp,
        &RunRequest {
            message: "draw".to_owned(),
            ..RunRequest::default()
        },
        dispatch.clone(),
        None,
        &mut host,
        Some(dir.path()),
    )
    .await
    .expect("run failed");
    assert_eq!(out.reply, "done");
    assert!(out.image_errors.is_empty(), "{:?}", out.image_errors);
    assert_eq!(out.images.len(), 1, "images = {:?}", out.images);
    assert_eq!(std::fs::read(&out.images[0]).expect("saved file"), b"LAST");
    assert!(out.images[0].ends_with("-0.png"), "{}", out.images[0]);
    // The earlier round's image never reached the disk.
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(host.rec.round_count(), 2);

    // Children pass None: nothing is saved, and every image is an error line (POLICY I-06 + HOME_NOT_DEFINED).
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            content: "done".to_owned(),
            images: vec![Attachment {
                mime_type: "image/png".to_owned(),
                ..Attachment::default()
            }],
            ..RoundResult::default()
        }],
        "done",
    );
    let out = run_once(
        &RunCtx::default(),
        &tp,
        &RunRequest {
            message: "draw".to_owned(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");
    assert!(out.images.is_empty());
    assert_eq!(
        out.image_errors,
        ["saving image failed: $HOME is not defined"]
    );
}

/// A tool provider that plays `rounds`, then `tail` on every later call — the seeding and delta
/// assertions read the exact prompt off its log.
fn recording(rounds: Vec<RoundResult>, tail: RoundResult) -> FakeProvider {
    FakeProvider::new()
        .with_tools()
        .rounds(rounds.into_iter().map(Round::result))
        .tail(Round::result(tail))
}

/// A three-message imported view, as a resumed bundle hands it over.
fn imported() -> Vec<Message> {
    vec![
        Message::system("stored system"),
        Message::user("earlier"),
        Message::assistant("hi"),
    ]
}

/// Round usage n: `{input: 10n, output: n, total: 11n}`.
fn usage(n: u64) -> Usage {
    Usage {
        input: 10 * n,
        output: n,
        total: 11 * n,
        ..Usage::default()
    }
}

// New (chat/run.go:68-74): the imported history is replayed VERBATIM, ahead of the turn's user message, on every
// call of the turn.
#[tokio::test]
async fn imported_history_is_sent_before_the_new_user_message() {
    let tc = call("c1", "noop");
    let p = recording(
        vec![RoundResult {
            tool_calls: vec![tc.clone()],
            usage: Some(usage(1)),
            ..RoundResult::default()
        }],
        RoundResult {
            content: "done".to_owned(),
            usage: Some(usage(2)),
            ..RoundResult::default()
        },
    );
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");
    assert_eq!(out.reply, "done");

    let sent = p.sent();
    assert_eq!(sent.len(), 2, "rounds = {}", sent.len());
    // Round 1: exactly the imported view, unchanged, then the new user message.
    let mut want = imported();
    want.push(Message::user("go"));
    assert_eq!(sent[0], want);
    // Round 2 keeps carrying it: the imported prefix is never rewritten.
    assert_eq!(sent[1][..4], want[..]);
    assert_eq!(sent[1].len(), 6);
}

// New (chat/run.go:221-229, `history[persisted:]`): the delta is the turn ONLY — never the imported history —
// and every assistant message in it carries the usage of the round that paid for it (D-55).
#[tokio::test]
async fn delta_is_the_turn_only_with_usage_on_every_assistant() {
    let tc = call("c1", "noop");
    let p = recording(
        vec![RoundResult {
            tool_calls: vec![tc.clone()],
            usage: Some(usage(1)),
            ..RoundResult::default()
        }],
        RoundResult {
            content: "done".to_owned(),
            usage: Some(usage(2)),
            ..RoundResult::default()
        },
    );
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");

    assert_eq!(
        out.delta,
        vec![
            Message::user("go"),
            Message::assistant_with_calls("", vec![tc.clone()], None).with_usage(Some(usage(1))),
            Message::tool_result(&tc, "noop:{}", false),
            Message {
                content: "done".to_owned(),
                body: Body::Assistant(AssistantBody {
                    usage: Some(usage(2)),
                    ..AssistantBody::default()
                }),
                ..Message::default()
            },
        ]
    );
}

// New (chat/run.go:69-74): with NO imported history the `-s` system message is pushed first — and, the watermark
// being 0, it is part of the delta, so a session that starts here stores its own system prompt.
#[tokio::test]
async fn empty_history_keeps_the_system_prompt_first_and_in_the_delta() {
    let p = recording(
        Vec::new(),
        RoundResult {
            content: "ok".to_owned(),
            usage: Some(usage(3)),
            ..RoundResult::default()
        },
    );
    // No tools advertised: the unary path, which seeds the same way.
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&[]));
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            system: "be brief".to_owned(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");

    assert_eq!(
        p.sent()[0],
        vec![Message::system("be brief"), Message::user("go")]
    );
    assert_eq!(
        out.delta,
        vec![
            Message::system("be brief"),
            Message::user("go"),
            Message {
                content: "ok".to_owned(),
                body: Body::Assistant(AssistantBody {
                    usage: Some(usage(3)),
                    ..AssistantBody::default()
                }),
                ..Message::default()
            },
        ]
    );
}

// New (chat/run.go:69-74): a NON-EMPTY history wins over `system` — a resumed session keeps the system message
// from its own log and `-s` is inert.
#[tokio::test]
async fn imported_history_makes_the_system_prompt_inert() {
    let p = recording(
        Vec::new(),
        RoundResult {
            content: "ok".to_owned(),
            ..RoundResult::default()
        },
    );
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&[]));
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            system: "IGNORED".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");

    let sent = &p.sent()[0];
    assert_eq!(sent[0], Message::system("stored system"));
    assert!(
        !sent.iter().any(|m| m.content == "IGNORED"),
        "-s must not reach the model: {sent:?}"
    );
    let mut want = imported();
    want.push(Message::user("go"));
    assert_eq!(*sent, want);
    // The delta starts at the watermark: the imported view is never re-persisted.
    assert_eq!(out.delta.len(), 2);
    assert_eq!(out.delta[0], Message::user("go"));
}

// New (D-43): a failed round fails the run — there is no outcome, so nothing is persisted.
#[tokio::test]
async fn a_failed_round_yields_no_delta() {
    let p = FakeProvider::reporting(5, Some(1));
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let err = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect_err("call 1 fails");
    assert_eq!(err.to_string(), "boom");
    assert!(
        matches!(err, ChatError::Provider(_)),
        "err = {err}, want the upstream failure"
    );
}

// New (D-53, chat/images.go:117-148 `collectImages`): the final assistant carries exactly the SAVED images, each
// `filename` rewritten to the saved file's basename; a failed save is reported and NOT attached.
#[tokio::test]
async fn final_assistant_carries_the_saved_image_subset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let img = |name: &str, data: &[u8]| Attachment {
        filename: name.to_owned(),
        mime_type: "image/png".to_owned(),
        data: data.to_vec(),
    };
    let script = || {
        RoundResult {
            content: "drawn".to_owned(),
            // Generated images carry no usable name — Go rewrites it to the saved basename before persisting.
            images: vec![img("", b"ONE"), img("", b"TWO")],
            usage: Some(usage(4)),
            ..RoundResult::default()
        }
    };
    let p = recording(Vec::new(), script());
    let dispatch: Arc<StaticDispatcher> = Arc::new(StaticDispatcher::new(&["noop"]));
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "draw".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch.clone(),
        None,
        &mut QuietHost::new(),
        Some(dir.path()),
    )
    .await
    .expect("run failed");

    assert!(out.image_errors.is_empty(), "{:?}", out.image_errors);
    assert_eq!(out.images.len(), 2);
    let last = out.delta.last().expect("final assistant");
    assert_eq!(last.role(), Role::Assistant);
    assert_eq!(last.content, "drawn");
    assert_eq!(last.usage(), Some(usage(4)));
    assert_eq!(last.attachments.len(), 2);
    for (att, path) in last.attachments.iter().zip(&out.images) {
        let base = Path::new(path)
            .file_name()
            .expect("basename")
            .to_str()
            .expect("utf-8");
        assert_eq!(att.filename, base, "filename must be the saved basename");
        assert_eq!(att.mime_type, "image/png");
        assert_eq!(std::fs::read(path).expect("saved file"), att.data);
    }
    assert_eq!(last.attachments[0].data, b"ONE");
    assert_eq!(last.attachments[1].data, b"TWO");

    // A save that FAILS lands in image_errors and attaches nothing (images_dir None → HOME_NOT_DEFINED).
    let p = recording(Vec::new(), script());
    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "draw".to_owned(),
            history: imported(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");
    assert!(out.images.is_empty());
    assert_eq!(
        out.image_errors,
        [
            "saving image failed: $HOME is not defined",
            "saving image failed: $HOME is not defined"
        ]
    );
    let last = out.delta.last().expect("final assistant");
    assert!(
        last.attachments.is_empty(),
        "a failed save must not be attached: {:?}",
        last.attachments
    );
}

// The terminating round's raw blocks ride the final assistant message.
// A thinking block must go back on every later request that replays this turn, and a turn that ends
// in text (no tool call) is the common case, so dropping the blocks there breaks the replay contract
// for ordinary conversation, not just tool rounds.
#[tokio::test]
async fn terminating_round_raw_content_rides_the_final_message() {
    let sealed = Raw::from_string(
        r#"{"type":"thinking","thinking":"weigh it","signature":"SEAL"}"#.to_owned(),
    )
    .expect("raw");
    let tp = FakeProvider::scripted(
        vec![RoundResult {
            content: "the answer".to_owned(),
            raw_content: Some(RawContent::Anthropic(vec![sealed.clone()])),
            ..RoundResult::default()
        }],
        "unused",
    );
    let out = run_once(
        &RunCtx::default(),
        &tp,
        &RunRequest {
            message: "go".to_owned(),
            ..RunRequest::default()
        },
        Arc::new(StaticDispatcher::new(&["noop"])),
        None,
        &mut QuietHost::new(),
        None,
    )
    .await
    .expect("run failed");

    let last = out.delta.last().expect("delta has the assistant message");
    assert_eq!(last.role(), Role::Assistant);
    assert_eq!(last.content, "the answer");
    assert_eq!(
        last.raw_content().cloned(),
        Some(RawContent::Anthropic(vec![sealed])),
        "the closing round's thinking block must persist with the turn"
    );
}
