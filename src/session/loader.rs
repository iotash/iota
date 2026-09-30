//! Reading a bundle back (chat/session.go:762-901, chat/compact.go:19-28).
//!
//! `messages.jsonl` is an event store: [`scan_records`] streams it line by line with `bufio.Scanner`'s
//! tolerances, [`record_to_message`] turns one record into a replayable message, and [`load_log`] derives
//! the view — the last system record first, then the tail the last compaction marker retained, with the
//! summary preamble woven into it.

use std::{
    io::BufRead,
    path::{Path, PathBuf},
};

use crate::provider::ProviderKind;
use crate::provider::model::{AssistantBody, Attachment, Body, Message, Role, ToolBody, ToolCall};
use crate::provider::usage::Usage;

use crate::session::error::SessionError;
use crate::session::meta::SessionMeta;
use crate::session::rawcodec::blob_to_raw;
use crate::session::record::{
    ATTACHMENTS_DIR, DATA_REF_PREFIX, LOG_FILE, ROLE_COMPACTION, SessionRecord,
};

/// Prefix of the woven compaction summary (chat/compact.go:19).
pub(crate) const SUMMARY_PREFIX: &str = "[Earlier conversation summary]\n";
/// Separator between the summary and the message it is prepended to (chat/compact.go:20).
pub(crate) const SUMMARY_SEPARATOR: &str = "\n\n———\n\n";
/// The scanner's maximum line length (chat/session.go:827): 32 MiB. A line that REACHES it aborts the
/// scan with `read session log: …` (D-56).
pub const MAX_LOG_LINE: usize = 32 * 1024 * 1024;

/// The content of a tool result [`repair_tail`] synthesises for a call the log never answered.
pub const INTERRUPTED_RESULT: &str = "interrupted: no result was recorded";

/// The initial read buffer, matching `bufio.NewScanner`'s 64 KiB start (chat/session.go:821).
const SCAN_CHUNK: usize = 64 * 1024;

/// `summaryPrefix + strings.TrimSpace(summary) + summarySeparator` (chat/compact.go:25-27).
pub fn summary_preamble(summary: &str) -> String {
    format!("{SUMMARY_PREFIX}{}{SUMMARY_SEPARATOR}", summary.trim())
}

/// A fully loaded session (chat/session.go:145-152). `messages` is the DERIVED view (post-compaction
/// weave); `usage` is summed over the WHOLE log, compacted-away rounds and markers included — they were
/// paid for.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// The bundle's metadata.
    pub meta: SessionMeta,
    /// The replay view.
    pub messages: Vec<Message>,
    /// Cumulative token cost of the whole log.
    pub usage: Usage,
    /// How many unanswered tool calls [`repair_tail`] answered (and appended) on the way in.
    pub repaired: usize,
}

impl Session {
    /// The transcript line announcing a [`repair_tail`], or `None` when nothing was repaired.
    pub fn repair_notice(&self) -> Option<String> {
        (self.repaired > 0).then(|| {
            format!(
                "Recovered {} tool call(s) with no recorded result; they are marked as interrupted.",
                self.repaired
            )
        })
    }
}

/// What [`load_log`] produces (chat/session.go:845-901).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedLog {
    /// The derived replay view.
    pub view: Vec<Message>,
    /// TOTAL conversation messages on disk (non-system, non-marker) — the writer's marker index, NOT the
    /// view length.
    pub conv_count: usize,
    /// Cumulative token cost of the whole log.
    pub usage: Usage,
}

/// `scanRecords` (chat/session.go:813-837): line-oriented over `<dir>/messages.jsonl`, in append order.
///
/// Blank lines are skipped; a line that fails to parse is skipped SILENTLY (a crash mid-write leaves a
/// truncated trailing line); a line whose length REACHES [`MAX_LOG_LINE`], or an I/O fault, aborts with
/// [`SessionError::ReadLog`]. The cap is enforced WHILE reading, in bounded chunks — an unbounded
/// `read_until` would allocate the whole hostile line before the cap could fire (D-56).
pub fn scan_records(dir: &Path, f: &mut dyn FnMut(SessionRecord)) -> Result<(), SessionError> {
    let file = std::fs::File::open(dir.join(LOG_FILE))?;
    let mut reader = std::io::BufReader::with_capacity(SCAN_CHUNK, file);
    let mut line: Vec<u8> = Vec::new();
    loop {
        let (consumed, complete) = {
            let buf = reader.fill_buf().map_err(SessionError::ReadLog)?;
            if buf.is_empty() {
                break;
            }
            if let Some(i) = buf.iter().position(|b| *b == b'\n') {
                push_capped(&mut line, &buf[..i])?;
                (i + 1, true)
            } else {
                push_capped(&mut line, buf)?;
                (buf.len(), false)
            }
        };
        reader.consume(consumed);
        if complete {
            emit(&line, f);
            line.clear();
        }
    }
    // A final line without its newline is still a record (Go's scanner returns the remainder at EOF).
    if !line.is_empty() {
        emit(&line, f);
    }
    Ok(())
}

/// Appends `chunk` to the pending line, refusing to grow it to [`MAX_LOG_LINE`] — the cap fires BEFORE
/// the allocation, which is what makes a hostile line harmless.
fn push_capped(line: &mut Vec<u8>, chunk: &[u8]) -> Result<(), SessionError> {
    if line.len() + chunk.len() >= MAX_LOG_LINE {
        return Err(SessionError::ReadLog(std::io::Error::other(format!(
            "log line exceeds the {MAX_LOG_LINE}-byte limit"
        ))));
    }
    line.extend_from_slice(chunk);
    Ok(())
}

/// Parses one raw line and hands the record on; blank and unparsable lines are skipped.
fn emit(line: &[u8], f: &mut dyn FnMut(SessionRecord)) {
    if String::from_utf8_lossy(line).trim().is_empty() {
        return;
    }
    if let Ok(rec) = serde_json::from_slice::<SessionRecord>(line) {
        f(rec);
    }
}

/// The bundle-local path of an attachment reference (`readAttachmentRef`, chat/session.go:762-768).
/// `None` when the ref carries no `sha256:` prefix or an empty digest — Go builds the error text
/// `bad attachment ref: %s` here and then DISCARDS it, so only the behaviour is ported.
fn attachment_path(dir: &Path, data_ref: &str) -> Option<PathBuf> {
    let hex = data_ref.strip_prefix(DATA_REF_PREFIX)?;
    if hex.is_empty() {
        return None;
    }
    // Hardening beyond Go, which joins the stored value unchecked: a content-addressed name is 64 hex
    // digits, so anything that could walk out of the store is not a reference this store wrote.
    if hex.contains(['/', '\\']) || hex.contains("..") {
        return None;
    }
    Some(dir.join(ATTACHMENTS_DIR).join(hex))
}

/// `fromSessionMessage` (chat/session.go:769-808).
///
/// An unreadable or missing attachment is skipped and the message kept; a `data_ref` without the
/// `sha256:` prefix is likewise skipped. `raw` restores only under an exactly-matching tag.
///
/// `role == "compaction"` yields `None`: [`load_log`] CONSUMES marker records before conversion (Go's
/// `loadLog` intercepts them ahead of `fromSessionMessage`), so this arm is unreachable from `load_log`;
/// it exists so no direct caller can leak a marker into a prompt. Any OTHER role that is not a known
/// [`Role`] also yields `None` → the record is SKIPPED by `load_log` (D-47).
pub fn record_to_message(rec: &SessionRecord, dir: &Path, kind: ProviderKind) -> Option<Message> {
    let body = match rec.role.as_str() {
        "system" => Body::System,
        "user" => {
            if rec.notice {
                Body::Notice
            } else {
                Body::User
            }
        }
        "assistant" => Body::Assistant(AssistantBody {
            reasoning: rec.reasoning.clone(),
            tool_calls: rec
                .tool_calls
                .iter()
                .map(|call| ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            raw_content: rec.raw.as_ref().and_then(|raw| blob_to_raw(kind, raw)),
            usage: rec.usage.map(Into::into),
            interrupted: rec.interrupted,
        }),
        "tool" => Body::Tool(ToolBody {
            call_id: rec.tool_call_id.clone(),
            call_name: rec.tool_call_name.clone(),
            is_error: rec.is_error,
        }),
        _ => return None,
    };
    let mut msg = Message {
        content: rec.content.clone(),
        body,
        ..Message::default()
    };
    for att in &rec.attachments {
        let Some(path) = attachment_path(dir, &att.data_ref) else {
            continue;
        };
        let Ok(data) = std::fs::read(path) else {
            continue; // skip an unreadable attachment, keep the rest of the message
        };
        msg.attachments.push(Attachment {
            filename: att.filename.clone(),
            mime_type: att.mime_type.clone(),
            data,
        });
    }
    Some(msg)
}

/// `LoadFullHistory` (chat/session.go:920-941): every conversation record in `messages.jsonl`, in
/// APPEND order, with compaction markers skipped entirely.
///
/// Unlike [`load_log`] there is no view weaving, so `/compact` never hides an archived round from
/// an export — the archive is lossless. Attachments and raw content are restored exactly as
/// [`load_log`] restores them, because both go through [`record_to_message`] (which already
/// answers `None` for a marker; the explicit skip mirrors Go's own guard and keeps the intent
/// local to the reader).
pub fn load_full_history(dir: &Path, kind: ProviderKind) -> Result<Vec<Message>, SessionError> {
    let mut msgs: Vec<Message> = Vec::new();
    scan_records(dir, &mut |rec| {
        if rec.role == ROLE_COMPACTION {
            return;
        }
        if let Some(msg) = record_to_message(&rec, dir, kind) {
            msgs.push(msg);
        }
    })?;
    Ok(msgs)
}

/// Answers the tool calls the view's tail left open (docs/design/bot-mode.md §2.7): a batch cut short by a
/// crash or power loss, or a corrupt line [`scan_records`] skipped, can leave the last assistant message
/// with `tool_calls` that no tool record follows. Every dialect rejects such a history outright (Anthropic
/// with a 400 that is never retried), so the session would load and then fail every request.
///
/// Each unanswered call gets an `is_error` result with [`INTERRUPTED_RESULT`], appended to `view` in call
/// order; the return value is how many — the caller appends exactly `view[len - n..]` to the log, which
/// puts them where they belong on the next load too. Only the TAIL is inspected: an open call earlier in
/// the view cannot be repaired by appending.
pub fn repair_tail(view: &mut Vec<Message>) -> usize {
    let Some(last) = view.iter().rposition(|m| m.role() != Role::Tool) else {
        return 0;
    };
    let answered: Vec<&str> = view[last + 1..].iter().map(Message::tool_call_id).collect();
    let synthesized: Vec<Message> = view[last]
        .tool_calls()
        .iter()
        .filter(|call| !answered.contains(&call.id.as_str()))
        .map(|call| Message::tool_result(call, INTERRUPTED_RESULT, true))
        .collect();
    let n = synthesized.len();
    view.extend(synthesized);
    n
}

/// `loadLog` (chat/session.go:845-901).
///
/// Usage is summed over EVERY record, markers included. The LAST system record with content wins and is
/// placed FIRST in the view (a content-less one is a persisted defer mount — see the closure). The LAST
/// compaction marker wins, with `compacted_through` clamped to `[0, conversation length]`; when anything
/// is retained the FIRST retained message's content gets [`summary_preamble`] PREPENDED, otherwise a
/// synthetic `{role: User, content: preamble}` is appended.
pub fn load_log(dir: &Path, kind: ProviderKind) -> Result<LoadedLog, SessionError> {
    let mut system: Option<Message> = None;
    let mut conv: Vec<Message> = Vec::new();
    let mut summary = String::new();
    let mut through: i64 = 0;
    let mut has_summary = false;
    let mut usage = Usage::default();

    scan_records(dir, &mut |rec| {
        if let Some(u) = rec.usage {
            usage += u.into();
        }
        if rec.role == ROLE_COMPACTION {
            has_summary = true;
            summary = rec.content;
            through = rec.compacted_through;
            return;
        }
        let Some(msg) = record_to_message(&rec, dir, kind) else {
            return; // unknown role — skipped like a corrupt line (D-47)
        };
        if msg.role() == Role::System {
            // A frozen-mode defer mount (`Message::system_tools`) is runtime state (tool-defer.md), but it
            // rides in the history the REPL appends and lands here as a system record with NO content. It
            // must not win — it would replace the system prompt with an empty one (bot-mode.md §2.7).
            // No real system prompt is ever empty: an empty one is never put in the history at all.
            if !msg.content.is_empty() {
                system = Some(msg);
            }
            return;
        }
        conv.push(msg);
    })?;

    let conv_count = conv.len();
    let mut view: Vec<Message> = Vec::new();
    if let Some(sys) = system {
        view.push(sys);
    }
    // A hand-edited negative `compacted_through` clamps to 0, as in Go.
    let start = if has_summary {
        usize::try_from(through).unwrap_or(0).min(conv.len())
    } else {
        0
    };
    let mut retained = conv.split_off(start);
    if has_summary {
        if retained.is_empty() {
            view.push(Message::user(summary_preamble(&summary)));
        } else {
            let mut first = retained.remove(0);
            first.content = summary_preamble(&summary) + &first.content;
            view.push(first);
            view.append(&mut retained);
        }
    } else {
        view.append(&mut retained);
    }
    Ok(LoadedLog {
        view,
        conv_count,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        INTERRUPTED_RESULT, SUMMARY_PREFIX, SUMMARY_SEPARATOR, repair_tail, summary_preamble,
    };
    use crate::provider::model::{Message, ToolCall};

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: "t".to_owned(),
            ..ToolCall::default()
        }
    }

    /// Every call the tail left unanswered gets one interrupted error result, in call order.
    #[test]
    fn repair_tail_answers_open_calls_in_order() {
        let calls = vec![call("a"), call("b"), call("c")];
        let mut view = vec![
            Message::user("q"),
            Message::assistant("").with_tool_calls(calls.clone()),
            Message::tool_result(&calls[1], "ok", false),
        ];
        assert_eq!(repair_tail(&mut view), 2);
        assert_eq!(view.len(), 5);
        for (msg, id) in view[3..].iter().zip(["a", "c"]) {
            assert_eq!(msg.tool_call_id(), id);
            assert_eq!(msg.content, INTERRUPTED_RESULT);
            assert!(msg.is_error());
        }
    }

    /// A closed tail, a tail without calls, and an empty view are left alone; an open call EARLIER in
    /// the view is not the tail's business.
    #[test]
    fn repair_tail_leaves_a_closed_view_alone() {
        let c = call("a");
        let closed = vec![
            Message::user("q"),
            Message::assistant("").with_tool_calls(vec![c.clone()]),
            Message::tool_result(&c, "ok", false),
            Message::assistant("done"),
        ];
        let mut view = closed.clone();
        assert_eq!(repair_tail(&mut view), 0);
        assert_eq!(view, closed);

        let mut answered = closed[..3].to_vec();
        assert_eq!(repair_tail(&mut answered), 0);

        let mut earlier = vec![
            Message::assistant("").with_tool_calls(vec![call("x")]),
            Message::user("next"),
        ];
        assert_eq!(repair_tail(&mut earlier), 0);

        assert_eq!(repair_tail(&mut Vec::new()), 0);
    }

    /// The preamble is the Go constants around the TRIMMED summary (chat/compact.go:25-27).
    #[test]
    fn preamble_matches_go() {
        assert_eq!(SUMMARY_PREFIX, "[Earlier conversation summary]\n");
        assert_eq!(SUMMARY_SEPARATOR, "\n\n———\n\n");
        assert_eq!(
            summary_preamble("  SUMMARY \n"),
            "[Earlier conversation summary]\nSUMMARY\n\n———\n\n"
        );
    }
}
