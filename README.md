<p align="center">
  <img src="https://iota.sh/img/iota-app-icon.svg" alt="iota" width="96" height="96">
</p>

<h1 align="center">iota</h1>

<p align="center">The smallest thing between your terminal and a model.</p>

<p align="center">
  <a href="https://iota.sh">iota.sh</a> ·
  <a href="https://iota.sh/docs/install">Install</a> ·
  <a href="https://iota.sh/docs/quick-start">Quick start</a> ·
  <a href="https://iota.sh/docs/config-reference">Configuration</a> ·
  <a href="https://iota.sh/changelog">Changelog</a>
</p>

---

iota is an agent CLI for the terminal, written in Rust. You configure agents —
a model, a prompt, a set of tools — in one YAML file, and run them:
`iota run <agent>`, or a bare `iota` for the agent called `default`.

- **One config file, three layers.** `providers:` (endpoints and keys),
  `models:` (which model, which protocol), `agents:` (prompt, tools, MCP
  servers). A misplaced key is an error naming its coordinate, not a line
  nothing reads.
- **Any endpoint that speaks OpenAI, Anthropic or Gemini** — including one you
  run yourself. Dedicated image models generate straight into the conversation.
- **Tools that ask.** A `shell` under an OS sandbox, `code` tools confined to
  the project, MCP servers with OAuth login — every write and every unsandboxed
  command asks for confirmation unless the agent waives it.
- **Sessions are plain files.** Resume them, grep them, export them to HTML or
  Markdown; nothing is hidden, `/debug` shows the exact bytes on the wire.
- **The terminal is the interface.** Streaming replies you can type over,
  markdown, tables and math rendered inline, file attachments, a model picker.
- **No account, no daemon, no telemetry.** Child agents are `iota run <agent>
  -m "<task>"` from bash, like anything else.

## Install

```bash
curl -fsSL https://iota.sh/install.sh | sh        # macOS and Linux
brew install iotash/tap/iota                        # Homebrew
powershell -ExecutionPolicy Bypass -c "irm https://iota.sh/install.ps1 | iex"   # Windows
cargo install --git https://github.com/iotash/iota                             # from source
```

Prebuilt binaries for macOS (Apple Silicon and Intel), Linux (x86-64 and
arm64) and Windows (x86-64) come from every
[release](https://github.com/iotash/iota/releases); the
[install page](https://iota.sh/docs/install) has the details, the per-platform
notes and the checksums.

## First run

```bash
export OPENAI_API_KEY=…   # or put `key:` in the file afterwards
iota                      # writes ~/.iota.yaml on a first run, and runs agents.default
```

A first run writes a starter config — one provider, one model, one agent — and
goes on with it. Edit that file for your provider and model; `iota config
check` tells you whether it says what you think it says.

```bash
iota run reviewer                # another agent from the config
iota -m "explain this repo"      # one message, non-interactive
iota mcp add nb --url https://namebeta.com/api/mcp   # an MCP server; logs in if it asks
iota list agents                 # what is configured
```

## Documentation

Everything else is on [iota.sh/docs](https://iota.sh/docs):
[the config file](https://iota.sh/docs/config-file) and
[every key of it](https://iota.sh/docs/config-reference),
[the system prompt iota sends](https://iota.sh/docs/system-prompt),
[the built-in toolsets](https://iota.sh/docs/builtin-toolsets),
[MCP servers](https://iota.sh/docs/mcp),
[agent mode](https://iota.sh/docs/agent-mode) and
[slash commands](https://iota.sh/docs/slash-commands).

For the code: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) is the module map
and the layering rules, [`docs/DIVERGENCES.md`](docs/DIVERGENCES.md) the ledger
of deliberate behavioural decisions, [`docs/design/`](docs/design/) the design
notes per feature, and [`docs/RELEASING.md`](docs/RELEASING.md) how a release
is cut.

## Development

```bash
cargo test                                   # unit + integration tests (no network, no HOME access)
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX=1 cargo test --test ui_tmux        # the real-terminal suite (needs tmux; skips without it)
./ci.sh                                      # everything CI runs, in order — tmux and the OS sandbox required
```

## License

MIT
