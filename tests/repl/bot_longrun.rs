//! The long run (docs/design/bot-mode.md §5.2): a bot with an 8k window driven through 2000 turns by
//! [`GrowingProvider`] — whose usage is the request measured and which refuses a request over the window — with
//! the process dropped and resumed at seeded random points. It checks the mechanism, not a model:
//!
//! 1. the log only grows: every append leaves the bytes before it as they were;
//! 2. every request fits the window (measured on the request, never estimated — and a request over it would
//!    have been refused, so a pass is a real bound);
//! 3. the compactions are about as many as the growth divided by the room each one frees;
//! 4. every compaction is preceded by exactly one flush notice, or its marker says `flush_skipped`;
//! 5. `MEMORY.md` stays within its 8 KiB cap;
//! 6. a restart changes nothing: at every drop the same process is ALSO run on without the restart (a copy of
//!    the disk taken at the drop point is what the restart resumes). 6a: the view the restart loads is, byte for
//!    byte, the one the process held. 6b: the two send the model the same calls, history and memory block, up to
//!    and including the next user turn;
//! 7. the startup load time as the log grows — printed, and held to the §2.6 threshold (2 s) as a loose bound.
//!
//! Drops happen at the idle prompt, between turns — where a bot sits nearly all its life — including the
//! moment a flush notice has been queued and not yet run. Every verdict is printed (`--nocapture`); each test
//! requires the ones it names. Two do not hold today and are kept, `#[ignore]`d, as reproductions:
//! [`a_restart_sends_what_running_on_would_have`] (6b) and
//! [`a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window`] (2, 3, 6b). Most of the run's time
//! is the durability path itself: every persisted batch is a `sync_all` (a full flush on macOS).

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

/// "8k": a bot's reserve is capped at half of it, so it compacts at 4096.
const WINDOW: u64 = 8_192;
/// The bot threshold for [`WINDOW`] (`min(max(32k, 25%), window / 2)` reserved).
const THRESHOLD: u64 = WINDOW / 2;
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
            BotOpen::Fresh { writer, .. } => (writer, Vec::new()),
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
            harness: iota::agents::harness::HarnessInputs::default(),
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
                context_window: iota::session::Param::config(WINDOW),
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
    /// 6b: what the two sent, up to the next user turn — `None` when it was the same.
    diff: Option<String>,
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

/// Compares what the restarted run sent with what the run without the restart sent.
fn compare(turn: u64, reference: &[GrowingCall], restarted: &[GrowingCall]) -> Option<String> {
    let a = up_to_turn(reference, turn);
    let b = up_to_turn(restarted, turn);
    let kinds = |cs: &[GrowingCall]| cs.iter().map(|c| c.kind).collect::<Vec<_>>();
    if kinds(a) != kinds(b) {
        return Some(format!(
            "different calls: without the restart {:?}, with it {:?}",
            kinds(a),
            kinds(b)
        ));
    }
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if view_of(x) != view_of(y) {
            let at = view_of(x)
                .iter()
                .zip(view_of(y))
                .position(|(p, q)| p != q)
                .unwrap_or(view_of(x).len().min(view_of(y).len()));
            return Some(format!(
                "call {i} ({:?}): the views differ at message {at} of {}/{}:\n  without: {:?}\n  with:    {:?}",
                x.kind,
                view_of(x).len(),
                view_of(y).len(),
                view_of(x).get(at),
                view_of(y).get(at)
            ));
        }
        if x.messages.first() != y.messages.first() {
            return Some(format!(
                "call {i} ({:?}): the memory block differs:\n  without: {:?}\n  with:    {:?}",
                x.kind,
                x.messages.first(),
                y.messages.first()
            ));
        }
    }
    None
}

/// What the whole run left.
struct Run {
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
async fn drive(provider: GrowingProvider, drops: &BTreeSet<u64>) -> Run {
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
            .life(provider.clone(), script, move |dir| {
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
                loaded: compare_loaded(&reference, &calls[first..]),
                diff: compare(turn, &reference, &calls[first..]),
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

/// One invariant's verdict: what was measured, and whether it held.
struct Verdict {
    name: &'static str,
    held: bool,
    detail: String,
}

fn verdict(name: &'static str, held: bool, detail: String) -> Verdict {
    Verdict { name, held, detail }
}

/// The seven invariants over a finished run, each with what it measured.
#[allow(clippy::cast_precision_loss)] // token counts far below 2^52
fn verdicts(run: &Run) -> Vec<Verdict> {
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
        refused.is_empty() && widest <= WINDOW,
        format!(
            "{} of {} calls refused as over {WINDOW} tokens {by_kind:?} (largest refused: {}); widest answered call {widest}",
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
    let room = THRESHOLD as f64 - mean(&kept);
    let cycle = room + mean(&turn_sizes) / 2.0 + mean(&flush_sizes);
    let expected = growth / cycle;
    let ratio = markers as f64 / expected;
    let turns = TURNS as f64;
    out.push(verdict(
        "3 compactions ≈ expected",
        summaries.len() == markers && (0.85..=1.15).contains(&ratio),
        format!(
            "{markers} markers ({} summary passes), expected ≈ {expected:.0} = growth {growth:.0} / ({THRESHOLD} − {:.0} kept + {:.0} half a turn + {:.0} flush); ratio {ratio:.2} (0.85–1.15 accepted), one per {:.1} turns",
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

    // 6b. A restart sends the model what running on would have: the same calls, the same history and the same
    // memory block, up to and including the next user turn.
    let broken: Vec<String> = run
        .drops
        .iter()
        .filter_map(|d| {
            d.diff.as_ref().map(|diff| {
                format!(
                    "drop before #{} (flush queued: {}): {diff}",
                    d.turn, d.flush_queued
                )
            })
        })
        .collect();
    out.push(verdict(
        "6b restart sends what running on would have",
        broken.is_empty() && run.drops.len() == DROPS,
        format!(
            "{} restarts compared ({} with a flush queued), {} differ{}",
            run.drops.len(),
            run.drops.iter().filter(|d| d.flush_queued).count(),
            broken.len(),
            if broken.is_empty() {
                String::new()
            } else {
                format!(":\n{}", broken.join("\n"))
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

/// The mechanism, with a model that keeps its memory short (at most 12 lines of its own): invariants 1–5, 6a
/// and 7. 6b, the full restart statement, does not hold today — [`a_restart_sends_what_running_on_would_have`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_runs_two_thousand_turns_in_an_8k_window() {
    let run = drive(tidy(), &drop_points(SEED, DROPS)).await;
    report(
        "tidy memory",
        &run,
        &["1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "7 "],
    );
}

/// The long run's model: a `remember` every 25 turns, a line per flush, at most 12 lines kept.
fn tidy() -> GrowingProvider {
    GrowingProvider::new(WINDOW, SEED)
        .remembering_at(remember_turns())
        .keeping(12)
}

/// Invariant 6 in full: a restart sends the model exactly what running on would have sent.
///
/// FAILS (2026-10-01) on three counts, each a way the process is more than a cache of the session (§1.2):
///
/// - **A queued flush dies with the process.** `flush_pending` and the queued notice live in memory only; the
///   restarted bot neither flushes nor, usually, compacts before the next turn (see the next point), and the
///   flush it owed is gone.
/// - **The resumed meter under-counts.** `repl::run` seeds the budget with `budget.update(&history)`, a local
///   count of the view alone: the usage persisted on the view's last answer is not read, and neither the
///   memory block nor the tool definitions are counted. A turn the running process would have compacted
///   before (the projected usage over the threshold) goes out uncompacted after a restart.
/// - **The memory block is re-read at startup** (§3.4 makes startup a refresh moment), while the running
///   process keeps its copy frozen until the next compaction: after a `remember` in a normal turn, the system
///   message differs. This one is by design; it is here because it is a difference the model sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "reproduces a design gap: a restart loses a queued flush and re-seeds the meter low (see the doc)"]
async fn a_restart_sends_what_running_on_would_have() {
    let run = drive(tidy(), &drop_points(SEED, DROPS)).await;
    report("restart = running on", &run, &["6b "]);
}

/// The long run with a model that consolidates only when told to — the design's own equilibrium: MEMORY.md
/// grows to the soft threshold (6 KiB) and hovers there (§3.5). Every invariant.
///
/// FAILS (2026-10-01), kept as the reproduction of a design gap: 6 KiB of memory is ~1.5k tokens in every
/// request's system message, each `remember` result repeats the whole section it wrote to (~6 KiB again), and
/// the flush exchange carrying those results is what a compaction keeps (§3.6.1 S2c). In an 8k window the
/// occupancy right after a compaction is already at the 4096 threshold, so the bot compacts every other turn
/// (invariant 3: "one per 2.2 turns"), and a flush turn that writes, then consolidates twice, goes over the
/// window and is refused (invariant 2). Nothing scales the flat 8 KiB cap or the section echo with the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "reproduces a design gap: a memory at its soft threshold outgrows an 8k window (see the doc)"]
async fn a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window() {
    let provider = GrowingProvider::new(WINDOW, SEED).remembering_at(remember_turns());
    let run = drive(provider, &drop_points(SEED, DROPS)).await;
    report(
        "memory at the soft threshold",
        &run,
        &["1 ", "2 ", "3 ", "4 ", "5 ", "6a ", "6b ", "7 "],
    );
}
