//! Config-model integration tests (`config/config_test.go` ported, plus the load-order and parse-error rules,
//! the three-layer resolution and the soft-migration layer that keeps a one-layer file behaving as it did).
//! Every test injects `HostDirs` and a map-backed `VarResolver` — nothing reads or mutates the process
//! environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::common::temp_project;
use iota::cmd::{AgentConfig, Config, ConfigError, ModelRef, ProviderConfig};
use iota::testing::{MapResolver, map_resolver};
use pretty_assertions::assert_eq;

/// `Load(path)` from the Go tests: the explicit file alone, no home/cwd tiers, warnings collected.
fn load_explicit(path: &Path, resolver: &MapResolver) -> (Config, Vec<String>) {
    let (cfg, warnings) = try_load_explicit(path, resolver);
    (cfg.expect("the config loads"), warnings)
}

/// The same, without insisting that the load succeeded.
fn try_load_explicit(
    path: &Path,
    resolver: &MapResolver,
) -> (Result<Config, ConfigError>, Vec<String>) {
    let mut warnings = Vec::new();
    let cfg = Config::load(
        Some(path),
        &iota::app::HostDirs::default(),
        resolver,
        &mut |w| {
            warnings.push(w);
        },
    );
    (cfg, warnings)
}

/// Writes `content` as `<root>/<name>` and loads it explicitly, asserting no warning was printed.
fn load_yaml(root: &Path, name: &str, content: &str) -> Config {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    let (cfg, warnings) = load_explicit(&path, &map_resolver(&[]));
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    cfg
}

/// Writes a ONE-LAYER config and loads it: the migration lines are expected, so they come back for the
/// caller to inspect instead of failing the load.
fn load_legacy(root: &Path, name: &str, content: &str) -> (Config, Vec<String>) {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    load_explicit(&path, &map_resolver(&[]))
}

/// One config document, straight from a string.
fn parse(yaml: &str) -> Result<Config, ConfigError> {
    Config::parse(yaml.as_bytes(), &map_resolver(&[]), &mut |_| {})
}

/// The same, keeping the warnings.
fn parse_warned(yaml: &str) -> (Result<Config, ConfigError>, Vec<String>) {
    let mut warnings = Vec::new();
    let cfg = Config::parse(yaml.as_bytes(), &map_resolver(&[]), &mut |w| {
        warnings.push(w);
    });
    (cfg, warnings)
}

// ---------------------------------------------------------------- the one-layer form, still working

// Go: config/config_test.go:12
#[test]
fn test_load_tools() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "config.yaml",
        "
providers:
  claude:
    type: anthropic
    key: sk-ant-xxx
    model: claude-sonnet-4
    tools:
      shell:
        - git
        - ssh
  openai:
    key: sk-official
    tools:
      shell:
",
    );

    // claude: shell present with a populated list.
    let node = cfg.agents["claude"]
        .tools
        .get("shell")
        .expect("claude: shell should be present");
    let allow: Vec<String> = serde_norway::from_value(node.clone()).expect("decode allow");
    assert_eq!(allow, vec!["git".to_owned(), "ssh".to_owned()]);

    // openai: shell present but empty (key exists → enabled, defaults).
    let tools = &cfg.agents["openai"].tools;
    assert!(
        tools.contains_key("shell"),
        "openai: shell key should be present even when empty"
    );
    assert_eq!(tools.get("shell"), Some(&serde_norway::Value::Null));

    // A provider without a tools block gets no agent at all, so it has no enabled tools.
    assert!(!cfg.agents.contains_key("missing"));
    assert!(
        cfg.resolve("openai")
            .expect("openai resolves")
            .agent
            .tools
            .contains_key("shell")
    );
}

// Go: config/config_test.go:65 — `agent:` under a provider is now `workspace:` on the agent it implies.
#[test]
fn test_load_agent() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "config.yaml",
        "
providers:
  a:
    agent: true
  b:
    agent: yes
  c:
    agent: on
  d:
    agent: false
  e:
    key: sk-xxx
",
    );
    for name in ["a", "b", "c"] {
        assert!(
            cfg.agents[name].workspace,
            "provider {name}: workspace should be enabled"
        );
    }
    for name in ["d", "e", "missing"] {
        assert!(
            !cfg.resolve(name).is_some_and(|r| r.agent.workspace),
            "provider {name}: workspace should be disabled"
        );
    }

    // DIVERGENCES I-01: every YAML 1.1 spelling, any case, quoted or plain, for every bool field.
    let (cfg, _) = load_legacy(
        dir.path(),
        "bools.yaml",
        "
providers:
  f:
    agent: On
    image: \"yes\"
    json_edits: TRUE
    no_save: off
    notify: No
",
    );
    assert!(cfg.agents["f"].workspace);
    assert!(cfg.models["f"].image && cfg.models["f"].json_edits);
    assert!(!cfg.agents["f"].no_save);
    assert_eq!(cfg.agents["f"].notify, Some(false));
}

// Go: config/config_test.go:100 — the prompt belongs to the agent now.
#[test]
fn test_resolve_system() {
    let (dir, _dirs) = temp_project(&[("sys.md", "You are terse.\n")]);
    let f = dir.path().join("sys.md").to_string_lossy().into_owned();

    let inline = AgentConfig {
        system: "inline".to_owned(),
        system_file: f.clone(),
        ..AgentConfig::default()
    };
    assert_eq!(
        inline.resolve_system().unwrap(),
        "inline",
        "inline should win"
    );

    let file_only = AgentConfig {
        system_file: f,
        ..AgentConfig::default()
    };
    assert_eq!(file_only.resolve_system().unwrap(), "You are terse.\n");

    let missing_path = dir.path().join("missing.md");
    let missing = AgentConfig {
        system_file: missing_path.to_string_lossy().into_owned(),
        ..AgentConfig::default()
    };
    let err = missing
        .resolve_system()
        .expect_err("missing system_file must error, not silently blank the prompt");
    assert!(matches!(err, ConfigError::SystemFile(_)));
    let text = err.to_string();
    assert!(
        text.starts_with(&format!("system_file: open {}: ", missing_path.display())),
        "error text = {text:?}"
    );

    assert_eq!(AgentConfig::default().resolve_system().unwrap(), "");
}

// Go: config/config_test.go:120
#[test]
fn test_load_expands_provider_vars() {
    let (dir, dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "providers:\n  d:\n    type: openai\n    key: ${env:CFG_TEST_KEY}\n    url: ${env:CFG_TEST_KEY}/v1\n    system_file: ${appHome}/sys.md\n    effort: high\n",
    )
    .unwrap();
    let resolver = MapResolver {
        home: dirs.home.clone(),
        cwd: dirs.cwd.clone(),
        ..map_resolver(&[("CFG_TEST_KEY", "sk-expanded")])
    };

    let (cfg, _) = load_explicit(&path, &resolver);
    let (_, pc) = cfg.get("d");
    assert_eq!(pc.key, "sk-expanded");
    assert_eq!(pc.url, "sk-expanded/v1");
    let want = dirs.home.clone().unwrap().join(".iota").join("sys.md");
    assert_eq!(PathBuf::from(&cfg.agents["d"].system_file), want);
    assert_eq!(cfg.models["d"].effort, "high");

    // The same three keys expand when they are written in their new homes.
    let path = dir.path().join("layered.yaml");
    fs::write(
        &path,
        "providers:\n  d:\n    type: openai\n    key: ${env:CFG_TEST_KEY}\nagents:\n  coder:\n    models: [\"d:gpt-4o\"]\n    system_file: ${appHome}/sys.md\n",
    )
    .unwrap();
    let (cfg, warnings) = load_explicit(&path, &resolver);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.providers["d"].key, "sk-expanded");
    let want = dirs.home.unwrap().join(".iota").join("sys.md");
    assert_eq!(PathBuf::from(&cfg.agents["coder"].system_file), want);
}

// Go: config/config_test.go:147
#[test]
fn test_mcp_servers_for() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "
providers:
  all:
    type: openai
    tools: {}
  none:
    type: openai
    mcp_servers: []
  some:
    type: openai
    mcp_servers: [fs]
  typo:
    type: openai
    mcp_servers: [nope]
mcp_servers:
  fs:
    command: fs-server
  gh:
    url: https://x/sse
",
    );

    let all = AgentConfig::default();
    assert_eq!(all.mcp_servers, None, "absent key must stay None");
    assert_eq!(
        cfg.mcp_servers_for(&all).unwrap().len(),
        2,
        "absent key: want all 2"
    );

    let none = &cfg.agents["none"];
    assert_eq!(
        none.mcp_servers,
        Some(vec![]),
        "empty list must stay Some([])"
    );
    assert!(
        cfg.mcp_servers_for(none).unwrap().is_empty(),
        "empty list: want 0"
    );

    let got = cfg.mcp_servers_for(&cfg.agents["some"]).unwrap();
    assert_eq!(got.len(), 1, "subset: want 1");
    assert_eq!(
        got["fs"].command, "fs-server",
        "subset picked the wrong server: {got:?}"
    );

    let err = cfg
        .mcp_servers_for(&cfg.agents["typo"])
        .expect_err("unknown name must error, not silently skip");
    assert_eq!(
        err.to_string(),
        "mcp_servers: \"nope\" is not defined under the top-level mcp_servers"
    );
}

// Go: config/config_test.go:195
#[test]
fn test_temperature_field() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  tuned:\n    type: openai\n    temperature: 0.3\n  norm:\n    type: openai\n",
    );
    assert_eq!(cfg.models["tuned"].temperature, Some(0.3));
    assert!(!cfg.models.contains_key("norm"), "no knob, no model entry");
    assert_eq!(
        cfg.resolve("norm").unwrap().temperature(),
        None,
        "temperature must default None"
    );
}

// Go: config/config_test.go:209
#[test]
fn test_top_p_field() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  tuned:\n    type: openai\n    top_p: 0.9\n  norm:\n    type: openai\n",
    );
    assert_eq!(cfg.models["tuned"].top_p, Some(0.9));
    assert_eq!(
        cfg.resolve("norm").unwrap().top_p(),
        None,
        "top_p must default None"
    );
}

// Go: config/config_test.go:224
#[test]
fn test_notify_field() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  quiet:\n    type: openai\n    notify: false\n  norm:\n    type: openai\n",
    );
    assert_eq!(
        cfg.agents["quiet"].notify,
        Some(false),
        "notify: false not parsed"
    );
    assert_eq!(
        cfg.resolve("norm").unwrap().agent.notify,
        None,
        "notify must default None (on)"
    );
}

// Go: config/config_test.go:238
#[test]
fn test_no_save_field() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  eph:\n    type: openai\n    no_save: true\n  norm:\n    type: openai\n",
    );
    assert!(cfg.agents["eph"].no_save, "no_save: true not parsed");
    assert!(
        !cfg.resolve("norm").unwrap().agent.no_save,
        "no_save must default false"
    );
}

// Go: config/config_test.go:253
#[test]
fn test_defer_field() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "
providers:
  x:
    type: openai
mcp_servers:
  github:
    command: gh-mcp
    defer: \"GitHub repos, issues, PRs\"
  fs:
    command: fs-mcp
  blank:
    command: b-mcp
    defer: \"\"
",
    );
    assert_eq!(
        cfg.mcp_servers["github"].defer.as_deref(),
        Some("GitHub repos, issues, PRs")
    );
    assert_eq!(
        cfg.mcp_servers["fs"].defer, None,
        "absent defer must be None"
    );
    assert_eq!(
        cfg.mcp_servers["blank"].defer.as_deref(),
        Some(""),
        "blank defer must be present-and-empty (cmd warns)"
    );
}

// Go: config/config_test.go:283
#[test]
fn test_defer_mode_field() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, _) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  a:\n    type: anthropic\n    defer_mode: reference\n  b:\n    type: openai\n",
    );
    assert_eq!(cfg.models["a"].defer_mode, "reference");
    assert_eq!(
        cfg.resolve("b").unwrap().model.defer_mode,
        "",
        "defer_mode must default empty"
    );
}

// Go: config/config_test.go:298
#[test]
fn test_find_config_file() {
    let (dir, _dirs) = temp_project(&[]);
    let sub = dir.path().join("lookup");
    fs::create_dir_all(&sub).unwrap();
    assert_eq!(Config::find_config_file(&sub), None, "empty dir");

    let fallback = sub.join(".iota.yml");
    fs::write(&fallback, "providers: {}\n").unwrap();
    assert_eq!(
        Config::find_config_file(&sub),
        Some(fallback.clone()),
        ".yml only"
    );

    let preferred = sub.join(".iota.yaml");
    fs::write(&preferred, "providers: {}\n").unwrap();
    assert_eq!(
        Config::find_config_file(&sub),
        Some(preferred),
        "both present: .yaml wins"
    );
}

// ---------------------------------------------------------------- the soft-migration layer

/// Every key a one-layer `providers.<name>` block could carry lands where the three-layer config keeps it,
/// with the SAME value — and the block prints exactly one deprecation line naming what moved.
#[test]
fn one_layer_block_splits_into_three_entries() {
    let (dir, _dirs) = temp_project(&[("sys.md", "be terse\n")]);
    let sys = dir.path().join("sys.md").to_string_lossy().into_owned();
    let (cfg, warnings) = load_legacy(
        dir.path(),
        "c.yaml",
        &format!(
            "
providers:
  deepseek:
    type: openai
    key: sk-cfg
    url: https://api.deepseek.com/v1
    model: deepseek-chat
    context_window: 200k
    defer_mode: system-tools
    effort: high
    temperature: 0.3
    top_p: 0.9
    image: true
    aspect_ratio: \"1:1\"
    image_size: 1024x1024
    negative_prompt: blurry
    json_edits: true
    system: inline prompt
    system_file: {sys}
    tools:
      code: {{}}
    mcp_servers: [fs]
    agent: true
    no_save: true
    notify: false
mcp_servers:
  fs:
    command: fs-server
"
        ),
    );

    // ONE line, naming every key that moved, in the order the block reads.
    assert_eq!(
        warnings,
        vec![
            "Warning: config providers.deepseek: model, context_window, defer_mode, effort, temperature, top_p, image, aspect_ratio, image_size, negative_prompt, json_edits, system, system_file, tools, mcp_servers, agent, no_save, notify now belong under `models:` / `agents:` (still accepted; see README)"
                .to_owned()
        ]
    );

    // providers: the endpoint, and only the endpoint.
    assert_eq!(
        cfg.providers["deepseek"],
        ProviderConfig {
            kind: "openai".to_owned(),
            key: "sk-cfg".to_owned(),
            url: "https://api.deepseek.com/v1".to_owned(),
        }
    );

    // models: the id, the model's own properties, the protocol and the tunable defaults.
    let m = &cfg.models["deepseek"];
    assert_eq!(m.provider, "deepseek");
    assert_eq!(m.id, "deepseek-chat");
    assert_eq!(m.context_window, "200k");
    assert_eq!(m.defer_mode, "system-tools");
    assert_eq!(m.effort, "high");
    assert_eq!(m.temperature, Some(0.3));
    assert_eq!(m.top_p, Some(0.9));
    assert!(m.image && m.json_edits);
    assert_eq!(m.aspect_ratio, "1:1");
    assert_eq!(m.image_size, "1024x1024");
    assert_eq!(m.negative_prompt, "blurry");

    // agents: the usage.
    let a = &cfg.agents["deepseek"];
    assert_eq!(a.models, vec![ModelRef::Entry("deepseek".to_owned())]);
    assert_eq!(a.system, "inline prompt");
    assert_eq!(a.system_file, sys);
    assert!(a.tools.contains_key("code"));
    assert_eq!(a.mcp_servers, Some(vec!["fs".to_owned()]));
    assert!(a.workspace && a.no_save);
    assert_eq!(a.notify, Some(false));

    // …and the name still resolves to all three at once.
    let r = cfg.resolve("deepseek").expect("resolves");
    assert_eq!(r.provider_type, "openai");
    assert_eq!(r.provider_name, "deepseek");
    assert_eq!(r.model.id, "deepseek-chat");
    assert_eq!(r.effort(), "high");
    assert_eq!(r.temperature(), Some(0.3));
    assert_eq!(r.top_p(), Some(0.9));
    assert_eq!(r.agent.system, "inline prompt");
}

/// A block with only endpoint keys migrates nothing and says nothing; a block with no `model:` still opens
/// the picker, which is spelled `<provider>:*` now.
#[test]
fn a_plain_endpoint_block_migrates_nothing() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, warnings) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  plain:\n    type: openai\n    key: k\n  toolsonly:\n    type: openai\n    tools: {code: {}}\n",
    );
    assert_eq!(
        warnings,
        vec![
            "Warning: config providers.toolsonly: tools now belong under `models:` / `agents:` (still accepted; see README)"
                .to_owned()
        ]
    );
    assert!(!cfg.models.contains_key("plain"));
    assert!(!cfg.agents.contains_key("plain"));
    assert_eq!(
        cfg.agents["toolsonly"].models,
        vec![ModelRef::All {
            provider: "toolsonly".to_owned()
        }],
        "no `model:` means the candidate set is whatever the provider lists"
    );
    assert_eq!(cfg.resolve("toolsonly").unwrap().model.id, "");
}

/// The `agent` toolset is the `skills` toolset now; the old key is accepted with one line.
#[test]
fn the_agent_toolset_is_renamed_to_skills() {
    let (dir, _dirs) = temp_project(&[]);
    let (cfg, warnings) = load_legacy(
        dir.path(),
        "c.yaml",
        "providers:\n  p:\n    type: openai\n    tools:\n      agent:\n",
    );
    assert_eq!(
        warnings,
        vec![
            "Warning: config providers.p: tools now belong under `models:` / `agents:` (still accepted; see README)".to_owned(),
            "Warning: config providers.p.tools: toolset `agent` is now `skills` (still accepted; see README)".to_owned(),
        ]
    );
    let tools = &cfg.agents["p"].tools;
    assert!(tools.contains_key("skills") && !tools.contains_key("agent"));

    // …and under an explicit agent too.
    let (cfg, warnings) = parse_warned(
        "providers:\n  p: {type: openai}\nagents:\n  coder:\n    models: [\"p:gpt-4o\"]\n    tools:\n      agent:\n",
    );
    assert_eq!(
        warnings,
        vec![
            "Warning: config agents.coder.tools: toolset `agent` is now `skills` (still accepted; see README)"
                .to_owned()
        ]
    );
    assert!(cfg.unwrap().agents["coder"].tools.contains_key("skills"));
}

/// A later file replaces a provider WHOLE, so the entries an earlier one implied go with it.
#[test]
fn a_replaced_provider_drops_the_entries_it_implied() {
    let (dir, dirs) = temp_project(&[
        (
            "home/.iota.yaml",
            "providers:\n  shared:\n    type: openai\n    key: home\n    model: home-model\n    tools: {code: {}}\n",
        ),
        (
            ".iota.yaml",
            "providers:\n  shared:\n    type: openai\n    key: cwd\n",
        ),
    ]);
    let mut warnings = Vec::new();
    let cfg =
        Config::load(None, &dirs, &map_resolver(&[]), &mut |w| warnings.push(w)).expect("loads");
    assert_eq!(cfg.providers["shared"].key, "cwd");
    assert!(!cfg.models.contains_key("shared"), "{:?}", cfg.models);
    assert!(!cfg.agents.contains_key("shared"), "{:?}", cfg.agents);
    let _ = dir;
}

// ---------------------------------------------------------------- references

/// The three reference forms, each parsed the same way wherever one is written.
#[test]
fn model_reference_forms() {
    assert_eq!(
        "sonnet".parse::<ModelRef>().unwrap(),
        ModelRef::Entry("sonnet".to_owned())
    );
    assert_eq!(
        "anthropic:claude-sonnet-4".parse::<ModelRef>().unwrap(),
        ModelRef::Inline {
            provider: "anthropic".to_owned(),
            id: "claude-sonnet-4".to_owned()
        }
    );
    // Everything after the FIRST colon is the id, so a relay's own `vendor/model` shape survives.
    assert_eq!(
        "openrouter:anthropic/claude-3.5-sonnet"
            .parse::<ModelRef>()
            .unwrap(),
        ModelRef::Inline {
            provider: "openrouter".to_owned(),
            id: "anthropic/claude-3.5-sonnet".to_owned()
        }
    );
    assert_eq!(
        "anthropic:*".parse::<ModelRef>().unwrap(),
        ModelRef::All {
            provider: "anthropic".to_owned()
        }
    );
    // …and each renders back to what it was written as.
    for s in [
        "sonnet",
        "anthropic:claude-sonnet-4",
        "openrouter:anthropic/claude-3.5-sonnet",
        "anthropic:*",
    ] {
        assert_eq!(s.parse::<ModelRef>().unwrap().to_string(), s);
    }

    for (bad, want) in [
        ("", "a model reference must not be empty"),
        (
            ":gpt-5",
            "model reference \":gpt-5\": nothing before the ':' (want \"provider:model\")",
        ),
        (
            "openai:",
            "model reference \"openai:\": nothing after the ':' (want \"provider:model\" or \"provider:*\")",
        ),
    ] {
        assert_eq!(
            bad.parse::<ModelRef>().unwrap_err().to_string(),
            want,
            "{bad:?}"
        );
    }
}

/// All three forms decode from a real config, and each names the right endpoint.
#[test]
fn model_references_resolve_through_the_config() {
    let cfg = parse(
        "
providers:
  anthropic: {key: k}
  relay: {type: openai, key: k, url: https://relay/v1}
models:
  sonnet: anthropic:claude-sonnet-4
  gpt5:
    provider: relay
    id: gpt-5.2
agents:
  mixed:
    models: [sonnet, \"relay:anthropic/claude-3.5\", \"relay:*\"]
",
    )
    .expect("loads");

    assert_eq!(cfg.models["sonnet"].provider, "anthropic");
    assert_eq!(cfg.models["sonnet"].id, "claude-sonnet-4");

    let r = cfg.resolve("mixed").expect("resolves");
    assert_eq!(r.provider_name, "anthropic");
    assert_eq!(r.provider_type, "anthropic");
    assert_eq!(r.model.id, "claude-sonnet-4");

    // The inline form carries its own provider; the wildcard carries only the provider.
    let inline = cfg.model_of(&r.agent.models[1]).unwrap();
    assert_eq!(
        (inline.provider.as_str(), inline.id.as_str()),
        ("relay", "anthropic/claude-3.5")
    );
    let all = cfg.model_of(&r.agent.models[2]).unwrap();
    assert_eq!((all.provider.as_str(), all.id.as_str()), ("relay", ""));

    // `gpt5` resolves on its own, without an agent.
    let r = cfg.resolve("gpt5").expect("resolves");
    assert_eq!(r.provider_name, "relay");
    assert_eq!(r.provider_type, "openai");
    assert_eq!(r.model.id, "gpt-5.2");
}

/// The one mistake `provider:model` invites: a space after the colon makes YAML read a mapping. The error
/// says so instead of leaving the user with serde's "invalid type: map".
#[test]
fn a_reference_written_as_a_yaml_mapping_explains_itself() {
    let err = parse(
        "providers:\n  anthropic: {key: k}\nagents:\n  x:\n    models:\n      - anthropic: claude-sonnet-4\n",
    )
    .expect_err("a mapping is not a reference");
    let text = err.to_string();
    assert!(
        text.contains(
            "a model reference must be a string like \"provider:model\", but `anthropic: claude-sonnet-4` parses as a YAML mapping — remove the space after the colon (`anthropic:claude-sonnet-4`)"
        ),
        "error text = {text:?}"
    );

    // A file that fails this way is dropped whole, with the same reason, exactly like a parse error.
    let (dir, _dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "agents:\n  x:\n    models:\n      - openai: gpt-4o\n",
    )
    .unwrap();
    let (cfg, warnings) = load_explicit(&path, &map_resolver(&[]));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].ends_with(" (file ignored)"), "{warnings:?}");
    assert_eq!(cfg, Config::default());
}

/// A `models:` shorthand must name a provider AND a model — the two other reference forms mean something
/// only where a SET is expected.
#[test]
fn a_models_shorthand_must_name_a_provider_and_a_model() {
    assert_eq!(
        parse("models:\n  sonnet: gpt-4o\n")
            .expect_err("bare name")
            .to_string(),
        "models.sonnet: \"gpt-4o\" names no provider (want \"provider:model\")"
    );
    assert_eq!(
        parse("providers:\n  openai: {}\nmodels:\n  any: openai:*\n")
            .expect_err("wildcard")
            .to_string(),
        "models.any: \"openai:*\" is a candidate set, not a model (use it in `agents.<name>.models`)"
    );
}

/// Dangling references are refused where they are written, not three tool calls into a conversation.
#[test]
fn dangling_references_are_refused_at_load() {
    assert_eq!(
        parse("models:\n  m: {provider: nosuch, id: x}\n")
            .expect_err("unknown provider")
            .to_string(),
        "models.m: unknown provider \"nosuch\""
    );
    assert_eq!(
        parse("agents:\n  a:\n    models: [nosuch]\n")
            .expect_err("unknown model")
            .to_string(),
        "agents.a: models: unknown model \"nosuch\""
    );
    assert_eq!(
        parse("agents:\n  a:\n    models: [\"nosuch:*\"]\n")
            .expect_err("unknown provider")
            .to_string(),
        "agents.a: models: unknown provider \"nosuch\""
    );
    assert_eq!(
        parse("agents:\n  a:\n    system: hi\n")
            .expect_err("no models")
            .to_string(),
        "agents.a: models: at least one model is required"
    );
    // A built-in type needs no `providers:` entry.
    assert!(parse("models:\n  m: anthropic:claude-x\n").is_ok());
    // A single reference does not have to be written as a list.
    let cfg = parse("agents:\n  a:\n    models: anthropic:claude-x\n").expect("loads");
    assert_eq!(cfg.agents["a"].models.len(), 1);
}

// ---------------------------------------------------------------- defer_mode as a dialect statement

/// A `defer_mode:` the provider's dialect cannot speak is a CONFIG error now, raised by `Config::load` —
/// it used to warn at assembly time and silently fall back to `normal`.
#[test]
fn defer_mode_must_match_the_provider_dialect() {
    // Every mode on the dialect that speaks it.
    for (kind, mode) in [
        ("anthropic", "reference"),
        ("openresponses", "tool-search"),
        ("openai", "system-tools"),
        ("gemini", "normal"),
    ] {
        let yaml =
            format!("models:\n  m:\n    provider: {kind}\n    id: x\n    defer_mode: {mode}\n");
        assert!(parse(&yaml).is_ok(), "{mode} on {kind}");
    }

    for (kind, mode) in [
        ("openai", "reference"),
        ("anthropic", "tool-search"),
        ("openresponses", "system-tools"),
        ("gemini", "reference"),
    ] {
        let yaml =
            format!("models:\n  m:\n    provider: {kind}\n    id: x\n    defer_mode: {mode}\n");
        assert_eq!(
            parse(&yaml).expect_err("mismatch must fail").to_string(),
            format!(
                "models.m: defer_mode {mode:?} does not apply to provider type {kind} (see docs/design/tool-defer.md)"
            ),
            "{mode} on {kind}"
        );
    }

    // The check follows the ALIAS's `type:`, so it sees the dialect the run will really speak (F-01).
    let err = parse(
        "providers:\n  relay: {type: openai}\nmodels:\n  m: {provider: relay, id: x, defer_mode: reference}\n",
    )
    .expect_err("an alias is judged by its type");
    assert_eq!(
        err.to_string(),
        "models.m: defer_mode \"reference\" does not apply to provider type openai (see docs/design/tool-defer.md)"
    );

    // A one-layer block reaches the same verdict through the model it implies.
    let (dir, _dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "providers:\n  a:\n    type: openai\n    defer_mode: reference\n",
    )
    .unwrap();
    let (cfg, _) = try_load_explicit(&path, &map_resolver(&[]));
    assert_eq!(
        cfg.expect_err("mismatch must fail").to_string(),
        "models.a: defer_mode \"reference\" does not apply to provider type openai (see docs/design/tool-defer.md)"
    );
}

/// An unrecognised spelling is still a warning with a fallback: a typo should not lock the user out of a run
/// that would otherwise work. A provider whose `type:` is not a built-in has no dialect to judge against.
#[test]
fn an_unknown_defer_mode_warns_and_falls_back() {
    let (cfg, warnings) =
        parse_warned("models:\n  m: {provider: openai, id: x, defer_mode: quantum}\n");
    assert!(cfg.is_ok());
    assert_eq!(
        warnings,
        vec!["Warning: config models.m: unknown defer_mode \"quantum\" (using normal)".to_owned()]
    );

    let (cfg, warnings) = parse_warned(
        "providers:\n  odd: {type: custom}\nmodels:\n  m: {provider: odd, id: x, defer_mode: reference}\n",
    );
    assert!(cfg.is_ok(), "an unknown type is rejected at construction");
    assert!(warnings.is_empty(), "{warnings:?}");
}

// ---------------------------------------------------------------- the four-level positional resolution

/// `agents:` → `models:` → `providers:` → a built-in type, first match wins, and a name defined twice is
/// taken from the higher layer.
#[test]
fn positional_names_resolve_through_four_levels_in_order() {
    let cfg = parse(
        "
providers:
  openai: {key: pk, url: https://p/v1}
  anthropic: {key: ak}
models:
  openai: {provider: anthropic, id: from-models}
  sonnet: anthropic:claude-x
agents:
  openai:
    models: [sonnet]
    system: from-agents
  solo:
    models: [\"openai:*\"]
",
    )
    .expect("loads");

    // `openai` is all three: the AGENT wins.
    let r = cfg.resolve("openai").expect("resolves");
    assert_eq!(r.agent.system, "from-agents");
    assert_eq!(r.model.id, "claude-x", "the agent's first model decides");
    assert_eq!(r.provider_name, "anthropic");

    // `sonnet` is a model only: no agent, the model's own provider.
    let r = cfg.resolve("sonnet").expect("resolves");
    assert_eq!(r.agent, iota::cmd::AgentConfig::default());
    assert_eq!(
        (r.provider_name.as_str(), r.model.id.as_str()),
        ("anthropic", "claude-x")
    );

    // `anthropic` is a provider only: no model, no agent — the picker's job.
    let r = cfg.resolve("anthropic").expect("resolves");
    assert_eq!(r.provider.key, "ak");
    assert_eq!(r.model.id, "");

    // A built-in type nobody configured still resolves, with defaults.
    let r = cfg.resolve("gemini").expect("resolves");
    assert_eq!(r.provider_type, "gemini");
    assert_eq!(r.provider, ProviderConfig::default());

    // A wildcard-first agent has no default model: the run starts in the picker.
    let r = cfg.resolve("solo").expect("resolves");
    assert_eq!(
        (r.provider_name.as_str(), r.model.id.as_str()),
        ("openai", "")
    );
    assert_eq!(r.provider.url, "https://p/v1");

    assert_eq!(cfg.resolve("nosuch"), None);
    assert_eq!(
        cfg.configured_names(),
        vec!["anthropic", "openai", "solo", "sonnet"],
        "the hint lists every layer's names once"
    );
}

/// The agent's overrides win over the model's defaults, and only for the keys it sets.
#[test]
fn an_agent_overrides_the_models_tunables_one_level_deep() {
    let cfg = parse(
        "
models:
  base:
    provider: openai
    id: gpt-5.2
    effort: low
    temperature: 0.2
    top_p: 0.5
agents:
  hot:
    models: [base]
    temperature: 1.5
",
    )
    .expect("loads");
    let r = cfg.resolve("hot").expect("resolves");
    assert_eq!(r.temperature(), Some(1.5), "the agent wins");
    assert_eq!(r.effort(), "low", "unset keys keep the model's default");
    assert_eq!(r.top_p(), Some(0.5));
}

// ---------------------------------------------------------------- load order and parse errors

/// `-c <file>` reads that file ALONE; without it the home file is merged under the cwd file, whole entry by
/// name (config.go:114-137, 186-197).
#[test]
fn config_only_explicit_path_when_given() {
    let (dir, dirs) = temp_project(&[
        (
            "home/.iota.yaml",
            "providers:\n  home_only:\n    type: openai\n    key: home-key\n  shared:\n    type: openai\n    key: home-shared\nmcp_servers:\n  fs:\n    command: home-fs\n  gh:\n    url: https://home/gh\n",
        ),
        (
            ".iota.yml",
            "providers:\n  cwd_only:\n    type: anthropic\n  shared:\n    type: openai\n    key: cwd-shared\nmcp_servers:\n  fs:\n    command: cwd-fs\n",
        ),
        (
            "explicit.yaml",
            "providers:\n  explicit:\n    type: gemini\n    key: ex\n",
        ),
    ]);
    let resolver = map_resolver(&[]);

    // Explicit: nothing from home or cwd.
    let mut warnings = Vec::new();
    let cfg = Config::load(
        Some(&dir.path().join("explicit.yaml")),
        &dirs,
        &resolver,
        &mut |w| warnings.push(w),
    )
    .expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.providers.keys().collect::<Vec<_>>(), vec!["explicit"]);
    assert!(cfg.mcp_servers.is_empty());

    // Discovery: home (.yaml) then cwd (.yml, the .yaml fallback); cwd replaces WHOLE entries by name.
    let cfg = Config::load(None, &dirs, &resolver, &mut |w| warnings.push(w)).expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        cfg.providers.keys().collect::<Vec<_>>(),
        vec!["cwd_only", "home_only", "shared"]
    );
    let (_, shared) = cfg.get("shared");
    assert_eq!(shared.key, "cwd-shared");
    assert_eq!(cfg.mcp_servers["fs"].command, "cwd-fs");
    assert_eq!(cfg.mcp_servers["gh"].url, "https://home/gh");

    // A missing explicit file is silent (os.IsNotExist) and yields an empty config.
    let cfg = Config::load(
        Some(&dir.path().join("nope.yaml")),
        &dirs,
        &resolver,
        &mut |w| warnings.push(w),
    )
    .expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());

    // No home and no cwd: nothing is read, nothing is warned.
    let cfg = Config::load(None, &iota::app::HostDirs::default(), &resolver, &mut |w| {
        warnings.push(w);
    })
    .expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());
}

/// A YAML parse error drops the WHOLE file with one loud warning and leaves the other tiers intact
/// (config.go:178-185); an empty file is a valid empty config.
#[test]
fn config_parse_error_drops_file_with_warning() {
    let (dir, dirs) = temp_project(&[
        (
            "home/.iota.yaml",
            "providers:\n  good:\n    type: openai\n    key: k\n",
        ),
        (
            ".iota.yaml",
            "providers:\n  bad:\n    temperature: [not, a, float]\n",
        ),
        ("empty.yaml", ""),
    ]);
    let resolver = map_resolver(&[]);

    let mut warnings = Vec::new();
    let cfg = Config::load(None, &dirs, &resolver, &mut |w| warnings.push(w)).expect("loads");
    let cwd_file = dir.path().join(".iota.yaml");
    assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
    assert!(
        warnings[0].starts_with(&format!("Warning: config {}: ", cwd_file.display())),
        "warning = {:?}",
        warnings[0]
    );
    assert!(
        warnings[0].ends_with(" (file ignored)"),
        "warning = {:?}",
        warnings[0]
    );
    assert_eq!(
        cfg.providers.keys().collect::<Vec<_>>(),
        vec!["good"],
        "the broken file is dropped entirely; the home tier survives"
    );

    // An unknown key is not an error (non-strict decode); a type mismatch on a bool field is.
    let (cfg, warnings) = load_explicit(
        &{
            let p = dir.path().join("unknown.yaml");
            fs::write(
                &p,
                "providers:\n  x:\n    type: openai\n    bogus_key: 1\nnot_a_key: true\n",
            )
            .unwrap();
            p
        },
        &resolver,
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.get("x").0, "openai");

    let (cfg, warnings) = load_explicit(
        &{
            let p = dir.path().join("badbool.yaml");
            fs::write(&p, "providers:\n  x:\n    agent: 1\n").unwrap();
            p
        },
        &resolver,
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].ends_with(" (file ignored)"));
    assert_eq!(cfg, Config::default());

    // Empty file: a valid, empty config.
    let (cfg, warnings) = load_explicit(&dir.path().join("empty.yaml"), &resolver);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());
}
