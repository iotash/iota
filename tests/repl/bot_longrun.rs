//! The long run (docs/design/bot-mode.md §5.2): a bot with a 32k window — the smallest a bot runs in (§4.1) —
//! driven through 2000 turns by
//! [`GrowingProvider`] — whose usage is the request measured and which refuses a request over the window — with
//! the process dropped and resumed at seeded random points. It checks the mechanism, not a model:
//!
//! 0. every user turn `1..=2000` is in the log exactly once, with its final reply after it;
//! 1. the log only grows: every append leaves the bytes before it as they were;
//! 2. every request fits the window (measured on the request, never estimated — and a request over it would
//!    have been refused, so a pass is a real bound);
//! 3. the compactions are about as many as the growth divided by the room each one frees;
//! 4. every compaction is preceded by exactly one flush notice, or its marker says `flush_skipped`;
//! 5. `MEMORY.md` stays within its 8 KiB cap;
//! 6. a restart changes nothing: at every drop the same process is ALSO run on without the restart (a copy of
//!    the disk taken at the drop point is what the restart resumes). 6a: the view the restart loads is, byte for
//!    byte, the one the process held. 6b: the two send the model the same calls and the same history, up to and
//!    including the next user turn — everything but the memory block, which a restart re-reads by design (§3.4:
//!    startup is a refresh moment) while the running process keeps its copy until the next compaction. The
//!    model may answer the re-read block differently, and that alone is listed, not failed: once what the
//!    fake produced after it was shown the block (its new tool calls, their results, the `memory:` notices of
//!    their writes) is left out — and, in a summary request, the memory section and the count of lines its
//!    flush saved — the two sides must make the same calls with the same histories, in order
//!    ([`refresh_explains`]). Anything else — a call of another kind, an older message changed, a history cut
//!    or reordered — is the process's, and fails;
//! 7. the startup load time as the log grows — printed, and held to the §2.6 threshold (2 s) as a loose bound.
//!
//! Drops happen at the idle prompt, between turns — where a bot sits nearly all its life — including the
//! moment a flush notice has been queued and not yet run. The harness clock is fixed, so no run crosses a
//! midnight that one side of a restart sees and the other does not. Every verdict is printed (`--nocapture`);
//! each test requires the ones it names. One scenario is kept, `#[ignore]`d, as the evidence for the minimum
//! window: [`a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window`]. Most of the run's time is
//! the durability path itself: every persisted batch is a `sync_all` (a full flush on macOS).
//!
//! What it does NOT cover (review R7), so it is no substitute for the targeted tests: every restart is at the
//! idle prompt, never inside a write (the failed-batch, cut-back and interrupt paths have tests of their own,
//! `tests/repl/bot_flush.rs` and `session::writer`); one fixed seed; only the memory toolset enabled, so no
//! other tool's output; no agent overlay (AGENTS.md, skills) in the request; and a `bytes / 4` measure, not a
//! real provider's tokenizer. Its load times are for a log of a few MiB and say nothing about 256 MiB.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iota::host::Presenter;
use iota::llm::reqlog::RequestLog;
use iota::provider::ProviderKind;
use iota::provider::model::{Message, Role};
use iota::repl::{McpHooks, RunParams, SessionCtx};
use iota::session::{BotOpen, NewSession, SessionStore};
use iota::testing::{CallKind, GrowingCall, GrowingProvider, RecordingHost, Reply, ScriptedUi};
use iota::tool::Dispatcher;
use iota::ui::facade::{Input, Ui};
use tokio_util::sync::CancellationToken;

/// The smallest window a bot runs in (`BOT_MIN_WINDOW`, bot-mode.md §4.1): its reserve is capped at half of it,
/// so it compacts at 16k.
const WINDOW: u64 = 32_000;
/// "8k" — below the minimum; kept only for the reproduction that shows why there is one.
const SMALL_WINDOW: u64 = 8_192;
/// The fixed date the harness clock reads.
const TODAY: &str = "2026-10-01";
const TURNS: u64 = 2_000;
/// Fixed, so a failure reproduces: it picks the reply sizes and the drop points.
const SEED: u64 = 0x5eed_2026_1001;
const DROPS: usize = 24;
/// `agents::memory::MEMORY_CAP`.
const MEMORY_CAP: usize = 8 * 1024;
const FLUSH_MARK: &str = "The conversation is about to be compacted";
/// The §2.6 threshold past which the log would have to roll (L4).
const LOAD_BOUND: Duration = Duration::from_secs(2);

/// Every `remember` of a user turn: one in 25.
fn remember_turns() -> impl Iterator<Item = u64> {
    (1..=TURNS).filter(|n| n % 25 == 7)
}

/// `n` distinct turns in `1..TURNS`, picked by a splitmix64 stream from `seed`.
fn drop_points(seed: u64, n: usize) -> BTreeSet<u64> {
    let mut x = seed;
    let mut out = BTreeSet::new();
    while out.len() < n {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        out.insert(1 + z % (TURNS - 1));
    }
    out
}

fn feed(s: String) -> Reply {
    Reply::Feed(Input {
        display: s.clone(),
        text: s,
        ..Input::default()
    })
}

/// Copies directory `from` to `to` (which must not exist).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("readdir") {
        let e = e.expect("entry");
        let dst = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).expect("copy");
        }
    }
}

/// A bot's threshold for `window`: `min(max(32k, 25%), window / 2)` reserved.
fn threshold(window: u64) -> u64 {
    window - 32_000u64.max(window / 4).min(window / 2)
}

/// `MEMORY.md`'s body — what the cap measures (the frontmatter is not counted, §3.5).
fn memory_body(text: &str) -> &str {
    text.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(text, |(_, body)| body)
}

/// What the disk looks like while the run is under way, checked at every provider call.
#[derive(Default)]
struct Watch {
    /// The log as last seen: the next look must start with these bytes.
    log: Vec<u8>,
    /// Growth checks passed, and any violation.
    appends: usize,
    violations: Vec<String>,
    /// The largest `MEMORY.md` body and whole file seen.
    memory_body_max: usize,
    memory_file_max: usize,
}

impl Watch {
    fn look(&mut self, log: &Path, memory: &Path, at: &str) {
        let now = std::fs::read(log).unwrap_or_default();
        if now.len() < self.log.len() || now[..self.log.len()] != self.log[..] {
            let same = now
                .iter()
                .zip(&self.log)
                .take_while(|(a, b)| a == b)
                .count();
            self.violations.push(format!(
                "{at}: the log went from {} to {} bytes and differs from byte {same}",
                self.log.len(),
                now.len()
            ));
        } else if now.len() > self.log.len() {
            self.appends += 1;
        }
        self.log = now;
        if let Ok(text) = std::fs::read_to_string(memory) {
            self.memory_body_max = self.memory_body_max.max(memory_body(&text).len());
            self.memory_file_max = self.memory_file_max.max(text.len());
        }
    }
}

/// The bot `coder` over a temp store.
struct Bot {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    store: SessionStore,
    bots: PathBuf,
}

impl Bot {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("home");
        let bots = root.join("bots");
        let store = SessionStore::new(root.join("sessions")).with_bots(&bots);
        Self {
            _tmp: tmp,
            root,
            store,
            bots,
        }
    }

    fn memory(&self) -> iota::agents::memory::BotMemory {
        iota::agents::memory::BotMemory::new("coder", self.bots.join("coder"))
    }

    /// One life of the process: opens the bot as `iota run coder` does (resumed, or created under its
    /// pointer), runs the loop over `script`, returns the bundle and how long the open took.
    /// `on_open` is told the bundle before the loop starts.
    async fn life(
        &self,
        window: u64,
        provider: GrowingProvider,
        script: Vec<Reply>,
        on_open: impl FnOnce(&Path),
    ) -> (PathBuf, Duration) {
        let t0 = Instant::now();
        let opened = self
            .store
            .open_bot(
                &self.bots.join("coder"),
                NewSession::new(ProviderKind::OpenAi, "gpt-test"),
                ProviderKind::OpenAi,
            )
            .expect("open the bot");
        let load = t0.elapsed();
        // The wiring's own resume (`cmd::interactive::bot`): the view as loaded; the config's system prompt is
        // empty, which the view's head already agrees with; no resume notice (the gap is under an hour).
        let (writer, history) = match opened {
            BotOpen::Fresh(writer) => (writer, Vec::new()),
            BotOpen::Resumed(writer, session) => (writer, session.messages),
        };
        let dir = writer.dir().to_path_buf();
        on_open(&dir);
        let memory = self.memory();
        let env = iota::tool::ToolEnv {
            memory: Some(memory.clone()),
            ..iota::tool::ToolEnv::default()
        };
        let mut registry = iota::tool::Registry::default();
        registry.enable_set(&env, iota::tool::sets::MEMORY_SET, &mut |w| {
            panic!("unexpected warning {w}")
        });
        let dispatch = iota::tool::merge(vec![Arc::new(registry) as Arc<dyn Dispatcher>]);
        let params = RunParams {
            ui: ScriptedUi::new(script) as Arc<dyn Ui>,
            provider: Box::new(provider),
            title_provider: None,
            system: String::new(),
            harness: iota::agents::harness::HarnessInputs {
                clock: Arc::new(|| TODAY.to_owned()),
                ..iota::agents::harness::HarnessInputs::default()
            },
            imported_history: history,
            dispatch,
            jobs: iota::shell::jobs::Jobs::new(Path::new("")),
            mcp: McpHooks::default(),
            session: SessionCtx {
                writer: Some(writer),
                store: self.store.clone(),
                new_session: None,
                scope: None,
                bot: true,
                notices: Vec::new(),
                recorded_notices: Vec::new(),
                memory: Some(memory),
            },
            params: iota::session::LayeredParams {
                context_window: iota::session::Param::config(window),
                ..iota::session::LayeredParams::default()
            },
            layers: iota::cmd::ParamLayers::default(),
            catalog: iota::repl::ModelCatalog::default(),
            agent: iota::headless::AgentOptions::default(),
            dark_background: true,
            root_cancel: CancellationToken::new(),
            reqlog: Arc::new(RequestLog::new()),
            pres: Arc::new(Presenter::with_hosts(
                vec![Box::new(RecordingHost::new("recorder"))],
                true,
            )),
        };
        iota::repl::run(params).await.expect("exit");
        (dir, load)
    }
}

/// The history a call carries, without the system message the overlay (the memory block) composes: the view.
fn view_of(call: &GrowingCall) -> &[Message] {
    match call.messages.first() {
        Some(m) if m.role() == Role::System => &call.messages[1..],
        _ => &call.messages,
    }
}

/// `calls` up to and including the first round of user turn `turn` (all of them when it never came).
fn up_to_turn(calls: &[GrowingCall], turn: u64) -> &[GrowingCall] {
    let end = calls
        .iter()
        .position(|c| c.kind == CallKind::Turn && c.turn == turn)
        .map_or(calls.len(), |i| i + 1);
    &calls[..end]
}

/// How one restart compared with running on.
struct DropReport {
    turn: u64,
    /// A flush notice was queued when the process went down.
    flush_queued: bool,
    /// 6a: the view the restart loaded against the one the process held; `None` when a compaction came first on
    /// either side, so neither call shows the view as it was at the drop.
    loaded: Option<Result<(), String>>,
    /// 6b: where what the two sent, up to the next user turn, first differs — the calls or the history, not the
    /// memory block; `None` when it was the same.
    diff: Option<(usize, String)>,
    /// The first call whose memory block differs (the restart's startup re-read, §3.4), if any.
    memory_at: Option<usize>,
    /// When there is a `diff` and a `memory_at`: whether the model's answer to the re-read block accounts for
    /// ALL of the difference ([`refresh_explains`]), or what it does not account for.
    refresh: Option<Result<(), String>>,
}

impl DropReport {
    /// The only difference is the model answering the re-read memory block (the §3.4 refresh) — checked
    /// message by message, not inferred from where the first difference sits.
    fn follows_the_refresh(&self) -> bool {
        matches!(self.refresh, Some(Ok(())))
    }
}

/// The ids of the tool calls in `view`.
fn call_ids(view: &[Message]) -> BTreeSet<&str> {
    view.iter()
        .flat_map(Message::tool_calls)
        .map(|c| c.id.as_str())
        .collect()
}

/// A write one of the model's new calls made to the memory: where its result says it saved, and the line the
/// call passed (`text`, `new` or `old` — the notice of the write shows it).
struct Write {
    place: String,
    line: String,
}

impl Write {
    /// Whether `notice` is this write's: `memory: <place> <verb>: <line …>`.
    fn made(&self, notice: &str) -> bool {
        notice
            .strip_prefix("memory: ")
            .and_then(|n| n.strip_prefix(self.place.as_str()))
            .and_then(|n| n.strip_prefix(' '))
            .is_some_and(|n| n.contains(self.line.as_str()))
    }
}

/// The writes that the calls in `view` whose ids are `fresh` made: each successful result (`saved to <place>
/// (…)`) with its call.
fn new_writes(view: &[Message], fresh: &dyn Fn(&str) -> bool) -> Vec<Write> {
    view.iter()
        .filter(|r| r.role() == Role::Tool && !r.is_error() && fresh(r.tool_call_id()))
        .filter_map(|r| {
            let call = view
                .iter()
                .flat_map(Message::tool_calls)
                .find(|c| c.id == r.tool_call_id())?;
            let (place, _) = r
                .content
                .lines()
                .next()?
                .strip_prefix("saved to ")?
                .rsplit_once(" (")?;
            let line = ["text", "new", "old"]
                .iter()
                .find_map(|k| call.arguments.get(*k)?.as_str())?;
            Some(Write {
                place: place.to_owned(),
                line: line.to_owned(),
            })
        })
        .collect()
}

/// The header of a bot's summary request's long-term memory section (`repl::commands::compact::summarize`).
const MEMORY_SECTION: &str =
    "\n\n--- LONG-TERM MEMORY (already saved separately; do not repeat these) ---\n";

/// `prompt` with the body of its long-term memory section — `MEMORY.md` as the summary pass is shown it,
/// which a restart re-reads (§3.4) — replaced by a placeholder; the rest, the rendered history included, is
/// kept byte for byte.
fn without_memory_section(prompt: &str) -> String {
    let Some(at) = prompt.find(MEMORY_SECTION) else {
        return prompt.to_owned();
    };
    let body = at + MEMORY_SECTION.len();
    let end = [
        "\n--- CONVERSATION START ---\n",
        "\n--- NEW CONVERSATION START ---\n",
    ]
    .iter()
    .filter_map(|mark| prompt[body..].find(mark))
    .min()
    .map_or(prompt.len(), |i| body + i);
    format!("{}<memory>{}", &prompt[..body], &prompt[end..])
}

/// The writes the flush in `view` made: its successful results (`saved to …`) after the flush notice.
fn flush_writes(view: &[Message]) -> usize {
    view.iter()
        .rposition(|m| m.is_notice() && m.content.starts_with(FLUSH_MARK))
        .map_or(0, |at| {
            view[at + 1..]
                .iter()
                .filter(|m| {
                    m.role() == Role::Tool && !m.is_error() && m.content.starts_with("saved to ")
                })
                .count()
        })
}

/// A summary request as the process is accountable for it: [`without_memory_section`], and the number of
/// lines the flush saved — which the model's answer decides — replaced by a placeholder only when it is the
/// number of writes that side's flush made (`writes`); a wrong count stays and differs.
fn summary_request(prompt: &str, writes: usize) -> String {
    let saved = format!(
        " The memory flush just before this compaction saved {writes} line{}.",
        if writes == 1 { "" } else { "s" }
    );
    without_memory_section(prompt).replacen(
        &saved,
        " The memory flush just before this compaction saved <its writes>.",
        1,
    )
}

/// Whether the model's answer to the re-read memory block — first shown in call `m` — explains every
/// difference between the two sides up to the next user turn. The model decides what it calls and how many
/// rounds it takes (the follow-ups); the process decides everything else. So:
///
/// - nothing differs up to call `m`;
/// - with the messages the model's new answer produced left out of every history — an assistant message whose
///   tool calls are all new since call `m`, those calls' results, and the `memory:` notices of the writes they
///   made — the two sides make the same calls with the same histories, in order. A `memory:` notice is left
///   out only when it is not already in call `m`'s history AND a write of a new call in the same history
///   accounts for it (its result saved to the place the notice names, and the notice shows the line the call
///   passed), one notice per write: a notice no new write made is the process's. A reply the model gave after
///   call `m` is compared without its usage: that measures its request, which the new messages made longer. A
///   follow-up round that adds nothing else is the model's own and folds into the call before it, so a side may
///   take more of them. Every other message — an older one, a plain reply, a user message, a summary — must be
///   equal and in place, so a history cut, reordered or rewritten fails even after a legitimate refresh;
/// - a summary pass is compared on its whole request — the older history rendered into one text, in order —
///   less its long-term memory section, which is the re-read memory itself, and less the count of lines the
///   flush saved when that count is the side's own flush's writes ([`summary_request`]). What it summarizes
///   ends before the last user turn, so the flush exchange the model answered differently is not in it.
fn refresh_explains(
    turn: u64,
    reference: &[GrowingCall],
    restarted: &[GrowingCall],
    m: usize,
) -> Result<(), String> {
    let (a, b) = (up_to_turn(reference, turn), up_to_turn(restarted, turn));
    if a.iter()
        .zip(b)
        .take(m + 1)
        .any(|(x, y)| view_of(x) != view_of(y) || x.kind != y.kind)
    {
        return Err(format!("a difference at or before call {m}"));
    }
    let shown = view_of(&a[m]);
    let before = call_ids(shown);
    let fresh = |id: &str| !before.contains(id);
    let unmeasured = |msg: &Message| {
        if msg.role() == Role::Assistant && !shown.contains(msg) {
            msg.clone().with_usage(None)
        } else {
            msg.clone()
        }
    };
    // What the process put in `view`: everything but the new calls, their results, and the notices of the
    // writes they made.
    let of_the_process = |view: &[Message]| -> Vec<Message> {
        let mut writes = new_writes(view, &fresh);
        let mut out = Vec::new();
        for msg in view {
            let calls = msg.tool_calls();
            let keep = match msg.role() {
                Role::Assistant => calls.is_empty() || !calls.iter().all(|c| fresh(&c.id)),
                Role::Tool => !fresh(msg.tool_call_id()),
                _ if msg.is_notice() && !shown.contains(msg) => {
                    match writes.iter().position(|w| w.made(&msg.content)) {
                        Some(i) => {
                            writes.remove(i);
                            false
                        }
                        None => true,
                    }
                }
                _ => true,
            };
            if keep {
                out.push(unmeasured(msg));
            }
        }
        out
    };
    let process = |cs: &[GrowingCall]| -> Vec<(CallKind, Vec<Message>)> {
        let mut out: Vec<(CallKind, Vec<Message>)> = Vec::new();
        for (i, c) in cs.iter().enumerate() {
            let view: Vec<Message> = if c.kind == CallKind::Summary {
                // The flush it follows is read off the call before it, which carries every round's result.
                let writes = i
                    .checked_sub(1)
                    .map_or(0, |p| flush_writes(view_of(&cs[p])));
                c.messages
                    .iter()
                    .map(|msg| {
                        let mut msg = msg.clone();
                        msg.content = summary_request(&msg.content, writes);
                        msg
                    })
                    .collect()
            } else {
                of_the_process(view_of(c))
            };
            if c.kind == CallKind::Followup && out.last().is_some_and(|(_, v)| *v == view) {
                continue;
            }
            out.push((c.kind, view));
        }
        out
    };
    let (pa, pb) = (process(a), process(b));
    let kinds = |p: &[(CallKind, Vec<Message>)]| p.iter().map(|c| c.0).collect::<Vec<_>>();
    if kinds(&pa) != kinds(&pb) {
        return Err(format!(
            "the process made different calls: {:?} / {:?}",
            kinds(&pa),
            kinds(&pb)
        ));
    }
    for (i, ((kind, x), (_, y))) in pa.iter().zip(&pb).enumerate() {
        if x != y {
            let at = x
                .iter()
                .zip(y)
                .position(|(p, q)| p != q)
                .unwrap_or(x.len().min(y.len()));
            return Err(format!(
                "the process's call {i} ({kind:?}) differs beyond the model's new calls, at message {at} of {}/{}:\n  without: {:?}\n  with:    {:?}",
                x.len(),
                y.len(),
                x.get(at),
                y.get(at)
            ));
        }
    }
    Ok(())
}

/// The view as it was at the drop, read off the first call that carries it (a user turn or the flush turn, less
/// its new last message) — unless a compaction's summary pass came first.
fn view_at_drop(calls: &[GrowingCall]) -> Option<&[Message]> {
    let first = calls.first()?;
    match first.kind {
        CallKind::Turn | CallKind::Flush => {
            let view = view_of(first);
            Some(&view[..view.len().saturating_sub(1)])
        }
        CallKind::Summary | CallKind::Followup => None,
    }
}

/// 6a for one drop.
fn compare_loaded(
    reference: &[GrowingCall],
    restarted: &[GrowingCall],
) -> Option<Result<(), String>> {
    let (a, b) = (view_at_drop(reference)?, view_at_drop(restarted)?);
    if a == b {
        return Some(Ok(()));
    }
    let at = a
        .iter()
        .zip(b)
        .position(|(p, q)| p != q)
        .unwrap_or(a.len().min(b.len()));
    Some(Err(format!(
        "the views differ at message {at} of {}/{}:\n  held:   {:?}\n  loaded: {:?}",
        a.len(),
        b.len(),
        a.get(at),
        b.get(at)
    )))
}

/// The first call, up to the next user turn, whose memory block (the system message) differs.
fn memory_differs_at(
    turn: u64,
    reference: &[GrowingCall],
    restarted: &[GrowingCall],
) -> Option<usize> {
    up_to_turn(reference, turn)
        .iter()
        .zip(up_to_turn(restarted, turn))
        .position(|(x, y)| x.messages.first() != y.messages.first())
}

/// Where what the restarted run sent first differs from what the run without the restart sent — the kind of a
/// call or its history, not its memory block ([`memory_differs_at`]): the call's index and what differed.
fn compare(
    turn: u64,
    reference: &[GrowingCall],
    restarted: &[GrowingCall],
) -> Option<(usize, String)> {
    let a = up_to_turn(reference, turn);
    let b = up_to_turn(restarted, turn);
    let kinds = |cs: &[GrowingCall]| cs.iter().map(|c| c.kind).collect::<Vec<_>>();
    for i in 0..a.len().max(b.len()) {
        let (Some(x), Some(y)) = (a.get(i), b.get(i)) else {
            return Some((
                i,
                format!(
                    "different calls: without the restart {:?}, with it {:?}",
                    kinds(a),
                    kinds(b)
                ),
            ));
        };
        if x.kind != y.kind {
            return Some((
                i,
                format!(
                    "different calls: without the restart {:?}, with it {:?}",
                    kinds(a),
                    kinds(b)
                ),
            ));
        }
        if view_of(x) != view_of(y) {
            let at = view_of(x)
                .iter()
                .zip(view_of(y))
                .position(|(p, q)| p != q)
                .unwrap_or(view_of(x).len().min(view_of(y).len()));
            return Some((
                i,
                format!(
                    "call {i} ({:?}): the views differ at message {at} of {}/{}:\n  without: {:?}\n  with:    {:?}",
                    x.kind,
                    view_of(x).len(),
                    view_of(y).len(),
                    view_of(x).get(at),
                    view_of(y).get(at)
                ),
            ));
        }
    }
    None
}

/// What the whole run left.
struct Run {
    /// The window the bot ran in.
    window: u64,
    elapsed: Duration,
    calls: Vec<GrowingCall>,
    /// Indices into `calls` of the branches run only as the no-restart reference (discarded afterwards).
    reference: Vec<std::ops::Range<usize>>,
    drops: Vec<DropReport>,
    watch: Watch,
    /// The log's records at the end.
    records: Vec<serde_json::Value>,
    log_bytes: u64,
    /// (turn the process started at, log bytes, open time).
    loads: Vec<(u64, u64, Duration)>,
}

/// Drives the bot through [`TURNS`] turns, dropping it after each turn in `drops`.
async fn drive(window: u64, provider: GrowingProvider, drops: &BTreeSet<u64>) -> Run {
    let started = Instant::now();
    let bot = Bot::new();
    let watch = Arc::new(Mutex::new(Watch::default()));
    // The bundle's log: known once the first life has opened the bot (its id is fixed from then on).
    let log_path: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let memory_path = bot.memory().path().clone();
    {
        let watch = Arc::clone(&watch);
        let log_path = Arc::clone(&log_path);
        let memory_path = memory_path.clone();
        provider.on_call(move |call| {
            if let Some(log) = log_path.lock().unwrap().as_ref() {
                watch.lock().unwrap().look(
                    log,
                    &memory_path,
                    &format!("{:?} of #{}", call.kind, call.turn),
                );
            }
        });
    }
    let mut run = Run {
        window,
        elapsed: Duration::ZERO,
        calls: Vec::new(),
        reference: Vec::new(),
        drops: Vec::new(),
        watch: Watch::default(),
        records: Vec::new(),
        log_bytes: 0,
        loads: Vec::new(),
    };
    let snapshot = bot.root.with_extension("snap");
    let mut next = 1;
    let mut pending: Option<(u64, Vec<GrowingCall>, bool)> = None;
    for end in drops.iter().copied().chain([TURNS]) {
        let mut script: Vec<Reply> = (next..=end).map(|n| feed(provider.prompt(n))).collect();
        let dropped = end < TURNS;
        let paused_at = Arc::new(AtomicUsize::new(usize::MAX));
        if dropped {
            // The drop point: the disk as a kill would leave it, copied aside; then the SAME process runs on
            // into the next turn — the run without the restart.
            let (p, root, snap, at) = (
                provider.clone(),
                bot.root.clone(),
                snapshot.clone(),
                Arc::clone(&paused_at),
            );
            script.push(Reply::Pause(Arc::new(move || {
                copy_tree(&root, &snap);
                at.store(p.call_count(), Ordering::SeqCst);
            })));
            script.push(feed(provider.prompt(end + 1)));
        }
        // Idle at the prompt, then killed.
        script.push(Reply::Pause(Arc::new(|| {})));
        script.push(Reply::Interrupted);

        let first = provider.call_count();
        let log_bytes = log_path
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let slot = Arc::clone(&log_path);
        let (_, load) = bot
            .life(window, provider.clone(), script, move |dir| {
                *slot.lock().unwrap() = Some(dir.join("messages.jsonl"));
            })
            .await;
        run.loads.push((next, log_bytes, load));
        let calls = provider.calls();

        // The restart before this life, against the run that went on without it.
        if let Some((turn, reference, flush_queued)) = pending.take() {
            let restarted = &calls[first..];
            let diff = compare(turn, &reference, restarted);
            let memory_at = memory_differs_at(turn, &reference, restarted);
            let refresh = diff
                .as_ref()
                .and(memory_at)
                .map(|m| refresh_explains(turn, &reference, restarted, m));
            run.drops.push(DropReport {
                turn,
                flush_queued,
                loaded: compare_loaded(&reference, restarted),
                diff,
                memory_at,
                refresh,
            });
        }
        if dropped {
            let at = paused_at.load(Ordering::SeqCst);
            assert_ne!(at, usize::MAX, "the drop point was reached");
            let reference = calls[at..].to_vec();
            // A flush notice was waiting: the run on began with the flush turn.
            let flush_queued = reference.first().is_some_and(|c| c.kind == CallKind::Flush);
            run.reference.push(at..calls.len());
            pending = Some((end + 1, reference, flush_queued));
            // The kill: what the process did after the drop point never happened.
            std::fs::remove_dir_all(&bot.root).expect("rm");
            std::fs::rename(&snapshot, &bot.root).expect("restore");
            let log = log_path.lock().unwrap().clone().expect("a log");
            watch.lock().unwrap().log = std::fs::read(&log).unwrap_or_default();
        }
        next = end + 1;
    }
    // One last look at what the last life left.
    let log = log_path.lock().unwrap().clone().expect("a log");
    watch.lock().unwrap().look(&log, &memory_path, "the end");
    run.elapsed = started.elapsed();
    run.calls = provider.calls();
    run.watch = std::mem::take(&mut *watch.lock().unwrap());
    run.log_bytes = std::fs::metadata(&log).map_or(0, |m| m.len());
    run.records = std::fs::read_to_string(&log)
        .expect("log")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    run
}

/// Invariant 4, read off the log: between one compaction marker and the next, the flush notices that were
/// persisted, and whether the marker says the flush was skipped.
fn flush_accounting(records: &[serde_json::Value]) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    let mut notices = 0;
    for r in records {
        if r["role"] == "compaction" {
            out.push((notices, r["flush_skipped"] == true));
            notices = 0;
        } else if r["notice"] == true
            && r["content"]
                .as_str()
                .is_some_and(|c| c.starts_with(FLUSH_MARK))
        {
            notices += 1;
        }
    }
    out
}

/// Invariant 0, read off the log: every user turn's number in the order it was saved (a flush notice is not a
/// user turn), and the turns whose final reply — an assistant message without tool calls, not interrupted,
/// naming the turn back (`[#n] …`) — was saved inside the turn. A turn ends at the next user message OR the
/// next notice: what the model answers after a flush notice (`[#n] Saved.`, named after the last user turn
/// too) is the flush's reply, never the user's.
fn saved_turns(records: &[serde_json::Value]) -> (Vec<u64>, BTreeSet<u64>) {
    let (mut asked, mut answered, mut current) = (Vec::new(), BTreeSet::new(), None);
    for r in records {
        let content = r["content"].as_str().unwrap_or("");
        if r["role"] == "user" && r["notice"] == true {
            current = None;
        } else if r["role"] == "user" {
            current = content
                .strip_prefix('#')
                .and_then(|t| t.split(' ').next())
                .and_then(|n| n.parse::<u64>().ok());
            asked.extend(current);
        } else if r["role"] == "assistant"
            && r["interrupted"] != true
            && r["tool_calls"].as_array().is_none_or(Vec::is_empty)
            && let Some(n) = current
            && content.starts_with(&format!("[#{n}] "))
        {
            answered.insert(n);
        }
    }
    (asked, answered)
}

/// Invariant 0's verdict over the main line's log.
fn every_turn_answered(records: &[serde_json::Value]) -> Verdict {
    let (asked, answered) = saved_turns(records);
    let every: BTreeSet<u64> = (1..=TURNS).collect();
    let twice = asked.len() - asked.iter().collect::<BTreeSet<_>>().len();
    let missing: Vec<u64> = every.difference(&answered).copied().collect();
    verdict(
        "0 every turn saved with its reply",
        twice == 0 && asked.iter().copied().collect::<BTreeSet<_>>() == every && answered == every,
        format!(
            "{} of {TURNS} turns saved with a final reply, {} user turns in the log, {twice} twice{}",
            answered.len(),
            asked.len(),
            if missing.is_empty() {
                String::new()
            } else {
                format!(
                    "; missing or unanswered: {:?}",
                    &missing[..missing.len().min(20)]
                )
            }
        ),
    )
}

/// One invariant's verdict: what was measured, and whether it held.
struct Verdict {
    name: &'static str,
    held: bool,
    detail: String,
}

fn verdict(name: &'static str, held: bool, detail: String) -> Verdict {
    Verdict { name, held, detail }
}

/// The invariants over a finished run, each with what it measured.
#[allow(clippy::cast_precision_loss)] // token counts far below 2^52
fn verdicts(run: &Run) -> Vec<Verdict> {
    let (window, threshold) = (run.window, threshold(run.window));
    let calls = &run.calls;
    let mainline: Vec<&GrowingCall> = calls
        .iter()
        .enumerate()
        .filter(|(i, _)| !run.reference.iter().any(|r| r.contains(i)))
        .map(|(_, c)| c)
        .collect();
    let records = &run.records;
    let markers = records.iter().filter(|r| r["role"] == "compaction").count();
    let mut out = Vec::new();

    // 0. Every user turn is in the log once, answered: the main line's turns, read off the disk — not the
    // calls, which include the no-restart references, and not a count of lines.
    out.push(every_turn_answered(records));

    // 1. The log only grows: every look at it (one per provider call, one per restart) found the bytes of the
    // last look unchanged at its head.
    out.push(verdict(
        "1 log append-only",
        run.watch.violations.is_empty(),
        if run.watch.violations.is_empty() {
            format!("{} growths seen, none rewrote a byte", run.watch.appends)
        } else {
            format!("{:#?}", run.watch.violations)
        },
    ));

    // 2. Every request fits the window — measured on the request; one over it was refused, not answered.
    let refused: Vec<_> = calls.iter().filter(|c| c.refused).collect();
    let widest = calls
        .iter()
        .filter(|c| !c.refused)
        .map(|c| c.input + c.output)
        .max()
        .unwrap_or(0);
    let mut by_kind = std::collections::BTreeMap::<String, usize>::new();
    for c in &refused {
        *by_kind.entry(format!("{:?}", c.kind)).or_default() += 1;
    }
    out.push(verdict(
        "2 view ≤ window",
        refused.is_empty() && widest <= window,
        format!(
            "{} of {} calls refused as over {window} tokens {by_kind:?} (largest refused: {}); widest answered call {widest}",
            refused.len(),
            calls.len(),
            refused.iter().map(|c| c.input).max().unwrap_or(0),
        ),
    ));

    // 3. The compactions. Each frees the room between the threshold and what it keeps; the conversation then
    // grows past the threshold by part of a turn (the one that crosses it) and the flush exchange before the
    // compaction runs. So their number is the growth over (room + half a turn + a flush exchange), all three
    // measured on the calls. A range around that, not a number: the crossing turn's overshoot is anywhere from
    // nothing to a whole turn (and one turn in twenty is long), a compaction a user's message triggers before
    // sending has no flush exchange, and the memory block in every request changes with MEMORY.md.
    let summaries: Vec<usize> = mainline
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == CallKind::Summary)
        .map(|(i, _)| i)
        .collect();
    let adds =
        |c: &GrowingCall| c.output + c.messages.last().map_or(0, |m| m.content.len() as u64 / 4);
    let prompt_tokens =
        |c: &GrowingCall| c.messages.last().map_or(0, |m| m.content.len() as u64 / 4);
    // What the first call after each compaction was sent, less its own new prompt: the occupancy it left.
    let kept: Vec<u64> = summaries
        .iter()
        .filter_map(|&i| mainline.get(i + 1))
        .map(|c| c.input.saturating_sub(prompt_tokens(c)))
        .collect();
    let mean = |v: &[u64]| v.iter().sum::<u64>() as f64 / v.len().max(1) as f64;
    // Each first round adds its prompt (a follow-up the tool result it answers), each round its answer; a
    // user turn's rounds and a flush's rounds are summed apart.
    let (mut turn_sizes, mut flush_sizes) = (Vec::new(), Vec::new());
    let mut in_flush = false;
    for c in mainline.iter().filter(|c| !c.refused) {
        match c.kind {
            CallKind::Turn => {
                in_flush = false;
                turn_sizes.push(adds(c));
            }
            CallKind::Flush => {
                in_flush = true;
                flush_sizes.push(adds(c));
            }
            CallKind::Followup => {
                let v = if in_flush {
                    &mut flush_sizes
                } else {
                    &mut turn_sizes
                };
                if let Some(last) = v.last_mut() {
                    *last += adds(c);
                }
            }
            CallKind::Summary => {}
        }
    }
    let growth = turn_sizes.iter().chain(&flush_sizes).sum::<u64>() as f64;
    let room = threshold as f64 - mean(&kept);
    let cycle = room + mean(&turn_sizes) / 2.0 + mean(&flush_sizes);
    let expected = growth / cycle;
    let ratio = markers as f64 / expected;
    let turns = TURNS as f64;
    out.push(verdict(
        "3 compactions ≈ expected",
        summaries.len() == markers && (0.85..=1.15).contains(&ratio),
        format!(
            "{markers} markers ({} summary passes), expected ≈ {expected:.0} = growth {growth:.0} / ({threshold} − {:.0} kept + {:.0} half a turn + {:.0} flush); ratio {ratio:.2} (0.85–1.15 accepted), one per {:.1} turns",
            summaries.len(),
            mean(&kept),
            mean(&turn_sizes) / 2.0,
            mean(&flush_sizes),
            turns / markers.max(1) as f64
        ),
    ));

    // 4. Before every compaction: exactly one flush notice in the log, or a marker that says it was skipped.
    let accounting = flush_accounting(records);
    let odd: Vec<_> = accounting
        .iter()
        .enumerate()
        .filter(|(_, (n, skipped))| !matches!((n, skipped), (1, false) | (0, true)))
        .map(|(i, (n, s))| format!("compaction {i}: {n} flush notices, flush_skipped {s}"))
        .collect();
    let skipped = accounting.iter().filter(|(_, s)| *s).count();
    out.push(verdict(
        "4 one flush (or flush_skipped) per compaction",
        odd.is_empty() && accounting.len() == markers,
        format!(
            "{} with a flush, {skipped} flush_skipped{}",
            accounting.len() - skipped,
            if odd.is_empty() {
                String::new()
            } else {
                format!("; off: {}", odd.join("; "))
            }
        ),
    ));

    // 5. MEMORY.md within its cap (the body: the frontmatter is not counted, §3.5).
    out.push(verdict(
        "5 MEMORY.md ≤ 8 KiB",
        run.watch.memory_body_max <= MEMORY_CAP,
        format!(
            "body up to {} bytes, whole file up to {} bytes",
            run.watch.memory_body_max, run.watch.memory_file_max
        ),
    ));

    // 6a. A restart loads exactly the view the process held: byte for byte, at every drop where some call shows
    // it (a compaction's summary pass first, on either side, hides it) — and at least half of them must, or the
    // check would be vacuous.
    let loaded_bad: Vec<String> = run
        .drops
        .iter()
        .filter_map(|d| match &d.loaded {
            Some(Err(e)) => Some(format!("drop before #{}: {e}", d.turn)),
            _ => None,
        })
        .collect();
    let compared = run.drops.iter().filter(|d| d.loaded.is_some()).count();
    out.push(verdict(
        "6a restart loads the view the process held",
        loaded_bad.is_empty() && compared * 2 >= DROPS,
        format!(
            "{compared} of {} drops show the view on both sides, {} differ{}",
            run.drops.len(),
            loaded_bad.len(),
            if loaded_bad.is_empty() {
                String::new()
            } else {
                format!(":\n{}", loaded_bad.join("\n"))
            }
        ),
    ));

    // 6b. A restart sends the model what running on would have: the same calls and the same history, up to and
    // including the next user turn. The memory block is not compared — a restart re-reads it (§3.4) — only
    // counted.
    let describe = |d: &DropReport| {
        d.diff.as_ref().map(|(_, diff)| {
            format!(
                "drop before #{} (flush queued: {}, memory block differs from call {:?}): {diff}{}",
                d.turn,
                d.flush_queued,
                d.memory_at,
                match &d.refresh {
                    Some(Err(why)) => format!("\n  not explained by the re-read block: {why}"),
                    _ => String::new(),
                }
            )
        })
    };
    let broken: Vec<String> = run
        .drops
        .iter()
        .filter(|d| !d.follows_the_refresh())
        .filter_map(describe)
        .collect();
    let refreshed: Vec<String> = run
        .drops
        .iter()
        .filter(|d| d.follows_the_refresh())
        .filter_map(describe)
        .collect();
    out.push(verdict(
        "6b restart sends what running on would have",
        broken.is_empty() && run.drops.len() == DROPS,
        format!(
            "{} restarts compared ({} with a flush queued, {} with the memory block re-read differently), {} differ{}; {} differ only after the model was shown the re-read block{}",
            run.drops.len(),
            run.drops.iter().filter(|d| d.flush_queued).count(),
            run.drops.iter().filter(|d| d.memory_at.is_some()).count(),
            broken.len(),
            if broken.is_empty() {
                String::new()
            } else {
                format!(":\n{}\n", broken.join("\n"))
            },
            refreshed.len(),
            if refreshed.is_empty() {
                String::new()
            } else {
                format!(":\n{}", refreshed.join("\n"))
            }
        ),
    ));

    // 7. The startup load, against the §2.6 threshold (2 s) as a loose bound.
    let slowest = run.loads.iter().map(|l| l.2).max().unwrap_or_default();
    let mut curve = String::new();
    for (turn, bytes, load) in &run.loads {
        let _ = write!(
            curve,
            "\n    #{turn:>4}  {:>6} KiB  {:>6.2} ms",
            bytes / 1024,
            load.as_secs_f64() * 1000.0
        );
    }
    out.push(verdict(
        "7 startup load",
        slowest < LOAD_BOUND,
        format!("slowest {slowest:?} (bound {LOAD_BOUND:?}); turn, log size, open time:{curve}"),
    ));
    out
}

/// Prints the run and every verdict (`--nocapture` shows them); panics naming each invariant in `required`
/// (name prefixes) that failed.
fn report(title: &str, run: &Run, required: &[&str]) {
    let answered: BTreeSet<u64> = run
        .calls
        .iter()
        .filter(|c| c.kind == CallKind::Turn && !c.refused)
        .map(|c| c.turn)
        .collect();
    eprintln!(
        "{title}: {} of {TURNS} turns answered, {} calls ({} in no-restart references), {} drops, log {} KiB, {:?}",
        answered.len(),
        run.calls.len(),
        run.reference
            .iter()
            .map(ExactSizeIterator::len)
            .sum::<usize>(),
        run.drops.len(),
        run.log_bytes / 1024,
        run.elapsed
    );
    let verdicts = verdicts(run);
    for v in &verdicts {
        eprintln!(
            "  [{}] {}: {}",
            if v.held { "ok" } else { "FAILED" },
            v.name,
            v.detail
        );
    }
    let failed: Vec<&str> = verdicts
        .iter()
        .filter(|v| !v.held && required.iter().any(|r| v.name.starts_with(r)))
        .map(|v| v.name)
        .collect();
    assert!(failed.is_empty(), "{title}: invariants failed: {failed:?}");
}

/// The 6b exemption is the model's answer and nothing else: after a re-read block the flush may remove another
/// line (new arguments, its result, its `memory:` notice, a longer request), but an older message cut from
/// what the next turn is sent is the process's and is not explained — nor is a call of another kind.
#[test]
fn only_the_models_answer_to_the_reread_block_is_excused() {
    use iota::provider::model::ToolCall;
    let call = |kind, messages: Vec<Message>| GrowingCall {
        kind,
        turn: 1,
        messages,
        tools: Vec::new(),
        input: 0,
        output: 0,
        refused: false,
    };
    let remove = |old: &str| {
        Message::assistant("").with_tool_calls(vec![ToolCall {
            id: "f-1-1rm".to_owned(),
            name: "remember".to_owned(),
            arguments: serde_json::json!({"action": "remove", "old": old})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }])
    };
    let side = |block: &str, old: &str, extra: &[Message], cut: bool| {
        let head = vec![
            Message::system(block),
            Message::user("#1 tell me about item 1."),
            Message::assistant("[#1] Here is item 1."),
            Message::notice(FLUSH_MARK),
        ];
        let mut round = head.clone();
        round.push(remove(old));
        round.push(Message::tool_result(
            &remove(old).tool_calls()[0],
            "saved to MEMORY.md ## User (1 / 8 KiB)",
            false,
        ));
        let mut turn = round.clone();
        turn.push(Message::assistant("[#1] Saved.").with_usage(Some(
            iota::provider::usage::Usage {
                input: 10 + extra.len() as u64,
                ..Default::default()
            },
        )));
        turn.extend_from_slice(extra);
        turn.push(Message::user("#2 tell me about item 2."));
        if cut {
            turn.remove(2);
        }
        vec![
            call(CallKind::Flush, head),
            call(CallKind::Followup, round),
            call(CallKind::Turn, turn),
        ]
    };
    let written = [Message::notice(
        "memory: MEMORY.md ## User -1 line: fact-u2;",
    )];
    let held = side("memory as held", "fact-u1;", &[], false);
    let reread = side("memory as re-read", "fact-u2;", &written, false);
    assert_eq!(refresh_explains(2, &held, &reread, 0), Ok(()));

    let cut = side("memory as re-read", "fact-u2;", &written, true);
    let err = refresh_explains(2, &held, &cut, 0).expect_err("an older message cut");
    assert!(err.contains("(Turn) differs"), "{err}");

    let mut other = side("memory as re-read", "fact-u2;", &written, false);
    other[2].kind = CallKind::Summary;
    assert!(refresh_explains(2, &held, &other, 0).is_err());

    // A `memory:` notice the new write did not make — another place, or another line — is the process's.
    for invented in [
        "memory: invented unrelated process notice",
        "memory: MEMORY.md ## User -1 line: fact-u9;",
    ] {
        let extra = [Message::notice(invented)];
        let odd = side("memory as re-read", "fact-u2;", &extra, false);
        let err = refresh_explains(2, &held, &odd, 0).expect_err(invented);
        assert!(err.contains("(Turn) differs"), "{err}");
    }
    // Nor does one write excuse two notices.
    let twice = [written[0].clone(), written[0].clone()];
    let doubled = side("memory as re-read", "fact-u2;", &twice, false);
    assert!(refresh_explains(2, &held, &doubled, 0).is_err());
}

/// A call of `kind` in a unit test of the judges.
fn test_call(kind: CallKind, messages: Vec<Message>) -> GrowingCall {
    GrowingCall {
        kind,
        turn: 1,
        messages,
        tools: Vec::new(),
        input: 0,
        output: 0,
        refused: false,
    }
}

/// A `memory:` notice with no new tool call behind it is not the model's answer: the two sides share their
/// older history and differ only in the memory block, and one of them has a notice nothing wrote.
#[test]
fn a_memory_notice_no_new_write_made_is_not_excused() {
    let side = |block: &str, extra: &[Message]| {
        let head = vec![
            Message::system(block),
            Message::user("#1 tell me about item 1."),
            Message::assistant("[#1] Here is item 1."),
            Message::notice(FLUSH_MARK),
        ];
        let mut turn = head.clone();
        turn.push(Message::assistant("[#1] Nothing to keep."));
        turn.extend_from_slice(extra);
        turn.push(Message::user("#2 tell me about item 2."));
        vec![
            test_call(CallKind::Flush, head),
            test_call(CallKind::Turn, turn),
        ]
    };
    let held = side("memory as held", &[]);
    assert_eq!(
        refresh_explains(2, &held, &side("memory as re-read", &[]), 0),
        Ok(())
    );
    let invented = side(
        "memory as re-read",
        &[Message::notice("memory: invented unrelated process notice")],
    );
    let err = refresh_explains(2, &held, &invented, 0).expect_err("a notice nothing wrote");
    assert!(err.contains("(Turn) differs"), "{err}");
}

/// A summary pass is compared on the history it was handed, not by kind: with the older history the same on
/// both sides and only the memory block re-read, a summary request whose history is lost is the process's —
/// while one that differs only in its long-term memory section (the re-read memory) is not.
#[test]
fn a_summary_request_that_lost_its_history_is_not_excused() {
    let summary = |memory: &str, conversation: &str| {
        format!(
            "Summarize.{MEMORY_SECTION}{memory}\n--- CONVERSATION START ---\n{conversation}--- CONVERSATION END ---"
        )
    };
    let old = "User: #1 tell me about item 1.\nAssistant: [#1] Here is item 1.\n";
    let side = |block: &str, request: String| {
        let head = vec![
            Message::system(block),
            Message::user("#1 tell me about item 1."),
            Message::assistant("[#1] Here is item 1."),
            Message::notice(FLUSH_MARK),
        ];
        let mut turn = head.clone();
        turn.push(Message::assistant("[#1] Nothing to keep."));
        turn.push(Message::user("#2 tell me about item 2."));
        vec![
            test_call(CallKind::Flush, head),
            test_call(CallKind::Summary, vec![Message::user(request)]),
            test_call(CallKind::Turn, turn),
        ]
    };
    let held = side("memory as held", summary("- [user] fact-u1; a", old));
    let reread = side("memory as re-read", summary("- [user] fact-u2; b", old));
    assert_eq!(refresh_explains(2, &held, &reread, 0), Ok(()));

    let lost = side(
        "memory as re-read",
        summary("- [user] fact-u2; b", "CORRUPTED: all old history lost\n"),
    );
    assert!(compare(2, &held, &lost).is_some());
    assert_eq!(memory_differs_at(2, &held, &lost), Some(0));
    let err = refresh_explains(2, &held, &lost, 0).expect_err("a summary of lost history");
    assert!(err.contains("(Summary) differs"), "{err}");

    // Cut a message from what the summary pass is handed: also the process's.
    let cut = side(
        "memory as re-read",
        summary("- [user] fact-u2; b", "User: #1 tell me about item 1.\n"),
    );
    assert!(refresh_explains(2, &held, &cut, 0).is_err());

    // A count of saved lines the side's flush did not make (it wrote nothing): the process's.
    let miscounted = side(
        "memory as re-read",
        summary("- [user] fact-u2; b", old).replacen(
            "Summarize.",
            "Summarize. The memory flush just before this compaction saved 2 lines.",
            1,
        ),
    );
    assert!(refresh_explains(2, &held, &miscounted, 0).is_err());
}

/// Invariant 0 is the user's turn answered, not any reply after it: turns whose only reply without tool calls
/// is the flush's (`[#n] Saved.` after the flush notice) fail, while the same log with the user's own final
/// reply in each turn passes.
#[test]
fn a_flush_reply_does_not_answer_the_users_turn() {
    use serde_json::json;
    let log = |answered: bool| {
        let mut records = Vec::new();
        for n in 1..=TURNS {
            records
                .push(json!({"role": "user", "content": format!("#{n} tell me about item {n}.")}));
            records.push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": format!("t-{n}-0add"), "name": "remember", "arguments": {}}]}));
            records.push(json!({"role": "tool", "content": "saved to MEMORY.md ## User (1 / 8 KiB)", "tool_call_id": format!("t-{n}-0add")}));
            if answered {
                records.push(json!({"role": "assistant", "content": format!("[#{n}] Saved.")}));
            }
            records.push(json!({"role": "user", "notice": true, "content": FLUSH_MARK}));
            records.push(json!({"role": "assistant", "content": "Saved the memory."}));
            records.push(json!({"role": "assistant", "content": format!("[#{n}] Saved.")}));
        }
        records
    };
    let bad = every_turn_answered(&log(false));
    assert!(!bad.held, "{}", bad.detail);
    assert!(bad.detail.starts_with("0 of 2000 turns"), "{}", bad.detail);
    let good = every_turn_answered(&log(true));
    assert!(good.held, "{}", good.detail);

    // A turn missing from the log, or saved twice, fails too.
    let mut records = log(true);
    records.retain(|r| r["content"] != "#5 tell me about item 5.");
    assert!(!every_turn_answered(&records).held);
    let mut records = log(true);
    records.push(json!({"role": "user", "content": "#5 tell me about item 5."}));
    records.push(json!({"role": "assistant", "content": "[#5] Here is item 5."}));
    assert!(!every_turn_answered(&records).held);
}

/// The mechanism in the smallest window a bot runs in, with a model that keeps its memory short (at most 12
/// lines of its own): every invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_runs_two_thousand_turns_in_a_32k_window() {
    let run = drive(WINDOW, tidy(WINDOW), &drop_points(SEED, DROPS)).await;
    report(
        "tidy memory, 32k",
        &run,
        &["0 ", "1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "6b ", "7 "],
    );
}

/// The long run's model: a `remember` every 25 turns, a line per flush, at most 12 lines kept.
fn tidy(window: u64) -> GrowingProvider {
    GrowingProvider::new(window, SEED)
        .remembering_at(remember_turns())
        .keeping(12)
}

/// The long run with a model that consolidates only when told to — the design's own equilibrium: MEMORY.md
/// grows to the soft threshold (6 KiB) and hovers there (§3.5) — in a 32k window. Every invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_whose_memory_sits_at_the_soft_threshold_runs_in_a_32k_window() {
    let provider = GrowingProvider::new(WINDOW, SEED).remembering_at(remember_turns());
    let run = drive(WINDOW, provider, &drop_points(SEED, DROPS)).await;
    report(
        "memory at the soft threshold, 32k",
        &run,
        &["0 ", "1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "6b ", "7 "],
    );
}

/// The same model in an 8k window — BELOW the minimum a bot runs in (bot-mode.md §4.1, `BOT_MIN_WINDOW`), which
/// `iota run` now refuses at startup. Kept as the evidence for that minimum, not as a gap to close.
///
/// FAILS (2026-10-01): 6 KiB of memory is ~1.5k tokens in every request's system message, each `remember`
/// result repeats the whole section it wrote to (~6 KiB again), and the flush exchange carrying those results
/// is what a compaction keeps (§3.6.1 S2c). In an 8k window the occupancy right after a compaction is already
/// at the 4096 threshold, so the bot compacts every other turn (invariant 3: "one per 2.2 turns"), and a flush
/// turn that writes, then consolidates twice, goes over the window and is refused (invariant 2). The memory cap
/// (8 KiB) and the reserve floor (32k) are flat — neither scales with the window — which is why the window has
/// a floor instead. 6b fails here too, as a consequence: most drops follow a turn refused over the window, which
/// (not having landed) queued no flush in the running process, while the restart — resumed over the threshold
/// — queues one; the rest follow a snoozed compaction whose watermark lives in memory only. Neither happens in
/// a 32k window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "below the minimum window: the evidence for BOT_MIN_WINDOW (see the doc)"]
async fn a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window() {
    let provider = GrowingProvider::new(SMALL_WINDOW, SEED).remembering_at(remember_turns());
    let run = drive(SMALL_WINDOW, provider, &drop_points(SEED, DROPS)).await;
    report(
        "memory at the soft threshold, 8k",
        &run,
        &["0 ", "1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "6b ", "7 "],
    );
}
