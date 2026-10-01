//! The long run (docs/design/bot-mode.md §5.2): a bot with a 32k window — the smallest a bot runs in (§4.1) —
//! driven through 2000 turns by
//! [`GrowingProvider`] — a model whose memory grows to the soft threshold and hovers there; whose usage is the request measured and which refuses a request over the window — with
//! the process dropped and resumed at seeded random points. It checks the mechanism, not a model:
//!
//! 0. every user turn `1..=2000` is in the log exactly once, with its final reply after it;
//! 1. the log only grows: every append leaves the bytes before it as they were;
//! 2. every request fits the window (measured on the request, never estimated — and a request over it would
//!    have been refused, so a pass is a real bound);
//! 3. the compactions are about as many as the growth divided by the room each one frees;
//! 4. every compaction is preceded by exactly one flush notice, or its marker says `flush_skipped`;
//! 5. `MEMORY.md` stays within its 8 KiB cap;
//! 6. (6a) a restart loads the view the process held: at every drop the same process is ALSO run on without the
//!    restart (a copy of the disk taken at the drop point is what the restart resumes), and the view the restart
//!    loaded is, byte for byte, the one the running process sends next. What the two then SEND is not compared
//!    here — the fake answers the memory block, which a restart re-reads (§3.4), so the two may diverge for the
//!    model's reasons; the paired runs with a fixed model in `tests/repl/bot_flush.rs`
//!    (`a_restart_sends_what_running_on_would_have_*`) compare the sends;
//! 7. the startup load time as the log grows — printed, and held to the §2.6 threshold (2 s) as a loose bound.
//!
//! Drops happen at the idle prompt, between turns — where a bot sits nearly all its life — including the
//! moment a flush notice has been queued and not yet run. The harness clock is fixed, so no run crosses a
//! midnight that one side of a restart sees and the other does not. Every verdict is printed (`--nocapture`);
//! the test requires the ones it names. The 8k run that is the evidence for the minimum window is no longer
//! here: `docs/history/bot-mode/bot-mode-8k-evidence.md` keeps its command and output, and `BOT_MIN_WINDOW` its
//! numbers. Most of the run's time is the durability path itself: every persisted batch is a `sync_all` (a full
//! flush on macOS).
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
pub(crate) fn copy_tree(from: &Path, to: &Path) {
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
    /// pointer), runs the loop over `script`, returns the bundle, how long the open took and the view it loaded.
    /// `on_open` is told the bundle before the loop starts.
    async fn life(
        &self,
        window: u64,
        provider: GrowingProvider,
        script: Vec<Reply>,
        on_open: impl FnOnce(&Path),
    ) -> (PathBuf, Duration, Vec<Message>) {
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
            imported_history: history.clone(),
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
        (dir, load, history)
    }
}

/// The history a call carries, without the system message the overlay (the memory block) composes: the view.
fn view_of(call: &GrowingCall) -> &[Message] {
    match call.messages.first() {
        Some(m) if m.role() == Role::System => &call.messages[1..],
        _ => &call.messages,
    }
}

/// How one restart compared with running on.
struct DropReport {
    turn: u64,
    /// A flush notice was queued when the process went down.
    flush_queued: bool,
    /// 6a: the view the restart loaded against the one the process held; `None` when the run without the
    /// restart compacted first, so its first call does not show the view as it was at the drop.
    loaded: Option<Result<(), String>>,
}

/// The view the process held at the drop, read off the first call it made after it (a user turn or the flush
/// turn, less its new last message) — unless a compaction's summary pass came first.
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

/// 6a for one drop: the view the restart loaded (`loaded`, as `open_bot` returned it) against the one the run
/// without the restart held.
fn compare_loaded(reference: &[GrowingCall], loaded: &[Message]) -> Option<Result<(), String>> {
    let held = view_at_drop(reference)?;
    let loaded = match loaded.first() {
        Some(m) if m.role() == Role::System => &loaded[1..],
        _ => loaded,
    };
    if held == loaded {
        return Some(Ok(()));
    }
    let at = held
        .iter()
        .zip(loaded)
        .position(|(p, q)| p != q)
        .unwrap_or(held.len().min(loaded.len()));
    Some(Err(format!(
        "the views differ at message {at} of {}/{}:\n  held:   {:?}\n  loaded: {:?}",
        held.len(),
        loaded.len(),
        held.get(at),
        loaded.get(at)
    )))
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

        let log_bytes = log_path
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let slot = Arc::clone(&log_path);
        let (_, load, loaded) = bot
            .life(window, provider.clone(), script, move |dir| {
                *slot.lock().unwrap() = Some(dir.join("messages.jsonl"));
            })
            .await;
        run.loads.push((next, log_bytes, load));
        let calls = provider.calls();

        // The restart before this life, against the run that went on without it.
        if let Some((turn, reference, flush_queued)) = pending.take() {
            run.drops.push(DropReport {
                turn,
                flush_queued,
                loaded: compare_loaded(&reference, &loaded),
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
///
/// Its reach: the main line's own log agreeing with itself — one flush (or a skip) per compaction, counted. It
/// never sees a no-restart reference, so it is no restart/no-restart comparison: a restart that ran the flush
/// at another turn, ran the summary before it, or sent a different history still balances here. The sends
/// are compared by the paired runs in `tests/repl/bot_flush.rs`.
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

    // 6a. A restart loads exactly the view the process held: byte for byte, at every drop where the run without
    // the restart shows it (its compaction's summary pass first hides it; the restart's side is read off the open
    // itself, so never hidden) — and at least half of them must, or the check would be vacuous.
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
        loaded_bad.is_empty() && run.drops.len() == DROPS && compared * 2 >= DROPS,
        format!(
            "{compared} of {} drops compared ({} with a flush queued), {} differ{}",
            run.drops.len(),
            run.drops.iter().filter(|d| d.flush_queued).count(),
            loaded_bad.len(),
            if loaded_bad.is_empty() {
                String::new()
            } else {
                format!(":\n{}", loaded_bad.join("\n"))
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

/// The long run with a model that consolidates only when told to — the design's own equilibrium: MEMORY.md
/// grows to the soft threshold (6 KiB) and hovers there (§3.5) — in a 32k window. Every invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_whose_memory_sits_at_the_soft_threshold_runs_in_a_32k_window() {
    let provider = GrowingProvider::new(WINDOW, SEED).remembering_at(remember_turns());
    let run = drive(WINDOW, provider, &drop_points(SEED, DROPS)).await;
    report(
        "memory at the soft threshold, 32k",
        &run,
        &["0 ", "1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "7 "],
    );
}
