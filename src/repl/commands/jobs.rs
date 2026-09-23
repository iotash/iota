//! `/jobs` — the background jobs running right now, LIVE: two tabs over the registry, and a detail page
//! for the one picked, all re-read once a second.
//!
//! The `Jobs` tab (`Panel::list`, the `/session` Resume shape, a search past the fold) has one row per job;
//! Enter opens the detail page. The `Kill` tab (`Panel::multi`, the `/session` Delete shape) has the same
//! rows with a checkbox each; Space checks, Enter kills the checked ([`Jobs::kill`]) and the surface
//! closes — nothing is printed for it, the notice `[background job b3 finished: killed] …` says what
//! happened. Both tabs refresh the `/tools` way ([`Panel::with_refresh`], [`TabbedSpec::refresh_every_ms`],
//! [`REFRESH_EVERY_MS`]): the rows are made again from [`Jobs::snapshot`] each tick — the clocks walk, a job
//! that ended is gone, one that started is there, the count above the rows follows — and the cursor and the
//! checks follow the job's ID ([`Refreshed::keys`]), not the row's index, so a job finishing above the cursor
//! never moves it onto another job; a search filter is kept. A row's command is the header's label
//! ([`crate::text::header_command`]), the text the `[shell …]` row, the notice and the status row show.
//!
//! The detail page — a `View` titled `job b3`: the command in full, the clock with the wall-clock start,
//! the pid, the log path and the log's last twenty lines — re-reads the clock and the tail each tick, and
//! when the job ends under it the clock line becomes the verdict, `finished: exit 0 after 1m 12s` (or
//! `killed`, or `timed out after …`, [`Jobs::ended`]), the rest of the page staying as it was; Esc returns
//! to the list, re-read, until Esc closes the list. No key on the page kills the job: the list is
//! searchable, so a letter there is the filter, and the same key meaning two things on one surface was
//! refused — killing is the Kill tab's (decided 2026-09-23).
//!
//! The command exists only while a job runs (`CmdFlags::jobs`, flipped by the registry's watch), so an
//! empty set is the race between the row and the registry, answered with one notice rather than an empty
//! panel.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::shell::exec::read_capped;
use crate::shell::jobs::{JobDone, JobInfo, Jobs, job_status};
use crate::sync::lock;
use crate::text::width::str_width;
use crate::text::{clock, header_command};
use crate::ui::facade::{Panel, Refreshed, TabbedSpec};

use crate::repl::render::banner::tilde;
use crate::repl::render::styles::{bold, dim};
use crate::repl::run::Repl;

/// How often the tabs and the page re-read the registry, in milliseconds: the clocks walk once a second,
/// the status row's cadence.
pub(crate) const REFRESH_EVERY_MS: u64 = 1000;

/// How many of the log's last lines the detail page shows.
pub(crate) const TAIL_LINES: usize = 20;

/// What `/jobs` says when the last job ended between the row and the command.
pub(crate) const NO_JOBS: &str = "no background job is running";

/// The list's rows, one per job: `b3  1m12s  cargo test --test session resume` — the id and the clock
/// padded to their widest, the command as the header showed it (one line, 64 runes), nothing else: the
/// log path is the detail page's. Oldest first, as the registry lists them; empty for an empty set, which
/// the command answers with [`NO_JOBS`] rather than a panel.
pub(crate) fn job_rows(jobs: &[JobInfo], now: Instant) -> Vec<String> {
    let clocks: Vec<String> = jobs
        .iter()
        .map(|j| clock(now.saturating_duration_since(j.started)))
        .collect();
    let id_width = jobs.iter().map(|j| str_width(&j.id)).max().unwrap_or(0);
    let clock_width = clocks.iter().map(|c| str_width(c)).max().unwrap_or(0);
    jobs.iter()
        .zip(&clocks)
        .map(|(job, clock)| {
            format!(
                "{}  {clock:>clock_width$}  {}",
                bold(&format!("{:<id_width$}", job.id)),
                header_command(&job.command)
            )
        })
        .collect()
}

/// The line above the list: `1 job running`, `2 jobs running`.
pub(crate) fn job_count(n: usize) -> String {
    let plural = if n == 1 { "job" } else { "jobs" };
    format!("{n} {plural} running")
}

/// One tab's rows over the live registry: a fresh [`Jobs::snapshot`] each call, rendered as the rows
/// ([`job_rows`]), keyed by the job's id and headed by the count ([`job_count`]) — and the snapshot kept,
/// so a commit's row index is read against the rows the user last SAW, not the set at open. One per tab:
/// each tab's refresh is its own registry read, and its rows are its own record.
pub(crate) struct LiveRows {
    jobs: Arc<Jobs>,
    /// The jobs behind the rows last handed out, in row order.
    shown: Arc<Mutex<Vec<JobInfo>>>,
}

impl LiveRows {
    pub(crate) fn new(jobs: &Arc<Jobs>) -> Self {
        Self {
            jobs: Arc::clone(jobs),
            shown: Arc::default(),
        }
    }

    /// The rows as of now — one registry read — remembered as what is shown.
    pub(crate) fn refreshed(&self) -> Refreshed {
        let jobs = self.jobs.snapshot();
        let out = Refreshed {
            rows: job_rows(&jobs, Instant::now()),
            keys: jobs.iter().map(|j| j.id.clone()).collect(),
            prompt: Some(job_count(jobs.len())),
        };
        *lock(&self.shown) = jobs;
        out
    }

    /// The job behind row `i` of the rows last handed out.
    pub(crate) fn shown(&self, i: usize) -> Option<JobInfo> {
        lock(&self.shown).get(i).cloned()
    }

    /// The refresh closure: the same registry, the same record.
    pub(crate) fn refresh(&self) -> impl FnMut() -> Refreshed + Send + 'static {
        let live = Self {
            jobs: Arc::clone(&self.jobs),
            shown: Arc::clone(&self.shown),
        };
        move || live.refreshed()
    }
}

/// The detail page for one job, `tail` being its log's last lines ([`log_tail`]) and `ended` how it
/// ended, if it has:
///
/// ```text
/// command:  cargo test --test session resume
///           --features everything
/// running:  1m12s (started 14:02:11)
/// pid:      4242
/// output:   ~/.cache/iota-jobs/77/b3.log
/// ── last 20 lines ──
/// …
/// ```
///
/// The command is whole — every line, under the first — and the page wraps, so nothing is cut; `now` and
/// `wall_now` are one instant on two clocks (the elapsed figure, and the wall-clock start it is counted
/// back from); a pid the OS never reported reads `(unknown)`, an empty tail `(no output yet)`. Once the
/// job has ended the clock line is the verdict instead — `finished: exit 0 after 1m 12s`, `finished:
/// killed`, `finished: timed out after 10m 0s` ([`job_status`]) — and the rest of the page stays.
pub(crate) fn job_detail(
    job: &JobInfo,
    now: Instant,
    wall_now: &jiff::Zoned,
    home: Option<&Path>,
    tail: &[String],
    ended: Option<&JobDone>,
) -> Vec<String> {
    let label = |name: &str| format!("{}  ", dim(&format!("{name:<8}")));
    let pid = job
        .pid
        .map_or_else(|| "(unknown)".to_owned(), |pid| pid.to_string());

    let mut lines = Vec::with_capacity(6 + tail.len());
    let mut command = job.command.lines();
    lines.push(format!(
        "{}{}",
        label("command:"),
        command.next().unwrap_or("")
    ));
    lines.extend(command.map(|line| format!("{:10}{line}", "")));
    if let Some(done) = ended {
        // `finished:` is nine columns and one space: the verdict sits in the value column like the rest.
        lines.push(format!("{} {}", dim("finished:"), job_status(done)));
    } else {
        let elapsed = now.saturating_duration_since(job.started);
        let started = jiff::SignedDuration::try_from(elapsed)
            .ok()
            .and_then(|d| wall_now.checked_sub(d).ok())
            .map_or_else(|| "?".to_owned(), |at| at.strftime("%H:%M:%S").to_string());
        lines.push(format!(
            "{}{} (started {started})",
            label("running:"),
            clock(elapsed)
        ));
    }
    lines.push(format!("{}{pid}", label("pid:")));
    lines.push(format!(
        "{}{}",
        label("output:"),
        tilde(&job.output_path, home)
    ));
    lines.push(dim(&format!("── last {TAIL_LINES} lines ──")));
    if tail.is_empty() {
        lines.push(dim("(no output yet)"));
    } else {
        lines.extend(tail.iter().cloned());
    }
    lines
}

/// The last [`TAIL_LINES`] lines of the log at `path` — read under the notice's byte caps
/// ([`read_capped`]: two seeks, never the whole of a big file), so the page costs the loop one open.
/// Empty for a missing log or one holding only whitespace.
pub(crate) fn log_tail(path: &Path) -> Vec<String> {
    let text = read_capped(path).unwrap_or_default();
    if text.trim().is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|l| (*l).to_owned())
        .collect()
}

/// The page's lines as of now: the tail re-read from the log, and the job's end — if it has ended
/// ([`Jobs::ended`]) — in the clock's place. What the page opens with and what each tick brings.
pub(crate) fn page_lines(jobs: &Jobs, job: &JobInfo, home: Option<&Path>) -> Vec<String> {
    let tail = log_tail(&job.output_path);
    let ended = jobs.ended(&job.id);
    job_detail(
        job,
        Instant::now(),
        &jiff::Zoned::now(),
        home,
        &tail,
        ended.as_ref(),
    )
}

/// `/jobs`: the two tabs, then — for a row picked on `Jobs` — the page, then the tabs again, until the
/// tabs are left with Esc, Enter on `Kill` has killed the checked, or the facade goes away. Every open is
/// a fresh read, and the rows keep reading while open.
pub(crate) async fn cmd_jobs(repl: &Repl) {
    let jobs = &repl.handles.jobs;
    loop {
        let list = LiveRows::new(jobs);
        let kill = LiveRows::new(jobs);
        let (l, k) = (list.refreshed(), kill.refreshed());
        if l.rows.is_empty() {
            repl.handles.tr.notice(NO_JOBS);
            return;
        }
        let tabs = TabbedSpec {
            refresh_every_ms: REFRESH_EVERY_MS,
            panels: vec![
                Panel::list("Jobs", l.rows)
                    .with_keys(l.keys)
                    .with_prompt(l.prompt.unwrap_or_default())
                    .with_search(true)
                    .with_refresh(list.refresh()),
                Panel::multi("Kill", k.rows)
                    .with_keys(k.keys)
                    .with_prompt(k.prompt.unwrap_or_default())
                    .with_search(true)
                    .with_refresh(kill.refresh()),
            ],
            ..TabbedSpec::default()
        };
        let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, tabs).await else {
            return;
        };
        if r.cancelled {
            return;
        }
        if r.focused == 1 {
            // The Kill tab: each checked row's job, as the rows stood when Enter was pressed. The notice
            // each one produces is the whole report; nothing is printed here.
            let checked = r
                .panels
                .get(1)
                .map(|p| p.checked.clone())
                .unwrap_or_default();
            for i in checked {
                if let Some(job) = kill.shown(i) {
                    jobs.kill(&job.id);
                }
            }
            return;
        }
        let cursor = r.panels.first().map_or(0, |p| p.cursor);
        let Some(job) = list.shown(cursor) else {
            return;
        };
        let home: Option<PathBuf> = repl.conv.agent.home.clone();
        let lines = page_lines(jobs, &job, home.as_deref());
        let (page_jobs, page_job) = (Arc::clone(jobs), job.clone());
        let page = TabbedSpec {
            refresh_every_ms: REFRESH_EVERY_MS,
            panels: vec![
                Panel::view(format!("job {}", job.id), lines)
                    .with_wrap(true)
                    .with_refresh(move || page_lines(&page_jobs, &page_job, home.as_deref())),
            ],
            ..TabbedSpec::default()
        };
        // Esc (or any close) on the page goes back to the tabs; only the facade going away ends this.
        if repl
            .handles
            .ui
            .tabbed(&repl.handles.cancel, page)
            .await
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::shell::jobs::{JobDone, JobInfo, Jobs};
    use crate::text::ansi::strip_sgr;

    use super::{LiveRows, TAIL_LINES, job_count, job_detail, job_rows, log_tail, page_lines};

    fn job(id: &str, command: &str, ago: u64, now: Instant) -> JobInfo {
        JobInfo {
            id: id.to_owned(),
            command: command.to_owned(),
            pid: Some(4242),
            started: now.checked_sub(Duration::from_secs(ago)).unwrap_or(now),
            output_path: PathBuf::from(format!("/home/u/.cache/iota-jobs/77/{id}.log")),
        }
    }

    // Three columns, aligned to the widest id and clock; the command is the header's label — a script's
    // first line and ` …`, a long line cut at 64 runes — and the log path is not on the row.
    #[test]
    fn rows_are_aligned_and_cut_like_the_header() {
        let now = Instant::now();
        let long =
            "cargo test --test session resume --features everything-under-the-sun -- --nocapture";
        let jobs = [
            job("b3", "cargo test\n  --test session resume", 72, now),
            job("b12", long, 3725, now),
        ];
        let rows: Vec<String> = job_rows(&jobs, now).iter().map(|r| strip_sgr(r)).collect();
        assert_eq!(
            rows,
            [
                "b3      1m12s  cargo test …",
                "b12  1h02m05s  cargo test --test session resume --features everything-under-th…",
            ]
        );
        assert_eq!(
            rows[1].rsplit("  ").next(),
            Some(crate::text::header_command(long).as_str())
        );
        assert!(!rows.iter().any(|r| r.contains(".log")), "{rows:?}");
        // One job: no padding to speak of. A job "started" after `now` reads 0s: a clock cannot run
        // backwards.
        assert_eq!(
            strip_sgr(&job_rows(&jobs[..1], now)[0]),
            "b3  1m12s  cargo test …"
        );
        let future = [job("b1", "x", 0, now + Duration::from_secs(5))];
        assert_eq!(strip_sgr(&job_rows(&future, now)[0]), "b1  0s  x");
        // Nothing running — the race between the row and the registry — is no row at all: the command
        // says `NO_JOBS` and opens nothing.
        assert!(job_rows(&[], now).is_empty());
        assert_eq!(job_count(1), "1 job running");
        assert_eq!(job_count(2), "2 jobs running");
    }

    // The page: the command whole (a script line by line under the first), the clock counted back to a
    // wall-clock start, the pid, the log under `~`, then the tail — and, once the job has ended, the
    // verdict where the clock was, the rest untouched.
    #[test]
    fn detail_lines() {
        let now = Instant::now();
        // Built on `TimeZone::UTC` directly: no name lookup, so no tz database is needed (Windows CI).
        let wall = jiff::civil::date(2026, 9, 23)
            .at(14, 3, 23, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .expect("a zoned time");
        let job = job("b3", "cargo test\n  --test session resume\n", 72, now);
        let tail = vec!["running 3 tests".to_owned(), "test a ... ok".to_owned()];
        let home = std::path::Path::new("/home/u");
        let lines: Vec<String> = job_detail(&job, now, &wall, Some(home), &tail, None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            lines,
            [
                "command:  cargo test".to_owned(),
                "            --test session resume".to_owned(),
                "running:  1m12s (started 14:02:11)".to_owned(),
                "pid:      4242".to_owned(),
                format!("output:   ~{sep}.cache/iota-jobs/77/b3.log"),
                "── last 20 lines ──".to_owned(),
                "running 3 tests".to_owned(),
                "test a ... ok".to_owned(),
            ]
        );
        // No pid, no home, no output: the page says so rather than leaving a blank.
        let bare = JobInfo {
            pid: None,
            ..job.clone()
        };
        let lines: Vec<String> = job_detail(&bare, now, &wall, None, &[], None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(lines[3], "pid:      (unknown)");
        assert_eq!(lines[4], "output:   /home/u/.cache/iota-jobs/77/b3.log");
        assert_eq!(lines[6], "(no output yet)");
        assert_eq!(lines.len(), 7);

        // Ended under the page: the clock line is the verdict, in the value column, and every other
        // line is what it was.
        let done = JobDone {
            id: "b3".to_owned(),
            command: job.command.clone(),
            exit: Some(0),
            timed_out: false,
            killed: false,
            elapsed: Duration::from_secs(72),
            output_path: job.output_path.clone(),
        };
        let ended: Vec<String> = job_detail(&job, now, &wall, Some(home), &tail, Some(&done))
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(ended[2], "finished: exit 0 after 1m 12s");
        assert_eq!(ended.len(), 8);
        assert_eq!(
            ended[..2],
            lines_of(&job, now, &wall, Some(home), &tail)[..2]
        );
        assert_eq!(
            ended[3..],
            lines_of(&job, now, &wall, Some(home), &tail)[3..]
        );
        let killed = JobDone {
            exit: None,
            killed: true,
            ..done.clone()
        };
        assert_eq!(
            strip_sgr(&job_detail(&job, now, &wall, Some(home), &tail, Some(&killed))[2]),
            "finished: killed"
        );
        let slow = JobDone {
            exit: None,
            timed_out: true,
            elapsed: Duration::from_secs(600),
            ..done
        };
        assert_eq!(
            strip_sgr(&job_detail(&job, now, &wall, Some(home), &tail, Some(&slow))[2]),
            "finished: timed out after 10m 0s"
        );
    }

    /// The running page's lines, SGR stripped.
    fn lines_of(
        job: &JobInfo,
        now: Instant,
        wall: &jiff::Zoned,
        home: Option<&std::path::Path>,
        tail: &[String],
    ) -> Vec<String> {
        job_detail(job, now, wall, home, tail, None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect()
    }

    // The tail: the last twenty lines of what is there, none for a missing or blank log.
    #[test]
    fn tail_is_the_last_twenty_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("b1.log");
        assert!(log_tail(&path).is_empty(), "a missing log");
        std::fs::write(&path, "  \n\n").expect("write");
        assert!(log_tail(&path).is_empty(), "a blank log");
        std::fs::write(&path, "one\ntwo\n").expect("write");
        assert_eq!(log_tail(&path), ["one", "two"]);
        let many = (0..25).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "line {i}");
            acc
        });
        std::fs::write(&path, &many).expect("write");
        let tail = log_tail(&path);
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(tail[0], "line 5");
        assert_eq!(tail[19], "line 24");
    }

    /// Whether the command lines below (`sleep`, `true`) would run here: the interpreter is `bash` on
    /// Unix and, on Windows, whatever `shell::interp`'s ladder found — the twin of
    /// `tests/tool/shell.rs::skip_unless_posix`.
    fn skip_unless_posix(test: &str) -> bool {
        let shell = crate::shell::interp::resolve().expect("this machine has no shell at all");
        if shell.is_posix() {
            return false;
        }
        println!(
            "SKIP: {test} — the resolved interpreter is {}, not a POSIX shell",
            shell.program.display()
        );
        true
    }

    fn opts(command: &str) -> crate::shell::exec::Options {
        crate::shell::exec::Options {
            command: command.to_owned(),
            dir: PathBuf::new(),
            timeout: None,
            sandbox: None,
        }
    }

    /// Waits for the registry to have `n` jobs running, up to four seconds.
    async fn running(jobs: &Jobs, n: usize) {
        for _ in 0..400 {
            if jobs.running() == n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{} jobs still running, wanted {n}", jobs.running());
    }

    // The live rows: every call is the registry as of now — a job gone is a row gone, the keys are the ids
    // in row order, the count follows — and the rows a commit indexes are the rows last handed out. The
    // page's lines follow the job's end: the clock line becomes the verdict once the registry remembers
    // one, the log's tail staying.
    #[tokio::test]
    async fn live_rows_and_the_page_follow_the_registry() {
        if skip_unless_posix("live_rows_and_the_page_follow_the_registry") {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let jobs = Jobs::new(dir.path());
        jobs.spawn(&opts("sleep 30")).expect("spawn");
        jobs.spawn(&opts("sleep 31")).expect("spawn");
        let live = LiveRows::new(&jobs);

        let first = live.refreshed();
        assert_eq!(first.keys, ["b1", "b2"]);
        assert_eq!(first.prompt.as_deref(), Some("2 jobs running"));
        let rows: Vec<String> = first.rows.iter().map(|r| strip_sgr(r)).collect();
        assert!(
            rows[0].starts_with("b1  ") && rows[0].ends_with("  sleep 30"),
            "{rows:?}"
        );
        assert!(
            rows[1].starts_with("b2  ") && rows[1].ends_with("  sleep 31"),
            "{rows:?}"
        );
        assert_eq!(live.shown(1).map(|j| j.id), Some("b2".to_owned()));
        assert_eq!(live.shown(2), None);

        // b1 is killed: the next read has one row, keyed b2 — and row 0 is b2 now.
        let b1 = live.shown(0).expect("b1");
        jobs.kill("b1");
        running(&jobs, 1).await;
        let second = live.refresh()();
        assert_eq!(second.keys, ["b2"]);
        assert_eq!(second.prompt.as_deref(), Some("1 job running"));
        assert_eq!(second.rows.len(), 1);
        assert_eq!(live.shown(0).map(|j| j.id), Some("b2".to_owned()));

        // The page open on b1: its clock line is the verdict now, and the rest is what it was.
        let page: Vec<String> = page_lines(&jobs, &b1, None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(page[0], "command:  sleep 30");
        assert_eq!(page[1], "finished: killed");
        assert!(page[2].starts_with("pid:      "), "{page:?}");
        assert_eq!(page[4], "── last 20 lines ──");
        // …and on b2, still running, the clock.
        let b2 = live.shown(0).expect("b2");
        let page: Vec<String> = page_lines(&jobs, &b2, None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert!(
            page[1].starts_with("running:  ") && page[1].contains("(started "),
            "{page:?}"
        );

        // A job that ends on its own with output: `exit 0`, the tail under the rule.
        jobs.spawn(&opts("echo all green")).expect("spawn");
        let b3 = live.refresh()();
        let b3 = live.shown(b3.keys.len() - 1).expect("b3");
        assert_eq!(b3.id, "b3");
        running(&jobs, 1).await;
        let page: Vec<String> = page_lines(&jobs, &b3, None)
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert!(page[1].starts_with("finished: exit 0 after "), "{page:?}");
        assert_eq!(page[5], "all green");

        jobs.kill_all();
    }
}
