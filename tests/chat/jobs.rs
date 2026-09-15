//! Background jobs in a HEADLESS run (`chat/run.rs`): a `-m` run has no idle loop for a finished job to
//! wake, so the tool loop itself waits for one and injects its notice as another round.

use std::sync::Arc;

use iota::chat::turns::RunCtx;
use iota::chat::{QuietHost, RunRequest, run_once};
use iota::provider::RoundResult;
use iota::provider::model::{JsonObject, Message, Role, ToolCall};
use iota::shell::jobs::Jobs;
use iota::testing::{FakeProvider, Round};
use iota::tool::sets::{RawNode, ToolsConfig};
use iota::tool::{Dispatcher, Env, Registry};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

/// A tool provider playing `rounds`, then `"nothing left to do"`, recording the history of every request.
fn recorder(rounds: Vec<RoundResult>) -> FakeProvider {
    FakeProvider::new()
        .with_tools()
        .with_model("fake")
        .rounds(rounds.into_iter().map(Round::result))
        .replying("nothing left to do")
}

/// A `shell` dispatcher over a temp project with a job registry bound — the real toolset, so the test
/// exercises the same path the binary does.
fn shell_over_jobs() -> (TempDir, Arc<Jobs>, Arc<dyn Dispatcher>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let jobs = Jobs::new(dir.path());
    let env = Env {
        project_root: Some(dir.path().to_path_buf()),
        jobs: Some(Arc::clone(&jobs)),
        ..Env::default()
    };
    let node: RawNode =
        serde_norway::from_str("sandbox: off\nauto_run: true\n").expect("shell config");
    let mut cfg = ToolsConfig::new();
    cfg.insert("shell".to_owned(), node);
    let registry = Registry::build(&env, &cfg, &mut |w| panic!("the shell set complained: {w}"));
    (dir, jobs, Arc::new(registry))
}

/// Whether to skip a test whose command line is POSIX. The interpreter is `bash` on Unix and, on Windows,
/// whatever `shell::interp`'s ladder found — under PowerShell or `cmd.exe` these scripts would not parse, so
/// the test prints a `SKIP:` line instead (the twin of `tests/tool/shell.rs::skip_unless_posix`).
fn skip_unless_posix(test: &str) -> bool {
    let shell =
        iota::shell::interp::resolve().expect("this machine has no shell interpreter at all");
    if shell.is_posix() {
        return false;
    }
    println!(
        "SKIP: {test} — the resolved interpreter is {}, not a POSIX shell",
        shell.program.display()
    );
    true
}

/// One `shell` call with `background: true`.
fn background_call(id: &str, command: &str) -> ToolCall {
    let mut args = JsonObject::new();
    args.insert("command".to_owned(), serde_json::Value::from(command));
    args.insert("background".to_owned(), serde_json::Value::from(true));
    ToolCall {
        id: id.to_owned(),
        name: "shell".to_owned(),
        arguments: args,
    }
}

// New (DIVERGENCES X-08): the model starts a job, answers, and the run does NOT end — the loop blocks for
// the job, injects its completion notice and gives the model another round with it.
#[tokio::test]
async fn a_headless_run_waits_for_its_background_job_and_reports_it() {
    if skip_unless_posix("a_headless_run_waits_for_its_background_job_and_reports_it") {
        return;
    }
    let (_dir, jobs, dispatch) = shell_over_jobs();
    let p = recorder(vec![
        // Round 1: start the job.
        RoundResult {
            tool_calls: vec![background_call("c1", "sleep 0.2; echo all green")],
            ..RoundResult::default()
        },
        // Round 2: the model has nothing else to do — this is where the wait happens.
        RoundResult {
            content: "started it".to_owned(),
            ..RoundResult::default()
        },
        // Round 3 (after the notice): the final answer.
        RoundResult {
            content: "the job finished".to_owned(),
            ..RoundResult::default()
        },
    ]);
    let mut host = QuietHost::new();
    host.jobs = Some(Arc::clone(&jobs));

    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "run the tests in the background".to_owned(),
            ..RunRequest::default()
        },
        Arc::clone(&dispatch),
        None,
        &mut host,
        None,
    )
    .await
    .expect("the run failed");

    assert_eq!(out.reply, "the job finished");
    // The third request is the one that carries the notice, right after the reply that preceded the wait.
    let third = p.send(2);
    let notice = third
        .iter()
        .find(|m| m.is_notice())
        .expect("no notice reached the model");
    assert_eq!(notice.role(), Role::User, "a notice rides the user role");
    assert!(
        notice
            .content
            .starts_with("[background job b1 finished: exit 0 after "),
        "{:?}",
        notice.content
    );
    assert!(
        notice.content.ends_with("all green\n"),
        "{:?}",
        notice.content
    );
    // The reply the model gave BEFORE the wait is still part of the conversation.
    assert!(
        third
            .iter()
            .any(|m| m.role() == Role::Assistant && m.content == "started it"),
        "the pre-wait reply was dropped: {third:#?}"
    );
    // …and the whole exchange is in the persisted delta, notice included.
    assert!(out.delta.iter().any(Message::is_notice));
    assert_eq!(jobs.running(), 0);
}

// A run with nothing in flight ends exactly as it always did: the wait is not a new way to hang.
#[tokio::test]
async fn a_run_with_no_jobs_ends_on_the_first_reply() {
    let (_dir, jobs, dispatch) = shell_over_jobs();
    let p = recorder(vec![RoundResult {
        content: "done".to_owned(),
        ..RoundResult::default()
    }]);
    let mut host = QuietHost::new();
    host.jobs = Some(jobs);

    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "hi".to_owned(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut host,
        None,
    )
    .await
    .expect("the run failed");
    assert_eq!(out.reply, "done");
    assert!(!out.delta.iter().any(Message::is_notice));
    assert_eq!(p.send(1).len(), 0, "there must be no second request");
}

// A job that lands WHILE a tool round is running enters at the top of the next round, without the loop
// having to wait for it.
#[tokio::test]
async fn a_job_that_lands_mid_round_enters_at_the_next_round() {
    if skip_unless_posix("a_job_that_lands_mid_round_enters_at_the_next_round") {
        return;
    }
    let (_dir, jobs, dispatch) = shell_over_jobs();
    let mut noop = JsonObject::new();
    noop.insert(
        "command".to_owned(),
        serde_json::Value::from("sleep 0.3; true"),
    );
    let p = recorder(vec![
        RoundResult {
            tool_calls: vec![background_call("c1", "true")],
            ..RoundResult::default()
        },
        // A foreground call long enough for the job above to finish underneath it.
        RoundResult {
            tool_calls: vec![ToolCall {
                id: "c2".to_owned(),
                name: "shell".to_owned(),
                arguments: noop,
            }],
            ..RoundResult::default()
        },
        RoundResult {
            content: "done".to_owned(),
            ..RoundResult::default()
        },
    ]);
    let mut host = QuietHost::new();
    host.jobs = Some(Arc::clone(&jobs));

    let out = run_once(
        &RunCtx::default(),
        &p,
        &RunRequest {
            message: "go".to_owned(),
            ..RunRequest::default()
        },
        dispatch,
        None,
        &mut host,
        None,
    )
    .await
    .expect("the run failed");
    assert_eq!(out.reply, "done");
    // Round 3's request carries the notice, and the run never had to block for it.
    assert!(
        p.send(2).iter().any(Message::is_notice),
        "the notice missed the round it should have entered: {:#?}",
        p.send(2)
    );
    assert_eq!(jobs.running(), 0);
}
