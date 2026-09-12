//! The resume echo (chat/replay.go): the last rounds of a resumed conversation replayed
//! into the terminal so the user sees where they left off.
//!
//! The replay mirrors the LIVE layout rather than the log: user messages as full-width
//! reversed blocks, assistant replies through the markdown renderer, and one blank line
//! between blocks. Tool activity is deliberately NOT replayed verbatim — each run of tool
//! results collapses into the live summary's timeless form, a dim `"◇ ran N tool(s)"` line
//! — except interactive tools, whose results replay as their `"?"` record blocks: those
//! are the user's own answers. Reasoning and tool bodies are never replayed.

use std::path::Path;

use crate::chat::images::{IMAGE_INDENT_COLS, IMAGE_MAX_COLS, IMAGE_MAX_ROWS};
use crate::markdown::hyperlink;
use crate::provider::model::{Attachment, Message, Role};
use crate::text::ansi::wrap_by_width;
use crate::text::width::str_width;

use crate::repl::group::ask_record_lines;
use crate::repl::styles::{dim, reverse};
use crate::repl::uisink::LineCommitter;

/// How many trailing rounds a resumed session replays (chat/replay.go:19
/// `resumeEchoRounds`).
pub(crate) const RESUME_ECHO_ROUNDS: usize = 3;

/// The user block's gutter (chat/chat.go:160 `userPrompt`).
const USER_PROMPT: &str = "❯ ";

/// Indent of a replayed image's rows and caption (`" " × IMAGE_INDENT_COLS`, chat/images.go:66).
fn image_indent() -> String {
    " ".repeat(IMAGE_INDENT_COLS)
}

/// Whether `path` names an existing REGULAR file (chat/replay.go:194-198 `fileExists`) — the
/// guard behind every `file://` caption: a missing file gets the bare name, never a dead link.
pub(crate) fn file_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// The clickable caption for an attachment whose file still lives under `img_dir`
/// (chat/replay.go:167-172): the absolute path, hyperlinked; `None` when there is no directory,
/// no filename, or no file.
fn linked_path(img_dir: Option<&Path>, filename: &str) -> Option<String> {
    let dir = img_dir?;
    if filename.is_empty() {
        return None;
    }
    let p = dir.join(filename);
    if !file_exists(&p) {
        return None;
    }
    let p = p.to_string_lossy().into_owned();
    Some(hyperlink(&format!("file://{p}"), &p, true))
}

/// Renders one replayed attachment (chat/replay.go:163-192 `echoImage`): the half-block rows at
/// the live turn's size caps and indent, then a dim caption.
///
/// The caption is the bare filename, upgraded to the live turn's clickable path (OSC 8) when the
/// file still exists under `img_dir`. A non-image attachment, empty data or a decode failure
/// falls back to the caption line ALONE — the replay never claims to have drawn what it could
/// not decode.
pub(crate) fn echo_image(
    att: &Attachment,
    term_width: usize,
    img_dir: Option<&Path>,
) -> Vec<String> {
    let indent = image_indent();
    let caption = linked_path(img_dir, &att.filename).unwrap_or_else(|| att.filename.clone());
    let caption_line = dim(&format!("{indent}🖼 {caption}"));
    if !att.is_image() || att.data.is_empty() {
        return vec![caption_line];
    }
    let max_cols = IMAGE_MAX_COLS.min(term_width.saturating_sub(2 + IMAGE_INDENT_COLS));
    let Ok(rows) = crate::imgterm::render(&att.data, max_cols, IMAGE_MAX_ROWS) else {
        return vec![caption_line];
    };
    let mut out: Vec<String> = rows.iter().map(|r| format!("{indent}{r}")).collect();
    out.push(caption_line);
    out
}

/// The tail of `history` covering at most `n` rounds — a round starts at a user message
/// and runs until the next one (chat/replay.go:26-48).
///
/// When `history` holds fewer than `n` rounds the whole tail is returned; `n == 0` returns
/// nothing.
///
/// Go filtered system messages OUT of the returned slice; a borrowed subslice can only
/// trim its front, so LEADING system messages are skipped here and an interior one (the
/// `system-tools` mount a round appends) rides along — [`echo_rounds`] renders nothing for
/// the System role, so the echoed bytes are identical either way (DEVIATIONS3 `[WP50]`).
pub(crate) fn last_rounds(history: &[Message], n: usize) -> &[Message] {
    if n == 0 {
        return &[];
    }
    let mut start = 0;
    let mut rounds = 0;
    for (i, m) in history.iter().enumerate().rev() {
        if m.role() == Role::User {
            rounds += 1;
            if rounds == n {
                start = i;
                break;
            }
        }
    }
    while start < history.len() && history[start].role() == Role::System {
        start += 1;
    }
    &history[start..]
}

/// Commits the round's aggregated tool-activity line, once something else is about to
/// print (or the replay ends), so it lands ABOVE the round's reply like the live summary.
fn flush_tools(w: &mut LineCommitter, n: &mut u32) {
    if *n == 0 {
        return;
    }
    let unit = if *n == 1 { "tool" } else { "tools" };
    w.write(&dim(&format!("◇ ran {n} {unit}")));
    w.write("\n\n");
    *n = 0;
}

/// Replays `msgs` (as returned by [`last_rounds`]) as display lines, at `width` columns
/// (chat/replay.go:56-160 `echoRounds`).
///
/// `interactive` reports whether a tool's results are the USER's answers (the ask set):
/// those replay as `"?"` record blocks through the live renderer, everything else folds
/// into the round's `"◇ ran N tool(s)"` line.
///
/// `img_dir` is the session's images directory (`None` while ephemeral): an image whose file
/// still exists there re-renders as half-blocks under the live turn's clickable-path caption
/// ([`echo_image`]), and a user attachment still on disk gets the same clickable marker.
pub(crate) fn echo_rounds(
    msgs: &[Message],
    interactive: impl Fn(&str) -> bool,
    width: usize,
    img_dir: Option<&Path>,
) -> Vec<String> {
    let mut w = LineCommitter::default();
    // Assistant attachment names in this window: a user attachment with the same name is
    // the /edit canvas copy of an image rendered elsewhere in the echo — repeating it as a
    // marker is pure noise. When the canvas's source round fell outside the window the
    // name will not match and the marker rightly shows (then it IS the only trace).
    let rendered: Vec<&str> = msgs
        .iter()
        .filter(|m| m.role() == Role::Assistant)
        .flat_map(|m| m.attachments.iter())
        .map(|a| a.filename.as_str())
        .filter(|n| !n.is_empty())
        .collect();

    let mut tool_results: u32 = 0;
    for msg in msgs {
        match msg.role() {
            // A host notice replays as the ONE dim line it was printed as, not as a `❯` block: the
            // rest of its text is the job output the model read, and the log file still holds it.
            Role::User if msg.is_notice() => {
                flush_tools(&mut w, &mut tool_results);
                w.write(&dim(msg.content.lines().next().unwrap_or_default()));
                w.write("\n\n");
            }
            Role::User => {
                flush_tools(&mut w, &mut tool_results);
                for row in print_user_block(&msg.content, width) {
                    w.write(&row);
                    w.write("\n");
                }
                // Named per-file lines instead of a bare count; canvas copies are
                // suppressed (see `rendered`).
                for att in &msg.attachments {
                    if rendered.contains(&att.filename.as_str()) {
                        continue;
                    }
                    // Files still present in the session's images dir get the clickable path.
                    let label = linked_path(img_dir, &att.filename).unwrap_or_else(|| {
                        if att.filename.is_empty() {
                            att.mime_type.clone()
                        } else {
                            att.filename.clone()
                        }
                    });
                    w.write(&dim(&format!("📎 {label}")));
                    w.write("\n");
                }
                w.write("\n");
            }
            Role::Assistant => {
                if msg.content.is_empty() && msg.attachments.is_empty() {
                    continue; // a tool-call-only step; its results are counted below
                }
                flush_tools(&mut w, &mut tool_results);
                if !msg.content.is_empty() {
                    w.write(&render_markdown(msg.content.trim_end_matches('\n'), width));
                }
                // Generated images re-render as half-blocks, mirroring the live turn (same size
                // caps and indent); a decode failure or a non-image attachment falls back to the
                // caption line alone.
                for (i, att) in msg.attachments.iter().enumerate() {
                    if !msg.content.is_empty() || i > 0 {
                        w.write("\n");
                    }
                    for row in echo_image(att, width, img_dir) {
                        w.write(&row);
                        w.write("\n");
                    }
                }
                if msg.interrupted() {
                    w.write(&dim("(interrupted)"));
                    w.write("\n");
                }
                w.write("\n");
            }
            Role::Tool => {
                if interactive(msg.tool_call_name()) {
                    // The user's own answers replay as the "?" record block, in the live
                    // block's exact styling (the shared renderer).
                    flush_tools(&mut w, &mut tool_results);
                    for ln in ask_record_lines(&msg.content, msg.is_error()) {
                        w.write(&ln);
                        w.write("\n");
                    }
                    w.write("\n");
                    continue;
                }
                tool_results += 1;
            }
            Role::System => {}
        }
    }
    flush_tools(&mut w, &mut tool_results);
    w.flush()
}

/// Renders one message as a stack of full-width reversed rows (chat/replay.go:196-215
/// `printUserBlock`).
///
/// A two-column gutter keeps `"❯ "` on the first row — so the block still reads as a
/// prompt — and aligns wrapped rows under it; padding fills every row to the same display
/// width, which is what makes the reversed background a block instead of a ragged edge.
pub(crate) fn print_user_block(display: &str, width: usize) -> Vec<String> {
    let gutter_width = str_width(USER_PROMPT);
    let body = width.saturating_sub(gutter_width);
    wrap_by_width(display, body)
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let gutter = if i == 0 { USER_PROMPT } else { "  " };
            let pad = width
                .saturating_sub(str_width(&line))
                .saturating_sub(gutter_width);
            reverse(&format!("{gutter}{line}{}", " ".repeat(pad)))
        })
        .collect()
}

/// One-shot markdown render of a replayed reply, always newline-terminated.
///
/// `Writer::write` consumes complete lines and `flush` may leave the trailing one
/// unterminated; closing it here (and only when needed) keeps the round separator at
/// exactly one blank line — the live loop's spacing.
fn render_markdown(content: &str, width: usize) -> String {
    let out = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut mdw = crate::markdown::Writer::new(
        Box::new(EchoSink {
            out: std::sync::Arc::clone(&out),
            width,
        }),
        crate::markdown::RenderOptions {
            color: true,
            code_theme: crate::markdown::CodeTheme::Monokai,
        },
    );
    mdw.write(content.as_bytes());
    mdw.flush();
    let mut s = std::mem::take(
        &mut *out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// The replay's markdown sink: a fixed width and no previews (nothing is streaming).
struct EchoSink {
    out: std::sync::Arc<std::sync::Mutex<String>>,
    width: usize,
}

impl crate::markdown::sink::Sink for EchoSink {
    fn write(&mut self, rendered: &str) {
        self.out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_str(rendered);
    }

    fn width(&self) -> usize {
        self.width
    }

    fn block_preview(
        &mut self,
        _label: &str,
    ) -> Option<Box<dyn crate::markdown::sink::PreviewHandle>> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    //! The resume echo (`chat/replay_test.go`): which messages a round covers, and what the
    //! replay may and may not show.
    //!
    //! `print_user_block` is crate-private, so these tests live in-file (formerly a
    //! `#[path]`-mounted `tests/replay.rs`; merged 2026-09-02).

    use std::path::Path;

    use crate::provider::model::{AssistantBody, Attachment, Body, Message, ToolBody, ToolCall};
    use crate::repl::replay::{echo_image, echo_rounds, last_rounds, print_user_block};
    use crate::text::ansi::strip_sgr;
    use crate::text::width::str_width;
    use pretty_assertions::assert_eq;

    fn user(s: &str) -> Message {
        Message::user(s)
    }

    fn assistant(s: &str) -> Message {
        Message::assistant(s)
    }

    fn tool(name: &str, content: &str) -> Message {
        Message {
            content: content.to_owned(),
            body: Body::Tool(ToolBody {
                call_name: name.to_owned(),
                ..ToolBody::default()
            }),
            ..Message::default()
        }
    }

    fn calls(name: &str) -> Message {
        Message {
            body: Body::Assistant(AssistantBody {
                tool_calls: vec![ToolCall {
                    name: name.to_owned(),
                    ..ToolCall::default()
                }],
                ..AssistantBody::default()
            }),
            ..Message::default()
        }
    }

    /// Nothing is interactive (Go's `nil` predicate).
    fn no_ask(_: &str) -> bool {
        false
    }

    fn plain(lines: &[String]) -> String {
        lines
            .iter()
            .map(|l| strip_sgr(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Go: chat/replay_test.go:16 TestLastRounds — a round starts at a user message; system
    // messages are excluded; fewer rounds than asked returns the whole tail; trailing tool
    // results belong to the round that called them.
    #[test]
    fn test_last_rounds() {
        let sys = Message::system("sys");
        let cases: Vec<(&str, Vec<Message>, usize, Vec<Message>)> = vec![
            ("empty", Vec::new(), 3, Vec::new()),
            (
                "zero-rounds",
                vec![user("q"), assistant("r")],
                0,
                Vec::new(),
            ),
            (
                "fewer-than-n-returns-all",
                vec![user("q1"), assistant("r1"), user("q2"), assistant("r2")],
                3,
                vec![user("q1"), assistant("r1"), user("q2"), assistant("r2")],
            ),
            (
                "exactly-n",
                vec![
                    user("q1"),
                    assistant("r1"),
                    user("q2"),
                    assistant("r2"),
                    user("q3"),
                    assistant("r3"),
                ],
                3,
                vec![
                    user("q1"),
                    assistant("r1"),
                    user("q2"),
                    assistant("r2"),
                    user("q3"),
                    assistant("r3"),
                ],
            ),
            (
                "more-than-n-keeps-last-n-starting-at-user",
                vec![
                    user("q1"),
                    assistant("r1"),
                    user("q2"),
                    assistant("r2"),
                    user("q3"),
                    assistant("r3"),
                    user("q4"),
                    assistant("r4"),
                ],
                3,
                vec![
                    user("q2"),
                    assistant("r2"),
                    user("q3"),
                    assistant("r3"),
                    user("q4"),
                    assistant("r4"),
                ],
            ),
            (
                "system-excluded",
                vec![sys.clone(), user("q1"), assistant("r1")],
                3,
                vec![user("q1"), assistant("r1")],
            ),
            (
                "trailing-tool-results-kept",
                vec![
                    user("q1"),
                    assistant("r1"),
                    user("q2"),
                    calls("f"),
                    tool("f", "result"),
                    tool("f", "result"),
                ],
                1,
                vec![
                    user("q2"),
                    calls("f"),
                    tool("f", "result"),
                    tool("f", "result"),
                ],
            ),
        ];
        for (name, history, n, want) in cases {
            assert_eq!(last_rounds(&history, n), want.as_slice(), "{name}");
        }
    }

    // Go: chat/replay_test.go:70 TestEchoRounds — the replay shows the conversation's SHAPE:
    // user blocks, attachment markers, one aggregated tool line ABOVE the round's reply, the
    // reply itself and the interrupted marker. Tool bodies and reasoning never replay.
    #[test]
    fn test_echo_rounds() {
        let msgs = vec![
            Message {
                attachments: vec![
                    Attachment {
                        filename: "a.png".to_owned(),
                        ..Attachment::default()
                    },
                    Attachment {
                        filename: "b.pdf".to_owned(),
                        ..Attachment::default()
                    },
                ],
                ..user("first question")
            },
            calls("shell"),
            tool("shell", "output 1"),
            tool("shell", "output 2"),
            assistant("final answer").with_reasoning("secret thinking".to_owned()),
            user("second question"),
            assistant("partial reply").with_interrupted(true),
        ];
        let out = plain(&echo_rounds(&msgs, no_ask, 80, None));

        for want in [
            "first question",
            "📎 a.png",
            "📎 b.pdf",
            "◇ ran 2 tools",
            "final answer",
            "second question",
            "partial reply",
            "(interrupted)",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        for forbidden in ["output 1", "output 2", "secret thinking"] {
            assert!(!out.contains(forbidden), "leaked {forbidden:?} in:\n{out}");
        }
        assert!(
            out.find("◇ ran 2 tools") < out.find("final answer"),
            "the tool line must precede the round's reply:\n{out}"
        );
    }

    // Go: chat/replay_test.go:110 TestEchoRoundsTrailingToolResults — a history ending in tool
    // results still flushes its aggregated line at the end of the replay.
    #[test]
    fn test_echo_rounds_trailing_tool_results() {
        let msgs = vec![user("do the thing"), calls("f"), tool("f", "out")];
        let out = plain(&echo_rounds(&msgs, no_ask, 80, None));
        assert!(out.contains("◇ ran 1 tool"), "{out}");
    }

    // Go: chat/replay_test.go:256 TestEchoRoundsAskRecord — interactive results are the USER's
    // own answers: they replay as the "?" record block instead of folding into the tool count.
    #[test]
    fn test_echo_rounds_ask_record() {
        let msgs = vec![
            user("pick for me"),
            calls("choose"),
            tool("choose", "Auth: JWT\nLib: chi"),
            tool("shell", "out"),
            assistant("done"),
        ];
        let out = plain(&echo_rounds(&msgs, |n| n == "choose", 80, None));
        for want in ["? Auth: JWT", "  Lib: chi", "◇ ran 1 tool"] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        assert!(
            !out.contains("2 tools"),
            "the ask result must not count as a tool:\n{out}"
        );
    }

    // Go: chat/replay_test.go:281 TestEchoRoundsAskRecordParity — the replayed record uses the
    // LIVE block's renderer: error styling survives and an empty answer falls back to
    // "(no answer)" rather than a blank row.
    #[test]
    fn test_echo_rounds_ask_record_parity() {
        let msgs = vec![
            user("pick"),
            calls("choose"),
            tool("choose", "boom").with_error(true),
            tool("confirm", "  \n"),
        ];
        let lines = echo_rounds(&msgs, |_| true, 80, None);
        let styled_first = crate::repl::group::ask_record_lines("boom", true)
            .first()
            .cloned()
            .expect("the shared renderer produces a row");
        assert!(
            lines.contains(&styled_first),
            "an errored ask must replay with the live error styling: {lines:?}"
        );
        assert!(
            plain(&lines).contains("? (no answer)"),
            "an empty ask must replay as \"(no answer)\": {lines:?}"
        );
    }

    // Go: chat/replay_test.go:224 TestPrintUserBlockMultiline — one row per embedded line, the
    // "❯ " gutter on row 0 and a two-space indent after, every row padded to the SAME display
    // width (that is what makes the reversed background a block).
    #[test]
    fn test_print_user_block_multiline() {
        let rows = print_user_block("first line\nsecond line\nthird", 40);
        assert_eq!(rows.len(), 3, "one row per line: {rows:?}");
        let mut width = 0;
        for (i, row) in rows.iter().enumerate() {
            let flat = strip_sgr(row);
            assert!(
                !flat.contains('\r'),
                "row {i} carries a control char: {flat:?}"
            );
            let want = if i == 0 { "❯ " } else { "  " };
            assert!(
                flat.starts_with(want),
                "row {i} = {flat:?}, want the {want:?} gutter"
            );
            let w = str_width(&flat);
            if i == 0 {
                width = w;
            } else {
                assert_eq!(w, width, "row {i} must pad to the full block width");
            }
        }
        assert_eq!(width, 40, "the block fills the terminal width");
    }

    /// The checked-in 2×2 fixture: top row red, bottom row blue — the rasteriser's byte golden
    /// (`T3_TEST_PLAN` §2). Kept HERE rather than in `imgterm.rs` because only that module may
    /// name the `image` crate, so this is the one place a real PNG file proves the pipeline.
    const RB_2X2_PNG: &[u8] = include_bytes!("../../tests/fixtures/images/rb-2x2.png");

    /// GOLDEN: the checked-in PNG rasterises to the exact Go bytes (`imgterm_test.go:44`) — one
    /// line of two half-block cells, fg the top pixel, bg the bottom, reset-terminated.
    #[test]
    fn rasteriser_golden_from_the_checked_in_png() {
        let rows = crate::imgterm::render(RB_2X2_PNG, 80, 24).expect("the fixture decodes");
        assert_eq!(
            rows,
            vec![
                "\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[0m"
                    .to_owned()
            ]
        );
    }

    // Go: chat/replay_test.go:128 TestEchoRoundsRendersImages — a replayed assistant image
    // renders as half-blocks with a filename caption, not just the name (the pre-imagen
    // placeholder); undecodable data falls back to the caption line alone.
    #[test]
    fn test_echo_rounds_renders_images() {
        let msgs = vec![
            user("draw"),
            Message {
                attachments: vec![Attachment {
                    filename: "image-1.png".to_owned(),
                    mime_type: "image/png".to_owned(),
                    data: RB_2X2_PNG.to_vec(),
                }],
                ..Message {
                    body: Body::Assistant(AssistantBody {
                        ..AssistantBody::default()
                    }),
                    ..Message::default()
                }
            },
        ];
        let out = plain(&echo_rounds(&msgs, no_ask, 100, None));
        assert!(out.contains('▀'), "no half-block rows in echo:\n{out}");
        assert!(out.contains("🖼 image-1.png"), "caption missing:\n{out}");

        let broken = vec![Message {
            attachments: vec![Attachment {
                filename: "broken.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: vec![1, 2],
            }],
            ..Message {
                body: Body::Assistant(AssistantBody {
                    ..AssistantBody::default()
                }),
                ..Message::default()
            }
        }];
        let out = plain(&echo_rounds(&broken, no_ask, 100, None));
        assert!(
            !out.contains('▀') && out.contains("🖼 broken.png"),
            "a broken image must fall back to the caption line:\n{out}"
        );
    }

    // Go: chat/replay_test.go:165 TestEchoImageLinkedCaption — with the session's images
    // directory supplied and the file still on disk the caption becomes the live turn's
    // clickable path; a missing file falls back to the bare filename.
    #[test]
    fn test_echo_image_linked_caption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("image-1.png");
        std::fs::write(&path, [9_u8]).expect("write");
        // Undecodable data: the caption line is all there is, which is what is pinned.
        let att = Attachment {
            filename: "image-1.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: vec![1],
        };

        let out = plain(&echo_image(&att, 100, Some(dir.path())));
        assert!(
            out.contains(&path.to_string_lossy().into_owned()),
            "the caption must carry the on-disk path:\n{out}"
        );

        let gone = Attachment {
            filename: "gone.png".to_owned(),
            ..att.clone()
        };
        let out = plain(&echo_image(&gone, 100, Some(dir.path())));
        assert!(
            !out.contains(&dir.path().to_string_lossy().into_owned()) && out.contains("gone.png"),
            "a missing file must fall back to the bare name:\n{out}"
        );
    }

    // Go: chat/replay_test.go:192 TestEchoRoundsCanvasDedup — a user attachment sharing its name
    // with an assistant image in the echoed window is suppressed (that picture already rendered
    // above); when the source round fell outside the window the marker shows — then it is the
    // only trace of the reference.
    #[test]
    fn test_echo_rounds_canvas_dedup() {
        let img = Attachment {
            filename: "gen-1.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: vec![1],
        };
        let gen2 = Attachment {
            filename: "gen-2.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: vec![2],
        };
        let assistant_with = |att: &Attachment| Message {
            attachments: vec![att.clone()],
            body: Body::Assistant(AssistantBody {
                ..AssistantBody::default()
            }),
            ..Message::default()
        };

        let msgs = vec![
            user("a cat"),
            assistant_with(&img),
            Message {
                attachments: vec![img.clone()],
                ..user("add a hat")
            }, // the /edit turn
            assistant_with(&gen2),
        ];
        let out = plain(&echo_rounds(&msgs, no_ask, 100, None));
        assert!(
            !out.contains('📎'),
            "the canvas copy must be suppressed:\n{out}"
        );

        // The same /edit turn, but its source round is outside the window.
        let msgs = vec![
            Message {
                attachments: vec![img],
                ..user("add a hat")
            },
            assistant_with(&gen2),
        ];
        let out = plain(&echo_rounds(&msgs, no_ask, 100, None));
        assert!(
            out.contains("📎 gen-1.png"),
            "a cut-off canvas must keep its marker:\n{out}"
        );
    }

    /// New (no Go twin — Go's `echoImage` never saw a linked, DECODABLE image in a test): with
    /// the file on disk the caption is a real OSC 8 hyperlink wrapping the absolute path, and
    /// the half-block rows still precede it under the two-space indent.
    #[test]
    fn linked_caption_wraps_the_path_in_osc8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("image-1.png");
        std::fs::write(&path, RB_2X2_PNG).expect("write");
        let att = Attachment {
            filename: "image-1.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: RB_2X2_PNG.to_vec(),
        };
        let rows = echo_image(&att, 100, Some(dir.path()));
        assert_eq!(rows.len(), 2, "one picture row plus the caption: {rows:?}");
        assert!(rows[0].starts_with("  \x1b[38;2;255;0;0m"), "{:?}", rows[0]);
        let want_url = format!("file://{}", path.display());
        assert!(rows[1].contains(&want_url), "{:?}", rows[1]);
        assert!(rows[1].contains("\x1b]8;;"), "OSC 8 missing: {:?}", rows[1]);
    }

    /// New: `Path`-typed `file_exists` refuses a DIRECTORY, so a directory named like an image
    /// never becomes a `file://` link (Go pinned `fi.Mode().IsRegular()`).
    #[test]
    fn file_exists_rejects_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!super::file_exists(dir.path()));
        let f = dir.path().join("x");
        std::fs::write(&f, b"1").expect("write");
        assert!(super::file_exists(&f));
        assert!(!super::file_exists(Path::new("/definitely/not/here")));
    }

    /// New (no Go equivalent — Go measured the terminal inside the function): the block wraps
    /// to the width it is GIVEN, and a CJK run is never split mid-rune.
    #[test]
    fn user_block_wraps_cjk_at_the_given_width() {
        let rows = print_user_block("生成一张图片的描述", 12);
        assert!(rows.len() > 1, "a 18-column run must wrap at 12: {rows:?}");
        for row in &rows {
            assert_eq!(str_width(&strip_sgr(row)), 12);
        }
    }
}
