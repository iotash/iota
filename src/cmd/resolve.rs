//! Pure run resolution (cmd/root.go:46-123, 534-566): the agent lookup, the key/model/system precedence, the
//! `-m -` stdin read, the message/model rules and the config temperature range check — everything `run`
//! decides before it constructs a provider. The command's error type is `crate::cmd::error`.

use crate::cmd::args::{Invocation, Resume};
use crate::cmd::error::{ArgsError, CliError, SetupError};
use crate::config::{ApiKey, Config, ModelConfig, ModelRef, Resolved};

use crate::app::env::Env;

/// What `resolve_run` decided for one invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSettings {
    /// The `agents:` entry this run is (as typed, or `default`).
    pub name: String,
    /// The resolved provider type string.
    pub raw_type: String,
    /// The three config layers this run landed on.
    pub resolved: Resolved,
    /// The API key: the env var of the RESOLVED provider type, else `providers.<name>.key`. Never a flag —
    /// a key on the command line lands in the shell history and in `ps`, and iota hands the model a shell
    /// tool (brain page `cli-surface-agent-first`).
    pub api_key: String,
    /// The base URL from `providers.<name>.url` (`""` = dialect default).
    pub base_url: String,
    /// The model (`-M` > the agent's default model; may be `""` without `-m`).
    pub model: String,
    /// The system prompt (`-s` > `system:` > `system_file:`; `""` = none).
    pub system: String,
    /// `None` = `-m` not given (Go: `chatMessage == ""` → the interactive branch, taken at root.go:259's
    /// position in `run`). `Some` is never empty (`MessageEmpty`).
    pub message: Option<String>,
    /// The temperature the agent or its model declares (range-checked).
    pub temperature: Option<f64>,
    /// The agent's `workspace: true` — the AGENTS.md overlay and the skills toolset.
    pub agent_mode: bool,
    /// `--max-turns` as a cap (`None` = unlimited; the flag's `<= 0`).
    pub max_turns: Option<std::num::NonZeroU32>,
    /// `--output-format` as typed (`None` = flag absent). Carried RAW: `run` parses it at exactly Go's
    /// position (root.go:249-252, after tuning/MCP assembly), so `unknown output format …` keeps
    /// Go's precedence; `Some` also drives `OutputFormatWithoutMessage` there (root.go:253).
    pub output_format_raw: Option<String>,
    /// The id fragment `iota resume <id>` named; `None` for `iota run` AND for the bare `iota resume`, whose
    /// session the interactive picker chooses.
    pub resume: Option<String>,
}

/// Pure. Order (root.go:53-123): the agent — the name given, else `agents.default` when the config declares
/// one, else `NoAgent` → the agent lookup (`resolve_agent`) → `-M` (which may move the run to another
/// provider) → key (env of the RESOLVED type > `providers.<name>.key`) → system (`-s` > the agent's) →
/// `ApiKeyRequired` → `-m -` reads stdin (trim; `failed to read from stdin: {e}`; `no message provided via
/// stdin`) → `-m ""` → `MessageEmpty` → model required ONLY when message is `Some` AND no resume is in play
/// (D-52: a resume supplies the model from meta, so `run` re-raises `ModelRequired` after the replay) →
/// temperature (config range).
/// `--output-format` is NOT parsed here and `OutputFormatWithoutMessage` is NOT raised here (Go does both after
/// tuning/MCP, root.go:249-255) — the raw flag rides `output_format_raw` and `run` does both.
///
/// `-M` is applied BEFORE the key because `provider:id` names an endpoint: a model the run was not started on
/// brings its own key variable and base URL with it.
pub fn resolve_run(
    inv: &Invocation,
    cfg: &Config,
    env: &Env,
    stdin: &mut dyn std::io::Read,
    warn: &mut dyn FnMut(String),
) -> Result<RunSettings, CliError> {
    // root.go:53-60, as the agent-first surface reads it: `iota run <agent>` names one, everything else takes
    // `agents.default` when the config declares it.
    let name = match inv.agent.as_deref() {
        Some(name) => name,
        None => cfg.default_agent().ok_or(ArgsError::NoAgent)?,
    };
    let mut resolved = resolve_agent(cfg, name)?;
    if let Some(flag) = &inv.args.model {
        apply_model_flag(&mut resolved, cfg, flag, warn);
    }
    let raw_type = resolved.provider_type.clone();

    // root.go:62-87, minus the flag: the env var of the RESOLVED type, else the config `key:` — the one
    // precedence, `Endpoint::api_key`.
    let api_key = resolved.endpoint().api_key(env);
    let base_url = resolved.provider.url.clone();
    let model = resolved.model.id.clone();
    let system = match &inv.args.system {
        Some(flag) => flag.clone(),
        None => resolved.agent.resolve_system()?,
    };

    // root.go:89-92
    let api_key = match api_key {
        ApiKey::Env { key, .. } | ApiKey::Config(key) => key,
        ApiKey::Missing { var } => {
            return Err(SetupError::ApiKeyRequired {
                env: var,
                provider: resolved.provider_name.clone(),
            }
            .into());
        }
    };

    // root.go:95-104 (`-m -`), then POLICY F-03 (`-m ""`).
    let message = match inv.args.message.as_deref() {
        None => None,
        Some("-") => Some(read_stdin_message(stdin)?),
        Some("") => return Err(ArgsError::MessageEmpty.into()),
        Some(m) => Some(m.to_owned()),
    };

    // root.go:107-109: non-interactive mode requires a model — DEFERRED for a resume, because a resumed
    // session can supply it from its meta (root.go:323-325). `run` re-raises this exact error after the model
    // replay, so a provider-mismatched or model-less bundle still fails with Go's text (DIVERGENCES D-52).
    if message.is_some() && model.is_empty() && inv.resume.is_none() {
        return Err(ArgsError::ModelRequired.into());
    }

    // root.go:114-123: the config default is range-checked (the `-t` flag that skipped the check is gone).
    let temperature = match resolved.temperature() {
        Some(t) if !(0.0..=2.0).contains(&t) => {
            return Err(SetupError::ConfigTemperature(t).into());
        }
        t => t,
    };

    Ok(RunSettings {
        name: name.to_owned(),
        raw_type,
        api_key,
        base_url,
        model,
        system,
        message,
        temperature,
        agent_mode: resolved.agent.workspace,
        max_turns: crate::tool::context::turn_cap(inv.args.max_turns),
        output_format_raw: inv.args.output_format.clone(),
        resume: match &inv.resume {
            Some(Resume::Id(id)) => Some(id.clone()),
            Some(Resume::Pick) | None => None,
        },
        resolved,
    })
}

/// The agent lookup, with the unknown-name error that lists the agents there ARE.
pub(crate) fn resolve_agent(cfg: &Config, name: &str) -> Result<Resolved, ArgsError> {
    cfg.resolve_agent(name)
        .ok_or_else(|| ArgsError::UnknownAgent {
            name: name.to_owned(),
            agents: cfg.agent_names(),
        })
}

/// `-M`, in the three forms it takes:
///
/// - `provider:id` — an endpoint AND a model, so the run moves to that provider (only when `provider` is a
///   name iota knows; a bare id that happens to contain a colon is left alone, which is what a relay's
///   `vendor:model` ids need);
/// - `provider:*` — that provider with no model chosen, i.e. start in the picker;
/// - a bare value — a `models:` entry from the agent's own candidate set if it names one, else a raw model id
///   on the provider the run already resolved to.
///
/// A model outside the agent's candidate set is a WARNING, never a refusal (decision of 2026-09-10): the set
/// is advice about what is good here, not a whitelist.
///
/// What a `provider:id` (or a raw id on the current provider) brings with it is decided by [`model_at`]: the
/// `models:` entry serving exactly that pair, or nothing at all — never the knobs of the candidate the flag
/// is replacing (brain page `model-param-layering`; DIVERGENCES X-32).
fn apply_model_flag(r: &mut Resolved, cfg: &Config, flag: &str, warn: &mut dyn FnMut(String)) {
    if flag.is_empty() {
        // `-M ""` is verbatim, like every other flag: no model was chosen.
        r.model.id.clear();
        return;
    }
    match flag.split_once(':') {
        Some((provider, "*")) if cfg.knows_provider(provider) => {
            r.move_to_provider(cfg, provider);
            r.model.id.clear();
        }
        Some((provider, id)) if cfg.knows_provider(provider) && !id.is_empty() => {
            r.model = model_at(r, cfg, provider, id);
            r.move_to_provider(cfg, provider);
        }
        _ => match cfg.models.get(flag) {
            // A candidate by name brings its own provider and protocol with it.
            Some(entry)
                if r.agent
                    .models
                    .iter()
                    .any(|c| matches!(c, ModelRef::Entry(n) if n == flag)) =>
            {
                let mut model = entry.clone();
                model.anchor_provider(flag);
                let provider = model.provider.clone();
                r.model = model;
                r.move_to_provider(cfg, &provider);
            }
            // Everything else is a raw model id on the provider the run resolved to: `<provider>:id`.
            _ => {
                let provider = r.provider_name.clone();
                r.model = model_at(r, cfg, &provider, flag);
            }
        },
    }
    if !r.agent.models.is_empty()
        && !r.model.id.is_empty()
        && !r.covers(cfg, &r.provider_name, &r.model.id)
    {
        warn(format!(
            "Warning: model {}:{} is not in agent {:?}'s models (using it anyway)",
            r.provider_name, r.model.id, r.name
        ));
    }
}

/// The `ModelConfig` a `provider:id` pair stands for: the `models:` entry serving exactly that pair when one
/// exists — a member of the agent's candidate set first, then any entry, in name order — so its own knobs
/// travel with it; otherwise the bare pair, every other field at its default. A model the config declares
/// nothing about gets no declaration: the knobs of whatever candidate the run resolved to first (effort,
/// temperature, `top_p`, window, defer mode, the image parameters) stay with that candidate.
fn model_at(r: &Resolved, cfg: &Config, provider: &str, id: &str) -> ModelConfig {
    let serves = |name: &str, m: &ModelConfig| m.provider_or(name) == provider && m.id == id;
    let candidate = r.agent.models.iter().find_map(|c| match c {
        ModelRef::Entry(name) => cfg
            .models
            .get(name)
            .filter(|m| serves(name, m))
            .map(|m| (name.as_str(), m)),
        ModelRef::Inline { .. } | ModelRef::All { .. } => None,
    });
    let entry = candidate.or_else(|| {
        cfg.models
            .iter()
            .find(|(name, m)| serves(name, m))
            .map(|(name, m)| (name.as_str(), m))
    });
    match entry {
        Some((name, entry)) => {
            let mut model = entry.clone();
            model.anchor_provider(name);
            model
        }
        None => ModelConfig {
            provider: provider.to_owned(),
            id: id.to_owned(),
            ..ModelConfig::default()
        },
    }
}

/// root.go:95-104: read ALL of stdin, trim, reject an empty message.
fn read_stdin_message(stdin: &mut dyn std::io::Read) -> Result<String, ArgsError> {
    let mut data = Vec::new();
    stdin.read_to_end(&mut data).map_err(ArgsError::Stdin)?;
    let message = String::from_utf8_lossy(&data).trim().to_owned();
    if message.is_empty() {
        return Err(ArgsError::EmptyStdin);
    }
    Ok(message)
}
