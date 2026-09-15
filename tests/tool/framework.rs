//! Tool-framework integration tests (`tool/defer_test.go`, `tool/parallel_test.go`, `tool/ask_test.go`,
//! `tool/shell_test.go` — the Registry/Merge shape with a stub tool instead of the shell set).

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

use iota::BoxFuture;
use iota::chat::turns::RunCtx;
use iota::provider::ProviderKind;
use iota::provider::model::{JsonObject, ToolDef};
use iota::testing::{FakeMcp, prefix_for, static_prefix, stub_tool};
use iota::tool::ask::new_ask_set;
use iota::tool::defer::{CATALOG_NAMES_ONLY_AT, DESC_BUDGET, SEARCH_TOP_K, defer};
use iota::tool::error::ToolError;
use iota::tool::set_disabled;
use iota::tool::sets::ToolsConfig;
use iota::tool::{
    DeferMode, DeferredGroup, Owner, Registry, SEARCH_TOOL_NAME, ToolSearcher, merge,
};
use iota::tool::{DeferState, Dispatcher, Env, ToolOutput, ToolResult};
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::common::temp_project;

/// `map[string]any{"query": q}`.
fn query(q: &str) -> JsonObject {
    let mut args = JsonObject::new();
    args.insert("query".to_owned(), q.into());
    args
}

/// Go's `toolNames`.
fn tool_names(defs: &[ToolDef]) -> HashSet<String> {
    defs.iter().map(|d| d.name.clone()).collect()
}

/// A set of `ToolDef`s with `input_schema: None`.
fn defs(pairs: &[(&str, &str)]) -> Vec<ToolDef> {
    pairs
        .iter()
        .map(|(n, d)| ToolDef {
            name: (*n).to_owned(),
            description: (*d).to_owned(),
            input_schema: None,
            deferred: false,
        })
        .collect()
}

// The shared fixture: one deferred group (`github`, connected under `mcp__gh__`)
// plus a non-deferred server.
fn new_defer_fixture() -> (Arc<FakeMcp>, Arc<dyn Dispatcher>) {
    let inner = Arc::new(FakeMcp::with_defs(&[
        ("mcp__gh__create_pr", "Create a pull request on GitHub"),
        ("mcp__gh__search_code", "Search code across repositories"),
        ("mcp__gh__danger", "Force-push a branch"),
        ("mcp__fs__read", "Read a file"),
    ]));
    let d = defer(
        Arc::clone(&inner) as Arc<dyn Dispatcher>,
        vec![DeferredGroup {
            name: "github".to_owned(),
            summary: "GitHub repos, issues, PRs".to_owned(),
        }],
        prefix_for("github", "mcp__gh__"),
    );
    (inner, d)
}

fn group(name: &str, summary: &str) -> DeferredGroup {
    DeferredGroup {
        name: name.to_owned(),
        summary: summary.to_owned(),
    }
}

async fn call(d: &dyn Dispatcher, name: &str, args: JsonObject) -> ToolOutput {
    d.call_tool(&RunCtx::default(), name, args)
        .await
        .expect("no hard error")
}

#[tokio::test]
async fn deferred_tools_stay_hidden_until_a_search_loads_them() {
    let (_, d) = new_defer_fixture();

    let names = tool_names(&d.tools());
    assert!(
        names.contains(SEARCH_TOOL_NAME) && names.contains("mcp__fs__read"),
        "search_tools and non-deferred tools must be advertised: {names:?}"
    );
    assert!(
        !names.contains("mcp__gh__create_pr") && !names.contains("mcp__gh__danger"),
        "deferred tools leaked before any search: {names:?}"
    );

    let out = call(&*d, SEARCH_TOOL_NAME, query("pull request")).await;
    assert!(!out.is_error, "search failed: {out:?}");
    assert!(
        out.text.contains("mcp__gh__create_pr"),
        "search result must name the loaded tool:\n{}",
        out.text
    );
    assert_eq!(
        out.text,
        "Loaded 1 tool(s) — available from the next step:\n- mcp__gh__create_pr — Create a pull request on GitHub"
    );
    let names = tool_names(&d.tools());
    assert!(
        names.contains("mcp__gh__create_pr"),
        "searched tool must be advertised afterwards"
    );
    assert!(
        !names.contains("mcp__gh__danger"),
        "unmatched tools must stay hidden"
    );
}

#[tokio::test]
async fn a_direct_call_to_a_hidden_tool_loads_and_runs_it() {
    let (inner, d) = new_defer_fixture();

    let out = call(&*d, "mcp__gh__danger", JsonObject::new()).await;
    assert_eq!(
        out,
        ToolOutput::ok("ok:mcp__gh__danger"),
        "implicit call failed"
    );
    assert_eq!(
        inner.calls(),
        vec!["mcp__gh__danger".to_owned()],
        "inner not called"
    );
    assert!(
        tool_names(&d.tools()).contains("mcp__gh__danger"),
        "implicitly called tool must be advertised afterwards"
    );
}

#[tokio::test]
async fn a_merged_dispatcher_routes_hidden_tools_and_approval_to_their_owner() {
    let (_, d) = new_defer_fixture();
    let merged = merge(vec![d]);

    let out = call(&*merged, "mcp__gh__create_pr", JsonObject::new()).await;
    assert_eq!(
        out.text, "ok:mcp__gh__create_pr",
        "merged call to hidden tool"
    );
    assert!(
        merged.requires_approval("mcp__gh__danger"),
        "approval must route through Owner to the wrapped dispatcher"
    );
    assert!(
        !merged.requires_approval("mcp__gh__create_pr"),
        "non-approval tool misreported"
    );
}

#[tokio::test]
async fn an_empty_query_lists_the_hidden_catalog() {
    let (_, d) = new_defer_fixture();
    call(&*d, SEARCH_TOOL_NAME, query("pull request")).await;

    let out = call(&*d, SEARCH_TOOL_NAME, query("")).await;
    for want in [
        "github (3 tools): GitHub repos, issues, PRs",
        "mcp__gh__create_pr — Create a pull request on GitHub (loaded)",
        "mcp__gh__danger — Force-push a branch",
    ] {
        assert!(
            out.text.contains(want),
            "catalog missing {want:?}:\n{}",
            out.text
        );
    }
    assert!(
        !out.text.contains("mcp__fs__read"),
        "non-deferred tools must not clutter the catalog:\n{}",
        out.text
    );
    // The exact catalog (defer.go:405-432).
    assert_eq!(
        out.text,
        "Hidden tool groups (1 groups, 3 tools):\n\
         \n\
         github (3 tools): GitHub repos, issues, PRs\n\
         \x20 mcp__gh__create_pr — Create a pull request on GitHub (loaded)\n\
         \x20 mcp__gh__search_code — Search code across repositories\n\
         \x20 mcp__gh__danger — Force-push a branch\n\
         \n\
         Query by capability keywords to load tools, or call a listed tool directly to load and run it in one step."
    );
    assert!(!out.is_error);
    // A non-string query reads as empty (Go's type assertion) and yields the same catalog.
    let mut args = JsonObject::new();
    args.insert("query".to_owned(), json!(42));
    assert_eq!(call(&*d, SEARCH_TOOL_NAME, args).await.text, out.text);
}

#[tokio::test]
async fn a_large_catalog_lists_names_only() {
    let pairs: Vec<(String, &str)> = (0..CATALOG_NAMES_ONLY_AT + 10)
        .map(|i| (format!("mcp__gh__tool_{i:02}"), "A described tool"))
        .collect();
    let inner = Arc::new(FakeMcp::with_defs(
        &pairs
            .iter()
            .map(|(n, d)| (n.as_str(), *d))
            .collect::<Vec<_>>(),
    ));
    let d = defer(
        inner as Arc<dyn Dispatcher>,
        vec![group("github", "big")],
        prefix_for("github", "mcp__gh__"),
    );

    let out = call(&*d, SEARCH_TOOL_NAME, JsonObject::new()).await;
    assert!(
        !out.text.contains("— A described tool"),
        "large catalog must drop per-tool descriptions:\n{}",
        out.text
    );
    assert!(
        out.text.contains("mcp__gh__tool_00"),
        "names must survive the degradation:\n{}",
        out.text
    );
    assert!(out.text.starts_with(
        "Hidden tool groups (1 groups, 60 tools):\n\ngithub (60 tools): big\n  mcp__gh__tool_00\n"
    ));
}

#[test]
fn the_search_tool_description_folds_overflow_into_a_more_groups_tail() {
    let inner = Arc::new(FakeMcp::with_defs(&[("mcp__gh__a", "x")]));
    let long = "very long summary ".repeat(20);
    let groups: Vec<DeferredGroup> = (0..12)
        .map(|i| group(&format!("srv{i:02}"), &long))
        .collect();
    let d = defer(inner as Arc<dyn Dispatcher>, groups, static_prefix(""));

    let def = &d.tools()[0];
    assert_eq!(def.name, SEARCH_TOOL_NAME, "first advertised tool");
    assert!(
        def.description.len() <= DESC_BUDGET + 100,
        "description {} chars, budget {DESC_BUDGET}",
        def.description.len()
    );
    assert!(
        def.description.contains("more groups"),
        "overflow must fold into a +N more tail:\n{}",
        def.description
    );
    // The exact budget arithmetic (defer.go:193, quirks included): 537 bytes of group lines, the tail line
    // uncounted, every kept line clamped to 120 chars.
    let line = format!(
        "- srv00 (connecting…): {}…",
        long.chars().take(119).collect::<String>()
    );
    assert!(def.description.starts_with(&format!(
        "Search and load additional tools before first use. Hidden tool groups:\n{line}\n"
    )));
    let kept = 537 / (line.len() + 1);
    assert!(def.description.contains(&format!(
        "\n… +{} more groups — empty query lists all\nQuery by capability keywords",
        12 - kept
    )));
    assert!(
        def.description
            .ends_with("Calling a hidden tool directly by name also loads it.")
    );
    // The query schema has no `required` list.
    assert_eq!(
        def.input_schema,
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Capability keywords; empty lists everything hidden."}
            }
        })
        .as_object()
        .cloned()
    );
}

#[tokio::test]
async fn a_group_without_a_prefix_yet_is_shown_as_connecting() {
    let inner = Arc::new(FakeMcp::default());
    let d = defer(
        inner as Arc<dyn Dispatcher>,
        vec![group("slack", "Send Slack messages")],
        static_prefix(""),
    );

    let def = &d.tools()[0];
    assert!(
        def.description
            .contains("slack (connecting…): Send Slack messages"),
        "connecting group line missing:\n{}",
        def.description
    );
    let out = call(&*d, SEARCH_TOOL_NAME, query("slack")).await;
    assert!(
        out.text.contains("No tools matched") && out.text.contains("connecting…"),
        "search against a connecting group must say so:\n{}",
        out.text
    );
    assert_eq!(
        out.text,
        "No tools matched \"slack\". Hidden groups:\n- slack (connecting…): Send Slack messages\nTry different keywords, or an empty query to list every tool."
    );
    assert!(!out.is_error);
}

#[test]
fn an_inner_tool_named_search_tools_does_not_shadow_the_real_one() {
    let inner = Arc::new(FakeMcp::with_defs(&[
        (SEARCH_TOOL_NAME, "impostor"),
        ("mcp__fs__read", "Read a file"),
    ]));
    let d = defer(
        inner as Arc<dyn Dispatcher>,
        vec![group("x", "y")],
        static_prefix(""),
    );

    let search: Vec<ToolDef> = d
        .tools()
        .into_iter()
        .filter(|def| def.name == SEARCH_TOOL_NAME)
        .collect();
    assert_eq!(
        search.len(),
        1,
        "search_tools advertised {} times",
        search.len()
    );
    assert!(
        !search[0].description.contains("impostor"),
        "the impostor's definition leaked"
    );
}

#[tokio::test]
async fn a_search_loads_top_k_and_names_the_overflow() {
    let names: Vec<String> = (0..SEARCH_TOP_K + 3)
        .map(|i| format!("mcp__gh__widget_{i:02}"))
        .collect();
    let inner = Arc::new(FakeMcp::with_defs(
        &names
            .iter()
            .map(|n| (n.as_str(), "Operate a github widget"))
            .collect::<Vec<_>>(),
    ));
    let d = defer(
        inner as Arc<dyn Dispatcher>,
        vec![group("github", "widgets")],
        prefix_for("github", "mcp__gh__"),
    );

    let out = call(&*d, SEARCH_TOOL_NAME, query("github widget")).await;
    assert!(
        out.text.contains(&format!("Loaded {SEARCH_TOP_K} tool(s)")),
        "must load exactly top-K:\n{}",
        out.text
    );
    assert!(
        out.text.contains("+3 more matched"),
        "overflow must be named:\n{}",
        out.text
    );
    assert!(out.text.ends_with(
        "+3 more matched but were not loaded — refine the query, list everything with an empty query, or call a tool by its exact name."
    ));
    let advertised = tool_names(&d.tools());
    for (i, name) in names.iter().enumerate() {
        if i < SEARCH_TOP_K {
            assert!(
                advertised.contains(name),
                "widget_{i:02} should be loaded (name tie-break)"
            );
        } else {
            assert!(
                !advertised.contains(name),
                "widget_{i:02} must stay hidden past top-K"
            );
        }
    }
}

#[tokio::test]
async fn a_search_matches_parameter_names_and_descriptions() {
    let mut order = defs(&[("mcp__gh__submit_order", "Submit a customer order")]);
    order[0].input_schema = json!({
        "type": "object",
        "properties": {
            "items": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "options": {"type": "object", "properties": {
                        "giftwrap": {"type": "boolean", "description": "Wrap the item as a gift"},
                    }},
                },
            }},
        },
    })
    .as_object()
    .cloned();
    order.extend(defs(&[("mcp__gh__other", "Unrelated")]));
    let inner = Arc::new(FakeMcp::new(order));
    let d = defer(
        inner as Arc<dyn Dispatcher>,
        vec![group("github", "shop")],
        prefix_for("github", "mcp__gh__"),
    );

    let out = call(&*d, SEARCH_TOOL_NAME, query("giftwrap")).await;
    assert!(
        out.text.contains("mcp__gh__submit_order"),
        "param-name match must find the tool:\n{}",
        out.text
    );
    assert!(
        !out.text.contains("mcp__gh__other"),
        "unrelated tool must not match:\n{}",
        out.text
    );
}

// The config spelling and the default. (Whether a mode APPLIES to a dialect is
// `DeferMode::supports`, and the config layer turns a mismatch into an error; see `tests/cmd/config.rs`.)
#[test]
fn defer_modes_parse_from_their_config_names() {
    assert_eq!(DeferMode::DEFAULT.name(), "normal");
    assert_eq!(DeferMode::from_name(""), None, "empty mode names nothing");
    assert_eq!(DeferMode::from_name("normal"), Some(DeferMode::Normal));
    assert_eq!(
        DeferMode::from_name("reference"),
        Some(DeferMode::Reference)
    );
    assert_eq!(
        DeferMode::from_name("tool-search"),
        Some(DeferMode::ToolSearch)
    );
    assert_eq!(
        DeferMode::from_name("system-tools"),
        Some(DeferMode::SystemTools)
    );
    assert_eq!(DeferMode::from_name("quantum"), None, "unknown mode");
    for m in [
        DeferMode::Normal,
        DeferMode::Reference,
        DeferMode::ToolSearch,
        DeferMode::SystemTools,
    ] {
        assert_eq!(DeferMode::from_name(m.name()), Some(m), "round trip {m:?}");
    }

    // The mode's wrapper is the real deferring dispatcher.
    let inner = Arc::new(FakeMcp::with_defs(&[("mcp__gh__a", "x")]));
    let d = DeferMode::Normal.wrap(
        inner as Arc<dyn Dispatcher>,
        vec![group("github", "s")],
        prefix_for("github", "mcp__gh__"),
    );
    let names = tool_names(&d.tools());
    assert!(
        names.contains(SEARCH_TOOL_NAME) && !names.contains("mcp__gh__a"),
        "wrapped dispatcher must defer: {names:?}"
    );
    assert_eq!(DeferMode::DEFAULT, DeferMode::Normal);
    for m in [
        DeferMode::Normal,
        DeferMode::Reference,
        DeferMode::ToolSearch,
        DeferMode::SystemTools,
    ] {
        assert_eq!(DeferMode::from_name(m.name()), Some(m));
    }
}

#[tokio::test]
async fn the_protocol_modes_wrap_the_dispatcher_differently() {
    let inner = Arc::new(FakeMcp::with_defs(&[
        ("mcp__gh__pr", "Create a pull request"),
        ("mcp__fs__read", "Read a file"),
    ]));
    let groups = vec![group("github", "gh")];
    let wrap = |mode: &str, _kind: ProviderKind| {
        DeferMode::from_name(mode)
            .unwrap_or_else(|| panic!("{mode} is a mode"))
            .wrap(
                Arc::clone(&inner) as Arc<dyn Dispatcher>,
                groups.clone(),
                prefix_for("github", "mcp__gh__"),
            )
    };

    let marked = wrap("reference", ProviderKind::Anthropic);
    let names = tool_names(&marked.tools());
    assert!(
        !names.contains(SEARCH_TOOL_NAME),
        "protocol modes must not advertise our search_tools"
    );
    let (mut deferred_mark, mut plain_mark) = (false, false);
    for def in marked.tools() {
        if def.name == "mcp__gh__pr" {
            deferred_mark = def.deferred;
        }
        if def.name == "mcp__fs__read" {
            plain_mark = def.deferred;
        }
    }
    assert!(
        deferred_mark && !plain_mark,
        "marking wrong: deferred={deferred_mark} plain={plain_mark}"
    );
    assert!(
        marked.as_tool_searcher().is_none(),
        "reference mode has no searcher"
    );
    assert!(
        marked.as_owner().is_none(),
        "marked wrapper has no Owner capability"
    );

    let searching = wrap("tool-search", ProviderKind::OpenResponses);
    let searcher = searching
        .as_tool_searcher()
        .expect("tool-search wrapper must implement ToolSearcher");
    let hits = searcher.search_tools("pull request");
    assert_eq!(hits.len(), 1, "SearchTools = {hits:?}");
    assert_eq!(hits[0].name, "mcp__gh__pr");
    assert!(hits[0].deferred);
    assert!(searcher.search_tools("nothing here").is_empty());
    // Calls pass through untouched.
    assert_eq!(
        call(&*searching, "mcp__fs__read", JsonObject::new())
            .await
            .text,
        "ok:mcp__fs__read"
    );

    let frozen = wrap("system-tools", ProviderKind::OpenAi);
    call(&*frozen, SEARCH_TOOL_NAME, query("pull request")).await;
    assert!(
        !tool_names(&frozen.tools()).contains("mcp__gh__pr"),
        "frozen mode must never grow the tools array"
    );
    let pending = frozen.take_pending_loads();
    assert_eq!(pending.len(), 1, "pending loads = {pending:?}");
    assert_eq!(pending[0].name, "mcp__gh__pr");
    assert!(
        frozen.take_pending_loads().is_empty(),
        "loads must drain exactly once"
    );
    // A re-search of the same tool must not re-queue it.
    call(&*frozen, SEARCH_TOOL_NAME, query("pull request")).await;
    assert!(
        frozen.take_pending_loads().is_empty(),
        "re-search re-queued an already-loaded tool"
    );
    // An implicit call queues too, once.
    call(&*frozen, "mcp__gh__pr", JsonObject::new()).await;
    assert!(frozen.take_pending_loads().is_empty());
}

// The dialect matrix `crate::config` validates a `defer_mode:` against.
#[test]
fn each_defer_mode_supports_its_dialects() {
    for (mode, kind, want) in [
        (DeferMode::Reference, ProviderKind::Anthropic, true),
        (DeferMode::Reference, ProviderKind::OpenAi, false),
        (DeferMode::ToolSearch, ProviderKind::OpenResponses, true),
        (DeferMode::ToolSearch, ProviderKind::Anthropic, false),
        (DeferMode::SystemTools, ProviderKind::OpenAi, true),
        (DeferMode::SystemTools, ProviderKind::OpenResponses, false),
    ] {
        assert_eq!(mode.supports(kind), want, "{mode:?} on {kind}");
    }
    // `normal` is the one mode every dialect speaks.
    for kind in [
        ProviderKind::OpenAi,
        ProviderKind::Anthropic,
        ProviderKind::OpenResponses,
    ] {
        assert!(DeferMode::Normal.supports(kind));
    }
    // The resolved kind decides (POLICY F-01): a Gemini/Vertex/Imagen/Images provider never carries a protocol mode.
    for kind in [
        ProviderKind::Gemini,
        ProviderKind::VertexAi,
        ProviderKind::Imagen,
        ProviderKind::Images,
    ] {
        assert!(DeferMode::Normal.supports(kind));
        assert!(!DeferMode::Reference.supports(kind));
        assert!(!DeferMode::ToolSearch.supports(kind));
        assert!(!DeferMode::SystemTools.supports(kind));
    }
}

#[tokio::test]
async fn the_inspector_reports_each_deferred_tools_state() {
    let (inner, d) = new_defer_fixture();
    call(&*d, SEARCH_TOOL_NAME, query("pull request")).await;

    let merged = merge(vec![d]);
    let by_name: BTreeMap<String, _> = merged
        .deferred_tools()
        .into_iter()
        .map(|st| (st.name.clone(), st))
        .collect();
    let st = &by_name["mcp__gh__create_pr"];
    assert!(
        st.state == DeferState::Loaded && st.group == "github",
        "loaded state wrong: {st:?}"
    );
    let st = &by_name["mcp__gh__danger"];
    assert_eq!(st.state, DeferState::Deferred, "hidden state wrong: {st:?}");
    assert!(!by_name.contains_key("mcp__fs__read"));
    assert_eq!(by_name.len(), 3);

    let marked = DeferMode::Reference.wrap(
        inner as Arc<dyn Dispatcher>,
        vec![group("github", "gh")],
        prefix_for("github", "mcp__gh__"),
    );
    let sts = marked.deferred_tools();
    assert_eq!(sts.len(), 3);
    for st in &sts {
        assert_eq!(
            st.state,
            DeferState::DeferredProtocol,
            "protocol state wrong: {st:?}"
        );
        assert_eq!(st.group, "github");
    }
}

#[test]
fn parallel_support_defaults_to_no() {
    // Go's nil registry is an empty one here.
    let empty = Registry::default();
    assert!(
        !empty.supports_parallel("read_file", None),
        "an empty registry claimed parallel support"
    );
    assert!(
        !empty.supports_parallel("anything", None),
        "an unknown tool claimed parallel support"
    );
    // A tool that simply does not implement the capability.
    let mut plain = Registry::default();
    assert!(plain.add(stub_tool("x", &[], false)));
    assert!(
        !plain.supports_parallel("x", None),
        "a plain tool claimed parallel support"
    );
    assert!(!plain.supports_parallel("x", Some(&JsonObject::new())));
}

#[test]
fn parallel_support_is_decided_per_call() {
    let mut r = Registry::default();
    r.add(stub_tool("task", &["search"], false));
    let agent = |name: &str| {
        let mut a = JsonObject::new();
        a.insert("agent".to_owned(), name.into());
        a
    };
    assert!(
        r.supports_parallel("task", Some(&agent("search"))),
        "the read-only entry must be allowed to run concurrently"
    );
    assert!(
        !r.supports_parallel("task", Some(&agent("implement"))),
        "the write-capable entry must stay serialized"
    );
    // An entry that is not in the table at all, and a call with no argument: unknown resolves to the safe
    // answer, never to the permissive one.
    assert!(
        !r.supports_parallel("task", Some(&agent("nonesuch"))),
        "an unknown entry must default to serialized"
    );
    assert!(
        !r.supports_parallel("task", None),
        "a call with no entry named must default to serialized"
    );
    // The same answers through Merge.
    let merged = merge(vec![Arc::new(r) as Arc<dyn Dispatcher>]);
    assert!(merged.supports_parallel("task", Some(&agent("search"))));
    assert!(!merged.supports_parallel("task", Some(&agent("implement"))));
    assert!(!merged.supports_parallel("nope", Some(&agent("search"))));
}

/// `tools:` parsed the way the config file does it.
fn raw_tools(yaml: &str) -> ToolsConfig {
    #[derive(serde::Deserialize)]
    struct Raw {
        tools: ToolsConfig,
    }
    serde_norway::from_str::<Raw>(yaml).expect("yaml").tools
}

/// An `Env` rooted in a temp project (the shape the host builds: project root + host dirs, never the process
/// environment).
fn project_env() -> (tempfile::TempDir, Env) {
    let (dir, dirs) = temp_project(&[("AGENTS.md", "# rules")]);
    let env = Env {
        project_root: Some(dir.path().to_path_buf()),
        dirs,
        ..Env::default()
    };
    (dir, env)
}

// The ask set contributes no tools headlessly, so `shell` stands in as the one
// that does.
#[test]
fn a_false_set_entry_disables_the_set() {
    let raw = raw_tools("tools:\n  ask: false\n  shell:\n");
    assert!(set_disabled(&raw, "ask"), "ask: false must report disabled");
    assert!(
        !set_disabled(&raw, "shell") && !set_disabled(&raw, "code"),
        "present-empty and absent sets are not disabled"
    );
    let mut warns = Vec::new();
    let (_dir, env) = project_env();
    let r = Registry::build(&env, &raw, &mut |w| warns.push(w));
    for def in r.tools() {
        assert!(
            def.name != "choose" && def.name != "confirm",
            "disabled ask set still built tool {:?}",
            def.name
        );
    }
    // `shell:` was left ON, so it is the one set still contributing — on every platform now that the
    // toolset resolves an interpreter on Windows too.
    assert!(warns.is_empty(), "{warns:?}");
    assert_eq!(tool_names(&r.tools()), HashSet::from(["shell".to_owned()]));

    // DIVERGENCES I-01: every YAML-1.1 false spelling disables, plain or quoted, any case.
    for spelling in [
        "False",
        "FALSE",
        "no",
        "No",
        "NO",
        "off",
        "Off",
        "\"false\"",
        "'no'",
        "\"OFF\"",
    ] {
        let raw = raw_tools(&format!("tools:\n  ask: {spelling}\n"));
        assert!(set_disabled(&raw, "ask"), "ask: {spelling} must disable");
        let mut warns = Vec::new();
        let r = Registry::build(&Env::default(), &raw, &mut |w| warns.push(w));
        assert!(
            r.is_empty() && warns.is_empty(),
            "ask: {spelling}: {warns:?}"
        );
    }
    for spelling in [
        "true", "yes", "on", "0", "1", "~", "{}", "[]", "\"\"", "maybe",
    ] {
        let raw = raw_tools(&format!("tools:\n  ask: {spelling}\n"));
        assert!(
            !set_disabled(&raw, "ask"),
            "ask: {spelling} must not disable"
        );
    }
}

#[test]
fn the_ask_set_is_absent_without_an_interactor() {
    let tools = new_ask_set(&Env::default(), None).expect("ask set never errors");
    assert!(
        tools.is_empty(),
        "want no tools without an Interactor, got {}",
        tools.len()
    );
    // Any node is accepted and ignored.
    let node = serde_norway::from_str("{x: 1}").expect("yaml");
    assert!(
        new_ask_set(&Env::default(), Some(&node))
            .expect("ok")
            .is_empty()
    );
    // Through the registry: `ask:` is accepted and contributes nothing, without a warning.
    let mut warns = Vec::new();
    let r = Registry::build(&Env::default(), &raw_tools("tools:\n  ask:\n"), &mut |w| {
        warns.push(w);
    });
    assert!(r.is_empty() && warns.is_empty(), "{warns:?}");
    assert_eq!(r.len(), 0);
}

// A stub tool stands in for the shell set.
#[tokio::test]
async fn merge_exposes_and_routes_every_parts_tools() {
    let mut reg = Registry::default();
    reg.add(stub_tool("shell", &[], false));
    // Go passes a nil second part; hosts skip absent parts before calling `merge`.
    let merged = merge(vec![Arc::new(reg) as Arc<dyn Dispatcher>]);
    assert_eq!(
        merged.tools().len(),
        1,
        "merge should expose the shell tool"
    );
    let mut args = JsonObject::new();
    args.insert("command".to_owned(), "echo merged".into());
    let out = call(&*merged, "shell", args).await;
    assert!(
        !out.is_error && out.text.contains("merged"),
        "merged routing failed: {out:?}"
    );
    let err = merged
        .call_tool(&RunCtx::default(), "nope", JsonObject::new())
        .await
        .expect_err("expected error for unknown tool");
    assert!(matches!(&err, ToolError::UnknownTool(n) if n == "nope"));
    assert_eq!(err.to_string(), "unknown tool: nope");
    // An empty merge never panics and answers the defaults.
    let empty = merge(Vec::new());
    assert!(empty.tools().is_empty());
    assert!(!empty.requires_approval("x"));
    assert!(empty.as_tool_searcher().is_none());
    assert!(empty.deferred_tools().is_empty());
    assert!(empty.take_pending_loads().is_empty());
}

// The ask and shell sets stand in for every set here; the shell and agent halves have their own suites.
#[test]
fn build_registry_enables_exactly_the_configured_sets() {
    // absent key disables set
    {
        let (_dir, env) = project_env();
        let r = Registry::build(&env, &raw_tools("tools:\n  ask:\n"), &mut |_| {});
        assert!(
            r.tools().is_empty(),
            "expected no tools without a shell key, got {:?}",
            r.tools()
        );
        // A present `shell` key is what turns the set on.
        let r = Registry::build(&env, &raw_tools("tools:\n  shell:\n"), &mut |_| {});
        assert_eq!(tool_names(&r.tools()), HashSet::from(["shell".to_owned()]));
        assert_eq!(r.len(), 1);
        assert!(r.get("shell").is_some());
    }

    // unknown set warns and is skipped
    {
        let mut warned = Vec::new();
        let r = Registry::build(
            &Env::default(),
            &raw_tools("tools:\n  bogus_set:\n"),
            &mut |w| {
                warned.push(w);
            },
        );
        assert!(
            r.tools().is_empty() && warned.len() == 1,
            "expected skip+1 warning, got tools={:?} warned={warned:?}",
            r.tools()
        );
        assert_eq!(warned[0], "unknown toolset \"bogus_set\" (ignored)");
    }

    // a factory error warns and is skipped (the shell set with an unknown sandbox)
    {
        let mut warned = Vec::new();
        let r = Registry::build(
            &Env::default(),
            &raw_tools("tools:\n  shell:\n    sandbox: bogus\n"),
            &mut |w| {
                warned.push(w);
            },
        );
        assert!(r.is_empty());
        assert_eq!(
            warned,
            vec![
                "toolset \"shell\": sandbox must be \"auto\" or \"off\", got \"bogus\" (ignored)"
                    .to_owned()
            ]
        );
    }

    // enable_set: same warnings; an already-registered tool is not duplicated.
    {
        let (_dir, env) = project_env();
        let mut r = Registry::build(&env, &raw_tools("tools:\n  shell:\n"), &mut |_| {});
        let mut warned = Vec::new();
        r.enable_set(&env, "shell", &mut |w| warned.push(w));
        assert_eq!(r.len(), 1, "enable_set duplicated a registered tool");
        r.enable_set(&env, "bogus", &mut |w| warned.push(w));
        assert_eq!(
            warned,
            vec!["unknown toolset \"bogus\" (ignored)".to_owned()]
        );
        let mut fresh = Registry::default();
        fresh.enable_set(&env, "shell", &mut |w| warned.push(w));
        assert_eq!(
            tool_names(&fresh.tools()),
            HashSet::from(["shell".to_owned()])
        );
        assert_eq!(warned.len(), 1);
    }

    // Registry::add is first-wins by name.
    {
        let mut r = Registry::default();
        assert!(r.add(stub_tool("t", &["*"], false)));
        assert!(!r.add(stub_tool("t", &[], true)));
        assert!(
            r.supports_parallel("t", None),
            "the first registration must win"
        );
        assert!(!r.requires_approval("t"));
    }
}

// The Windows half of `build_registry_enables_exactly_the_configured_sets`, rewritten for the backend that landed: the set that used to
// contribute one warning and no tool now registers the same `shell` tool every other platform gets. What it must
// still do ONCE, at build time, is tell the model WHICH interpreter that tool runs — the name does not say
// so — and that Windows has no sandbox to put the calls in.
#[cfg(windows)]
#[test]
fn shell_set_registers_the_resolved_interpreter_on_windows() {
    use iota::shell::interp::Family;

    let (_dir, env) = project_env();
    let mut warned = Vec::new();
    let r = Registry::build(&env, &raw_tools("tools:\n  shell:\n"), &mut |w| {
        warned.push(w);
    });
    assert!(warned.is_empty(), "{warned:?}");
    assert_eq!(tool_names(&r.tools()), HashSet::from(["shell".to_owned()]));

    // Windows always has at least cmd.exe, so the ladder always ends somewhere.
    let shell = iota::shell::interp::resolve().expect("no interpreter on a Windows machine");
    let description = &r.tools()[0].description;
    assert!(
        description.starts_with(match shell.family {
            Family::Posix => "Run a bash command line",
            Family::PowerShell => "Run a PowerShell command line",
            Family::Cmd => "Run a cmd.exe command line",
        }),
        "the description must name the interpreter that will run:\n{description}"
    );
    assert!(
        description.contains("WITHOUT a sandbox"),
        "Windows has no OS sandbox and the description has to say so:\n{description}"
    );
}

/// A part with the Owner capability that advertises `name` but disowns it (`Some(false)`), or owns a hidden
/// `name` (`Some(true)`) — the three-way `owns` Merge routes by (tool/tool.go:572-587).
struct Owning {
    advertised: Vec<ToolDef>,
    owns: Option<bool>,
    search: Option<Vec<ToolDef>>,
    called: Mutex<Vec<String>>,
}

impl Dispatcher for Owning {
    fn tools(&self) -> Vec<ToolDef> {
        self.advertised.clone()
    }

    fn call_tool<'a>(
        &'a self,
        _cx: &'a RunCtx,
        name: &'a str,
        _args: JsonObject,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            self.called.lock().unwrap().push(name.to_owned());
            Ok(ToolOutput::ok(format!("owning:{name}")))
        })
    }

    fn as_owner(&self) -> Option<&dyn Owner> {
        self.owns.map(|_| self as &dyn Owner)
    }

    fn as_tool_searcher(&self) -> Option<&dyn ToolSearcher> {
        self.search.as_ref().map(|_| self as &dyn ToolSearcher)
    }
}

impl Owner for Owning {
    fn owns(&self, _name: &str) -> bool {
        self.owns.unwrap_or(false)
    }
}

impl ToolSearcher for Owning {
    fn search_tools(&self, _query: &str) -> Vec<ToolDef> {
        self.search.clone().unwrap_or_default()
    }
}

// New: Merge's owner() takes an Owner part's answer as final (a `false` skips the part without scanning),
// scans `tools()` for a part without one, and search_tools goes to the FIRST part with a searcher even when
// it returns no hits.
#[tokio::test]
async fn merge_owner_three_way_and_first_capable_search() {
    let disowning = Arc::new(Owning {
        advertised: defs(&[("t", "advertised but disowned")]),
        owns: Some(false),
        search: Some(Vec::new()),
        called: Mutex::new(Vec::new()),
    });
    let scanning = Arc::new(FakeMcp::with_defs(&[("t", "scanned")]));
    let hidden = Arc::new(Owning {
        advertised: Vec::new(),
        owns: Some(true),
        search: Some(defs(&[("h", "hit")])),
        called: Mutex::new(Vec::new()),
    });
    let merged = merge(vec![
        Arc::clone(&disowning) as Arc<dyn Dispatcher>,
        Arc::clone(&scanning) as Arc<dyn Dispatcher>,
        Arc::clone(&hidden) as Arc<dyn Dispatcher>,
    ]);

    // tools(): earlier part wins the name even though it disowns it.
    let tools = merged.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].description, "advertised but disowned");
    // call: the disowning part is skipped; the scanning part is found through its tools().
    assert_eq!(call(&*merged, "t", JsonObject::new()).await.text, "ok:t");
    assert!(disowning.called.lock().unwrap().is_empty());
    assert_eq!(scanning.calls(), vec!["t".to_owned()]);
    // A name nobody advertises reaches the part that owns everything.
    assert_eq!(
        call(&*merged, "ghost", JsonObject::new()).await.text,
        "owning:ghost"
    );
    // search_tools: the first capable part answers, even with zero hits.
    assert!(
        merged
            .as_tool_searcher()
            .expect("a capable part")
            .search_tools("h")
            .is_empty()
    );
    // Without a capable part the merged view has none.
    let none = merge(vec![Arc::clone(&scanning) as Arc<dyn Dispatcher>]);
    assert!(none.as_tool_searcher().is_none());
    assert!(none.as_owner().is_none());
}

// The registry reports
// capability PRESENCE, so the chat layer can tell "no summary" (fall back to the argument digest)
// from "empty summary" (render a bare `[name]`).
#[test]
fn the_registry_reports_header_summary_presence() {
    let (dir, _dirs) = temp_project(&[]);
    let env = Env {
        project_root: Some(dir.path().to_path_buf()),
        ..Env::default()
    };
    let mut warns: Vec<String> = Vec::new();
    let r = Registry::build(&env, &raw_tools("tools:\n  code:\n"), &mut |w| {
        warns.push(w);
    });
    assert!(warns.is_empty(), "{warns:?}");

    let mut path = JsonObject::new();
    path.insert("path".to_owned(), "a.go".into());
    assert_eq!(
        r.header_summary("edit_file", &path).as_deref(),
        Some("a.go"),
        "edit_file declares a summary"
    );

    let mut pattern = JsonObject::new();
    pattern.insert("pattern".to_owned(), "*.go".into());
    assert!(
        r.header_summary("glob", &pattern).is_none(),
        "glob declares no summary; want None so the digest applies"
    );
    assert!(
        r.header_summary("nope", &JsonObject::new()).is_none(),
        "unknown tool reported a summary"
    );
}
