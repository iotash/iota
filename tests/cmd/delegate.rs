//! `tools.delegate` wiring tests (`cmd/delegate_test.go`): agent resolution, the startup validation pass,
//! the child's toolset and the freshness of everything the `ChildFactory` builds.
//!
//! Every test injects `HostDirs` and a map-backed `EnvSource` — nothing reads or mutates the process
//! environment, and no test reaches the network (a provider is constructed but never called).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{fs, path::Path, sync::Arc};

use crate::common::temp_project;
use iota::app::HostDirs;
use iota::cmd::Config;
use iota::cmd::delegate::{
    DelegateAgents, DelegateConfig, LegacyAgentRef, build_delegator, build_delegator_with_factory,
    child_tools,
};
use iota::testing::{map_env, map_resolver};
use iota::tool::sets::{RawNode, ToolsConfig};
use iota::vars::EnvSource;

/// Go `loadConfig`: writes `body` into the project and loads it as the ONLY config file.
fn load_config(root: &Path, body: &str) -> Config {
    let path = root.join("c.yaml");
    fs::write(&path, body).unwrap();
    // One-layer blocks are what most of these fixtures are, so the migration lines are expected; the
    // three-layer fixtures assert their own silence.
    Config::load(
        Some(&path),
        &HostDirs::default(),
        &map_resolver(&[]),
        &mut |_| {},
    )
    .expect("the config loads")
}

/// Go `agentsNode`: the `tools.delegate` value as a raw YAML node.
fn agents_node(body: &str) -> RawNode {
    serde_norway::from_str(body).expect("agents node")
}

/// The four injected seams every `build_delegator` call needs.
fn seams() -> (reqwest::Client, Arc<dyn EnvSource>) {
    (reqwest::Client::new(), Arc::new(map_env(&[])))
}

/// Whether `d` advertises a tool called `name`. Only the agent-mode child-toolset test needs it.
fn has(d: &dyn iota::tool::Dispatcher, name: &str) -> bool {
    d.tools().iter().any(|def| def.name == name)
}

// Go: cmd/delegate_test.go:42
//
// The parallel decision has to survive a whole chain: a provider entry's toolset → read_only_registry →
// AgentInfo.read_only → the answer the tool gives for a call naming that agent.
#[test]
fn test_delegate_parallel_follows_the_agents_toolset() {
    use iota::provider::model::JsonObject;
    use iota::tool::Registry;
    use iota::tool::{Delegator, Dispatcher, Env};
    use serde_json::json;

    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "
providers:
  worker:  {type: openai, key: k, model: m}
  scout:   {type: openai, key: k, model: m, tools: {code: {read_only: true}}}
  codeboy: {type: openai, key: k, model: m, tools: {code: }}
",
    );
    let node = agents_node(
        "
agents:
  fast: {provider: worker, description: has no tools at all}
  scout: {provider: scout, description: searches but cannot write}
  slow: {provider: codeboy, description: can edit files}
",
    );
    let (http, env) = seams();
    let del = build_delegator(
        &cfg,
        Some(&node),
        http,
        dir.path().to_path_buf(),
        dirs.clone(),
        env,
    )
    .expect("buildDelegator");

    assert!(
        del.agent("fast").expect("fast").read_only,
        "an agent with no toolset must resolve as read-only"
    );
    // The case the whole feature was blocked on: an agent that can SEARCH and still fan out.
    assert!(
        del.agent("scout").expect("scout").read_only,
        "an agent with the read-only code set must be read-only"
    );
    assert!(
        !del.agent("slow").expect("slow").read_only,
        "an agent holding edit_file/write_file must not be read-only"
    );

    let mut tools = ToolsConfig::new();
    tools.insert("delegate".to_owned(), RawNode::Null);
    let env = Env {
        project_root: Some(dir.path().to_path_buf()),
        dirs,
        delegate: Some(del),
        ..Env::default()
    };
    let reg = Registry::build(&env, &tools, &mut |w| panic!("unexpected warning: {w}"));

    let args = |agent: &str| -> JsonObject {
        match json!({ "agent": agent }) {
            serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        }
    };
    assert!(
        reg.supports_parallel("delegate", Some(&args("fast"))),
        "a delegation to a read-only agent must be allowed to run concurrently"
    );
    assert!(
        reg.supports_parallel("delegate", Some(&args("scout"))),
        "a searching-but-not-writing agent must be allowed to run concurrently"
    );
    assert!(
        !reg.supports_parallel("delegate", Some(&args("slow"))),
        "a delegation to a write-capable agent must stay serialized"
    );
    // Unknown and absent both mean serial: the permissive answer is never the one an unresolved name falls
    // back to.
    assert!(!reg.supports_parallel("delegate", Some(&args("nope"))));
    assert!(!reg.supports_parallel("delegate", None));
}

// Go: cmd/delegate_test.go:93
//
// A referenced provider without a model cannot be asked to pick one, so it has to fail at startup rather than as
// a 400 mid-conversation.
#[test]
fn test_delegate_requires_a_model() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(dir.path(), "providers:\n  worker: {type: openai, key: k}\n");
    let node = agents_node("agents:\n  fast: worker\n");
    let (http, env) = seams();
    let err = build_delegator(&cfg, Some(&node), http, dir.path().to_path_buf(), dirs, env)
        .err()
        .expect("an agent whose provider has no model: must be a startup error");
    assert_eq!(
        err.to_string(),
        "agent \"fast\": provider \"worker\" has no `model:` (a delegated agent cannot be asked to pick one)"
    );
}

// Go: cmd/delegate_test.go:103
//
// The bare-string form is the common one and must mean the same as the mapping form with only `provider:` set.
#[test]
fn test_agent_ref_accepts_both_forms() {
    let cfg: DelegateConfig = serde_norway::from_str(
        "agents:\n  a: worker\n  b: {provider: worker, description: d}\n  c: {description: only}\n",
    )
    .expect("decode");
    let DelegateAgents::Legacy(refs) = &cfg.agents else {
        panic!("a mapping is the one-layer form: {:?}", cfg.agents);
    };
    assert_eq!(refs["a"], LegacyAgentRef::Name("worker".to_owned()));
    assert_eq!(refs["a"].provider(), "worker");
    assert_eq!(refs["a"].description(), "");
    assert_eq!(refs["b"].provider(), "worker");
    assert_eq!(refs["b"].description(), "d");
    // A description-only mapping decodes with an EMPTY provider (`provider` defaults) so the failure is the
    // useful one, not a YAML type error.
    assert_eq!(refs["c"].provider(), "");
    assert_eq!(refs["c"].description(), "only");

    let (dir, dirs) = temp_project(&[]);
    let config = load_config(dir.path(), "providers:\n  worker: {type: openai, key: k}\n");
    let node = agents_node("agents:\n  review: {description: reviews things}\n");
    let (http, env) = seams();
    let err = build_delegator(
        &config,
        Some(&node),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .err()
    .expect("a description-only agent names no provider");
    assert_eq!(err.to_string(), "agent \"review\": no provider named");
}

/// The three-layer form: `agents:` is a LIST of top-level `agents:` entries, so a child finally has its own
/// prompt, its own tools and its own description without a provider entry standing in for it.
#[test]
fn delegate_agents_list_references_top_level_agents() {
    use iota::tool::Delegator as _;

    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "
providers:
  oa: {type: openai, key: k}
models:
  fast: oa:gpt-4o-mini
  smart: oa:gpt-5.2
agents:
  reviewer:
    models: [smart]
    system: you review
    description: reviews code
  scout:
    models: [fast]
    tools: {code: {read_only: true}}
    description: searches but cannot write
",
    );
    let node = agents_node("agents: [reviewer, scout]\nmax_turns: 4\n");
    let (http, env) = seams();
    let (del, factory) =
        build_delegator_with_factory(&cfg, Some(&node), http, dir.path().to_path_buf(), dirs, env)
            .expect("delegator");

    // The description comes from the agent entry itself.
    assert_eq!(
        del.agent("reviewer").expect("reviewer").description,
        "reviews code"
    );
    assert!(
        del.agent("reviewer").expect("reviewer").read_only,
        "no toolset at all is read-only"
    );
    assert!(del.agent("scout").expect("scout").read_only);

    // The child rides the agent's model, the model's provider, and the agent's prompt and cap.
    let child = factory("reviewer").expect("child");
    assert_eq!(child.provider.model(), "gpt-5.2");
    assert_eq!(child.system, "you review");
    assert_eq!(child.max_turns.map(std::num::NonZeroU32::get), Some(4));

    // The bare list form (`delegate: [reviewer]`) means the same thing said shorter.
    let node = agents_node("[reviewer]\n");
    let (http, env) = seams();
    let del = build_delegator(
        &cfg,
        Some(&node),
        http,
        dir.path().to_path_buf(),
        HostDirs::default(),
        env,
    )
    .expect("delegator");
    assert_eq!(
        del.agent("reviewer").expect("reviewer").description,
        "reviews code"
    );
    assert!(del.agent("scout").is_none(), "only what the list named");

    // A name that resolves nowhere fails at startup, with the reference in the message.
    let node = agents_node("agents: [nosuch]\n");
    let (http, env) = seams();
    let err = build_delegator(
        &cfg,
        Some(&node),
        http,
        dir.path().to_path_buf(),
        HostDirs::default(),
        env,
    )
    .err()
    .expect("an unknown agent must be a startup error");
    assert!(
        err.to_string()
            .starts_with("agent \"nosuch\": unknown provider \"nosuch\""),
        "{err}"
    );
}

/// A `workspace: true` agent gets the `skills` set in its child, exactly as `agent: true` did.
#[test]
fn a_workspace_agent_child_gets_the_skills_set() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "providers:\n  oa: {type: openai, key: k}\nagents:\n  helper:\n    models: [\"oa:gpt-4o\"]\n    workspace: true\n",
    );
    let node = agents_node("agents: [helper]\n");
    let (http, env) = seams();
    let (_del, factory) =
        build_delegator_with_factory(&cfg, Some(&node), http, dir.path().to_path_buf(), dirs, env)
            .expect("delegator");
    let child = factory("helper").expect("child");
    assert!(
        has(child.dispatch.as_ref(), "load_skill"),
        "workspace: true must enable the skills set: {:?}",
        child.dispatch.tools()
    );
    assert!(child.agent.enabled, "the child runs in workspace mode");
}

// Go: cmd/delegate_test.go:120
//
// A child must not be handed the tool that made it: recursive delegation is unbounded in a way no per-run cap
// describes.
#[test]
fn test_child_tools_drop_delegate() {
    let mut raw = ToolsConfig::new();
    for key in ["code", "delegate", "shell"] {
        raw.insert(key.to_owned(), RawNode::Null);
    }
    let got = child_tools(&raw);
    assert!(
        !got.contains_key("delegate"),
        "the child kept the delegate set"
    );
    assert_eq!(
        got.len(),
        2,
        "childTools dropped more than delegate: {got:?}"
    );
}

// Go: cmd/delegate_test.go:134
//
// AgentMode only injects the AGENTS.md/skills text; load_skill comes from the agent SET, which the main session
// enables separately.
#[test]
fn test_delegate_child_gets_agent_mode_tools() {
    use iota::cmd::delegate::build_child_tools;
    use iota::tool::Delegator;

    let (dir, dirs) = temp_project(&[]);
    let (on, warn) = build_child_tools(dir.path(), &dirs, &ToolsConfig::new(), true);
    assert!(warn.is_empty(), "unexpected warnings: {warn:?}");
    assert!(
        has(on.as_ref(), "load_skill"),
        "an agent-mode child must be able to open the skills it is told about"
    );
    let (off, _) = build_child_tools(dir.path(), &dirs, &ToolsConfig::new(), false);
    assert!(
        !has(off.as_ref(), "load_skill"),
        "a child without agent mode gained load_skill"
    );

    // And the flag reaches the builder from the provider entry: load_skill is not parallel-safe, so an
    // agent-mode child cannot be classified read-only.
    let cfg = load_config(
        dir.path(),
        "providers:\n  skilled: {type: openai, key: k, model: m, agent: true}\n",
    );
    let (http, env) = seams();
    let del = build_delegator(
        &cfg,
        Some(&agents_node("agents:\n  a: skilled\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .expect("buildDelegator");
    assert!(
        !del.agent("a").expect("a").read_only,
        "an agent-mode child holds load_skill and cannot be read-only"
    );
}

// Go: cmd/delegate_test.go:170
//
// A malformed toolset in an agent's provider entry has to be a startup error, not a warning written to stderr
// mid-turn.
#[test]
fn test_delegate_rejects_a_malformed_child_toolset() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "
providers:
  broken: {type: openai, key: k, model: m, tools: {code: [not, a, mapping]}}
",
    );
    let (http, env) = seams();
    let err = build_delegator(
        &cfg,
        Some(&agents_node("agents:\n  a: broken\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .err()
    .expect("a malformed child toolset must fail at startup");
    let text = err.to_string();
    assert!(
        text.starts_with("agent \"a\": toolset \"code\": "),
        "the error should name the agent and the set: {text}"
    );
}

// Go: cmd/delegate_test.go:190
//
// A child's provider entry reaches the wire unchecked unless it is checked here.
#[test]
fn test_delegate_validates_the_childs_provider_entry() {
    let (dir, dirs) = temp_project(&[]);
    for (entry, want) in [
        (
            "{type: openai, key: k, model: m, effort: turbo}",
            "agent \"a\": provider \"bad\" has effort \"turbo\": want low|medium|high|xhigh|max",
        ),
        (
            "{type: openai, key: k, model: m, temperature: 3.5}",
            "agent \"a\": provider \"bad\" has temperature 3.5: want 0.0-2.0",
        ),
        (
            "{type: openai, key: k, model: m, top_p: 2}",
            "agent \"a\": provider \"bad\" has top_p 2: want 0.0-1.0",
        ),
    ] {
        let cfg = load_config(dir.path(), &format!("providers:\n  bad: {entry}\n"));
        let (http, env) = seams();
        let err = build_delegator(
            &cfg,
            Some(&agents_node("agents:\n  a: bad\n")),
            http,
            dir.path().to_path_buf(),
            dirs.clone(),
            env,
        )
        .err()
        .unwrap_or_else(|| panic!("{entry}: an invalid value passed startup"));
        assert_eq!(err.to_string(), want);
    }

    // Valid values still build.
    let cfg = load_config(
        dir.path(),
        "providers:\n  ok: {type: openai, key: k, model: m, effort: high, temperature: 0.7, top_p: 0.9}\n",
    );
    let (http, env) = seams();
    build_delegator(
        &cfg,
        Some(&agents_node("agents:\n  a: ok\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .expect("a valid entry was rejected");
}

/// The `ChildFactory` must rebuild BOTH the provider and the dispatcher on every delegation (delegate.go:137-186
/// calls `buildChildTools` again inside the closure): each child needs its own `Registry`/`CodeSet`
/// read-before-edit ledger, so a later or concurrent child can never edit a file only an earlier child read.
#[test]
fn child_dispatcher_is_fresh_per_delegation() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "providers:\n  worker: {type: openai, key: k, model: m, tools: {code: }}\n",
    );
    let (http, env) = seams();
    let (_, factory) = build_delegator_with_factory(
        &cfg,
        Some(&agents_node("agents:\n  a: worker\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .expect("buildDelegator");

    let first = factory("a").expect("first delegation");
    let second = factory("a").expect("second delegation");
    assert!(
        !Arc::ptr_eq(&first.dispatch, &second.dispatch),
        "the two delegations shared one dispatcher"
    );
    assert_eq!(
        first.dispatch.tools().len(),
        second.dispatch.tools().len(),
        "both children get the same toolset, just not the same instance"
    );
    // The provider is fresh too (a per-task effort override must not leak into another child).
    assert_eq!(first.provider.model(), "m");
    assert_eq!(second.provider.model(), "m");

    // An unknown name never reaches a provider.
    assert_eq!(
        factory("nope").err().expect("unknown agent").to_string(),
        "unknown agent \"nope\""
    );
}

/// delegate.go:74-77: a negative `max_turns` is clamped to 0 (= unlimited) rather than becoming a cap that can
/// never be met.
#[test]
fn delegate_max_turns_negative_clamps_to_zero() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "providers:\n  worker: {type: openai, key: k, model: m}\n",
    );
    let (http, env) = seams();
    let (_, factory) = build_delegator_with_factory(
        &cfg,
        Some(&agents_node("agents:\n  a: worker\nmax_turns: -5\n")),
        http.clone(),
        dir.path().to_path_buf(),
        dirs.clone(),
        Arc::clone(&env),
    )
    .expect("buildDelegator");
    assert_eq!(factory("a").expect("child").max_turns, None);

    let (_, factory) = build_delegator_with_factory(
        &cfg,
        Some(&agents_node("agents:\n  a: worker\nmax_turns: 3\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .expect("buildDelegator");
    assert_eq!(
        factory("a").expect("child").max_turns,
        std::num::NonZeroU32::new(3)
    );
}

/// An absent or empty `agents:` mapping is the one thing the seam cannot work without.
#[test]
fn no_agents_configured_is_a_startup_error() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(dir.path(), "providers: {}\n");
    for node in [None, Some(RawNode::Null), Some(agents_node("agents: {}\n"))] {
        let (http, env) = seams();
        let err = build_delegator(
            &cfg,
            node.as_ref(),
            http,
            dir.path().to_path_buf(),
            dirs.clone(),
            env,
        )
        .err()
        .expect("no agents must fail");
        assert_eq!(
            err.to_string(),
            "no agents configured (add `agents:` mapping agent names to provider names)"
        );
    }

    // A node that is not a mapping at all keeps the decoder's own text behind Go's prefix (DIVERGENCES D-15).
    let (http, env) = seams();
    let err = build_delegator(
        &cfg,
        Some(&agents_node("[1, 2]\n")),
        http,
        dir.path().to_path_buf(),
        dirs,
        env,
    )
    .err()
    .expect("a sequence is not a delegate config");
    assert!(
        err.to_string()
            .starts_with("config must be a mapping (agents, max_turns): "),
        "{err}"
    );
}

/// The API key is resolved LAZILY, per delegation: the environment outranks `key:` and an agent with neither
/// fails with the text that names both places to put one.
#[test]
fn child_api_key_comes_from_the_injected_environment() {
    let (dir, dirs) = temp_project(&[]);
    let cfg = load_config(
        dir.path(),
        "providers:\n  worker: {type: openai, model: m}\n",
    );
    let http = reqwest::Client::new();
    let node = agents_node("agents:\n  a: worker\n");

    let empty: Arc<dyn EnvSource> = Arc::new(map_env(&[]));
    let (_, factory) = build_delegator_with_factory(
        &cfg,
        Some(&node),
        http.clone(),
        dir.path().to_path_buf(),
        dirs.clone(),
        empty,
    )
    .expect("buildDelegator");
    assert_eq!(
        factory("a").err().expect("no key").to_string(),
        "agent \"a\": API key is required (set OPENAI_API_KEY or `key:`)"
    );

    let set: Arc<dyn EnvSource> = Arc::new(map_env(&[("OPENAI_API_KEY", "sk-env")]));
    let (_, factory) =
        build_delegator_with_factory(&cfg, Some(&node), http, dir.path().to_path_buf(), dirs, set)
            .expect("buildDelegator");
    factory("a").expect("the environment supplies the key");
}
