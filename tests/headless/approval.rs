//! The approval gate of the quiet loop (`chat/approval_test.go`): refusal with nobody to ask, forwarding to an
//! injected approver, and the shape of `QuietHost::ask_approval`.

use std::sync::{Arc, Mutex};

use iota::headless::run::refusal_text;
use iota::headless::{QuietHost, TurnParams, execute_with_tools};
use iota::provider::model::{JsonObject, Message, Role, ToolCall};
use iota::testing::{FakeProvider, GatedDispatch, Round, lock};
use iota::tool::context::RunCtx;
use iota::tool::{Approval, Dispatcher};

/// A store whose static level holds the built-in `tools` — what an agent's `allow:` gives a run.
fn allowing(tools: &[&str]) -> Arc<iota::tool::grants::Grants> {
    let mut fixed = iota::tool::grants::GrantSet::default();
    for t in tools {
        fixed.add_tool(iota::tool::grants::ToolKey::Builtin((*t).to_owned()));
    }
    Arc::new(iota::tool::grants::Grants::with_static(fixed))
}

/// Asks for `write_file` (with `args`) once, then answers `saw: ` + the last history entry's content — so a
/// test can assert on what the model was actually told.
fn writing(args: JsonObject) -> FakeProvider {
    FakeProvider::new()
        .with_tools()
        .answering(move |n, messages| {
            if n == 1 {
                return Round::calls(vec![ToolCall {
                    id: "c1".to_owned(),
                    name: "write_file".to_owned(),
                    arguments: args.clone(),
                }]);
            }
            let last = messages
                .last()
                .map(|m| m.content.as_str())
                .unwrap_or_default();
            Round::reply(&format!("saw: {last}"))
        })
}

/// The `write_file` call names the file it would write.
fn writing_path(path: &str) -> FakeProvider {
    let mut args = JsonObject::new();
    args.insert("path".to_owned(), serde_json::Value::from(path));
    writing(args)
}

/// Runs the gated round trip: `write_file` requested once, then the model echoes the last history entry.
async fn run_gated(host: &mut QuietHost) -> (String, Arc<GatedDispatch>, Vec<Message>) {
    let d = Arc::new(GatedDispatch::new());
    let mut history = vec![Message::user("go")];
    let outcome = execute_with_tools(
        TurnParams {
            cx: &RunCtx::default(),
            tp: &writing(JsonObject::new()),
            dispatch: d.clone(),
            tools: d.tools(),
            harness: "",
            overlay: "",
            max_turns: None,
        },
        &mut history,
        host,
    )
    .await
    .expect("loop failed");
    (outcome.content, d, history)
}

#[tokio::test]
async fn the_quiet_loop_refuses_when_there_is_nobody_to_ask() {
    // With nobody to ask, the loop refuses and says how to enable the call. That is the -m contract.
    let mut host = QuietHost::new();
    let (reply, d, history) = run_gated(&mut host).await;
    assert_eq!(d.ran(), 0, "a gated tool ran with no approval");
    assert!(
        reply.contains("--allow write_file"),
        "the refusal must name the way to enable it, got {reply:?}"
    );
    assert_eq!(reply, format!("saw: {}", refusal_text("write_file")));
    // The refusal is fed back as an IsError tool result answering the call.
    let result = &history[2];
    assert_eq!(result.role(), Role::Tool);
    assert!(result.is_error());
    assert_eq!(result.tool_call_id(), "c1");
    assert_eq!(result.tool_call_name(), "write_file");
    assert_eq!(result.content, refusal_text("write_file"));
    assert_eq!(host.rec.round_count(), 2);
}

#[tokio::test]
async fn the_quiet_loop_forwards_approval_to_the_injected_approver() {
    // A headless loop has no user of its own, so an injected approver is what decides a gated call: the
    // question travels up and the answer decides it.
    let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let mut host = QuietHost {
        approve: Some(Box::new(move |tc: &ToolCall, detail: &str| {
            lock(&seen).push((tc.name.clone(), detail.to_owned()));
            Approval::Allow
        })),
        ..QuietHost::new()
    };
    let (reply, d, history) = run_gated(&mut host).await;
    // A dispatcher with no `header_summary` of its own hands the gate an empty detail; the
    // capability-bearing shape is `test_forwarded_approval_carries_the_call_detail` below.
    assert_eq!(
        lock(&asked).as_slice(),
        &[("write_file".to_owned(), String::new())]
    );
    assert_eq!(d.ran(), 1, "tool ran {} times, want 1", d.ran());
    assert!(
        reply.contains("written"),
        "the model should have seen the result, got {reply:?}"
    );
    assert_eq!(history[2].content, "written");
    assert!(!history[2].is_error());
}

#[tokio::test]
async fn a_denied_call_does_not_end_the_run() {
    // A denial is a result the model reads, not an aborted turn: the child carries on and reports back.
    let mut host = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            Approval::Deny("The user declined this call.".to_owned())
        })),
        ..QuietHost::new()
    };
    let (reply, d, history) = run_gated(&mut host).await;
    assert_eq!(d.ran(), 0, "a denied tool ran anyway");
    assert!(
        reply.contains("declined"),
        "the model must be told it was declined, got {reply:?}"
    );
    assert_eq!(history[2].content, "The user declined this call.");
    assert!(history[2].is_error());
}

#[test]
fn the_quiet_host_answers_approval_from_its_approver_or_refuses() {
    // The refusal text is the call's result either way, so a failing prompt must not be mistaken for consent.
    let h = QuietHost::new();
    let tc = ToolCall {
        name: "edit_file".to_owned(),
        ..ToolCall::default()
    };
    let Approval::Deny(why) = h.ask_approval(&tc, "path:x", None) else {
        panic!("a refusal was expected")
    };
    assert!(why.contains("edit_file"), "nil approver = {why:?}");
    assert_eq!(why, refusal_text("edit_file"));
    assert_eq!(
        why,
        "edit_file was not executed: it needs the user's approval, which this non-interactive run cannot ask for; pass --allow edit_file, or list it in the agent's `allow:`, to permit it here."
    );
    // The dispatcher's own refusal (an MCP server's, naming its `auto_run`) replaces the generic text.
    assert_eq!(
        h.ask_approval(&tc, "path:x", Some("own words".to_owned())),
        Approval::Deny("own words".to_owned())
    );

    let h = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| {
            Approval::Deny("prompt broke".to_owned())
        })),
        ..QuietHost::new()
    };
    let Approval::Deny(why) = h.ask_approval(&tc, "path:x", None) else {
        panic!("a refusal was expected")
    };
    assert_eq!(
        why, "prompt broke",
        "failed prompt must be a refusal carrying the reason"
    );

    // An approver's answer travels verbatim, detail included.
    let h = QuietHost {
        approve: Some(Box::new(|tc: &ToolCall, detail: &str| {
            Approval::Deny(format!("{}:{detail}", tc.name))
        })),
        ..QuietHost::new()
    };
    assert_eq!(
        h.ask_approval(&tc, "path:x", None),
        Approval::Deny("edit_file:path:x".to_owned())
    );
    let h = QuietHost {
        approve: Some(Box::new(|_tc: &ToolCall, _detail: &str| Approval::Allow)),
        ..QuietHost::new()
    };
    assert_eq!(h.ask_approval(&tc, "path:x", None), Approval::Allow);
}

// The forwarded prompt has to say what the call is ABOUT: a gate naming only the tool asks the user to authorize "write_file"
// without saying which file. (Portable since T-30 gave the built-in code/shell sets real
// `header_summary` capabilities — D-12 is closed.)
#[tokio::test]
async fn a_forwarded_approval_carries_the_call_detail() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut host = QuietHost {
        approve: Some(Box::new(move |_tc: &ToolCall, detail: &str| {
            lock(&sink).push(detail.to_owned());
            Approval::Deny("The user declined this call.".to_owned())
        })),
        ..QuietHost::new()
    };
    let d = Arc::new(GatedDispatch::with_header());
    let mut history = vec![Message::user("go")];
    execute_with_tools(
        TurnParams {
            cx: &RunCtx::default(),
            tp: &writing_path("src/ui/model.rs"),
            dispatch: d.clone(),
            tools: d.tools(),
            harness: "",
            overlay: "",
            max_turns: None,
        },
        &mut history,
        &mut host,
    )
    .await
    .expect("loop failed");
    assert_eq!(lock(&seen).as_slice(), ["src/ui/model.rs"]);
    assert_eq!(d.ran(), 0);
}

/// The header half of the detail split (`chat/approval_test.go:171`): one implementation, two
/// readers — the transcript's `[name detail]` header and the gate's bare detail.
mod header_split {
    use iota::tool::Dispatcher;
    use iota::tool::fmt::tool_call_header;
    use pretty_assertions::assert_eq;

    use iota::testing::{GatedDispatch, tool_call_with};

    /// `provider.ToolCall{Name: n, Arguments: {k: v}…}` — the id is irrelevant to a header.
    fn call(name: &str, args: &[(&str, &str)]) -> iota::provider::model::ToolCall {
        tool_call_with("c1", name, args)
    }

    // The header keeps its shape after the detail was split out of it.
    #[test]
    fn the_tool_call_header_is_unchanged_by_the_detail_split() {
        let detail = GatedDispatch::with_header();
        let tc = call("write_file", &[("path", "a/b.rs")]);
        assert_eq!(
            tool_call_header(&detail as &dyn Dispatcher, &tc),
            "[write_file a/b.rs]"
        );

        // A tool with no summary of its own falls back to the argument digest, and an empty summary
        // renders as a bare name rather than the digest.
        let plain = GatedDispatch::new();
        assert_eq!(
            tool_call_header(&plain as &dyn Dispatcher, &tc),
            "[write_file path:a/b.rs]"
        );
        assert_eq!(
            tool_call_header(&plain as &dyn Dispatcher, &call("x", &[])),
            "[x]"
        );
        // The capability's OWN empty answer is a bare name too — `edit_file`'s `new_string` must
        // never reach a header through the digest fallback (tool/codepath_test.go:96).
        assert_eq!(
            tool_call_header(
                &detail as &dyn Dispatcher,
                &call("write_file", &[("new_string", "x")])
            ),
            "[write_file]"
        );
    }
}

/// The headless matrix over the real `code` set (`-m`, the SDK path: no interactor): inside the root the
/// writers' `allow:` decides (absent → the quiet refusal naming `--allow`, present → written); outside it
/// nothing is ever written, allowed or not, and even an injected approver that allows everything is never asked
/// — the refusal names `--allow` and `allow:`.
#[tokio::test]
async fn the_quiet_loop_never_writes_outside_the_roots() {
    use iota::tool::sets::ToolsConfig;
    use iota::tool::{Registry, ToolEnv};

    for writers in [false, true] {
        for approver in [false, true] {
            let at = format!("writers={writers} approver={approver}");
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().join("proj");
            std::fs::create_dir(&root).expect("root");
            let grants = allowing(if writers {
                &["edit_file", "write_file"]
            } else {
                &[]
            });
            let env = ToolEnv {
                project_root: Some(root.clone()),
                grants: Arc::clone(&grants),
                ..ToolEnv::default()
            };
            let raw: ToolsConfig = serde_norway::from_str("code:\n").expect("tools");
            let d = Arc::new(Registry::build(&env, &raw, &mut |w| panic!("{w}")));
            let asked = Arc::new(Mutex::new(0_usize));
            let seen = Arc::clone(&asked);
            let mut host = QuietHost {
                grants,
                approve: approver.then(|| {
                    Box::new(move |_: &ToolCall, _: &str| {
                        *lock(&seen) += 1;
                        Approval::Allow
                    }) as Box<dyn Fn(&ToolCall, &str) -> Approval + Send + Sync>
                }),
                ..QuietHost::new()
            };
            for (path, inside) in [("in.txt", true), ("../out.txt", false)] {
                let mut args = JsonObject::new();
                args.insert("path".to_owned(), path.into());
                args.insert("content".to_owned(), "x".into());
                let mut history = vec![Message::user("go")];
                *lock(&asked) = 0;
                execute_with_tools(
                    TurnParams {
                        cx: &RunCtx::default(),
                        tp: &writing(args),
                        dispatch: d.clone(),
                        tools: d.tools(),
                        harness: "",
                        overlay: "",
                        max_turns: None,
                    },
                    &mut history,
                    &mut host,
                )
                .await
                .expect("loop");
                let result = &history[2];
                let landed = if inside {
                    root.join("in.txt")
                } else {
                    dir.path().join("out.txt")
                };
                if inside {
                    let written = writers || approver;
                    assert_eq!(landed.exists(), written, "{at} {path}");
                    assert_eq!(*lock(&asked), usize::from(!writers && approver), "{at}");
                    if !written {
                        assert_eq!(result.content, refusal_text("write_file"), "{at}");
                    }
                } else {
                    assert!(!landed.exists(), "{at}: an outside write landed");
                    assert_eq!(
                        *lock(&asked),
                        0,
                        "{at}: nobody is asked about an outside write"
                    );
                    assert!(result.is_error(), "{at}");
                    let want = format!(
                        "path is outside the project root ({}): ../out.txt — writing there needs the user's approval, which this non-interactive run cannot ask for; pass --allow <dir>, or list its directory in the agent's `allow:`, to allow it",
                        root.display()
                    );
                    assert_eq!(result.content, want, "{at}");
                }
            }
        }
    }
}

/// `--allow <dir>` is the static level of the run's store, resolved as the assembly resolves it — against the
/// working directory, not the project root — so an unattended run (`-m`, the SDK path: nobody to ask) writes
/// into it unasked, while a sibling outside it is refused with the sentence that names the flag.
#[tokio::test]
async fn an_outside_write_names_the_allow_flag() {
    use iota::app::HostDirs;
    use iota::cmd::{Config, flag_grants, static_grants};
    use iota::tool::grants::{GrantSet, Grants};
    use iota::tool::{Registry, ToolEnv};

    let dir = tempfile::tempdir().expect("tempdir");
    let real = iota::app::fs::physical(dir.path()).expect("real");
    let root = real.join("proj");
    let cwd = root.join("sub");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let added = real.join("added");
    std::fs::create_dir(&added).expect("added");
    let cfg = Config::parse(
        b"agents:\n  coder:\n    tools: {code: }\n    allow: [edit_file, write_file]\n",
        &iota::app::env::Env::default(),
        &mut |_| {},
    )
    .expect("the config loads");
    let agent = &cfg.agents["coder"];
    let env = ToolEnv {
        project_root: Some(root.clone()),
        dirs: HostDirs {
            cwd: Some(cwd),
            ..HostDirs::default()
        },
        ..ToolEnv::default()
    };
    let config = static_grants(&cfg, "coder", agent, &env).expect("resolves");
    let flag = flag_grants(&cfg, agent, &["../../added".to_owned()], &[], &config, &env)
        .expect("resolves");
    assert_eq!(flag.dirs, std::slice::from_ref(&added));
    let mut fixed: GrantSet = config;
    fixed.dirs.extend(flag.dirs);
    let grants = Arc::new(Grants::with_static(fixed));
    let env = ToolEnv {
        grants: Arc::clone(&grants),
        ..env
    };
    let d = Arc::new(Registry::build(&env, &agent.tools, &mut |w| panic!("{w}")));
    let outside = real.join("out.txt");
    for (target, lands) in [(added.join("in.txt"), true), (outside.clone(), false)] {
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), target.display().to_string().into());
        args.insert("content".to_owned(), "x".into());
        let mut history = vec![Message::user("go")];
        execute_with_tools(
            TurnParams {
                cx: &RunCtx::default(),
                tp: &writing(args),
                dispatch: d.clone(),
                tools: d.tools(),
                harness: "",
                overlay: "",
                max_turns: None,
            },
            &mut history,
            &mut QuietHost {
                grants: Arc::clone(&grants),
                ..QuietHost::new()
            },
        )
        .await
        .expect("loop");
        assert_eq!(
            target.exists(),
            lands,
            "{}: {}",
            target.display(),
            history[2].content
        );
        assert_eq!(history[2].is_error(), !lands, "{}", history[2].content);
        if !lands {
            assert_eq!(
                history[2].content,
                format!(
                    "path is outside the project root ({}) and the extra write roots ({}): {} — writing there \
                     needs the user's approval, which this non-interactive run cannot ask for; pass --allow <dir>, \
                     or list its directory in the agent's `allow:`, to allow it",
                    root.display(),
                    added.display(),
                    outside.display()
                )
            );
        }
    }
}

/// A project grant is the one approval an unattended run inherits: the project's `grants.json`, loaded into the
/// run's store as every start loads it, makes the granted directory a root — the `-m` loop writes there with
/// nobody to ask — while a sibling directory is still refused, and a second project that shares the bucket
/// inherits nothing.
#[tokio::test]
async fn a_project_grant_runs_unattended() {
    use iota::session::SessionStore;
    use iota::tool::sets::ToolsConfig;
    use iota::tool::{Registry, ToolEnv};

    let dir = tempfile::tempdir().expect("tempdir");
    let real = iota::app::fs::physical(dir.path()).expect("real");
    let root = real.join("a/b/proj");
    let twin = real.join("a-b/proj"); // the same bucket slug
    let lib = real.join("lib");
    for d in [&root, &twin, &lib, &real.join("sibling")] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let store = SessionStore::new(real.join("sessions"));
    store
        .update_project_grants(&root, |s| s.dirs.push(lib.clone()))
        .expect("a project grant");

    let run = |project: &std::path::Path, path: std::path::PathBuf| {
        // `allow: [edit_file, write_file]`: the project's directory grant is the one thing under test.
        let grants = allowing(&["edit_file", "write_file"]);
        grants.replace_project(store.project_grants(project).expect("read"));
        let env = ToolEnv {
            project_root: Some(project.to_path_buf()),
            grants: Arc::clone(&grants),
            ..ToolEnv::default()
        };
        let raw: ToolsConfig = serde_norway::from_str("code:\n").expect("tools");
        let d = Arc::new(Registry::build(&env, &raw, &mut |w| panic!("{w}")));
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), path.to_str().expect("utf-8").into());
        args.insert("content".to_owned(), "x".into());
        async move {
            let mut history = vec![Message::user("go")];
            execute_with_tools(
                TurnParams {
                    cx: &RunCtx::default(),
                    tp: &writing(args),
                    dispatch: d.clone(),
                    tools: d.tools(),
                    harness: "",
                    overlay: "",
                    max_turns: None,
                },
                &mut history,
                &mut QuietHost {
                    grants,
                    ..QuietHost::new()
                },
            )
            .await
            .expect("loop");
            history[2].clone()
        }
    };

    let result = run(&root, lib.join("deep/x.txt")).await;
    assert!(!result.is_error(), "{}", result.content);
    assert!(lib.join("deep/x.txt").exists(), "the granted write landed");

    let result = run(&root, real.join("sibling/x.txt")).await;
    assert!(result.is_error());
    assert!(!real.join("sibling/x.txt").exists());

    let result = run(&twin, lib.join("twin.txt")).await;
    assert!(result.is_error(), "another project's grant applied");
    assert!(!lib.join("twin.txt").exists());
}

/// A built-in tool's project grant is inherited by an unattended run: `{"builtin": "write_file"}` in the project's
/// `grants.json`, loaded into the run's store, lets the `-m` loop make an inside write that would otherwise be
/// put to the user — while a project without the grant is refused, and the grant never answers an
/// outside write.
#[tokio::test]
async fn a_project_builtin_grant_runs_unattended() {
    use iota::session::SessionStore;
    use iota::tool::grants::{Grants, ToolKey};
    use iota::tool::sets::ToolsConfig;
    use iota::tool::{Registry, ToolEnv};

    let dir = tempfile::tempdir().expect("tempdir");
    let real = iota::app::fs::physical(dir.path()).expect("real");
    let (root, other, out) = (real.join("proj"), real.join("other"), real.join("out"));
    for d in [&root, &other, &out] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let store = SessionStore::new(real.join("sessions"));
    store
        .update_project_grants(&root, |s| {
            s.tools.push(ToolKey::Builtin("write_file".to_owned()));
        })
        .expect("a project grant");

    let run = |project: &std::path::Path, path: std::path::PathBuf| {
        let grants = Arc::new(Grants::default());
        grants.replace_project(store.project_grants(project).expect("read"));
        let env = ToolEnv {
            project_root: Some(project.to_path_buf()),
            grants: Arc::clone(&grants),
            ..ToolEnv::default()
        };
        let raw: ToolsConfig = serde_norway::from_str("code: {}\n").expect("tools");
        let d = Arc::new(Registry::build(&env, &raw, &mut |w| panic!("{w}")));
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), path.to_str().expect("utf-8").into());
        args.insert("content".to_owned(), "x".into());
        async move {
            let mut history = vec![Message::user("go")];
            let mut host = QuietHost {
                grants,
                ..QuietHost::new()
            };
            execute_with_tools(
                TurnParams {
                    cx: &RunCtx::default(),
                    tp: &writing(args),
                    dispatch: d.clone(),
                    tools: d.tools(),
                    harness: "",
                    overlay: "",
                    max_turns: None,
                },
                &mut history,
                &mut host,
            )
            .await
            .expect("loop");
            history[2].clone()
        }
    };

    let result = run(&root, root.join("x.txt")).await;
    assert!(!result.is_error(), "{}", result.content);
    assert!(root.join("x.txt").exists(), "the granted write landed");

    let result = run(&other, other.join("x.txt")).await;
    assert!(result.is_error(), "another project's grant applied");
    assert!(!other.join("x.txt").exists());

    let result = run(&root, out.join("x.txt")).await;
    assert!(result.is_error(), "a tool grant answered an outside write");
    assert!(!out.join("x.txt").exists());
}

/// An agent's `allow:` is the static level of the run's store, resolved as the assembly resolves it: its
/// directory is a root and its `write_file` runs with nobody to ask, inside the project root and in that
/// directory alike — while a write outside both is still refused, and the same agent without the list is
/// refused inside too.
#[tokio::test]
async fn a_config_allow_runs_unattended() {
    use iota::cmd::{Config, static_grants};
    use iota::tool::grants::Grants;
    use iota::tool::{Registry, ToolEnv};

    let dir = tempfile::tempdir().expect("tempdir");
    let real = iota::app::fs::physical(dir.path()).expect("real");
    let (root, out, sibling) = (real.join("proj"), real.join("out"), real.join("sibling"));
    for d in [&root, &out, &sibling] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let run = |allow: &str, path: std::path::PathBuf| {
        let yaml = format!("agents:\n  coder:\n    tools: {{code: {{}}}}\n    allow: {allow}\n");
        let cfg = Config::parse(
            yaml.as_bytes(),
            &iota::app::env::Env::default(),
            &mut |_| {},
        )
        .expect("the config loads");
        let agent = &cfg.agents["coder"];
        let env = ToolEnv {
            project_root: Some(root.clone()),
            ..ToolEnv::default()
        };
        let fixed = static_grants(&cfg, "coder", agent, &env).expect("resolves");
        let grants = Arc::new(Grants::with_static(fixed));
        let env = ToolEnv {
            grants: Arc::clone(&grants),
            ..env
        };
        let d = Arc::new(Registry::build(&env, &agent.tools, &mut |w| panic!("{w}")));
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), path.to_str().expect("utf-8").into());
        args.insert("content".to_owned(), "x".into());
        async move {
            let mut history = vec![Message::user("go")];
            let mut host = QuietHost {
                grants,
                ..QuietHost::new()
            };
            execute_with_tools(
                TurnParams {
                    cx: &RunCtx::default(),
                    tp: &writing(args),
                    dispatch: d.clone(),
                    tools: d.tools(),
                    harness: "",
                    overlay: "",
                    max_turns: None,
                },
                &mut history,
                &mut host,
            )
            .await
            .expect("loop");
            history[2].clone()
        }
    };

    let allowed = "[../out, write_file]";
    for target in [root.join("x.txt"), out.join("deep/y.txt")] {
        let result = run(allowed, target.clone()).await;
        assert!(!result.is_error(), "{}", result.content);
        assert!(target.exists(), "{} landed", target.display());
    }
    let result = run(allowed, sibling.join("x.txt")).await;
    assert!(result.is_error(), "outside both roots");
    assert!(!sibling.join("x.txt").exists());

    let result = run("[]", root.join("z.txt")).await;
    assert!(result.is_error(), "without allow: an inside write asks");
    assert!(!root.join("z.txt").exists());
}

/// A built-in the run has nobody to ask about is refused with §8's sentence (docs/design/config-allow.md), which
/// names the two ways to permit it — and the first, `--allow <name>`, does: the same call through the same
/// agent, the flag resolved as the assembly resolves it, runs and lands.
#[tokio::test]
async fn a_builtin_refusal_names_the_allow_flag() {
    use iota::cmd::{Config, flag_grants, static_grants};
    use iota::tool::grants::Grants;
    use iota::tool::{Registry, ToolEnv};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = iota::app::fs::physical(dir.path()).expect("real");
    let cfg = Config::parse(
        b"agents:\n  coder:\n    tools: {code: }\n",
        &iota::app::env::Env::default(),
        &mut |_| {},
    )
    .expect("the config loads");
    let agent = &cfg.agents["coder"];
    for flag in [false, true] {
        let env = ToolEnv {
            project_root: Some(root.clone()),
            ..ToolEnv::default()
        };
        let config = static_grants(&cfg, "coder", agent, &env).expect("resolves");
        let values = if flag {
            vec!["write_file".to_owned()]
        } else {
            Vec::new()
        };
        let mut fixed = flag_grants(&cfg, agent, &values, &[], &config, &env).expect("resolves");
        fixed.tools.extend(config.tools);
        let grants = Arc::new(Grants::with_static(fixed));
        let env = ToolEnv {
            grants: Arc::clone(&grants),
            ..env
        };
        let d = Arc::new(Registry::build(&env, &agent.tools, &mut |w| panic!("{w}")));
        let target = root.join(format!("flag-{flag}.txt"));
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), target.to_str().expect("utf-8").into());
        args.insert("content".to_owned(), "x".into());
        let mut history = vec![Message::user("go")];
        execute_with_tools(
            TurnParams {
                cx: &RunCtx::default(),
                tp: &writing(args),
                dispatch: d.clone(),
                tools: d.tools(),
                harness: "",
                overlay: "",
                max_turns: None,
            },
            &mut history,
            &mut QuietHost {
                grants,
                ..QuietHost::new()
            },
        )
        .await
        .expect("loop");
        let result = &history[2];
        assert_eq!(target.exists(), flag, "{}", result.content);
        if flag {
            assert!(!result.is_error(), "{}", result.content);
        } else {
            assert!(result.is_error());
            assert_eq!(
                result.content,
                "write_file was not executed: it needs the user's approval, which this non-interactive run cannot ask for; pass --allow write_file, or list it in the agent's `allow:`, to permit it here."
            );
        }
    }
}

/// A minimal MCP server in POSIX `sh`: `initialize`, `tools/list` (one tool `echo`) and `tools/call`
/// (`"pong"`), exiting on stdin EOF.
#[cfg(unix)]
const SH_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"says pong","inputSchema":{"type":"object"}}]}}' ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"pong"}],"isError":false}}' ;;
  esac
done
"#;

/// An MCP tool's project grant is inherited by an unattended run as a directory's is: `gh:echo` in the
/// project's `grants.json`, loaded into the run's store, lets the `-m` loop call the tool with nobody to ask —
/// while a project without the grant is refused, with the refusal naming the grant that would allow it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_mcp_grant_runs_unattended() {
    use iota::mcp::config::ServerConfig;
    use iota::session::SessionStore;
    use iota::tool::grants::{Grants, McpTool, ServerId, ToolKey};
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().expect("tempdir");
    let real = iota::app::fs::physical(dir.path()).expect("real");
    let (root, other) = (real.join("proj"), real.join("other"));
    for d in [&root, &other] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let store = SessionStore::new(real.join("sessions"));
    store
        .update_project_grants(&root, |s| {
            s.tools.push(ToolKey::Mcp {
                server: ServerId::Config("gh".to_owned()),
                tool: McpTool::Named("echo".to_owned()),
            });
        })
        .expect("a project grant");
    let manager = iota::mcp::Manager::new(
        vec![ServerConfig {
            name: "gh".to_owned(),
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), SH_SERVER.to_owned()],
            ..ServerConfig::default()
        }],
        // A run with a project: its refusal may name the project grant.
        iota::mcp::ManagerOptions {
            project: true,
            ..iota::mcp::ManagerOptions::new(reqwest::Client::new(), iota::app::env::Env::default())
        },
    );
    manager.connect_all(&CancellationToken::new()).await;
    let wire = "mcp__gh__echo";

    let run = |project: &std::path::Path| {
        let grants = Arc::new(Grants::default());
        grants.replace_project(store.project_grants(project).expect("read"));
        let d: Arc<dyn Dispatcher> = Arc::clone(&manager) as Arc<dyn Dispatcher>;
        let tp = FakeProvider::new().with_tools().answering(move |n, _| {
            if n == 1 {
                return Round::calls(vec![ToolCall {
                    id: "c1".to_owned(),
                    name: wire.to_owned(),
                    arguments: JsonObject::new(),
                }]);
            }
            Round::reply("done")
        });
        async move {
            let mut history = vec![Message::user("go")];
            let mut host = QuietHost {
                grants,
                ..QuietHost::new()
            };
            execute_with_tools(
                TurnParams {
                    cx: &RunCtx::default(),
                    tp: &tp,
                    dispatch: Arc::clone(&d),
                    tools: d.tools(),
                    harness: "",
                    overlay: "",
                    max_turns: None,
                },
                &mut history,
                &mut host,
            )
            .await
            .expect("loop");
            history[2].clone()
        }
    };

    let result = run(&root).await;
    assert!(!result.is_error(), "{}", result.content);
    assert_eq!(result.content, "pong");

    let result = run(&other).await;
    assert!(result.is_error());
    assert_eq!(
        result.content,
        format!(
            "{wire} was not executed: the tools of MCP server \"gh\" need the user's approval, which this \
             non-interactive run cannot ask for; set `mcp_servers.gh.auto_run: true`, or allow it for the \
             project from a chat (/allow gh:echo project)."
        )
    );
    manager.close().await;
}

/// Calls the tool it names (a wire name) once, then answers with the text of the tool result it was sent — so a
/// real `-m` run prints what the model was told.
#[cfg(unix)]
struct EchoTheToolResult(&'static str);

#[cfg(unix)]
impl wiremock::Respond for EchoTheToolResult {
    fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&req.body).expect("a JSON body");
        let told = body["messages"]
            .as_array()
            .and_then(|m| m.iter().find(|m| m["role"] == "tool"))
            .map(|m| m["content"].as_str().expect("text").to_owned());
        let chunks = match told {
            Some(text) => vec![
                serde_json::json!({"choices": [{"delta": {"content": text}}]}),
                serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ],
            None => vec![
                serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": self.0, "arguments": "{}"}}]}}]}),
                serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ],
        };
        let mut sse = String::new();
        for c in &chunks {
            sse.push_str("data: ");
            sse.push_str(&c.to_string());
            sse.push_str("\n\n");
        }
        sse.push_str("data: [DONE]\n\n");
        wiremock::ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
    }
}

/// The refusal of a real `-m` run names `/allow … project` only where that command can work: a `mode: chat`
/// agent has no project level, so it is told the `auto_run` key alone; a `mode: agent` one in a project of
/// its own is told the grant too (docs/design/grants.md §4.2, §8).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_unattended_mcp_refusal_follows_the_mode() {
    use std::process::{Command, Stdio};

    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(EchoTheToolResult("mcp__gh__echo"))
        .mount(&server)
        .await;
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    std::fs::create_dir_all(project.path().join(".git")).expect(".git");
    let script = home.path().join("server.sh");
    std::fs::write(&script, SH_SERVER).expect("the server script");

    let head = "mcp__gh__echo was not executed: the tools of MCP server \"gh\" need the user's approval, which \
                this non-interactive run cannot ask for; set `mcp_servers.gh.auto_run: true`";
    for (mode, tail) in [
        ("chat", ".".to_owned()),
        (
            "agent",
            ", or allow it for the project from a chat (/allow gh:echo project).".to_owned(),
        ),
    ] {
        std::fs::write(
            home.path().join(".iota.yaml"),
            format!(
                "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\n\
                 agents:\n  default: {{model: m, mode: {mode}}}\n\
                 mcp_servers:\n  gh: {{command: sh, args: [{}]}}\n",
                server.uri(),
                script.display()
            ),
        )
        .expect("config");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
        crate::common_child::cleared_env(&mut cmd, home.path())
            .current_dir(project.path())
            .stdin(Stdio::null())
            .args(["-m", "go"]);
        let o = tokio::task::spawn_blocking(move || cmd.output().expect("run iota"))
            .await
            .expect("child join");
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        assert_eq!(o.status.code(), Some(0), "mode: {mode}, stderr: {err}");
        assert_eq!(out, format!("{head}{tail}\n"), "mode: {mode}");
    }
}

/// A `--mcp` server can run unattended: `--allow '<spec>:<tool>'` names it by the flag's whole value — the key a
/// chat grant of it has (`ServerId::Flag`) — so a real `-m` run calls its tool with nobody to ask and prints
/// what the tool said; the same run without the value is refused.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_allow_flag_runs_a_flag_server_unattended() {
    use std::process::{Command, Stdio};

    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(EchoTheToolResult("mcp__sh__echo"))
        .mount(&server)
        .await;
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let script = home.path().join("server.sh");
    std::fs::write(&script, SH_SERVER).expect("the server script");
    std::fs::write(
        home.path().join(".iota.yaml"),
        format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\n\
             agents:\n  default: {{model: m}}\n",
            server.uri()
        ),
    )
    .expect("config");
    let spec = format!("sh {}", script.display());
    for (allow, ran) in [(Some(format!("{spec}:echo")), true), (None, false)] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
        crate::common_child::cleared_env(&mut cmd, home.path())
            .current_dir(project.path())
            .stdin(Stdio::null())
            .args(["--mcp", &spec]);
        if let Some(allow) = &allow {
            cmd.args(["--allow", allow]);
        }
        cmd.args(["-m", "go"]);
        let o = tokio::task::spawn_blocking(move || cmd.output().expect("run iota"))
            .await
            .expect("child join");
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        assert_eq!(o.status.code(), Some(0), "{allow:?}, stderr: {err}");
        if ran {
            assert_eq!(out, "pong\n", "stderr: {err}");
        } else {
            assert!(out.starts_with("mcp__sh__echo was not executed: "), "{out}");
        }
    }
}
