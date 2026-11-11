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
- **Tools that ask.** A `shell` under an OS sandbox, `code` tools that read
  anywhere and write in the project and the `write_roots:` you list, MCP
  servers with OAuth login — every file edit, every unsandboxed command and
  every MCP tool call asks for confirmation unless the config waives it. A
  write outside those directories, or to git's hooks and config or iota's own
  config, asks every time; the sandbox never makes one.
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

### A repository's own `.iota.yaml`

iota reads `~/.iota.yaml`, then the `.iota.yaml` (or `.iota.yml`) of the
directory it runs in, which overrides it entry by entry; `-c <file>` reads that
one file instead of both. A repository's file can start MCP servers, point a
provider somewhere else and loosen the sandbox, so iota does not use it until
you trust it:

- Started on a terminal, iota shows what the file would do and asks once,
  before anything of it runs: for each MCP server, the command it runs (or
  the url it connects to), its `env`, any `headers` or `client_secret` value
  holding a `${…}`, and its `auto_run`; a provider's `url` and a `${…}` key;
  an agent's `system_file` and its tools' `sandbox` (other than `auto`),
  `auto_run`, `network` and `auto_write`; `write_roots` and
  `outside_writes`. Trust it, or skip it for this run.
- Without a terminal (`-m`, a script, a bot), iota never asks: it skips the
  file with one warning and runs on `~/.iota.yaml`. `iota mcp list --probe`
  lists an untrusted file's servers without starting them, and `iota mcp
  login` refuses to log in to one.
- `iota config trust` trusts the file as it is now (`--path <file>` for
  another one); `iota config untrust` forgets it. Editing the file asks again.
  The record is `~/.iota/trusted.json`, never in the repository.
- `-c <file>` is a file you named, used without asking. Your own `~/.iota.yaml`
  is never asked about.

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

## Environment variables

Everything else is configured in the YAML file. These four are for the cases a
config file cannot cover:

| Variable | What it does | When you would use it |
|---|---|---|
| `IOTA_SHELL` | The interpreter the `shell` tool runs commands with — an absolute path or a name on `PATH`. Wins on every platform. | Your commands need a shell other than the one iota finds (`bash` on macOS and Linux; Git Bash, then PowerShell, then `cmd.exe` on Windows). A value that is not an executable is an error, never a silent fallback. |
| `IOTA_GIT_BASH_PATH` | Windows only: the `bash.exe` of Git Bash. | Git for Windows is installed somewhere iota does not look (not next to the `git.exe` on `PATH`, not in a default install root), or you want an MSYS2 bash. |
| `IOTA_LOG` | A file path; iota appends its diagnostic log there (`tracing` events: iota at `DEBUG`, its libraries at `INFO`). | Something misbehaves — an MCP server, a connection — and you want to see what happened underneath. Nothing reaches this log unless the variable is set; warnings you need to act on are always shown in the terminal regardless. |
| `IOTA_DEBUG_REGION` | A file path; iota appends a trace of every operation on the terminal's live region there. | You are reporting (or fixing) a rendering glitch — a stray blank row, a misplaced line — and need to show which layer drew it. Developer tooling; most people never need it. |

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
