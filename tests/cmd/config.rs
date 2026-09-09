//! Config-model integration tests (`config/config_test.go` ported, plus the load-order and parse-error rules).
//! Every test injects `HostDirs` and a map-backed `VarResolver` — nothing reads or mutates the process
//! environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::common::temp_project;
use iota::cmd::{Config, ConfigError, ProviderConfig};
use iota::testing::{MapResolver, map_resolver};
use pretty_assertions::assert_eq;

/// `Load(path)` from the Go tests: the explicit file alone, no home/cwd tiers, warnings collected.
fn load_explicit(path: &Path, resolver: &MapResolver) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let cfg = Config::load(
        Some(path),
        &iota::app::HostDirs::default(),
        resolver,
        &mut |w| warnings.push(w),
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

// Go: config/config_test.go:12
#[test]
fn test_load_tools() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
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
    let (_, pc) = cfg.get("claude");
    let node = pc
        .tools
        .get("shell")
        .expect("claude: shell should be present");
    let allow: Vec<String> = serde_norway::from_value(node.clone()).expect("decode allow");
    assert_eq!(allow, vec!["git".to_owned(), "ssh".to_owned()]);

    // openai: shell present but empty (key exists → enabled, defaults).
    let (_, pc) = cfg.get("openai");
    assert!(
        pc.tools.contains_key("shell"),
        "openai: shell key should be present even when empty"
    );
    assert_eq!(pc.tools.get("shell"), Some(&serde_norway::Value::Null));

    // A provider without a tools block has no enabled tools.
    let (_, pc) = cfg.get("missing");
    assert!(
        pc.tools.is_empty(),
        "missing provider should have no tools, got {:?}",
        pc.tools
    );
}

// Go: config/config_test.go:65
#[test]
fn test_load_agent() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
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
        let (_, pc) = cfg.get(name);
        assert!(pc.agent, "provider {name}: agent should be enabled");
    }
    for name in ["d", "e", "missing"] {
        let (_, pc) = cfg.get(name);
        assert!(!pc.agent, "provider {name}: agent should be disabled");
    }

    // DIVERGENCES I-01: every YAML 1.1 spelling, any case, quoted or plain, for every bool field.
    let cfg = load_yaml(
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
    let (_, pc) = cfg.get("f");
    assert!(pc.agent && pc.image && pc.json_edits);
    assert!(!pc.no_save);
    assert_eq!(pc.notify, Some(false));
}

// Go: config/config_test.go:100
#[test]
fn test_resolve_system() {
    let (dir, _dirs) = temp_project(&[("sys.md", "You are terse.\n")]);
    let f = dir.path().join("sys.md").to_string_lossy().into_owned();

    let inline = ProviderConfig {
        system: "inline".to_owned(),
        system_file: f.clone(),
        ..ProviderConfig::default()
    };
    assert_eq!(
        inline.resolve_system().unwrap(),
        "inline",
        "inline should win"
    );

    let file_only = ProviderConfig {
        system_file: f,
        ..ProviderConfig::default()
    };
    assert_eq!(file_only.resolve_system().unwrap(), "You are terse.\n");

    let missing_path = dir.path().join("missing.md");
    let missing = ProviderConfig {
        system_file: missing_path.to_string_lossy().into_owned(),
        ..ProviderConfig::default()
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

    assert_eq!(ProviderConfig::default().resolve_system().unwrap(), "");
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

    let (cfg, warnings) = load_explicit(&path, &resolver);
    assert!(warnings.is_empty(), "{warnings:?}");
    let (_, pc) = cfg.get("d");
    assert_eq!(pc.key, "sk-expanded");
    assert_eq!(pc.url, "sk-expanded/v1");
    let want = dirs.home.unwrap().join(".iota").join("sys.md");
    assert_eq!(PathBuf::from(&pc.system_file), want);
    assert_eq!(pc.effort, "high");
}

// Go: config/config_test.go:147
#[test]
fn test_mcp_servers_for() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "
providers:
  all:
    type: openai
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

    let (_, pc) = cfg.get("all");
    assert_eq!(pc.mcp_servers, None, "absent key must stay None");
    assert_eq!(
        cfg.mcp_servers_for(&pc).unwrap().len(),
        2,
        "absent key: want all 2"
    );

    let (_, pc) = cfg.get("none");
    assert_eq!(
        pc.mcp_servers,
        Some(vec![]),
        "empty list must stay Some([])"
    );
    assert!(
        cfg.mcp_servers_for(&pc).unwrap().is_empty(),
        "empty list: want 0"
    );

    let (_, pc) = cfg.get("some");
    let got = cfg.mcp_servers_for(&pc).unwrap();
    assert_eq!(got.len(), 1, "subset: want 1");
    assert_eq!(
        got["fs"].command, "fs-server",
        "subset picked the wrong server: {got:?}"
    );

    let (_, pc) = cfg.get("typo");
    let err = cfg
        .mcp_servers_for(&pc)
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
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "providers:\n  tuned:\n    type: openai\n    temperature: 0.3\n  norm:\n    type: openai\n",
    );
    assert_eq!(cfg.get("tuned").1.temperature, Some(0.3));
    assert_eq!(
        cfg.get("norm").1.temperature,
        None,
        "temperature must default None"
    );
}

// Go: config/config_test.go:209
#[test]
fn test_top_p_field() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "providers:\n  tuned:\n    type: openai\n    top_p: 0.9\n  norm:\n    type: openai\n",
    );
    assert_eq!(cfg.get("tuned").1.top_p, Some(0.9));
    assert_eq!(cfg.get("norm").1.top_p, None, "top_p must default None");
}

// Go: config/config_test.go:224
#[test]
fn test_notify_field() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "providers:\n  quiet:\n    type: openai\n    notify: false\n  norm:\n    type: openai\n",
    );
    assert_eq!(
        cfg.get("quiet").1.notify,
        Some(false),
        "notify: false not parsed"
    );
    assert_eq!(
        cfg.get("norm").1.notify,
        None,
        "notify must default None (on)"
    );
}

// Go: config/config_test.go:238
#[test]
fn test_no_save_field() {
    let (dir, _dirs) = temp_project(&[]);
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "providers:\n  eph:\n    type: openai\n    no_save: true\n  norm:\n    type: openai\n",
    );
    assert!(cfg.get("eph").1.no_save, "no_save: true not parsed");
    assert!(!cfg.get("norm").1.no_save, "no_save must default false");
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
    let cfg = load_yaml(
        dir.path(),
        "c.yaml",
        "providers:\n  a:\n    type: anthropic\n    defer_mode: reference\n  b:\n    type: openai\n",
    );
    assert_eq!(cfg.get("a").1.defer_mode, "reference");
    assert_eq!(
        cfg.get("b").1.defer_mode,
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

/// `-c <file>` reads that file ALONE; without it the home file is merged under the cwd file, whole entry by
/// name (config.go:114-137, 186-197).
#[test]
fn config_only_explicit_path_when_given() {
    let (dir, dirs) = temp_project(&[
        (
            "home/.iota.yaml",
            "providers:\n  home_only:\n    type: openai\n    key: home-key\n  shared:\n    type: openai\n    key: home-shared\n    model: home-model\nmcp_servers:\n  fs:\n    command: home-fs\n  gh:\n    url: https://home/gh\n",
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
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg.providers.keys().collect::<Vec<_>>(), vec!["explicit"]);
    assert!(cfg.mcp_servers.is_empty());

    // Discovery: home (.yaml) then cwd (.yml, the .yaml fallback); cwd replaces WHOLE entries by name.
    let cfg = Config::load(None, &dirs, &resolver, &mut |w| warnings.push(w));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        cfg.providers.keys().collect::<Vec<_>>(),
        vec!["cwd_only", "home_only", "shared"]
    );
    let (_, shared) = cfg.get("shared");
    assert_eq!(shared.key, "cwd-shared");
    assert_eq!(
        shared.model, "",
        "no field-level deep merge: the cwd entry replaced the home one"
    );
    assert_eq!(cfg.mcp_servers["fs"].command, "cwd-fs");
    assert_eq!(cfg.mcp_servers["gh"].url, "https://home/gh");

    // A missing explicit file is silent (os.IsNotExist) and yields an empty config.
    let cfg = Config::load(
        Some(&dir.path().join("nope.yaml")),
        &dirs,
        &resolver,
        &mut |w| warnings.push(w),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(cfg, Config::default());

    // No home and no cwd: nothing is read, nothing is warned.
    let cfg = Config::load(None, &iota::app::HostDirs::default(), &resolver, &mut |w| {
        warnings.push(w);
    });
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
    let cfg = Config::load(None, &dirs, &resolver, &mut |w| warnings.push(w));
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
