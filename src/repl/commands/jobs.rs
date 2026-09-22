//! `/jobs` — the background jobs running right now, one row each.
//!
//! A read-only `View` panel in the `/tools` shape (one tab, wrapped), over a SNAPSHOT of the registry at the
//! moment it opens:
//! the rows do not tick (the status row's job segment does — that is where a running clock belongs). The
//! command exists only while a job runs (`CmdFlags::jobs`, flipped by the registry's watch), so an empty
//! panel is a race the row still answers rather than a state the user can reach on purpose.

use std::path::Path;
use std::time::Instant;

use crate::shell::jobs::JobInfo;
use crate::text::clock;
use crate::text::width::str_width;
use crate::ui::facade::{Panel, TabbedSpec};

use crate::repl::render::banner::tilde;
use crate::repl::render::styles::{bold, dim, truncate_runes};
use crate::repl::run::Repl;

/// Runes of the command column past which a command line is cut with `…`, so the output path beside it
/// stays in view.
const COMMAND_RUNES: usize = 60;

/// The panel's rows: a count, then one row per job — `b3  1m12s  cargo test --test session resume
/// ~/.iota/jobs/b3.log` — the id and the clock padded to their widest, the command on one line and cut
/// past [`COMMAND_RUNES`], the log with `home` shortened to `~`. Oldest first, as the registry lists them.
pub(crate) fn job_rows(jobs: &[JobInfo], now: Instant, home: Option<&Path>) -> Vec<String> {
    if jobs.is_empty() {
        return vec![dim("No background jobs running.")];
    }
    let clocks: Vec<String> = jobs
        .iter()
        .map(|j| clock(now.saturating_duration_since(j.started)))
        .collect();
    let id_width = jobs.iter().map(|j| str_width(&j.id)).max().unwrap_or(0);
    let clock_width = clocks.iter().map(|c| str_width(c)).max().unwrap_or(0);
    let plural = if jobs.len() == 1 { "job" } else { "jobs" };
    let mut rows = Vec::with_capacity(jobs.len() + 1);
    rows.push(dim(&format!("{} {plural} running", jobs.len())));
    for (job, clock) in jobs.iter().zip(&clocks) {
        let command: String = job.command.split_whitespace().collect::<Vec<_>>().join(" ");
        rows.push(format!(
            "{}  {clock:>clock_width$}  {}  {}",
            bold(&format!("{:<id_width$}", job.id)),
            truncate_runes(&command, COMMAND_RUNES),
            dim(&tilde(&job.output_path, home)),
        ));
    }
    rows
}

/// `/jobs`: one wrapped view — a log path under a long temp directory outruns a narrow terminal, and the
/// path is the one thing on the row the user may want to copy.
pub(crate) async fn cmd_jobs(repl: &Repl) {
    let lines = job_rows(
        &repl.handles.jobs.snapshot(),
        Instant::now(),
        repl.conv.agent.home.as_deref(),
    );
    let spec = TabbedSpec {
        panels: vec![Panel::view("Jobs".to_owned(), lines).with_wrap(true)],
        ..TabbedSpec::default()
    };
    let _ = repl.handles.ui.tabbed(&repl.handles.cancel, spec).await;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::shell::jobs::JobInfo;
    use crate::text::ansi::strip_sgr;

    use super::job_rows;

    fn job(id: &str, command: &str, ago: u64, now: Instant) -> JobInfo {
        JobInfo {
            id: id.to_owned(),
            command: command.to_owned(),
            started: now.checked_sub(Duration::from_secs(ago)).unwrap_or(now),
            output_path: PathBuf::from(format!("/home/u/.cache/iota-jobs/77/{id}.log")),
        }
    }

    // The four columns, aligned to the widest id and clock, the command on one line and cut, the log under
    // `~` — and the count above them.
    #[test]
    fn rows_are_aligned_and_cut() {
        let now = Instant::now();
        let long =
            "cargo test --test session resume --features everything-under-the-sun -- --nocapture";
        let jobs = [
            job("b3", "cargo test\n  --test session resume", 72, now),
            job("b12", long, 3725, now),
        ];
        let rows: Vec<String> = job_rows(&jobs, now, Some(std::path::Path::new("/home/u")))
            .iter()
            .map(|r| strip_sgr(r))
            .collect();
        assert_eq!(rows[0], "2 jobs running");
        // `tilde` joins with the platform separator (the banner does the same), so the expected
        // rows are built with it: `~/.cache/…` here, `~\.cache/…` on Windows.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            rows[1],
            format!(
                "b3      1m12s  cargo test --test session resume  ~{sep}.cache/iota-jobs/77/b3.log"
            )
        );
        assert_eq!(
            rows[2],
            format!(
                "b12  1h02m05s  cargo test --test session resume --features everything-under…  ~{sep}.cache/iota-jobs/77/b12.log"
            )
        );
        // One job, no home: the path stays whole and the count is singular.
        let rows: Vec<String> = job_rows(&jobs[..1], now, None)
            .iter()
            .map(|r| strip_sgr(r))
            .collect();
        assert_eq!(rows[0], "1 job running");
        assert_eq!(
            rows[1],
            "b3  1m12s  cargo test --test session resume  /home/u/.cache/iota-jobs/77/b3.log"
        );
        // A clock cannot run backwards: a job "started" after `now` reads 0s.
        let future = [job("b1", "x", 0, now + Duration::from_secs(5))];
        assert!(strip_sgr(&job_rows(&future, now, None)[1]).starts_with("b1  0s  x"));
    }

    // Nothing running (the race between the row and the registry) says so.
    #[test]
    fn no_jobs_is_one_dim_line() {
        let rows = job_rows(&[], Instant::now(), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(strip_sgr(&rows[0]), "No background jobs running.");
    }
}
