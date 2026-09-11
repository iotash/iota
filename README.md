# iota

An agent CLI for the terminal, written in Rust. You configure agents — a model,
a prompt, a set of tools — and run them: `iota run <agent>`, interactively or
one message at a time.

## Features

**The agent is what you run.** An `agents:` entry is a model, a prompt, a
toolset and an MCP subset under one name, and naming one is the whole surface:
`iota run <agent>`, or a bare `iota` for the agent called `default`. A model or
a provider is reached through an agent, never run on its own.

- **Config file** — three layers in `~/.iota.yaml`: `providers:` (endpoints and API keys), `models:` (configured models and their protocol), `agents:` (prompt, tools, MCP subset), plus MCP server definitions. Every key is checked against the layer it belongs to, so a misplaced or misspelled one is an error naming its coordinate rather than a line nothing reads
- **System prompt** — per agent in the config, or `-s` for one run
- **Agent mode** — opt-in via `workspace: true` per agent: layered `AGENTS.md` instructions and [Agent Skills](https://agentskills.io/specification) are injected as a volatile system-prompt overlay, the `skills` toolset (`load_skill`) is auto-enabled, and sessions are grouped per project
- **Headless runs** — `-m` sends a single message and prints the response, pipe-friendly, with an optional JSON report and a tool-turn budget; child agents are exactly this, run from `bash`

**Its fuel — providers and models.** Which endpoint answers and which model
thinks are config, not command line; a run picks from the candidate set the
agent declares.

- **Multi-provider** — OpenAI, OpenAI Responses API, Anthropic, Gemini, and Vertex AI, with custom base URL support, plus dedicated image-generation providers (`imagen`, `images`)
- **Interactive model selection** — arrow-key list at startup
- **Model settings mid-session** — `/model` opens a tabbed panel over the model, context window, reasoning effort, and temperature (plus a read-only view of the system prompt in effect), all persisted with the session and replayed on resume
- **Image generation** — image-capable models generate straight into the conversation, rendered inline as ANSI half-block art and saved with the session; dedicated image models get `/edit` and `/redo` instead of a chat loop

**Its hands — tools and MCP.** What an agent can actually do to your machine,
enabled per agent and gated per call.

- **Built-in toolsets** — `shell` (bash under an OS sandbox, with background jobs), `code` (glob, grep, read, edit, write, confined to the project root), `skills` (`load_skill`), and `ask` (put a decision to the user mid-turn). Writes and unsandboxed commands ask for confirmation in the conversation unless the agent waives it
- **MCP tool support** — connect external MCP tool servers (filesystem, GitHub, databases, etc.) and let the agent call them, with tool names namespaced per server (`mcp__<server>__<tool>`) so same-named tools never collide

**Its record — sessions.** Everything a run did, on disk, resumable and
exportable.

- **Session persistence** — every interactive session is auto-saved (losslessly: messages, tool calls, attachments, reasoning) to `~/.iota/sessions/`. Resume with `/session` from inside a session, or `iota resume [<id>]` at launch (any unique id prefix works), and resuming echoes the last few exchanges back to the terminal; auto-titled by the model after the first reply; `--no-save` (or `no_save: true` per agent) starts ephemeral — nothing touches disk unless you run `/save [title]`, which persists the whole backlog and auto-saves from then on
- **Conversation history** — full context maintained within a session
- **Context management** — live token accounting against the context window (configurable via `context_window:` per model or the `/model` Context tab), with `/compact` LLM-summarization of older history; when the window nears full a confirmation is offered before compacting (declining snoozes the prompt until usage grows further)
- **Conversation export** — `/export` renders the session to a single self-contained HTML file (inline CSS, dark mode with a toggle, syntax-highlighted code) or a plain Markdown document; saved sessions export the full on-disk log, so compaction never hides older rounds (ephemeral `--no-save` sessions export the current in-memory view)

**The terminal it runs in.**

- **Streaming responses** — real-time token output with a busy spinner in the status line; keep typing while a reply streams (type-ahead — queued submits are sent in order); press **Esc** (cancels the innermost running scope) or **Ctrl+C** (cancels the turn) to interrupt a streaming reply — the partial reply is kept in history and marked interrupted
- **Markdown highlighting** — inline ANSI styling for headings, bold, italic, code, tables, and code blocks in streaming output; inline LaTeX math (`$...$`, `\(...\)`) is approximated in Unicode, and display math (`$$...$$`, `\[...\]`) renders as a 2D block — stacked fractions, roots with a drawn vinculum, matrices, aligned environments, sums/integrals with limits, drawn accents (`\hat`/`\vec`/`\bar`), and math fonts (`\mathbb`/`\mathcal`); anything unsupported falls back to a readable single-line approximation, never raw LaTeX
- **Slash-command completion** — a suggestion row appears when the line starts with `/` and narrows as you type; press Tab to cycle through the completions
- **File attachments** — send images, PDFs, and text files alongside messages; `/file` opens a tabbed surface with the attached list and a directory browser
- **Request inspector** — `/debug` opens a two-tab console: a **Verbose** toggle turns recording on/off (off by default; `/debug on` / `/debug off` do the same from the prompt), and **Messages** browses the captured API calls (newest first), each summarized by action and content (e.g. `Chat 你好…`) rather than raw method/URL — drill into any one to read its `↑ Request` and `↓ Response` bodies, pretty-printed, with `c` to copy to the clipboard. Nothing is printed to the terminal
- **Host integration** — the terminal's native progress indicator follows the turn (busy, needs input, error), a desktop notification is sent when a reply lands or the model needs you while the window is unfocused (`notify: false` per agent turns it off), and the cmux multiplexer is driven natively when detected
- **Styled terminal output** — color-coded prompts

## Install

### Homebrew

```bash
brew install iotash/tap/iota
```

### Shell

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/iotash/iota/releases/latest/download/iota-installer.sh | sh
```

Fetches the prebuilt binary for your platform from the latest release, verifies
its checksum and puts it in `~/.cargo/bin` (or `$CARGO_HOME/bin`), adding that
directory to your `PATH` if it is not there already. No Rust toolchain needed.

### Cargo

```bash
cargo install --git https://github.com/iotash/iota
```

Requires Rust 1.98 or newer.

### Build from source

```bash
git clone https://github.com/iotash/iota.git
cd iota
cargo build --release
```

The binary is at `target/release/iota`. The toolchain is pinned by `rust-toolchain.toml`, which `rustup` picks up automatically; building with another toolchain needs Rust 1.98 or newer.

### Platforms

macOS and Linux, Apple Silicon and x86-64 alike: every release carries
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu` and
`x86_64-unknown-linux-gnu` binaries.

Windows builds and is tested in CI, but it is **not at parity yet**, and the
gap is the one that matters most: there is **no `bash` tool** there. The
`shell` toolset warns once at startup and registers nothing, because the tool
runs POSIX shell scripts and the Windows shell backend is still being written;
the OS sandbox is absent for the same reason. Everything else works — reading,
writing and editing files, skills, MCP servers, the whole TUI. Releases do not
carry a Windows binary yet either, so today it means building from source.
Until both land, WSL gives you the complete thing, as ordinary Linux.

### First run

```bash
iota config init          # writes ~/.iota.yaml with one provider, one model and agents.default
export OPENAI_API_KEY=…   # or put `key:` in the file
iota                      # runs agents.default
```

`iota config check` tells you whether the file says what you think it says,
and `iota config path` which files a run actually reads.

## Usage

```bash
iota [command] [flags]
```

| Command | What it does |
|---------|--------------|
| `iota` | Run the agent named `default`, interactively |
| `iota run <agent>` | Run that `agents:` entry, interactively |
| `iota run <agent> -m "…"` | One headless turn: message in, reply out |
| `iota run` | The same as a bare `iota` |
| `iota list [agents\|models\|providers\|sessions]` | What the config declares (no argument: `agents`); `iota list models <agent>` shows one agent's candidate set |
| `iota resume [<id>]` | Resume a saved session — any unique id prefix; with no id, pick from a list |
| `iota config [check\|path\|init]` | Validate the config, print which files it reads, or write a starter one (no argument: `check`) |
| `iota version` | Print the version (`--version` does the same) |

The positional argument is the **command**, never a name from your config, so
`--help` is the complete map of what iota can do and a future command can
never collide with an agent you named.

### Flags

Nine flags, and every one of them describes THIS invocation. Anything that
describes configuration — the key, the endpoint, the temperature, the context
window, the prompt you want every time — lives in the three config layers
instead.

Only `-c/--config` is global; the rest belong to `iota run` (and `iota resume`,
which is a run that starts from a saved session), so they go after the command.

| Flag | Short | Description |
|------|-------|-------------|
| `--message` | `-m` | Send a single message and print the response (non-interactive; `-` reads stdin) |
| `--model` | `-M` | Model for this run: a `models:` entry, a bare id, or `provider:id` (`provider:*` opens the picker) |
| `--system` | `-s` | System prompt for this run (beats the agent's `system:` / `system_file:`) |
| `--config` | `-c` | Path to config file (default: `~/.iota.yaml`, then `./.iota.yaml`). Global: valid before or after the command, so `iota -c f.yaml list` and `iota list -c f.yaml` are the same |
| `--mcp` | | MCP server (command string or URL, repeatable) |
| `--no-save` | | Start ephemeral — nothing touches disk unless `/save` is run (interactive only) |
| `--max-turns` | | Limit agentic tool turns for the whole run (`-m` only; 0 = unlimited) |
| `--output-format` | | `-m` output: `text` (default, the reply alone) or `json` (one result object with per-round token usage) |
| `--version` | `-V` | Print the version |

Headless resume (`iota resume <id> -m "…"`) takes what you did not pass from
the session bundle: the model (when the session was recorded under the same
provider type), temperature, reasoning effort, context window and image
settings all replay, and an explicit `-M` still wins. A resumed run prints
`Resumed session <id> (<n> messages)` on stderr, so stdout stays the reply (or
the JSON report) alone; the new turn is appended only when it succeeds.

### Environment Variables

| Variable | Provider |
|----------|----------|
| `OPENAI_API_KEY` | OpenAI / OpenResponses / Images |
| `ANTHROPIC_API_KEY` | Anthropic |
| `GOOGLE_API_KEY` | Gemini / Vertex AI / Imagen |

### Config File

iota supports YAML config files for persistent settings, models and agents.

#### Config Lookup Order

1. `~/.iota.yaml` or `~/.iota.yml` (global)
2. `./.iota.yaml` or `./.iota.yml` (project-local, merges over global)
3. `-c/--config <path>` (explicit, highest priority, used alone)

Same-name entries in later files override earlier ones, whole entry at a time.

#### Priority

The API key is **env var > `providers.<name>.key`** — never a flag, so it stays
out of the shell history and out of `ps`. The two per-run flags that overlap
the config (`-M`, `-s`) win over it for that one invocation.

#### The three layers

The config has three top-level maps, each answering one question:

| Map | Answers | Keys |
|-----|---------|------|
| `providers:` | *how do I reach the API?* | `type`, `key`, `url` |
| `models:` | *which model, and what does its protocol look like?* | `provider`, `id`, `context_window`, `defer_mode`, image knobs, `effort`/`temperature`/`top_p` defaults |
| `agents:` | *how do I use it?* | `models`, `system`/`system_file`, `tools`, `mcp_servers`, `workspace`, `no_save`, `notify`, `description`, and overrides for the three tunables |

**A run names an agent.** `iota run <name>` resolves `agents:` and nothing
else: the agent decides which model it drives, and the model decides which
endpoint it talks to. A `models:` or `providers:` entry is reached through an
agent, never named directly — one name meant four things once, and a collision
silently changed what ran.

**`agents.default` is what a bare `iota` runs.** With no name iota takes the
agent called `default`; a name always wins over it, and without such an agent
the invocation asks for one. A `models.default` or a `providers.default` says
which model or endpoint it is, never how to drive one, so neither is an entry
point.

**Every key is checked against its layer.** A key written in the wrong one —
or simply misspelled — fails the load with its coordinate and the file it is
in (`config ~/.iota.yaml: providers.deepseek.system: `system` belongs under
`agents:``), rather than sitting there doing nothing.

#### Referring to a model

Wherever a model is named — `agents.<name>.models`, a `models:` shorthand,
`-M` — three forms are accepted:

| Form | Means |
|------|-------|
| `sonnet` | the `models:` entry called `sonnet` |
| `anthropic:claude-sonnet-4` | that model id, on that provider (everything after the FIRST colon is the id, so `openrouter:anthropic/claude-3.5-sonnet` works) |
| `anthropic:*` | every model the provider lists, fetched at startup |

**No space after the colon.** `- anthropic: claude-x` is a YAML *mapping*, not
a string; iota says so rather than failing with a type error.

#### Example

```yaml
# ~/.iota.yaml
providers:                   # endpoints: how to connect, how to authenticate
  openai:
    key: sk-official
  anthropic:
    key: ${env:ANTHROPIC_KEY}
  deepseek:                  # a custom endpoint
    type: openai             # the underlying provider type
    key: ${env:DEEPSEEK_KEY} # key/url expand ${…} variables
    url: https://api.deepseek.com/v1

models:                      # configured models: provider + id + protocol + defaults
  sonnet: anthropic:claude-sonnet-4-20250514    # shorthand: provider:id
  gpt5:
    provider: openai
    id: gpt-5.2
    context_window: 400k     # context window for compaction accounting (/model's Context tab overrides)
    defer_mode: system-tools # protocol for deferred MCP tools: normal|reference|tool-search|system-tools
    effort: high             # default reasoning effort: low|medium|high|xhigh|max
    temperature: 0.7         # default sampling temperature, 0.0-2.0 (-t and /model override)
    top_p: 0.9               # nucleus sampling, 0.0-1.0 (advanced: tune this OR temperature, not both;
                             # reasoning models reject/ignore it — omit to use the provider default)
  chat: deepseek:deepseek-chat

agents:                      # usage: how a model is driven
  default:
    models: [gpt5, sonnet, "deepseek:*"]   # candidate set, best first; the FIRST one is the default
    system: "You are a helpful coding assistant"
    tools:
      code:
      shell:
    mcp_servers: [github]    # load only these MCP servers; [] = none; key absent = all
    workspace: true          # project overlay (AGENTS.md) + skills + project-scoped sessions

  reviewer:
    models: [sonnet]
    system_file: ${appHome}/prompts/reviewer.md  # prompt from a file (inline `system` wins)
    description: Reads a diff and reports what is wrong with it   # documentation of the entry
    effort: high             # overrides the model's default (one level, no deeper)

  scratch:
    models: ["openai:*"]     # a wildcard first entry starts in the model picker
    no_save: true            # start ephemeral (like --no-save); an explicit `iota resume` outranks it
    notify: false            # no desktop notification while the terminal is unfocused (default: on)

# MCP tool servers
mcp_servers:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "${workspaceFolder}"]
    env:
      LOG_LEVEL: info

  github:
    url: https://mcp.example.com/sse
    headers:
      Authorization: "Bearer ${env:GITHUB_TOKEN}"
    # Deferred loading: instead of advertising every schema on every request,
    # only a search_tools entry is advertised and the model loads this
    # server's tools on demand — the value IS the group's one-line summary
    # shown in the manifest (that's why it's a string, not a bool). Worth it
    # for servers with many tools; leave unset for small ones. The MODEL's
    # defer_mode selects the protocol (default "normal"; see
    # docs/design/tool-defer.md) — and because a protocol belongs to a
    # provider's dialect, a mode the provider cannot speak is refused when the
    # config loads rather than quietly downgraded.
```

With this config:

```bash
# The agent named "default": its first model (gpt5 → openai/gpt-5.2), prompt, tools and MCP subset
iota                              # …and with no argument at all, that is what runs
iota run default -m "hello"

# Another agent: its own models, prompt, tools and MCP subset
iota run reviewer -m "what is wrong with this diff?"

# -M picks another model from the candidate set (a warning if it is outside it — the set is advice)
iota run default -M sonnet -m "hi"

# -M also takes provider:id, which moves the run to that endpoint
iota run default -M "deepseek:deepseek-reasoner" -m "hi"

# …and provider:* starts in the model picker
iota run default -M "deepseek:*"
```

`-M` accepts a candidate's name, a bare model id, or `provider:id`. A model
outside the agent's `models:` list is a warning, not a refusal — the list is
advice about what works well here, not a whitelist.

#### One layer per key

Every key belongs to exactly one layer, and writing it in another is an
error naming the layer that owns it. Two keys changed name when the layers
split: a provider's `agent: true` is an agent's `workspace: true`, and the
`agent` toolset is now called `skills`. The `delegate` toolset was removed
outright — a child agent is a bash subprocess now (see the `shell` set).

```yaml
# what a single-layer config used to look like — every key of it is refused today
providers:
  deepseek:
    type: openai                      # ✓ the endpoint
    key: ${env:DEEPSEEK_KEY}          # ✓
    url: https://api.deepseek.com/v1  # ✓
    model: deepseek-chat              # ✗ → a `models:` entry
    system: "You are terse"           # ✗ → `agents.<name>.system`
    tools: {code: {}}                 # ✗ → `agents.<name>.tools`
    agent: true                       # ✗ → `agents.<name>.workspace`

# the same thing, in three layers
providers:
  deepseek: {type: openai, key: "${env:DEEPSEEK_KEY}", url: https://api.deepseek.com/v1}
models:
  deepseek: deepseek:deepseek-chat
agents:
  deepseek:
    models: [deepseek]
    system: "You are terse"
    tools: {code: {}}
    workspace: true
```

#### Variable Expansion

Provider values (`key`, `url`), an agent's `system_file` and MCP server values
(`command`, `args`, `url`, `env`, `headers`) support VS Code-style variable
expansion:

| Variable | Expands to |
|----------|-----------|
| `${workspaceFolder}` / `${cwd}` | Current working directory |
| `${userHome}` | User home directory |
| `${appHome}` | iota's global directory (`~/.iota`) |
| `${pathSeparator}` / `${/}` | OS path separator (`/`) |
| `${env:VAR}` | Value of environment variable `VAR` |

Unknown variables are left untouched.

### Image Generation

Image-capable models generate straight into the conversation: with a Gemini image
model (e.g. `gemini-3.1-flash-image`) just ask — the picture renders inline
as ANSI half-block art (capped well below a screenful, indented like other
blocks), and is saved INSIDE the session bundle (`<session>/images/` —
deleted with the session; ephemeral and `-m` runs fall back to
`~/.iota/images/`). The printed path is an OSC 8 hyperlink — clickable
in terminals that support it (⌘-click in Ghostty/iTerm2; Terminal.app has no
OSC 8 support). Generated images round-trip
into the conversation, so follow-ups like "make the circle blue" edit the
previous image in place. Sessions persist them losslessly (attachments), and
`-m` single-shot runs print the saved path instead of rasterizing into a
pipe.

Two ways to switch generation on where it needs an explicit request-side
opt-in: the per-provider `image: true` config key, or the `/model` surface's
**Image** tab at runtime (shown for capable providers; persisted with the
session). On `openresponses` it advertises the `image_generation` built-in
tool (works with gpt-5-family models); on Google it adds
`responseModalities: ["TEXT","IMAGE"]` for official-API models that require
the opt-in — relays like zenmux generate without it.

#### Dedicated image models — the `imagen` and `images` provider types

Models that ONLY generate images — Doubao Seedream, official Imagen, and the
other pure image models relay stations host — have no chat endpoint at all;
they speak Google's Imagen `:predict` protocol instead. The `imagen` provider
type carries them: **every message is a fresh generation** (your text is the
prompt; `/file` attachments ride along as reference images for
image-to-image), matching how stateless image tools conventionally work.
Editing is explicit: `/edit add a robot` re-sends the last generated image as
the reference, and consecutive `/edit`s chain naturally. To edit an *earlier*
picture, run `/edit` with no prompt: a picker opens with the image previewed
beside a newest-first list of everything this session generated (each row
labeled by the prompt that made it, with the file's clickable path below);
pick one and type your prompt. Unhappy with what came back? `/redo` rolls
again from the same canvas and prompt, and `/redo <reworded prompt>` retries
from that canvas with new wording — so a rejected picture never becomes the
input to the next attempt. Images render and persist exactly like
conversational generation, sessions keep the whole iteration history, and
`-m` does one-shot generation.

```yaml
providers:
  seedream:
    type: imagen
    key: ${env:ZENMUX_API_KEY}
    url: https://zenmux.ai/api/vertex-ai  # omit for the official Gemini API

models:
  seedream:
    provider: seedream
    id: bytedance/doubao-seedream-5.0-pro
    aspect_ratio: "3:2"                   # optional generation defaults,
    image_size: "2K"                      # passed through verbatim
    negative_prompt: "blurry, watermark"
```

The same knobs are adjustable mid-session: `/model` grows **Aspect**, **Size**,
and **Negative** tabs for image providers (a "default" row omits the
parameter), persisted with the session and replayed on resume. Only the tabs
a dialect actually has appear.

`type: images` is the sibling for the OpenAI Images protocol
(`/v1/images/generations` + multipart `/v1/images/edits`) — gpt-image
models, DALL·E, and the relays that mirror the endpoints. Same session
shape (`/edit`, `/file` references, one image per call); the dialect folds
dimensions into a single **Size** knob (e.g. `image_size: "1536x1024"`)
and has no aspect-ratio or negative-prompt parameters. DALL·E's URL-form
responses are fetched automatically.

Interactive turns ask for **progressive frames**: a refining thumbnail
appears in the generation widget and the finished picture replaces it in
place, exactly like conversational generation. Verified live on OpenAI and
zenmux, which both stream; xAI ignores the flags and answers with the plain
body — the same request serves both, so nothing is generated twice and a
non-streaming backend simply shows the elapsed clock. `imagen` has no
streaming form at all, so those turns show only the elapsed clock.

The edit endpoint comes in two wire flavors: OpenAI's native
`/images/edits` is multipart, while some backends (xAI) accept only a JSON
body and reject multipart outright. Set `json_edits: true` for those, or
flip the **JSON edits** tab on `/model` mid-session (persisted with the
session). Generation is unaffected either way.

Parameter and editing support varies by backend: relays map the full set
(seedream's aspect ratio, size, negative prompt, and reference-image editing
are live-verified), while the official Gemini API hosts generate-only Imagen
models that ignore reference images and the negative prompt — unknown
parameters are dropped server-side, so a knob with no visible effect means
that backend doesn't support it. A custom `url` is addressed in the vertex
`publishers/{vendor}/models` form (the relay convention); omitting `url`
targets the official Gemini API form. Results arrive either inline or as an
expiring signed URL (some relay-hosted models, e.g. Kling, answer that way) —
both land in the session the same, the link being fetched right away.
Generation is billed per call, so failures are never auto-retried — an error
surfaces immediately and you decide whether to spend again.

These models have no tokens, no temperature, and no reasoning, so the
corresponding machinery disappears for such sessions: `/model` shows no
Context/Effort/Temperature tabs, the status bar drops its context meter, and
`/compact` does not apply. `/model` still picks models — filtered to
image-capable ones when the server provides capability metadata.

### Built-in Toolsets

Besides MCP servers, iota ships built-in tools grouped into named
**toolsets** that you enable per agent in the config file. A toolset is
enabled by listing it under that agent's `tools:` key; the value is the
set's shared configuration, and an empty value uses its defaults. Available
sets: `shell` (running bash commands, sandboxed), `code` (reading, searching,
and editing project files), `skills` (skill activation; auto-enabled by agent
mode), and `ask` (interactive questions to the user; enabled by default in
interactive sessions — disable with `ask: false`).

```yaml
agents:
  claude:
    models: ["anthropic:claude-sonnet-4-20250514"]
    tools:
      shell:                 # empty → sandboxed, network blocked
      code:

  coder:
    models: ["openai:gpt-4o"]
    tools:
      shell:
        network: true        # allow network inside the sandbox
        write: [~/.cache]    # extra sandbox-writable paths
```

#### `ask` — `choose`, `confirm`

Lets the model put a decision to you on an interactive selector instead of
asking in prose — and, crucially, WITHOUT ending its turn: the answer flows
back as a tool result and the same agentic round continues. `choose` packs
1–4 questions into a tabbed surface (short headers as tab labels; Tab
switches, one Enter commits all; single- or multi-select per question, and an
"Other…" free-text answer unless the model disables it). `confirm` is a
single yes/no. ESC declines — the model is told and proceeds on its own.
Zero side effects, on by default interactively, absent in `-m` runs; opt out
per agent with `tools: {ask: false}`.

#### `shell` — `bash`

Lets the model run real bash command lines — pipes, redirects, `&&` chaining,
heredocs — and returns their combined stdout/stderr. The model calls it with
`command` (required), an optional `cwd` (defaults to the project root), an
optional `timeout` in seconds (default 600, maximum 3600; outside that range
the call is refused and nothing runs) and an optional `background` (below).

Safety model — the same one Claude Code and Codex CLI use:

- **OS sandbox by default.** On macOS commands run under Seatbelt
  (`sandbox-exec`, built into the system); on Linux under
  [bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`, if
  installed). Inside the sandbox, file **writes are confined to the project
  root plus temp/cache directories** (add more via `write:`), and **network
  access is blocked** unless `network: true`.
- **Sandboxed calls run without prompting.** Where no sandbox is available
  (Linux without bwrap) or with `sandbox: off`, every call instead
  asks for confirmation in the conversation (allow once / allow for this session /
  deny), and non-interactive `-m` runs reject it — set `auto_run: true` to
  waive that.
- Output is capped at 32 KB and 512 lines (head + tail kept, middle elided,
  bounded even while streaming). Each call is capped at **10 minutes** unless
  it asks for a different `timeout`; while a command runs, the status-line
  spinner shows the elapsed time — press **ESC** (or Ctrl+C) to terminate it.
- **Calls issued together run concurrently.** A round's consecutive `bash`
  calls execute as one batch — ESC cancels the batch, and results still come
  back in call order. (Every other toolset keeps the conservative rule: only
  calls that cannot change state batch.)

**Background jobs.** `"background": true` starts the command and returns at
once with a job id, its pid and an output file:

```
Started background job b1 (pid 4242). Output: /tmp/iota-jobs/931/b1.log
A notice with its exit status and output arrives when it finishes; run
`tail -n 50 /tmp/iota-jobs/931/b1.log` to see progress meanwhile.
```

When the job ends, its result enters the conversation on its own as a
**notice** — the model is told, it never polls:

```
[background job b1 finished: exit 0 after 42s] make test
<the job's output, under the same 32 KB / 512 line caps a foreground call gets>
```

If you are sitting at the prompt, the notice wakes the model for one turn (your
half-typed draft is untouched). If a turn is already running, it lands at the
next round boundary, like a message you typed while the model was working. In
`-m` runs the run does not end while a job is still going: the loop waits for
it, hands the model the notice and gives it another round — each one counted
against `--max-turns`.

Up to **16** jobs at a time (past that the call is refused), `timeout` applies
the same way, and the approval rules are unchanged. The log file is left on
disk. **Background jobs are killed when iota exits** — `/quit`, Ctrl+C at the
prompt, or the end of a `-m` run — so a resumed session never inherits one; a
job that must survive that has to detach itself (`nohup`, `setsid`).

**Child agents.** iota has no delegation tool: a child agent is
`iota run <agent> -m "<task>"` run from `bash`, which is why the set is the one
that matters most. The child is a full run of that `agents:` entry — its own
model, tools, MCP servers and session. Start it with `background: true` and
its answer comes back as the notice above. For it to write without a user to
ask, set `tools.code.auto_write` / `tools.shell.auto_run` on that agent; for it
to reach an API at all, the parent's sandbox has to allow it, since
`network: false` (the default) blocks the child's HTTP too. How to dispatch,
to whom, and how many at once is your prompt's business, not the binary's.

Design: docs/design/shell-toolset.md

#### `code` — coding tools

The coding loop: `glob` and `grep` locate files (`.git`, `.gitignore` matches,
and binaries excluded), `list_dir` explores, `read_file` returns line-numbered
content, and `edit_file` (exact, unique string replacement) / `write_file`
change files. Everything is confined to the **project root** (the git root of
the working directory). Verification — builds, tests — goes through the
`shell` set's `bash`, so enable it alongside.

Safety model:

- A file must be **read before it can be modified**, and a file that changed
  on disk since it was read must be re-read first — the model can never
  blind-overwrite your edits.
- Every modifying call asks for confirmation in the conversation (allow once / allow
  for this session / deny). Non-interactive `-m` runs reject modifications
  outright. Set `auto_write: true` under `tools: code:` to skip confirmations
  and allow `-m` writes:

```yaml
    tools:
      code:
        auto_write: true   # optional; default asks before every write
```

  Or withhold the writers entirely with `read_only: true`, leaving `glob`,
  `grep`, `list_dir` and `read_file`. A tool the model cannot see is never
  attempted and never refused — useful for a reviewer.
  (`read_only` and `auto_write` together are rejected as contradictory.)

Design: docs/design/code-toolset.md

### Agent Mode

Agent mode is explicitly opt-in — set `workspace: true` on an agent in the
config file. Off, the agent runs with just its own prompt and tools: no project
overlay, no skills, no project-scoped sessions.

```yaml
agents:
  claude:
    models: ["anthropic:claude-sonnet-4-20250514"]
    workspace: true
```

Everything is anchored at the **project root**: the git root of the working
directory, or the working directory itself outside a repository.

#### AGENTS.md

Following the [AGENTS.md convention](https://agents.md/), every `AGENTS.md`
from the project root down to the current directory (at most one per
directory) is concatenated root-first — nearer files come later and override —
capped at 32 KiB, and appended to the system prompt as a **volatile overlay**:
composed at send time, never stored in the conversation history or the session
file, and re-read automatically when a file changes between turns (a dim
`AGENTS.md reloaded` notice is printed). Resuming a session elsewhere applies
that directory's `AGENTS.md`.

#### Skills

Skills follow the [Agent Skills specification](https://agentskills.io/specification):
a skill is a directory containing a `SKILL.md` with `name` and `description`
frontmatter. Discovery directories, highest precedence first (same-name skill:
higher wins):

1. `<project root>/.agents/skills/` — project skills
2. `~/.iota/skills/` — iota user skills
3. `~/.agents/skills/` — cross-client user skills

Discovered skills are advertised to the model as a name + description catalog
inside the overlay; the model activates one by calling `load_skill` with the
skill's name, reads files the skill references through the same tool's `file`
argument, and runs bundled scripts through `bash` (enable the `shell`
toolset for the agent if your skills need scripts). Invalid skills are
skipped with a warning, never fatal. You can also run a skill yourself with
`/skills <name> [instructions]` — the skill's instructions become the message
that is sent.

#### `skills` — `load_skill`

Agent mode auto-enables the `skills` toolset (it was called `agent` before the
config split, where the word became the name of a layer). Its `load_skill` tool activates a skill by name: it returns the skill's instructions (the `SKILL.md` body) and
directory, and the optional `file` argument reads a file bundled inside that
directory — reads never leave the skill's directory. Output is size-capped
with an optional `offset`/`limit` line window. The set can also be enabled
explicitly under `tools:` like any other, agent mode or not.

#### Project-Scoped Sessions

Sessions started in agent mode are stored per project under
`~/.iota/sessions/projects/<slug>/`, and `/session` and `iota resume` list
only the current project's sessions there (`iota resume <id>` with an id from
anywhere still works). Normal-mode sessions stay in the flat global store,
whose list also shows every project's sessions labelled with their project —
nothing is ever invisible.

### Slash Commands

In interactive mode, the following commands are available. When the line starts
with `/`, a suggestion row appears below the input and narrows as you keep typing;
press Tab to cycle through the completions. Commands that do not apply to the
current session (for example `/edit` on a text provider) are not offered at all,
and an unknown `/word` is sent as a normal message.

| Command | Description |
|---------|-------------|
| `/file [path]` | Attach a file (image, PDF, or text). With a path, attaches directly. With no path, opens a tabbed selector: "Attached" to remove attachments, "Add" to pick one from a directory browser. |
| `/edit [prompt]` | Edit a generated image by re-sending it as this turn's reference. With a prompt it takes the newest image (consecutive `/edit`s iterate). With no prompt it opens a picker — preview on the left, every image this session generated on the right (newest first, labeled by its prompt, clickable path below) — and after you choose, type the prompt in the composer. Dedicated image providers (`type: imagen` / `images`) only. |
| `/redo [prompt]` | Re-send the last request: same reference images, same prompt unless you supply a new one. Bare `/redo` rolls the dice again (image models vary per call); `/redo <reworded prompt>` retries from the *same* canvas, so a rejected result never becomes the next input. Dedicated image providers only. |
| `/session` | Tabbed selector over saved sessions: "Resume" to resume one, "Delete" to multi-select and delete others. |
| `/save [title]` | Start persisting an ephemeral session (one started with `--no-save` or `no_save: true`): the whole backlog is written at once and auto-save continues from then on. An optional title is kept as-is; otherwise the model-generated one is used. Only offered while the session is ephemeral. |
| `/model` | Tabbed settings for the current session: "Model" picks the model, "Context" the context window, "Effort" the reasoning effort (`default`, `low`, `medium`, `high`, `xhigh`, `max` — passed to the provider verbatim, so a level the model doesn't support surfaces as an API error and you pick another), "Temperature" a slider (`default` omits the parameter), and a read-only "System" tab showing the system prompt exactly as sent. Enter applies all tabs; only changed values are announced. Image providers get their own tabs instead (see Image Generation). |
| `/compact [hint]` | Summarize older history to free context; optional hint guides what to keep. Offered only while token accounting is live. |
| `/export [file]` | Export the conversation (saved sessions: the full on-disk log, so compaction never hides older rounds) to a single self-contained HTML file — the default — or Markdown with a `.md`/`.markdown` extension. With no argument, a selector picks the format and the filename is generated from the session title. Never overwrites an existing file. |
| `/status` | Show provider, model, context usage, and last-turn token counts |
| `/tools` | Tabbed read-only view of the model's capabilities: a "Tools" tab (every built-in and MCP tool with its source) and an "MCP" tab (server status, endpoints, and tools) |
| `/debug [on\|off]` | Request inspector. `/debug on` / `/debug off` toggle recording of API round trips (a `debug` marker appears in the status row while on); bare `/debug` opens the two-tab console — "Messages" (newest first, drill into a request/response pair) and the "Verbose" switch. Recording is off by default and MCP traffic is not recorded. |
| `/skills [name [instructions]]` | Bare `/skills` lists discovered agent skills — name, source (project/user), description, and any invalid skills that were skipped. `/skills <name>` runs one: its instructions (plus anything you add after the name) are sent as the message. Agent mode only; every discovered skill also shows up as a completion row. |

Attached files are sent with your next message, then cleared automatically.

#### Supported File Types

| Type | Extensions |
|------|-----------|
| Images | `.jpg`, `.jpeg`, `.png`, `.gif`, `.webp` |
| Documents | `.pdf` |
| Text | `.txt`, `.md`, `.rs`, `.py`, `.js`, `.ts`, `.jsx`, `.tsx`, `.java`, `.c`, `.cpp`, `.h`, `.rb`, `.sh`, `.json`, `.yaml`, `.yml`, `.toml`, `.xml`, `.html`, `.css`, `.sql`, `.csv`, `.log`, `.ini`, `.cfg`, `.conf`, and more |

### Examples

These assume a config like the one above — the agents, models and providers a
run names live there, not on the command line.

```bash
# The default agent, interactively
iota

# A configured agent (its models, prompt, tools and MCP subset)
iota run reviewer

# Pick the model at startup (a `provider:*` candidate, or -M)
iota run scratch
iota run default -M "openai:*"

# Specify the model directly
iota run default -M gpt-4o
iota run default -M "anthropic:claude-sonnet-4-20250514"

# A system prompt for this run only
iota run default -s 'You are a helpful translator' -m "Translate to French: hello"

# Non-interactive mode
iota run default -m "Explain quicksort in one paragraph"

# Non-interactive mode with a JSON report and a tool-turn budget
iota run default -m "Summarise this repo" --output-format json --max-turns 5

# Continue a saved session headlessly (any unique id prefix works)
iota resume k7q -m "And the second question?"

# …or pick one from a list
iota resume

# With MCP tools (ad-hoc server via CLI flag; config servers load automatically)
iota run default --mcp "npx -y @modelcontextprotocol/server-filesystem /tmp"

# Multiple MCP servers
iota run default --mcp "npx -y @modelcontextprotocol/server-filesystem /tmp" --mcp "https://mcp.example.com/sse"

# Read message from stdin (pipe-friendly)
echo "Explain quicksort" | iota run default -m -
cat prompt.txt | iota run default -m -

# One-shot image generation with a dedicated image provider (prints the saved path)
iota run seedream -m "A red bicycle leaning on a stone wall, golden hour"

# What is configured, and what is saved
iota list                     # agents (the default listing)
iota list models reviewer     # that agent's candidate set, best first
iota list providers           # endpoints, and where each key comes from
iota list sessions            # saved sessions, newest first
```

### File Attachment Example

```
You> /file photo.png
  Attached: photo.png (image/png, 245760 bytes)
You> /file report.pdf
  Attached: report.pdf (application/pdf, 102400 bytes)
You> /file
  (tabbed selector — "Attached" to remove, "Add" to browse and add)
You> Summarize the report and describe the photo
...
```

## Project Structure

One package, one module tree: the library under `src/` holds every module and `src/main.rs` is the thin binary.

| Module (`src/`) | Role |
|---|---|
| `provider/`, `llm/` | The seven provider adapters (`openai`, `openresponses`, `anthropic`, `gemini`, `vertexai`, `imagen`, `images`) over a minimal built-in HTTP/SSE wire layer — no vendor SDKs |
| `tool/`, `shell/`, `agents/` | The tool framework and the four built-in toolsets; process execution, the macOS/Linux sandboxes and the background-job registry; the AGENTS.md and skills overlay |
| `mcp/` | The MCP client manager (stdio and streamable-HTTP transports, deferred tool groups) |
| `chat/` | The non-interactive run loop (`-m`), the tool-calling loop, and the text/JSON reports |
| `session/` | The on-disk session bundle store (`meta.json`, append-only `messages.jsonl`, `attachments/`, `images/`) |
| `markdown/`, `text/` | The streaming markdown-to-ANSI renderer, syntax highlighting, HTML for `/export`, display-width measurement and ANSI helpers |
| `mathtext/` | The LaTeX math engine: inline Unicode approximation and 2D display layout |
| `imgterm.rs` | The half-block image rasteriser |
| `host/` | Host integration: desktop notifications, the terminal progress indicator, cmux |
| `ui/` | The inline terminal engine (composer, status line, tabbed surfaces) — the only module that touches the terminal library |
| `repl/` | The interactive chat loop over the `ui` facade: slash commands, turn engine, token meter, compaction |
| `cmd/`, `config.rs`, `main.rs` | The command line, config loading and merging, wiring, and the `iota` binary |
| `app.rs`, `vars.rs`, `paths.rs`, `sync.rs` | Host directories, `${var}` expansion and the environment seam, path and lock helpers |
| `testing/` | Shared test fakes (cargo feature `testing`, enabled for tests only — it never changes the shipped binary) |

## Dependencies

| Crate | Used for |
|---|---|
| [tokio](https://github.com/tokio-rs/tokio) | Async runtime (HTTP streaming, child processes, signals) |
| [reqwest](https://github.com/seanmonstar/reqwest) | HTTP client; TLS is always [rustls](https://github.com/rustls/rustls) |
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk) | Model Context Protocol client (stdio and streamable-HTTP transports) |
| [serde](https://github.com/serde-rs/serde) / [serde_json](https://github.com/serde-rs/json) / [serde_norway](https://github.com/cafkafk/serde-yaml) | Wire formats, session files, and YAML config |
| [clap](https://github.com/clap-rs/clap) | Command-line parsing |
| [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) | Inline terminal UI (composer, status line, tabbed surfaces) |
| [syntect](https://github.com/trishume/syntect) + [two-face](https://codeberg.org/CosmicHarper/two-face) | Syntax highlighting for code blocks, diffs, and `/export` HTML |
| [comrak](https://github.com/kivikakk/comrak) | Markdown to HTML for `/export` |
| [tiktoken](https://github.com/goliajp/rust-tiktoken) | Offline token counting for context accounting |
| [jiff](https://github.com/BurntSushi/jiff) | Timestamps (session metadata, request log, export and image filenames) |
| [image](https://github.com/image-rs/image) | Decoding PNG/JPEG/GIF/WebP for inline rendering |
| [ignore](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore) / [globset](https://github.com/BurntSushi/ripgrep/tree/master/crates/globset) / [regex](https://github.com/rust-lang/regex) | The `code` toolset's `glob` and `grep` |
| [nix](https://github.com/nix-rust/nix) | Unix signals, process groups, and file permissions |

## Documentation

- `docs/ARCHITECTURE.md` — the architecture and module map
- `docs/design/` — design documents per feature (shell and code toolsets, agent mode, sessions, export, math rendering, tool deferral, UI architecture, and more)
- `docs/DIVERGENCES.md` — the compatibility ledger of deliberate behavioural differences
- `docs/TUI-VERIFY.md` — the manual terminal verification gate (rendering, IME, per-terminal checks)

## Development

```bash
cargo test                                   # unit + integration tests (no network, no HOME access)
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX=1 cargo test --test ui_tmux        # the real-terminal suite (needs tmux)
./ci.sh                                      # everything CI runs, in order
```

## Releases

A release is cut by pushing a tag. Creating a Release by hand on the GitHub web
page builds nothing — the tag is the trigger:

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

`.github/workflows/release.yml` does the rest: it builds the four targets on
native runners, packs each one as `iota-<target>.tar.xz` with a SHA-256, opens
the GitHub Release, publishes `iota-installer.sh` beside the archives and
commits `Formula/iota.rb` to
[iotash/homebrew-tap](https://github.com/iotash/homebrew-tap) — which needs a
`HOMEBREW_TAP_TOKEN` repository secret that can write to the tap.

That workflow is generated, never hand-edited: `dist-workspace.toml` is the
source of truth and `dist init` rewrites the YAML from it. `dist plan` prints
what a tag would produce, without building anything.

## License

MIT
