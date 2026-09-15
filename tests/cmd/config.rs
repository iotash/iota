//! Config-model integration tests (`config/config_test.go` ported, plus the load-order and parse-error rules,
//! the three-layer resolution and the key audit that refuses a key written in the wrong layer).
//! Every test injects a fixed `Env` (its `HostDirs` included) — nothing reads or mutates the process
//! environment.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::common::temp_project;
use iota::app::env::Env;
use iota::cmd::{AgentConfig, Config, ConfigError, ModelRef};
use pretty_assertions::assert_eq;

/// `Load(path)` from the Go tests: the explicit file alone, no home/cwd tiers, warnings collected.
fn load_explicit(path: &Path, env: &Env) -> (Config, Vec<String>) {
    let (cfg, warnings) = try_load_explicit(path, env);
    (cfg.expect("the config loads"), warnings)
}

/// The same, without insisting that the load succeeded.
fn try_load_explicit(path: &Path, env: &Env) -> (Result<Config, ConfigError>, Vec<String>) {
    let mut warnings = Vec::new();
    let cfg = Config::load(Some(path), env, &mut |w| {
        warnings.push(w);
    });
    (cfg, warnings)
}

/// Writes `content` as `<root>/<name>` and loads it explicitly, asserting no warning was printed.
fn load_yaml(root: &Path, name: &str, content: &str) -> Config {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    let (cfg, warnings) = load_explicit(&path, &Env::default());
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    cfg
}

/// One config document, straight from a string.
fn parse(yaml: &str) -> Result<Config, ConfigError> {
    Config::parse(yaml.as_bytes(), &Env::default(), &mut |_| {})
}

/// The same, keeping the warnings.
fn parse_warned(yaml: &str) -> (Result<Config, ConfigError>, Vec<String>) {
    let mut warnings = Vec::new();
    let cfg = Config::parse(yaml.as_bytes(), &Env::default(), &mut |w| {
        warnings.push(w);
    });
    (cfg, warnings)
}

// ---------------------------------------------------------------- the three layers
#[test]
fn the_tools_map_loads_with_each_sets_node() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "config.yaml",
        "
providers:
  anthropic: {key: sk-ant-xxx}
  openai: {key: sk-official}
agents:
  claude:
    models: [\"anthropic:claude-sonnet-4\"]
    tools:
      shell:
        - git
        - ssh
  plain:
    models: [\"openai:gpt-4o\"]
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

    // plain: shell present but empty (key exists → enabled, defaults).
    let tools = &cfg.agents["plain"].tools;
    assert!(
        tools.contains_key("shell"),
        "plain: shell key should be present even when empty"
    );
    assert_eq!(tools.get("shell"), Some(&serde_norway::Value::Null));

    // The agent a run resolves to carries them.
    assert!(
        cfg.resolve_agent("plain")
            .expect("plain resolves")
            .agent
            .tools
            .contains_key("shell")
    );
    assert_eq!(cfg.resolve_agent("missing"), None);
}

// What Go spelled `agent:` under a provider is `workspace:` on an agent.
#[test]
fn an_agent_entry_loads_with_its_workspace_flag() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "config.yaml",
        "
agents:
  a: {models: [\"openai:x\"], workspace: true}
  b: {models: [\"openai:x\"], workspace: yes}
  c: {models: [\"openai:x\"], workspace: on}
  d: {models: [\"openai:x\"], workspace: false}
  e: {models: [\"openai:x\"]}
",
    );
    for name in ["a", "b", "c"] {
        assert!(
            cfg.agents[name].workspace,
            "agent {name}: workspace should be enabled"
        );
    }
    for name in ["d", "e", "missing"] {
        assert!(
            !cfg.resolve_agent(name).is_some_and(|r| r.agent.workspace),
            "agent {name}: workspace should be disabled"
        );
    }

    // DIVERGENCES I-01: every YAML 1.1 spelling, any case, quoted or plain, for every bool field.
    let cfg = load_yaml(
        dir.path(),
        "bools.yaml",
        "
models:
  m:
    provider: openai
    id: x
    image: \"yes\"
    json_edits: TRUE
agents:
  f:
    models: [m]
    workspace: On
    no_save: off
    notify: No
",
    );
    assert!(cfg.agents["f"].workspace);
    assert!(cfg.models["m"].image && cfg.models["m"].json_edits);
    assert!(!cfg.agents["f"].no_save);
    assert_eq!(cfg.agents["f"].notify, Some(false));
}

// The prompt belongs to the agent now.
#[test]
fn the_system_prompt_resolves_from_system_then_system_file() {
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
#[test]
fn provider_fields_expand_their_variables_at_load() {
    let (dir, dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "providers:\n  d:\n    type: openai\n    key: ${env:CFG_TEST_KEY}\n    url: ${env:CFG_TEST_KEY}/v1\nmodels:\n  m: {provider: d, id: x, effort: high}\nagents:\n  d: {models: [m], system_file: \"${appHome}/sys.md\"}\n",
    )
    .unwrap();
    let resolver = Env::fixed(&[("CFG_TEST_KEY", "sk-expanded")]).with_dirs(dirs.clone());

    let (cfg, _) = load_explicit(&path, &resolver);
    let pc = cfg.provider("d").config;
    assert_eq!(pc.key, "sk-expanded");
    assert_eq!(pc.url, "sk-expanded/v1");
    let want = dirs.home.clone().unwrap().join(".iota").join("sys.md");
    assert_eq!(PathBuf::from(&cfg.agents["d"].system_file), want);
    assert_eq!(cfg.models["m"].effort, "high");

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
#[test]
fn mcp_servers_for_selects_all_none_or_the_named_subset() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "
agents:
  none:
    models: [\"openai:x\"]
    mcp_servers: []
  some:
    models: [\"openai:x\"]
    mcp_servers: [fs]
  typo:
    models: [\"openai:x\"]
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
#[test]
fn the_temperature_field_parses_and_stays_optional() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "models:\n  tuned: {provider: openai, id: x, temperature: 0.3}\n  norm: openai:x\nagents:\n  norm: {models: [norm]}\n",
    );
    assert_eq!(cfg.models["tuned"].temperature, Some(0.3));
    assert_eq!(
        cfg.resolve_agent("norm").unwrap().temperature(),
        None,
        "temperature must default None"
    );
}
#[test]
fn the_top_p_field_parses_and_stays_optional() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "models:\n  tuned: {provider: openai, id: x, top_p: 0.9}\n  norm: openai:x\nagents:\n  norm: {models: [norm]}\n",
    );
    assert_eq!(cfg.models["tuned"].top_p, Some(0.9));
    assert_eq!(
        cfg.resolve_agent("norm").unwrap().top_p(),
        None,
        "top_p must default None"
    );
}
#[test]
fn the_notify_field_defaults_on_and_parses_yaml_booleans() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "agents:\n  quiet: {models: [\"openai:x\"], notify: false}\n  norm: {models: [\"openai:x\"]}\n",
    );
    assert_eq!(
        cfg.agents["quiet"].notify,
        Some(false),
        "notify: false not parsed"
    );
    assert_eq!(
        cfg.resolve_agent("norm").unwrap().agent.notify,
        None,
        "notify must default None (on)"
    );
}
#[test]
fn the_no_save_field_parses_yaml_booleans() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "agents:\n  eph: {models: [\"openai:x\"], no_save: true}\n  norm: {models: [\"openai:x\"]}\n",
    );
    assert!(cfg.agents["eph"].no_save, "no_save: true not parsed");
    assert!(
        !cfg.resolve_agent("norm").unwrap().agent.no_save,
        "no_save must default false"
    );
}
#[test]
fn the_defer_field_parses_the_deferred_group_list() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "
providers:
  x: {type: openai}
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
#[test]
fn the_defer_mode_field_is_validated_against_the_dialect() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "models:\n  a: {provider: anthropic, id: x, defer_mode: reference}\n  b: openai:x\nagents:\n  b: {models: [b]}\n",
    );
    assert_eq!(cfg.models["a"].defer_mode, "reference");
    assert_eq!(
        cfg.resolve_agent("b").unwrap().model.defer_mode,
        "",
        "defer_mode must default empty"
    );
}
#[test]
fn find_config_file_walks_explicit_then_home_then_cwd() {
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

// ---------------------------------------------------------------- the key audit

/// A key written in the wrong layer names the layer that owns it — and the file it was written in, so the
/// user knows which of the merged files to open.
#[test]
fn a_key_of_another_layer_fails_the_load_naming_the_file() {
    let (dir, _dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "providers:\n  deepseek:\n    type: openai\n    key: k\n    system: be terse\n",
    )
    .unwrap();
    let (cfg, warnings) = try_load_explicit(&path, &Env::default());
    assert!(
        warnings.is_empty(),
        "a key mistake is an error, not a warning: {warnings:?}"
    );
    assert_eq!(
        cfg.expect_err("a misplaced key must fail the load")
            .to_string(),
        format!(
            "config {}: providers.deepseek.system: `system` belongs under `agents:` (see README, \"The three layers\")",
            path.display()
        )
    );
}

/// The one-layer config iota used to accept: every key of it now reports where that key lives.
#[test]
fn the_one_layer_keys_report_their_new_home() {
    for (yaml, want) in [
        (
            "providers:\n  p: {type: openai, model: gpt-4o}\n",
            "providers.p.model: `model` is now a `models:` entry — write `models.<name>: <provider>:<id>` and list it in `agents.<name>.models`",
        ),
        (
            "providers:\n  p: {type: openai, agent: true}\n",
            "providers.p.agent: `agent` is now `workspace:` on an `agents:` entry",
        ),
        (
            "providers:\n  p: {type: openai, effort: high}\n",
            "providers.p.effort: `effort` belongs under `models:` (see README, \"The three layers\")",
        ),
        (
            "providers:\n  p: {type: openai, tools: {code: {}}}\n",
            "providers.p.tools: `tools` belongs under `agents:` (see README, \"The three layers\")",
        ),
        (
            "providers:\n  p: {type: openai, no_save: true}\n",
            "providers.p.no_save: `no_save` belongs under `agents:` (see README, \"The three layers\")",
        ),
    ] {
        assert_eq!(
            parse(yaml)
                .expect_err("a retired key must fail")
                .to_string(),
            want
        );
    }
}

/// A misspelled key is an error too, wherever it is written: silently doing nothing is the failure mode the
/// audit exists to close.
#[test]
fn an_unknown_key_is_refused_with_its_coordinate() {
    for (yaml, want) in [
        (
            "providers:\n  p: {kye: k}\n",
            "providers.p.kye: unknown key (want type, key, url)",
        ),
        (
            "models:\n  m: {provider: openai, idd: x}\n",
            "models.m.idd: unknown key (want provider, id, context_window, defer_mode, effort, temperature, top_p, image, aspect_ratio, image_size, negative_prompt, json_edits)",
        ),
        (
            "agents:\n  coder: {models: [m], sytem: hi}\n",
            "agents.coder.sytem: unknown key (want models, system, system_file, tools, mcp_servers, workspace, no_save, notify, description, context_window, effort, temperature, top_p)",
        ),
        (
            "agnets:\n  coder: {}\n",
            "agnets: unknown top-level key (want providers:, models:, agents:, mcp_servers:)",
        ),
    ] {
        assert_eq!(
            parse(yaml)
                .expect_err("an unknown key must fail")
                .to_string(),
            want
        );
    }
}

/// The toolset table is closed as well: the `agent` set that became `skills` and the `delegate` set that was
/// removed each say so where they are written.
#[test]
fn a_toolset_that_does_not_exist_is_refused() {
    for (yaml, want) in [
        (
            "agents:\n  a: {models: [m], tools: {agent: {}}}\n",
            "agents.a.tools.agent: the `agent` toolset is now called `skills` (the word `agent` names a config layer)",
        ),
        (
            "agents:\n  a: {models: [m], tools: {delegate: [reviewer]}}\n",
            "agents.a.tools.delegate: the `delegate` toolset was removed — run child agents from bash instead (see README)",
        ),
        (
            "agents:\n  a: {models: [m], tools: {shel: {}}}\n",
            "agents.a.tools.shel: unknown toolset (want shell, skills, code, ask)",
        ),
    ] {
        assert_eq!(
            parse(yaml)
                .expect_err("an unknown toolset must fail")
                .to_string(),
            want
        );
    }
    // The four that exist all load.
    assert!(
        parse("agents:\n  a:\n    models: [\"openai:x\"]\n    tools: {shell: {}, skills: {}, code: {}, ask: {}}\n")
            .is_ok()
    );
}

/// A later file replaces an entry WHOLE, by name and by layer.
#[test]
fn a_later_file_replaces_whole_entries() {
    let (dir, dirs) = temp_project(&[
        (
            "home/.iota.yaml",
            "providers:\n  shared: {type: openai, key: home, url: https://home/v1}\n",
        ),
        (
            ".iota.yaml",
            "providers:\n  shared: {type: openai, key: cwd}\n",
        ),
    ]);
    let mut warnings = Vec::new();
    let cfg = Config::load(None, &Env::default().with_dirs(dirs), &mut |w| {
        warnings.push(w);
    })
    .expect("loads");
    assert_eq!(cfg.providers["shared"].key, "cwd");
    assert_eq!(
        cfg.providers["shared"].url, "",
        "the cwd entry replaces the home one whole, url included"
    );
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

    let r = cfg.resolve_agent("mixed").expect("resolves");
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

    // A `models:` entry is reached THROUGH an agent, never named by a run of its own.
    assert_eq!(cfg.resolve_agent("gpt5"), None);
    assert_eq!(cfg.models["gpt5"].provider, "relay");
    assert_eq!(cfg.models["gpt5"].id, "gpt-5.2");
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
    let (cfg, warnings) = load_explicit(&path, &Env::default());
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

    // The verdict is reached at LOAD, so it names no file: the cross-layer pass runs once, over the merged
    // stack, and no single file owns the mismatch.
    let (dir, _dirs) = temp_project(&[]);
    let path = dir.path().join("c.yaml");
    fs::write(
        &path,
        "models:\n  a: {provider: openai, id: x, defer_mode: reference}\n",
    )
    .unwrap();
    let (cfg, _) = try_load_explicit(&path, &Env::default());
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

// ---------------------------------------------------------------- what a run may name

/// `iota run <name>` resolves `agents:` and NOTHING else: a `models:` or `providers:` entry of the same name
/// is not a run, and the hint lists the agents there are.
#[test]
fn a_run_resolves_agents_only() {
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

    // `openai` names an agent, a model AND a provider: only the agent is reachable, and it decides
    // everything — including which endpoint the run talks to.
    let r = cfg.resolve_agent("openai").expect("resolves");
    assert_eq!(r.agent.system, "from-agents");
    assert_eq!(r.model.id, "claude-x", "the agent's first model decides");
    assert_eq!(r.provider_name, "anthropic");
    assert_eq!(r.agent_name, "openai");

    // A model, a provider and a built-in type are not runs.
    for name in ["sonnet", "anthropic", "gemini", "nosuch"] {
        assert_eq!(cfg.resolve_agent(name), None, "{name} is not an agent");
    }

    // A wildcard-first agent has no default model: the run starts in the picker.
    let r = cfg.resolve_agent("solo").expect("resolves");
    assert_eq!(
        (r.provider_name.as_str(), r.model.id.as_str()),
        ("openai", "")
    );
    assert_eq!(r.provider.url, "https://p/v1");

    assert_eq!(
        cfg.agent_names(),
        vec!["openai", "solo"],
        "the hint lists the agents, sorted"
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
    let r = cfg.resolve_agent("hot").expect("resolves");
    assert_eq!(r.temperature(), Some(1.5), "the agent wins");
    assert_eq!(r.effort(), "low", "unset keys keep the model's default");
    assert_eq!(r.top_p(), Some(0.5));
}

/// `agents.<name>.context_window` — the fourth key the layer gained (brain page `model-param-layering`):
/// same spelling as the model's, and the same one-level override, so an agent that knows how long its
/// conversations run says so once instead of forking a `models:` entry per usage.
#[test]
fn an_agent_overrides_the_models_context_window() {
    let cfg = parse(
        "
models:
  base:
    provider: openai
    id: gpt-5.2
    context_window: 128k
agents:
  long:
    models: [base]
    context_window: 400k
  plain:
    models: [base]
",
    )
    .expect("loads");
    assert_eq!(cfg.agents["long"].context_window, "400k");

    // The agent's, when it has one...
    let long = cfg.resolve_agent("long").expect("resolves");
    let decl = long.window_decl().expect("a window is declared");
    assert_eq!((decl.raw, decl.label), ("400k", "agent context_window"));
    assert_eq!(long.declared(Some(400_000)).context_window, Some(400_000));

    // ...the model's otherwise, with the label that names THAT layer in a parse error.
    let plain = cfg.resolve_agent("plain").expect("resolves");
    let decl = plain.window_decl().expect("a window is declared");
    assert_eq!((decl.raw, decl.label), ("128k", "config context_window"));

    // Neither: no declaration at all, which is what leaves the session's own value standing.
    let cfg = parse("models:\n  base: openai:gpt-5.2\nagents:\n  plain: {models: [base]}\n")
        .expect("loads");
    assert!(
        cfg.resolve_agent("plain")
            .expect("resolves")
            .window_decl()
            .is_none()
    );
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
    let env = Env::default().with_dirs(dirs);

    // Explicit: nothing from home or cwd.
    let mut warnings = Vec::new();
    let cfg = Config::load(Some(&dir.path().join("explicit.yaml")), &env, &mut |w| {
        warnings.push(w);
    })
    .expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.providers.keys().collect::<Vec<_>>(), vec!["explicit"]);
    assert!(cfg.mcp_servers.is_empty());

    // Discovery: home (.yaml) then cwd (.yml, the .yaml fallback); cwd replaces WHOLE entries by name.
    let cfg = Config::load(None, &env, &mut |w| warnings.push(w)).expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        cfg.providers.keys().collect::<Vec<_>>(),
        vec!["cwd_only", "home_only", "shared"]
    );
    let shared = cfg.provider("shared").config;
    assert_eq!(shared.key, "cwd-shared");
    assert_eq!(cfg.mcp_servers["fs"].command, "cwd-fs");
    assert_eq!(cfg.mcp_servers["gh"].url, "https://home/gh");

    // A missing explicit file is silent (os.IsNotExist) and yields an empty config.
    let cfg = Config::load(Some(&dir.path().join("nope.yaml")), &env, &mut |w| {
        warnings.push(w);
    })
    .expect("loads");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());

    // No home and no cwd: nothing is read, nothing is warned.
    let cfg = Config::load(None, &Env::default(), &mut |w| {
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
            "models:\n  bad: {provider: openai, temperature: [not, a, float]}\n",
        ),
        ("empty.yaml", ""),
    ]);
    let env = Env::default().with_dirs(dirs);

    let mut warnings = Vec::new();
    let cfg = Config::load(None, &env, &mut |w| warnings.push(w)).expect("loads");
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

    // A type mismatch on a bool field is a PARSE error: the file is dropped with a warning, because the
    // parser is the only thing that knows what went wrong and the message is its own.
    let (cfg, warnings) = load_explicit(
        &{
            let p = dir.path().join("badbool.yaml");
            fs::write(&p, "agents:\n  x: {models: [m], workspace: 1}\n").unwrap();
            p
        },
        &env,
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].ends_with(" (file ignored)"));
    assert_eq!(cfg, Config::default());

    // Empty file: a valid, empty config.
    let (cfg, warnings) = load_explicit(&dir.path().join("empty.yaml"), &env);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());
}
