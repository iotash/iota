//! Pure resolution tests (cmd/root.go:46-123, 439-566; chat/tokens.go:20-43): `resolve_run`, `-l` precedence
//! and formats, `parse_window_size`, the headless flag rejections. Every environment lookup goes through a
//! `map_env`; nothing reads or mutates the process environment or the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// No `mod common;`: these tests need neither a temp project nor TLS (no `reqwest::Client` is ever built), and
// declaring the shared fixtures unused would trip `unused_imports` in the ★ WP00-owned module.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};

use clap::Parser;
use iota::cmd::io::Streams;
use iota::cmd::list::{ListTarget, has_api_key, provider_line, resolve_list, run_list};
use iota::cmd::window::parse_window_size;
use iota::cmd::{Cli, CliError, Config, ProviderConfig, resolve_run};
use iota::testing::map_env;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("iota").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("parse {args:?}: {e}"))
}

fn config(yaml: &str) -> Config {
    Config::parse(
        yaml.as_bytes(),
        &iota::testing::map_resolver(&[]),
        &mut |_| {},
    )
    .expect("test config")
}

fn resolve(
    args: &[&str],
    cfg: &Config,
    env: &[(&str, &str)],
) -> Result<iota::cmd::RunSettings, CliError> {
    resolve_warned(args, cfg, env).0
}

/// The same, keeping the warnings `-M` may have printed.
fn resolve_warned(
    args: &[&str],
    cfg: &Config,
    env: &[(&str, &str)],
) -> (Result<iota::cmd::RunSettings, CliError>, Vec<String>) {
    let mut warnings = Vec::new();
    let r = resolve_run(
        &cli(args),
        cfg,
        &map_env(env),
        &mut std::io::empty(),
        &mut |w| warnings.push(w),
    );
    (r, warnings)
}

/// A `Write` the test can read back after handing it to `Streams` as a `Box<dyn Write + Send>`.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn streams() -> (Streams, SharedBuf, SharedBuf) {
    let out = SharedBuf::default();
    let err = SharedBuf::default();
    let io = Streams {
        stdout: Box::new(out.clone()),
        stderr: Box::new(err.clone()),
    };
    (io, out, err)
}

// Go: chat/tokens_test.go:11
#[test]
fn test_parse_window_size() {
    let ok = [
        ("128000", 128_000),
        ("200k", 200_000),
        ("200K", 200_000),
        ("1m", 1_000_000),
        ("1.5m", 1_500_000),
        ("2b", 2_000_000_000),
        (" 64k ", 64_000),
    ];
    for (input, want) in ok {
        assert_eq!(
            parse_window_size(input),
            Ok(want),
            "ParseWindowSize({input:?})"
        );
    }
    for bad in ["", "abc", "-5", "0", "k", "1x"] {
        assert!(
            parse_window_size(bad).is_err(),
            "ParseWindowSize({bad:?}) expected error"
        );
    }
    // The exact texts (tokens.go:23, 36, 40): `%q` of the post-suffix numeric part.
    assert_eq!(
        parse_window_size("  ").map_err(|e| e.to_string()),
        Err("empty size".to_owned())
    );
    assert_eq!(
        parse_window_size("abc").map_err(|e| e.to_string()),
        Err("invalid context window size: \"abc\"".to_owned())
    );
    assert_eq!(
        parse_window_size("k").map_err(|e| e.to_string()),
        Err("invalid context window size: \"\"".to_owned())
    );
    assert_eq!(
        parse_window_size("-5").map_err(|e| e.to_string()),
        Err("invalid context window size: \"-5\"".to_owned())
    );
    assert_eq!(
        parse_window_size("1x").map_err(|e| e.to_string()),
        Err("invalid context window size: \"1x\"".to_owned())
    );
    assert_eq!(
        parse_window_size("0.4").map_err(|e| e.to_string()),
        Err("invalid context window size".to_owned()),
        "positive but truncates to zero"
    );
}

const ALIAS_CFG: &str = "
providers:
  deepseek:
    type: openai
    key: cfg-key
    url: https://cfg.example/v1
    model: cfg-model
    system: cfg-system
  claude:
    type: anthropic
";

/// root.go:62-92: `-k` (verbatim, even `""`) > env var of the RESOLVED type > config `key:`; url/model/system
/// flag > config; the env var is looked up by type, so an alias reads its type's variable.
#[test]
fn resolve_precedence_key_flag_env_config() {
    let cfg = config(ALIAS_CFG);
    let env = [
        ("OPENAI_API_KEY", "env-key"),
        ("ANTHROPIC_API_KEY", "ant-key"),
    ];

    // env beats config; url/model/system come from the config entry.
    let s = resolve(&["deepseek"], &cfg, &env).unwrap();
    assert_eq!(s.name, "deepseek");
    assert_eq!(s.raw_type, "openai");
    assert_eq!(s.api_key, "env-key");
    assert_eq!(s.base_url, "https://cfg.example/v1");
    assert_eq!(s.model, "cfg-model");
    assert_eq!(s.system, "cfg-system");
    assert_eq!(s.message, None);
    assert_eq!(s.temperature, None);
    assert!(!s.agent_mode);
    assert_eq!(s.max_turns, None);
    assert_eq!(s.output_format_raw, None);
    assert_eq!(s.resolved.provider, cfg.providers["deepseek"]);

    // The flag beats the environment; every other flag beats its config field.
    let s = resolve(
        &[
            "deepseek",
            "-k",
            "flag-key",
            "-u",
            "https://flag",
            "-M",
            "flag-model",
            "-s",
            "flag-system",
            "-m",
            "hi",
            "--agent",
            "--max-turns",
            "-1",
            "--output-format",
            "json",
        ],
        &cfg,
        &env,
    )
    .unwrap();
    assert_eq!(s.api_key, "flag-key");
    assert_eq!(s.base_url, "https://flag");
    assert_eq!(s.model, "flag-model");
    assert_eq!(s.system, "flag-system");
    assert_eq!(s.message.as_deref(), Some("hi"));
    assert!(s.agent_mode);
    assert_eq!(s.max_turns, None);
    assert_eq!(s.output_format_raw.as_deref(), Some("json"));

    // `-k ""` is used verbatim: neither env nor config rescues it.
    let err = resolve(&["deepseek", "-k", ""], &cfg, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "API key is required: use -k/--key or set OPENAI_API_KEY"
    );
    assert!(matches!(err, CliError::ApiKeyRequired("OPENAI_API_KEY")));

    // Without the env var, the config key applies; `-u ""` / `-s ""` are verbatim (Changed) too.
    let s = resolve(&["deepseek", "-u", "", "-s", ""], &cfg, &[]).unwrap();
    assert_eq!(s.api_key, "cfg-key");
    assert_eq!(s.base_url, "");
    assert_eq!(s.system, "");

    // An empty env value counts as unset (Go `os.Getenv(..) != ""`).
    let s = resolve(&["deepseek"], &cfg, &[("OPENAI_API_KEY", "")]).unwrap();
    assert_eq!(s.api_key, "cfg-key");

    // The env var follows the resolved TYPE: an anthropic alias reads ANTHROPIC_API_KEY.
    let s = resolve(&["claude"], &cfg, &env).unwrap();
    assert_eq!(s.api_key, "ant-key");
    let err = resolve(&["claude"], &cfg, &[]).unwrap_err();
    assert!(matches!(err, CliError::ApiKeyRequired("ANTHROPIC_API_KEY")));

    // Unconfigured built-in types read their own variable; the unknown-type fallback is the literal API_KEY.
    let s = resolve(&["gemini"], &cfg, &[("GOOGLE_API_KEY", "g")]).unwrap();
    assert_eq!((s.raw_type.as_str(), s.api_key.as_str()), ("gemini", "g"));
    assert_eq!(s.resolved.provider, ProviderConfig::default());
    let odd = config("providers:\n  odd:\n    type: custom\n");
    let err = resolve(&["odd"], &odd, &[("API_KEY", "")]).unwrap_err();
    assert!(matches!(err, CliError::ApiKeyRequired("API_KEY")));
    let s = resolve(&["odd"], &odd, &[("API_KEY", "generic")]).unwrap();
    assert_eq!(s.api_key, "generic");

    // No provider argument at all.
    let err = resolve(&[], &cfg, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "provider argument is required (e.g. openai, anthropic, gemini), or use -l to list available providers"
    );

    // A `system_file` that cannot be read aborts (after the name check, before the key check).
    let bad = config("providers:\n  openai:\n    system_file: /nonexistent/iota-sys.md\n");
    let err = resolve(&["openai"], &bad, &[("OPENAI_API_KEY", "k")]).unwrap_err();
    assert!(matches!(err, CliError::Config(_)), "{err}");
    assert!(
        err.to_string()
            .starts_with("system_file: open /nonexistent/iota-sys.md: "),
        "{err}"
    );
    // `-s` skips the file entirely.
    let s = resolve(&["openai", "-s", "x"], &bad, &[("OPENAI_API_KEY", "k")]).unwrap();
    assert_eq!(s.system, "x");
}

/// A reader that fails on the first read.
struct FailingStdin;

impl Read for FailingStdin {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

/// root.go:95-104: only the literal `-m -` reads stdin; the text is trimmed; whitespace-only and read failures
/// are errors with the Go texts.
#[test]
fn resolve_stdin_message_trimmed_and_empty_error() {
    let cfg = Config::default();
    let env = map_env(&[("OPENAI_API_KEY", "k")]);
    let args = cli(&["openai", "-M", "gpt", "-m", "-"]);

    let s = resolve_run(
        &args,
        &cfg,
        &env,
        &mut "  hello\n  world \n\n".as_bytes(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(s.message.as_deref(), Some("hello\n  world"));

    let err = resolve_run(&args, &cfg, &env, &mut " \n\t".as_bytes(), &mut |_| {}).unwrap_err();
    assert_eq!(err.to_string(), "no message provided via stdin");
    assert!(matches!(err, CliError::EmptyStdin));

    let err = resolve_run(&args, &cfg, &env, &mut FailingStdin, &mut |_| {}).unwrap_err();
    assert_eq!(err.to_string(), "failed to read from stdin: boom");
    assert!(matches!(err, CliError::Stdin(_)));

    // Any other -m value is used as-is (not trimmed) and never touches stdin.
    let args = cli(&["openai", "-M", "gpt", "-m", "  spaced  "]);
    let s = resolve_run(&args, &cfg, &env, &mut FailingStdin, &mut |_| {}).unwrap();
    assert_eq!(s.message.as_deref(), Some("  spaced  "));

    // The stdin read happens AFTER the key check: no key → the key error, stdin untouched.
    let args = cli(&["openai", "-M", "gpt", "-m", "-"]);
    let err = resolve_run(&args, &cfg, &map_env(&[]), &mut FailingStdin, &mut |_| {}).unwrap_err();
    assert!(matches!(err, CliError::ApiKeyRequired(_)));
}

/// POLICY F-03: `-m ""` is an error, not an interactive run.
#[test]
fn resolve_message_empty_is_error() {
    let cfg = Config::default();
    let err = resolve(
        &["openai", "-M", "gpt", "-m", ""],
        &cfg,
        &[("OPENAI_API_KEY", "k")],
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "--message must not be empty");
    assert!(matches!(err, CliError::MessageEmpty));
    // …even without a model: the message rule precedes the model rule.
    let err = resolve(&["openai", "-m", ""], &cfg, &[("OPENAI_API_KEY", "k")]).unwrap_err();
    assert!(matches!(err, CliError::MessageEmpty));
}

/// No `-m` → `message: None` and NO `ModelRequired` (Go: `chatMessage == ""` is the interactive branch; `run`
/// takes the interactive branch at root.go:259). With `-m`, a model is required (root.go:107-109).
#[test]
fn resolve_message_absent_is_none() {
    let cfg = Config::default();
    let env = [("OPENAI_API_KEY", "k")];
    let s = resolve(&["openai"], &cfg, &env).unwrap();
    assert_eq!(s.message, None);
    assert_eq!(s.model, "");

    let err = resolve(&["openai", "-m", "hi"], &cfg, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "--model/-M is required when using --message/-m"
    );
    assert!(matches!(err, CliError::ModelRequired));

    // The config model satisfies the rule.
    let cfg = config("providers:\n  openai:\n    model: cfg-model\n");
    let s = resolve(&["openai", "-m", "hi"], &cfg, &env).unwrap();
    assert_eq!(s.model, "cfg-model");
    assert_eq!(s.message.as_deref(), Some("hi"));

    // `--output-format` is carried RAW in every mode — even a bad value resolves: the parse belongs to `run`
    // at root.go:249-252's position (after tuning/MCP), so `unknown output format …` keeps Go's
    // precedence (tests/cli.rs pins both the text and the order), and root.go:253 belongs to `run` too.
    let s = resolve(&["openai", "--output-format", " text "], &cfg, &env).unwrap();
    assert_eq!(s.output_format_raw.as_deref(), Some(" text "));
    let s = resolve(&["openai", "--output-format", "xml"], &cfg, &env).unwrap();
    assert_eq!(s.output_format_raw.as_deref(), Some("xml"));
}

/// root.go:114-123: the `-t` flag is passed through unchecked; the config `temperature:` must be 0.0-2.0.
#[test]
fn resolve_config_temperature_range_checked_flag_not() {
    let env = [("OPENAI_API_KEY", "k")];
    let hot = config("providers:\n  openai:\n    temperature: 3.5\n");
    let err = resolve(&["openai"], &hot, &env).unwrap_err();
    assert_eq!(err.to_string(), "config temperature 3.5: want 0.0-2.0");
    assert!(matches!(err, CliError::ConfigTemperature(t) if t.to_bits() == 3.5_f64.to_bits()));

    let neg = config("providers:\n  openai:\n    temperature: -0.5\n");
    assert_eq!(
        resolve(&["openai"], &neg, &env).unwrap_err().to_string(),
        "config temperature -0.5: want 0.0-2.0"
    );

    // The flag wins and is never range-checked (even -t 7 or -t 0).
    let s = resolve(&["openai", "-t", "7"], &hot, &env).unwrap();
    assert_eq!(s.temperature, Some(7.0));
    let s = resolve(&["openai", "-t", "0"], &hot, &env).unwrap();
    assert_eq!(s.temperature, Some(0.0));

    // In-range config values pass; the bounds are inclusive; no source → None.
    let warm = config("providers:\n  openai:\n    temperature: 0.7\n");
    assert_eq!(
        resolve(&["openai"], &warm, &env).unwrap().temperature,
        Some(0.7)
    );
    let edge = config("providers:\n  openai:\n    temperature: 2\n");
    assert_eq!(
        resolve(&["openai"], &edge, &env).unwrap().temperature,
        Some(2.0)
    );
    assert_eq!(
        resolve(&["openai"], &Config::default(), &env)
            .unwrap()
            .temperature,
        None
    );

    // `-t` must be a number (clap's own argument error, exit 2 territory).
    assert!(Cli::try_parse_from(["iota", "openai", "-t", "x"]).is_err());
}

/// root.go:534-549: the multi-line unknown-provider text, with the configured aliases sorted when there are any.
#[test]
fn resolve_unknown_provider_text_with_sorted_aliases() {
    let cfg = config(
        "providers:\n  zeta:\n    type: openai\n  alpha:\n    type: anthropic\n  mid:\n    type: gemini\n",
    );
    let err = resolve(&["opnai"], &cfg, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown provider \"opnai\": not a configured alias or a built-in type\n  configured aliases: alpha, mid, zeta\n  built-in types: openai, anthropic, gemini, vertexai, openresponses, imagen, images"
    );
    assert!(matches!(err, CliError::UnknownProvider { .. }));

    // No aliases: no hint line at all.
    let err = resolve(&["opnai"], &Config::default(), &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown provider \"opnai\": not a configured alias or a built-in type\n  built-in types: openai, anthropic, gemini, vertexai, openresponses, imagen, images"
    );

    // The name check precedes the key check: no misleading "API key is required" for a typo.
    assert!(matches!(
        resolve(&["opnai"], &cfg, &[]).unwrap_err(),
        CliError::UnknownProvider { .. }
    ));

    // A configured alias whose `type:` is not a built-in passes the name check (it is configured) and is
    // rejected later, at construction (provider.go:329 text via `ProviderKind::from_str`).
    let odd = config("providers:\n  odd:\n    type: custom\n    key: k\n");
    let s = resolve(&["odd"], &odd, &[]).unwrap();
    assert_eq!(s.raw_type, "custom");
    let err: CliError = s
        .raw_type
        .parse::<iota::provider::ProviderKind>()
        .unwrap_err()
        .into();
    assert_eq!(
        err.to_string(),
        "unknown provider type: custom (supported: openai, anthropic, gemini, vertexai, openresponses, imagen, images)"
    );

    // Every built-in type is accepted unconfigured.
    for t in [
        "openai",
        "anthropic",
        "gemini",
        "vertexai",
        "openresponses",
        "imagen",
        "images",
    ] {
        let s = resolve(&[t, "-k", "k"], &Config::default(), &[]).unwrap();
        assert_eq!(s.raw_type, t);
    }
}

/// `-l <provider>` precedence (root.go:485-508): `-k ""` beats the environment (the key error), `-u` beats
/// `provider_cfg.url`; and the listing without a provider only shows configured entries that have a key.
#[tokio::test]
async fn list_key_flag_wins() {
    let cfg = config(ALIAS_CFG);
    let env = map_env(&[("OPENAI_API_KEY", "env-key")]);
    let cancel = CancellationToken::new();

    // Pure half: env > config; -u > provider_cfg.url; -k verbatim.
    assert_eq!(
        resolve_list(&cli(&["-l", "deepseek"]), &cfg, &env).unwrap(),
        ListTarget {
            name: "deepseek".to_owned(),
            raw_type: "openai".to_owned(),
            api_key: "env-key".to_owned(),
            base_url: "https://cfg.example/v1".to_owned(),
        }
    );
    assert_eq!(
        resolve_list(
            &cli(&["-l", "deepseek", "-k", "flag", "-u", "https://flag"]),
            &cfg,
            &env
        )
        .unwrap(),
        ListTarget {
            name: "deepseek".to_owned(),
            raw_type: "openai".to_owned(),
            api_key: "flag".to_owned(),
            base_url: "https://flag".to_owned(),
        }
    );
    assert_eq!(
        resolve_list(&cli(&["-l", "deepseek"]), &cfg, &map_env(&[]))
            .unwrap()
            .api_key,
        "cfg-key"
    );
    let err = resolve_list(&cli(&["-l", "deepseek", "-k", ""]), &cfg, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "API key is required to list models: use -k/--key or set OPENAI_API_KEY"
    );
    assert!(matches!(
        err,
        CliError::ApiKeyRequiredForList("OPENAI_API_KEY")
    ));

    // `run_list` with a provider stops at the same errors before any provider is built (no network).
    let (mut io, out, errs) = streams();
    let err = run_list(
        &cli(&["-l", "deepseek", "-k", ""]),
        &cfg,
        &env,
        &cancel,
        None,
        &mut io,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        CliError::ApiKeyRequiredForList("OPENAI_API_KEY")
    ));
    let err = run_list(
        &cli(&["-l", "claude"]),
        &cfg,
        &map_env(&[]),
        &cancel,
        None,
        &mut io,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        CliError::ApiKeyRequiredForList("ANTHROPIC_API_KEY")
    ));
    let err = run_list(&cli(&["-l", "opnai"]), &cfg, &env, &cancel, None, &mut io)
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::UnknownProvider { .. }));
    let err = run_list(
        &cli(&["-l", "odd", "-k", "k"]),
        &config("providers:\n  odd:\n    type: custom\n"),
        &env,
        &cancel,
        None,
        &mut io,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CliError::UnknownType(_)), "{err}");
    assert_eq!(out.text(), "");
    assert_eq!(errs.text(), "", "nothing was fetched");

    // No provider: configured entries with a key, sorted, in the Go line format.
    let listing = config(
        "providers:\n  zeta:\n    type: openai\n    model: m\n  openai:\n    key: k\n    model: gpt\n  anthropic: {}\n  gemini:\n    type: gemini\n    key: k\n",
    );
    let (mut io, out, errs) = streams();
    run_list(&cli(&["-l"]), &listing, &env, &cancel, None, &mut io)
        .await
        .unwrap();
    assert_eq!(
        out.text(),
        "Available providers:\n  gemini\n  openai (default model: gpt)\n  zeta (type: openai, model: m)\n"
    );
    assert_eq!(errs.text(), "");

    // Nothing usable (a bare env var without a config entry is NOT listed — DIVERGENCES D-21).
    let (mut io, out, _) = streams();
    run_list(
        &cli(&["-l"]),
        &Config::default(),
        &env,
        &cancel,
        None,
        &mut io,
    )
    .await
    .unwrap();
    assert_eq!(
        out.text(),
        "No providers configured. Set API keys via environment variables or ~/.iota.yaml\n"
    );

    // has_api_key: config key, else the env var of the TYPE.
    let (t, provider_cfg) = cfg.get("deepseek");
    assert!(has_api_key(&t, &provider_cfg, &map_env(&[])));
    let (t, provider_cfg) = cfg.get("claude");
    assert!(!has_api_key(&t, &provider_cfg, &env));
    assert!(has_api_key(
        &t,
        &provider_cfg,
        &map_env(&[("ANTHROPIC_API_KEY", "x")])
    ));
    assert!(!has_api_key(
        &t,
        &provider_cfg,
        &map_env(&[("ANTHROPIC_API_KEY", "")])
    ));
}

/// DIVERGENCES D-23: the interactive-only flags are accepted by the parser and rejected by `reject_unsupported`
/// with the headless text. `--resume` left this set in phase 2 slice 1 — see `blank_resume_rejected`.
#[test]
fn unsupported_flags_rejected() {
    let cases: [(&[&str], &str); 3] = [
        (&["openai", "-S"], "-S/--system-input"),
        (&["openai", "--system-input"], "-S/--system-input"),
        (&["openai", "--no-save"], "--no-save"),
    ];
    for (args, flag) in cases {
        let err = cli(args).reject_unsupported().unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("flag {flag} is not supported in headless mode"),
            "{args:?}"
        );
        assert!(matches!(err, CliError::UnsupportedFlag(f) if f == flag));
    }
    // -S is checked first when several are given.
    assert!(matches!(
        cli(&["openai", "--no-save", "-S"])
            .reject_unsupported()
            .unwrap_err(),
        CliError::UnsupportedFlag("-S/--system-input")
    ));
    assert!(cli(&["openai", "-m", "hi"]).reject_unsupported().is_ok());

    // Parser shape (cmd/root.go:418-436): bare --resume yields " ", --mcp appends without comma splitting,
    // -M is the model, --max-turns takes a negative value, --context-window and --agent are accepted.
    let c = cli(&["openai", "--resume"]);
    assert_eq!(c.resume.as_deref(), Some(" "));
    let c = cli(&[
        "openai",
        "--mcp",
        "a b,c",
        "--mcp",
        "http://x",
        "-M",
        "gpt",
        "--max-turns",
        "-3",
        "--context-window",
        "200k",
        "--agent",
        "-c",
        "/tmp/x.yaml",
    ]);
    assert_eq!(c.mcp, vec!["a b,c".to_owned(), "http://x".to_owned()]);
    assert_eq!(c.model.as_deref(), Some("gpt"));
    assert_eq!(c.max_turns, -3);
    assert_eq!(c.context_window.as_deref(), Some("200k"));
    assert!(c.agent);
    assert_eq!(
        c.config.as_deref(),
        Some(std::path::Path::new("/tmp/x.yaml"))
    );
    assert_eq!(c.resume, None);
    // Two positionals are a parser error (cobra RangeArgs(0, 1); DIVERGENCES D-24).
    assert!(Cli::try_parse_from(["iota", "openai", "extra"]).is_err());
}

/// DIVERGENCES D-42: the BLANK `--resume` forms get their own text (the picker is what is missing), while a
/// valued `--resume=<id>` passes `reject_unsupported` untouched. Check order is Go's (root.go:284-286), so
/// `-S` and `--no-save` still win when combined with a resume.
#[test]
fn blank_resume_rejected() {
    for args in [&["openai", "--resume"][..], &["openai", "--resume="][..]] {
        let err = cli(args).reject_unsupported().unwrap_err();
        assert_eq!(
            err.to_string(),
            "--resume requires a session id in headless mode (--resume=<id>)",
            "{args:?}"
        );
        assert!(matches!(err, CliError::ResumeNeedsId), "{args:?}");
    }
    // Whitespace only is blank too — it is exactly what the bare form's sentinel is.
    assert!(matches!(
        cli(&["openai", "--resume=  "])
            .reject_unsupported()
            .unwrap_err(),
        CliError::ResumeNeedsId
    ));
    // A valued resume is supported now (D-41).
    assert!(
        cli(&["openai", "--resume=abc"])
            .reject_unsupported()
            .is_ok()
    );
    // root.go:284-286 precedence: --no-save is reported against a resume, and -S beats both.
    assert!(matches!(
        cli(&["openai", "--no-save", "--resume=abc"])
            .reject_unsupported()
            .unwrap_err(),
        CliError::UnsupportedFlag("--no-save")
    ));
    assert!(matches!(
        cli(&["openai", "-S", "--resume=abc"])
            .reject_unsupported()
            .unwrap_err(),
        CliError::UnsupportedFlag("-S/--system-input")
    ));
    // Even the blank form loses to both, so the older texts keep their precedence.
    assert!(matches!(
        cli(&["openai", "--no-save", "--resume"])
            .reject_unsupported()
            .unwrap_err(),
        CliError::UnsupportedFlag("--no-save")
    ));
}

/// CONTRACTS S§4.1 (design §3.3): pflag binds a `NoOptDefVal` flag's value ONLY in the `--resume=<id>` form, so
/// a space-separated token stays a positional. `iota --resume 50tnb -m hi` is Go's `provider="50tnb"` plus a
/// BARE resume — without `require_equals` clap would swallow `50tnb` as the flag's value and leave no provider.
#[test]
fn resume_space_form_stays_positional() {
    let c = cli(&["--resume", "50tnb", "-m", "hi"]);
    assert_eq!(c.provider.as_deref(), Some("50tnb"));
    assert_eq!(c.resume.as_deref(), Some(" "));
    // …and being blank, that resume is the D-42 error rather than a session lookup for "50tnb".
    assert!(matches!(
        c.reject_unsupported().unwrap_err(),
        CliError::ResumeNeedsId
    ));
    // The equals form binds, and only the equals form: the value is taken verbatim, spaces and all.
    let c = cli(&["openai", "--resume=50tnb", "-m", "hi"]);
    assert_eq!(c.provider.as_deref(), Some("openai"));
    assert_eq!(c.resume.as_deref(), Some("50tnb"));
}

/// DIVERGENCES D-52: `--model/-M is required when using --message/-m` is DEFERRED while `--resume` is present
/// (a session supplies the model from its meta); `run` re-raises it byte-identically after the replay. Without
/// a resume the error is unchanged, and `RunSettings.resume` is the TRIMMED fragment.
#[test]
fn resolve_resume_defers_model_required() {
    let cfg = config("providers: {}");
    // No -M and no config model, but a resume is given: resolution succeeds and carries the fragment.
    let s = resolve(
        &["openai", "-k", "sk", "-m", "hi", "--resume=abc"],
        &cfg,
        &[],
    )
    .unwrap();
    assert_eq!(s.model, "");
    assert_eq!(s.resume.as_deref(), Some("abc"));
    // root.go:288 `strings.TrimSpace(resumeID)`.
    let s = resolve(
        &["openai", "-k", "sk", "-m", "hi", "--resume=  abc  "],
        &cfg,
        &[],
    )
    .unwrap();
    assert_eq!(s.resume.as_deref(), Some("abc"));
    // A blank fragment never reaches the store (it died in `reject_unsupported`); if one did, it is absent.
    let s = resolve(
        &["openai", "-k", "sk", "-M", "gpt", "-m", "hi", "--resume= "],
        &cfg,
        &[],
    )
    .unwrap();
    assert_eq!(s.resume, None);
    // Without --resume the deferral does not apply.
    assert!(matches!(
        resolve(&["openai", "-k", "sk", "-m", "hi"], &cfg, &[]).unwrap_err(),
        CliError::ModelRequired
    ));
    // The flag is absent from every other run.
    let s = resolve(&["openai", "-k", "sk", "-M", "gpt", "-m", "hi"], &cfg, &[]).unwrap();
    assert_eq!(s.resume, None);
}

/// root.go:455-467: the `-l` line for an alias (type, optional url/model) and for a same-name entry. The
/// model comes from the same-named `models:` entry, which is where a one-layer `model:` lands.
#[test]
fn provider_line_formats() {
    let provider_cfg = |url: &str| ProviderConfig {
        url: url.to_owned(),
        ..ProviderConfig::default()
    };
    assert_eq!(
        provider_line(
            "deepseek",
            "openai",
            &provider_cfg("https://api.deepseek.com/v1"),
            "deepseek-chat"
        ),
        "deepseek (type: openai, url: https://api.deepseek.com/v1, model: deepseek-chat)"
    );
    assert_eq!(
        provider_line("deepseek", "openai", &provider_cfg(""), "deepseek-chat"),
        "deepseek (type: openai, model: deepseek-chat)"
    );
    assert_eq!(
        provider_line("deepseek", "openai", &provider_cfg("https://x"), ""),
        "deepseek (type: openai, url: https://x)"
    );
    assert_eq!(
        provider_line("deepseek", "openai", &provider_cfg(""), ""),
        "deepseek (type: openai)"
    );
    assert_eq!(
        provider_line("openai", "openai", &provider_cfg(""), ""),
        "openai"
    );
    assert_eq!(
        provider_line(
            "openai",
            "openai",
            &provider_cfg("https://ignored"),
            "gpt-4o"
        ),
        "openai (default model: gpt-4o)",
        "a same-name entry never prints its url"
    );

    // The id the listing shows is the migrated `model:` — and only when the entry belongs to that provider.
    let cfg = config(
        "providers:\n  deepseek: {type: openai}\n  other: {type: openai}\nmodels:\n  deepseek: deepseek:chat-v3\n  other: deepseek:not-mine\n",
    );
    assert_eq!(cfg.default_model_id("deepseek"), "chat-v3");
    assert_eq!(
        cfg.default_model_id("other"),
        "",
        "an entry that points at another provider is not that provider's default"
    );
    assert_eq!(cfg.default_model_id("nosuch"), "");
}

// ---------------------------------------------------------------- `-M` against the candidate set

/// `-M` takes a bare id, a `provider:id` pair (which MOVES the run to that provider, key and URL included)
/// and `provider:*` (that provider, no model chosen). A candidate entry may also be named directly.
#[test]
fn model_flag_forms() {
    let cfg = config(
        "
providers:
  anthropic: {key: ak}
  relay: {type: openai, key: rk, url: https://relay/v1}
models:
  sonnet: anthropic:claude-sonnet-4
  gpt5: {provider: relay, id: gpt-5.2, defer_mode: system-tools}
agents:
  team:
    models: [sonnet, gpt5]
",
    );

    // No flag: the first candidate.
    let s = resolve(&["team"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("anthropic", "claude-sonnet-4")
    );
    assert_eq!(s.api_key, "ak");

    // A candidate by name brings its provider, its key, its url AND its protocol with it.
    let (s, warnings) = resolve_warned(&["team", "-M", "gpt5"], &cfg, &[]);
    let s = s.unwrap();
    assert_eq!(s.raw_type, "openai");
    assert_eq!(s.model, "gpt-5.2");
    assert_eq!(s.api_key, "rk");
    assert_eq!(s.base_url, "https://relay/v1");
    assert_eq!(s.resolved.model.defer_mode, "system-tools");
    assert!(
        warnings.is_empty(),
        "a candidate is not a surprise: {warnings:?}"
    );

    // `provider:id` moves the run to that provider even when nothing configured that pair.
    let (s, warnings) = resolve_warned(&["team", "-M", "relay:o3-mini"], &cfg, &[]);
    let s = s.unwrap();
    assert_eq!(s.raw_type, "openai");
    assert_eq!(s.model, "o3-mini");
    assert_eq!(s.base_url, "https://relay/v1");
    assert_eq!(
        warnings,
        vec![
            "Warning: model relay:o3-mini is not in agent \"team\"'s models (using it anyway)"
                .to_owned()
        ]
    );

    // `provider:*` is that provider with nothing chosen — the picker's job.
    let s = resolve(&["team", "-M", "relay:*"], &cfg, &[]).unwrap();
    assert_eq!((s.raw_type.as_str(), s.model.as_str()), ("openai", ""));

    // A bare id stays on the provider the run resolved to.
    let (s, warnings) = resolve_warned(&["team", "-M", "claude-opus-5"], &cfg, &[]);
    assert_eq!(s.unwrap().raw_type, "anthropic");
    assert_eq!(warnings.len(), 1, "{warnings:?}");

    // An id whose colon names nothing iota knows is left alone — relays put colons in model ids.
    let s = resolve(&["team", "-M", "vendor:weird:id"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("anthropic", "vendor:weird:id")
    );
}

/// The candidate warning is advice about a declared set. A migrated one-layer block has an implicit set of
/// one, which was never advice, so `-M` on it stays as quiet as it always was.
#[test]
fn model_flag_is_quiet_without_a_declared_candidate_set() {
    let cfg =
        config("providers:\n  deepseek:\n    type: openai\n    key: k\n    model: deepseek-chat\n");
    let (s, warnings) = resolve_warned(&["deepseek", "-M", "anything-else"], &cfg, &[]);
    assert_eq!(s.unwrap().model, "anything-else");
    assert!(warnings.is_empty(), "{warnings:?}");

    // A provider name with no config at all has no set either.
    let (s, warnings) = resolve_warned(
        &["openai", "-k", "k", "-M", "gpt-4o"],
        &Config::default(),
        &[],
    );
    assert_eq!(s.unwrap().model, "gpt-4o");
    assert!(warnings.is_empty(), "{warnings:?}");
}

// ---------------------------------------------------------------- the implicit `agents.default`

/// A run with no positional argument takes `agents.default` when the config DECLARES one — the whole entry,
/// not just its model.
#[test]
fn a_declared_default_agent_runs_without_a_positional_argument() {
    let cfg = config(
        "
providers:
  anthropic: {key: ak}
  openai: {key: pk}
models:
  sonnet: anthropic:claude-sonnet-4
agents:
  default:
    models: [sonnet]
    system: the default prompt
    workspace: true
",
    );

    let s = resolve(&[], &cfg, &[]).unwrap();
    assert_eq!(s.name, iota::cmd::DEFAULT_AGENT);
    assert_eq!(s.raw_type, "anthropic");
    assert_eq!(s.model, "claude-sonnet-4");
    assert_eq!(s.system, "the default prompt");
    assert_eq!(s.api_key, "ak");
    assert!(s.agent_mode, "the default agent's workspace: true applies");
    assert_eq!(s.resolved.agent_name, iota::cmd::DEFAULT_AGENT);

    // Naming it explicitly means the same thing.
    assert_eq!(resolve(&["default"], &cfg, &[]).unwrap(), s);

    // The flags still apply to it.
    let s = resolve(&["-M", "openai:gpt-4o"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("openai", "gpt-4o")
    );
    assert_eq!(s.api_key, "pk");
}

/// A positional argument ALWAYS wins over the fallback — it is a fallback, not a default overlay.
#[test]
fn a_positional_argument_beats_the_default_agent() {
    let cfg = config(
        "
providers:
  anthropic: {key: ak}
  openai: {key: pk}
agents:
  default:
    models: [\"anthropic:claude-x\"]
    system: the default prompt
    workspace: true
  other:
    models: [\"openai:gpt-4o\"]
    system: the other prompt
",
    );

    let s = resolve(&["other"], &cfg, &[]).unwrap();
    assert_eq!(s.name, "other");
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("openai", "gpt-4o")
    );
    assert_eq!(s.system, "the other prompt");
    assert!(!s.agent_mode, "nothing of the default leaks in");

    // A bare provider name reaches no agent at all, exactly as it does without a default configured.
    let s = resolve(&["openai"], &cfg, &[]).unwrap();
    assert_eq!(s.system, "");
    assert_eq!(s.model, "");
    assert!(!s.agent_mode);
}

/// Without a declared `agents.default` the refusal is Go's, unchanged to the byte.
#[test]
fn no_default_agent_keeps_the_provider_required_text() {
    const WANT: &str = "provider argument is required (e.g. openai, anthropic, gemini), or use -l to list available providers";

    for cfg in [
        Config::default(),
        // A config with agents, models and providers — just not a `default` agent.
        config(
            "providers:\n  openai: {key: k}\nmodels:\n  default: openai:gpt-4o\nagents:\n  coder:\n    models: [default]\n",
        ),
    ] {
        let err = resolve(&[], &cfg, &[]).unwrap_err();
        assert_eq!(err.to_string(), WANT);
        assert!(matches!(err, CliError::ProviderRequired));
    }

    // `models.default` and `providers.default` are NOT fallbacks: one entry point, and only the one that
    // says how to drive a model.
    let cfg = config("providers:\n  default: {type: openai, key: k}\n");
    assert!(matches!(
        resolve(&[], &cfg, &[]).unwrap_err(),
        CliError::ProviderRequired
    ));
    assert_eq!(
        resolve(&["default"], &cfg, &[]).unwrap().raw_type,
        "openai",
        "…while naming it still works"
    );
}

/// An `agents.default` the MIGRATION synthesised from a one-layer `providers.default` block is the old shape
/// of a provider entry, not a declaration of intent: it never becomes the implicit default, so a one-layer
/// config keeps failing exactly as it did.
#[test]
fn a_migrated_default_agent_is_not_a_declared_one() {
    let cfg = config(
        "providers:\n  default:\n    type: openai\n    key: k\n    model: gpt-4o\n    system: from the old block\n    agent: true\n",
    );
    // The migration DID produce the entry…
    assert!(cfg.agents.contains_key("default"));
    assert_eq!(cfg.agents["default"].system, "from the old block");
    // …and naming it works, with everything the block configured.
    let s = resolve(&["default"], &cfg, &[]).unwrap();
    assert_eq!(s.model, "gpt-4o");
    assert_eq!(s.system, "from the old block");
    assert!(s.agent_mode);
    // …but it is not what a bare `iota` falls back to.
    assert!(matches!(
        resolve(&[], &cfg, &[]).unwrap_err(),
        CliError::ProviderRequired
    ));

    // A file that DECLARES `agents.default` alongside the one-layer block does become the fallback.
    let cfg = config(
        "providers:\n  default:\n    type: openai\n    key: k\n    model: gpt-4o\nagents:\n  default:\n    models: [\"default:gpt-5.2\"]\n    system: declared\n",
    );
    let s = resolve(&[], &cfg, &[]).unwrap();
    assert_eq!(s.model, "gpt-5.2");
    assert_eq!(s.system, "declared");
}

/// The four-level resolution as the command sees it: an agent shadows a model shadows a provider, and the
/// agent's switches reach `RunSettings`.
#[test]
fn positional_resolution_reaches_run_settings() {
    let cfg = config(
        "
providers:
  openai: {key: pk}
  anthropic: {key: ak}
models:
  openai: {provider: anthropic, id: from-models}
agents:
  openai:
    models: [\"anthropic:from-agents\"]
    system: agent prompt
    workspace: true
",
    );
    let s = resolve(&["openai"], &cfg, &[]).unwrap();
    assert_eq!(
        s.raw_type, "anthropic",
        "the agent's model decides the endpoint"
    );
    assert_eq!(s.model, "from-agents");
    assert_eq!(s.system, "agent prompt");
    assert_eq!(s.api_key, "ak");
    assert!(s.agent_mode, "workspace: true is --agent");

    // Without the agent, the model entry wins over the provider of the same name.
    let cfg = config(
        "providers:\n  openai: {key: pk}\n  anthropic: {key: ak}\nmodels:\n  openai: {provider: anthropic, id: from-models}\n",
    );
    let s = resolve(&["openai"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("anthropic", "from-models")
    );
    assert!(!s.agent_mode);

    // …and without the model entry, the provider itself.
    let cfg = config("providers:\n  openai: {key: pk}\n");
    let s = resolve(&["openai"], &cfg, &[]).unwrap();
    assert_eq!((s.raw_type.as_str(), s.api_key.as_str()), ("openai", "pk"));
    assert_eq!(s.model, "");

    // An unknown name lists every configured name once, sorted across the three layers.
    let cfg = config(
        "providers:\n  zeta: {type: openai}\nmodels:\n  mid: openai:gpt-4o\nagents:\n  alpha:\n    models: [mid]\n",
    );
    let err = resolve(&["opnai"], &cfg, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown provider \"opnai\": not a configured alias or a built-in type\n  configured aliases: alpha, mid, zeta\n  built-in types: openai, anthropic, gemini, vertexai, openresponses, imagen, images"
    );
}
