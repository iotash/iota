//! Pure resolution tests (cmd/root.go:46-123, 439-566; chat/tokens.go:20-43): the verb set, `resolve_run`,
//! the listings and `parse_window_size`. Every environment lookup goes through a `map_env`; nothing reads or
//! mutates the process environment or the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// No `mod common;`: these tests need neither a temp project nor TLS (no `reqwest::Client` is ever built), and
// declaring the shared fixtures unused would trip `unused_imports` in the ★ WP00-owned module.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};

use clap::Parser;
use iota::cmd::io::Streams;
use iota::cmd::list::{provider_line, run_list};
use iota::cmd::window::parse_window_size;
use iota::cmd::{Cli, CliError, Command, Config, Invocation, ProviderConfig, Resume, resolve_run};
use iota::testing::map_env;
use pretty_assertions::assert_eq;

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("iota").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("parse {args:?}: {e}"))
}

/// The `run`/`resume` invocation `args` means (every other verb panics — those tests name their own).
fn inv(args: &[&str]) -> Invocation {
    match cli(args).into_command() {
        (Command::Run(cmd), config) => Invocation::of_run(cmd, config),
        (Command::Resume(cmd), config) => Invocation::of_resume(cmd, config),
        (other, _) => panic!("{args:?} is not a run: {other:?}"),
    }
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
        &inv(args),
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

/// Runs `iota list …` against `cfg` and returns (stdout, stderr).
fn list(args: &[&str], cfg: &Config, env: &[(&str, &str)]) -> (String, String) {
    let (Command::List(cmd), _) = cli(args).into_command() else {
        panic!("{args:?} is not a listing");
    };
    let (mut io, out, errs) = streams();
    run_list(
        &cmd,
        cfg,
        &iota::app::HostDirs::default(),
        &map_env(env),
        &mut io,
    )
    .unwrap_or_else(|e| panic!("list {args:?}: {e}"));
    (out.text(), errs.text())
}

// Go: chat/tokens_test.go
#[test]
fn test_parse_window_size() {
    for (input, want) in [
        ("128000", 128_000),
        (" 200k ", 200_000),
        ("1M", 1_000_000),
        ("0.5m", 500_000),
        ("2b", 2_000_000_000),
        ("1.5K", 1_500),
    ] {
        assert_eq!(parse_window_size(input), Ok(want), "{input:?}");
    }
    assert_eq!(
        parse_window_size("   ").map_err(|e| e.to_string()),
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

// ---------------------------------------------------------------- the verb set

/// Every verb parses to the command it names, and a bare invocation IS `run` — so `iota` and `iota -m "hi"`
/// keep working with no subcommand at all.
#[test]
fn every_verb_parses_to_its_command() {
    assert!(matches!(
        cli(&[]).into_command(),
        (Command::Run(c), _) if c.agent.is_none() && c.args.message.is_none()
    ));
    assert!(matches!(
        cli(&["-m", "hi"]).into_command(),
        (Command::Run(c), _) if c.agent.is_none() && c.args.message.as_deref() == Some("hi")
    ));
    assert!(matches!(
        cli(&["run", "coder", "-m", "hi"]).into_command(),
        (Command::Run(c), _) if c.agent.as_deref() == Some("coder")
    ));
    assert!(matches!(
        cli(&["run"]).into_command(),
        (Command::Run(c), _) if c.agent.is_none()
    ));
    assert!(matches!(
        cli(&["version"]).into_command(),
        (Command::Version, _)
    ));

    // `list`: no argument means agents; `list models <agent>` is the one form that takes a name.
    let (Command::List(l), _) = cli(&["list"]).into_command() else {
        panic!("not a listing")
    };
    assert!(l.what.is_none() && l.agent.is_none());
    let (Command::List(l), _) = cli(&["list", "models", "coder"]).into_command() else {
        panic!("not a listing")
    };
    assert_eq!(l.what, Some(iota::cmd::ListWhat::Models));
    assert_eq!(l.agent.as_deref(), Some("coder"));

    // `config`: no argument means check.
    let (Command::Config(c), _) = cli(&["config", "init"]).into_command() else {
        panic!("not a config command")
    };
    assert_eq!(c.action, Some(iota::cmd::ConfigAction::Init));

    // Unknown verbs, unknown listings and a second positional are clap's own errors (D-24, exit 2).
    for args in [
        &["nosuchverb"][..],
        &["list", "nosuch"][..],
        &["config", "nosuch"][..],
        &["run", "a", "b"][..],
        &["resume", "a", "b"][..],
    ] {
        assert!(
            Cli::try_parse_from(std::iter::once("iota").chain(args.iter().copied())).is_err(),
            "{args:?} must be a parse error"
        );
    }
}

/// The six flags that described CONFIGURATION are gone: the parser refuses them outright, so nobody keeps
/// using them against a build that would ignore them (brain page `cli-surface-agent-first`).
#[test]
fn the_retired_flags_no_longer_parse() {
    for args in [
        &["-k", "sk-x"][..],
        &["--key", "sk-x"][..],
        &["-u", "https://x"][..],
        &["--url", "https://x"][..],
        &["-t", "0.5"][..],
        &["--temperature", "0.5"][..],
        &["-S"][..],
        &["--system-input"][..],
        &["--context-window", "200k"][..],
        &["--agent"][..],
        &["-l"][..],
        &["--list"][..],
        &["--resume=abc"][..],
    ] {
        assert!(
            Cli::try_parse_from(std::iter::once("iota").chain(args.iter().copied())).is_err(),
            "{args:?} must no longer parse"
        );
    }
}

/// `-c/--config` is GLOBAL: the same invocation whether it precedes the verb or follows it, for every verb
/// that reads a config. `git -C` and `cargo --config` both take theirs first, so users type it that way.
#[test]
fn the_config_flag_works_on_either_side_of_the_verb() {
    let want = Some(std::path::Path::new("/tmp/x.yaml"));
    for (before, after) in [
        (
            vec!["-c", "/tmp/x.yaml", "list", "agents"],
            vec!["list", "agents", "-c", "/tmp/x.yaml"],
        ),
        (
            vec!["-c", "/tmp/x.yaml", "config", "check"],
            vec!["config", "check", "-c", "/tmp/x.yaml"],
        ),
        (
            vec!["-c", "/tmp/x.yaml", "run", "coder"],
            vec!["run", "coder", "-c", "/tmp/x.yaml"],
        ),
        (
            vec!["-c", "/tmp/x.yaml", "resume", "abc"],
            vec!["resume", "abc", "-c", "/tmp/x.yaml"],
        ),
    ] {
        for args in [&before, &after] {
            let (command, config) = cli(args).into_command();
            assert_eq!(config.as_deref(), want, "{args:?}");
            // …and the verb still parsed as itself, so the flag consumed nothing of it.
            assert_eq!(
                std::mem::discriminant(&command),
                std::mem::discriminant(&cli(&before[2..]).into_command().0),
                "{args:?}"
            );
        }
    }
    // A bare run takes it too, in the only position there is.
    assert_eq!(
        cli(&["-c", "/tmp/x.yaml", "-m", "hi"])
            .into_command()
            .1
            .as_deref(),
        want
    );
}

/// A `run` flag given BEFORE another verb is a mistake, not a shape with a meaning — and the refusal says
/// where the flag belongs. (`-c` is exempt: it is global, and the test above pins that.)
#[test]
fn a_run_flag_before_another_verb_is_refused() {
    for (args, want) in [
        (
            vec!["-m", "hi", "list"],
            "'-m/--message' is a flag of `iota run`; put it after the 'list' command",
        ),
        (
            vec!["--no-save", "resume", "abc"],
            "'--no-save' is a flag of `iota run`; put it after the 'resume' command",
        ),
        (
            vec!["--max-turns", "3", "config", "check"],
            "'--max-turns' is a flag of `iota run`; put it after the 'config' command",
        ),
    ] {
        let err = cli(&args)
            .check_flag_placement()
            .expect_err("a misplaced run flag must be refused");
        assert!(err.to_string().contains(want), "{args:?}: {err}");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
    // The same flags AFTER the verb, and every legal invocation, pass.
    for args in [
        &["run", "coder", "-m", "hi"][..],
        &["-m", "hi"][..],
        &["-c", "/tmp/x.yaml", "list"][..],
        &["list", "-c", "/tmp/x.yaml"][..],
        &["resume", "abc", "--no-save"][..],
        &["version"][..],
    ] {
        assert!(cli(args).check_flag_placement().is_ok(), "{args:?}");
    }
}

/// The nine that stayed, in one invocation — plus `--version`, which the Go-ported build did not have.
#[test]
fn the_surviving_flags_parse() {
    let c = cli(&[
        "run",
        "coder",
        "-m",
        "hi",
        "-M",
        "gpt",
        "-s",
        "be terse",
        "--mcp",
        "a b,c",
        "--mcp",
        "http://x",
        "--no-save",
        "--max-turns",
        "-3",
        "--output-format",
        "json",
        "-c",
        "/tmp/x.yaml",
    ]);
    let (Command::Run(cmd), config) = c.into_command() else {
        panic!("not a run")
    };
    assert_eq!(config.as_deref(), Some(std::path::Path::new("/tmp/x.yaml")));
    assert_eq!(cmd.agent.as_deref(), Some("coder"));
    let a = cmd.args;
    assert_eq!(a.message.as_deref(), Some("hi"));
    assert_eq!(a.model.as_deref(), Some("gpt"));
    assert_eq!(a.system.as_deref(), Some("be terse"));
    // `--mcp` appends and never splits on commas.
    assert_eq!(a.mcp, vec!["a b,c".to_owned(), "http://x".to_owned()]);
    assert!(a.no_save);
    assert_eq!(a.max_turns, -3);
    assert_eq!(a.output_format.as_deref(), Some("json"));
    assert!(Cli::try_parse_from(["iota", "--version"]).is_err_and(|e| {
        e.kind() == clap::error::ErrorKind::DisplayVersion && e.to_string() == "iota 0.1.0\n"
    }));
}

/// `iota resume [<id>]`: an id is a fragment to resolve, no id is the picker, and a blank one is the picker
/// too (it is what the user typed, not a fragment). The `--resume` flag's `require_equals` binding — pflag's
/// `NoOptDefVal` emulation, which made `iota --resume abc -m hi` read `abc` as the PROVIDER — is gone with it.
#[test]
fn resume_takes_its_id_as_a_positional() {
    assert_eq!(
        inv(&["resume", "abc"]).resume,
        Some(Resume::Id("abc".into()))
    );
    assert_eq!(inv(&["resume"]).resume, Some(Resume::Pick));
    assert_eq!(inv(&["resume", "  "]).resume, Some(Resume::Pick));
    assert_eq!(
        inv(&["resume", "  abc "]).resume,
        Some(Resume::Id("abc".into()))
    );
    assert_eq!(inv(&[]).resume, None);
    // The id and the flags no longer compete for the same token.
    let i = inv(&["resume", "abc", "-m", "hi"]);
    assert_eq!(i.resume, Some(Resume::Id("abc".into())));
    assert_eq!(i.args.message.as_deref(), Some("hi"));
}

const ALIAS_CFG: &str = "
providers:
  deepseek:
    type: openai
    key: cfg-key
    url: https://cfg.example/v1
  anthropic:
    key: ant-cfg-key
models:
  chat: {provider: deepseek, id: cfg-model}
agents:
  deepseek:
    models: [chat]
    system: cfg-system
  claude:
    models: [\"anthropic:claude-x\"]
";

// ---------------------------------------------------------------- key, url, system

/// root.go:62-92, minus the flags: the env var of the RESOLVED type beats `providers.<name>.key`, and the
/// url and the prompt come from the config (`-s` still overrides the prompt for one run).
#[test]
fn resolve_precedence_key_env_then_config() {
    let cfg = config(ALIAS_CFG);
    let env = [
        ("OPENAI_API_KEY", "env-key"),
        ("ANTHROPIC_API_KEY", "ant-key"),
    ];

    // env beats config; url/model/system come from the config entries.
    let s = resolve(&["run", "deepseek"], &cfg, &env).unwrap();
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

    // The per-run flags apply to it.
    let s = resolve(
        &[
            "run",
            "deepseek",
            "-M",
            "flag-model",
            "-s",
            "flag-system",
            "-m",
            "hi",
            "--max-turns",
            "-1",
            "--output-format",
            "json",
        ],
        &cfg,
        &env,
    )
    .unwrap();
    assert_eq!(s.model, "flag-model");
    assert_eq!(s.system, "flag-system");
    assert_eq!(s.message.as_deref(), Some("hi"));
    assert_eq!(s.max_turns, None);
    assert_eq!(s.output_format_raw.as_deref(), Some("json"));

    // Without the env var, the config key applies; `-s ""` is verbatim.
    let s = resolve(&["run", "deepseek", "-s", ""], &cfg, &[]).unwrap();
    assert_eq!(s.api_key, "cfg-key");
    assert_eq!(s.system, "");

    // An empty env value counts as unset (Go `os.Getenv(..) != ""`).
    let s = resolve(&["run", "deepseek"], &cfg, &[("OPENAI_API_KEY", "")]).unwrap();
    assert_eq!(s.api_key, "cfg-key");

    // The env var follows the resolved TYPE: an anthropic agent reads ANTHROPIC_API_KEY.
    let s = resolve(&["run", "claude"], &cfg, &env).unwrap();
    assert_eq!(s.api_key, "ant-key");

    // No key anywhere: the error names BOTH places it could have come from.
    let keyless =
        config("providers:\n  p: {type: openai}\nagents:\n  a:\n    models: [\"p:gpt-4o\"]\n");
    let err = resolve(&["run", "a"], &keyless, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "API key is required: set OPENAI_API_KEY or providers.p.key in your config"
    );
    assert!(matches!(
        err,
        CliError::ApiKeyRequired { env: "OPENAI_API_KEY", ref provider } if provider == "p"
    ));

    // An unconfigured built-in type is still an endpoint an agent may name.
    let builtin = config("agents:\n  g:\n    models: [\"gemini:flash\"]\n");
    let s = resolve(&["run", "g"], &builtin, &[("GOOGLE_API_KEY", "g")]).unwrap();
    assert_eq!((s.raw_type.as_str(), s.api_key.as_str()), ("gemini", "g"));
    assert_eq!(s.resolved.provider, ProviderConfig::default());

    // An unknown `type:` falls back to the literal API_KEY variable.
    let odd = config("providers:\n  odd: {type: custom}\nagents:\n  a:\n    models: [\"odd:x\"]\n");
    assert!(matches!(
        resolve(&["run", "a"], &odd, &[("API_KEY", "")]).unwrap_err(),
        CliError::ApiKeyRequired { env: "API_KEY", .. }
    ));
    let s = resolve(&["run", "a"], &odd, &[("API_KEY", "generic")]).unwrap();
    assert_eq!(s.api_key, "generic");

    // A `system_file` that cannot be read aborts (after the agent lookup, before the key check).
    let bad = config(
        "agents:\n  a:\n    models: [\"openai:gpt-4o\"]\n    system_file: /nonexistent/iota-sys.md\n",
    );
    let err = resolve(&["run", "a"], &bad, &[("OPENAI_API_KEY", "k")]).unwrap_err();
    assert!(matches!(err, CliError::Config(_)), "{err}");
    assert!(
        err.to_string()
            .starts_with("system_file: open /nonexistent/iota-sys.md: "),
        "{err}"
    );
    // `-s` skips the file entirely.
    let s = resolve(&["run", "a", "-s", "x"], &bad, &[("OPENAI_API_KEY", "k")]).unwrap();
    assert_eq!(s.system, "x");
}

/// A reader that fails on the first read.
struct FailingStdin;

impl Read for FailingStdin {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

/// The config every stdin/message test resolves against: one agent, one model, one key.
fn simple() -> Config {
    config("agents:\n  a:\n    models: [\"openai:gpt-4o\"]\n")
}

/// root.go:95-104: only the literal `-m -` reads stdin; the text is trimmed; whitespace-only and read failures
/// are errors with the Go texts.
#[test]
fn resolve_stdin_message_trimmed_and_empty_error() {
    let cfg = simple();
    let env = map_env(&[("OPENAI_API_KEY", "k")]);
    let args = inv(&["run", "a", "-m", "-"]);

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
    let args = inv(&["run", "a", "-m", "  spaced  "]);
    let s = resolve_run(&args, &cfg, &env, &mut FailingStdin, &mut |_| {}).unwrap();
    assert_eq!(s.message.as_deref(), Some("  spaced  "));

    // The stdin read happens AFTER the key check: no key → the key error, stdin untouched.
    let args = inv(&["run", "a", "-m", "-"]);
    let err = resolve_run(&args, &cfg, &map_env(&[]), &mut FailingStdin, &mut |_| {}).unwrap_err();
    assert!(matches!(err, CliError::ApiKeyRequired { .. }));
}

/// POLICY F-03: `-m ""` is an error, not an interactive run.
#[test]
fn resolve_message_empty_is_error() {
    let cfg = simple();
    let err = resolve(&["run", "a", "-m", ""], &cfg, &[("OPENAI_API_KEY", "k")]).unwrap_err();
    assert_eq!(err.to_string(), "--message must not be empty");
    assert!(matches!(err, CliError::MessageEmpty));
    // …even without a model: the message rule precedes the model rule.
    let picker = config("agents:\n  a:\n    models: [\"openai:*\"]\n");
    let err = resolve(&["run", "a", "-m", ""], &picker, &[("OPENAI_API_KEY", "k")]).unwrap_err();
    assert!(matches!(err, CliError::MessageEmpty));
}

/// No `-m` → `message: None` and NO `ModelRequired` (Go: `chatMessage == ""` is the interactive branch; `run`
/// takes the interactive branch at root.go:259). With `-m`, a model is required (root.go:107-109).
#[test]
fn resolve_message_absent_is_none() {
    let env = [("OPENAI_API_KEY", "k")];
    // A wildcard-first agent has no model until the picker runs.
    let picker = config("agents:\n  a:\n    models: [\"openai:*\"]\n");
    let s = resolve(&["run", "a"], &picker, &env).unwrap();
    assert_eq!(s.message, None);
    assert_eq!(s.model, "");

    let err = resolve(&["run", "a", "-m", "hi"], &picker, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "--model/-M is required when using --message/-m"
    );
    assert!(matches!(err, CliError::ModelRequired));

    // The agent's own candidate satisfies the rule.
    let cfg = simple();
    let s = resolve(&["run", "a", "-m", "hi"], &cfg, &env).unwrap();
    assert_eq!(s.model, "gpt-4o");
    assert_eq!(s.message.as_deref(), Some("hi"));

    // `--output-format` is carried RAW in every mode — even a bad value resolves: the parse belongs to `run`
    // at root.go:249-252's position (after tuning/MCP), so `unknown output format …` keeps Go's
    // precedence (tests/cli.rs pins both the text and the order), and root.go:253 belongs to `run` too.
    let s = resolve(&["run", "a", "--output-format", " text "], &cfg, &env).unwrap();
    assert_eq!(s.output_format_raw.as_deref(), Some(" text "));
    let s = resolve(&["run", "a", "--output-format", "xml"], &cfg, &env).unwrap();
    assert_eq!(s.output_format_raw.as_deref(), Some("xml"));
}

/// root.go:114-123: the config `temperature:` must be 0.0-2.0. The `-t` flag that used to skip the check is
/// gone, so the range now holds for every value a run can carry.
#[test]
fn resolve_config_temperature_is_range_checked() {
    let env = [("OPENAI_API_KEY", "k")];
    let tuned = |t: &str| {
        config(&format!(
            "models:\n  m: {{provider: openai, id: x, temperature: {t}}}\nagents:\n  a:\n    models: [m]\n"
        ))
    };
    let err = resolve(&["run", "a"], &tuned("3.5"), &env).unwrap_err();
    assert_eq!(err.to_string(), "config temperature 3.5: want 0.0-2.0");
    assert!(matches!(err, CliError::ConfigTemperature(t) if t.to_bits() == 3.5_f64.to_bits()));
    assert_eq!(
        resolve(&["run", "a"], &tuned("-0.5"), &env)
            .unwrap_err()
            .to_string(),
        "config temperature -0.5: want 0.0-2.0"
    );

    // In-range values pass; the bounds are inclusive; no source → None.
    assert_eq!(
        resolve(&["run", "a"], &tuned("0.7"), &env)
            .unwrap()
            .temperature,
        Some(0.7)
    );
    assert_eq!(
        resolve(&["run", "a"], &tuned("2"), &env)
            .unwrap()
            .temperature,
        Some(2.0)
    );
    assert_eq!(
        resolve(&["run", "a"], &simple(), &env).unwrap().temperature,
        None
    );

    // An agent's own override is checked the same way.
    let agent_hot =
        config("models:\n  m: openai:x\nagents:\n  a:\n    models: [m]\n    temperature: 9\n");
    assert_eq!(
        resolve(&["run", "a"], &agent_hot, &env)
            .unwrap_err()
            .to_string(),
        "config temperature 9: want 0.0-2.0"
    );
}

/// `iota run <name>` resolves `agents:` and nothing else, and the refusal lists the agents there are.
#[test]
fn resolve_unknown_agent_lists_the_configured_ones() {
    let cfg = config(
        "
providers:
  zeta: {type: openai, key: k}
models:
  mid: openai:gpt-4o
agents:
  reviewer: {models: [mid]}
  coder: {models: [mid]}
",
    );
    let err = resolve(&["run", "codr"], &cfg, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown agent \"codr\"\n  configured agents: coder, reviewer"
    );
    assert!(matches!(err, CliError::UnknownAgent { .. }));

    // A model, a provider and a built-in type are NOT runs — the four-namespace lookup is gone.
    for name in ["mid", "zeta", "openai"] {
        assert!(
            matches!(
                resolve(&["run", name], &cfg, &[]).unwrap_err(),
                CliError::UnknownAgent { .. }
            ),
            "{name} must not be runnable"
        );
    }

    // Nothing configured at all: the error points at the command that fixes it.
    let err = resolve(&["run", "coder"], &Config::default(), &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unknown agent \"coder\"\n  no agents are configured — run `iota config init` to write a starter config"
    );

    // The agent lookup precedes the key check: no misleading "API key is required" for a typo.
    assert!(matches!(
        resolve(&["run", "codr"], &cfg, &[("OPENAI_API_KEY", "")]).unwrap_err(),
        CliError::UnknownAgent { .. }
    ));

    // An agent whose provider `type:` is not a built-in resolves and is rejected later, at construction
    // (provider.go:329 text via `ProviderKind::from_str`).
    let odd = config(
        "providers:\n  odd: {type: custom, key: k}\nagents:\n  a:\n    models: [\"odd:x\"]\n",
    );
    let s = resolve(&["run", "a"], &odd, &[]).unwrap();
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
}

// ---------------------------------------------------------------- the listings

/// `iota list` reads the config and nothing else: no key, no network, and `agents` when no `what` is given.
#[test]
fn list_reads_the_config() {
    let cfg = config(
        "
providers:
  deepseek: {type: openai, key: cfg-key, url: https://cfg.example/v1}
  anthropic: {}
models:
  chat: {provider: deepseek, id: cfg-model}
  sonnet: anthropic:claude-x
agents:
  coder:
    models: [chat, sonnet, \"deepseek:*\"]
    description: Writes and reviews code
  scratch:
    models: [sonnet]
",
    );
    let env = [("ANTHROPIC_API_KEY", "ant")];

    // agents: the name, the size of the candidate set, and what the entry says it is for.
    let (out, errs) = list(&["list"], &cfg, &env);
    assert_eq!(
        out,
        "Agents:\n  coder    3 models  Writes and reviews code\n  scratch  1 model\n"
    );
    assert_eq!(errs, "");
    assert_eq!(list(&["list", "agents"], &cfg, &env).0, out);

    // models: every entry and the endpoint it rides on.
    assert_eq!(
        list(&["list", "models"], &cfg, &env).0,
        "Models:\n  chat    deepseek:cfg-model\n  sonnet  anthropic:claude-x\n"
    );

    // models <agent>: that agent's candidate set, best first, wildcards included.
    assert_eq!(
        list(&["list", "models", "coder"], &cfg, &env).0,
        "Models for agent coder:\n  chat (deepseek:cfg-model)\n  sonnet (anthropic:claude-x)\n  deepseek:* (every model deepseek lists)\n"
    );

    // providers: the endpoints, each with where its key comes from — config, environment, or nowhere.
    assert_eq!(
        list(&["list", "providers"], &cfg, &env).0,
        "Providers:\n  anthropic  [key: ANTHROPIC_API_KEY]\n  deepseek (type: openai, url: https://cfg.example/v1)  [key: config]\n"
    );
    assert_eq!(
        list(&["list", "providers"], &cfg, &[]).0,
        "Providers:\n  anthropic  [no key: set ANTHROPIC_API_KEY]\n  deepseek (type: openai, url: https://cfg.example/v1)  [key: config]\n"
    );

    // An empty config says what to do about it rather than printing nothing.
    assert_eq!(
        list(&["list"], &Config::default(), &[]).0,
        "No agents configured. Run `iota config init` to write a starter config.\n"
    );
    assert_eq!(
        list(&["list", "providers"], &Config::default(), &[]).0,
        "No providers configured. Run `iota config init` to write a starter config.\n"
    );
}

/// The two refusals the listing owns: a name where no name belongs, and an agent that does not exist.
#[test]
fn list_refuses_a_name_it_cannot_use() {
    let cfg = config("agents:\n  a: {models: [\"openai:x\"]}\n");
    let run = |args: &[&str]| {
        let (Command::List(cmd), _) = cli(args).into_command() else {
            panic!("not a listing")
        };
        let (mut io, _, _) = streams();
        run_list(
            &cmd,
            &cfg,
            &iota::app::HostDirs::default(),
            &map_env(&[]),
            &mut io,
        )
        .unwrap_err()
    };
    assert_eq!(
        run(&["list", "agents", "a"]).to_string(),
        "iota list agents takes no argument (only `iota list models <agent>` does)"
    );
    assert_eq!(
        run(&["list", "providers", "a"]).to_string(),
        "iota list providers takes no argument (only `iota list models <agent>` does)"
    );
    assert!(matches!(
        run(&["list", "models", "nosuch"]),
        CliError::UnknownAgent { .. }
    ));
}

/// One `providers:` line: the type when it differs from the name, the url when the entry sets one.
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
            &provider_cfg("https://api.deepseek.com/v1")
        ),
        "deepseek (type: openai, url: https://api.deepseek.com/v1)"
    );
    assert_eq!(
        provider_line("deepseek", "openai", &provider_cfg("")),
        "deepseek (type: openai)"
    );
    assert_eq!(
        provider_line("openai", "openai", &provider_cfg("https://x")),
        "openai (url: https://x)"
    );
    assert_eq!(
        provider_line("openai", "openai", &provider_cfg("")),
        "openai"
    );
}

// ---------------------------------------------------------------- headless refusals

/// DIVERGENCES D-23: `--no-save` is the last interactive-only flag, and it is rejected by
/// `reject_unsupported` with the headless text. Everything else that used to live in that set is gone.
#[test]
fn unsupported_flags_rejected() {
    let err = inv(&["run", "a", "--no-save"])
        .args
        .reject_unsupported()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "flag --no-save is not supported in headless mode"
    );
    assert!(matches!(err, CliError::UnsupportedFlag("--no-save")));
    assert!(
        inv(&["run", "a", "-m", "hi"])
            .args
            .reject_unsupported()
            .is_ok()
    );
}

/// DIVERGENCES D-52: `--model/-M is required when using --message/-m` is DEFERRED for a resume (a session
/// supplies the model from its meta); `run` re-raises it byte-identically after the replay.
#[test]
fn resolve_resume_defers_model_required() {
    let cfg = config("agents:\n  default:\n    models: [\"openai:*\"]\n");
    let env = [("OPENAI_API_KEY", "sk")];

    // No -M and no model in the candidate set, but a session is named: resolution succeeds and carries the
    // fragment.
    let s = resolve(&["resume", "abc", "-m", "hi"], &cfg, &env).unwrap();
    assert_eq!(s.model, "");
    assert_eq!(s.resume.as_deref(), Some("abc"));
    assert_eq!(s.name, "default", "a resume runs under agents.default");

    // The picker form carries no fragment — `run_agent` refuses it headlessly before this point.
    let s = resolve(&["resume", "-M", "gpt", "-m", "hi"], &cfg, &env).unwrap();
    assert_eq!(s.resume, None);

    // Without a resume the deferral does not apply.
    assert!(matches!(
        resolve(&["run", "-m", "hi"], &cfg, &env).unwrap_err(),
        CliError::ModelRequired
    ));
    // And a plain run carries no fragment.
    assert_eq!(
        resolve(&["run", "-M", "gpt", "-m", "hi"], &cfg, &env)
            .unwrap()
            .resume,
        None
    );
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
    let s = resolve(&["run", "team"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("anthropic", "claude-sonnet-4")
    );
    assert_eq!(s.api_key, "ak");

    // A candidate by name brings its provider, its key, its url AND its protocol with it.
    let (s, warnings) = resolve_warned(&["run", "team", "-M", "gpt5"], &cfg, &[]);
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
    let (s, warnings) = resolve_warned(&["run", "team", "-M", "relay:o3-mini"], &cfg, &[]);
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
    let s = resolve(&["run", "team", "-M", "relay:*"], &cfg, &[]).unwrap();
    assert_eq!((s.raw_type.as_str(), s.model.as_str()), ("openai", ""));

    // A bare id stays on the provider the run resolved to.
    let (s, warnings) = resolve_warned(&["run", "team", "-M", "claude-opus-5"], &cfg, &[]);
    assert_eq!(s.unwrap().raw_type, "anthropic");
    assert_eq!(warnings.len(), 1, "{warnings:?}");

    // An id whose colon names nothing iota knows is left alone — relays put colons in model ids.
    let s = resolve(&["run", "team", "-M", "vendor:weird:id"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("anthropic", "vendor:weird:id")
    );
}

// ---------------------------------------------------------------- the implicit `agents.default`

/// A run with no agent name takes `agents.default` — the whole entry, not just its model.
#[test]
fn a_declared_default_agent_runs_without_a_name() {
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

    // `iota run`, and naming it explicitly, mean the same thing.
    assert_eq!(resolve(&["run"], &cfg, &[]).unwrap(), s);
    assert_eq!(resolve(&["run", "default"], &cfg, &[]).unwrap(), s);

    // The flags still apply to it.
    let s = resolve(&["-M", "openai:gpt-4o"], &cfg, &[]).unwrap();
    assert_eq!(
        (s.raw_type.as_str(), s.model.as_str()),
        ("openai", "gpt-4o")
    );
    assert_eq!(s.api_key, "pk");
}

/// Without an `agents.default` the refusal names the two ways forward.
#[test]
fn no_default_agent_is_refused_with_both_ways_out() {
    const WANT: &str = "no agent to run: name one with `iota run <agent>` (see `iota list agents`), or add an `agents.default` entry — `iota config init` writes a starter config";

    for cfg in [
        Config::default(),
        // A config with all three layers — just not a `default` agent.
        config(
            "providers:\n  openai: {key: k}\nmodels:\n  default: openai:gpt-4o\nagents:\n  coder:\n    models: [default]\n",
        ),
    ] {
        let err = resolve(&[], &cfg, &[]).unwrap_err();
        assert_eq!(err.to_string(), WANT);
        assert!(matches!(err, CliError::NoAgent));
        assert!(matches!(
            resolve(&["run"], &cfg, &[]).unwrap_err(),
            CliError::NoAgent
        ));
    }

    // `models.default` and `providers.default` are NOT fallbacks: one entry point, and only the one that
    // says how to drive a model.
    let cfg = config("providers:\n  default: {type: openai, key: k}\n");
    assert!(matches!(
        resolve(&[], &cfg, &[]).unwrap_err(),
        CliError::NoAgent
    ));
    assert!(matches!(
        resolve(&["run", "default"], &cfg, &[]).unwrap_err(),
        CliError::UnknownAgent { .. }
    ));
}

/// The agent decides everything a run talks to, and its switches reach `RunSettings`.
#[test]
fn the_agent_decides_the_whole_run() {
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
    let s = resolve(&["run", "openai"], &cfg, &[]).unwrap();
    assert_eq!(
        s.raw_type, "anthropic",
        "the agent's model decides the endpoint"
    );
    assert_eq!(s.model, "from-agents");
    assert_eq!(s.system, "agent prompt");
    assert_eq!(s.api_key, "ak");
    assert!(
        s.agent_mode,
        "workspace: true is what the --agent flag used to switch on"
    );
}
