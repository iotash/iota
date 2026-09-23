//! `/jobs` — the background jobs running right now: a single-select list, one row per job, and a read-only
//! detail page for the one picked.
//!
//! The list (`Panel::list`, the `/session` Resume shape, a search past the fold) is a SNAPSHOT of the
//! registry at the moment it opens: the rows do not tick (the status row's job segment does — that is where
//! a running clock belongs), and a row's command is the header's label ([`crate::text::header_command`]),
//! the text the `[shell …]` row, the notice and the status row show. Enter opens the detail page — a `View`
//! titled `job b3`: the command in full, the clock with the wall-clock start, the pid, the log path and the
//! log's last twenty lines — and Esc there returns to the list, re-read, until Esc closes the list. The
//! command exists only while a job runs (`CmdFlags::jobs`, flipped by the registry's watch), so an empty set
//! is the race between the row and the registry, answered with one notice rather than an empty panel.

use std::path::Path;
use std::time::Instant;

use crate::shell::exec::read_capped;
use crate::shell::jobs::JobInfo;
use crate::text::width::str_width;
use crate::text::{clock, header_command};
use crate::ui::facade::{Panel, TabbedSpec};

use crate::repl::render::banner::tilde;
use crate::repl::render::styles::{bold, dim};
use crate::repl::run::Repl;

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

/// The detail page for one job, `tail` being its log's last lines ([`log_tail`]):
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
/// back from); a pid the OS never reported reads `(unknown)`, an empty tail `(no output yet)`.
pub(crate) fn job_detail(
    job: &JobInfo,
    now: Instant,
    wall_now: &jiff::Zoned,
    home: Option<&Path>,
    tail: &[String],
) -> Vec<String> {
    let label = |name: &str| format!("{}  ", dim(&format!("{name:<8}")));
    let elapsed = now.saturating_duration_since(job.started);
    let started = jiff::SignedDuration::try_from(elapsed)
        .ok()
        .and_then(|d| wall_now.checked_sub(d).ok())
        .map_or_else(|| "?".to_owned(), |at| at.strftime("%H:%M:%S").to_string());
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
    lines.push(format!(
        "{}{} (started {started})",
        label("running:"),
        clock(elapsed)
    ));
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

/// `/jobs`: the list, then the page for the row picked, then the list again — until the list is left
/// with Esc, or the facade goes away. Each list is a fresh snapshot, so a job that ended while its page
/// was open is simply not offered again.
pub(crate) async fn cmd_jobs(repl: &Repl) {
    loop {
        let jobs = repl.handles.jobs.snapshot();
        if jobs.is_empty() {
            repl.handles.tr.notice(NO_JOBS);
            return;
        }
        let list = TabbedSpec {
            panels: vec![
                Panel::list("Jobs", job_rows(&jobs, Instant::now()))
                    .with_prompt(job_count(jobs.len()))
                    .with_search(true),
            ],
            ..TabbedSpec::default()
        };
        let Ok(r) = repl.handles.ui.tabbed(&repl.handles.cancel, list).await else {
            return;
        };
        if r.cancelled {
            return;
        }
        let cursor = r.panels.first().map_or(0, |p| p.cursor);
        let Some(job) = jobs.get(cursor) else {
            return;
        };
        let lines = job_detail(
            job,
            Instant::now(),
            &jiff::Zoned::now(),
            repl.conv.agent.home.as_deref(),
            &log_tail(&job.output_path),
        );
        let page = TabbedSpec {
            panels: vec![Panel::view(format!("job {}", job.id), lines).with_wrap(true)],
            ..TabbedSpec::default()
        };
        // Esc (or any close) on the page goes back to the list; only the facade going away ends this.
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

    use crate::shell::jobs::JobInfo;
    use crate::text::ansi::strip_sgr;

    use super::{TAIL_LINES, job_count, job_detail, job_rows, log_tail};

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
    // wall-clock start, the pid, the log under `~`, then the tail.
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
        let lines: Vec<String> = job_detail(
            &job,
            now,
            &wall,
            Some(std::path::Path::new("/home/u")),
            &tail,
        )
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
        let lines: Vec<String> = job_detail(&bare, now, &wall, None, &[])
            .iter()
            .map(|l| strip_sgr(l))
            .collect();
        assert_eq!(lines[3], "pid:      (unknown)");
        assert_eq!(lines[4], "output:   /home/u/.cache/iota-jobs/77/b3.log");
        assert_eq!(lines[6], "(no output yet)");
        assert_eq!(lines.len(), 7);
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
}
