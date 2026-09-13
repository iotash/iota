# Changelog

All notable changes to iota are recorded here. The same notes, rendered, are at
<https://iota.sh/changelog>.

## 0.1.0 - 2026-09-13

The first tagged release of iota as a Rust binary. It is an agent CLI: you
configure agents — a model, a prompt, a set of tools — and run them. If you
used the Go build, this is the list of things that changed under you.

### What breaks

- **The positional argument is a command, not a name.** `run`, `list`,
  `resume`, `config` and `version` are the whole set, and a bare `iota` means
  `iota run`. `iota <name>` used to be looked up in four namespaces — agents,
  models, providers, then the built-in provider types — so one word could mean
  four things. Now `iota run <agent>` resolves `agents:` alone, and a model or
  a provider is reached through an agent. `--help` is the complete map, and a
  command added later can never shadow an agent you named.
- **Six flags are gone.** `-k/--key`, `-u/--url`, `-t/--temperature`,
  `-S/--system-input`, `--context-window` and the boolean `--agent` are
  refused by the parser. A flag stays on the command line only if it describes
  THIS call: the key comes from an environment variable or
  `providers.<name>.key`, the URL from `providers.<name>.url`, the temperature
  and the window from `models:` or `agents:`, the prompt from
  `agents.<name>.system`, and agent mode from `agents.<name>.workspace`. Nine
  flags remain, and only `-c/--config` is global.
- **`--resume` and `-l` became commands.** `iota resume [<id>]` takes any
  unique id prefix and opens a picker with no id;
  `iota list [agents|models|providers|sessions]` reads the config and the
  session store — no key, no network.
- **The config has three layers, and they are enforced.** `providers:` says how
  to reach an API, `models:` which model and what its protocol looks like,
  `agents:` how it is driven. Every key is audited against the layer it was
  written in before the document is decoded: a key of another layer, a retired
  key, an unknown key, an unknown top-level key and an unknown toolset are all
  errors naming the coordinate and the file. There is no migration layer — a
  one-layer `providers.<name>` block carrying `model:`, `system:`, `tools:` or
  `agent:` is refused, not split.
- **The `delegate` toolset is retired.** A child agent is now
  `iota run <agent> -m "<task>"` run from `bash` — a full run of that `agents:`
  entry, with its own model, tools and session.
- **`defer_mode` moved to `models:`.** The provider — and so the dialect — is
  already fixed there, so a mode the provider cannot speak is refused when the
  config loads instead of silently degrading to `normal`.

### What is new

- **`iota config`.** `init` writes a commented starter config (refusing to
  overwrite one), `path` prints the files a run reads, and `check` loads them
  and reports the three layers, warning when no `agents.default` exists. The
  zero-config start was given up on purpose; `init` is where it went.
- **Headless resume replays the session.** `iota resume <id> -m "…"` takes the
  model (when the session was recorded under the same provider type),
  temperature, reasoning effort, context window and image settings from the
  bundle; an explicit `-M` still wins. The banner goes to stderr, so stdout
  stays the reply — or the JSON report — alone, and the new turn is appended
  only when it succeeds.
- **Dedicated image providers.** `type: imagen` and `type: images` carry models
  that only generate pictures, with `/edit`, `/redo`, reference images through
  `/file`, and per-dialect knobs on `/model`.
- **Background bash jobs.** `"background": true` returns a job id at once and
  the result arrives later as a notice the model is handed. Up to 16 at a time,
  and they are killed when iota exits — a resumed session never inherits one.
- **`iota version`.** The Go build had no way to say its version; `iota version`
  and `--version` both print it.
- **Windows.** `x86_64-pc-windows-msvc` is a release target, installed by one
  PowerShell line. There is no embedded interpreter: the `shell` tool drives
  Git Bash, PowerShell or `cmd.exe` — whichever the machine has — and says
  which one in its own description. There is no OS sandbox on Windows, so
  every command asks, and the binaries are not code-signed; see
  [installation](https://iota.sh/docs/install).

### Fixes worth naming

- `--mcp ""` and `-m ""` are errors with a message instead of a panic or a
  silent fall-through into the TUI.
- A stream chunk whose `error` field is literally `null` is treated as absent,
  not as an in-band error.
- Sparse tool-call indices in a chat-completions stream no longer drop the
  calls after the gap.
- Ctrl+C closes the MCP servers and exits 130; a `--output-format json` run
  prints its report with `"error": "interrupted"`.

### Upgrading

Start from `iota config init` and move what you had into the three layers: the
endpoint into `providers:`, the model into `models:`, the prompt and tools into
`agents:`. `iota config check` names anything still in the wrong place, with
the file and the coordinate — see [the config file](https://iota.sh/docs/config-file).
